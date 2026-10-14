/**
 * Client-side tile-version store for the versioned read path
 * (docs/scaling-architecture.md, Read path). Versions arrive two ways:
 * the viewport snapshot (GET /api/versions) on (re)connect and after
 * large pans, and batched `versions` SSE events (1s ticks, z-filtered to
 * the subscribed area). Tile URLs carry the learned version as `?v=` —
 * a hint backed by the same counter as the tile etag — so browsers and
 * the edge cache immutable tiles per version instead of re-fetching per
 * viewer. Until the backend's versions lane ships, the store simply has
 * no entries and tile URLs stay version-less (the server's legacy
 * max-age=1 row), which keeps the staggered deploy safe.
 */

import { tileVersionsResponseSchema, type TileVersionEntry } from "@contract";
import { clampToWorld, type BBox } from "./world";

/** The subscribed area grows by this margin before a pan counts as
 * "large" and triggers a resubscribe + snapshot refetch. */
const AREA_PAD_FRACTION = 0.25;

export interface TileVersionsStore {
  /** Merge batched versions (SSE event or snapshot). Returns the keys
   * whose version changed (all of them on first sight of a tile). */
  learn(tiles: TileVersionEntry[]): string[];
  /** Learned version for a tile, if any. */
  get(z: number, x: number, y: number): number | undefined;
  /** Fetch the snapshot for a viewport and merge it, remembering the
   * area as subscribed. Null bbox (map not ready) or a malformed body
   * is a no-op; the next trigger retries. */
  refreshSnapshot(bbox: BBox | null, z: number): Promise<void>;
  /** Query string ("bbox=…&z=…") for /api/versions and /api/events —
   * empty until the first snapshot establishes the area (subscribe-all,
   * the behavior of pre-versions backends). */
  areaQuery(): string;
  /** True when the viewport has left the subscribed area (padded) or
   * the tile zoom changed — the "large pan" resubscribe trigger. */
  shouldResubscribe(bbox: BBox, z: number): boolean;
  /** Drop everything (stream gave up; the next snapshot rebuilds). */
  clear(): void;
}

/** "z/x/y" → integer tile coords; null for foreign keys. */
export function parseVersionKey(key: string): {
  z: number;
  x: number;
  y: number;
} | null {
  const parts = key.split("/");
  if (parts.length !== 3) return null;
  const [z, x, y] = parts.map(Number);
  if (
    z === undefined ||
    x === undefined ||
    y === undefined ||
    !Number.isInteger(z) ||
    !Number.isInteger(x) ||
    !Number.isInteger(y)
  ) {
    return null;
  }
  return { z, x, y };
}

/** Pad a bbox by a fraction of its size on every side, clipped to the
 * world square so containment checks stay exact at the edges. */
function padBBox(bbox: BBox, fraction: number): BBox {
  const padX = bbox.w * fraction;
  const padY = bbox.h * fraction;
  const x = clampToWorld(bbox.x - padX);
  const y = clampToWorld(bbox.y - padY);
  return { x, y, w: bbox.w + padX * 2, h: bbox.h + padY * 2 };
}

export function createTileVersionsStore(options: {
  fetchVersions: (query: string) => Promise<unknown>;
}): TileVersionsStore {
  const versions = new Map<string, number>();
  let area: { bbox: BBox; z: number } | null = null;

  const store: TileVersionsStore = {
    learn(tiles: TileVersionEntry[]): string[] {
      const changed: string[] = [];
      for (const { key, version } of tiles) {
        if (parseVersionKey(key) === null) continue;
        if (versions.get(key) !== version) {
          versions.set(key, version);
          changed.push(key);
        }
      }
      return changed;
    },

    get(z: number, x: number, y: number): number | undefined {
      return versions.get(`${z}/${x}/${y}`);
    },

    async refreshSnapshot(bbox: BBox | null, z: number): Promise<void> {
      if (bbox === null) return;
      const query = `bbox=${bbox.x},${bbox.y},${bbox.w},${bbox.h}&z=${z}`;
      let body: unknown;
      try {
        body = await options.fetchVersions(query);
      } catch {
        // Network blip: tiles stay version-less; the next trigger retries.
        return;
      }
      const parsed = tileVersionsResponseSchema.safeParse(body);
      // Malformed body: degrade silently; the caller can warn.
      if (!parsed.success) return;
      area = { bbox: { ...bbox }, z };
      store.learn(parsed.data.tiles);
    },

    areaQuery(): string {
      if (area === null) return "";
      const { bbox, z } = area;
      return `bbox=${bbox.x},${bbox.y},${bbox.w},${bbox.h}&z=${z}`;
    },

    shouldResubscribe(bbox: BBox, z: number): boolean {
      if (area === null) return true;
      if (area.z !== z) return true;
      const padded = padBBox(area.bbox, AREA_PAD_FRACTION);
      return !(
        bbox.x >= padded.x &&
        bbox.y >= padded.y &&
        bbox.x + bbox.w <= padded.x + padded.w &&
        bbox.y + bbox.h <= padded.y + padded.h
      );
    },

    clear(): void {
      versions.clear();
      area = null;
    },
  };

  return store;
}
