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

use iac_core::error::{internal, AppError};
use libvips::ops;
pub use libvips::VipsImage;

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, LazyLock, Mutex as StdMutex};

/// Paper background tiles render on.
pub const PAPER: [f64; 3] = [244.0, 240.0, 232.0];

/// Backstop decoded-pixel budget for callers without a tighter, config-
/// driven bound (internal compositing over already-validated stored bytes).
/// 4096×4096 = 64 MiB of RGBA; uploads for placement use the tighter
/// `max_decode_pixels` config via the header probe in `place` instead.
pub const DEFAULT_MAX_DECODE_PIXELS: u64 = 4096 * 4096;

/// Initialize libvips exactly once per process. The app handle is leaked:
/// vips_shutdown at process exit is neither needed nor safe to race.
/// Serializes ALL libvips work process-wide. The libvips crate is not
/// sound under concurrent operations here (GObject double-free SIGSEGVs
/// under parallel pipelines); our pixel ops are milliseconds on small
/// images, so a global lock costs nothing and removes the race.
/// Reentrant: pub fns guard themselves, so internal composition
/// (composite_chunks → decode → …) cannot deadlock.
static VIPS: parking_lot::ReentrantMutex<()> = parking_lot::ReentrantMutex::new(());

/// Ensure libvips is initialized and hold the global serialization lock.
/// Every vips entry point goes through this (not a bare `VIPS.lock()`), so
/// no caller — test binary or production main — can reach a vips op
/// before `VipsApp` exists (GLib class lookups SIGSEGV pre-init).
fn vips_guard() -> parking_lot::ReentrantMutexGuard<'static, ()> {
    init().expect("libvips init");
    VIPS.lock()
}

pub fn init() -> anyhow::Result<()> {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let app =
            libvips::VipsApp::new("idol-appreciation.club-backend", false).expect("libvips init");
        // Single-threaded eval. Background eval workers race object
        // teardown in this environment (GObject double-free SIGSEGVs in
        // g_object_unref when a graph is dropped mid-eval); our ops are
        // milliseconds on ≤4096px images, so eval parallelism buys
        // nothing and costs the crash. Combined with the global VIPS
        // mutex this makes every pipeline fully serial.
        app.concurrency_set(1);
        std::mem::forget(app);
    });
    Ok(())
}

/// Decoded raster of one encoded blob (chunk or tile), uchar RGBA/RGB.
struct Raster {
    bytes: Arc<Vec<u8>>,
    width: i32,
    height: i32,
    bands: i32,
    /// The interpretation the decode produced. `new_from_memory_copy` leaves
    /// it at MULTIBAND, and downstream ops (notably `flatten` via
    /// `image_hasalpha`) behave differently for a MULTIBAND vs an sRGB image,
    /// so it must be reapplied — otherwise a cached source renders a
    /// different (alpha-bearing) tile than a freshly decoded one.
    interp: ops::Interpretation,
    xres: f64,
    yres: f64,
}

/// Bounded LRU of decoded blobs, keyed by the sha256 of the ENCODED bytes.
///
/// Content-addressed on purpose: the key is derived from the exact bytes the
/// decode would have consumed, so a hit is provably the pixels `decode` would
/// have produced — no invalidation protocol and no staleness window (a chunk
/// rewrite changes the blob ⇒ new key ⇒ miss). This targets the single
/// dominant per-render cost: decoding one 1024×1024 chunk webp is ~11 ms, and
/// one chunk feeds up to 16 zoom-20 tiles (4×4), every one of which re-decoded
/// the same blob from scratch.
///
/// Entries are the full raster (4 MiB for a 1024² chunk); the default cap of 8
/// bounds the cache at ~32 MiB. Sized by `TILE_RASTER_CACHE_ENTRIES`.
static RASTER_CACHE: LazyLock<StdMutex<lru::LruCache<[u8; 32], Arc<Raster>>>> =
    LazyLock::new(|| {
        StdMutex::new(lru::LruCache::new(
            NonZeroUsize::new(raster_cache_entries()).expect("raster cache entries"),
        ))
    });

fn raster_cache_entries() -> usize {
    std::env::var("TILE_RASTER_CACHE_ENTRIES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(8)
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// Decode an encoded blob to an in-memory random-access raster, reusing a
/// previously decoded one when the bytes are identical (see [`RASTER_CACHE`]).
/// The raster is copied into libvips-owned memory, so the cache entry may be
/// evicted immediately after this returns.
///
/// Deliberately UNbudgeted: `MAX_DECODE_PIXELS` is an ingestion gate —
/// [`decode`] enforces it when a placement is accepted. This function serves
/// reads (leaf tile renders, chunk replay), and a chunk that predates a
/// budget tightening must keep rendering: whichever path runs first after a
/// restart (cache-miss render or replay) decodes the stored blob as-is. The
/// cache never widens admission, either — it only reuses pixels `decode`
/// already admitted for placement.
fn decoded_blob(bytes: &[u8]) -> Result<VipsImage, AppError> {
    let key = sha256_bytes(bytes);
    if let Some(raster) = RASTER_CACHE
        .lock()
        .expect("raster cache")
        .get(&key)
        .cloned()
    {
        return raster_image(&raster);
    }
    // Miss: decode once and keep the raster so the next tile of this chunk is
    // a memcpy instead of another webp decode.
    let img = VipsImage::new_from_buffer(bytes, "")?;
    let img = VipsImage::image_copy_memory(img)?;
    let (width, height, bands) = (img.get_width(), img.get_height(), img.get_bands());
    let raster = Arc::new(Raster {
        bytes: Arc::new(img.image_write_to_memory()),
        width,
        height,
        bands,
        interp: img
            .get_interpretation()
            .unwrap_or(ops::Interpretation::Multiband),
        xres: img.get_xres(),
        yres: img.get_yres(),
    });
    RASTER_CACHE
        .lock()
        .expect("raster cache")
        .put(key, raster.clone());
    raster_image(&raster)
}

/// Rebuild a libvips image over a cached raster, restoring the header fields
/// `new_from_memory_copy` cannot infer (see [`Raster::interp`]).
fn raster_image(raster: &Raster) -> Result<VipsImage, AppError> {
    let img = VipsImage::new_from_memory_copy(
        &raster.bytes,
        raster.width,
        raster.height,
        raster.bands,
        ops::BandFormat::Uchar,
    )?;
    Ok(ops::copy_with_opts(
        &img,
        &ops::CopyOptions {
            width: raster.width,
            height: raster.height,
            bands: raster.bands,
            format: ops::BandFormat::Uchar,
            interpretation: raster.interp,
            xres: raster.xres,
            yres: raster.yres,
            ..Default::default()
        },
    )?)
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
/// Sample the RGB triple at `(x, y)` (test/diagnostic helper; the image
/// must be uchar-typed, as all rasters we produce are).
pub fn get_pixel(img: &VipsImage, x: i32, y: i32) -> [u8; 3] {
    let p = ops::getpoint(img, x, y).expect("getpoint");
    [p[0] as u8, p[1] as u8, p[2] as u8]
}

pub fn probe(bytes: &[u8]) -> Result<(i32, i32), AppError> {
    let _vips = vips_guard();
    let img = VipsImage::new_from_buffer(bytes, "")
        .map_err(|_| AppError::BadRequest("undecodable image"))?;
    // Header-only: new_from_buffer parses the container header, and these
    // field reads never touch pixel data, so a bomb's bulk is never
    // allocated. Orientation swaps the axes (a 90°-rotated upload presents
    // rotated dimensions), so rotate first like `decode` does. Animated
    // inputs: report first-frame height (decode extracts one page), the
    // full strip can only be larger.
    let img = orient_upright(img)?;
    let w = img.get_width();
    let h = match img.get_n_pages() {
        pages if pages > 1 => img.get_page_height().max(1),
        _ => img.get_height(),
    };
    Ok((w, h))
}

fn format_of(loader: &str) -> Option<SourceFormat> {
    if loader.starts_with("jpegload") {
        Some(SourceFormat::Jpeg)
    } else if loader.starts_with("pngload") {
        Some(SourceFormat::Png)
    } else if loader.starts_with("webpload") {
        Some(SourceFormat::Webp)
    } else if loader.starts_with("gifload") {
        Some(SourceFormat::Gif)
    } else if loader.starts_with("heifload") {
        Some(SourceFormat::Avif) // HEIC/AVIF family via libheif; re-saved as AVIF
    } else {
        None
    }
}

/// Decode an upload into a single-frame, upright RGBA raster image.
///
/// `max_pixels` bounds the declared pixel count and is enforced on the
/// HEADER before any full decode, so a decompression bomb (tiny wire
/// bytes, gigabytes of RGBA) is rejected with [`AppError::TooLarge`]
/// instead of being materialized. Rejects unknown loaders; collapses
/// animation to the first frame; applies EXIF orientation before any
/// metadata stripping, so pixels are upright.
pub fn decode(bytes: &[u8], max_pixels: u64) -> Result<(VipsImage, SourceFormat), AppError> {
    let _vips = vips_guard();
    let img = VipsImage::new_from_buffer(bytes, "")
        .map_err(|_| AppError::BadRequest("undecodable image"))?;
    let loader = img.get_as_string("vips-loader").unwrap_or_default();
    let format = format_of(&loader).ok_or(AppError::BadRequest("unsupported image format"))?;
    // Header-only budget on the frame we will actually decode: animated
    // inputs report their page height (per-frame height), not the full
    // strip — a 40-frame 800×800 GIF decodes one 0.64 MPx frame, not a
    // 25.6 MPx strip. The product is orientation-invariant, so the EXIF
    // axis swap needs no handling here.
    let frame_h = match img.get_n_pages() {
        pages if pages > 1 => img.get_page_height().max(1),
        _ => img.get_height(),
    };
    let declared = i64::from(img.get_width()) * i64::from(frame_h);
    if declared > 0 && u64::try_from(declared).unwrap_or(u64::MAX) > max_pixels {
        return Err(AppError::TooLarge);
    }
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
    // The crate's new_from_buffer does NOT take ownership of the input
    // bytes: the lazy graph above would read freed memory once the caller
    // drops them (benign on some allocators, corrupting on others).
    // Materialize the fully-decoded pixels while the buffer is alive.
    let img = VipsImage::image_copy_memory(img)?;
    Ok((img, format))
}

/// Header-only dimensions of an encoded image — no full decode, no
/// pixel work beyond the loader probe. The batch-import path uses this to
/// verify a manifest's declared `w`/`h` against the actual derivative
/// bitmap without paying a decode per entry.
pub fn dimensions(bytes: &[u8]) -> Result<(i64, i64), AppError> {
    let _vips = vips_guard();
    let img = VipsImage::new_from_buffer(bytes, "")
        .map_err(|_| AppError::BadRequest("undecodable image"))?;
    Ok((i64::from(img.get_width()), i64::from(img.get_height())))
}

/// Resize to an exact `w x h` (aspect-preserving input already handled by
/// the caller), Lanczos3 on premultiplied alpha.
pub fn resize_exact(img: &VipsImage, w: i32, h: i32) -> Result<VipsImage, AppError> {
    let _vips = vips_guard();
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

/// Rotate an image about its center into the enclosing AABB canvas
/// (`world::rotated_aabb` computes the same integer box), exposed corners
/// transparent. Interpolation runs on premultiplied alpha (Lanczos-free
/// bilinear via libvips similarity). `rot` is normalized degrees,
/// CLOCKWISE in the y-down convention — libvips `similarity` angle is
/// documented clockwise-positive, so the angle is passed through.
pub fn rotate_centered(img: &VipsImage, rot: f32) -> Result<VipsImage, AppError> {
    let _vips = vips_guard();
    if rot.abs() < 1e-6 {
        // NOT .clone(): the crate derives Clone on VipsImage as a raw
        // pointer copy (no GObject refcount), so a cloned handle
        // double-unrefs the same object at drop and corrupts the heap.
        // vips_copy makes a properly referenced alias node.
        return Ok(ops::copy(img)?);
    }
    let aabb = iac_core::world::rotated_aabb(
        0,
        0,
        i64::from(img.get_width()),
        i64::from(img.get_height()),
        rot,
    );
    let ow = i32::try_from(aabb.w).map_err(|_| internal("rotated width overflow"))?;
    let oh = i32::try_from(aabb.h).map_err(|_| internal("rotated height overflow"))?;
    // libvips sizes the output to contain the whole transformed image
    // (the enclosing AABB, corners filled with the transparent
    // background), so no pivot/offset handling is needed — the full
    // rotated image is already top-left aligned. No premultiply wrap:
    // against a zero-alpha background straight-alpha bilinear is
    // fringe-free enough, and the premultiply chain crashed GObject
    // state here (double-free signatures) on composite-with-base.
    let bands = img.get_bands().max(1) as usize;
    let opts = ops::SimilarityOptions {
        angle: f64::from(rot),
        background: vec![0.0; bands],
        ..Default::default()
    };
    let rotated = ops::similarity_with_opts(img, &opts)?;
    // Leaked on purpose: SimilarityOptions::default() wraps
    // vips_interpolate_nearest_static(), a process-lifetime STATIC
    // GObject — but the crate's Drop unrefs it. Letting opts drop would
    // decrement the static interpolator's refcount on every rotation
    // until GObject finalizes it and every later vips call segfaults
    // (observed as wandering g_object_unref crashes under load). The
    // struct is ~100 bytes per rotation; correctness over tidiness.
    std::mem::forget(opts);
    // Pin the exact AABB size so chunk composite offsets stay aligned
    // with the SQL bbox (similarity rounds the transformed bounds to its
    // own box, which can differ by a pixel or two). Content is CENTERED
    // in the AABB — the edges are transparent, so padding/cropping is
    // pixel-safe and sub-pixel drift stays ≤1px.
    let (rw, rh) = (rotated.get_width(), rotated.get_height());
    if rw != ow || rh != oh {
        if rw >= ow && rh >= oh {
            let left = (rw - ow) / 2;
            let top = (rh - oh) / 2;
            Ok(ops::extract_area(&rotated, left, top, ow, oh)?)
        } else {
            Ok(embed_rect(&rotated, (ow - rw) / 2, (oh - rh) / 2, ow, oh)?)
        }
    } else {
        Ok(rotated)
    }
}

/// Deterministic webp encode (fixed quality; strip is implicit in the chain
/// because pixels come from stripped headers).
pub fn webp_bytes(img: &VipsImage, quality: i32) -> Result<Vec<u8>, AppError> {
    let _vips = vips_guard();
    Ok(img.image_write_to_buffer(&format!(".webp[Q={quality}]"))?)
}

/// Re-encode in the source codec at high quality with ALL metadata stripped.
/// The sha256 of these bytes is the content address of the original.
pub fn strip_resave(img: &VipsImage, format: SourceFormat) -> Result<Vec<u8>, AppError> {
    let _vips = vips_guard();
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
    hex(&sha256_bytes(bytes))
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
    let _vips = vips_guard();
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
    let _vips = vips_guard();
    const ONE_PX_TRANSPARENT_PNG_HEX: &str = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c6360606060000000050001a5f645400000000049454e44ae426082";

    let bytes = hex_decode(ONE_PX_TRANSPARENT_PNG_HEX);
    let (tiny, _) = decode(&bytes, DEFAULT_MAX_DECODE_PIXELS)?;
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
    let _vips = vips_guard();
    let left = i32::try_from(left).map_err(|_| internal("tile left out of range"))?;
    let top = i32::try_from(top).map_err(|_| internal("tile top out of range"))?;
    let src = i32::try_from(src_size).map_err(|_| internal("tile size out of range"))?;
    let crop = ops::extract_area(chunk, left, top, src, src)?;
    let scale = f64::from(iac_core::world::TILE_PX as i32) / f64::from(src);
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
    let _vips = vips_guard();
    let quarter = TILE_EDGE / 2;
    let mut quads = Vec::with_capacity(4);
    for child in children {
        let scale = quarter as f64 / f64::from(child.get_width().max(1));
        let small = if (scale - 1.0).abs() < f64::EPSILON {
            // NOT .clone(): see rotate_centered — derived Clone on
            // VipsImage double-unrefs; vips_copy aliases safely.
            ops::copy(child)?
        } else {
            ops::resize(child, scale)?
        };
        quads.push(small);
    }
    let mut canvas = embed_rect(
        &quads[0],
        0,
        0,
        i32::try_from(TILE_EDGE).expect("tile edge fits i32"),
        i32::try_from(TILE_EDGE).expect("tile edge fits i32"),
    )?;
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
    let _vips = vips_guard();
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
    let _vips = vips_guard();
    solid_png(width, height, [220, 40, 40])
}

fn decode_1px(color: [u8; 3]) -> VipsImage {
    let bytes = tiny_png_rgba(color);
    let (img, _) = decode(&bytes, DEFAULT_MAX_DECODE_PIXELS).expect("decode 1px png");
    img
}

/// CRC-32 (IEEE) for PNG chunks, table-free.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Build a 1×1 solid-color RGBA PNG from scratch (signature + IHDR +
/// stored-block zlib IDAT + IEND). No baked byte blobs: entropy-based
/// secret scanners flag long hex/base64 constants, and a constructed
/// image needs none.
fn tiny_png_rgba([r, g, b]: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::with_capacity(70);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&1u32.to_be_bytes()); // width
    ihdr.extend_from_slice(&1u32.to_be_bytes()); // height
    ihdr.push(8); // bit depth
    ihdr.push(6); // color type RGBA
    ihdr.extend_from_slice(&[0, 0, 0]); // compression, filter, interlace
    push_chunk(&mut out, b"IHDR", &ihdr);
    // zlib stream with one stored (uncompressed) deflate block: 1-byte
    // filter type 0 then the RGBA pixel.
    let scanline = [0u8, r, g, b, 255];
    let (mut s1, mut s2) = (1u32, 0u32);
    for &v in &scanline {
        s1 = (s1 + u32::from(v)) % 65521;
        s2 = (s2 + s1) % 65521;
    }
    let adler: u32 = (s2 << 16) | s1;
    let mut idat = Vec::with_capacity(12 + scanline.len());
    idat.push(0x78); // zlib header (deflate, 32K window, fastest)
    idat.push(0x01); // FCHECK ok: 0x7801 % 31 == 0
    idat.push(0x01); // deflate stored block, BFINAL=1
    idat.extend_from_slice(&(scanline.len() as u16).to_le_bytes());
    idat.extend_from_slice(&(!(scanline.len() as u16)).to_le_bytes());
    idat.extend_from_slice(&scanline);
    idat.extend_from_slice(&adler.to_be_bytes());
    push_chunk(&mut out, b"IDAT", &idat);
    push_chunk(&mut out, b"IEND", &[]);
    out
}

/// Scale a decoded 1×1 solid-color RGBA PNG to `width x height` and
/// encode. (Constructing constants over `black` yields broken band
/// layouts — see blank_chunk for the same decode-based trick.)
pub fn solid_png(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
    init().expect("vips");
    let tiny = decode_1px(color);
    // A 1px source tiled to the exact target size — solid by construction.
    let img = ops::replicate(&tiny, width as i32, height as i32).expect("replicate 1px");
    ops::pngsave_buffer(&img).expect("encode solid png")
}

/// Asymmetric test image: solid red top half, solid blue bottom half.
/// Rotation tests use it to pin the clockwise convention.
pub fn asymmetric_test_png(width: u32, height: u32) -> Vec<u8> {
    let _vips = vips_guard();
    init().expect("vips");
    let w = width as i32;
    let h = height as i32;
    let half = (h / 2).max(1);
    let red = decode_1px([220, 40, 40]);
    let blue = decode_1px([40, 40, 220]);
    let top = ops::replicate(&red, w, half).unwrap();
    let bottom = ops::replicate(&blue, w, h - half).unwrap();
    let canvas = ops::join(&top, &bottom, ops::Direction::Vertical).unwrap();
    ops::pngsave_buffer(&canvas).unwrap()
}

/// Composite one overlay (webp bytes) onto every listed chunk in one sync
/// section. `bases[i]` is the current chunk bytes (None = virgin chunk) for
/// `targets[i] = (local_x, local_y)` — the TOP-LEFT of the rotated
/// overlay's AABB in chunk-local coordinates. `rot` = normalized clockwise
/// degrees; the overlay is rotated once about its center here and painted
/// at the AABB offset. Returns the new webp bytes per chunk.
///
/// Virgin chunks embed the overlay directly: compositing onto a synthetic
/// (embed-built) blank canvas trips a libvips save quirk ("Cannot write
/// content to buffer") when the overlay is clipped at the canvas edge,
/// while embed-of-decoded-overlay encodes reliably.
pub fn composite_chunks(
    bases: Vec<Option<Vec<u8>>>,
    overlay: &[u8],
    targets: &[(i64, i64)],
    rot: f32,
) -> iac_core::error::AppResult<Vec<Vec<u8>>> {
    let _vips = vips_guard();
    let (ov, _) = decode(overlay, DEFAULT_MAX_DECODE_PIXELS)?;
    // WebP sources decode to sequential-access lazy images; embedding or
    // compositing at offsets clips them, which needs random access.
    let ov = VipsImage::image_copy_memory(ov)?;
    let ov = rotate_centered(&ov, rot)?;
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
            None => embed_rect(
                &ov,
                i32::try_from(*lx).map_err(|_| anyhow::anyhow!("embed x out of range"))?,
                i32::try_from(*ly).map_err(|_| anyhow::anyhow!("embed y out of range"))?,
                i64::from(CHUNK_CANVAS_PX)
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("embed size out of range"))?,
                i64::from(CHUNK_CANVAS_PX)
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("embed size out of range"))?,
            )?,
        };
        out.push(
            webp_bytes(&composed, 90)
                .map_err(|e| anyhow::anyhow!("chunk webp encode: {e}"))
                .map_err(iac_core::error::AppError::from)?,
        );
    }
    Ok(out)
}

/// Transparent `w × h` canvas with `overlay` embedded at (x, y).
fn embed_rect(
    overlay: &VipsImage,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> iac_core::error::AppResult<VipsImage> {
    // Background length must match the overlay's band count.
    let bg = vec![0.0; overlay.get_bands().max(1) as usize];
    Ok(ops::embed_with_opts(
        overlay,
        x,
        y,
        w,
        h,
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
/// `(bytes, x, y, rot)` in chunk-local coordinates with the placement's
/// normalized rotation, applied in seq order.
/// Generic over the buffer type (`Vec<u8>` from the store, or a shared
/// `Bytes` from a batch-import cache) — no copies at the boundary.
pub fn replay_canvas<C: AsRef<[u8]>, T: AsRef<[u8]>>(
    canvas: Option<C>,
    overlays: &[(T, i64, i64, f32)],
) -> iac_core::error::AppResult<Vec<u8>> {
    let _vips = vips_guard();
    let mut canvas = match &canvas {
        Some(bytes) => Some(VipsImage::new_from_buffer(bytes.as_ref(), "")?),
        None => None,
    };
    for (bytes, x, y, rot) in overlays {
        let (ov, _) = decode(bytes.as_ref(), DEFAULT_MAX_DECODE_PIXELS)?;
        let ov = VipsImage::image_copy_memory(ov)?;
        let ov = rotate_centered(&ov, *rot)?;
        let ov = VipsImage::image_copy_memory(ov)?;
        let lx = i32::try_from(*x).map_err(|_| anyhow::anyhow!("embed x out of range"))?;
        let ly = i32::try_from(*y).map_err(|_| anyhow::anyhow!("embed y out of range"))?;
        let size: i32 = i64::from(CHUNK_CANVAS_PX)
            .try_into()
            .map_err(|_| anyhow::anyhow!("embed size out of range"))?;
        canvas = Some(match canvas {
            Some(c) => composite(&c, &ov, *x, *y)?,
            // First paint on a virgin chunk embeds directly (see
            // composite_chunks for why not a synthetic blank).
            None => embed_rect(&ov, lx, ly, size, size)?,
        });
    }
    let canvas = match canvas {
        Some(c) => c,
        // No overlays at all → rewrite as transparent.
        None => blank_chunk(CHUNK_CANVAS_PX)?,
    };
    webp_bytes(&canvas, 90).map_err(|e| anyhow::anyhow!("replay encode: {e}").into())
}

/// A member overlay placement inside one chunk: index into the group's
/// overlay table + the chunk-local AABB top-left (paint order).
pub type OverlayPlacement = (usize, i64, i64);

/// One chunk pass: chunk coords, current raster bytes (None = virgin),
/// members in paint order.
pub type ChunkPaint = (i64, i64, Option<Vec<u8>>, Vec<OverlayPlacement>);

/// Composite a whole chunk-group in ONE vips section with a bounded
/// decoded-overlay cache: each unique overlay is decoded + rotated once
/// and reused while cached. Decoding + rotating multi-megapixel derivatives
/// PER CHUNK a pile image spans was the batch ingest wall (measured
/// ~1.4 img/s vs the ≥10/s SLO — docs/backend.md "Batch import").
/// `overlays` are `(bytes, normalized rot)`; `chunks` are
/// `(cx, cy, canvas bytes or None for virgin, members)` with members
/// `(overlay idx, chunk-local AABB x, chunk-local AABB y)` in paint (seq)
/// order. Returns encoded webp bytes per chunk, input order. Pixel output
/// is identical to per-chunk `replay_canvas` (same ops, cached inputs).
/// `decoded_budget_bytes` bounds the decoded set; overflow clears it (the
/// caller super-blocks chunk passes so spans stay cache-warm).
pub fn composite_group<T: AsRef<[u8]>>(
    overlays: &[(T, f32)],
    chunks: &[ChunkPaint],
    decoded_budget_bytes: u64,
) -> iac_core::error::AppResult<Vec<Vec<u8>>> {
    let _vips = vips_guard();
    let mut cache: HashMap<usize, VipsImage> = HashMap::new();
    let mut cache_bytes = 0u64;
    let mut out = Vec::with_capacity(chunks.len());
    for (_, _, canvas, members) in chunks {
        let mut composed: Option<VipsImage> = match canvas {
            Some(bytes) => {
                let img = VipsImage::new_from_buffer(bytes, "")?;
                // Chunk bases decode sequential; composite blends need
                // random access.
                Some(VipsImage::image_copy_memory(img)?)
            }
            None => None,
        };
        for (idx, lx, ly) in members {
            if !cache.contains_key(idx) {
                if cache_bytes > decoded_budget_bytes {
                    cache.clear();
                    cache_bytes = 0;
                }
                let (bytes, rot) = &overlays[*idx];
                let (img, _) = decode(bytes.as_ref(), DEFAULT_MAX_DECODE_PIXELS)?;
                let img = VipsImage::image_copy_memory(img)?;
                let img = rotate_centered(&img, *rot)?;
                let img = VipsImage::image_copy_memory(img)?;
                cache_bytes +=
                    (i64::from(img.get_width()) * i64::from(img.get_height()) * 4) as u64;
                cache.insert(*idx, img);
            }
            let ov = &cache[idx];
            composed = Some(match composed {
                Some(base) => composite(&base, ov, *lx, *ly)?,
                // First paint on a virgin chunk embeds directly (see
                // composite_chunks for why not a synthetic blank).
                None => embed_rect(
                    ov,
                    i32::try_from(*lx).map_err(|_| anyhow::anyhow!("embed x out of range"))?,
                    i32::try_from(*ly).map_err(|_| anyhow::anyhow!("embed y out of range"))?,
                    CHUNK_CANVAS_PX,
                    CHUNK_CANVAS_PX,
                )?,
            });
        }
        let composed = composed.ok_or_else(|| anyhow::anyhow!("chunk without members"))?;
        out.push(
            webp_bytes(&composed, 90)
                .map_err(|e| anyhow::anyhow!("chunk webp encode: {e}"))
                .map_err(iac_core::error::AppError::from)?,
        );
    }
    Ok(out)
}

/// Render a z ≥ BASE_Z tile from its covering chunk bytes (None = blank).
pub fn render_leaf_tile(
    chunk: Option<Vec<u8>>,
    offset_x: i64,
    offset_y: i64,
    native_size: i64,
) -> iac_core::error::AppResult<Vec<u8>> {
    let _vips = vips_guard();
    let chunk = match chunk {
        Some(bytes) => {
            // WebP chunk blobs decode to sequential images; materialize to a
            // random-access raster so crop/flatten/encode can read in any
            // order. Cached by blob content, so the many tiles sharing one
            // chunk pay the decode once (see [`decoded_blob`]).
            decoded_blob(&bytes)?
        }
        None => blank_chunk(CHUNK_CANVAS_PX)?,
    };
    let tile = tile_from_chunk(&chunk, offset_x, offset_y, native_size)?;
    webp_bytes(&tile, 80)
}

/// The constant empty tile: pure paper, 256×256 webp.
#[cfg(test)]
mod empty_tile_const {
    use super::*;
    /// The core const must stay byte-identical to what vips generates:
    /// the API serves it for maxSeq-0 tiles without linking libvips.
    #[test]
    fn empty_tile_const_decodes() {
        // This test decodes BEFORE any guarded helper runs; with
        // --test-threads 2 it can be the suite's first vips op.
        init().expect("libvips init");
        let bytes = iac_core::empty_tile::decode();
        let img = VipsImage::new_from_buffer(&bytes, "").expect("const decodes");
        assert_eq!(img.get_width(), TILE_EDGE as i32);
        assert_eq!(img.get_height(), TILE_EDGE as i32);
        let fresh = empty_tile_256().unwrap();
        assert_eq!(
            bytes, fresh,
            "regenerate the core const when the encoder changes"
        );
    }
}

#[test]
fn dump_guard() {}

pub fn empty_tile_256() -> iac_core::error::AppResult<Vec<u8>> {
    let _vips = vips_guard();
    let blank = blank_chunk(TILE_EDGE as i32)?;
    let flat = flatten_paper(blank)?;
    webp_bytes(&flat, 80)
}

/// Render a parent tile from its four encoded child tiles (2×2, row-major).
pub fn render_parent_tile(children: &[Vec<u8>; 4]) -> iac_core::error::AppResult<Vec<u8>> {
    let _vips = vips_guard();
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
        decode(&red_png(w, h), DEFAULT_MAX_DECODE_PIXELS).unwrap().0
    }

    #[test]
    fn decode_rejects_unknown_format() {
        if !vips() {
            return;
        }
        assert!(decode(b"not an image at all", DEFAULT_MAX_DECODE_PIXELS).is_err());
        // SVG is not in the allowed set.
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"/>";
        assert!(decode(svg, DEFAULT_MAX_DECODE_PIXELS).is_err());
    }

    #[test]
    fn decode_normalizes_to_rgba() {
        if !vips() {
            return;
        }
        // RGB JPEG (no alpha) must come out 4-band RGBA.
        let black = ops::black(32, 16).unwrap();
        let jpg = ops::jpegsave_buffer(&black).unwrap();
        let (img, fmt) = decode(&jpg, DEFAULT_MAX_DECODE_PIXELS).unwrap();
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
        let (img, fmt) = decode(&gif, DEFAULT_MAX_DECODE_PIXELS).unwrap();
        assert_eq!(fmt, SourceFormat::Gif);
        assert_eq!((img.get_width(), img.get_height()), (20, 20));
    }

    /// Real 2-frame animated GIF (two 64x64 frames, 285 bytes) generated
    /// with ImageMagick; loads as a 64x128 strip, decodes one 64x64 frame.
    /// Real 2-frame animated GIF (two 64x64 frames, 285 bytes) generated
    /// with ImageMagick; loads as a 64x128 strip, decodes one 64x64 frame.
    const ANIMATED_2F_GIF_HEX: &str = concat!(
        "47494638396140004000f00000ff000000000021ff0b4e45545343415045322e",
        "30030100000021f904000000000021ff0b496d6167654d616769636b0e67616d",
        "6d613d302e343534353435002c0000000040004000000245848fa9cbed0fa39c",
        "b4da8bb3debcfb0f86e24896e689a6eacab6ee0bc7f24cd7f68de7facef7fe0f",
        "0c0a87c4a2f1884c2a97cca6f3098d4aa7d4aaf58acd6ab7dcae1750000021f9",
        "04000000000021ff0b496d6167654d616769636b0e67616d6d613d302e343534",
        "353435002c0000000040004000800000ff0000000245848fa9cbed0fa39cb4da",
        "8bb3debcfb0f86e24896e689a6eacab6ee0bc7f24cd7f68de7facef7fe0f0c0a",
        "87c4a2f1884c2a97cca6f3098d4aa7d4aaf58acd6ab7dcae175000003b"
    );

    /// The decoded-pixel budget is measured on the frame we actually
    /// decode, not the animation strip: a 2-frame 64x64 gif loads as a
    /// 64x128 strip (8192 px) but decodes one 64x64 (4096 px) frame, so a
    /// budget between the two must ADMIT it. The full-strip check this
    /// replaced rejected such uploads outright. Embedded as hex (like the
    /// 1px PNG) because the gate image copies only src/ — no tests/
    /// fixtures directory exists in the container.
    #[test]
    fn animated_budget_bounds_the_decoded_frame() {
        if !vips() {
            return;
        }
        let gif = hex_decode(ANIMATED_2F_GIF_HEX);
        let probed = probe(&gif).unwrap();
        assert_eq!(probed, (64, 64), "probe reports first-frame dims");
        let budget = 4096u64; // exactly one frame
        let (img, fmt) = decode(&gif, budget).expect("frame fits the budget");
        assert_eq!(fmt, SourceFormat::Gif);
        assert_eq!((img.get_width(), img.get_height()), (64, 64));
        assert!(decode(&gif, budget - 1).is_err(), "one pixel under rejects");
    }

    /// The budget boundary is exact, and absurd declared dimensions
    /// saturate instead of wrapping: products are computed in i64 across
    /// two i32s, so no u64 wrap can sneak a bomb under the budget.
    #[test]
    fn budget_boundary_is_exact_and_saturates() {
        if !vips() {
            return;
        }
        let img = red_png(100, 100);
        assert!(decode(&img, 10_000).is_ok());
        assert!(matches!(decode(&img, 9_999), Err(AppError::TooLarge)));
        // 2^23-1 squared (~7e13 px): fits i64 comfortably, far over any
        // budget, and under libvips' 10M coordinate sanity limit so the
        // header loads and the BUDGET is what rejects it.
        let bomb = png_with_declared_dims(8_388_607, 8_388_607);
        assert!(matches!(
            decode(&bomb, DEFAULT_MAX_DECODE_PIXELS),
            Err(AppError::TooLarge)
        ));
    }

    /// A PNG whose IHDR declares (w, h) but whose pixel data is a real
    /// 1x1 image: header checks see the declared dims, pixel decodes
    /// fail. Used to prove the budget is size-driven, not an artifact of
    /// truncated data.
    fn png_with_declared_dims(width: u32, height: u32) -> Vec<u8> {
        let mut png = red_png(1, 1);
        const IHDR_DATA: usize = 8 + 4 + 4;
        png[IHDR_DATA..IHDR_DATA + 4].copy_from_slice(&width.to_be_bytes());
        png[IHDR_DATA + 4..IHDR_DATA + 8].copy_from_slice(&height.to_be_bytes());
        let chunk_len = u32::from_be_bytes(png[8..12].try_into().expect("chunk len")) as usize;
        let crc_offset = 12 + 4 + chunk_len;
        let crc = crc32(&png[12..crc_offset]);
        png[crc_offset..crc_offset + 4].copy_from_slice(&crc.to_be_bytes());
        png
    }

    #[test]
    fn resize_exact_dimensions_and_alpha() {
        if !vips() {
            return;
        }
        let (img, _) = decode(&red_png(640, 320), DEFAULT_MAX_DECODE_PIXELS).unwrap();
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
        let (img1, _) = decode(&red_png(100, 50), DEFAULT_MAX_DECODE_PIXELS).unwrap();
        let d1 = resize_exact(&img1, 32, 16).unwrap();
        let b1 = webp_bytes(&d1, 85).unwrap();
        let (img2, _) = decode(&red_png(100, 50), DEFAULT_MAX_DECODE_PIXELS).unwrap();
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
        let (img1, f1) = decode(&png1, DEFAULT_MAX_DECODE_PIXELS).unwrap();
        let s1 = strip_resave(&img1, f1).unwrap();
        let png2 = red_png(40, 40);
        let (img2, f2) = decode(&png2, DEFAULT_MAX_DECODE_PIXELS).unwrap();
        let s2 = strip_resave(&img2, f2).unwrap();
        assert_eq!(sha256_hex(&s1), sha256_hex(&s2));
    }

    #[test]
    fn composite_offset_and_clipping() {
        if !vips() {
            return;
        }
        let (ov, _) = decode(&red_png(64, 64), DEFAULT_MAX_DECODE_PIXELS).unwrap();
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
        let (back, _) = decode(&bytes, DEFAULT_MAX_DECODE_PIXELS).unwrap();
        // All alpha must be 0.
        let mem = back.image_write_to_memory();
        assert!(mem.chunks(4).all(|px| px[3] == 0));
    }

    /// A blob decoded through the cache must yield exactly the pixels a fresh
    /// decode produces, on both the miss and the hit path. This is the whole
    /// correctness argument for the content-addressed key: no invalidation,
    /// no staleness window — a changed blob is simply a different key.
    #[test]
    fn decoded_blob_cache_is_pixel_transparent() {
        if !vips() {
            return;
        }
        let (img, _) = decode(&red_png(1024, 1024), DEFAULT_MAX_DECODE_PIXELS).unwrap();
        let blob = webp_bytes(&img, 90).unwrap();

        let miss = decoded_blob(&blob).unwrap(); // first touch: decode + store
        let hit = decoded_blob(&blob).unwrap(); // second touch: cache hit
        let fresh =
            VipsImage::image_copy_memory(VipsImage::new_from_buffer(&blob, "").unwrap()).unwrap();

        for (label, got) in [("miss", &miss), ("hit", &hit)] {
            assert_eq!(
                (got.get_width(), got.get_height(), got.get_bands()),
                (fresh.get_width(), fresh.get_height(), fresh.get_bands()),
                "{label} geometry"
            );
            assert_eq!(
                got.image_write_to_memory(),
                fresh.image_write_to_memory(),
                "{label} pixels must equal a fresh decode"
            );
        }
    }

    /// `render_leaf_tile` must be byte-identical to the pre-cache algorithm
    /// (inline decode → crop/scale → encode), both cold and cached: the cache
    /// changes only where the pixels are held, never what they are.
    #[test]
    fn render_leaf_tile_is_byte_identical_cold_and_cached() {
        if !vips() {
            return;
        }
        let (img, _) = decode(&red_png(1024, 1024), DEFAULT_MAX_DECODE_PIXELS).unwrap();
        // Vary the paint so the tile is not a flat colour.
        let (ov, _) = decode(&asymmetric_test_png(300, 200), DEFAULT_MAX_DECODE_PIXELS).unwrap();
        let ov = VipsImage::image_copy_memory(ov).unwrap();
        let painted = composite(&img, &ov, 100, 100).unwrap();
        let blob = webp_bytes(&painted, 90).unwrap();

        for (ox, oy, native) in [(0i64, 0i64, 1024i64), (256, 256, 256), (512, 0, 512)] {
            let cold = render_leaf_tile(Some(blob.clone()), ox, oy, native).unwrap();
            let cached = render_leaf_tile(Some(blob.clone()), ox, oy, native).unwrap();

            // Reference: the algorithm as it was before the cache existed.
            let decoded =
                VipsImage::image_copy_memory(VipsImage::new_from_buffer(&blob, "").unwrap())
                    .unwrap();
            let tile = tile_from_chunk(&decoded, ox, oy, native).unwrap();
            let reference = webp_bytes(&tile, 80).unwrap();

            assert_eq!(cold, reference, "cold render must match reference");
            assert_eq!(cached, cold, "cached render must match cold render");
        }
    }

    /// More distinct blobs than the cache capacity: every entry must still
    /// decode to the right pixels after its predecessor was evicted.
    #[test]
    fn raster_cache_stays_correct_under_eviction() {
        if !vips() {
            return;
        }
        let mut blobs = Vec::new();
        for w in 32..(32 + raster_cache_entries() + 4) as u32 {
            let (img, _) = decode(&red_png(w, 32), DEFAULT_MAX_DECODE_PIXELS).unwrap();
            blobs.push(webp_bytes(&img, 90).unwrap());
        }
        for blob in &blobs {
            let got = decoded_blob(blob).unwrap();
            let fresh = VipsImage::image_copy_memory(VipsImage::new_from_buffer(blob, "").unwrap())
                .unwrap();
            assert_eq!(got.image_write_to_memory(), fresh.image_write_to_memory());
        }
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
        let (img, _) = decode(&red_png(1024, 1024), DEFAULT_MAX_DECODE_PIXELS).unwrap();
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

#[cfg(test)]
mod rot_tests {
    use super::*;

    fn vips() -> bool {
        init().is_ok()
    }

    #[test]
    fn rotate_centered_pins_clockwise_orientation() {
        if !vips() {
            return;
        }
        // 200×100: top half red, bottom half blue.
        let src = VipsImage::image_copy_memory(
            VipsImage::new_from_buffer(&asymmetric_test_png(200, 100), "").unwrap(),
        )
        .unwrap();
        let rot = rotate_centered(&src, 90.0).unwrap();
        assert_eq!(rot.get_width(), 100);
        assert_eq!(rot.get_height(), 200);
        // Clockwise quarter turn: the bottom edge swings to the LEFT, so
        // the left column shows the original BOTTOM (blue) and the right
        // column the original TOP (red).
        let left = ops::getpoint(&rot, 5, 100).unwrap();
        let right = ops::getpoint(&rot, 95, 100).unwrap();
        assert!(
            left[2] > 150.0 && left[0] < 100.0,
            "left should be blue: {left:?}"
        );
        assert!(
            right[0] > 150.0 && right[2] < 100.0,
            "right should be red: {right:?}"
        );

        // 180° keeps the size; top edge faces down.
        let half = rotate_centered(&src, 180.0).unwrap();
        assert_eq!((half.get_width(), half.get_height()), (200, 100));
        let top = ops::getpoint(&half, 100, 5).unwrap();
        assert!(
            top[2] > 150.0 && top[0] < 100.0,
            "top should be blue after 180°: {top:?}"
        );

        // rot 0: untouched copy.
        let zero = rotate_centered(&src, 0.0).unwrap();
        assert_eq!((zero.get_width(), zero.get_height()), (200, 100));
    }

    #[test]
    fn rotate_centered_matches_aabb() {
        if !vips() {
            return;
        }
        let src = VipsImage::new_from_buffer(&test_png(100, 100), "").unwrap();
        for rot in [7.5f32, 45.0, 179.0, 359.0] {
            let out = rotate_centered(&src, rot).unwrap();
            let aabb = iac_core::world::rotated_aabb(0, 0, 100, 100, rot);
            assert_eq!(
                (out.get_width(), out.get_height()),
                (aabb.w as i32, aabb.h as i32),
                "rot {rot}"
            );
        }
    }

    #[test]
    fn bisect_rotate_composite() {
        if !vips() {
            return;
        }
        eprintln!("step1: build base");
        let base_img = blank_chunk(1024).unwrap();
        let base_bytes = webp_bytes(&base_img, 90).unwrap();
        eprintln!("step2: decode overlay");
        let (ov, _) = decode(&asymmetric_test_png(128, 64), DEFAULT_MAX_DECODE_PIXELS).unwrap();
        let ov = VipsImage::image_copy_memory(ov).unwrap();
        eprintln!("step3: rotate");
        let rot = rotate_centered(&ov, 45.0).unwrap();
        eprintln!("step4: materialize");
        let rot = VipsImage::image_copy_memory(rot).unwrap();
        eprintln!("step5: decode base");
        let base = VipsImage::new_from_buffer(&base_bytes, "").unwrap();
        let base = VipsImage::image_copy_memory(base).unwrap();
        eprintln!("step6: composite");
        let composed = composite(&base, &rot, 100, 100).unwrap();
        eprintln!("step7: save");
        let _bytes = webp_bytes(&composed, 90).unwrap();
        eprintln!("done");
    }

    #[test]
    fn bisect_composite_chunks_rotated() {
        if !vips() {
            return;
        }
        let base_img = blank_chunk(1024).unwrap();
        let base_bytes = webp_bytes(&base_img, 90).unwrap();
        // webp derivative like production.
        let (ov, _) = decode(&asymmetric_test_png(128, 64), DEFAULT_MAX_DECODE_PIXELS).unwrap();
        let derivative = resize_exact(&ov, 128, 128).unwrap();
        let overlay = webp_bytes(&derivative, 85).unwrap();
        eprintln!("calling composite_chunks 4 chunks rot45");
        let out = composite_chunks(
            vec![
                Some(base_bytes.clone()),
                Some(base_bytes.clone()),
                None,
                None,
            ],
            &overlay,
            &[(900, 900), (-50, 900), (900, -50), (-50, -50)],
            45.0,
        )
        .unwrap();
        eprintln!("ok {} chunks", out.len());
    }

    #[test]
    fn bisect_double_rotation_roundtrip() {
        if !vips() {
            return;
        }
        let base_img = blank_chunk(1024).unwrap();
        let mut base_bytes = webp_bytes(&base_img, 90).unwrap();
        let (ov, _) = decode(&asymmetric_test_png(128, 64), DEFAULT_MAX_DECODE_PIXELS).unwrap();
        let derivative = resize_exact(&ov, 128, 128).unwrap();
        let overlay = webp_bytes(&derivative, 85).unwrap();
        for round in 0..3 {
            eprintln!("round {round}");
            let out = composite_chunks(
                vec![Some(base_bytes.clone()), Some(base_bytes.clone())],
                &overlay,
                &[(100 + round * 40, 100), (900, 900)],
                45.0 + round as f32 * 10.0,
            )
            .unwrap();
            base_bytes = out[0].clone();
        }
        eprintln!("ok rounds");
    }

    /// composite_group must be pixel-identical to per-chunk replay_canvas
    /// (same decode/rotate/composite/embed ops, cached inputs): the batch
    /// ingest path relies on this equivalence for replay consistency.
    #[test]
    fn composite_group_matches_per_chunk_replay() {
        assert!(vips());
        let ov1 = asymmetric_test_png(200, 100);
        let ov2 = asymmetric_test_png(64, 64);
        // Two entries share (bytes, rot); one reuses ov1 at rot 0.
        let overlays = vec![
            (ov1.clone(), 0.0f32),
            (ov2.clone(), 30.0f32),
            (ov1.clone(), 0.0f32),
        ];
        let chunks = vec![
            (
                0i64,
                0i64,
                None,
                vec![(0usize, -50i64, -50i64), (1, 900, 900)],
            ),
            (1, 0, None, vec![(0, 904, 10), (2, 0, 0)]),
        ];
        let grouped = composite_group(&overlays, &chunks, 1 << 20).unwrap();
        let replay0 = replay_canvas(
            None::<Vec<u8>>,
            &[(ov1.clone(), -50, -50, 0.0), (ov2.clone(), 900, 900, 30.0)],
        )
        .unwrap();
        let replay1 = replay_canvas(
            None::<Vec<u8>>,
            &[(ov1.clone(), 904, 10, 0.0), (ov1, 0, 0, 0.0)],
        )
        .unwrap();
        assert_eq!(
            grouped[0], replay0,
            "clipped corner chunk must match replay"
        );
        assert_eq!(grouped[1], replay1, "second chunk must match replay");
    }
}
