/**
 * World coordinate system.
 *
 * The world is a native pixel space of 2^28 x 2^28 px (256 px tiles at zoom
 * 20). x grows right, y grows DOWN (image convention, y=0 is the top row).
 *
 * Leaflet's L.CRS.Simple maps (lat, lng) linearly to screen px:
 *   screenX = lng * 256 * 2^zoom
 *   screenY = -lat * 256 * 2^zoom   (lat grows UP)
 *
 * CRS.Simple projects one lat/lng unit to 2^zoom screen px, so the world must
 * span 256 units for Leaflet's 256px tile grid to line up with the server:
 *   lng = x / 2^20, lat = -y / 2^20   (world = 2^20 units = 256 units/side)
 * gives screenX = x * 2^(z-20), tiles/side = 2^z and therefore
 *   tileIndex(z) = floor(worldPx / (256 * 2^(20-z)))  — the server formula.
 * The y-flip lives entirely in these two helpers; all other app math stays in
 * native world px.
 */

export const WORLD_SIZE = 2 ** 28;
export const WORLD_CENTER = WORLD_SIZE / 2;
export const TILE_SIZE = 256;
export const MIN_ZOOM = 0;
export const MAX_ZOOM = 20;

/** The world spans 256 CRS.Simple units: 1 unit = one tile edge at z0. */
export const WORLD_UNITS = WORLD_SIZE / TILE_SIZE;

/** World extent in LatLng space, y-flip aware: SW [-256, 0], NE [0, 256]. */
export const WORLD_LAT_LNG_BOUNDS: [[number, number], [number, number]] = [
  [-TILE_SIZE, 0],
  [0, TILE_SIZE],
];

/** Screen size (CSS px) of the full world at a given zoom. */
export function worldScreenSize(zoom: number): number {
  return TILE_SIZE * 2 ** zoom;
}

/** True when the whole world fits inside a viewport at the given zoom. */
export function worldFitsViewport(
  width: number,
  height: number,
  zoom: number,
): boolean {
  const size = worldScreenSize(zoom);
  return size <= width && size <= height;
}

export interface WorldPoint {
  x: number;
  y: number;
}

export interface BBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface LatLng {
  lat: number;
  lng: number;
}

export function worldToLatLng(x: number, y: number): LatLng {
  // Normalize -0 at the top edge so the exported bounds compare cleanly.
  return { lat: y === 0 ? 0 : -y / WORLD_UNITS, lng: x / WORLD_UNITS };
}

export function latLngToWorld(lat: number, lng: number): WorldPoint {
  return { x: lng * WORLD_UNITS, y: lat === 0 ? 0 : -lat * WORLD_UNITS };
}

/**
 * Server tile formula: the index of the tile covering native worldPx at the
 * given zoom — floor(worldPx / (256 * 2^(20-z))). Keep in sync with
 * GET /api/tiles/{z}/{x}/{y}.webp.
 */
export function tileIndexAt(zoom: number, worldPx: number): number {
  return Math.floor(worldPx / (TILE_SIZE * 2 ** (MAX_ZOOM - zoom)));
}

/** Screen px per native world px at a given zoom. */
export function worldScale(zoom: number): number {
  return 2 ** (zoom - MAX_ZOOM);
}

export function clampToWorld(value: number): number {
  return Math.min(Math.max(value, 0), WORLD_SIZE);
}

export function clampToWorldPoint(p: WorldPoint): WorldPoint {
  return { x: clampToWorld(p.x), y: clampToWorld(p.y) };
}

export function tileUrl(z: number, x: number, y: number): string {
  return `/api/tiles/${z}/${x}/${y}.webp`;
}

/** Native-px bbox covered by tile (x, y) at zoom z. */
export function nativeTileBBox(z: number, x: number, y: number): BBox {
  const edge = WORLD_SIZE / 2 ** z;
  return { x: x * edge, y: y * edge, w: edge, h: edge };
}

export function bboxesIntersect(a: BBox, b: BBox): boolean {
  return (
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
  );
}

/** Leaflet tile keys look like "z:x:y". Returns null for other shapes. */
export function parseTileKey(key: string): (WorldPoint & { z: number }) | null {
  const parts = key.split(":");
  if (parts.length !== 3) return null;
  const z = Number(parts[0]);
  const x = Number(parts[1]);
  const y = Number(parts[2]);
  if (![z, x, y].every((n) => Number.isInteger(n) && n >= 0)) return null;
  return { z, x, y };
}

/**
 * Visible tiles (by Leaflet tile key) whose native bbox intersects the given
 * world bbox — the set to refetch after a place/remove SSE event.
 */
export function tilesIntersecting(keys: string[], bbox: BBox): string[] {
  return keys.filter((key) => {
    const tile = parseTileKey(key);
    if (!tile) return false;
    return bboxesIntersect(nativeTileBBox(tile.z, tile.x, tile.y), bbox);
  });
}

export function bboxContainsPoint(b: BBox, x: number, y: number): boolean {
  return x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h;
}

/**
 * Search radius (native px) for the "placements near" popup query, scaled to
 * zoom and clamped to [256, 65536].
 */
export function queryRadiusForZoom(zoom: number): number {
  const r = TILE_SIZE * 2 ** (16 - zoom);
  return Math.min(Math.max(r, 256), 65536);
}
