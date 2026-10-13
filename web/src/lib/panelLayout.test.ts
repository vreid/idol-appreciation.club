import { describe, expect, it } from "vite-plus/test";
import {
  PANEL_GAP,
  PANEL_MARGIN,
  panelLayout,
  type ScreenRect,
} from "./panelLayout";

const VIEWPORT = { w: 1280, h: 800 };
const PANEL = { w: 220, h: 96 };

function aabb(x: number, y: number, w = 64, h = 64): ScreenRect {
  return { x, y, w, h };
}

/** The panel rect must stay clear of the AABB by at least the gap. */
function expectNeverCovers(
  layout: { left: number; top: number },
  image: ScreenRect,
): void {
  const panelRight = layout.left + PANEL.w;
  const panelBottom = layout.top + PANEL.h;
  const clearHorizontally =
    panelRight <= image.x - PANEL_GAP + 0.001 ||
    layout.left >= image.x + image.w + PANEL_GAP - 0.001;
  const clearVertically =
    panelBottom <= image.y - PANEL_GAP + 0.001 ||
    layout.top >= image.y + image.h + PANEL_GAP - 0.001;
  expect(
    clearHorizontally || clearVertically,
    `${JSON.stringify(layout)} vs ${JSON.stringify(aabb)}`,
  ).toBe(true);
}

describe("panelLayout", () => {
  it("prefers the right side, aligned with the image top", () => {
    const layout = panelLayout(aabb(600, 300), PANEL, VIEWPORT);
    expect(layout.side).toBe("right");
    expect(layout.left).toBe(600 + 64 + PANEL_GAP);
    expect(layout.top).toBe(300);
  });

  it("flips to the left side near the right viewport edge", () => {
    const layout = panelLayout(aabb(VIEWPORT.w - 80, 300), PANEL, VIEWPORT);
    expect(layout.side).toBe("left");
    expect(layout.left).toBe(VIEWPORT.w - 80 - PANEL_GAP - PANEL.w);
  });

  it("clamps the panel top inside the viewport", () => {
    const layout = panelLayout(aabb(600, 4), PANEL, VIEWPORT);
    expect(layout.top).toBe(PANEL_MARGIN);
    const low = panelLayout(aabb(600, VIEWPORT.h - 20), PANEL, VIEWPORT);
    expect(low.top).toBe(VIEWPORT.h - PANEL_MARGIN - PANEL.h);
  });

  it("keeps the gap on the flip side too", () => {
    const a = aabb(VIEWPORT.w - 80, 300);
    const layout = panelLayout(a, PANEL, VIEWPORT);
    expectNeverCovers(layout, a);
  });

  it("never covers the selection across boundary positions", () => {
    const xs = [
      0,
      1,
      PANEL_MARGIN,
      320,
      640,
      VIEWPORT.w - 81,
      VIEWPORT.w - 80,
      VIEWPORT.w - 1,
    ];
    const ys = [0, 1, PANEL_MARGIN, 300, 640, VIEWPORT.h - 21, VIEWPORT.h - 1];
    for (const x of xs) {
      for (const y of ys) {
        const a = aabb(x, y);
        expectNeverCovers(panelLayout(a, PANEL, VIEWPORT), a);
      }
    }
  });

  it("never covers for tall and wide images alike", () => {
    for (const a of [
      aabb(500, 200, 400, 300),
      aabb(500, 200, 900, 40),
      aabb(200, 200, 40, 560),
    ]) {
      expectNeverCovers(panelLayout(a, PANEL, VIEWPORT), a);
    }
  });
});
