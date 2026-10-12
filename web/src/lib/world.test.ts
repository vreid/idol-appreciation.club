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
  WORLD_SIZE,
  worldScale,
  worldToLatLng,
} from "./world";

describe("world <-> latLng conversion", () => {
  it("round-trips the world center", () => {
    const ll = worldToLatLng(WORLD_CENTER, WORLD_CENTER);
    expect(ll).toEqual({ lat: -0.5, lng: 0.5 });
    expect(latLngToWorld(ll.lat, ll.lng)).toEqual({
      x: WORLD_CENTER,
      y: WORLD_CENTER,
    });
  });

  it("flips y: world top (y=0) maps to lat 0, bottom to lat -1", () => {
    expect(worldToLatLng(WORLD_CENTER, 0)).toEqual({
      lat: -0,
      lng: 0.5,
    });
    expect(worldToLatLng(WORLD_CENTER, WORLD_SIZE)).toEqual({
      lat: -1,
      lng: 0.5,
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
