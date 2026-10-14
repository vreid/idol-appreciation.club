import { expect, test, type Page } from "@playwright/test";
import { MIN_ZOOM, WORLD_SIZE } from "../src/lib/world";

const LIMIT = WORLD_SIZE / 2;
// A 1×1 PNG (data URL) — the flow only needs a decodable image.
const PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

function stubMap(page: Page): void {
  void page.route("**/api/placements/near**", (route) =>
    route.fulfill({ status: 200, contentType: "application/json", body: "[]" }),
  );
  void page.route("**/api/center", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ current: null, all_time: [] }),
    }),
  );
  void page.route("**/api/events", (route) =>
    route.fulfill({ status: 200, contentType: "text/event-stream", body: "" }),
  );
  void page.route("**/api/tiles/**", (route) =>
    route.fulfill({
      status: 200,
      contentType: "image/webp",
      body: Buffer.alloc(1),
    }),
  );
  void page.route("**/api/auth/me", (route) =>
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({
        id: "u1",
        username: "bot",
        is_admin: false,
        cooldown_until: null,
      }),
    }),
  );
}

/** Leave the world: at the far-out zoom the world square covers only part
 * of the viewport; click the empty void beyond its right edge. */
async function goOffWorld(page: Page): Promise<void> {
  // z2: half-world ≈ 512 CSS px on screen.
  const zoom = MIN_ZOOM + 2;
  await page.goto(`/#${zoom}/0/0`);
  await page.waitForSelector(".map-root");
  await page.waitForLoadState("networkidle");
  const viewport = page.viewportSize() ?? { width: 1280, height: 720 };
  const halfWorldPx = LIMIT * ((256 * 2 ** zoom) / WORLD_SIZE);
  await page.mouse.click(
    viewport.width / 2 + halfWorldPx + 80,
    viewport.height / 2,
  );
}

test.describe("off-world guard", () => {
  test("clicking the void is silent: no toast, no panel, idle app", async ({
    page,
  }) => {
    stubMap(page);
    await goOffWorld(page);
    await page.waitForTimeout(600);
    await expect(page.locator(".toast")).toHaveCount(0);
    await expect(page.locator(".place-detail")).toHaveCount(0);
    await expect(page.locator(".map-root.placing")).toHaveCount(0);
    // The app is alive and navigable afterwards.
    await page.mouse.click(400, 400);
    await page.waitForTimeout(300);
    await expect(page.locator(".map-root")).toHaveCount(1);
  });

  test("an armed placing flow stands down on an off-world click", async ({
    page,
  }) => {
    stubMap(page);
    await page.goto("/#16/0/0");
    await page.waitForSelector(".map-root");

    // Arm the flow WITHOUT pinning: top bar button, pick a file, then
    // choose the spot (the pinned primary would skip choose mode).
    const chooserPromise = page.waitForEvent("filechooser");
    await page.getByRole("button", { name: /Place image/u }).click();
    const chooser = await chooserPromise;
    await chooser.setFiles({
      name: "pin.png",
      mimeType: "image/png",
      buffer: Buffer.from(PNG, "base64"),
    });
    const panel = page.locator(".place-panel");
    await expect(panel).toBeVisible();
    await panel.getByText(/Choose spot/u).click();
    await expect(page.locator(".place-hint")).toBeVisible();
    await expect(page.locator(".map-root.placing")).toHaveCount(1);

    // Off-world click stands the flow down instead of confirming a spot.
    await goOffWorld(page);
    await page.waitForTimeout(400);
    await expect(page.locator(".map-root.placing")).toHaveCount(0);
    await expect(page.locator(".place-panel")).toHaveCount(0);
    await expect(page.locator(".place-hint")).toHaveCount(0);
    await expect(page.locator(".place-detail")).toHaveCount(0);
  });

  test("an off-world hash restores a clamped in-world view", async ({
    page,
  }) => {
    stubMap(page);
    // Way outside the world: 2× the world size as a centre offset.
    await page.goto(`/#20/${WORLD_SIZE * 2}/${WORLD_SIZE * 2}`);
    await page.waitForSelector(".map-root");
    await page.waitForLoadState("networkidle");
    // Let the app write back what it actually shows.
    await page.mouse.move(400, 400);
    await page.mouse.wheel(0, 10);
    await page.waitForTimeout(800);
    const hash = await page.evaluate(() => window.location.hash);
    const match = /^#(\d+)\/(-?\d+)\/(-?\d+)$/u.exec(hash);
    expect(match).not.toBeNull();
    const x = Number(match?.[2]);
    const y = Number(match?.[3]);
    expect(Math.abs(x)).toBeLessThanOrEqual(LIMIT);
    expect(Math.abs(y)).toBeLessThanOrEqual(LIMIT);
  });
});
