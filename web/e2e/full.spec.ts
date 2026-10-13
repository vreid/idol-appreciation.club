import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { isRecord } from "../src/lib/guards";
import { SKIP_UNHEALTHY, backendHealthy, registerAndLogin } from "./helpers";

// Deterministic far-from-center spot so runs only ever collide with
// themselves; size 64 keeps the footprint tiny.
const FAR_X = 4096;
const FAR_Y = 4096;
const SIZE = 64;

const FIXTURE = fileURLToPath(
  new URL("../test-assets/e2e-tile.png", import.meta.url),
);

let healthy = false;

test.beforeAll(async ({ request }) => {
  healthy = await backendHealthy(request);
});

test("@full auth: registers and logs in a unique bot account", async ({
  request,
}) => {
  test.skip(!healthy, SKIP_UNHEALTHY);
  const username = await registerAndLogin(request);
  const me = await request.get("/api/auth/me");
  expect(me.ok()).toBeTruthy();
  const body: unknown = await me.json();
  expect(body).toMatchObject({ username });
});

test("@full place flow: place, near, cooldown 429, center shape", async ({
  request,
}) => {
  test.skip(!healthy, SKIP_UNHEALTHY);
  const username = await registerAndLogin(request);

  const place = await request.post("/api/place", {
    multipart: {
      image: {
        name: "e2e-tile.png",
        mimeType: "image/png",
        buffer: readFileSync(FIXTURE),
      },
      x: String(FAR_X),
      y: String(FAR_Y),
      size: String(SIZE),
    },
  });
  expect(place.status()).toBe(201);
  const placed: unknown = await place.json();
  expect(placed).toMatchObject({ x: FAR_X, y: FAR_Y, seq: expect.any(Number) });
  if (!isRecord(placed)) throw new Error("place response was not an object");
  const placedId = placed["id"];
  // Placement ids are UUIDv7 strings per the API contract.
  if (typeof placedId !== "string" || !/^[0-9a-f-]{10,}$/iu.test(placedId)) {
    throw new TypeError(
      `place response returned a non-uuid id: ${String(placedId)}`,
    );
  }

  const near = await request.get(
    `/api/placements/near?x=${FAR_X}&y=${FAR_Y}&r=256`,
  );
  expect(near.ok()).toBeTruthy();
  const nearby: unknown = await near.json();
  expect(Array.isArray(nearby)).toBe(true);
  const list: unknown[] = Array.isArray(nearby) ? nearby : [];
  const visible = list.some(
    (p: unknown) =>
      isRecord(p) && p["id"] === placedId && p["username"] === username,
  );
  expect(visible, "placement must be visible via /api/placements/near").toBe(
    true,
  );

  // Immediate overlapping placement must hit the cooldown.
  const again = await request.post("/api/place", {
    multipart: {
      image: {
        name: "e2e-tile.png",
        mimeType: "image/png",
        buffer: readFileSync(FIXTURE),
      },
      x: String(FAR_X),
      y: String(FAR_Y),
      size: String(SIZE),
    },
  });
  expect(again.status(), "second immediate placement must be cooled down").toBe(
    429,
  );
  expect(
    again.headers()["retry-after"],
    "429 must carry Retry-After",
  ).toBeDefined();

  const center = await request.get("/api/center");
  expect(center.ok()).toBeTruthy();
  const centerBody: unknown = await center.json();
  if (!isRecord(centerBody))
    throw new Error("center response was not an object");
  // `current` is null until someone holds 0/0 — valid on a fresh world.
  const current = centerBody["current"];
  expect(
    current === null || isRecord(current),
    "current must be a holder object or null",
  ).toBe(true);
  expect(
    Array.isArray(centerBody["all_time"]),
    "all_time must be an array",
  ).toBe(true);
});

test("@full center tile serves real content, not the empty placeholder", async ({
  page,
}) => {
  test.skip(!healthy, SKIP_UNHEALTHY);
  // Collect every tile response the map makes while booting (the initial
  // view is z16 centered on 0/0) and after the explicit jump.
  const tiles: { url: string; etag: string; length: number }[] = [];
  page.on("response", (res) => {
    const url = res.url();
    if (!url.includes("/api/tiles/16/")) return;
    tiles.push({
      url,
      etag: res.headers()["etag"] ?? "",
      length: Number(res.headers()["content-length"] ?? "-1"),
    });
  });
  await page.goto("/");
  await page.waitForSelector(".leaflet-container");
  // The map boots centred on the world centre at z16 by default.
  await page.waitForTimeout(2000);

  // Server formula: the z16 centre tile index for worldPx 134217728 is
  // floor(134217728 / (256 * 2^4)) = 32768 — the 2x2 around it covers
  // 32767..32768 per axis.
  const requested = tiles.filter((t) =>
    /\/api\/tiles\/16\/3276[78]\/3276[78]\.webp$/u.test(t.url),
  );
  expect(
    requested.length,
    "the map must request the z16 centre tiles per the server formula",
  ).toBeGreaterThan(0);
  const placeholder = requested.find(
    (t) => t.etag.replaceAll('"', "") === "0" || t.length === 394,
  );
  expect(
    placeholder,
    "centre tiles must carry real image content, not the 394B etag-0 placeholder",
  ).toBeUndefined();
});
