/**
 * Leaflet tile-cache access: the undocumented, protected `Grid._tiles`
 * index (keyed "x:y:z", one zoom level at a time), viewport conversion and
 * the in-place tile refetch used by live SSE-driven refreshes.
 */

import type L from "leaflet";
import { isRecord } from "./guards";
import {
  latLngBoundsToWorldBBox,
  parseTileKey,
  tileUrl,
  type BBox,
} from "./world";

export function tileImageOf(
  tiles: Record<string, unknown>,
  key: string,
): HTMLImageElement | null {
  const record: unknown = tiles[key];
  if (!isRecord(record)) return null;
  const el = record["el"];
  return el instanceof HTMLImageElement ? el : null;
}

/**
 * Leaflet keeps rendered tiles in the `_tiles` index; read it through a
 * widened alias — the shape is re-validated because the library does not
 * publish it.
 */
export function renderedTileMap(
  tileLayer: L.TileLayer,
): Record<string, unknown> {
  const internal: object = tileLayer;
  if (!("_tiles" in internal)) return {};
  const tiles: unknown = internal["_tiles"];
  return isRecord(tiles) ? tiles : {};
}

/** Viewport as a native-px world bbox (null when the map has no view yet). */
export function viewWorldBBox(map: L.Map): BBox | null {
  const bounds = map.getBounds();
  const viewport = latLngBoundsToWorldBBox(
    bounds.getNorth(),
    bounds.getEast(),
    bounds.getSouth(),
    bounds.getWest(),
  );
  return viewport.w > 0 && viewport.h > 0 ? viewport : null;
}

/**
 * Refetch the given rendered tiles IN PLACE. The refresh URL is
 * cache-busted: a plain same-URL reassignment would be served from the
 * HTTP cache's stale-while-revalidate grace (stale pixels now, fresh bytes
 * into the cache only), so the visible update would lag one refresh behind.
 * A unique query forces a real GET — 200 with the current tile bytes —
 * while the browser keeps showing the old bitmap until the new bytes
 * decode: no blanking, no flicker.
 */
export function refetchTiles(tileLayer: L.TileLayer, keys: string[]): void {
  const tiles = renderedTileMap(tileLayer);
  const bust = Date.now();
  for (const key of keys) {
    const el = tileImageOf(tiles, key);
    const tile = parseTileKey(key);
    if (el !== null && tile !== null) {
      el.src = `${tileUrl(tile.z, tile.x, tile.y)}?r=${bust}`;
    }
  }
}
