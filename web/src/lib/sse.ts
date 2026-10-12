/**
 * Server-sent events for world changes, with manual reconnect + exponential
 * backoff when the browser gives up (readyState CLOSED). The EventSource is
 * injectable so tests can drive the manager with a fake.
 */

import { bboxContainsPoint, WORLD_CENTER } from "./world";

export type WorldEvent =
  | { type: "place"; seq: number; bbox: [number, number, number, number] }
  | { type: "remove"; id: number; bbox: [number, number, number, number] };

export interface EventSourceLike {
  readyState: number;
  close(): void;
  onopen: (() => void) | null;
  onmessage: ((event: { data: string }) => void) | null;
  onerror: (() => void) | null;
}

export type EventSourceFactory = (url: string) => EventSourceLike;

export interface SseClientOptions {
  onEvent: (event: WorldEvent) => void;
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
  if (typeof data !== "object" || data === null) return null;
  const bbox = readBBox(data);
  if (bbox === null) return null;
  if ("type" in data && data.type === "place" && "seq" in data) {
    const seq = data.seq;
    if (typeof seq === "number") return { type: "place", seq, bbox };
    return null;
  }
  if ("type" in data && data.type === "remove" && "id" in data) {
    const id = data.id;
    if (typeof id === "number") return { type: "remove", id, bbox };
    return null;
  }
  return null;
}

function readBBox(data: object): [number, number, number, number] | null {
  if (!("bbox" in data) || !Array.isArray(data.bbox)) return null;
  const bbox = data.bbox;
  if (
    bbox.length === 4 &&
    bbox.every((n: unknown): n is number => typeof n === "number")
  ) {
    return [bbox[0] ?? 0, bbox[1] ?? 0, bbox[2] ?? 0, bbox[3] ?? 0];
  }
  return null;
}

/** True when an event bbox covers the 0/0 world center point. */
export function eventTouchesCenter(
  bbox: [number, number, number, number],
): boolean {
  const [x, y, w, h] = bbox;
  return bboxContainsPoint({ x, y, w, h }, WORLD_CENTER, WORLD_CENTER);
}

export class SseClient {
  private readonly options: Required<Pick<SseClientOptions, "onEvent">> & {
    sourceFactory: EventSourceFactory;
    initialBackoffMs: number;
    maxBackoffMs: number;
  };
  private source: EventSourceLike | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private backoffMs: number;
  private scheduledDelayMs = 0;
  private running = false;

  constructor(options: SseClientOptions) {
    this.options = {
      onEvent: options.onEvent,
      sourceFactory: options.sourceFactory ?? defaultSourceFactory,
      initialBackoffMs: options.initialBackoffMs ?? DEFAULT_INITIAL_BACKOFF_MS,
      maxBackoffMs: options.maxBackoffMs ?? DEFAULT_MAX_BACKOFF_MS,
    };
    this.backoffMs = this.options.initialBackoffMs;
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    this.backoffMs = this.options.initialBackoffMs;
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

  /** Milliseconds of the next scheduled reconnect wait. */
  currentBackoffMs(): number {
    return this.reconnectTimer !== null ? this.scheduledDelayMs : this.backoffMs;
  }

  isReconnectScheduled(): boolean {
    return this.reconnectTimer !== null;
  }

  private connect(): void {
    const source = this.options.sourceFactory(EVENTS_URL);
    this.source = source;
    source.onopen = () => {
      this.backoffMs = this.options.initialBackoffMs;
    };
    source.onmessage = (event) => {
      const parsed = parseWorldEvent(event.data);
      if (parsed !== null) this.options.onEvent(parsed);
    };
    source.onerror = () => {
      if (source.readyState !== READY_STATE_CLOSED) return; // browser retries
      source.close();
      this.source = null;
      if (!this.running) return;
      this.scheduleReconnect();
    };
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
    this.backoffMs = Math.min(delay * 2, this.options.maxBackoffMs);
  }
}

function defaultSourceFactory(url: string): EventSourceLike {
  return new EventSource(url);
}
