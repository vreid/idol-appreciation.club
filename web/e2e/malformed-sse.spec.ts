import { expect, test } from "@playwright/test";

/**
 * Contract resilience: a malformed SSE event must not take the app down.
 * /api/events is intercepted before the app connects, so the injected
 * payload arrives through the real SseClient path. The suite needs no live
 * backend beyond whatever BASE_URL already serves.
 */

const BAD_EVENT =
  'data: {"type":"place","seq":"not-a-number","bbox":[1,2,3,4]}\n\n';

test("malformed SSE event: app stays alive, toasts, map still navigable", async ({
  page,
}) => {
  const warnings: string[] = [];
  page.on("console", (msg) => {
    if (msg.type() === "warning") warnings.push(msg.text());
  });

  // Fulfill the stream with one malformed event; every EventSource
  // reconnect gets it again, and the app's toast dedupe keeps one pill.
  await page.route("**/api/events", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      body: BAD_EVENT,
    }),
  );

  await page.goto("/#16/0/0");
  await expect(page.locator(".leaflet-container")).toBeVisible();

  // The rejected payload surfaces as a toast (and a console warning).
  await expect(
    page.locator(".toast.error").filter({ hasText: /bad payload/iu }),
  ).toBeVisible();
  expect(warnings.some((w) => /malformed/iu.test(w))).toBe(true);

  // No crash: the map is still live — the hash view survives the debounced
  // write-back and a drag produces a new hash, like url-view.spec.ts.
  await page.waitForTimeout(600);
  expect(new URL(page.url()).hash).toBe("#16/0/0");

  await page.mouse.move(640, 400);
  await page.mouse.down();
  await page.mouse.move(520, 300, { steps: 5 });
  await page.mouse.up();
  await page.waitForTimeout(1000);
  const afterDrag = new URL(page.url()).hash;
  expect(afterDrag).toMatch(/^#16\/-?\d+\/-?\d+$/u);
  expect(afterDrag).not.toBe("#16/0/0");
});
