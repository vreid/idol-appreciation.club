-- Rotation for placements: float degrees, CLOCKWISE (y-down image
-- convention), default 0, normalized to [0, 360) on write. Free angle —
-- no snapping.
ALTER TABLE placements ADD COLUMN rot real NOT NULL DEFAULT 0;

-- bbox becomes a regular column: it now stores the AABB of the ROTATED
-- rect, computed once in Rust (world::rotated_aabb) and written by the
-- INSERT, so the GiST index, chunk scoping and cooldown overlap all use
-- the same conservative box. Existing rows keep their (correct for
-- rot = 0) stored values.
ALTER TABLE placements ALTER COLUMN bbox DROP EXPRESSION;
