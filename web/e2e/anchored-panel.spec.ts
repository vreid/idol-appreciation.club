import { expect, test, type Page } from "@playwright/test";
import type { Placement } from "../src/lib/api";
import { WORLD_CENTER } from "../src/lib/world";

// z16 default view; worldScale(zoom) = 2^(zoom - 20) → 1/16 px per world px.
const SCALE = 2 ** (16 - 20);

function makePlacement(
  id: string,
  username: string,
  centerX: number,
  centerY: number,
): Placement {
  return {
    id,
    username,
    x: centerX - 128,
    y: centerY - 128,
    w: 256,
    h: 256,
    rot: 0,
    created_at: "2026-10-13T00:00:00Z",
  };
}

// Two piles around the world centre (the default view's centre point):
// one dead-centre, one far east so its screen position sits near the
// right viewport edge.
const CENTER_PILE = makePlacement(
  "pile-center",
  "wren",
  WORLD_CENTER,
  WORLD_CENTER,
);
const EAST_PILE = makePlacement(
  "pile-east",
  "noor",
  WORLD_CENTER + 9600,
  WORLD_CENTER,
);

/** Answer near() with a real point-in-AABB hit test over the fixtures. */
async function stubPiles(page: Page): Promise<void> {
  await page.route("**/api/placements/near**", (route) => {
    const url = new URL(route.request().url());
    const x = Number(url.searchParams.get("x") ?? "0");
    const y = Number(url.searchParams.get("y") ?? "0");
    const hits = [CENTER_PILE, EAST_PILE].filter(
      (p) => x >= p.x && x <= p.x + p.w && y >= p.y && y <= p.y + p.h,
    );
    void route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(hits),
    });
  });
}

/** Screen point of a world point at the default view (#16/0/0 centred). */
function screenPoint(page: Page, worldX: number, worldY: number) {
  const vp = page.viewportSize() ?? { width: 1280, height: 720 };
  return {
    x: vp.width / 2 + worldX * SCALE,
    y: vp.height / 2 + worldY * SCALE,
  };
}

type Box = { x: number; y: number; width: number; height: number };

/** The panel must stay at least PANEL_GAP (8px CSS) clear of the image. */
async function expectSelectionUncovered(page: Page): Promise<void> {
  const boxes: {
    image: Box | null;
    panel: Box | null;
  } = await page.evaluate(() => {
    // Runs in the browser via page.evaluate — cannot be hoisted.
    // eslint-disable-next-line unicorn/consistent-function-scoping
    const pick = (selector: string): Box | null => {
      const el = document.querySelector(selector);
      if (el === null) return null;
      const r = el.getBoundingClientRect();
      return { x: r.left, y: r.top, width: r.width, height: r.height };
    };
    return {
      image: pick(".highlight-aabb"),
      panel: pick(".place-detail"),
    };
  });
  expect(boxes.image).not.toBeNull();
  expect(boxes.panel).not.toBeNull();
  const gap = 8;
  const clearH =
    boxes.panel!.x + boxes.panel!.width <= boxes.image!.x - gap ||
    boxes.panel!.x >= boxes.image!.x + boxes.image!.width + gap;
  const clearV =
    boxes.panel!.y + boxes.panel!.height <= boxes.image!.y - gap ||
    boxes.panel!.y >= boxes.image!.y + boxes.image!.height + gap;
  expect(clearH || clearV, "panel overlaps the selected image").toBe(true);
}

test("selecting a pile opens an anchored panel that never covers it", async ({
  page,
}) => {
  await stubPiles(page);
  await page.goto("/#16/0/0");
  await expect(page.locator(".leaflet-container")).toBeVisible();

  const at = screenPoint(page, 0, 0);
  await page.mouse.click(at.x, at.y);
  await expect(page.locator(".place-detail")).toBeVisible();
  expect(await page.locator(".place-detail").getAttribute("class")).toContain(
    "side-right",
  );
  await expectSelectionUncovered(page);
  await expect(page.locator(".place-detail")).toContainText("@wren");
});

test("a pile near the right edge flips the panel to the left", async ({
  page,
}) => {
  await stubPiles(page);
  await page.goto("/#16/0/0");
  await expect(page.locator(".leaflet-container")).toBeVisible();

  const at = screenPoint(page, 9600, 0);
  await page.mouse.click(at.x, at.y);
  await expect(page.locator(".place-detail")).toBeVisible();
  expect(await page.locator(".place-detail").getAttribute("class")).toContain(
    "side-left",
  );
  await expectSelectionUncovered(page);
});

test("the panel follows a drag and stays clear; ESC deselects", async ({
  page,
}) => {
  await stubPiles(page);
  await page.goto("/#16/0/0");
  await expect(page.locator(".leaflet-container")).toBeVisible();

  const at = screenPoint(page, 0, 0);
  await page.mouse.click(at.x, at.y);
  await expect(page.locator(".place-detail")).toBeVisible();

  // Drag from clear canvas so the pointer never touches panel or image.
  await page.mouse.move(at.x - 300, at.y + 200);
  await page.mouse.down();
  await page.mouse.move(at.x - 420, at.y + 120, { steps: 5 });
  await page.mouse.up();
  await expect(page.locator(".place-detail")).toBeVisible();
  await expectSelectionUncovered(page);

  await page.keyboard.press("Escape");
  await expect(page.locator(".place-detail")).toHaveCount(0);
});

test("clicking empty canvas deselects silently", async ({ page }) => {
  await stubPiles(page);
  await page.goto("/#16/0/0");
  await expect(page.locator(".leaflet-container")).toBeVisible();

  const at = screenPoint(page, 0, 0);
  await page.mouse.click(at.x, at.y);
  await expect(page.locator(".place-detail")).toBeVisible();

  const empty = screenPoint(page, -8000, 4000);
  await page.mouse.click(empty.x, empty.y);
  await expect(page.locator(".place-detail")).toHaveCount(0);
});
