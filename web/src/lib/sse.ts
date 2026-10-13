/**
 * Server-sent events for world changes, with manual reconnect + exponential
 * backoff when the browser gives up (readyState CLOSED). The EventSource is
 * injectable so tests can drive the manager with a fake.
 *
 * Every message is parsed against the wire contract (contract/zod); events
 * that fail the schema are dropped here and reported through
 * `onMalformed` (console.warn + toast at the app layer), never thrown.
 */

import { worldEventSchema } from "@contract";
import type { WorldEvent } from "@contract";
import { bboxContainsPoint, WORLD_CENTER } from "./world";

export type { WorldEvent };

export interface EventSourceLike {
  readyState: number;
  close(): void;
  /** Listener always receives a message-shaped event; open/error ignore it. */
  addEventListener(
    type: "open" | "message" | "error",
    listener: (event: { data: string }) => void,
  ): void;
}

export type EventSourceFactory = (url: string) => EventSourceLike;

export interface SseClientOptions {
  onEvent: (event: WorldEvent) => void;
  /** Called with the raw payload of every message the contract rejects. */
  onMalformed?: (raw: string) => void;
  sourceFactory?: EventSourceFactory;
  initialBackoffMs?: number;
  maxBackoffMs?: number;
}

export const EVENTS_URL = "/api/events";
const DEFAULT_INITIAL_BACKOFF_MS = 1000;
const DEFAULT_MAX_BACKOFF_MS = 30000;
/** EventSource readyState value meaning "no longer trying". */
const READY_STATE_CLOSED = 2;

export function parseWorldEvent(raw: string): WorldEvent | null {
  let data: unknown;
  try {
    data = JSON.parse(raw) as unknown;
  } catch {
    return null;
  }
  const parsed = worldEventSchema.safeParse(data);
  return parsed.success ? parsed.data : null;
}

/** True when an event bbox covers the 0/0 world center point. */
export function eventTouchesCenter(
  bbox: readonly [number, number, number, number],
): boolean {
  const [x, y, w, h] = bbox;
  return bboxContainsPoint({ x, y, w, h }, WORLD_CENTER, WORLD_CENTER);
}

export class SseClient {
  private readonly onEvent: (event: WorldEvent) => void;
  private readonly onMalformed: ((raw: string) => void) | null;
  private readonly sourceFactory: EventSourceFactory;
  private readonly initialBackoffMs: number;
  private readonly maxBackoffMs: number;

  private source: EventSourceLike | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private backoffMs: number;
  private scheduledDelayMs = 0;
  private running = false;

  constructor(options: SseClientOptions) {
    this.onEvent = options.onEvent;
    this.onMalformed = options.onMalformed ?? null;
    this.sourceFactory =
      options.sourceFactory ?? ((url) => new EventSource(url));
    this.initialBackoffMs =
      options.initialBackoffMs ?? DEFAULT_INITIAL_BACKOFF_MS;
    this.maxBackoffMs = options.maxBackoffMs ?? DEFAULT_MAX_BACKOFF_MS;
    this.backoffMs = this.initialBackoffMs;
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.backoffMs = this.initialBackoffMs;
    this.connect();
  }

  stop(): void {
    this.running = false;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
      this.scheduledDelayMs = 0;
    }
    this.source?.close();
    this.source = null;
  }

  /** Milliseconds of the currently scheduled reconnect wait, if any. */
  currentBackoffMs(): number {
    if (this.reconnectTimer === null) return this.backoffMs;
    return this.scheduledDelayMs;
  }

  isReconnectScheduled(): boolean {
    return this.reconnectTimer !== null;
  }

  private connect(): void {
    const source = this.sourceFactory(EVENTS_URL);
    this.source = source;
    source.addEventListener("open", () => {
      this.backoffMs = this.initialBackoffMs;
    });
    source.addEventListener("message", (event) => {
      const parsed = parseWorldEvent(event.data);
      if (parsed === null) {
        console.warn("[sse] malformed event ignored:", event.data);
        this.onMalformed?.(event.data);
      } else {
        this.onEvent(parsed);
      }
    });
    source.addEventListener("error", () => {
      // Non-CLOSED means the browser is retrying on its own.
      if (source.readyState !== READY_STATE_CLOSED) return;
      source.close();
      this.source = null;
      if (!this.running) return;
      this.scheduleReconnect();
    });
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer !== null) return;
    const delay = this.backoffMs;
    this.scheduledDelayMs = delay;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.scheduledDelayMs = 0;
      if (this.running) this.connect();
    }, delay);
    this.backoffMs = Math.min(delay * 2, this.maxBackoffMs);
  }
}
