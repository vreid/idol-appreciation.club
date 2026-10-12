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
  if (typeof placedId !== "number") {
    throw new TypeError("place response missing numeric id");
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
  expect(centerBody).toEqual(
    expect.objectContaining({
      current: expect.anything(),
      all_time: expect.any(Array),
    }),
  );
});
