//! Zstandard (RFC 8878) frames: bounded decompression and compression.
//!
//! ZArchive, the only caller, compresses fixed-size blocks independently, so whole-frame
//! functions fit; a streaming wrapper can be added when a caller needs one.
//! The frame content checksum is not verified (ruzstd's `hash` feature is off); ZArchive
//! carries its own SHA-256 digests.

use crate::{Error, Result, limit_error};
use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use std::io::Read;

/// Window every decoder should accept (RFC 8878 §3.1.1.1.2 recommends 8 MiB).
const MIN_WINDOW: usize = 8 << 20;

/// Decompresses one zstd frame of at most `limit` output bytes.
pub fn decompress(frame: &[u8], limit: usize) -> Result<Vec<u8>> {
    let window = u64::try_from(limit.max(MIN_WINDOW)).unwrap_or(u64::MAX);
    let mut decoder = StreamingDecoder::new_with_max_window_size(frame, window)
        .map_err(|_| Error::Malformed("zstd frame header"))?;
    let mut out = Vec::new();
    // Read one byte past the bound so an oversized frame is detected, not silently cut.
    let probe = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    (&mut decoder)
        .take(probe)
        .read_to_end(&mut out)
        .map_err(|_| Error::Malformed("zstd frame"))?;
    if out.len() > limit {
        return Err(limit_error("zstd output", out.len(), limit));
    }
    if !decoder.decoder.is_finished() {
        return Err(Error::Malformed("truncated zstd frame"));
    }
    Ok(out)
}

/// Compresses `input` into one zstd frame (ruzstd's fastest level, as pkg-tools uses).
pub fn compress(input: &[u8]) -> Vec<u8> {
    compress_to_vec(input, CompressionLevel::Fastest)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::test_util::hex;
    use pantheon_core::testing::Corruptor;

    // Produced by the reference zstd CLI v1.5.7 (`zstd -19`, with checksum) from
    // "Pantheon zstd vector. " repeated 20 times: one compressed block with a repeat match.
    const KNOWN: &str =
        "28b52ffd0468ed0000b050616e7468656f6e207a73746420766563746f722e2001003e539d4d824070ba";

    fn known_plain() -> Vec<u8> {
        b"Pantheon zstd vector. ".repeat(20)
    }

    fn sample() -> Vec<u8> {
        (0..100_000u32)
            .map(|i| (i % 251) as u8 ^ (i / 1000) as u8)
            .collect()
    }

    #[test]
    fn known_frame() {
        let plain = known_plain();
        assert_eq!(decompress(&hex(KNOWN), plain.len()).unwrap(), plain);
        assert!(matches!(
            decompress(&hex(KNOWN), plain.len() - 1),
            Err(Error::Limit(_))
        ));
    }

    #[test]
    fn roundtrip() {
        let data = sample();
        assert_eq!(decompress(&compress(&data), data.len()).unwrap(), data);
        assert_eq!(decompress(&compress(&[]), 0).unwrap(), b"");
    }

    #[test]
    fn corrupted_frames_err_and_stay_bounded() {
        let data = sample();
        let packed = compress(&data);
        let limit = data.len();
        for prefix in Corruptor::truncations(&packed).take(packed.len()) {
            assert!(
                decompress(prefix, limit).is_err(),
                "prefix {}",
                prefix.len()
            );
        }
        let known = hex(KNOWN);
        for input in [&packed, &known] {
            Corruptor::new(0x25d).run(input, 512, |input| {
                if let Ok(out) = decompress(input, limit) {
                    assert!(out.len() <= limit);
                }
            });
        }
        let bomb = compress(&vec![0; 1 << 20]);
        assert!(matches!(decompress(&bomb, 4096), Err(Error::Limit(_))));
    }
}
