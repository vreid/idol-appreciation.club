import { expect, test } from "@playwright/test";

test.describe("@smoke static deploy checks", () => {
  test("root page loads", async ({ request }) => {
    const res = await request.get("/");
    expect(res.status()).toBe(200);
  });

  test("hashed assets serve with immutable caching", async ({ request }) => {
    const home = await request.get("/");
    expect(home.status()).toBe(200);
    const html = await home.text();
    const asset = /\/assets\/[^"'\s()]+\.(?:js|css)/u.exec(html);
    if (asset === null) {
      test.info().annotations.push({
        type: "notice",
        description:
          "no /assets/ reference in index.html — frontend not deployed yet",
      });
      return;
    }
    const res = await request.get(asset[0]);
    expect(res.status()).toBe(200);
    const cacheControl = res.headers()["cache-control"] ?? "";
    expect(
      cacheControl,
      "hashed /assets/* must be immutable (Caddy config)",
    ).toContain("immutable");
  });

  test("SPA fallback route returns 200", async ({ request }) => {
    const res = await request.get("/e2e-spa-fallback-probe");
    expect(res.status()).toBe(200);
  });

  test("reticle is centred at min zoom (world fit)", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto("/");
    await page.waitForSelector(".leaflet-container");
    await page.click(".leaflet-container");
    // Keyboard zoom steps must be sequential; parallel presses would race.
    // eslint-disable-next-line unicorn/no-await-in-loop
    for (let i = 0; i < 20; i++) await page.keyboard.press("-");
    await page.waitForTimeout(600);
    const box = await page.evaluate(() => {
      const container = document.querySelector(".leaflet-container");
      const reticle = document.querySelector<HTMLElement>(".reticle");
      if (container === null || reticle === null) return null;
      const c = container.getBoundingClientRect();
      const r = reticle.getBoundingClientRect();
      return {
        x: r.x + r.width / 2 - c.x,
        y: r.y + r.height / 2 - c.y,
        w: c.width,
        h: c.height,
      };
    });
    if (box === null) throw new Error("reticle or map container missing");
    // The reticle must sit within the central quarter of the viewport.
    expect(Math.abs(box.x - box.w / 2)).toBeLessThan(box.w * 0.125);
    expect(Math.abs(box.y - box.h / 2)).toBeLessThan(box.h * 0.125);
  });

  test("tile endpoint is reachable (non-200 tolerated pre-backend)", async ({
    request,
  }) => {
    const res = await request.get("/api/tiles/0/0/0.webp");
    const contentType = res.headers()["content-type"] ?? "";
    // A static-only deploy answers with the SPA fallback (200 text/html);
    // only a real backend serves an image here.
    const servedByBackend =
      res.status() === 200 && contentType.includes("image");
    if (!servedByBackend) {
      test.info().annotations.push({
        type: "notice",
        description: `/api/tiles/0/0/0.webp returned HTTP ${res.status()} (${contentType}) — tolerated until the backend ships`,
      });
    }
  });
});
