//! zlib (RFC 1950) inflate with a bounded output, and deflate.

use crate::{Error, Result, limit_error};
use miniz_oxide::inflate::stream::{InflateState, inflate as inflate_step};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

/// Output produced per inner inflate call.
const OUTPUT_CHUNK: usize = 16 * 1024;
/// zlib's default compression level.
const DEFAULT_LEVEL: u8 = 6;

/// Streaming zlib decompressor that refuses to produce more than `limit` bytes.
pub struct Inflater {
    state: Box<InflateState>,
    limit: usize,
    produced: usize,
    finished: bool,
}

/// Result of one [`Inflater::push`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    /// Input bytes consumed; bytes after the end of the stream are left unconsumed.
    pub consumed: usize,
    /// The end of the zlib stream (with a valid Adler-32) was reached.
    pub finished: bool,
}

impl Inflater {
    /// A decompressor for one zlib stream of at most `limit` output bytes.
    pub fn new(limit: usize) -> Inflater {
        Inflater {
            state: InflateState::new_boxed(DataFormat::Zlib),
            limit,
            produced: 0,
            finished: false,
        }
    }

    /// Decompresses as much of `input` as possible, appending the output to `out`.
    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<Step> {
        let mut consumed = 0usize;
        let mut chunk = vec![0u8; OUTPUT_CHUNK];
        while !self.finished {
            let rest = input.get(consumed..).unwrap_or_default();
            let step = inflate_step(&mut self.state, rest, &mut chunk, MZFlush::None);
            consumed = consumed.saturating_add(step.bytes_consumed);
            let produced = self.produced.saturating_add(step.bytes_written);
            if produced > self.limit {
                return Err(limit_error("inflated size", produced, self.limit));
            }
            self.produced = produced;
            out.extend_from_slice(chunk.get(..step.bytes_written).unwrap_or_default());
            match step.status {
                Ok(MZStatus::StreamEnd) => self.finished = true,
                Ok(MZStatus::Ok) if step.bytes_consumed == 0 && step.bytes_written == 0 => break,
                Ok(MZStatus::Ok) => {}
                Ok(MZStatus::NeedDict) => return Err(Error::Malformed("zlib preset dictionary")),
                // No progress possible: either we need more input or the stream is broken.
                Err(MZError::Buf) if consumed == input.len() => break,
                Err(_) => return Err(Error::Malformed("zlib stream")),
            }
        }
        Ok(Step {
            consumed,
            finished: self.finished,
        })
    }
}

/// Inflates one complete zlib stream of at most `limit` bytes.
pub fn inflate(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    if Inflater::new(limit).push(input, &mut out)?.finished {
        Ok(out)
    } else {
        Err(Error::Malformed("truncated zlib stream"))
    }
}

/// Compresses `input` into a zlib stream at the default level.
pub fn deflate(input: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(input, DEFAULT_LEVEL)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::test_util::hex;
    use pantheon_core::testing::Corruptor;

    // zlib stream of "hello world" produced by Python `zlib.compress(b"hello world")`
    // (reference zlib 1.x, level 6); a public, well-known byte string.
    const HELLO: &str = "789ccb48cdc9c95728cf2fca4901001a0b045d";

    fn sample() -> Vec<u8> {
        (0..100_000u32)
            .map(|i| (i % 251) as u8 ^ (i / 1000) as u8)
            .collect()
    }

    #[test]
    fn known_stream() {
        assert_eq!(inflate(&hex(HELLO), 11).unwrap(), b"hello world");
        assert!(matches!(inflate(&hex(HELLO), 10), Err(Error::Limit(_))));
    }

    #[test]
    fn roundtrip_and_streaming() {
        let data = sample();
        let packed = deflate(&data);
        assert_eq!(inflate(&packed, data.len()).unwrap(), data);
        let mut inflater = Inflater::new(data.len());
        let mut out = Vec::new();
        let mut finished = false;
        for piece in packed.chunks(7) {
            let step = inflater.push(piece, &mut out).unwrap();
            assert_eq!(step.consumed, piece.len());
            finished = step.finished;
        }
        assert!(finished);
        assert_eq!(out, data);
    }

    #[test]
    fn trailing_bytes_are_not_consumed() {
        let mut input = hex(HELLO);
        input.extend_from_slice(b"tail");
        let mut out = Vec::new();
        let step = Inflater::new(64).push(&input, &mut out).unwrap();
        assert_eq!(
            step,
            Step {
                consumed: input.len() - 4,
                finished: true
            }
        );
    }

    #[test]
    fn corrupted_streams_err_and_stay_bounded() {
        let data = sample();
        let packed = deflate(&data);
        let limit = data.len();
        for prefix in Corruptor::truncations(&packed).take(packed.len()) {
            assert!(inflate(prefix, limit).is_err());
        }
        Corruptor::new(0x2b1).run(&packed, 512, |input| {
            if let Ok(out) = inflate(input, limit) {
                assert!(out.len() <= limit);
            }
        });
        // A small bomb: 1 MiB of zeros against a 4 KiB bound.
        let bomb = deflate(&vec![0; 1 << 20]);
        assert!(matches!(inflate(&bomb, 4096), Err(Error::Limit(_))));
    }
}
