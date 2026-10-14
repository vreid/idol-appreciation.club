import { expect, test, type Page } from "@playwright/test";
import { PNG } from "pngjs";
import { WORLD_CENTER, WORLD_SIZE, worldScreenSize } from "../src/lib/world";

// ---- explicit knobs (vetoable) -----------------------------------------
// After `networkidle`, allow tile decode/paint to finish. 1200ms covers a
// known-good local run with margin; CI runners are slower and colder, which
// is why the ASSERTION threshold below carries the slack instead.
const SETTLE_MS = 1_200;
// Primary gate: fraction of non-background pixels inside the world rect.
// Known-good runs sit ~97%+; 0.90 leaves room for CI variance while still
// catching the live failure class (tiles that never arrive).
const NON_GRAY_THRESHOLD = 0.9;
// The canvas background (.map-root) — what a missing tile shows as.
const GRAY = { r: 16, g: 18, b: 22 };
const CHANNEL_TOLERANCE = 6;
// Secondary budget: time from navigation to a mostly-complete view.
const RESOLVE_BUDGET_MS = 5_000;
// Main-thread heartbeat budget while zooming (vetoable).
const FRAME_GAP_BUDGET_MS = 500;
// Zooms exercised, dense-pile centre to wide.
const ZOOMS = [4, 8, 13, 16] as const;

/** Screen rect of the world at `zoom` for the view `#zoom/x/y`. */
function worldRect(
  zoom: number,
  x: number,
  y: number,
  vp: { width: number; height: number },
): { x0: number; y0: number; x1: number; y1: number } {
  const worldPx = worldScreenSize(zoom);
  const scale = worldPx / WORLD_SIZE;
  const originX = vp.width / 2 - (WORLD_CENTER + x) * scale;
  const originY = vp.height / 2 - (WORLD_CENTER + y) * scale;
  return {
    x0: originX,
    y0: originY,
    x1: originX + worldPx,
    y1: originY + worldPx,
  };
}

/** Fraction of pixels that are NOT the background gray, counted inside
 * the world rect only (off-world gray is legitimate empty canvas). */
function nonGrayFraction(
  shot: Buffer,
  rect: { x0: number; y0: number; x1: number; y1: number },
): number {
  const png = PNG.sync.read(shot);
  const x0 = Math.max(0, Math.floor(rect.x0));
  const y0 = Math.max(0, Math.floor(rect.y0));
  const x1 = Math.min(png.width, Math.ceil(rect.x1));
  const y1 = Math.min(png.height, Math.ceil(rect.y1));
  let total = 0;
  let nonGray = 0;
  for (let y = y0; y < y1; y++) {
    for (let x = x0; x < x1; x++) {
      const i = (png.width * y + x) << 2;
      const r = png.data[i]!;
      const g = png.data[i + 1]!;
      const b = png.data[i + 2]!;
      total++;
      const gray =
        Math.abs(r - GRAY.r) <= CHANNEL_TOLERANCE &&
        Math.abs(g - GRAY.g) <= CHANNEL_TOLERANCE &&
        Math.abs(b - GRAY.b) <= CHANNEL_TOLERANCE;
      if (!gray) nonGray++;
    }
  }
  return total === 0 ? 0 : nonGray / total;
}

/**
 * Density-torture proxy: the invariant under guard is that VIEW cost is
 * depth-independent — tiles derive from stored rasters, never from
 * placement walks — so a several-thousand-placement pile must render
 * identically to a sparse one at every zoom.
 *
 * The pile density comes from the near/dig stubs (import endpoints need
 * admin auth; stubbing is the CI-viable harness), while the TILES are the
 * real ones for the region: exactly the split that makes a regression in
 * either the tile path or the client trip this spec.
 */
async function stubDensePile(page: Page): Promise<void> {
  // A few thousand placements stacked inside one chunk region: enough
  // to make any per-placement client work obvious, bounded to keep the
  // spec's runtime flat.
  const dense = Array.from({ length: 3_000 }, (_, i) => ({
    id: `dense-${i}`,
    username: "loadtest",
    x: WORLD_CENTER - 128 + (i % 8),
    y: WORLD_CENTER - 128 + (Math.floor(i / 8) % 8),
    w: 1024,
    h: 1024,
    rot: 0,
    created_at: "2026-10-14T00:00:00Z",
    seq: i,
    tags: [],
  }));
  await page.route("**/api/placements/near**", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(dense.slice(0, 64)),
    }),
  );
  await page.route("**/api/dig**", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        stack: dense.slice(0, 64),
        total: dense.length,
        truncated: true,
      }),
    }),
  );
}

const wait = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

test.describe("scale density", () => {
  // The centre pile.
  const VIEW = { x: 0, y: 0 };

  for (const zoom of ZOOMS) {
    test(`viewport completeness at z${zoom}`, async ({ page }) => {
      await stubDensePile(page);
      const t0 = Date.now();
      await page.goto(`/#${zoom}/0/0`);
      await page.waitForLoadState("networkidle");
      await page.waitForTimeout(SETTLE_MS);

      const vp = page.viewportSize() ?? { width: 1280, height: 720 };
      const rect = worldRect(zoom, VIEW.x, VIEW.y, vp);
      const shot = await page.screenshot();
      const fraction = nonGrayFraction(shot, rect);

      // Primary: the viewport is actually painted inside the world.
      expect(
        fraction,
        `z${zoom}: only ${(fraction * 100).toFixed(1)}% of the world rect is painted (tiles missing?)`,
      ).toBeGreaterThanOrEqual(NON_GRAY_THRESHOLD);

      // Secondary: the view resolved within budget (vetoable).
      const resolveMs = Date.now() - t0;
      expect(
        resolveMs,
        `z${zoom} took ${resolveMs}ms to navigate+settle`,
      ).toBeLessThanOrEqual(RESOLVE_BUDGET_MS);
    });
  }

  test("zooming the dense pile keeps the main thread responsive", async ({
    page,
  }) => {
    await stubDensePile(page);
    await page.goto("/#16/0/0");
    await page.waitForLoadState("networkidle");
    await page.waitForTimeout(SETTLE_MS);

    const heartbeats: number[] = await page.evaluate(async () => {
      const gaps: number[] = [];
      let last = performance.now();
      const deadline = last + 2_500;
      let stop = false;
      const tick = (now: number) => {
        gaps.push(now - last);
        last = now;
        if (now < deadline && !stop) requestAnimationFrame(tick);
        else stop = true;
      };
      requestAnimationFrame(tick);
      // Wheel steps are pre-scheduled on timers so the harness adds no
      // awaited work while the heartbeat samples the main thread.
      for (let i = 0; i < 4; i++) {
        setTimeout(
          () => window.dispatchEvent(new WheelEvent("wheel", { deltaY: 240 })),
          i * 150,
        );
      }
      for (let i = 0; i < 4; i++) {
        setTimeout(
          () => window.dispatchEvent(new WheelEvent("wheel", { deltaY: -240 })),
          700 + i * 150,
        );
      }
      await wait(2_300);
      gaps.shift();
      return gaps;
    });
    const maxGap = Math.max(...heartbeats);
    expect(
      maxGap,
      `main thread stalled ${maxGap.toFixed(0)}ms during zoom (budget ${FRAME_GAP_BUDGET_MS}ms)`,
    ).toBeLessThanOrEqual(FRAME_GAP_BUDGET_MS);
  });
});
