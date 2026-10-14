/**
 * idol-appreciation.club wire contract — the single source of truth for
 * every JSON payload that crosses the web↔backend boundary.
 *
 * Consumers:
 * - web: parse-on-receive at the API boundary (`web/src/lib/api.ts`,
 *   `web/src/lib/sse.ts`). Malformed payloads never crash the app; they
 *   surface as a console warning plus a toast.
 * - contract integration suite (`web/test/contract/`): boots the real
 *   backend and asserts every response and SSE event parses clean.
 *
 * Rules for backend PRs:
 * - Changing a response or event shape REQUIRES updating the matching
 *   schema here in the same PR.
 * - Unknown keys are stripped (zod default), so additive backend fields
 *   are non-breaking by construction; removing/renaming a field breaks
 *   the web and must be flagged as such.
 * - IDs are opaque strings on purpose: the wire format does not promise a
 *   UUID scheme (placements are UUIDv7 today, but nothing may rely on it).
 *
 * Schemas use `zod/mini` (the functional, tree-shakeable build) — the web
 * bundles only the validators it actually uses. Phase 2 (planned, post
 * stack-wipe): schemars derives these shapes from the Rust types into
 * `contract/generated/`, and CI diffs generated vs. hand-written schemas
 * so drift fails loudly. See README.md.
 */

import { z } from "zod/mini";

/** Non-empty opaque identifier. */
const idSchema = z.string().check(z.minLength(1));

// ---------------------------------------------------------------------------
// SSE events (GET /api/events, `data:` payload of each message)
// ---------------------------------------------------------------------------

/** Native world-px AABB of a rotated placement: [x, y, w, h]. */
export const bboxSchema = z.tuple([
  z.number(),
  z.number(),
  z.number(),
  z.number(),
]);
export type BBox = z.infer<typeof bboxSchema>;

export const placeEventSchema = z.object({
  type: z.literal("place"),
  seq: z.number(),
  bbox: bboxSchema,
});

export const removeEventSchema = z.object({
  type: z.literal("remove"),
  id: idSchema,
  bbox: bboxSchema,
});

// TODO(ingest): the in-flight batch-import feature will emit a batch event
// on this stream. Reserve the shape here BEFORE the backend ships it:
//   batchEventSchema = z.object({ type: z.literal("batch"), ... })
// and add it to the worldEventSchema union below. The web drops unknown
// event types silently today; once the schema knows the shape, the web can
// start consuming it without another contract review round.

export const worldEventSchema = z.discriminatedUnion("type", [
  placeEventSchema,
  removeEventSchema,
]);

export type PlaceEvent = z.infer<typeof placeEventSchema>;
export type RemoveEvent = z.infer<typeof removeEventSchema>;
export type WorldEvent = z.infer<typeof worldEventSchema>;

// ---------------------------------------------------------------------------
// Placements
// ---------------------------------------------------------------------------

/** GET /api/placements/near entry (rot stays the unrotated rect angle). */
export const placementSchema = z.object({
  id: z.string(),
  username: z.string(),
  x: z.number(),
  y: z.number(),
  w: z.number(),
  h: z.number(),
  rot: z.number(),
  created_at: z.string(),
  /** Normalized label set (sorted; zod/mini surface — optional here so
   * older cached payloads and fixtures parse; consumers fall back to []
   * and the backend always sends the field since the tags migration). */
  tags: z.optional(z.array(z.string())),
});

/** PUT /api/placements/{id}/tags body (owner or admin). The refine
 * mirrors the backend's normalize_tags: at most 8 tags of 1..=32 chars
 * (the backend is the authority; this fails fast client-side). */
export const tagsUpdateSchema = z.object({
  tags: z
    .array(z.string())
    .check(z.refine((tags) => tags.length <= 8 && tags.every((t) => t.length <= 32))),
});

/** 200 response of the tags PUT. */
export const tagsSchema = z.object({
  id: z.string(),
  tags: z.array(z.string()),
});

export const placementsNearSchema = z.array(placementSchema);

/** One stratum in a /api/dig stack: a placement plus its 1-based dig
 * depth (1 = topmost / newest at the point). */
export const digEntrySchema = z.object({
  id: z.string(),
  username: z.string(),
  x: z.number(),
  y: z.number(),
  w: z.number(),
  h: z.number(),
  rot: z.number(),
  created_at: z.string(),
  depth: z.number(),
  tags: z.optional(z.array(z.string())),
});

/** GET /api/dig response. `stack` is newest-first, capped at 64 entries;
 * `total` is the full stack size; `truncated` reports a cut stack. */
export const digStackSchema = z.object({
  stack: z.array(digEntrySchema),
  total: z.number(),
  truncated: z.boolean(),
});


/** One GET /api/feed row: a placement plus its global `seq`, which is
 * also the keyset-pagination cursor (`?before=`). */
export const feedEntrySchema = z.object({
  id: z.string(),
  username: z.string(),
  seq: z.number(),
  x: z.number(),
  y: z.number(),
  w: z.number(),
  h: z.number(),
  rot: z.number(),
  created_at: z.string(),
  tags: z.array(z.string()),
});

/** GET /api/feed response. `items` is newest-first; `next_before` is the
 * cursor for the next-older page (null when exhausted). */
export const feedSchema = z.object({
  items: z.array(feedEntrySchema),
  next_before: z.nullable(z.number()),
});

/** 201 response of POST /api/place. */
export const placedPlacementSchema = z.object({
  id: z.string(),
  x: z.number(),
  y: z.number(),
  w: z.number(),
  h: z.number(),
  rot: z.number(),
  seq: z.number(),
});

export type Placement = z.infer<typeof placementSchema>;
export type PlacedPlacement = z.infer<typeof placedPlacementSchema>;

// ---------------------------------------------------------------------------
// Center (GET /api/center)
// ---------------------------------------------------------------------------

export const centerHolderSchema = z.object({
  username: z.string(),
  since: z.string(),
  placement: z.object({
    id: z.string(),
    x: z.number(),
    y: z.number(),
    w: z.number(),
    h: z.number(),
    rot: z.number(),
  }),
});

export const centerInfoSchema = z.object({
  current: z.nullable(centerHolderSchema),
  all_time: z.array(z.object({ username: z.string(), seconds: z.number() })),
});

export type CenterHolder = z.infer<typeof centerHolderSchema>;
export type CenterInfo = z.infer<typeof centerInfoSchema>;

// ---------------------------------------------------------------------------
// Health (GET /api/health)
// ---------------------------------------------------------------------------

export const healthSchema = z.object({ ok: z.literal(true) });
export type Health = z.infer<typeof healthSchema>;

// ---------------------------------------------------------------------------
// Auth / accounts
// ---------------------------------------------------------------------------

/** GET /api/auth/me. */
export const meSchema = z.object({
  id: z.string(),
  username: z.string(),
  is_admin: z.boolean(),
  cooldown_until: z.nullable(z.string()),
});

/** 201 response of POST /api/auth/register. */
export const registerResultSchema = z.object({
  username: z.string(),
  pending: z.boolean(),
});

/** GET /api/admin/accounts?pending=true entry. */
export const pendingAccountSchema = z.object({
  username: z.string(),
  created_at: z.string(),
});

export const pendingAccountsSchema = z.array(pendingAccountSchema);

export type Me = z.infer<typeof meSchema>;
export type RegisterResult = z.infer<typeof registerResultSchema>;
export type PendingAccount = z.infer<typeof pendingAccountSchema>;
export type DigEntry = z.infer<typeof digEntrySchema>;
export type DigStack = z.infer<typeof digStackSchema>;
export type TagsUpdate = z.infer<typeof tagsUpdateSchema>;
export type TagList = z.infer<typeof tagsSchema>;
export type FeedEntry = z.infer<typeof feedEntrySchema>;
export type Feed = z.infer<typeof feedSchema>;
