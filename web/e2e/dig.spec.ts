import { expect, test, type Page } from "@playwright/test";
import type { DigEntry, Placement } from "../src/lib/api";
import { WORLD_CENTER } from "../src/lib/world";

function makeEntry(
  id: string,
  username: string,
  depth: number,
): Placement & DigEntry {
  return {
    id,
    username,
    x: WORLD_CENTER - 128,
    y: WORLD_CENTER - 128,
    w: 256,
    h: 256,
    rot: 0,
    created_at: "2026-10-13T00:00:00Z",
    depth,
  };
}

// One three-stratum pile dead-centre: newest on top, one per username so
// the panel content proves which stratum is showing.
const STACK = [
  makeEntry("pile-top", "wren", 1),
  makeEntry("pile-mid", "noor", 2),
  makeEntry("pile-deep", "sable", 3),
];

async function stubPile(page: Page): Promise<void> {
  await page.route("**/api/placements/near**", (route) => {
    const url = new URL(route.request().url());
    const x = Number(url.searchParams.get("x") ?? "0");
    const y = Number(url.searchParams.get("y") ?? "0");
    const hits = STACK.filter(
      (p) => x >= p.x && x <= p.x + p.w && y >= p.y && y <= p.y + p.h,
    );
    // The near query answers AABB hits, newest first — the click selects
    // hits[0]; the dig query then reveals the strata under the point.
    void route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(hits),
    });
  });
  await page.route("**/api/dig**", (route) => {
    const url = new URL(route.request().url());
    const x = Number(url.searchParams.get("x") ?? "0");
    const y = Number(url.searchParams.get("y") ?? "0");
    const top = STACK[0]!;
    const inPile =
      x >= top.x && x <= top.x + top.w && y >= top.y && y <= top.y + top.h;
    void route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(
        inPile
          ? { stack: STACK, total: 3, truncated: false }
          : { stack: [], total: 0, truncated: false },
      ),
    });
  });
}

function centerPoint(page: Page) {
  const vp = page.viewportSize() ?? { width: 1280, height: 720 };
  return { x: vp.width / 2, y: vp.height / 2 };
}

test("wheel flips the pile under a selection", async ({ page }) => {
  await stubPile(page);
  await page.goto("/#16/0/0");
  const c = centerPoint(page);

  await page.mouse.click(c.x, c.y);
  const panel = page.locator(".place-detail");
  await expect(panel).toBeVisible();
  await expect(panel.getByText("@wren")).toBeVisible();
  await expect(panel.getByText("1 of 3")).toBeVisible();

  // Wheel zoom must NOT move the map while the selection holds the wheel.
  const zoomAtSelect = new URL(page.url()).hash.split("/")[0];
  await page.mouse.move(c.x, c.y);
  await page.mouse.wheel(0, 120);
  await expect(panel.getByText("@noor")).toBeVisible();
  await expect(panel.getByText("2 of 3")).toBeVisible();
  await page.mouse.wheel(0, 120);
  await expect(panel.getByText("@sable")).toBeVisible();
  await expect(panel.getByText("3 of 3")).toBeVisible();
  // Clamped: deeper than the deepest stratum changes nothing.
  await page.mouse.wheel(0, 120);
  await expect(panel.getByText("3 of 3")).toBeVisible();
  expect(new URL(page.url()).hash.split("/")[0]).toBe(zoomAtSelect);

  // Wheel back up toward the newest stratum.
  await page.mouse.wheel(0, -120);
  await expect(panel.getByText("@noor")).toBeVisible();

  // The highlight follows the visible stratum's AABB.
  const aabb = await page.locator(".highlight-aabb").boundingBox();
  expect(aabb).not.toBeNull();
});

test("deselecting restores wheel zoom", async ({ page }) => {
  await stubPile(page);
  await page.goto("/#16/0/0");
  const c = centerPoint(page);

  await page.mouse.click(c.x, c.y);
  await expect(page.locator(".place-detail")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator(".place-detail")).toHaveCount(0);

  // With no selection the wheel belongs to the map again: scroll zooms.
  await page.mouse.move(c.x, c.y);
  await page.mouse.wheel(0, 240);
  await expect
    .poll(() => new URL(page.url()).hash.split("/")[0], { timeout: 5000 })
    .not.toBe("#16");
});
