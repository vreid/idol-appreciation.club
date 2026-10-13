import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";
import {
  affectedRenderedTiles,
  createTileRefresher,
  TILE_REFRESH_WINDOW_MS,
  type TileRefreshSource,
} from "./tileRefresh";
import type { BBox } from "./world";

/** Rendered keys for tiles [x0..x1]×[y0..y1] at zoom 16 (Leaflet order). */
function keys16(x0: number, x1: number, y0: number, y1: number): string[] {
  const keys: string[] = [];
  for (let x = x0; x <= x1; x++) {
    for (let y = y0; y <= y1; y++) keys.push(`${x}:${y}:16`);
  }
  return keys;
}

/** Native-px bbox of tile (x, y) at z16 (4096px edges). */
function tile16(x: number, y: number): BBox {
  return { x: x * 4096, y: y * 4096, w: 4096, h: 4096 };
}

/** Source over a fixed 2×2 grid of z16 tiles with a full-grid viewport. */
function gridSource(): TileRefreshSource & { keys: string[] } {
  const source = {
    keys: keys16(10, 11, 10, 11),
    renderedTileKeys: () => source.keys,
    viewport: () => ({ x: 0, y: 0, w: 2 ** 28, h: 2 ** 28 }),
  };
  return source;
}

describe("affectedRenderedTiles", () => {
  it("dedupes tile coords across overlapping bboxes", () => {
    const source = gridSource();
    const hits = affectedRenderedTiles(source, [
      tile16(10, 10),
      tile16(10, 10),
    ]);
    expect(hits).toEqual(["10:10:16"]);
  });

  it("drops tiles outside the viewport", () => {
    const source = gridSource();
    source.viewport = () => tile16(11, 11);
    const hits = affectedRenderedTiles(source, [
      tile16(10, 10),
      tile16(11, 11),
    ]);
    expect(hits).toEqual(["11:11:16"]);
  });

  it("refreshes every tile when the viewport is unknown", () => {
    const source = gridSource();
    source.viewport = () => null;
    const hits = affectedRenderedTiles(source, [
      { x: 0, y: 0, w: 2 ** 28, h: 2 ** 28 },
    ]);
    expect(hits).toEqual(keys16(10, 11, 10, 11));
  });
});

function harness(source = gridSource()) {
  const refreshed: string[][] = [];
  const refresher = createTileRefresher(
    source,
    (keys) => {
      refreshed.push(keys);
    },
    TILE_REFRESH_WINDOW_MS,
  );
  return { refresher, refreshed, source };
}

describe("createTileRefresher", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not refresh before the window elapses", () => {
    const { refresher, refreshed } = harness();
    refresher.queue(tile16(10, 10));
    vi.advanceTimersByTime(TILE_REFRESH_WINDOW_MS - 1);
    expect(refreshed).toEqual([]);
    expect(refresher.pending()).toBe(1);
  });

  it("flushes once per window with deduped tile coords", () => {
    const { refresher, refreshed } = harness();
    // Storm: five events in one window over the same two tiles.
    refresher.queue(tile16(10, 10));
    refresher.queue(tile16(11, 11));
    refresher.queue(tile16(10, 10));
    refresher.queue(tile16(11, 11));
    refresher.queue(tile16(10, 10));
    vi.advanceTimersByTime(TILE_REFRESH_WINDOW_MS);
    expect(refreshed).toEqual([["10:10:16", "11:11:16"]]);
    expect(refresher.pending()).toBe(0);
    // No trailing second flush.
    vi.advanceTimersByTime(TILE_REFRESH_WINDOW_MS * 3);
    expect(refreshed).toHaveLength(1);
  });

  it("starts a new window for events after a flush", () => {
    const { refresher, refreshed } = harness();
    refresher.queue(tile16(10, 10));
    vi.advanceTimersByTime(TILE_REFRESH_WINDOW_MS);
    expect(refreshed).toHaveLength(1);
    refresher.queue(tile16(11, 11));
    vi.advanceTimersByTime(TILE_REFRESH_WINDOW_MS);
    expect(refreshed).toEqual([["10:10:16"], ["11:11:16"]]);
  });

  it("manual flush() fires immediately and cancels the timer", () => {
    const { refresher, refreshed } = harness();
    refresher.queue(tile16(10, 11));
    refresher.flush();
    expect(refreshed).toEqual([["10:11:16"]]);
    vi.advanceTimersByTime(TILE_REFRESH_WINDOW_MS * 2);
    expect(refreshed).toHaveLength(1);
    expect(refresher.pending()).toBe(0);
  });

  it("dispose() drops pending work and ignores later queues", () => {
    const { refresher, refreshed } = harness();
    refresher.queue(tile16(10, 10));
    refresher.dispose();
    expect(refresher.pending()).toBe(0);
    refresher.queue(tile16(11, 11));
    vi.advanceTimersByTime(TILE_REFRESH_WINDOW_MS * 2);
    expect(refreshed).toEqual([]);
  });

  it("flush() with nothing pending never calls refresh", () => {
    const { refresher, refreshed } = harness();
    refresher.flush();
    expect(refreshed).toEqual([]);
  });
});
