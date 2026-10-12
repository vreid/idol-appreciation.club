import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";
import {
  eventTouchesCenter,
  parseWorldEvent,
  SseClient,
  type EventSourceLike,
} from "./sse";

class FakeEventSource implements EventSourceLike {
  static instances: FakeEventSource[] = [];

  readyState = 0;
  closed = false;

  private listeners: {
    type: "open" | "message" | "error";
    fn: (event: { data: string }) => void;
  }[] = [];

  constructor(public readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(
    type: "open" | "message" | "error",
    listener: (event: { data: string }) => void,
  ): void {
    this.listeners.push({ type, fn: listener });
  }

  close(): void {
    this.closed = true;
  }

  emitOpen(): void {
    for (const { type, fn } of this.listeners) {
      if (type === "open") fn({ data: "" });
    }
  }

  emitMessage(data: string): void {
    for (const { type, fn } of this.listeners) {
      if (type === "message") fn({ data });
    }
  }

  fail(closed: boolean): void {
    this.readyState = closed ? 2 : 1;
    for (const { type, fn } of this.listeners) {
      if (type === "error") fn({ data: "" });
    }
  }
}

function factory(): (url: string) => EventSourceLike {
  return (url: string) => new FakeEventSource(url);
}

function lastSource(): FakeEventSource {
  const last = FakeEventSource.instances.at(-1);
  if (last === undefined) throw new Error("no EventSource created");
  return last;
}

beforeEach(() => {
  FakeEventSource.instances = [];
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("parseWorldEvent", () => {
  it("parses place events with seq and bbox", () => {
    expect(
      parseWorldEvent('{"type":"place","seq":12,"bbox":[1,2,3,4]}'),
    ).toEqual({ type: "place", seq: 12, bbox: [1, 2, 3, 4] });
  });

  it("parses remove events", () => {
    expect(
      parseWorldEvent('{"type":"remove","id":"0197d3a4-82","bbox":[5,6,7,8]}'),
    ).toEqual({ type: "remove", id: "0197d3a4-82", bbox: [5, 6, 7, 8] });
  });

  it("returns null for malformed payloads", () => {
    expect(parseWorldEvent("not json")).toBeNull();
    expect(parseWorldEvent("null")).toBeNull();
    expect(parseWorldEvent("{}")).toBeNull();
    expect(
      parseWorldEvent('{"type":"place","seq":"x","bbox":[1,2,3,4]}'),
    ).toBeNull();
    expect(parseWorldEvent('{"type":"place","seq":1,"bbox":[1,2]}')).toBeNull();
    expect(
      parseWorldEvent('{"type":"remove","id":"","bbox":[1,2,3,4]}'),
    ).toBeNull();
    expect(
      parseWorldEvent('{"type":"remove","id":7,"bbox":[1,2,3,4]}'),
    ).toBeNull();
    expect(parseWorldEvent('{"type":"unknown","bbox":[1,2,3,4]}')).toBeNull();
  });
});

describe("eventTouchesCenter", () => {
  it("is true only when the bbox covers 0/0 center", () => {
    const c = 134217728;
    expect(eventTouchesCenter([c - 10, c - 10, 20, 20])).toBe(true);
    expect(eventTouchesCenter([c, c - 10, 5, 20])).toBe(true);
    expect(eventTouchesCenter([0, 0, c, c])).toBe(false);
    expect(eventTouchesCenter([c + 1, c, 10, 10])).toBe(false);
  });
});

describe("SseClient", () => {
  it("connects to the events URL on start", () => {
    const client = new SseClient({
      onEvent: () => {},
      sourceFactory: factory(),
    });
    client.start();
    expect(lastSource().url).toBe("/api/events");
    client.stop();
  });

  it("delivers parsed events to the handler and drops garbage", () => {
    const seen: unknown[] = [];
    const client = new SseClient({
      onEvent: (e) => {
        seen.push(e);
      },
      sourceFactory: factory(),
    });
    client.start();
    lastSource().emitMessage('{"type":"place","seq":1,"bbox":[0,0,1,1]}');
    lastSource().emitMessage("garbage");
    expect(seen).toEqual([{ type: "place", seq: 1, bbox: [0, 0, 1, 1] }]);
    client.stop();
  });

  it("schedules a reconnect with doubling backoff after hard close", () => {
    const client = new SseClient({
      onEvent: () => {},
      sourceFactory: factory(),
      initialBackoffMs: 1000,
      maxBackoffMs: 30000,
    });
    client.start();
    lastSource().fail(true);
    expect(client.isReconnectScheduled()).toBe(true);
    expect(client.currentBackoffMs()).toBe(1000);
    vi.advanceTimersByTime(1000);
    expect(FakeEventSource.instances.length).toBe(2);
    expect(client.currentBackoffMs()).toBe(2000);
    lastSource().fail(true);
    vi.advanceTimersByTime(2000);
    expect(FakeEventSource.instances.length).toBe(3);
    client.stop();
  });

  it("caps the backoff at maxBackoffMs", () => {
    const client = new SseClient({
      onEvent: () => {},
      sourceFactory: factory(),
      initialBackoffMs: 1000,
      maxBackoffMs: 30000,
    });
    client.start();
    for (let i = 0; i < 10; i++) {
      lastSource().fail(true);
      vi.advanceTimersByTime(client.currentBackoffMs());
    }
    expect(client.currentBackoffMs()).toBe(30000);
    client.stop();
  });

  it("resets backoff after a successful open", () => {
    const client = new SseClient({
      onEvent: () => {},
      sourceFactory: factory(),
      initialBackoffMs: 1000,
    });
    client.start();
    lastSource().fail(true);
    vi.advanceTimersByTime(1000);
    expect(client.currentBackoffMs()).toBe(2000);
    lastSource().emitOpen();
    expect(client.currentBackoffMs()).toBe(1000);
    client.stop();
  });

  it("lets the browser retry when readyState is not CLOSED", () => {
    const client = new SseClient({
      onEvent: () => {},
      sourceFactory: factory(),
    });
    client.start();
    lastSource().fail(false);
    expect(client.isReconnectScheduled()).toBe(false);
    expect(FakeEventSource.instances.length).toBe(1);
    client.stop();
  });

  it("stop() closes the socket and cancels pending reconnects", () => {
    const client = new SseClient({
      onEvent: () => {},
      sourceFactory: factory(),
    });
    client.start();
    lastSource().fail(true);
    client.stop();
    expect(lastSource().closed).toBe(true);
    expect(client.isReconnectScheduled()).toBe(false);
    vi.advanceTimersByTime(60000);
    expect(FakeEventSource.instances.length).toBe(1);
  });

  it("start() is idempotent", () => {
    const client = new SseClient({
      onEvent: () => {},
      sourceFactory: factory(),
    });
    client.start();
    client.start();
    expect(FakeEventSource.instances.length).toBe(1);
    client.stop();
  });
});
