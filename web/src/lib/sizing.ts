/**
 * Image sizing rules for the place flow: the long edge is chosen by the
 * user (16..2048), the short edge follows the natural aspect ratio.
 */

export const MIN_LONG_EDGE = 16;
export const MAX_LONG_EDGE = 2048;
export const DEFAULT_LONG_EDGE = 256;

export function clampLongEdge(value: number): number {
  const rounded = Math.round(value);
  return Math.min(Math.max(rounded, MIN_LONG_EDGE), MAX_LONG_EDGE);
}

/**
 * Target w x h in native px for an image with the given natural size when
 * its long edge is clamped to the allowed range. Never collapses a
 * dimension below 1 px.
 */
export function longEdgeToSize(
  naturalW: number,
  naturalH: number,
  longEdge: number,
): { w: number; h: number } {
  const edge = clampLongEdge(longEdge);
  if (naturalW <= 0 || naturalH <= 0) return { w: edge, h: edge };
  const longest = Math.max(naturalW, naturalH);
  const scale = edge / longest;
  const w = Math.max(1, Math.round(naturalW * scale));
  const h = Math.max(1, Math.round(naturalH * scale));
  return { w, h };
}

export function isUploadSizeOk(bytes: number): boolean {
  return bytes > 0 && bytes <= 10 * 1024 * 1024;
}
