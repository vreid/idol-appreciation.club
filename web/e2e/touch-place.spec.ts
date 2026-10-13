import { expect, test } from "@playwright/test";
import { WORLD_CENTER } from "../src/lib/world";

// z16 default view: screen = viewport centre + (world - WORLD_CENTER) * 2^(16-20).
const SCALE = 2 ** (16 - 20);

// A 1×1 PNG (data URL) — the flow only needs a decodable image.
const PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

let placedAt: { x: string; y: string } | null = null;

function stubMap(page: import("@playwright/test").Page): void {
  placedAt = null;
  void page.route(
    "**/api/placements/near**",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "application/json",
        body: "[]",
      }),
  );
  void page.route(
    "**/api/center",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ current: null, all_time: [] }),
      }),
  );
  void page.route(
    "**/api/events",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "",
      }),
  );
  void page.route(
    "**/api/tiles/**",
    (route) =>
      void route.fulfill({
        status: 200,
        contentType: "image/webp",
        body: Buffer.alloc(1),
      }),
  );
  void page.route(
    "**/api/auth/me",
    (route) =>
      void route.fulfill({
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
  void page.route("**/api/place", (route) => {
    const body = route.request().postData() ?? "";
    const x = /name="x"\r\n\r\n(-?\d+)/u.exec(body)?.[1] ?? "?";
    const y = /name="y"\r\n\r\n(-?\d+)/u.exec(body)?.[1] ?? "?";
    placedAt = { x, y };
    void route.fulfill({
      status: 201,
      contentType: "application/json",
      body: JSON.stringify({
        id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a003",
        x: Number(x),
        y: Number(y),
        w: 256,
        h: 256,
        rot: 0,
        seq: 99,
      }),
    });
  });
}

/** Screen point for a world point at the default #16/0/0 view. */
function screenPoint(
  page: import("@playwright/test").Page,
  dx: number,
  dy: number,
) {
  const vp = page.viewportSize() ?? { width: 1280, height: 720 };
  return { x: vp.width / 2 + dx, y: vp.height / 2 + dy };
}

/** Expected world coords for a screen offset from the view centre. */
function worldAt(dx: number, dy: number): { x: number; y: number } {
  return {
    x: WORLD_CENTER + dx * (1 / SCALE),
    y: WORLD_CENTER + dy * (1 / SCALE),
  };
}

async function placeViaChooser(
  page: import("@playwright/test").Page,
  chooserPromise: Promise<import("@playwright/test").FileChooser>,
): Promise<void> {
  const file = await chooserPromise;
  await file.setFiles({
    name: "pin.png",
    mimeType: "image/png",
    buffer: Buffer.from(PNG, "base64"),
  });
  const panel = page.locator(".place-panel");
  await expect(panel).toBeVisible();
  await panel.getByText(/Place at pinned spot/u).click();
  await expect(panel).toHaveCount(0);
}

test("right-click pins the spot and places there", async ({ page }) => {
  stubMap(page);
  await page.goto("/#16/0/0");
  const target = screenPoint(page, 64, -32);
  const expected = worldAt(64, -32);

  const chooserPromise = page.waitForEvent("filechooser");
  await page.mouse.click(target.x, target.y, { button: "right" });
  await placeViaChooser(page, chooserPromise);

  expect(placedAt).not.toBeNull();
  expect(Number(placedAt!.x)).toBeCloseTo(expected.x, -1);
  expect(Number(placedAt!.y)).toBeCloseTo(expected.y, -1);
});

test("touch long-press pins the spot without opening the native menu", async ({
  page,
  context,
}) => {
  expect(context.browser()?.browserType().name()).toBe("chromium");
  stubMap(page);
  await page.goto("/#16/0/0");
  const target = screenPoint(page, -48, 80);
  const expected = worldAt(-48, 80);

  const chooserPromise = page.waitForEvent("filechooser");
  // Synthetic TouchEvents: touchstart → 700ms (> the 500ms timer) → end.
  await page.evaluate(({ x, y }) => {
    const el = document.querySelector(".map-root");
    if (el === null) throw new Error("map root missing");
    const touch = new Touch({
      identifier: 1,
      target: el,
      clientX: x,
      clientY: y,
    });
    el.dispatchEvent(
      new TouchEvent("touchstart", { touches: [touch], bubbles: true }),
    );
    setTimeout(() => {
      el.dispatchEvent(
        new TouchEvent("touchend", {
          touches: [],
          changedTouches: [touch],
          bubbles: true,
        }),
      );
    }, 700);
  }, target);
  await page.waitForTimeout(900);

  await placeViaChooser(page, chooserPromise);

  expect(placedAt).not.toBeNull();
  expect(Number(placedAt!.x)).toBeCloseTo(expected.x, -1);
  expect(Number(placedAt!.y)).toBeCloseTo(expected.y, -1);
});

test("a drag cancels the pending long-press", async ({ page }) => {
  stubMap(page);
  await page.goto("/#16/0/0");
  const start = screenPoint(page, 0, 0);

  await page.evaluate(({ x, y }) => {
    const el = document.querySelector(".map-root");
    if (el === null) throw new Error("map root missing");
    const t1 = new Touch({ identifier: 1, target: el, clientX: x, clientY: y });
    const t2 = new Touch({
      identifier: 1,
      target: el,
      clientX: x + 60,
      clientY: y + 60,
    });
    el.dispatchEvent(
      new TouchEvent("touchstart", { touches: [t1], bubbles: true }),
    );
    el.dispatchEvent(
      new TouchEvent("touchmove", { touches: [t2], bubbles: true }),
    );
    el.dispatchEvent(
      new TouchEvent("touchend", {
        touches: [],
        changedTouches: [t2],
        bubbles: true,
      }),
    );
  }, start);
  await page.waitForTimeout(700);

  // No pin → no file chooser: prove it by absence of the panel for a bit.
  const chooserOpened = await Promise.race([
    page
      .waitForEvent("filechooser", { timeout: 1200 })
      .then(() => true)
      .catch(() => false),
    page.waitForTimeout(1300).then(() => false),
  ]);
  expect(chooserOpened).toBe(false);
});
