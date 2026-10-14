import { expect, test, type Page } from "@playwright/test";
import type { Placement } from "../src/lib/api";
import { WORLD_CENTER } from "../src/lib/world";

function makeEntry(id: string, username: string): Placement {
  return {
    id,
    username,
    x: WORLD_CENTER - 128,
    y: WORLD_CENTER - 128,
    w: 256,
    h: 256,
    rot: 0,
    created_at: "2026-10-13T00:00:00Z",
  };
}

// One placement dead-centre so a centre click selects it and holds the
// anchored panel open.
const TOP = makeEntry("pile-top", "wren");

async function stubPile(page: Page): Promise<void> {
  await page.route("**/api/dig**", (route) => {
    void route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        stack: [{ ...TOP, depth: 1 }],
        total: 1,
        truncated: false,
      }),
    });
  });
  await page.route("**/api/placements/near**", (route) => {
    const url = new URL(route.request().url());
    const x = Number(url.searchParams.get("x") ?? "0");
    const y = Number(url.searchParams.get("y") ?? "0");
    const hits = [TOP].filter(
      (p) => x >= p.x && x <= p.x + p.w && y >= p.y && y <= p.y + p.h,
    );
    void route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(hits),
    });
  });
}

function zoomOf(page: Page): number {
  return Number((new URL(page.url()).hash.split("/")[0] ?? "#0").slice(1));
}

test("scroll zooms while a selection is held", async ({ page }) => {
  await stubPile(page);
  await page.goto("/#16/0/0");
  const vp = page.viewportSize() ?? { width: 1280, height: 720 };
  const cx = vp.width / 2;
  const cy = vp.height / 2;

  await page.mouse.click(cx, cy);
  const panel = page.locator(".place-detail");
  await expect(panel).toBeVisible();
  await expect(panel.getByText("@wren")).toBeVisible();

  // The selection must NOT capture the wheel: scrolling over open map
  // (away from the anchored panel) zooms.
  await page.mouse.move(cx + 300, cy + 200);
  await page.mouse.wheel(0, -240);
  await expect.poll(() => zoomOf(page), { timeout: 5000 }).toBeGreaterThan(16);
  await expect(panel).toBeVisible();

  // And the other direction (wheel down = zoom out) also reaches the map.
  await page.mouse.move(cx + 300, cy + 200);
  await page.mouse.wheel(0, 240);
  await expect.poll(() => zoomOf(page), { timeout: 5000 }).toBeLessThan(17);
  await expect(panel).toBeVisible();
});
