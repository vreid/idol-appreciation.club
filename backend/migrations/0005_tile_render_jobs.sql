-- Background tile re-render queue (design: docs/backend.md, "Tile serving
-- and admission control"). One row per tile coord. INSERT = demand for a
-- re-render, 'rendering' = a worker claim, 'done' = rendered at a recent
-- seq. A future out-of-process render worker consumes the same table via
-- the claim query -- SKIP LOCKED is the whole coordination protocol.

CREATE TABLE tile_render_jobs (
    z smallint NOT NULL,
    x integer NOT NULL,
    y integer NOT NULL,
    state text NOT NULL DEFAULT 'pending',
    enqueued_at timestamptz NOT NULL DEFAULT now(),
    -- Quiet-window anchor: every stale GET for a queued coord resets this.
    last_bump_at timestamptz NOT NULL DEFAULT now(),
    -- Set when a worker claims the row; lease expiry makes it reclaimable.
    claimed_at timestamptz,
    PRIMARY KEY (z, x, y)
);

CREATE INDEX tile_render_jobs_pending_idx
    ON tile_render_jobs (enqueued_at) WHERE state = 'pending';

CREATE INDEX tile_render_jobs_rendering_idx
    ON tile_render_jobs (claimed_at) WHERE state = 'rendering';
