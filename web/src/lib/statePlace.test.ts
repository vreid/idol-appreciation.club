import { describe, expect, it, vi } from "vite-plus/test";

import { ApiError, type ApiClient, type PlaceRequest } from "./api";
import { DEFAULT_LONG_EDGE } from "./sizing";
import { createAppStore } from "./state";
import { samplePlacedEvent } from "./placeWait";

interface ClientProbe {
  placeCalls: { x: number; y: number; size: number }[];
}

function makeClient(
  overrides: Partial<ApiClient> = {},
): ApiClient & ClientProbe {
  const placeCalls: { x: number; y: number; size: number }[] = [];
  return {
    register: vi.fn(() => Promise.resolve({ username: "bot", pending: false })),
    login: vi.fn(() => Promise.resolve()),
    logout: vi.fn(() => Promise.resolve()),
    pendingAccounts: vi.fn(() => Promise.resolve([])),
    approveAccount: vi.fn(() => Promise.resolve()),
    feed: vi.fn(() => Promise.resolve({ items: [], next_before: null })),
    dig: vi.fn(() =>
      Promise.resolve({ stack: [], total: 0, truncated: false }),
    ),
    me: vi.fn(() => Promise.resolve(null)),
    place: vi.fn((req: PlaceRequest) => {
      placeCalls.push({ x: req.x, y: req.y, size: req.size });
      const { id } = samplePlacedEvent("job-1");
      return Promise.resolve({
        era: "quarantine" as const,
        accepted: {
          id,
          state: "pending" as const,
          x: req.x,
          y: req.y,
          w: req.size,
          h: req.size,
          rot: req.rot,
        },
      });
    }),
    uploadStatus: vi.fn(() => Promise.resolve(null)),
    near: vi.fn(() => Promise.resolve([])),
    center: vi.fn(() => Promise.resolve({ current: null, all_time: [] })),
    report: vi.fn(() => Promise.resolve()),
    removePlacement: vi.fn(() => Promise.resolve()),
    updateTags: vi.fn(() => Promise.resolve({ id: "j", tags: [] })),
    hot: vi.fn(() =>
      Promise.resolve({ window_hours: 24, total: 0, cells: [] }),
    ),
    ...overrides,
    placeCalls,
  };
}

function makeStore(client: ApiClient = makeClient()) {
  let urls = 0;
  const revoked: string[] = [];
  const store = createAppStore(client, {
    createObjectUrl: () => `blob:fake-${++urls}`,
    revokeObjectUrl: (url) => {
      revoked.push(url);
    },
  });
  return { store, revoked };
}

const PNG = new Blob(["png"], { type: "image/png" });

describe("place flow", () => {
  it("rejects oversized files without entering the flow", () => {
    const { store } = makeStore();
    const big = new Blob([new Uint8Array(11).fill(0)], {
      type: "image/png",
    });
    Object.defineProperty(big, "size", { value: 10 * 1024 * 1024 + 1 });
    expect(store.startPlace(big, "big.png", 100, 100)).toBe(false);
    expect(store.placeMode).toBe("idle");
  });

  it("prepares an image, adjusts size, and computes the ghost", () => {
    const { store, revoked } = makeStore();
    expect(store.startPlace(PNG, "a.png", 1000, 500)).toBe(true);
    expect(store.placeMode).toBe("panel");
    expect(store.longEdge).toBe(DEFAULT_LONG_EDGE);
    store.setLongEdge(300.9);
    expect(store.longEdge).toBe(301);
    expect(store.ghostSize()).toEqual({ w: 301, h: 151 });
    store.enterChoose();
    expect(store.placeMode).toBe("choose");
    store.backToPanel();
    expect(store.placeMode).toBe("panel");
    store.cancelPlace();
    expect(store.placeMode).toBe("idle");
    expect(revoked).toEqual(["blob:fake-1"]);
  });

  it("clamps out-of-world spots into the world", async () => {
    const client = makeClient();
    const { store } = makeStore(client);
    store.startPlace(PNG, "a.png", 100, 100);
    let settled = false;
    void store.confirmSpot(-50, 2 ** 29).then(() => {
      settled = true;
    });
    await vi.waitFor(() => {
      store.resolvePlacementEvent(samplePlacedEvent("job-1"));
      if (!settled) throw new Error("not settled");
    });
    expect(client.placeCalls[0]).toEqual({ x: 0, y: 2 ** 28, size: 256 });
  });

  it("shows the cooldown remaining on 429 and keeps the panel", async () => {
    const client = makeClient({
      place: () => Promise.reject(new ApiError(429, "cooldown", 30)),
    });
    const { store } = makeStore(client);
    store.startPlace(PNG, "a.png", 100, 100);
    store.enterChoose();
    const attempt = await store.confirmSpot(5, 5);
    expect(attempt.ok).toBe(false);
    expect(store.placeMode).toBe("panel");
    expect(
      store.toasts.some((t) => t.message.includes("Try again in 30s")),
    ).toBe(true);
  });

  it("prompts auth on 401 while placing", async () => {
    const client = makeClient({
      place: () => Promise.reject(new ApiError(401, "nope", null)),
    });
    const { store } = makeStore(client);
    store.startPlace(PNG, "a.png", 100, 100);
    await store.confirmSpot(1, 2);
    expect(store.authPrompt).toBe(true);
  });

  it("ignores confirm without a prepared image", async () => {
    const client = makeClient();
    const { store } = makeStore(client);
    expect((await store.confirmSpot(1, 2)).ok).toBe(false);
    expect(client.placeCalls.length).toBe(0);
  });
});

describe("place flow eras", () => {
  it("confirms a spot with the ghost top-left coordinates", async () => {
    const client = makeClient();
    const { store } = makeStore(client);
    store.startPlace(PNG, "a.png", 1000, 500);
    store.setLongEdge(512);
    store.enterChoose();
    // The 202 acceptance parks the pending marker; the resolution rides
    // the detached watcher.
    const attempt = await store.confirmSpot(134217000, 134217700);
    expect(attempt).toEqual({
      ok: true,
      pending: true,
      placement: {
        id: "job-1",
        x: 134217000,
        y: 134217700,
        w: 512,
        h: 512,
        rot: 0,
        seq: 0,
      },
    });
    expect(client.placeCalls[0]).toEqual({
      x: 134217000,
      y: 134217700,
      size: 512,
    });
    expect(store.placeMode).toBe("idle");
    expect(store.pendingUploads).toHaveLength(1);
    // The worker's placed event resolves the marker.
    store.resolvePlacementEvent(samplePlacedEvent("job-1"));
    await vi.waitFor(() => {
      expect(store.pendingUploads).toHaveLength(0);
    });
    expect(store.toasts.some((t) => t.message === "Image placed!")).toBe(true);
  });

  it("clears the pending marker with the failure toast on place_failed", async () => {
    const client = makeClient();
    const { store, revoked } = makeStore(client);
    store.startPlace(PNG, "a.png", 100, 100);
    const attempt = await store.confirmSpot(10, 10);
    expect(attempt.ok).toBe(true);
    store.resolvePlacementEvent({
      type: "place_failed",
      job: "job-1",
      attempts: 5,
      permanent: true,
      error: "decode failed",
    });
    await vi.waitFor(() => {
      expect(store.pendingUploads).toHaveLength(0);
    });
    expect(store.toasts.some((t) => t.message.includes("decode failed"))).toBe(
      true,
    );
    expect(
      store.toasts.some((t) => t.message.includes("Still processing")),
    ).toBe(false);
    // The marker's borrowed preview URL is revoked on cleanup.
    expect(revoked).toContain("blob:fake-1");
  });

  it("behaves exactly as before on a 201-era backend", async () => {
    const client = makeClient({
      place: () =>
        Promise.resolve({
          era: "sync" as const,
          placement: {
            id: "p1",
            x: 1,
            y: 2,
            w: 3,
            h: 4,
            rot: 0,
            seq: 5,
          },
        }),
    });
    const { store } = makeStore(client);
    store.startPlace(PNG, "a.png", 100, 100);
    const attempt = await store.confirmSpot(1, 2);
    expect(attempt).toEqual({
      ok: true,
      placement: { id: "p1", x: 1, y: 2, w: 3, h: 4, rot: 0, seq: 5 },
    });
    expect(store.pendingUploads).toHaveLength(0);
    expect(store.toasts.some((t) => t.message === "Image placed!")).toBe(true);
    expect(store.placeMode).toBe("idle");
  });
});
