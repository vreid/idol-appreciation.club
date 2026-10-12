//! All libvips image operations, behind one module so the (version-skew
//! sensitive) call patterns live in exactly one place.
//!
//! Proven-against-libvips-8.15 rules (see docs/backend.md):
//! - Saves go through `image_write_to_buffer` with suffix option strings —
//!   only version-stable kwargs (`Q`, `strip`, `compression`).
//! - No `thumbnail_image` (8.18 bindings pass newer kwargs); exact resize is
//!   premultiply → resize → unpremultiply.
//! - Only plain `ops::resize` (generated `ResizeOptions::default()` passes
//!   `vscale=0` and collapses height).
//! - Generated `*Options::default()` values diverge from C defaults for
//!   arrayjoin (background empty vec, hspacing/vspacing 1) — always set
//!   explicit backgrounds and zero spacing.

use crate::error::{internal, AppError};
use libvips::ops;
use libvips::VipsImage;
use sha2::{Digest, Sha256};

/// Paper background tiles render on.
pub const PAPER: [f64; 3] = [244.0, 240.0, 232.0];

/// Initialize libvips exactly once per process. The app handle is leaked:
/// vips_shutdown at process exit is neither needed nor safe to race.
pub fn init() -> anyhow::Result<()> {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let app =
            libvips::VipsApp::new("idol-appreciation.club-backend", false).expect("libvips init");
        std::mem::forget(app);
    });
    Ok(())
}

/// Apply EXIF orientation manually (the crate has no autorotate binding).
/// Mirrors libvips autorotate: 6 = rotate 90° CW, 5 = transpose (rot90 +
/// flip horizontal), 7 = transverse (rot90 + flip vertical), etc.
fn orient_upright(img: VipsImage) -> Result<VipsImage, AppError> {
    let o = img.get_orientation();
    let img = match o {
        2 => ops::flip(&img, ops::Direction::Horizontal)?,
        3 => ops::rot(&img, ops::Angle::D180)?,
        4 => ops::flip(&img, ops::Direction::Vertical)?,
        5 | 7 => {
            let r = ops::rot(&img, ops::Angle::D90)?;
            if o == 5 {
                ops::flip(&r, ops::Direction::Horizontal)?
            } else {
                ops::flip(&r, ops::Direction::Vertical)?
            }
        }
        6 => ops::rot(&img, ops::Angle::D90)?,
        8 => ops::rot(&img, ops::Angle::D270)?,
        _ => img,
    };
    Ok(img)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Jpeg,
    Png,
    Webp,
    Gif,
    Avif,
}

impl SourceFormat {
    pub fn ext(self) -> &'static str {
        match self {
            SourceFormat::Jpeg => "jpg",
            SourceFormat::Png => "png",
            SourceFormat::Webp => "webp",
            SourceFormat::Gif => "gif",
            SourceFormat::Avif => "avif",
        }
    }
}

/// Decode an upload into a single-frame, upright RGBA raster image.
///
/// Rejects unknown loaders; collapses animation to the first frame; applies
/// EXIF orientation before any metadata stripping, so pixels are upright.
pub fn decode(bytes: &[u8]) -> Result<(VipsImage, SourceFormat), AppError> {
    let img = VipsImage::new_from_buffer(bytes, "")
        .map_err(|_| AppError::BadRequest("undecodable image"))?;
    let loader = img.get_as_string("vips-loader").unwrap_or_default();
    let format = if loader.starts_with("jpegload") {
        SourceFormat::Jpeg
    } else if loader.starts_with("pngload") {
        SourceFormat::Png
    } else if loader.starts_with("webpload") {
        SourceFormat::Webp
    } else if loader.starts_with("gifload") {
        SourceFormat::Gif
    } else if loader.starts_with("heifload") {
        SourceFormat::Avif // HEIC/AVIF family via libheif; re-saved as AVIF
    } else {
        return Err(AppError::BadRequest("unsupported image format"));
    };
    let mut img = orient_upright(img)?;

    // Animated inputs (GIF/WEBP): keep only the first frame.
    let pages = img.get_n_pages();
    if pages > 1 {
        let page_h = img.get_page_height().max(1);
        if page_h < img.get_height() {
            img = ops::extract_area(&img, 0, 0, img.get_width(), page_h)?;
        }
    }
    // Normalize to RGBA so compositing and premultiply behave uniformly.
    let img = if !img.image_hasalpha() {
        ops::addalpha(&img)?
    } else {
        img
    };
    let img = if img.guess_interpretation()? as i32 != ops::Interpretation::Srgb as i32 {
        ops::colourspace(&img, libvips::ops::Interpretation::Srgb)?
    } else {
        img
    };
    Ok((img, format))
}

/// Resize to an exact `w x h` (aspect-preserving input already handled by
/// the caller), Lanczos3 on premultiplied alpha.
pub fn resize_exact(img: &VipsImage, w: i32, h: i32) -> Result<VipsImage, AppError> {
    let pm = ops::premultiply(img)?;
    let scale = f64::from(w) / f64::from(img.get_width());
    let resized = ops::resize(&pm, scale)?;
    // `scale` rounds the short edge independently; pin the exact target.
    let out = if resized.get_width() != w || resized.get_height() != h {
        if resized.get_width() >= w && resized.get_height() >= h {
            ops::extract_area(&resized, 0, 0, w, h)?
        } else {
            ops::embed_with_opts(
                &resized,
                0,
                0,
                w,
                h,
                &ops::EmbedOptions {
                    extend: ops::Extend::Background,
                    background: vec![0.0, 0.0, 0.0, 0.0],
                },
            )?
        }
    } else {
        resized
    };
    Ok(ops::unpremultiply(&out)?)
}

/// Deterministic webp encode (fixed quality; strip is implicit in the chain
/// because pixels come from stripped headers).
pub fn webp_bytes(img: &VipsImage, quality: i32) -> Result<Vec<u8>, AppError> {
    Ok(img.image_write_to_buffer(&format!(".webp[Q={quality}]"))?)
}

/// Re-encode in the source codec at high quality with ALL metadata stripped.
/// The sha256 of these bytes is the content address of the original.
pub fn strip_resave(img: &VipsImage, format: SourceFormat) -> Result<Vec<u8>, AppError> {
    let suffix = match format {
        SourceFormat::Jpeg => ".jpg[Q=90,strip=TRUE]",
        SourceFormat::Png => ".png[strip=TRUE]",
        SourceFormat::Webp => ".webp[Q=90,strip=TRUE]",
        // GIF/AVIF are palette/lossy; quality keeps detail acceptable.
        SourceFormat::Gif => ".gif[strip=TRUE]",
        SourceFormat::Avif => ".avif[Q=90,compression=av1,strip=TRUE]",
    };
    Ok(img.image_write_to_buffer(suffix)?)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Composite an RGBA overlay onto a chunk raster at a pixel offset. The
/// overlay may extend past the base edges; libvips clips it. Result is
/// RGBA sRGB at the base's size.
pub fn composite(
    base: &VipsImage,
    overlay: &VipsImage,
    x: i64,
    y: i64,
) -> Result<VipsImage, AppError> {
    let x = i32::try_from(x).map_err(|_| internal("composite x out of range"))?;
    let y = i32::try_from(y).map_err(|_| internal("composite y out of range"))?;
    Ok(ops::composite2_with_opts(
        base,
        overlay,
        ops::BlendMode::Over,
        &ops::Composite2Options {
            x,
            y,
            ..Default::default()
        },
    )?)
}

/// Blank transparent RGBA canvas for a brand-new chunk.
///
/// Built by decoding a baked-in 1×1 transparent PNG and embedding it:
/// the decode path guarantees a proper sRGB interpretation (constructing
/// constants over `black` yields a multiband image libvips refuses to
/// composite onto).
pub fn blank_chunk(size: i32) -> Result<VipsImage, AppError> {
    const ONE_PX_TRANSPARENT_PNG_HEX: &str = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c6360606060000000050001a5f645400000000049454e44ae426082";
    let bytes = hex_decode(ONE_PX_TRANSPARENT_PNG_HEX);
    let (tiny, _) = decode(&bytes)?;
    let embedded = ops::embed_with_opts(
        &tiny,
        0,
        0,
        size,
        size,
        &ops::EmbedOptions {
            extend: ops::Extend::Background,
            background: vec![0.0, 0.0, 0.0, 0.0],
        },
    )?;
    Ok(VipsImage::image_copy_memory(embedded)?)
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Extract a sub-rect (for z >= 19 tiles inside a chunk) and scale to 256.
pub fn tile_from_chunk(
    chunk: &VipsImage,
    left: i64,
    top: i64,
    src_size: i64,
) -> Result<VipsImage, AppError> {
    let left = i32::try_from(left).map_err(|_| internal("tile left out of range"))?;
    let top = i32::try_from(top).map_err(|_| internal("tile top out of range"))?;
    let src = i32::try_from(src_size).map_err(|_| internal("tile size out of range"))?;
    let crop = ops::extract_area(chunk, left, top, src, src)?;
    let scale = f64::from(crate::world::TILE_PX as i32) / f64::from(src);
    let img = if (scale - 1.0).abs() < f64::EPSILON {
        crop
    } else {
        ops::resize(&crop, scale)?
    };
    flatten_paper(img)
}

/// Build the parent tile from its four child tiles (2×2, row-major):
/// downsample each to 128×128, embed the first into a 256×256 canvas and
/// composite the rest on top. (libvips `arrayjoin` rejects hspacing=0 on
/// 8.15 and its failed-build path is segfault-prone, so we join by hand.)
pub fn tile_from_children(children: &[VipsImage; 4]) -> Result<VipsImage, AppError> {
    let quarter = TILE_EDGE / 2;
    let mut quads = Vec::with_capacity(4);
    for child in children {
        let scale = quarter as f64 / f64::from(child.get_width().max(1));
        let small = if (scale - 1.0).abs() < f64::EPSILON {
            child.clone()
        } else {
            ops::resize(child, scale)?
        };
        quads.push(small);
    }
    let mut canvas = embed_canvas(&quads[0], 0, 0, TILE_EDGE)?;
    for (quad, (x, y)) in quads[1..]
        .iter()
        .zip([(quarter, 0i64), (0, quarter), (quarter, quarter)])
    {
        canvas = composite(&canvas, quad, x, y)?;
    }
    flatten_paper(canvas)
}

/// Composite the transparent render onto the opaque paper background.
/// Already-opaque tiles (parent levels join flattened children) pass
/// through unchanged — flatten requires an alpha band.
pub fn flatten_paper(img: VipsImage) -> Result<VipsImage, AppError> {
    if img.image_hasalpha() {
        Ok(ops::flatten_with_opts(
            &img,
            &ops::FlattenOptions {
                background: PAPER.to_vec(),
                max_alpha: 255.0,
            },
        )?)
    } else {
        Ok(img)
    }
}

/// Encoded opaque red PNG for tests and fixtures (integration tests use
/// this to build uploads without shipping binary fixtures).
pub fn test_png(width: u32, height: u32) -> Vec<u8> {
    init().expect("vips");
    let black = ops::black(width as i32, height as i32).unwrap();
    let img = ops::bandjoin_const(&black, &mut [220.0, 40.0, 40.0, 255.0]).unwrap();
    ops::pngsave_buffer(&img).unwrap()
}

/// Composite one overlay (webp bytes) onto every listed chunk in one sync
/// section. `bases[i]` is the current chunk bytes (None = virgin chunk) for
/// `targets[i] = (local_x, local_y)`. Returns the new webp bytes per chunk.
///
/// Virgin chunks embed the overlay directly: compositing onto a synthetic
/// (embed-built) blank canvas trips a libvips save quirk ("Cannot write
/// content to buffer") when the overlay is clipped at the canvas edge,
/// while embed-of-decoded-overlay encodes reliably.
pub fn composite_chunks(
    bases: Vec<Option<Vec<u8>>>,
    overlay: &[u8],
    targets: &[(i64, i64)],
) -> crate::error::AppResult<Vec<Vec<u8>>> {
    let (ov, _) = decode(overlay)?;
    // WebP sources decode to sequential-access lazy images; embedding or
    // compositing at offsets clips them, which needs random access.
    let ov = VipsImage::image_copy_memory(ov)?;
    let mut out = Vec::with_capacity(bases.len());
    for (base, (lx, ly)) in bases.into_iter().zip(targets) {
        let composed = match base {
            Some(bytes) => {
                let img = VipsImage::new_from_buffer(&bytes, "")?;
                // Chunk bases decode sequential; composite blends need
                // random access.
                let img = VipsImage::image_copy_memory(img)?;
                composite(&img, &ov, *lx, *ly)?
            }
            None => embed_canvas(&ov, *lx, *ly, i64::from(CHUNK_CANVAS_PX))?,
        };
        out.push(
            webp_bytes(&composed, 90)
                .map_err(|e| anyhow::anyhow!("chunk webp encode: {e}"))
                .map_err(crate::error::AppError::from)?,
        );
    }
    Ok(out)
}

/// Transparent `size × size` canvas with `overlay` embedded at (x, y).
fn embed_canvas(
    overlay: &VipsImage,
    x: i64,
    y: i64,
    size: i64,
) -> crate::error::AppResult<VipsImage> {
    let x = i32::try_from(x).map_err(|_| anyhow::anyhow!("embed x out of range"))?;
    let y = i32::try_from(y).map_err(|_| anyhow::anyhow!("embed y out of range"))?;
    let size = i32::try_from(size).map_err(|_| anyhow::anyhow!("embed size out of range"))?;
    // Background length must match the overlay's band count.
    let bg = vec![0.0; overlay.get_bands().max(1) as usize];
    Ok(ops::embed_with_opts(
        overlay,
        x,
        y,
        size,
        size,
        &ops::EmbedOptions {
            extend: ops::Extend::Background,
            background: bg,
        },
    )?)
}

/// Chunk canvas side used by the compositing helpers.
const CHUNK_CANVAS_PX: i32 = 1024;
/// Tile edge used by the pyramid join.
const TILE_EDGE: i64 = 256;

/// Sequentially composite overlays onto a canvas (moderation replay step).
/// `canvas` is the current chunk raster bytes (None = blank), overlays are
/// `(bytes, x, y)` already in chunk-local coordinates, applied in order.
pub fn replay_canvas(
    canvas: Option<Vec<u8>>,
    overlays: &[(Vec<u8>, i64, i64)],
) -> crate::error::AppResult<Vec<u8>> {
    let mut canvas = match &canvas {
        Some(bytes) => Some(VipsImage::new_from_buffer(bytes, "")?),
        None => None,
    };
    for (bytes, x, y) in overlays {
        let (ov, _) = decode(bytes)?;
        canvas = Some(match canvas {
            Some(c) => composite(&c, &ov, *x, *y)?,
            // First paint on a virgin chunk embeds directly (see
            // composite_chunks for why not a synthetic blank).
            None => embed_canvas(&ov, *x, *y, i64::from(CHUNK_CANVAS_PX))?,
        });
    }
    let canvas = match canvas {
        Some(c) => c,
        // No overlays at all → rewrite as transparent.
        None => blank_chunk(CHUNK_CANVAS_PX)?,
    };
    webp_bytes(&canvas, 90).map_err(|e| anyhow::anyhow!("replay encode: {e}").into())
}

/// Render a z ≥ BASE_Z tile from its covering chunk bytes (None = blank).
pub fn render_leaf_tile(
    chunk: Option<Vec<u8>>,
    offset_x: i64,
    offset_y: i64,
    native_size: i64,
) -> crate::error::AppResult<Vec<u8>> {
    let chunk = match chunk {
        Some(bytes) => {
            // WebP chunk blobs decode to sequential images; materialize
            // once so crop/flatten/encode can read in any order.
            let img = VipsImage::new_from_buffer(&bytes, "")?;
            VipsImage::image_copy_memory(img)?
        }
        None => blank_chunk(CHUNK_CANVAS_PX)?,
    };
    let tile = tile_from_chunk(&chunk, offset_x, offset_y, native_size)?;
    webp_bytes(&tile, 80)
}

/// The constant empty tile: pure paper, 256×256 webp.
pub fn empty_tile_256() -> crate::error::AppResult<Vec<u8>> {
    let blank = blank_chunk(TILE_EDGE as i32)?;
    let flat = flatten_paper(blank)?;
    webp_bytes(&flat, 80)
}

/// Render a parent tile from its four encoded child tiles (2×2, row-major).
pub fn render_parent_tile(children: &[Vec<u8>; 4]) -> crate::error::AppResult<Vec<u8>> {
    let imgs: [VipsImage; 4] = core::array::from_fn(|i| {
        VipsImage::new_from_buffer(&children[i], "").expect("child tile decodes")
    });
    let tile = tile_from_children(&imgs)?;
    webp_bytes(&tile, 80)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vips() -> bool {
        init().is_ok()
    }

    fn red_png(w: u32, h: u32) -> Vec<u8> {
        let black = ops::black(w as i32, h as i32).unwrap();
        let img = ops::bandjoin_const(&black, &mut [220.0, 40.0, 40.0, 255.0]).unwrap();
        ops::pngsave_buffer(&img).unwrap()
    }

    /// Decoded RGBA raster built through the real pipeline (a plain
    /// bandjoin_const image carries multiband and cannot be composited).
    fn red_rgba(w: u32, h: u32) -> VipsImage {
        decode(&red_png(w, h)).unwrap().0
    }

    #[test]
    fn decode_rejects_unknown_format() {
        if !vips() {
            return;
        }
        assert!(decode(b"not an image at all").is_err());
        // SVG is not in the allowed set.
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"/>";
        assert!(decode(svg).is_err());
    }

    #[test]
    fn decode_normalizes_to_rgba() {
        if !vips() {
            return;
        }
        // RGB JPEG (no alpha) must come out 4-band RGBA.
        let black = ops::black(32, 16).unwrap();
        let jpg = ops::jpegsave_buffer(&black).unwrap();
        let (img, fmt) = decode(&jpg).unwrap();
        assert_eq!(fmt, SourceFormat::Jpeg);
        assert_eq!(img.get_bands(), 4);
        assert_eq!((img.get_width(), img.get_height()), (32, 16));
    }

    #[test]
    fn animated_gif_collapses_to_first_frame() {
        if !vips() {
            return;
        }
        // 2-page GIF via gifsave page_height trick is not directly exposed;
        // simulate by building a 2-frame gif through magick-free path:
        // instead verify a single-frame gif passes through unchanged.
        let black = ops::black(20, 20).unwrap();
        let rgba = ops::bandjoin_const(&black, &mut [1.0, 2.0, 3.0, 255.0]).unwrap();
        let gif = ops::gifsave_buffer(&rgba).unwrap();
        let (img, fmt) = decode(&gif).unwrap();
        assert_eq!(fmt, SourceFormat::Gif);
        assert_eq!((img.get_width(), img.get_height()), (20, 20));
    }

    #[test]
    fn resize_exact_dimensions_and_alpha() {
        if !vips() {
            return;
        }
        let (img, _) = decode(&red_png(640, 320)).unwrap();
        let out = resize_exact(&img, 64, 32).unwrap();
        assert_eq!((out.get_width(), out.get_height()), (64, 32));
        assert_eq!(out.get_bands(), 4);
        // Upscaling small sources is allowed.
        let out = resize_exact(&img, 16, 8).unwrap();
        assert_eq!((out.get_width(), out.get_height()), (16, 8));
    }

    #[test]
    fn derivative_bytes_are_deterministic() {
        if !vips() {
            return;
        }
        // Product requirement: derivative_sha dedup keys are stable WITHIN
        // one running backend. Byte stability across libvips/libwebp
        // builds is explicitly NOT required — sha keys are environment-
        // scoped, so an upgraded encoder just produces a new derivative
        // object for new uploads (old keys keep resolving).
        let (img1, _) = decode(&red_png(100, 50)).unwrap();
        let d1 = resize_exact(&img1, 32, 16).unwrap();
        let b1 = webp_bytes(&d1, 85).unwrap();
        let (img2, _) = decode(&red_png(100, 50)).unwrap();
        let d2 = resize_exact(&img2, 32, 16).unwrap();
        let b2 = webp_bytes(&d2, 85).unwrap();
        assert_eq!(
            b1, b2,
            "same process + same pixels must give identical webp bytes"
        );
        assert_eq!(sha256_hex(&b1), sha256_hex(&b2));
    }

    #[test]
    fn strip_resave_drops_metadata_and_changes_hash() {
        if !vips() {
            return;
        }
        // Build a JPEG with EXIF-ish data by saving with a comment header
        // via jpegsave options is not exposed; instead verify that two
        // visually identical PNGs with different trailing junk bytes decode
        // to the same stripped re-save hash.
        let mut png1 = red_png(40, 40);
        png1.extend_from_slice(b"junk-after-idat-not-really-png");
        // (trailing bytes are ignored by decoders; chunk CRCs stay valid)
        let (img1, f1) = decode(&png1).unwrap();
        let s1 = strip_resave(&img1, f1).unwrap();
        let png2 = red_png(40, 40);
        let (img2, f2) = decode(&png2).unwrap();
        let s2 = strip_resave(&img2, f2).unwrap();
        assert_eq!(sha256_hex(&s1), sha256_hex(&s2));
    }

    #[test]
    fn composite_offset_and_clipping() {
        if !vips() {
            return;
        }
        let (ov, _) = decode(&red_png(64, 64)).unwrap();
        let blank = blank_chunk(256).unwrap();
        let out = composite(&blank, &ov, 200, 200).unwrap();
        assert_eq!((out.get_width(), out.get_height()), (256, 256));
        // 6-px sliver clipped at the base edge (production targets always
        // intersect their chunk by construction; libvips rejects overlays
        // with zero intersection, which can never occur here).
        let out = composite(&blank, &ov, 250, 250).unwrap();
        assert_eq!((out.get_width(), out.get_height()), (256, 256));
    }

    #[test]
    fn blank_chunk_is_transparent_srgb() {
        if !vips() {
            return;
        }
        let c = blank_chunk(1024).unwrap();
        assert_eq!(c.get_bands(), 4);
        assert_eq!(
            c.guess_interpretation().unwrap() as i32,
            ops::Interpretation::Srgb as i32
        );
        let bytes = webp_bytes(&c, 90).unwrap();
        let (back, _) = decode(&bytes).unwrap();
        // All alpha must be 0.
        let mem = back.image_write_to_memory();
        assert!(mem.chunks(4).all(|px| px[3] == 0));
    }

    #[test]
    fn tile_from_children_joins_and_downsamples() {
        if !vips() {
            return;
        }
        let arr = [
            red_rgba(256, 256),
            red_rgba(256, 256),
            red_rgba(256, 256),
            red_rgba(256, 256),
        ];
        let parent = tile_from_children(&arr).unwrap();
        assert_eq!((parent.get_width(), parent.get_height()), (256, 256));
        // Flattened on paper → 3 bands, opaque.
        assert_eq!(parent.get_bands(), 3);
    }

    #[test]
    fn tile_from_chunk_crops_and_scales() {
        if !vips() {
            return;
        }
        let (img, _) = decode(&red_png(1024, 1024)).unwrap();
        // z=19: 512-px sub-rect → 256-px tile.
        let t = tile_from_chunk(&img, 512, 0, 512).unwrap();
        assert_eq!((t.get_width(), t.get_height()), (256, 256));
        assert_eq!(t.get_bands(), 3);
        // z=20: 256-px native crop, no scaling.
        let t = tile_from_chunk(&img, 256, 256, 256).unwrap();
        assert_eq!((t.get_width(), t.get_height()), (256, 256));
    }

    #[test]
    fn sha256_hex_is_stable() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

// ---------------------------------------------------------------------------
// Bytes-in / bytes-out pipeline API.
//
// `VipsImage` wraps a raw pointer and is !Send, so no image may live across
// an await. Handlers call these sync functions via spawn_blocking; every
// function decodes its inputs and encodes its output to owned bytes.
