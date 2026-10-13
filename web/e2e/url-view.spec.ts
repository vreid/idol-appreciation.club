import { expect, test } from "@playwright/test";

// Initial moveend plus the 250 ms hash-write debounce.
const SETTLE_MS = 600;

test("valid hash positions the map and survives the debounced write", async ({
  page,
}) => {
  await page.goto("/#16/0/0");
  await expect(page.locator(".leaflet-container")).toBeVisible();
  await page.waitForTimeout(SETTLE_MS);
  // The writer recomputes the hash from the live view; stability proves the
  // map centred exactly on the requested world-px offsets.
  expect(new URL(page.url()).hash).toBe("#16/0/0");
});

test("invalid hash falls back to the default view", async ({ page }) => {
  await page.goto("/#banana");
  await expect(page.locator(".leaflet-container")).toBeVisible();
  await page.waitForTimeout(SETTLE_MS);
  // Default view is z16 centred on 0/0 → the writer encodes exactly this.
  expect(new URL(page.url()).hash).toBe("#16/0/0");
});

test("panning updates the URL (debounced) and back re-applies history", async ({
  page,
}) => {
  await page.goto("/#16/0/0");
  await expect(page.locator(".leaflet-container")).toBeVisible();
  await page.waitForTimeout(SETTLE_MS);

  await page.mouse.move(640, 400);
  await page.mouse.down();
  await page.mouse.move(520, 300, { steps: 5 });
  await page.mouse.up();
  // URL must not update before the debounce window elapses.
  await page.waitForTimeout(120);
  expect(new URL(page.url()).hash).toBe("#16/0/0");
  // Leaflet's drag inertia ends after the mouseup, then the 250 ms debounce
  // runs — allow both to settle before reading the written hash.
  await page.waitForTimeout(1000);
  const afterDrag = new URL(page.url()).hash;
  expect(afterDrag).toMatch(/^#16\/-?\d+\/-?\d+$/u);
  expect(afterDrag).not.toBe("#16/0/0");

  // Editing the hash pushes a history entry; the map re-applies it…
  await page.evaluate(() => {
    window.location.hash = "#14/-4096/2048";
  });
  await page.waitForTimeout(SETTLE_MS);
  expect(new URL(page.url()).hash).toBe("#14/-4096/2048");

  // …and Back navigates the view to the previous entry.
  await page.goBack();
  await page.waitForTimeout(SETTLE_MS);
  expect(new URL(page.url()).hash).toBe(afterDrag);
  // The re-applied view must not clobber the entry it came from.
  await page.waitForTimeout(400);
  expect(new URL(page.url()).hash).toBe(afterDrag);
});
