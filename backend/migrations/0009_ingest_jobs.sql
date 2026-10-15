-- Phase 4 upload flow (docs/scaling-architecture.md, "Upload flow").
-- ingest_jobs is the queue behind the 202 quarantine placement path:
-- POST /api/place validates and stashes the upload, the ingest consumer
-- does the pixel work and records the placement; the client polls
-- GET /api/uploads/{id} and listens for the `placed`/`place_failed`
-- events. Removal jobs ride the same queue so a delete of a
-- still-quarantined placement applies after the placement lands.

CREATE TABLE ingest_jobs (
  id            uuid PRIMARY KEY,                  -- the upload id the client holds from the 202
  kind          text        NOT NULL DEFAULT 'place', -- place | removal
  account_id    uuid        REFERENCES accounts (id) ON DELETE SET NULL, -- NULL for admin removals
  placement_id  uuid,                              -- place: filled on completion; removal: the target
  quarantine_key text,                              -- place only: quarantine/{upload_id} object
  x             bigint,
  y             bigint,
  w             integer,
  h             integer,
  rot           real        NOT NULL DEFAULT 0,
  size          integer,                           -- derivative long edge requested by the client
  state         text        NOT NULL DEFAULT 'pending', -- pending | running | placed | failed
  attempts      smallint    NOT NULL DEFAULT 0,
  claimed_at    timestamptz,
  not_before    timestamptz NOT NULL DEFAULT now(),
  seq           bigint,                            -- drawn when the placement row commits
  aabb_x        bigint,                            -- AABB of the rotated rect, filled on completion
  aabb_y        bigint,
  aabb_w        bigint,
  aabb_h        bigint,
  error         text,
  created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX ingest_jobs_state_idx ON ingest_jobs (state, not_before);
