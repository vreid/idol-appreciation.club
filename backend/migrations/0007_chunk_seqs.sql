-- Per-chunk placement-version rollup (issue #120 doctrine end-state): the
-- tile read path consumes THIS table, never placement rows — view cost is
-- pile-depth-independent. A row's max_seq is the chunk's CONTENT VERSION:
-- bumped on every placement that touches the chunk, and explicitly
-- re-bumped (fresh sequence value) when a removal replays the chunk, so
-- versions only ever move forward.
CREATE TABLE chunk_seqs (
    cx      bigint NOT NULL,
    cy      bigint NOT NULL,
    max_seq bigint NOT NULL,
    PRIMARY KEY (cx, cy)
);

-- Seed at the current live maxima; writes from here on keep it fresh
-- (place/import upsert, removal replay re-bumps). One-time cost at deploy.
INSERT INTO chunk_seqs (cx, cy, max_seq)
SELECT e.cx, e.cy, max(e.seq)
FROM (
  SELECT p.seq AS seq, g.cx AS cx, g2.cy AS cy
  FROM placements p,
  generate_series(
    floor(((center(p.bbox))[0] - width(p.bbox) / 2.0) / 1024.0)::bigint,
    floor(((center(p.bbox))[0] + width(p.bbox) / 2.0) / 1024.0)::bigint
  ) g(cx),
  generate_series(
    floor(((center(p.bbox))[1] - height(p.bbox) / 2.0) / 1024.0)::bigint,
    floor(((center(p.bbox))[1] + height(p.bbox) / 2.0) / 1024.0)::bigint
  ) g2(cy)
  WHERE p.removed_at IS NULL
) e
GROUP BY e.cx, e.cy;
