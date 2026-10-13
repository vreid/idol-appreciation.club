/**
 * Valid/invalid fixtures per schema in contract/zod/schemas.ts. These run
 * under the web vitest gate (`task web:test`) and guard the hand-written
 * half of the wire contract; the integration suite in web/test/contract/
 * guards it against the real backend.
 */

import { describe, expect, it } from "vite-plus/test";
import {
  bboxSchema,
  centerInfoSchema,
  healthSchema,
  meSchema,
  pendingAccountsSchema,
  placedPlacementSchema,
  placeEventSchema,
  placementSchema,
  placementsNearSchema,
  registerResultSchema,
  removeEventSchema,
  worldEventSchema,
} from "./index";

/** Structural type so the helpers accept any zod schema without importing zod here. */
interface SchemaLike {
  safeParse(value: unknown): { success: boolean };
}

/** Asserts a payload parses, printing zod issues on failure. */
function ok(schema: SchemaLike, value: unknown): void {
  const parsed = schema.safeParse(value);
  expect(parsed.success).toBe(true);
}

/** Asserts a payload is rejected by a schema. */
function rejected(schema: SchemaLike, value: unknown): void {
  expect(schema.safeParse(value).success).toBe(false);
}

describe("bbox", () => {
  it("accepts four numbers and rejects other shapes", () => {
    ok(bboxSchema, [1, 2, 3, 4]);
    ok(bboxSchema, [0, -5, 0.5, 268435456]);
    rejected(bboxSchema, [1, 2, 3]);
    rejected(bboxSchema, [1, 2, 3, 4, 5]);
    rejected(bboxSchema, [1, 2, "3", 4]);
    rejected(bboxSchema, "1,2,3,4");
  });
});

describe("world events", () => {
  it("accepts a place event", () => {
    ok(worldEventSchema, { type: "place", seq: 12, bbox: [1, 2, 3, 4] });
    const parsed = placeEventSchema.safeParse({
      type: "place",
      seq: 1,
      bbox: [0, 0, 1, 1],
    });
    expect(parsed.success).toBe(true);
    if (parsed.success) expect(parsed.data).toEqual({ type: "place", seq: 1, bbox: [0, 0, 1, 1] });
  });

  it("accepts a remove event with a non-empty id", () => {
    ok(worldEventSchema, { type: "remove", id: "a", bbox: [1, 2, 3, 4] });
  });

  it("strips additive unknown fields", () => {
    const parsed = worldEventSchema.safeParse({
      type: "place",
      seq: 1,
      bbox: [1, 2, 3, 4],
      future_field: "x",
    });
    expect(parsed.success).toBe(true);
    if (parsed.success) expect(parsed.data).toEqual({ type: "place", seq: 1, bbox: [1, 2, 3, 4] });
  });

  it("rejects malformed events", () => {
    rejected(worldEventSchema, {});
    rejected(worldEventSchema, { type: "unknown", bbox: [1, 2, 3, 4] });
    rejected(worldEventSchema, { type: "place", seq: "x", bbox: [1, 2, 3, 4] });
    rejected(worldEventSchema, { type: "place", seq: 1, bbox: [1, 2] });
    rejected(worldEventSchema, { type: "remove", id: "", bbox: [1, 2, 3, 4] });
    rejected(worldEventSchema, { type: "remove", id: 7, bbox: [1, 2, 3, 4] });
  });
});

describe("placements", () => {
  const valid = {
    id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a101",
    username: "wonyoung",
    x: 134217728,
    y: 134217728,
    w: 256,
    h: 128,
    rot: 45.5,
    created_at: "2026-10-12T00:00:00Z",
  };

  it("accepts a placement and strips extras (server sends seq too)", () => {
    const parsed = placementSchema.safeParse({ ...valid, seq: 3 });
    expect(parsed.success).toBe(true);
    if (parsed.success) expect(parsed.data).toEqual(valid);
  });

  it("accepts a placement list", () => {
    ok(placementsNearSchema, []);
    ok(placementsNearSchema, [valid]);
  });

  it("rejects malformed placements and lists", () => {
    rejected(placementsNearSchema, {});
    rejected(placementsNearSchema, [{ ...valid, x: "1" }]);
    rejected(placementsNearSchema, [{ ...valid, created_at: 5 }]);
    rejected(placedPlacementSchema, valid);
  });

  it("accepts a placed placement (POST /api/place 201)", () => {
    ok(placedPlacementSchema, {
      id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a102",
      x: 1,
      y: 2,
      w: 32,
      h: 16,
      rot: 45,
      seq: 77,
    });
    rejected(placedPlacementSchema, { id: "x", x: 1, y: 2, w: 32, h: 16, rot: 45 });
  });
});

describe("center", () => {
  it("accepts an empty world", () => {
    ok(centerInfoSchema, { current: null, all_time: [] });
  });

  it("accepts a holder and all-time rows", () => {
    ok(centerInfoSchema, {
      current: {
        username: "yujin",
        since: "2026-10-12T09:00:00Z",
        placement: { id: "p", x: 1, y: 2, w: 3, h: 4, rot: 0 },
      },
      all_time: [{ username: "yujin", seconds: 800 }],
    });
  });

  it("rejects malformed center payloads", () => {
    rejected(centerInfoSchema, {});
    rejected(centerInfoSchema, { current: {}, all_time: [] });
    rejected(centerInfoSchema, { current: null, all_time: {} });
    rejected(centerInfoSchema, {
      current: null,
      all_time: [{ username: "a", seconds: "1" }],
    });
  });
});

describe("health", () => {
  it("accepts exactly ok:true", () => {
    ok(healthSchema, { ok: true });
    rejected(healthSchema, { ok: false });
    rejected(healthSchema, {});
  });
});

describe("auth", () => {
  it("accepts me payloads with null or string cooldown", () => {
    ok(meSchema, {
      id: "0197d3a4-8f2b-7c11-b3a1-5d2e9f60a0e1",
      username: "gaeul",
      is_admin: true,
      cooldown_until: "2026-10-12T10:00:00Z",
    });
    ok(meSchema, {
      id: "x",
      username: "gaeul",
      is_admin: false,
      cooldown_until: null,
    });
    rejected(meSchema, { id: 1, username: "g", is_admin: false, cooldown_until: null });
    rejected(meSchema, { id: "x", username: "g", is_admin: 0, cooldown_until: null });
  });

  it("accepts register results", () => {
    ok(registerResultSchema, { username: "liz", pending: true });
    rejected(registerResultSchema, { username: "liz" });
    rejected(registerResultSchema, { username: "liz", pending: "yes" });
  });

  it("accepts pending-account lists", () => {
    ok(pendingAccountsSchema, [
      { username: "newbie", created_at: "2026-10-12T09:00:00Z" },
    ]);
    rejected(pendingAccountsSchema, { username: "newbie" });
    rejected(pendingAccountsSchema, [{ username: "n", created_at: 1 }]);
  });
});
