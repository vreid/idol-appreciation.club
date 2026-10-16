//! The shared pixel engine (decode, strip, re-save, hash, derivative):
//! the one libvips entry point the request path (dark 201) and the
//! ingest consumer both use. Lives in the worker crate — the only crate
//! that links libvips (docs/scaling-architecture.md § Code and images).

use crate::img;
use iac_core::error::{internal, AppError, AppResult};

/// Result of the per-image pixel stage shared by POST /api/place and the
/// import preupload.
pub struct OriginalPipeline {
    /// Metadata-stripped original, re-saved in the source codec. The
    /// sha256 of these bytes is the content address.
    pub stripped: Vec<u8>,
    pub format: img::SourceFormat,
    /// Derivative dimensions (the placement rect for `Some(size)`).
    pub w: i64,
    pub h: i64,
    /// Deterministic webp derivative (`derivatives/{derivative_sha}.webp`).
    pub derivative: Vec<u8>,
    pub original_sha: String,
    pub derivative_sha: String,
}

/// The per-image pixel stage (libvips is !Send, so this runs in
/// spawn_blocking): decode, strip metadata, re-save in the source codec,
/// hash; then build the deterministic webp derivative from the stripped
/// raster. `size = Some(long_edge)` pins the derivative long edge
/// (bounds-checked); `None` keeps the source dimensions, capped to
/// `max_size` and `derivative_long_edge` (the import preupload passes its
/// canvas-resolution cap). The /api/place path does NOT go through here —
/// it keeps #95's inline ordering (cheap validation → header probe →
/// decoded-pixel budget → cooldown → pixel stage) — but this helper
/// applies the same decompression-bomb defense: header probe and budget
/// check before any pixel work, and every decode is bounded.
pub async fn process_original(
    image_bytes: Vec<u8>,
    size: Option<i64>,
    min_size: i64,
    max_size: i64,
    derivative_long_edge: i64,
    max_decode_pixels: u64,
) -> AppResult<OriginalPipeline> {
    if image_bytes.is_empty() {
        return Err(AppError::BadRequest("empty image"));
    }
    if let Some(size) = size {
        if !(min_size..=max_size).contains(&size) {
            return Err(AppError::BadRequest(
                "size must be within the configured bounds",
            ));
        }
    }
    // Cheap header probe before any pixel work: an oversized image is
    // rejected without spending the libvips stage (same policy as
    // /api/place).
    let (probe_w, probe_h) = img::probe(&image_bytes)?;
    let declared = i64::from(probe_w) * i64::from(probe_h);
    if declared > 0 && u64::try_from(declared).unwrap_or(u64::MAX) > max_decode_pixels {
        return Err(AppError::TooLarge);
    }
    let (stripped, format, (w, h), derivative_bytes) =
        iac_core::telemetry::blocking(move || -> AppResult<_> {
            let (src, format) = img::decode(&image_bytes, max_decode_pixels)?;
            let src_w = i64::from(src.get_width());
            let src_h = i64::from(src.get_height());
            let long = size
                .unwrap_or_else(|| src_w.max(src_h))
                .min(max_size)
                .min(derivative_long_edge);
            if long < min_size {
                return Err(AppError::BadRequest(
                    "image smaller than the configured minimum size",
                ));
            }
            let (w, h) = iac_core::world::derive_w_h(src_w, src_h, long);
            // Strip ALL metadata (EXIF/XMP/ICC) and re-save in the source
            // codec; sha256 of these bytes is the content address. No GPS or
            // EXIF data ever leaves the process.
            let stripped = img::strip_resave(&src, format)?;
            let (derivative_src, _) = img::decode(&stripped, max_decode_pixels)?;
            let dw = i32::try_from(w).map_err(|_| internal("width overflow"))?;
            let dh = i32::try_from(h).map_err(|_| internal("height overflow"))?;
            let derivative = img::resize_exact(&derivative_src, dw, dh)?;
            let derivative_bytes = img::webp_bytes(&derivative, 85)?;
            Ok((stripped, format, (w, h), derivative_bytes))
        })
        .await
        .map_err(internal)??;
    Ok(OriginalPipeline {
        original_sha: img::sha256_hex(&stripped),
        derivative_sha: img::sha256_hex(&derivative_bytes),
        stripped,
        format,
        w,
        h,
        derivative: derivative_bytes,
    })
}
