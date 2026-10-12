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
