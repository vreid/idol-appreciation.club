import { MAX_ZOOM, MIN_ZOOM } from "./world";

/**
 * Map view state encoded in the URL hash: `#z/x/y`.
 *
 * `z` is the zoom; `x`/`y` are SIGNED world-px offsets of the view centre from
 * the world centre (134217728, 134217728). y grows down (image convention), so
 * `#18/-1280/512` means zoom 18, centred 1280 px left of and 512 px below 0/0.
 */
export interface UrlView {
  z: number;
  x: number;
  y: number;
}

const VIEW_HASH = /^#(-?\d+)\/(-?\d+)\/(-?\d+)$/u;

function clampZoom(z: number): number {
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, z));
}

/** Round to int, normalising -0 so hashes never read "#16/0/-0". */
function roundedInt(v: number): number {
  const rounded = Math.round(v);
  return rounded === 0 ? 0 : rounded;
}

/** Parse a `#z/x/y` hash; null when missing or malformed. Zoom is clamped. */
export function parseViewHash(hash: string): UrlView | null {
  const match = VIEW_HASH.exec(hash);
  if (match === null) return null;
  return {
    z: clampZoom(Number(match[1])),
    x: Number(match[2]),
    y: Number(match[3]),
  };
}

/** Encode a view as a `#z/x/y` hash, rounding to integer px. */
export function encodeViewHash(view: UrlView): string {
  return (
    `#${clampZoom(Math.round(view.z))}/` +
    `${roundedInt(view.x)}/${roundedInt(view.y)}`
  );
}
