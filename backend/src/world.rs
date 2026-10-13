//! Pure world/tile/chunk geometry. No I/O, no state — heavily unit-tested.
//!
//! Conventions (stable spec):
//! - Native world is a `WORLD_PX x WORLD_PX` pixel grid (zoom 20 resolution).
//! - `x` grows right, `y` grows **down** (image convention).
//! - Tile `{z,x,y}`: `2^z` tiles per side, each covering
//!   `[x*256*2^(20-z), (x+1)*256*2^(20-z))` native px (half-open). `y=0` is
//!   the top row.
//! - Chunks are `CHUNK_PX` square, aligned at multiples of `CHUNK_PX`;
//!   `cx = x / CHUNK_PX` (all coords are non-negative, so `/` == floor).
//! - Rects are half-open `[x, x+w) x [y, y+h)`. Two rects "intersect" iff
//!   their open interiors overlap, i.e. `a.x < b.x+b.w && b.x < a.x+a.w ...`.
//!   Touching edges (zero-area overlap) do NOT count as intersection, for
//!   both the paint cooldown and tile seq coverage.

/// Native world size per side: 256 * 2^20 = 268,435,456 px.
pub const WORLD_PX: i64 = 256 * (1 << 20);
/// World center point "0/0".
pub const CENTER_X: i64 = WORLD_PX / 2;
pub const CENTER_Y: i64 = WORLD_PX / 2;
/// Chunk edge in native px.
pub const CHUNK_PX: i64 = 1024;
/// Slippy tile edge in px (also the tile pixel output size).
pub const TILE_PX: i64 = 256;
/// Maximum zoom level.
pub const MAX_Z: i64 = 20;
/// Zoom at which one tile covers exactly one chunk
/// (256 * 2^(20-18) == 1024). z >= BASE_Z renders straight from chunks;
/// z < BASE_Z renders from the four child tiles.
pub const BASE_Z: i64 = 18;

/// Half-open rectangle in native px.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Rect {
    pub const fn new(x: i64, y: i64, w: i64, h: i64) -> Self {
        Self { x, y, w, h }
    }

    /// Open-interior intersection (zero-area touch does not count).
    /// Geometry contract used by tests and property tests; production
    /// intersection decisions run in SQL against the GiST bbox index.
    #[allow(dead_code)]
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// Half-open point containment. Geometry contract (see intersects).
    #[allow(dead_code)]
    pub const fn contains(&self, px: i64, py: i64) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    /// Range of chunk column indices `cx` this rect touches.
    pub fn chunk_x_range(&self) -> (i64, i64) {
        (self.x / CHUNK_PX, (self.x + self.w - 1) / CHUNK_PX)
    }

    /// Range of chunk row indices `cy` this rect touches.
    pub fn chunk_y_range(&self) -> (i64, i64) {
        (self.y / CHUNK_PX, (self.y + self.h - 1) / CHUNK_PX)
    }

    /// All chunk (cx, cy) pairs this rect touches, in row-major order.
    pub fn chunk_keys(&self) -> Vec<(i64, i64)> {
        let (x0, x1) = self.chunk_x_range();
        let (y0, y1) = self.chunk_y_range();
        let mut keys = Vec::with_capacity(((x1 - x0 + 1) * (y1 - y0 + 1)) as usize);
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                keys.push((cx, cy));
            }
        }
        keys
    }
}

/// Placement rotation: float degrees, CLOCKWISE (y-down image
/// convention), free angle. Stored normalized to `[0, 360)`.
///
/// NaN/±inf must be rejected by the API layer before calling this.
pub fn normalize_rot(rot: f32) -> f32 {
    if rot.is_finite() {
        rot.rem_euclid(360.0)
    } else {
        0.0
    }
}

/// Conservative integer AABB of the rect rotated by `rot` degrees about
/// its own center, rounded OUTWARD (floor/ceil) so the box always
/// contains every rotated pixel. This is what the GiST `bbox` column and
/// chunk-touch ranges use. `rot` must already be normalized.
pub fn rotated_aabb(x: i64, y: i64, w: i64, h: i64, rot: f32) -> Rect {
    let (sin, cos) = (rot as f64).to_radians().sin_cos();
    // Snap quarter turns so 90°/180° boxes stay exact (f64 trig slop
    // would otherwise add a pixel of outward drift).
    let (sin, cos) = (snap(sin), snap(cos));
    let (a, b) = (cos.abs(), sin.abs());
    let cx = (x as f64) + (w as f64) / 2.0;
    let cy = (y as f64) + (h as f64) / 2.0;
    let hw = ((w as f64) * a + (h as f64) * b) / 2.0;
    let hh = ((w as f64) * b + (h as f64) * a) / 2.0;
    let x0 = (cx - hw).floor() as i64;
    let y0 = (cy - hh).floor() as i64;
    let x1 = (cx + hw).ceil() as i64;
    let y1 = (cy + hh).ceil() as i64;
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// Snap near-zero / near-one magnitudes from quarter-turn trig.
fn snap(v: f64) -> f64 {
    if v.abs() < 1e-12 {
        0.0
    } else if (v.abs() - 1.0).abs() < 1e-12 {
        v.signum()
    } else {
        v
    }
}

/// Rotated hit-test: does the rect rotated by `rot` degrees about its
/// center contain the point? The point is inverse-rotated into image
/// space and tested against the unrotated half-open bounds, so it matches
/// [`Rect::contains`] for `rot == 0`. `rot` must already be normalized.
pub fn rotated_contains(x: i64, y: i64, w: i64, h: i64, rot: f32, px: i64, py: i64) -> bool {
    let (sin, cos) = (-(rot as f64)).to_radians().sin_cos();
    let (sin, cos) = (snap(sin), snap(cos));
    let dx = (px as f64) - ((x as f64) + (w as f64) / 2.0);
    let dy = (py as f64) - ((y as f64) + (h as f64) / 2.0);
    // Rotate the point by -rot about the rect center.
    let rx = dx * cos - dy * sin;
    let ry = dx * sin + dy * cos;
    rx >= -(w as f64) / 2.0
        && rx < (w as f64) / 2.0
        && ry >= -(h as f64) / 2.0
        && ry < (h as f64) / 2.0
}

/// Object storage key of a chunk raster.
pub fn chunk_key(cx: i64, cy: i64) -> String {
    format!("chunks/{cx}/{cy}.webp")
}

/// Object storage key of a pyramid tile.
pub fn tile_key(z: i64, x: i64, y: i64) -> String {
    format!("tiles/{z}/{x}/{y}.webp")
}

/// Native px edge length of a tile at zoom `z`.
pub const fn tile_native_size(z: i64) -> i64 {
    TILE_PX << (MAX_Z - z)
}

/// Half-open native-px bbox of tile `{z,x,y}`. `x`/`y` must be valid for
/// `z` (see [`is_valid_tile`]) so the bbox stays in-world.
pub const fn tile_bounds(z: i64, x: i64, y: i64) -> Rect {
    let size = tile_native_size(z);
    Rect::new(x * size, y * size, size, size)
}

/// A tile index is valid iff `0 <= z <= 20` and `0 <= x,y < 2^z`.
pub const fn is_valid_tile(z: i64, x: i64, y: i64) -> bool {
    if z < 0 || z > MAX_Z {
        return false;
    }
    let n = 1i64 << z;
    x >= 0 && x < n && y >= 0 && y < n
}

/// The single chunk (cx, cy) containing tile `{z,x,y}` for `z >= BASE_Z`.
/// For `z == BASE_Z` tile == chunk; for `z > BASE_Z` the tile is a sub-rect
/// of the chunk and this returns the parent chunk index.
pub const fn tile_chunk(z: i64, x: i64, y: i64) -> (i64, i64) {
    (x >> (z - BASE_Z), y >> (z - BASE_Z))
}

/// Top-left corner of tile `{z,x,y}` relative to its chunk's top-left, in
/// native px. Only meaningful for `z >= BASE_Z`.
pub const fn tile_offset_in_chunk(z: i64, x: i64, y: i64) -> (i64, i64) {
    let mask = (1i64 << (z - BASE_Z)) - 1;
    (
        (x & mask) * tile_native_size(z),
        (y & mask) * tile_native_size(z),
    )
}

/// Placement rectangle derived from a requested long-edge size `L` and the
/// source aspect ratio. Long edge == `L`; short edge rounds to nearest,
/// minimum 1 px.
pub fn derive_w_h(src_w: i64, src_h: i64, long_edge: i64) -> (i64, i64) {
    assert!(src_w > 0 && src_h > 0 && long_edge > 0);
    if src_w >= src_h {
        let h = ((long_edge * src_h + src_w / 2) / src_w).max(1);
        (long_edge, h)
    } else {
        let w = ((long_edge * src_w + src_h / 2) / src_h).max(1);
        (w, long_edge)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_constants() {
        assert_eq!(WORLD_PX, 268_435_456);
        assert_eq!(CENTER_X, 134_217_728);
        assert_eq!(CHUNK_PX, 1024);
        assert_eq!(tile_native_size(0), WORLD_PX);
        assert_eq!(tile_native_size(20), 256);
        assert_eq!(tile_native_size(18), CHUNK_PX);
    }

    #[test]
    fn tile_bounds_at_extremes() {
        // z=0: single world tile.
        assert_eq!(tile_bounds(0, 0, 0), Rect::new(0, 0, WORLD_PX, WORLD_PX));
        // z=20 native tiles.
        assert_eq!(tile_bounds(20, 0, 0), Rect::new(0, 0, 256, 256));
        assert_eq!(tile_bounds(20, 1, 2), Rect::new(256, 512, 256, 256));
        // Last tiles at each zoom touch the world edge exactly.
        for z in 0..=20 {
            let n = 1i64 << z;
            let b = tile_bounds(z, n - 1, n - 1);
            assert_eq!(b.x + b.w, WORLD_PX);
            assert_eq!(b.y + b.h, WORLD_PX);
            assert!(is_valid_tile(z, n - 1, n - 1));
            assert!(!is_valid_tile(z, n, n));
        }
        assert!(!is_valid_tile(-1, 0, 0));
        assert!(!is_valid_tile(21, 0, 0));
        assert!(!is_valid_tile(2, 4, 0));
        assert!(!is_valid_tile(2, 0, -1));
    }

    #[test]
    fn chunk_keys_and_ranges() {
        assert_eq!(chunk_key(0, 0), "chunks/0/0.webp");
        assert_eq!(chunk_key(12, 345), "chunks/12/345.webp");
        // 1024-boundary: px 1023 is chunk 0, px 1024 is chunk 1.
        assert_eq!(Rect::new(0, 0, 1024, 1).chunk_keys(), vec![(0, 0)]);
        assert_eq!(
            Rect::new(1023, 1023, 2, 2).chunk_keys(),
            vec![(0, 0), (1, 0), (0, 1), (1, 1)]
        );
        // A 2048-px placement at 500 spans px 500..=2547 → 3x3 chunks.
        assert_eq!(Rect::new(500, 500, 2048, 2048).chunk_keys().len(), 9);
        assert_eq!(
            Rect::new(500, 500, 2048, 2048).chunk_keys(),
            vec![
                (0, 0),
                (1, 0),
                (2, 0),
                (0, 1),
                (1, 1),
                (2, 1),
                (0, 2),
                (1, 2),
                (2, 2),
            ]
        );
        // Aligned 2048-px placement spans exactly 2x2.
        assert_eq!(
            Rect::new(0, 0, 2048, 2048).chunk_keys(),
            vec![(0, 0), (1, 0), (0, 1), (1, 1)]
        );
        // Max placement at world edge.
        let r = Rect::new(WORLD_PX - 1024, WORLD_PX - 1, 1024, 1);
        let max_c = WORLD_PX / CHUNK_PX - 1;
        assert_eq!(r.chunk_keys(), vec![(max_c, max_c)]);
    }

    #[test]
    fn rect_intersection_touching_edges() {
        let a = Rect::new(0, 0, 10, 10);
        assert!(a.intersects(&Rect::new(9, 0, 10, 10))); // 1px overlap
        assert!(!a.intersects(&Rect::new(10, 0, 10, 10))); // shared edge
        assert!(!a.intersects(&Rect::new(0, 10, 10, 10))); // shared edge
        assert!(a.intersects(&Rect::new(-1, -1, 2, 2))); // corner overlap
        assert!(a.intersects(&a)); // itself
        assert!(!a.intersects(&Rect::new(11, 11, 5, 5)));
        // Degenerate-ish 1px rects.
        let p = Rect::new(5, 5, 1, 1);
        assert!(a.intersects(&p));
        assert!(!a.intersects(&Rect::new(10, 10, 1, 1)));
    }

    #[test]
    fn contains_half_open() {
        let r = Rect::new(10, 10, 5, 5);
        assert!(r.contains(10, 10));
        assert!(r.contains(14, 14));
        assert!(!r.contains(15, 10));
        assert!(!r.contains(10, 15));
        assert!(Rect::new(CENTER_X - 4, CENTER_Y - 4, 8, 8).contains(CENTER_X, CENTER_Y));
    }

    #[test]
    fn tile_chunk_mapping() {
        // z=18: tile == chunk.
        assert_eq!(tile_chunk(18, 5, 7), (5, 7));
        assert_eq!(tile_offset_in_chunk(18, 5, 7), (0, 0));
        // z=19: 2x2 tiles per chunk.
        assert_eq!(tile_chunk(19, 10, 11), (5, 5));
        assert_eq!(tile_offset_in_chunk(19, 10, 11), (0, 512));
        assert_eq!(tile_offset_in_chunk(19, 11, 10), (512, 0));
        // z=20: 4x4 tiles per chunk.
        assert_eq!(tile_chunk(20, 40, 43), (10, 10));
        assert_eq!(tile_offset_in_chunk(20, 43, 40), (768, 0));
    }

    #[test]
    fn derive_w_h_rounds_nearest() {
        assert_eq!(derive_w_h(100, 50, 64), (64, 32));
        assert_eq!(derive_w_h(50, 100, 64), (32, 64));
        assert_eq!(derive_w_h(3, 1, 16), (16, 5)); // 5.33 -> 5
        assert_eq!(derive_w_h(2, 3, 16), (11, 16)); // 10.67 -> 11
        assert_eq!(derive_w_h(1, 1000, 16), (1, 16)); // clamped to >=1
        assert_eq!(derive_w_h(1000, 1, 16), (16, 1));
        assert_eq!(derive_w_h(7, 7, 2048), (2048, 2048));
    }
}

#[cfg(test)]
pub(crate) fn tile_at_native(z: i64, px: i64, py: i64) -> (i64, i64) {
    let size = tile_native_size(z);
    (px / size, py / size)
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn tile_native_roundtrip_all_zooms(
            z in 0i64..=20,
            x in 0u32..(1u32 << 11),
            y in 0u32..(1u32 << 11),
            fx in 0u32..256,
            fy in 0u32..256,
        ) {
            let n = 1i64 << z;
            let (x, y) = (i64::from(x) % n, i64::from(y) % n);
            let bounds = tile_bounds(z, x, y);
            // In-world.
            prop_assert!(bounds.x >= 0 && bounds.y >= 0);
            prop_assert!(bounds.x + bounds.w <= WORLD_PX);
            prop_assert!(bounds.y + bounds.h <= WORLD_PX);
            // Sample points inside the tile round-trip back to (x, y).
            let px = bounds.x + i64::from(fx) * bounds.w / 256;
            let py = bounds.y + i64::from(fy) * bounds.h / 256;
            prop_assert_eq!(tile_at_native(z, px, py), (x, y));
            // Bottom-right edge (exclusive) belongs to the next tile.
            prop_assert_ne!(tile_at_native(z, bounds.x + bounds.w, bounds.y), (x, y));
        }

        #[test]
        fn boundary_tiles_valid_at_every_zoom(z in 0i64..=20) {
            let n = 1i64 << z;
            prop_assert!(is_valid_tile(z, 0, 0));
            prop_assert!(is_valid_tile(z, n - 1, n - 1));
            prop_assert!(!is_valid_tile(z, n, 0));
            prop_assert!(!is_valid_tile(z, 0, n));
            let last = tile_bounds(z, n - 1, n - 1);
            prop_assert_eq!(last.x + last.w, WORLD_PX);
            prop_assert_eq!(last.y + last.h, WORLD_PX);
        }

        #[test]
        fn invalid_tile_rejection(z in -2i64..=22, x in -2i64..=(1i64 << 21), y in -2i64..=(1i64 << 21)) {
            let n = if (0..=20).contains(&z) { 1i64 << z } else { 0 };
            let expected = (0..n).contains(&x) && (0..n).contains(&y);
            prop_assert_eq!(is_valid_tile(z, x, y), expected);
        }

        #[test]
        fn chunk_keys_cover_rect_exactly(
            x in 0i64..WORLD_PX,
            y in 0i64..WORLD_PX,
            w in 1i64..=2048i64,
            h in 1i64..=2048i64,
        ) {
            let rect = Rect::new(x, y, w, h);
            let keys = rect.chunk_keys();
            // Exact expected count from 1024-boundary arithmetic.
            let (x0, x1) = rect.chunk_x_range();
            let (y0, y1) = rect.chunk_y_range();
            prop_assert_eq!(keys.len(), ((x1 - x0 + 1) * (y1 - y0 + 1)) as usize);
            // Each listed chunk actually intersects the rect; every chunk
            // containing the rect's corners is listed.
            for (cx, cy) in &keys {
                let c = Rect::new(cx * CHUNK_PX, cy * CHUNK_PX, CHUNK_PX, CHUNK_PX);
                prop_assert!(rect.intersects(&c));
            }
            prop_assert_eq!(*keys.first().unwrap(), (x0, y0));
            prop_assert_eq!(*keys.last().unwrap(), (x1, y1));
            // Sample points across the rect land in the listed chunks.
            let px = x + w / 2;
            let py = y + h / 2;
            prop_assert!(keys.contains(&(px / CHUNK_PX, py / CHUNK_PX)));
        }

        #[test]
        fn derive_w_h_properties(
            sw in 1i64..=10000,
            sh in 1i64..=10000,
            l in 16i64..=2048,
        ) {
            let (w, h) = derive_w_h(sw, sh, l);
            prop_assert!(w >= 1 && h >= 1);
            if sw >= sh {
                prop_assert_eq!(w, l);
                prop_assert!(h <= l);
            } else {
                prop_assert_eq!(h, l);
                prop_assert!(w <= l);
            }
            prop_assert_eq!(max_component(w, h), l);
        }

        #[test]
        fn intersection_matches_closed_box_overlap(
            ax in 0i64..500, ay in 0i64..500,
            aw in 1i64..100, ah in 1i64..100,
            bx in -5i64..550, by in -5i64..550,
            bw in 1i64..100, bh in 1i64..100,
        ) {
            let a = Rect::new(ax, ay, aw, ah);
            let b = Rect::new(bx, by, bw, bh);
            let open = a.x < b.x + b.w && b.x < a.x + a.w
                && a.y < b.y + b.h && b.y < a.y + a.h;
            prop_assert_eq!(a.intersects(&b), open);
            // Zero-area overlap never counts.
            if a.x + a.w == b.x || b.x + b.w == a.x
                || a.y + a.h == b.y || b.y + b.h == a.y {
                let touching = (a.x + a.w == b.x || b.x + b.w == a.x)
                    && !(a.y < b.y + b.h && b.y < a.y + a.h && a.x < b.x + b.w && b.x < a.x + a.w);
                if touching { prop_assert!(!a.intersects(&b)); }
            }
        }
    }

    fn max_component(w: i64, h: i64) -> i64 {
        if w > h {
            w
        } else {
            h
        }
    }
}

#[cfg(test)]
mod rot_tests {
    use super::*;

    #[test]
    fn normalize_rot_wraps() {
        assert_eq!(normalize_rot(0.0), 0.0);
        assert_eq!(normalize_rot(370.0), 10.0);
        assert_eq!(normalize_rot(-15.0), 345.0);
        assert_eq!(normalize_rot(720.0), 0.0);
        assert_eq!(normalize_rot(359.999), 359.999);
        assert_eq!(normalize_rot(f32::NAN), 0.0);
        assert_eq!(normalize_rot(f32::INFINITY), 0.0);
    }

    #[test]
    fn aabb_identity_and_quarter_turns() {
        // rot 0: the rect itself.
        assert_eq!(
            rotated_aabb(10, 20, 100, 50, 0.0),
            Rect::new(10, 20, 100, 50)
        );
        // 90° clockwise swaps the edges about the center.
        let a = rotated_aabb(0, 0, 200, 100, 90.0);
        assert_eq!((a.w, a.h), (100, 200));
        assert_eq!(a.x, 50); // center (100, 50) stays put
        assert_eq!(a.y, -50);
        // 180°: same box.
        assert_eq!(
            rotated_aabb(10, 20, 100, 50, 180.0),
            Rect::new(10, 20, 100, 50)
        );
    }

    #[test]
    fn aabb_45_degrees_expands() {
        // 100×100 at 45°: diagonal = 100·√2 ≈ 141.42, rounded outward.
        let a = rotated_aabb(0, 0, 100, 100, 45.0);
        assert_eq!((a.w, a.h), (142, 142));
        assert_eq!(a.x, -21); // center (50, 50); 50 - 142/2 floor
                              // The unrotated rect must always sit inside the AABB.
        let r = Rect::new(0, 0, 100, 100);
        assert!(a.x <= r.x && a.y <= r.y && a.x + a.w >= r.x + r.w && a.y + a.h >= r.y + r.h);
    }

    #[test]
    fn contains_matches_unrotated_at_zero() {
        for (px, py) in [(0, 0), (99, 49), (100, 50), (-1, 0), (50, 25), (50, 26)] {
            assert_eq!(
                rotated_contains(0, 0, 100, 50, 0.0, px, py),
                Rect::new(0, 0, 100, 50).contains(px, py),
                "point {px},{py}"
            );
        }
    }

    #[test]
    fn contains_quarter_turn_orientation() {
        // 100 wide × 50 tall, 90° clockwise: the top edge faces RIGHT
        // (+x); the body spans x in [25, 75), y in [-25, 75).
        assert!(rotated_contains(0, 0, 100, 50, 90.0, 55, 25)); // right of center
        assert!(!rotated_contains(0, 0, 100, 50, 90.0, 20, 25)); // left of the rotated extent
        assert!(rotated_contains(0, 0, 100, 50, 90.0, 50, 70)); // near the bottom tip
        assert!(!rotated_contains(0, 0, 100, 50, 90.0, 50, -30)); // above the top tip
    }

    #[test]
    fn contains_misses_aabb_corner_at_45() {
        // The classic false positive: a point in the AABB but outside the
        // rotated rect's diagonal edge.
        assert!(rotated_aabb(0, 0, 100, 100, 45.0).contains(-15, -15));
        assert!(!rotated_contains(0, 0, 100, 100, 45.0, -15, -15));
        assert!(rotated_contains(0, 0, 100, 100, 45.0, 50, 50)); // center
                                                                 // On the diagonal, just inside: center + (24, -24) in a 100-box
                                                                 // rotated 45° has local rx = 0 (along the original x axis), |rx| < 50.
        assert!(rotated_contains(0, 0, 100, 100, 45.0, 50 + 24, 50 - 24));
        // Just past the corner along the same diagonal.
        assert!(!rotated_contains(0, 0, 100, 100, 45.0, 50 + 38, 50 - 38));
    }
}
