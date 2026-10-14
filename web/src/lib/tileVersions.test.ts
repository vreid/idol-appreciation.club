import { describe, expect, it, vi } from "vite-plus/test";
import { createTileVersionsStore, parseVersionKey } from "./tileVersions";

describe("parseVersionKey", () => {
  it("parses z/x/y keys and rejects foreign shapes", () => {
    expect(parseVersionKey("16/2/3")).toEqual({ z: 16, x: 2, y: 3 });
    expect(parseVersionKey("16/2")).toBeNull();
    expect(parseVersionKey("a/b/c")).toBeNull();
    expect(parseVersionKey("16/2.5/3")).toBeNull();
    expect(parseVersionKey("16/-2/3")).toEqual({ z: 16, x: -2, y: 3 });
  });
});

describe("tile versions store", () => {
  it("learns versions and reports changed keys", () => {
    const store = createTileVersionsStore({
      fetchVersions: vi.fn(),
    });
    expect(store.get(16, 2, 3)).toBeUndefined();
    const seven = { key: "16/2/3", version: 7 };
    expect(store.learn([seven])).toEqual(["16/2/3"]);
    expect(store.get(16, 2, 3)).toBe(7);
    // Same version again → no change; newer version → change.
    expect(store.learn([seven])).toEqual([]);
    expect(store.learn([{ key: "16/2/3", version: 8 }])).toEqual(["16/2/3"]);
    // Foreign keys are dropped silently.
    expect(
      store.learn([
        { key: "nope", version: 1 },
        { key: "16/2", version: 1 },
      ]),
    ).toEqual([]);
  });

  it("refreshSnapshot fetches with the bbox query and merges", async () => {
    const fetchVersions = vi.fn().mockResolvedValue({
      tiles: [
        { key: "16/1/1", version: 5 },
        { key: "16/1/2", version: 6 },
      ],
    });
    const store = createTileVersionsStore({ fetchVersions });
    const bbox = { x: 1000, y: 2000, w: 4096, h: 4096 };
    await store.refreshSnapshot(bbox, 16);
    expect(fetchVersions).toHaveBeenCalledWith("bbox=1000,2000,4096,4096&z=16");
    expect(store.get(16, 1, 1)).toBe(5);
    expect(store.areaQuery()).toBe("bbox=1000,2000,4096,4096&z=16");
  });

  it("refreshSnapshot survives network and malformed bodies", async () => {
    const fetchVersions = vi
      .fn()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValueOnce({ bogus: "not a map" })
      .mockResolvedValueOnce({ tiles: [{ key: "16/9/9", version: 3 }] });
    const store = createTileVersionsStore({ fetchVersions });
    const bbox = { x: 0, y: 0, w: 100, h: 100 };
    // Network error → swallowed.
    await store.refreshSnapshot(bbox, 16);
    expect(store.get(16, 9, 9)).toBeUndefined();
    // Malformed → swallowed.
    await store.refreshSnapshot(bbox, 16);
    expect(store.get(16, 9, 9)).toBeUndefined();
    // Good body → learned.
    await store.refreshSnapshot(bbox, 16);
    expect(store.get(16, 9, 9)).toBe(3);
  });

  it("refreshSnapshot with a null bbox never fetches", async () => {
    const fetchVersions = vi.fn();
    const store = createTileVersionsStore({ fetchVersions });
    await store.refreshSnapshot(null, 16);
    expect(fetchVersions).not.toHaveBeenCalled();
    expect(store.areaQuery()).toBe("");
  });

  it("shouldResubscribe on zoom change, first area, and large pans", async () => {
    const fetchVersions = vi.fn().mockResolvedValue({ tiles: [] });
    const store = createTileVersionsStore({ fetchVersions });
    // No area yet → resubscribe (first snapshot).
    const view = { x: 1000, y: 1000, w: 4096, h: 4096 };
    expect(store.shouldResubscribe(view, 16)).toBe(true);
    await store.refreshSnapshot(view, 16);
    // Same area, same zoom → no.
    expect(store.shouldResubscribe(view, 16)).toBe(false);
    // Zoom changed → yes.
    expect(store.shouldResubscribe(view, 15)).toBe(true);
    // Slightly moved (within the 25% pad) → no.
    const small = { x: view.x + 500, y: view.y + 500, w: 4096, h: 4096 };
    expect(store.shouldResubscribe(small, 16)).toBe(false);
    // Far moved (outside the padded area) → yes.
    const far = { x: view.x + 4096, y: view.y, w: 4096, h: 4096 };
    expect(store.shouldResubscribe(far, 16)).toBe(true);
  });

  it("clear drops versions and the area", async () => {
    const fetchVersions = vi
      .fn()
      .mockResolvedValue({ tiles: [{ key: "16/1/1", version: 5 }] });
    const store = createTileVersionsStore({ fetchVersions });
    await store.refreshSnapshot({ x: 0, y: 0, w: 1, h: 1 }, 16);
    expect(store.get(16, 1, 1)).toBe(5);
    store.clear();
    expect(store.get(16, 1, 1)).toBeUndefined();
    expect(store.areaQuery()).toBe("");
    expect(store.shouldResubscribe({ x: 0, y: 0, w: 1, h: 1 }, 16)).toBe(true);
  });
});
