/**
 * Contract integration suite: the TypeScript client against the REAL
 * backend. The harness (web/scripts/contract-harness.sh, task
 * `web:contract`) boots the pinned backend gate image + Postgres, exports
 * CONTRACT_BASE_URL, and runs this file via vitest.contract.config.ts.
 *
 * Every response and SSE event is parsed against the wire-contract schemas
 * (contract/zod) — first inside the API client itself (a malformed payload
 * throws ApiError and fails the test), then again explicitly for clarity.
 * One negative case asserts the schemas reject tampered payloads.
 */

import { describe, expect, it } from "vite-plus/test";
import http from "node:http";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  centerInfoSchema,
  healthSchema,
  meSchema,
  placedPlacementSchema,
  worldEventSchema,
  placementsNearSchema,
  type WorldEvent,
} from "@contract";
import { ApiError, createApiClient } from "../../src/lib/api";

// NOT `BASE_URL`: vite clobbers that name with its app base ("/") in
// process.env during vitest runs, which would 404 every request.
const BASE_URL = process.env.CONTRACT_BASE_URL ?? "http://127.0.0.1:18080";

const FIXTURE = fileURLToPath(
  new URL("../../test-assets/e2e-tile.png", import.meta.url),
);

/** The backend scopes a session by cookie; node fetch needs a jar. */
class CookieJar {
  private readonly cookies = new Map<string, string>();

  capture(response: Response): void {
    for (const cookie of response.headers.getSetCookie()) {
      const [pair] = cookie.split(";");
      if (pair === undefined) continue;
      const eq = pair.indexOf("=");
      if (eq < 0) continue;
      this.cookies.set(pair.slice(0, eq), pair.slice(eq + 1));
    }
  }

  header(): string {
    return [...this.cookies]
      .map(([name, value]) => `${name}=${value}`)
      .join("; ");
  }
}

const jar = new CookieJar();

const fetchWithCookies: typeof fetch = async (input, init) => {
  const headers = new Headers(init?.headers);
  const cookie = jar.header();
  if (cookie.length > 0) headers.set("cookie", cookie);
  const response = await fetch(input, { ...init, headers });
  jar.capture(response);
  return response;
};

const api = createApiClient({ fetch: fetchWithCookies, base: BASE_URL });

const RUN = Date.now().toString(36);
const USERNAME = `contract_bot_${RUN}`;

/** Disjoint far-from-center spots so runs never collide (size 64 each). */
function spot(index: number): { x: number; y: number } {
  const base = 32_000_000;
  return { x: base + index * 512, y: base + index * 512 };
}

const IMAGE_BYTES = readFileSync(FIXTURE);

/**
 * Real-clock delay. EXCEPTION to fake-timer policy: the wait honors the
 * LIVE backend's Retry-After on 429 — a server-side cooldown no in-process
 * timer control can fast-forward.
 */
function delay(ms: number): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, ms);
  return promise;
}

/** Places one image, retrying through the 1s empty-space cooldown. */
async function placeAt(index: number): Promise<void> {
  const { x, y } = spot(index);
  for (let attempt = 0; attempt < 30; attempt += 1) {
    try {
      // Sequential by necessity: each attempt depends on the previous
      // outcome (429 → Retry-After → retry). Parallelizing would just
      // spam the cooldown.
      // eslint-disable-next-line no-await-in-loop
      const outcome = await api.place({
        image: new Blob([IMAGE_BYTES], { type: "image/png" }),
        fileName: `contract-${index}.png`,
        x,
        y,
        size: 64,
        rot: index * 15,
      });
      // Era handling (docs "Upload flow"): a 202 parks the upload with
      // the worker; assertions on placed rows must await the commit the
      // same way the UI does — the upload-status poll.
      if (outcome.era === "quarantine") {
        // Sequential by necessity: each poll depends on the previous
        // (terminal state ends the wait).
        // eslint-disable-next-line no-await-in-loop
        await awaitSettled(outcome.accepted.id);
      }
      return;
    } catch (err) {
      if (err instanceof ApiError && err.status === 429) {
        // eslint-disable-next-line no-await-in-loop
        await delay((err.retryAfterSeconds ?? 1) * 1000);
        continue;
      }
      throw err;
    }
  }
  throw new Error(`place at spot ${index} never left cooldown`);
}

/**
 * Minimal SSE reader over node:http (fetch-based readers stall under
 * the vitest worker — its fetch wrapper never yields streamed chunks;
 * the raw socket client always does): yields the `data:` payload of
 * every complete frame.
 */
async function* sseDataPayloads(
  url: string,
  signal: AbortSignal,
): AsyncGenerator<string> {
  const target = new URL(url);
  const req = http.request(
    {
      hostname: target.hostname,
      port: Number(target.port),
      path: `${target.pathname}${target.search}`,
      method: "GET",
      headers: { accept: "text/event-stream" },
    },
    (res) => {
      if ((res.statusCode ?? 0) >= 300) {
        res.resume();
        throw new Error(`SSE stream failed: HTTP ${res.statusCode}`);
      }
    },
  );
  signal.addEventListener("abort", () => {
    req.destroy();
  });
  const res = await new Promise<http.IncomingMessage>((resolve, reject) => {
    req.on("response", resolve);
    req.on("error", reject);
    req.end();
  });
  let buffer = "";
  res.setEncoding("utf8");
  for await (const chunk of res) {
    buffer += String(chunk);
    let split = buffer.indexOf("\n\n");
    while (split >= 0) {
      for (const line of buffer.slice(0, split).split("\n")) {
        if (line.startsWith("data: ")) yield line.slice("data: ".length);
      }
      buffer = buffer.slice(split + 2);
      split = buffer.indexOf("\n\n");
    }
  }
}

/** Awaits one 202-accepted upload reaching a terminal state via the
 * status poll (the same resolution the UI falls back to when the event
 * stream is down). */
async function awaitSettled(id: string): Promise<void> {
  for (let poll = 0; poll < 100; poll += 1) {
    // eslint-disable-next-line no-await-in-loop
    const status = await api.uploadStatus(id);
    if (status?.state === "placed") return;
    if (status?.state === "failed") throw new Error(`upload ${id} failed`);
    // eslint-disable-next-line no-await-in-loop
    await delay(100);
  }
  throw new Error(`upload ${id} never settled`);
}

/** Consumes the SSE stream into `events`/`malformed` until one placed
 * frame plus three events arrive, the stream ends, or the signal aborts
 * (the expected end when the caller's assertions resolve first). */
async function consumeWorldEvents(
  url: string,
  signal: AbortSignal,
  events: WorldEvent[],
  malformed: string[],
): Promise<void> {
  try {
    for await (const raw of sseDataPayloads(url, signal)) {
      let parsedJson: unknown;
      try {
        parsedJson = JSON.parse(raw) as unknown;
      } catch {
        malformed.push(raw);
        continue;
      }
      const parsed = worldEventSchema.safeParse(parsedJson);
      if (parsed.success) {
        events.push(parsed.data);
        // The ingest worker batches per area per second (the coarse
        // event contract), so same-area placements may coalesce into
        // fewer place frames — require one place plus three events
        // overall, not three place frames.
        const placeEvents = events.filter((e) => e.type === "placed").length;
        if (placeEvents >= 1 && events.length >= 3) return;
      } else {
        malformed.push(raw);
      }
    }
  } catch (err) {
    // AbortError is the expected end when the assertions resolve first
    // and the caller aborts the stream mid-read.
    if (!(err instanceof Error && err.name === "AbortError")) throw err;
  }
}

describe("contract: real backend parses clean against contract/zod", () => {
  it("GET /api/health matches the health schema", async () => {
    const response = await fetch(`${BASE_URL}/api/health`);
    expect(response.ok, `health HTTP ${response.status}`).toBe(true);
    const body: unknown = await response.json();
    const parsed = healthSchema.safeParse(body);
    expect(parsed.success).toBe(true);
  });

  it("register → login → place → near → center → me all parse clean", async () => {
    const created = await api.register(USERNAME, "contract-password-0");
    expect(created).toEqual({ username: USERNAME, pending: false });
    await api.login(USERNAME, "contract-password-0");

    await placeAt(0);

    const { x, y } = spot(0);
    const nearby = await api.near(x, y, 256);
    const nearParsed = placementsNearSchema.safeParse(nearby);
    expect(nearParsed.success).toBe(true);
    expect(nearby.some((p) => p.username === USERNAME && p.x === x)).toBe(true);

    const center = await api.center();
    const centerParsed = centerInfoSchema.safeParse(center);
    expect(centerParsed.success).toBe(true);
    expect(Array.isArray(center.all_time)).toBe(true);

    // A fresh placement arms the cooldown, so /me must carry it.
    const me = await api.me();
    if (me === null) throw new Error("expected an authenticated session");
    const meParsed = meSchema.safeParse(me);
    expect(meParsed.success).toBe(true);
    expect(me.cooldown_until).not.toBeNull();
  });

  it("SSE stream delivers ≥3 events, all contract-clean, including a place", async () => {
    const events: WorldEvent[] = [];
    const malformed: string[] = [];
    const controller = new AbortController();

    // The consumer awaits the real stream; vitest's per-test timeout is
    // the only clock involved.
    const consuming = consumeWorldEvents(
      `${BASE_URL}/api/events`,
      controller.signal,
      events,
      malformed,
    );

    try {
      // Three placements while subscribed (spots disjoint enough to
      // avoid the paint cooldown; placeAt honors the server's
      // Retry-After). The worker batches per area per second, so the
      // assertions below require place ≥1 among ≥3 events.
      await placeAt(1);
      await placeAt(2);
      await placeAt(3);
      await consuming;
    } finally {
      controller.abort();
    }

    expect(malformed).toEqual([]);
    expect(events.length).toBeGreaterThanOrEqual(3);
    expect(events.some((event) => event.type === "placed")).toBe(true);
    for (const event of events) {
      expect(worldEventSchema.safeParse(event).success).toBe(true);
    }
  }, 60_000);

  it("negative: tampered payloads are rejected by the schemas", async () => {
    // A place event whose seq is a string must not parse.
    const badEvent = JSON.stringify({
      type: "place",
      seq: "not-a-number",
      bbox: [1, 2, 3, 4],
    });
    expect(worldEventSchema.safeParse(JSON.parse(badEvent)).success).toBe(
      false,
    );

    // A center payload missing all_time must not parse.
    const realCenter = await fetch(`${BASE_URL}/api/center`);
    const body: unknown = await realCenter.json();
    if (typeof body !== "object" || body === null) {
      throw new Error("center payload was not an object");
    }
    const tampered = { ...body, all_time: undefined };
    expect(centerInfoSchema.safeParse(tampered).success).toBe(false);

    // A placed-placement payload missing seq must not parse.
    expect(
      placedPlacementSchema.safeParse({ id: "x", x: 1, y: 2, w: 3, h: 4 })
        .success,
    ).toBe(false);
  });
});
