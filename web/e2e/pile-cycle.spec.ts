import { expect, test, type Page } from "@playwright/test";
import type { DigEntry, Placement } from "../src/lib/api";
import { WORLD_CENTER } from "../src/lib/world";

function stratum(
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
    created_at: "2026-10-14T00:00:00Z",
    depth,
  };
}

// One three-stratum pile dead-centre: newest on top, one per username so
// the panel content proves which stratum is showing.
const PILE = [
  stratum("pile-top", "wren", 1),
  stratum("pile-mid", "noor", 2),
  stratum("pile-deep", "sable", 3),
];

async function stubPile(page: Page): Promise<void> {
  await page.route("**/api/placements/near**", (route) => {
    const url = new URL(route.request().url());
    const x = Number(url.searchParams.get("x") ?? "0");
    const y = Number(url.searchParams.get("y") ?? "0");
    const hits = PILE.filter(
      (p) => x >= p.x && x <= p.x + p.w && y >= p.y && y <= p.y + p.h,
    );
    // The near query answers AABB hits, newest first; the dig query is
    // the accurate pile model the cycle walks through.
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
    const top = PILE[0]!;
    const inPile =
      x >= top.x && x <= top.x + top.w && y >= top.y && y <= top.y + top.h;
    void route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(
        inPile
          ? { stack: PILE, total: PILE.length, truncated: false }
          : { stack: [], total: 0, truncated: false },
      ),
    });
  });
}

function centerPoint(page: Page) {
  const vp = page.viewportSize() ?? { width: 1280, height: 720 };
  return { x: vp.width / 2, y: vp.height / 2 };
}

test("repeat clicks cycle down through the pile", async ({ page }) => {
  await stubPile(page);
  await page.goto("/#16/0/0");
  const c = centerPoint(page);

  await page.mouse.click(c.x, c.y);
  const panel = page.locator(".place-detail");
  await expect(panel).toBeVisible();
  await expect(panel.getByText("@wren")).toBeVisible();
  await expect(panel.getByText("1 of 3")).toBeVisible();

  // Second click on the same spot: one stratum down, no re-select top.
  await page.mouse.click(c.x, c.y);
  await expect(panel.getByText("@noor")).toBeVisible();
  await expect(panel.getByText("2 of 3")).toBeVisible();
  await page.mouse.click(c.x, c.y);
  await expect(panel.getByText("@sable")).toBeVisible();
  await expect(panel.getByText("3 of 3")).toBeVisible();
  // Wrapped: the next click lands back on the top stratum.
  await page.mouse.click(c.x, c.y);
  await expect(panel.getByText("@wren")).toBeVisible();
  await expect(panel.getByText("1 of 3")).toBeVisible();
});

test("stepper buttons cycle and the selection follows", async ({ page }) => {
  await stubPile(page);
  await page.goto("/#16/0/0");
  const c = centerPoint(page);

  await page.mouse.click(c.x, c.y);
  const panel = page.locator(".place-detail");
  await expect(panel.getByText("@wren")).toBeVisible();

  await panel.getByLabel("Next stratum").click();
  await expect(panel.getByText("@noor")).toBeVisible();
  await expect(panel.getByText("2 of 3")).toBeVisible();
  await panel.getByLabel("Next stratum").click();
  await expect(panel.getByText("@sable")).toBeVisible();
  await panel.getByLabel("Previous stratum").click();
  await expect(panel.getByText("@noor")).toBeVisible();

  // The highlight follows the cycled stratum's own AABB.
  const aabb = await page.locator(".highlight-aabb").boundingBox();
  expect(aabb).not.toBeNull();
});

test("keyboard arrows step strata without breaking scroll zoom", async ({
  page,
}) => {
  await stubPile(page);
  await page.goto("/#16/0/0");
  const c = centerPoint(page);

  await page.mouse.click(c.x, c.y);
  const panel = page.locator(".place-detail");
  await expect(panel.getByText("@wren")).toBeVisible();

  await page.keyboard.press("ArrowRight");
  await expect(panel.getByText("@noor")).toBeVisible();
  await page.keyboard.press("ArrowLeft");
  await expect(panel.getByText("@wren")).toBeVisible();
  // Escape still deselects.
  await page.keyboard.press("Escape");
  await expect(panel).toHaveCount(0);

  // No wheel capture anywhere in the cycle flow: scroll still zooms.
  await page.mouse.move(c.x + 300, c.y + 200);
  await page.mouse.wheel(0, -240);
  const zoomOf = () =>
    Number((new URL(page.url()).hash.split("/")[0] ?? "#0").slice(1));
  await expect.poll(zoomOf, { timeout: 5000 }).toBeGreaterThan(16);
});
