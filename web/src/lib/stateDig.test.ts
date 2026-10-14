import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { ApiClient, DigEntry, Placement } from "./api";
import { createAppStore } from "./state";

afterEach(() => {
  vi.useRealTimers();
});

function flush(): Promise<void> {
  return vi.waitFor(() => {});
}

function entry(
  id: string,
  username: string,
  depth: number,
): Placement & DigEntry {
  return {
    id,
    username,
    x: 2048,
    y: 2048,
    w: 256,
    h: 256,
    rot: 0,
    created_at: "2026-10-14T00:00:00Z",
    depth,
  };
}

// Newest-first, like /api/dig: top of the pile is stack[0].
const PILE = [
  entry("top", "wren", 1),
  entry("mid", "noor", 2),
  entry("deep", "sable", 3),
];

const DEFAULT_PAGE: { stack: DigEntry[]; total: number } = {
  stack: PILE,
  total: 3,
};

function makeClient(
  page: { stack: DigEntry[]; total: number } = DEFAULT_PAGE,
): ApiClient {
  return {
    register: vi.fn(() => Promise.resolve({ username: "b", pending: false })),
    login: vi.fn(() => Promise.resolve()),
    logout: vi.fn(() => Promise.resolve()),
    me: vi.fn(() => Promise.resolve(null)),
    place: vi.fn(() => Promise.resolve({ ...PILE[0]!, seq: 1 })),
    near: vi.fn(() => Promise.resolve([])),
    center: vi.fn(() => Promise.resolve({ current: null, all_time: [] })),
    report: vi.fn(() => Promise.resolve()),
    removePlacement: vi.fn(() => Promise.resolve()),
    updateTags: vi.fn(() => Promise.resolve({ id: "p1", tags: [] })),
    pendingAccounts: vi.fn(() => Promise.resolve([])),
    approveAccount: vi.fn(() => Promise.resolve()),
    dig: vi.fn(() => Promise.resolve({ ...page, truncated: false })),
    feed: vi.fn(() => Promise.resolve({ items: [], next_before: null })),
    hot: vi.fn(() =>
      Promise.resolve({ window_hours: 24, total: 0, cells: [] }),
    ),
  };
}

function makeStore(client: ApiClient) {
  return createAppStore(client, {
    createObjectUrl: () => "blob:fake",
    revokeObjectUrl: () => {},
  });
}

describe("pile cycling", () => {
  it("selectPlace with a point loads the pile and locates the selection", async () => {
    const stratum = PILE[1];
    if (stratum === undefined) throw new Error("fixture missing");
    const store = makeStore(makeClient());
    store.selectPlace(stratum, { x: 2176, y: 2176 });
    await flush();
    expect(store.digTotal).toBe(3);
    expect(store.digIndex).toBe(1);
    expect(store.digStack[0]?.id).toBe("top");
  });

  it("queryPile detects the selection's pile and stepDig cycles with wrap", async () => {
    const stratum = PILE[0];
    if (stratum === undefined) throw new Error("fixture missing");
    const store = makeStore(makeClient());
    store.selectPlace(stratum, { x: 2176, y: 2176 });
    await flush();
    expect(await store.queryPile(2176, 2176)).toBe(true);
    expect(store.stepDig(1)).toBe(true);
    expect(store.selectedPlace?.id).toBe("mid");
    expect(store.digIndex).toBe(1);
    // Wraps: deep -> top.
    store.stepDig(1);
    store.stepDig(1);
    expect(store.selectedPlace?.id).toBe("top");
    expect(store.digIndex).toBe(0);
    // And backwards wraps the other way.
    store.stepDig(-1);
    expect(store.selectedPlace?.id).toBe("deep");
  });

  it("queryPile is false for a different pile and adoptPile picks the top", async () => {
    const stratum = PILE[0];
    if (stratum === undefined) throw new Error("fixture missing");
    const store = makeStore(makeClient());
    store.selectPlace(stratum, { x: 2176, y: 2176 });
    await flush();
    const other = { stack: [entry("other", "quill", 1)], total: 1 };
    const client = makeClient(other);
    const store2 = makeStore(client);
    store2.selectPlace(stratum, { x: 10, y: 10 });
    await flush();
    expect(await store2.queryPile(11, 11)).toBe(false);
    const top = other.stack[0];
    if (top === undefined) throw new Error("fixture missing");
    store2.adoptPile(top);
    expect(store2.selectedPlace?.id).toBe("other");
    expect(store2.digIndex).toBe(0);
    expect(store2.digTotal).toBe(1);
    // Single-stratum piles have nothing to step through.
    expect(store2.stepDig(1)).toBe(false);
  });

  it("a failed dig degrades to no pile", async () => {
    const client = makeClient();
    client.dig = vi.fn(() => Promise.reject(new Error("down")));
    const store = makeStore(client);
    expect(await store.queryPile(1, 1)).toBe(false);
    expect(store.digTotal).toBe(0);
  });
});
