/**
 * Coalescing tile refresh for place/remove SSE storms: events queue
 * world-space AABBs, and at most one refresh per window recomputes the
 * affected tiles — deduped by tile coord, limited to the current zoom's
 * rendered tiles and the live viewport — then refetches each once.
 */

import {
  bboxesIntersect,
  nativeTileBBox,
  parseTileKey,
  tilesIntersecting,
  type BBox,
} from "./world";

/** Where rendered-tile state comes from (implemented over the Leaflet map). */
export interface TileRefreshSource {
  /** Rendered tile keys ("x:y:z") at the map's current tile zoom. */
  renderedTileKeys(): string[];
  /** Viewport as a native-px bbox; null disables the viewport filter. */
  viewport(): BBox | null;
}

export interface TileRefresher {
  /** Buffer a world-space AABB (place or remove event). */
  queue(bbox: BBox): void;
  /** Refresh pending bboxes immediately and clear the window timer. */
  flush(): void;
  /** Bboxes buffered but not yet flushed. */
  pending(): number;
  /** Drop buffered state and stop the timer; queue() becomes a no-op. */
  dispose(): void;
}

export const TILE_REFRESH_WINDOW_MS = 500;

/**
 * Rendered tile keys hit by any of the bboxes, deduped, and restricted to
 * the viewport when the source provides one.
 */
export function affectedRenderedTiles(
  source: TileRefreshSource,
  bboxes: BBox[],
): string[] {
  const rendered = source.renderedTileKeys();
  const viewport = source.viewport();
  const seen = new Set<string>();
  const hits: string[] = [];
  for (const bbox of bboxes) {
    for (const key of tilesIntersecting(rendered, bbox)) {
      if (seen.has(key)) continue;
      const tile = parseTileKey(key);
      if (tile === null) continue;
      if (
        viewport !== null &&
        !bboxesIntersect(nativeTileBBox(tile.z, tile.x, tile.y), viewport)
      ) {
        continue;
      }
      seen.add(key);
      hits.push(key);
    }
  }
  return hits;
}

/**
 * Trailing-window coalescer: the first queued bbox schedules a single
 * refresh `windowMs` out; further events inside the window just buffer.
 * The affected-tile computation runs at flush time so events that land
 * during a pan/zoom refresh the tiles actually on screen afterwards.
 */
export function createTileRefresher(
  source: TileRefreshSource,
  refresh: (keys: string[]) => void,
  windowMs: number = TILE_REFRESH_WINDOW_MS,
): TileRefresher {
  let pending: BBox[] = [];
  let timer: ReturnType<typeof setTimeout> | null = null;
  let disposed = false;

  const clearTimer = (): void => {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  };

  const flush = (): void => {
    clearTimer();
    if (disposed || pending.length === 0) return;
    const hits = affectedRenderedTiles(source, pending);
    pending = [];
    if (hits.length > 0) refresh(hits);
  };

  return {
    queue(bbox: BBox): void {
      if (disposed) return;
      pending.push(bbox);
      timer ??= setTimeout(flush, windowMs);
    },
    flush,
    pending: () => pending.length,
    dispose(): void {
      disposed = true;
      clearTimer();
      pending = [];
    },
  };
}
