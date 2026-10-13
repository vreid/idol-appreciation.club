import { describe, expect, it } from "vite-plus/test";
import {
  bboxesIntersect,
  bboxContainsPoint,
  clampToWorldPoint,
  clampToWorld,
  latLngToWorld,
  nativeTileBBox,
  parseTileKey,
  queryRadiusForZoom,
  tileUrl,
  tilesIntersecting,
  WORLD_CENTER,
  WORLD_LAT_LNG_BOUNDS,
  WORLD_SIZE,
  worldFitsViewport,
  worldScale,
  worldScreenSize,
  worldToLatLng,
  tileIndexAt,
  rotAABB,
} from "./world";

describe("world <-> latLng conversion", () => {
  it("round-trips the world center", () => {
    const ll = worldToLatLng(WORLD_CENTER, WORLD_CENTER);
    expect(ll).toEqual({ lat: -128, lng: 128 });
    expect(latLngToWorld(ll.lat, ll.lng)).toEqual({
      x: WORLD_CENTER,
      y: WORLD_CENTER,
    });
  });

  it("flips y: world top (y=0) maps to lat 0, bottom to lat -256", () => {
    expect(worldToLatLng(WORLD_CENTER, 0)).toEqual({
      lat: 0,
      lng: 128,
    });
    expect(worldToLatLng(WORLD_CENTER, WORLD_SIZE)).toEqual({
      lat: -256,
      lng: 128,
    });
  });

  it("round-trips arbitrary points", () => {
    for (const p of [
      { x: 0, y: 0 },
      { x: 123456789, y: 987654321 },
      { x: WORLD_SIZE, y: WORLD_SIZE },
    ]) {
      const ll = worldToLatLng(p.x, p.y);
      expect(latLngToWorld(ll.lat, ll.lng)).toEqual(p);
    }
  });
});

describe("worldScale", () => {
  it("is 1:1 native px at zoom 20", () => {
    expect(worldScale(20)).toBe(1);
  });

  it("halves per zoom level below 20", () => {
    expect(worldScale(16)).toBe(1 / 16);
    expect(worldScale(0)).toBe(2 ** -20);
    expect(worldScale(0) * WORLD_SIZE).toBe(256);
  });
});

describe("clamping", () => {
  it("clamps scalars into [0, WORLD_SIZE]", () => {
    expect(clampToWorld(-5)).toBe(0);
    expect(clampToWorld(WORLD_SIZE + 1)).toBe(WORLD_SIZE);
    expect(clampToWorld(42)).toBe(42);
  });

  it("clamps points component-wise", () => {
    expect(clampToWorldPoint({ x: -1, y: WORLD_SIZE + 9 })).toEqual({
      x: 0,
      y: WORLD_SIZE,
    });
  });
});

describe("tiles", () => {
  it("builds slippy tile URLs", () => {
    expect(tileUrl(5, 3, 7)).toBe("/api/tiles/5/3/7.webp");
  });

  it("z=0 is a single tile covering the whole world", () => {
    expect(nativeTileBBox(0, 0, 0)).toEqual({
      x: 0,
      y: 0,
      w: WORLD_SIZE,
      h: WORLD_SIZE,
    });
  });

  it("z=20 tiles are 256 native px", () => {
    expect(nativeTileBBox(20, 1048575, 1048575)).toEqual({
      x: WORLD_SIZE - 256,
      y: WORLD_SIZE - 256,
      w: 256,
      h: 256,
    });
    expect(nativeTileBBox(20, 0, 0)).toEqual({ x: 0, y: 0, w: 256, h: 256 });
  });

  it("z=16 tiles are 4096 native px", () => {
    expect(nativeTileBBox(16, 65535, 0)).toEqual({
      x: WORLD_SIZE - 4096,
      y: 0,
      w: 4096,
      h: 4096,
    });
  });

  it("parses Leaflet tile keys and rejects junk", () => {
    expect(parseTileKey("16:42:7")).toEqual({ z: 16, x: 42, y: 7 });
    expect(parseTileKey("16:4.2:7")).toBeNull();
    expect(parseTileKey("16:42")).toBeNull();
    expect(parseTileKey("a:b:c")).toBeNull();
    expect(parseTileKey("-1:0:0")).toBeNull();
  });

  it("selects exactly the tiles hit by a bbox", () => {
    const keys = ["16:65535:65535", "16:65534:65535", "16:65535:65534"];
    const hit = nativeTileBBox(16, 65535, 65535);
    expect(tilesIntersecting(keys, hit)).toEqual(["16:65535:65535"]);
  });

  it("selects nothing when bbox is outside visible tiles", () => {
    expect(tilesIntersecting(["5:3:7"], { x: -5, y: -5, w: 1, h: 1 })).toEqual(
      [],
    );
    expect(tilesIntersecting(["junk"], { x: 0, y: 0, w: 9, h: 9 })).toEqual([]);
  });
});

describe("bbox helpers", () => {
  it("detects strict overlap only", () => {
    const a = { x: 0, y: 0, w: 10, h: 10 };
    expect(bboxesIntersect(a, { x: 9, y: 9, w: 5, h: 5 })).toBe(true);
    expect(bboxesIntersect(a, { x: 10, y: 0, w: 5, h: 5 })).toBe(false);
    expect(bboxesIntersect(a, { x: 50, y: 50, w: 1, h: 1 })).toBe(false);
  });

  it("checks point containment with top-left origin", () => {
    const b = { x: 10, y: 10, w: 5, h: 5 };
    expect(bboxContainsPoint(b, 10, 10)).toBe(true);
    expect(bboxContainsPoint(b, 14, 14)).toBe(true);
    expect(bboxContainsPoint(b, 15, 12)).toBe(false);
    expect(bboxContainsPoint(b, 9, 12)).toBe(false);
  });
});

describe("queryRadiusForZoom", () => {
  it("scales down as you zoom in and clamps both ends", () => {
    expect(queryRadiusForZoom(0)).toBe(65536);
    expect(queryRadiusForZoom(16)).toBe(256);
    expect(queryRadiusForZoom(20)).toBe(256);
    expect(queryRadiusForZoom(8)).toBe(256 * 2 ** 8);
  });
});

describe("rotAABB", () => {
  it("is the identity at 0 and 180 degrees", () => {
    expect(rotAABB(100, 50, 0)).toEqual({ w: 100, h: 50 });
    expect(rotAABB(100, 50, 180)).toEqual({ w: 100, h: 50 });
    expect(rotAABB(100, 50, -180)).toEqual({ w: 100, h: 50 });
  });

  it("swaps dimensions at 90 degrees", () => {
    expect(rotAABB(100, 50, 90)).toEqual({ w: 50, h: 100 });
    expect(rotAABB(100, 50, -90)).toEqual({ w: 50, h: 100 });
  });

  it("grows a square by sqrt(2) at 45 degrees", () => {
    const aabb = rotAABB(100, 100, 45);
    expect(aabb.w).toBeCloseTo(100 * Math.SQRT2, 6);
    expect(aabb.h).toBeCloseTo(100 * Math.SQRT2, 6);
  });

  it("never shrinks below the shorter edge per axis", () => {
    for (const rot of [-180, -135, -90, -45, 0, 30, 45, 90, 135, 180]) {
      const aabb = rotAABB(100, 50, rot);
      // Each axis is at least the shorter edge, and area never decreases.
      expect(aabb.w).toBeGreaterThanOrEqual(50 - 1e-9);
      expect(aabb.h).toBeGreaterThanOrEqual(50 - 1e-9);
      expect(aabb.w * aabb.h).toBeGreaterThanOrEqual(100 * 50 - 1e-6);
    }
  });
});

describe("world bounds and fit math", () => {
  it("exposes y-flip aware LatLng bounds for the world corners", () => {
    // native bottom-left and top-right corners
    const sw = worldToLatLng(0, WORLD_SIZE);
    const ne = worldToLatLng(WORLD_SIZE, 0);
    const [[south, west], [north, east]] = WORLD_LAT_LNG_BOUNDS;
    expect(sw).toEqual({ lat: south, lng: west });
    expect(ne).toEqual({ lat: north, lng: east });
  });

  it("world centre maps to the middle of the bounds", () => {
    expect(worldToLatLng(WORLD_CENTER, WORLD_CENTER)).toEqual({
      lat: -128,
      lng: 128,
    });
  });

  it("matches the server tile formula at the world centre", () => {
    // 256 CRS.Simple units x 2^zoom px per unit / 256px tiles = 2^zoom tiles
    // per side — the server's slippy scheme at every zoom.
    expect(tileIndexAt(0, WORLD_CENTER)).toBe(0);
    expect(tileIndexAt(8, WORLD_CENTER)).toBe(128);
    expect(tileIndexAt(16, WORLD_CENTER)).toBe(32768);
    expect(tileIndexAt(19, WORLD_CENTER)).toBe(262144);
    // The centre sits exactly on a tile boundary at z20.
    expect(tileIndexAt(20, WORLD_CENTER)).toBe(524288);
  });

  it("splits tiles exactly at server tile boundaries", () => {
    // z8 tile edge in native px: 256 * 2^(20-8) = 2^20.
    const edge = 256 * 2 ** (20 - 8);
    expect(tileIndexAt(8, edge - 1)).toBe(0);
    expect(tileIndexAt(8, edge)).toBe(1);
    expect(tileIndexAt(8, 3 * edge + 5)).toBe(3);
  });

  it("computes world screen size at integer zooms", () => {
    expect(worldScreenSize(0)).toBe(256);
    expect(worldScreenSize(1)).toBe(512);
    expect(worldScreenSize(20)).toBe(268435456);
  });

  it("reports when the world fits a viewport", () => {
    expect(worldFitsViewport(1280, 800, 0)).toBe(true);
    // 512 <= 800 at zoom 1, 1024 > 800 at zoom 2
    expect(worldFitsViewport(1280, 800, 1)).toBe(true);
    expect(worldFitsViewport(1280, 800, 2)).toBe(false);
    expect(worldFitsViewport(256, 256, 0)).toBe(true);
    expect(worldFitsViewport(255, 800, 0)).toBe(false);
  });
});
