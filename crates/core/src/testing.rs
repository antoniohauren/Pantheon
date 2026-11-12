//! Test helpers (feature `testing`): a seeded PRNG, the `Corruptor` mutation
//! harness used by every format's robustness tests (D20), and a recording sink.

use std::sync::Mutex;

use crate::{Phase, Progress, ProgressSink};

/// SplitMix64: tiny, seeded, reproducible. Not for cryptography.
#[derive(Clone, Debug)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> SplitMix64 {
        SplitMix64(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-enough value in `0..bound` (`0` when `bound == 0`).
    pub fn below(&mut self, bound: usize) -> usize {
        let bound = u64::try_from(bound).unwrap_or(u64::MAX);
        self.next_u64()
            .checked_rem(bound)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0)
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

/// Length values that tend to break parsers when written over a length field.
const HOSTILE_LENGTHS: [u32; 6] = [0, 1, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFE, 0xFFFF_FFFF];

/// Produces corrupted variants of a valid input: truncation, bit flips and
/// length inflation. Seeded, so a failure is reproducible from its seed.
#[derive(Clone, Debug)]
pub struct Corruptor {
    rng: SplitMix64,
}

impl Corruptor {
    pub fn new(seed: u64) -> Corruptor {
        Corruptor {
            rng: SplitMix64::new(seed),
        }
    }

    /// Every prefix of `input`, shortest first (truncation at every boundary).
    pub fn truncations(input: &[u8]) -> impl Iterator<Item = &[u8]> {
        (0..input.len()).filter_map(|len| input.get(..len))
    }

    /// `input` with one random bit flipped.
    pub fn flip_bit(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = input.to_vec();
        let at = self.rng.below(out.len());
        let bit = self.rng.below(8);
        if let Some(byte) = out.get_mut(at) {
            *byte ^= 1u8 << bit;
        }
        out
    }

    /// `input` with a hostile 32-bit value written at a random 4-aligned offset,
    /// in a random byte order (length/count inflation).
    pub fn inflate_length(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = input.to_vec();
        let slots = out.len() / 4;
        if slots == 0 {
            return out;
        }
        let at = self.rng.below(slots).saturating_mul(4);
        let value = HOSTILE_LENGTHS
            .get(self.rng.below(HOSTILE_LENGTHS.len()))
            .copied()
            .unwrap_or(u32::MAX);
        let bytes = if self.rng.next_u64() & 1 == 0 {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        if let Some(slot) = out.get_mut(at..at.saturating_add(4)) {
            slot.copy_from_slice(&bytes);
        }
        out
    }

    /// One random mutation of any kind.
    pub fn mutate(&mut self, input: &[u8]) -> Vec<u8> {
        match self.rng.below(3) {
            0 => {
                let len = self.rng.below(input.len());
                input.get(..len).unwrap_or_default().to_vec()
            }
            1 => self.flip_bit(input),
            _ => self.inflate_length(input),
        }
    }

    /// The harness: calls `check` on every truncation of `input`, then on
    /// `rounds` random mutations. `check` must not panic for any of them.
    pub fn run(&mut self, input: &[u8], rounds: usize, mut check: impl FnMut(&[u8])) {
        for prefix in Corruptor::truncations(input) {
            check(prefix);
        }
        for _ in 0..rounds {
            let mutated = self.mutate(input);
            check(&mutated);
        }
    }
}

/// One recorded progress event, owning its item name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub phase: Phase,
    pub done: u64,
    pub total: Option<u64>,
    pub item: Option<String>,
}

/// A progress sink that records every event for assertions.
#[derive(Debug, Default)]
pub struct VecSink(Mutex<Vec<Event>>);

impl VecSink {
    pub fn new() -> VecSink {
        VecSink::default()
    }

    /// The events so far.
    pub fn events(&self) -> Vec<Event> {
        self.0.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

impl ProgressSink for VecSink {
    fn report(&self, p: &Progress<'_>) {
        if let Ok(mut events) = self.0.lock() {
            events.push(Event {
                phase: p.phase,
                done: p.done,
                total: p.total,
                item: p.item.map(str::to_owned),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix_is_reproducible() {
        // Reference values for seed 0 from the SplitMix64 paper's C implementation.
        let mut rng = SplitMix64::new(0);
        assert_eq!(rng.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(rng.next_u64(), 0x6E78_9E6A_A1B9_65F4);
    }

    #[test]
    fn corruptor_covers_every_truncation_and_keeps_lengths() {
        let input = [7u8; 16];
        let mut seen = 0usize;
        let mut corruptor = Corruptor::new(1);
        corruptor.run(&input, 50, |mutated| {
            assert!(mutated.len() <= input.len());
            seen += 1;
        });
        assert_eq!(seen, 16 + 50);
        assert_ne!(corruptor.flip_bit(&input), input);
        assert_eq!(corruptor.inflate_length(&input).len(), input.len());
        assert!(corruptor.mutate(&[]).is_empty());
    }

    #[test]
    fn vec_sink_records() {
        let sink = VecSink::new();
        sink.report(&Progress {
            phase: Phase::Writing,
            done: 3,
            total: Some(4),
            item: Some("a"),
        });
        assert_eq!(sink.events()[0].item.as_deref(), Some("a"));
    }
}
