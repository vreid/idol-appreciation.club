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
 * Refetch the given rendered tiles IN PLACE with their current versions:
 * the URL changes when the version changed, which forces a real GET (the
 * server's fallback table serves current bytes for a stale hint with
 * `max-age=60`), while the browser keeps showing the old bitmap until the
 * new bytes decode: no blanking, no flicker. A tile whose version is
 * unknown keeps the legacy cache-busting query — that fallback dies with
 * the versions backend lane (roadmap #163); until then it preserves
 * today's refresh semantics against pre-versions backends.
 */
export function refetchTiles(
  tileLayer: L.TileLayer,
  keys: string[],
  versionOf: (z: number, x: number, y: number) => number | undefined = () => {},
): void {
  const tiles = renderedTileMap(tileLayer);
  for (const key of keys) {
    const el = tileImageOf(tiles, key);
    const tile = parseTileKey(key);
    if (el === null || tile === null) continue;
    const version = versionOf(tile.z, tile.x, tile.y);
    el.src =
      version === undefined
        ? `${tileUrl(tile.z, tile.x, tile.y)}?r=${Date.now()}`
        : tileUrl(tile.z, tile.x, tile.y, version);
  }
}
