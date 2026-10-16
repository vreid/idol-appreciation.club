//! The constant empty (pure paper) 256×256 tile, WebP Q80 — the exact
//! bytes `img::empty_tile_256` produced at the pinned libvips (worker
//! crate test `empty_tile_const_decodes` guards that it still decodes to
//! 256×256). Living in core lets the API serve maxSeq-0 tiles without
//! linking libvips; cold pyramid renders stay O(spine).

/// 394 bytes, base64 of the RIFF payload above.
pub static EMPTY_TILE_WEBP_B64: &str = "UklGRoIBAABXRUJQVlA4WAoAAAAIAAAA/wAA/wAAVlA4IKIAAADwEACdASoAAQABPm02mkmkIyKhICgAgA2JaW7hdrEbQArN2vFych77ZOQ99snIe+2TkPfbJyHvtk5D32ych77ZOQ99snIe+2TkPfbJyHvtk5D32ych77ZOQ99snIe+2TkPfbJyHvtk5D32ych77ZOQ99snIe+2TkPfbJyHvtk5D32ych77ZOQ99snIe+2Tj+AA/v/F4X03ph5AAAAAAAAAAABFWElGugAAAEV4aWYAAElJKgAIAAAABgASAQMAAQAAAAEAAAAaAQUAAQAAAFYAAAAbAQUAAQAAAF4AAAAoAQMAAQAAAAIAAAATAgMAAQAAAAEAAABphwQAAQAAAGYAAAAAAAAASAAAAAEAAABIAAAAAQAAAAYAAJAHAAQAAAAwMjEwAZEHAAQAAAABAgMAAKAHAAQAAAAwMTAwAaADAAEAAAD//wAAAqAEAAEAAAAAAQAAA6AEAAEAAAAAAQAAAAAAAA==";

/// Decoded once per process by the caller.
pub const EMPTY_TILE_WEBP_LEN: usize = 394;

use std::sync::OnceLock;

static DECODED: OnceLock<Vec<u8>> = OnceLock::new();

/// The tile bytes, base64-decoded once per process.
pub fn decode() -> Vec<u8> {
    DECODED
        .get_or_init(|| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(EMPTY_TILE_WEBP_B64)
                .expect("embedded empty tile is valid base64")
        })
        .clone()
}
