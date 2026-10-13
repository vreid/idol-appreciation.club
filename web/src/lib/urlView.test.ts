import { describe, expect, it } from "vite-plus/test";
import { MAX_ZOOM, MIN_ZOOM } from "./world";
import { encodeViewHash, parseViewHash } from "./urlView";

describe("parseViewHash", () => {
  it("parses signed integer hashes", () => {
    expect(parseViewHash("#16/0/0")).toEqual({ z: 16, x: 0, y: 0 });
    expect(parseViewHash("#18/-1280/512")).toEqual({
      z: 18,
      x: -1280,
      y: 512,
    });
    expect(parseViewHash("#3/-45/-7")).toEqual({ z: 3, x: -45, y: -7 });
  });

  it("clamps zoom into the valid range", () => {
    expect(parseViewHash("#99/0/0")?.z).toBe(MAX_ZOOM);
    expect(parseViewHash("#-2/5/6")?.z).toBe(MIN_ZOOM);
  });

  it("returns null for missing or malformed hashes", () => {
    for (const hash of [
      "",
      "#",
      "#16",
      "#16/0",
      "#16/0/0/0",
      "#ab/0/0",
      "#16/1.5/2",
      "16/0/0",
      "# ++/0/0",
    ]) {
      expect(parseViewHash(hash)).toBeNull();
    }
  });
});

describe("encodeViewHash", () => {
  it("rounds coordinates to integers and normalises -0", () => {
    expect(encodeViewHash({ z: 16, x: 0, y: 0 })).toBe("#16/0/0");
    expect(encodeViewHash({ z: 18, x: -1280.4, y: 512.6 })).toBe(
      "#18/-1280/513",
    );
    expect(encodeViewHash({ z: 16, x: -0.4, y: -0.5 })).toBe("#16/0/0");
  });

  it("clamps zoom into the valid range", () => {
    expect(encodeViewHash({ z: 99, x: 0, y: 0 }).startsWith("#20/")).toBe(true);
    expect(encodeViewHash({ z: -3, x: 0, y: 0 }).startsWith("#0/")).toBe(true);
  });
});

describe("view hash roundtrip", () => {
  it("is lossless for integer views including negatives", () => {
    for (const view of [
      { z: 16, x: 0, y: 0 },
      { z: 18, x: -1280, y: 512 },
      { z: 20, x: -134217727, y: 134217727 },
      { z: 0, x: 0, y: 0 },
      { z: 7, x: -9, y: 12345678 },
    ]) {
      expect(parseViewHash(encodeViewHash(view))).toEqual(view);
    }
  });
});
