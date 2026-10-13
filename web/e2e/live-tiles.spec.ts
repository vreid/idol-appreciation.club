import {
  expect,
  test,
  type APIRequestContext,
  type Page,
  type TestInfo,
} from "@playwright/test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { isRecord } from "../src/lib/guards";
import { SKIP_UNHEALTHY, backendHealthy, registerAndLogin } from "./helpers";

/**
 * Live tile refresh: a placement landing via SSE must refresh the visible
 * tile pixels WITHOUT panning/zooming, without ever blanking the tile. The
 * client re-assigns the tile's src with a cache-busting query (same-URL
 * reloads would be answered from the stale-while-revalidate grace and lag a
 * refresh behind), so the observable refresh is the img src change plus an
 * on-screen pixel change.
 */

const FIXTURE = fileURLToPath(
  new URL("../test-assets/e2e-tile.png", import.meta.url),
);

// One bot account and one storm region per run — keep the tests sequential.
test.describe.configure({ mode: "serial" });

/** Stable per-run identity so the removal test reuses first-account admin. */
const BOT = `e2e_bot_${Date.now().toString(36)}`;

/** Signed world-px view offsets are relative to the world centre. */
const WORLD_CENTER = 2 ** 27;

/** Storm layout: disjoint spots around the view centre (1s cooldown safe). */
const OFFSETS: Array<[number, number]> = [
  [4096, 0],
  [-4096, 0],
  [0, 4096],
  [0, -4096],
];

/** Per-run far-from-centre region so runs never collide with other art. */
function stormOrigin(): { x: number; y: number } {
  const slot = Date.now() % 4096;
  return { x: WORLD_CENTER + slot * 16384, y: WORLD_CENTER + slot * 16384 };
}

let healthy = false;

test.beforeAll(async ({ request }) => {
  healthy = await backendHealthy(request);
});

async function place(
  request: APIRequestContext,
  x: number,
  y: number,
  size: number,
): Promise<string> {
  const res = await request.post("/api/place", {
    multipart: {
      image: {
        name: "e2e-tile.png",
        mimeType: "image/png",
        buffer: readFileSync(FIXTURE),
      },
      x: String(x),
      y: String(y),
      size: String(size),
    },
  });
  expect(res.status(), `place at ${x},${y}`).toBe(201);
  const body: unknown = await res.json();
  if (!isRecord(body) || typeof body["id"] !== "string") {
    throw new TypeError("place response missing id");
  }
  return body["id"];
}

/** Currently rendered (loaded) tile URLs, in DOM order. */
function loadedTileSrcs(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    [
      ...document.querySelectorAll<HTMLImageElement>(".leaflet-tile-loaded"),
    ].map((img) => img.src),
  );
}

/** Count of previously-loaded tiles that have blanked (never acceptable). */
function blankedTiles(page: Page): Promise<number> {
  return page.evaluate(
    () =>
      [
        ...document.querySelectorAll<HTMLImageElement>(".leaflet-tile-loaded"),
      ].filter((img) => !img.complete || img.naturalWidth === 0).length,
  );
}

/** True once any loaded tile's src carries the cache-busting query. */
function anyTileRefreshed(page: Page): Promise<boolean> {
  return page.evaluate(() =>
    [
      ...document.querySelectorAll<HTMLImageElement>(".leaflet-tile-loaded"),
    ].some((img) => img.src.includes("?r=")),
  );
}

/** Screenshot diff inside the page: count meaningfully-changed pixels. */
function pixelDiff(page: Page, beforeB64: string, afterB64: string) {
  return page.evaluate(
    ({ a, b }) => {
      // eslint-disable-next-line unicorn/consistent-function-scoping
      const load = (src: string): Promise<ImageData> => {
        const { promise, resolve, reject } = Promise.withResolvers<ImageData>();
        const img = new Image();
        img.addEventListener(
          "load",
          () => {
            const canvas = document.createElement("canvas");
            canvas.width = img.naturalWidth;
            canvas.height = img.naturalHeight;
            const ctx = canvas.getContext("2d");
            if (ctx === null) throw new Error("no 2d context");
            ctx.drawImage(img, 0, 0);
            resolve(ctx.getImageData(0, 0, canvas.width, canvas.height));
          },
          { once: true },
        );
        img.addEventListener(
          "error",
          () => {
            reject(new Error("screenshot decode failed"));
          },
          { once: true },
        );
        img.src = src;
        return promise;
      };
      return Promise.all([load(a), load(b)]).then(([pa, pb]) => {
        const da = pa.data;
        const db = pb.data;
        let diff = 0;
        for (let i = 0; i < da.length; i += 4) {
          if (
            Math.abs((da[i] ?? 0) - (db[i] ?? 0)) > 8 ||
            Math.abs((da[i + 1] ?? 0) - (db[i + 1] ?? 0)) > 8 ||
            Math.abs((da[i + 2] ?? 0) - (db[i + 2] ?? 0)) > 8
          ) {
            diff++;
          }
        }
        return diff;
      });
    },
    { a: beforeB64, b: afterB64 },
  );
}

async function saveArtifact(
  testInfo: TestInfo,
  name: string,
  png: Buffer,
): Promise<void> {
  await testInfo.attach(name, { body: png, contentType: "image/png" });
  const dir = process.env.LIVE_TILES_ARTIFACT_DIR;
  if (dir !== undefined && dir !== "") {
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, `${name}.png`), png);
  }
}

function asB64(png: Buffer): string {
  return `data:image/png;base64,${png.toString("base64")}`;
}

test("SSE placement refreshes visible tiles without blanking", async ({
  page,
  request,
}, testInfo) => {
  test.skip(!healthy, SKIP_UNHEALTHY);
  await registerAndLogin(request, BOT);

  const origin = stormOrigin();
  // z19: 512 native px tiles → 256 CSS px each → a multi-tile viewport.
  const view = { x: origin.x + 32768, y: origin.y + 32768 };
  await page.goto(`/#19/${view.x - WORLD_CENTER}/${view.y - WORLD_CENTER}`);
  await expect(page.locator(".leaflet-tile-loaded").first()).toBeVisible({
    timeout: 15_000,
  });
  await page.waitForFunction(() => {
    const imgs = [
      ...document.querySelectorAll<HTMLImageElement>(".leaflet-tile-loaded"),
    ];
    return (
      imgs.length > 0 && imgs.every((i) => i.complete && i.naturalWidth > 0)
    );
  });

  const tileSrcs = await loadedTileSrcs(page);
  expect(tileSrcs.length).toBeGreaterThan(1);
  const tilePaths = tileSrcs.map((src) => new URL(src).pathname);

  const before = await page.screenshot({ type: "png" });
  await saveArtifact(testInfo, "live-tiles-before", before);

  // One placement lands near the view centre; its tile must re-request
  // within 2s of the SSE event and take a new (cache-busted) src.
  const armTileRequest = page.waitForRequest(
    (req) => tilePaths.includes(new URL(req.url()).pathname),
    { timeout: 2000 },
  );
  await place(request, view.x, view.y, 1024);
  const refreshed = await armTileRequest;
  expect(tilePaths, "refreshed a currently visible tile").toContain(
    new URL(refreshed.url()).pathname,
  );
  await expect.poll(() => anyTileRefreshed(page), { timeout: 2000 }).toBe(true);

  // Storm: four more placements (disjoint, 1s empty-area cooldown pacing).
  // Throughout, no loaded tile may ever blank — old pixels stay until the
  // new bytes decode.
  const blankSamples: number[] = [];
  const watchBlank = setInterval(() => {
    void blankedTiles(page)
      .then((n) => blankSamples.push(n))
      .catch(() => {});
  }, 100);
  const burst: Array<Promise<Buffer>> = [];
  try {
    // Clear the account's empty-area cooldown from the placement above.
    await page.waitForTimeout(1100);
    for (const [i, [dx, dy]] of OFFSETS.entries()) {
      // Sequential by design: the 1s empty-area cooldown makes parallel
      // placements 429, and the pacing itself is part of the storm.
      // eslint-disable-next-line no-await-in-loop
      await place(request, view.x + dx, view.y + dy, 1024);
      // eslint-disable-next-line no-await-in-loop
      await page.waitForTimeout(1100);
      if (i % 2 === 1) burst.push(page.screenshot({ type: "png" }));
    }
  } finally {
    clearInterval(watchBlank);
  }
  expect(
    blankSamples.every((n) => n === 0),
    `loaded tiles must never blank during the storm: ${blankSamples.join(",")}`,
  ).toBe(true);

  // Let the pulse overlay (1400ms) clear, then diff — the placed art must
  // be on screen without any further interaction.
  await page.waitForTimeout(1600);
  const after = await page.screenshot({ type: "png" });
  await saveArtifact(testInfo, "live-tiles-after", after);
  await Promise.all(
    (await Promise.all(burst)).map((shot, i) =>
      saveArtifact(testInfo, `live-tiles-storm-${i}`, shot),
    ),
  );
  const diff = await pixelDiff(page, asB64(before), asB64(after));
  expect(diff, "tile pixels visibly updated after the storm").toBeGreaterThan(
    2000,
  );
});

test("SSE removal refreshes visible tiles too (admin)", async ({
  page,
  request,
}, testInfo) => {
  test.skip(!healthy, SKIP_UNHEALTHY);
  await registerAndLogin(request, BOT);
  const me: unknown = await (await request.get("/api/auth/me")).json();
  if (!isRecord(me) || me["is_admin"] !== true) {
    test.info().annotations.push({
      type: "notice",
      description: "bot is not admin on this backend — removal check skipped",
    });
    return;
  }

  const origin = stormOrigin();
  const view = { x: origin.x + 32768, y: origin.y + 32768 };
  await page.goto(`/#19/${view.x - WORLD_CENTER}/${view.y - WORLD_CENTER}`);
  await expect(page.locator(".leaflet-tile-loaded").first()).toBeVisible({
    timeout: 15_000,
  });

  const tileSrcs = await loadedTileSrcs(page);
  const tilePaths = new Set(tileSrcs.map((src) => new URL(src).pathname));

  // Baseline = art visible. Place, wait for the SSE refresh to land, then
  // shoot; the later diff proves the removal cleared it again.
  const placedId = await place(request, view.x, view.y, 1024);
  await page.waitForRequest(
    (req) => tilePaths.has(new URL(req.url()).pathname),
    { timeout: 2000 },
  );
  await page.waitForTimeout(1600);
  const before = await page.screenshot({ type: "png" });
  await saveArtifact(testInfo, "live-tiles-remove-before", before);

  const armTileRequest = page.waitForRequest(
    (req) => tilePaths.has(new URL(req.url()).pathname),
    { timeout: 2000 },
  );
  const del = await request.delete(`/api/admin/placements/${placedId}`);
  expect(del.status()).toBe(204);
  await armTileRequest;

  await page.waitForTimeout(1600);
  const after = await page.screenshot({ type: "png" });
  await saveArtifact(testInfo, "live-tiles-remove-after", after);
  const diff = await pixelDiff(page, asB64(before), asB64(after));
  expect(diff, "removed art left the visible tiles").toBeGreaterThan(500);
});
