import { describe, expect, it } from "vite-plus/test";
import {
  clampLongEdge,
  DEFAULT_LONG_EDGE,
  isUploadSizeOk,
  longEdgeToSize,
  MAX_LONG_EDGE,
  MIN_LONG_EDGE,
} from "./sizing";

describe("clampLongEdge", () => {
  it("clamps into [16, 4096] and rounds", () => {
    expect(clampLongEdge(0)).toBe(MIN_LONG_EDGE);
    expect(clampLongEdge(15.4)).toBe(MIN_LONG_EDGE);
    expect(clampLongEdge(300.6)).toBe(301);
    expect(clampLongEdge(9999)).toBe(MAX_LONG_EDGE);
    expect(clampLongEdge(DEFAULT_LONG_EDGE)).toBe(DEFAULT_LONG_EDGE);
  });
});

describe("longEdgeToSize", () => {
  it("lands the long edge exactly on the target for landscape", () => {
    expect(longEdgeToSize(1000, 500, 256)).toEqual({ w: 256, h: 128 });
  });

  it("lands the long edge exactly on the target for portrait", () => {
    expect(longEdgeToSize(500, 1000, 256)).toEqual({ w: 128, h: 256 });
  });

  it("keeps near-square images balanced", () => {
    expect(longEdgeToSize(101, 100, 256)).toEqual({ w: 256, h: 253 });
  });

  it("scales a tiny image up to the requested long edge", () => {
    expect(longEdgeToSize(8, 4, 512)).toEqual({ w: 512, h: 256 });
  });

  it("clamps the requested long edge", () => {
    expect(longEdgeToSize(200, 100, 1)).toEqual({ w: 16, h: 8 });
    expect(longEdgeToSize(200, 100, 5000)).toEqual({ w: 4096, h: 2048 });
  });

  it("guards against degenerate natural sizes", () => {
    expect(longEdgeToSize(0, 0, 64)).toEqual({ w: 64, h: 64 });
    expect(longEdgeToSize(1, 100, 200)).toEqual({ w: 2, h: 200 });
  });
});

describe("isUploadSizeOk", () => {
  it("accepts files up to 10MB and rejects the rest", () => {
    expect(isUploadSizeOk(1)).toBe(true);
    expect(isUploadSizeOk(10 * 1024 * 1024)).toBe(true);
    expect(isUploadSizeOk(10 * 1024 * 1024 + 1)).toBe(false);
    expect(isUploadSizeOk(0)).toBe(false);
    expect(isUploadSizeOk(-5)).toBe(false);
  });
});
