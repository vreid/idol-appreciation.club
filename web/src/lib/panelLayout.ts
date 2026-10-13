/**
 * Layout math for the anchored place-detail panel. Pure so the never-
 * covers-the-selection invariant is unit-testable without a DOM.
 *
 * The panel sits BESIDE the selected placement's rotated AABB (right side
 * preferred), expanding downward from near the image's top edge, and
 * flips to the left side when the right placement would overflow the
 * viewport. `PANEL_GAP` separates panel and AABB edges in both states —
 * the invariant the e2e asserts.
 */

export interface ScreenRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Size {
  w: number;
  h: number;
}

export interface PanelLayout {
  left: number;
  top: number;
  /** Which side of the selection the panel landed on. */
  side: "right" | "left";
}

export const PANEL_GAP = 12;
export const PANEL_MARGIN = 12;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), Math.max(min, max));
}

export function panelLayout(
  aabb: ScreenRect,
  panel: Size,
  viewport: Size,
): PanelLayout {
  const maxTop = viewport.h - PANEL_MARGIN - panel.h;
  const top = clamp(aabb.y, PANEL_MARGIN, Math.max(PANEL_MARGIN, maxTop));

  const rightLeft = aabb.x + aabb.w + PANEL_GAP;
  if (rightLeft + panel.w <= viewport.w - PANEL_MARGIN) {
    return { left: rightLeft, top, side: "right" };
  }

  const leftLeft = aabb.x - PANEL_GAP - panel.w;
  if (leftLeft >= PANEL_MARGIN) {
    return { left: leftLeft, top, side: "left" };
  }

  // Neither side fits (viewport narrower than image + 2×(gap+panel)):
  // keep the wider side and clamp inside the viewport. On such extremes
  // the overlap-free guarantee degrades — documented, unreachable in
  // supported viewports.
  const roomRight = viewport.w - (aabb.x + aabb.w);
  const roomLeft = aabb.x;
  if (roomRight >= roomLeft) {
    return {
      left: clamp(rightLeft, PANEL_MARGIN, viewport.w - PANEL_MARGIN - panel.w),
      top,
      side: "right",
    };
  }
  return {
    left: clamp(leftLeft, PANEL_MARGIN, viewport.w - PANEL_MARGIN - panel.w),
    top,
    side: "left",
  };
}
