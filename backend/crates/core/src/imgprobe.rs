//! Pure-Rust image header reads for the API's cheap pre-flight (the
//! decode budget and the placement rect). The API never links libvips
//! (docs/scaling-architecture.md § Code and images): this module parses
//! container headers only — dimensions without touching pixel data —
//! and libvips remains authoritative in the worker's pixel stage. A
//! stream that parses here but fails there fails the ingest job with a
//! cooldown refund, exactly as the upload flow's step 3 describes.
//!
//! Supported: PNG, JPEG, WebP, GIF, AVIF (`img::SourceFormat`'s set).

use crate::error::AppError;

/// (width, height) of the first frame, or `undecodable image` when the
/// header cannot be parsed. Oversized headers are bounded by the input
/// slice, never allocated.
pub fn dimensions(bytes: &[u8]) -> Result<(i64, i64), AppError> {
    if bytes.len() < 12 {
        return Err(AppError::BadRequest("undecodable image"));
    }
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => png(bytes),
        [0xFF, 0xD8, ..] => jpeg(bytes),
        [b'R', b'I', b'F', b'F', .., b'W', b'E', b'B', b'P'] => webp(bytes),
        [b'G', b'I', b'F', b'8', ..] => gif(bytes),
        _ if &bytes[4..8] == b"ftyp" => avif(bytes),
        _ => Err(AppError::BadRequest("undecodable image")),
    }
}

fn u16be(b: &[u8], at: usize) -> i64 {
    i64::from(u16::from_be_bytes([b[at], b[at + 1]]))
}

fn png(b: &[u8]) -> Result<(i64, i64), AppError> {
    // Signature (8) + IHDR length (4) + type (4), then width/height.
    if b.len() < 24 || &b[12..16] != b"IHDR" {
        return Err(AppError::BadRequest("undecodable image"));
    }
    let w = i64::from(u32::from_be_bytes([b[16], b[17], b[18], b[19]]));
    let h = i64::from(u32::from_be_bytes([b[20], b[21], b[22], b[23]]));
    ok_nonzero(w, h)
}

fn jpeg(b: &[u8]) -> Result<(i64, i64), AppError> {
    // Walk markers until an SOFn carries dimensions.
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return Err(AppError::BadRequest("undecodable image"));
        }
        let marker = b[i + 1];
        if (0xC0..=0xCF).contains(&marker)
            && ![0xC4, 0xC8, 0xCC].contains(&marker)
        {
            if i + 9 > b.len() {
                return Err(AppError::BadRequest("undecodable image"));
            }
            let h = u16be(b, i + 5);
            let w = u16be(b, i + 7);
            return ok_nonzero(w, h);
        }
        let skip = match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => 0,
            _ => {
                if i + 4 > b.len() {
                    return Err(AppError::BadRequest("undecodable image"));
                }
                usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]))
            }
        };
        i += 2 + skip;
    }
    Err(AppError::BadRequest("undecodable image"))
}

fn webp(b: &[u8]) -> Result<(i64, i64), AppError> {
    if b.len() < 30 {
        return Err(AppError::BadRequest("undecodable image"));
    }
    match &b[12..16] {
        b"VP8 " => {
            // Lossy keyframe: dims at a tagged offset inside the frame.
            let frame = 20usize;
            if b.len() < frame + 10 || &b[frame + 3..frame + 6] != b"\x9d\x01\x2a" {
                return Err(AppError::BadRequest("undecodable image"));
            }
            let w = i64::from(u16::from_le_bytes([
                b[frame + 6],
                b[frame + 7],
            ])) & 0x3FFF;
            let h = i64::from(u16::from_le_bytes([
                b[frame + 8],
                b[frame + 9],
            ])) & 0x3FFF;
            ok_nonzero(w, h)
        }
        b"VP8L" => {
            const F: usize = 21;
            if b.len() < F + 4 {
                return Err(AppError::BadRequest("undecodable image"));
            }
            let bits = u32::from_le_bytes([b[F], b[F + 1], b[F + 2], b[F + 3]]);
            let w = i64::from(bits & 0x3FFF) + 1;
            let h = i64::from((bits >> 14) & 0x3FFF) + 1;
            ok_nonzero(w, h)
        }
        b"VP8X" => {
            // Extended: 24-bit dims minus one, in a canvas-size box.
            if b.len() < 30 {
                return Err(AppError::BadRequest("undecodable image"));
            }
            let w = (i64::from(b[24])
                | i64::from(b[25]) << 8
                | i64::from(b[26]) << 16)
                + 1;
            let h = (i64::from(b[27])
                | i64::from(b[28]) << 8
                | i64::from(b[29]) << 16)
                + 1;
            ok_nonzero(w, h)
        }
        _ => Err(AppError::BadRequest("undecodable image")),
    }
}

fn gif(b: &[u8]) -> Result<(i64, i64), AppError> {
    if b.len() < 10 {
        return Err(AppError::BadRequest("undecodable image"));
    }
    let w = u16le(b, 6);
    let h = u16le(b, 8);
    ok_nonzero(w, h)
}

fn u16le(b: &[u8], at: usize) -> i64 {
    i64::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn avif(b: &[u8]) -> Result<(i64, i64), AppError> {
    // ISOBMFF: walk top-level boxes; the primary item's ispe carries
    // the dimensions (meta > iprp > ipco > ispe).
    fn walk(b: &[u8], from: usize, to: usize, depth: u8) -> Option<(i64, i64)> {
        if depth > 6 {
            return None;
        }
        let mut i = from;
        while i + 8 <= to && i + 8 <= b.len() {
            let size = u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
            let kind = &b[i + 4..i + 8];
            let (head, end) = if size == 1 {
                // 64-bit largesize
                if i + 16 > to {
                    return None;
                }
                let big = usize::try_from(u64::from_be_bytes([
                    b[i + 8],
                    b[i + 9],
                    b[i + 10],
                    b[i + 11],
                    b[i + 12],
                    b[i + 13],
                    b[i + 14],
                    b[i + 15],
                ]))
                .ok()?;
                (16, big.min(to))
            } else if size == 0 {
                (8, to)
            } else {
                (8, (i + size).min(to))
            };
            match kind {
                b"meta" | b"iprp" | b"ipco" => {
                    if let Some(d) = walk(b, i + head, end, depth + 1) {
                        return Some(d);
                    }
                }
                b"ispe" => {
                    if end >= i + head + 12 {
                        let at = i + head + 4;
                        let w = i64::from(u32::from_be_bytes([
                            b[at],
                            b[at + 1],
                            b[at + 2],
                            b[at + 3],
                        ]));
                        let h = i64::from(u32::from_be_bytes([
                            b[at + 4],
                            b[at + 5],
                            b[at + 6],
                            b[at + 7],
                        ]));
                        if w > 0 && h > 0 {
                            return Some((w, h));
                        }
                    }
                }
                _ => {}
            }
            if end <= i + head.min(1).max(0) || size == 0 && end == i {
                break;
            }
            i = end;
        }
        None
    }
    // Skip ftyp, then scan the remaining top-level boxes.
    let first = usize::try_from(u32::from_be_bytes([
        b[0], b[1], b[2], b[3],
    ]))
    .unwrap_or(0);
    walk(b, first.min(b.len()), b.len(), 0).ok_or(AppError::BadRequest("undecodable image"))
}

fn ok_nonzero(w: i64, h: i64) -> Result<(i64, i64), AppError> {
    if w > 0 && h > 0 {
        Ok((w, h))
    } else {
        Err(AppError::BadRequest("undecodable image"))
    }
}

#[cfg(test)]
mod tests {
    use super::dimensions;

    /// A minimal valid 3×2 PNG (magic + IHDR only is enough here).
    #[test]
    fn png_header() {
        let mut b = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13,
        ];
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&3u32.to_be_bytes());
        b.extend_from_slice(&2u32.to_be_bytes());
        assert_eq!(dimensions(&b).unwrap(), (3, 2));
    }

    #[test]
    fn garbage_rejected() {
        assert!(dimensions(b"not an image at all!!").is_err());
        assert!(dimensions(b"").is_err());
    }

    /// Construction-level checks for the VP8L and VP8X layouts.
    #[test]
    fn webp_headers() {
        let mut b = b"RIFF\x24\x00\x00\x00WEBPVP8L".to_vec();
        b.extend_from_slice(&[0x0A, 0x00, 0x00]); // chunk size
        b.extend_from_slice(&[0x00]); // flags
        // VP8L payload: signature byte then 14-bit w-1 / h-1.
        b.push(0x2F);
        let (w, h) = (5u16, 7u16);
        let bits: u32 = u32::from(w - 1) | (u32::from(h - 1) << 14);
        b.extend_from_slice(&bits.to_le_bytes());
        assert_eq!(dimensions(&b).unwrap(), (5, 7));

        let mut x = b"RIFF\x2A\x00\x00\x00WEBPVP8X".to_vec();
        x.extend_from_slice(&[0x0A, 0x00, 0x00]); // chunk size
        x.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // flags/reserved
        x.extend_from_slice(&[4u8, 0, 0]); // canvas w-1 (24-bit LE)
        x.extend_from_slice(&[9u8, 0, 0]); // canvas h-1
        assert_eq!(dimensions(&x).unwrap(), (5, 10));
    }
}
