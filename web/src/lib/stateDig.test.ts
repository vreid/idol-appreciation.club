import { describe, expect, it, vi } from "vite-plus/test";
import type { ApiClient, DigEntry, Placement } from "./api";
import { createAppStore, type AppStore } from "./state";

function placement(id: string, depth: number): Placement & DigEntry {
  return {
    id,
    username: `u-${id}`,
    x: 0,
    y: 0,
    w: 32,
    h: 32,
    rot: 0,
    created_at: "2026-10-13T00:00:00Z",
    depth,
  };
}

function digClient(): ApiClient {
  const stack = [
    placement("top", 1),
    placement("mid", 2),
    placement("deep", 3),
  ];
  return {
    register: vi.fn(() => Promise.resolve({ username: "b", pending: false })),
    login: vi.fn(() => Promise.resolve()),
    logout: vi.fn(() => Promise.resolve()),
    me: vi.fn(() => Promise.resolve(null)),
    place: vi.fn(() => Promise.resolve({ ...placement("new", 1), seq: 42 })),
    near: vi.fn(() => Promise.resolve([])),
    center: vi.fn(() => Promise.resolve({ current: null, all_time: [] })),
    report: vi.fn(() => Promise.resolve()),
    removePlacement: vi.fn(() => Promise.resolve()),
    pendingAccounts: vi.fn(() => Promise.resolve([])),
    approveAccount: vi.fn(() => Promise.resolve()),
    dig: vi.fn(() => Promise.resolve({ stack, total: 3, truncated: false })),
  };
}

function makeStore(client: ApiClient): AppStore {
  return createAppStore(client, {
    createObjectUrl: () => "blob:fake",
    revokeObjectUrl: () => {},
  });
}

describe("dig stack selection", () => {
  it("loads the stack and anchors the cue to the clicked placement", async () => {
    const client = digClient();
    const store = makeStore(client);
    store.selectPlace(placement("mid", 2), { x: 100, y: 100 });
    expect(client.dig).toHaveBeenCalledWith(100, 100);
    await vi.waitFor(() => {
      expect(store.digTotal).toBe(3);
    });
    // "mid" is stratum 2 of 3.
    expect(store.digIndex).toBe(1);
    expect(store.selectedPlace?.id).toBe("mid");
  });

  it("flips through the stack and clamps at both ends", async () => {
    const store = makeStore(digClient());
    store.selectPlace(placement("top", 1), { x: 100, y: 100 });
    await vi.waitFor(() => {
      expect(store.digTotal).toBe(3);
    });
    store.flipDig(1);
    expect(store.selectedPlace?.id).toBe("mid");
    store.flipDig(9); // clamps at the deepest stratum
    expect(store.selectedPlace?.id).toBe("deep");
    expect(store.digIndex).toBe(2);
    store.flipDig(-9); // clamps back at the top
    expect(store.selectedPlace?.id).toBe("top");
    expect(store.digIndex).toBe(0);
  });

  it("clears dig state on deselect", async () => {
    const store = makeStore(digClient());
    store.selectPlace(placement("top", 1), { x: 100, y: 100 });
    await vi.waitFor(() => {
      expect(store.digTotal).toBe(3);
    });
    store.deselectPlace();
    expect(store.selectedPlace).toBeNull();
    expect(store.digStack).toEqual([]);
    expect(store.digTotal).toBe(0);
    expect(store.digIndex).toBe(-1);
  });

  it("anchors the latest selection when digs resolve out of order", async () => {
    const client = digClient();
    const store = makeStore(client);
    store.selectPlace(placement("top", 1), { x: 100, y: 100 });
    store.selectPlace(placement("deep", 3), { x: 200, y: 200 });
    await vi.waitFor(() => {
      expect(store.digIndex).toBe(2);
    });
    expect(store.selectedPlace?.id).toBe("deep");
  });
});
