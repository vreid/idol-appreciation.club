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
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    /// Half-open point containment.
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
    ((x & mask) * tile_native_size(z), (y & mask) * tile_native_size(z))
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
        assert_eq!(
            tile_bounds(20, 1, 2),
            Rect::new(256, 512, 256, 256)
        );
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
        // A 2048-px placement touches 2x2 chunks.
        assert_eq!(
            Rect::new(500, 500, 2048, 2048).chunk_keys(),
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
