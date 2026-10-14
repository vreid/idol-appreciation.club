-- Phase 3 chunk-and-tile pipeline (docs/scaling-architecture.md,
-- "Chunk and tile pipeline"). chunk_seqs gains what has been PAINTED
-- next to what is WANTED; render_jobs is the one job table keyed by
-- (z, x, y) that covers chunk jobs (z = 18) and parent jobs (z < 18).

ALTER TABLE chunk_seqs
  ADD COLUMN painted_seq   bigint  NOT NULL DEFAULT 0,      -- version the stored raster and tiles show
  ADD COLUMN needs_rebuild boolean NOT NULL DEFAULT false;  -- set by removals

CREATE TABLE render_jobs (
  z smallint NOT NULL, x integer NOT NULL, y integer NOT NULL, -- z = 18: chunk job; z < 18: parent job
  state      text        NOT NULL DEFAULT 'pending',          -- pending | running | failed
  rerun      boolean     NOT NULL DEFAULT false,              -- changed while running
  not_before timestamptz NOT NULL DEFAULT now(),              -- per-zoom delay
  claimed_at timestamptz,
  attempts   smallint    NOT NULL DEFAULT 0,
  PRIMARY KEY (z, x, y)
);
