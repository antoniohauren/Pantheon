//! Checked offset arithmetic. Format crates use these instead of `+`, `*` or `as usize`.

use crate::{Limit, LimitError, RangeError};

/// A validated byte range: `offset + len` never overflows and never exceeds the
/// bound it was created within.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub offset: u64,
    pub len: u64,
}

impl Span {
    /// A span of `len` bytes at `offset`, checked to end at or before `within`.
    pub fn new(offset: u64, len: u64, within: u64) -> Result<Span, RangeError> {
        let end = offset.checked_add(len).ok_or(RangeError::Overflow)?;
        if end > within {
            return Err(RangeError::OutOfBounds { end, limit: within });
        }
        Ok(Span { offset, len })
    }

    /// A sub-span at `offset` relative to this span's start, checked to stay inside it.
    pub fn sub(&self, offset: u64, len: u64) -> Result<Span, RangeError> {
        let inner = Span::new(offset, len, self.len)?;
        // Cannot overflow: inner.offset <= self.len and self.offset + self.len was checked.
        let absolute = self
            .offset
            .checked_add(inner.offset)
            .ok_or(RangeError::Overflow)?;
        Ok(Span {
            offset: absolute,
            len,
        })
    }

    /// One past the last byte. Never overflows for a constructed span.
    pub fn end(&self) -> u64 {
        // Span::new validated offset + len; saturating keeps this panic-free for hand-built spans.
        self.offset.saturating_add(self.len)
    }

    /// True when the two spans share at least one byte. Empty spans overlap nothing.
    pub fn overlaps(&self, other: &Span) -> bool {
        self.len != 0 && other.len != 0 && self.offset < other.end() && other.offset < self.end()
    }
}

/// Rounds `value` up to a multiple of `align`, which must be a power of two.
pub fn align_up(value: u64, align: u64) -> Result<u64, RangeError> {
    if !align.is_power_of_two() {
        return Err(RangeError::BadAlignment(align));
    }
    let mask = align.wrapping_sub(1);
    value
        .checked_add(mask)
        .map(|v| v & !mask)
        .ok_or(RangeError::Overflow)
}

/// Converts an input length to `usize` after checking it against `cap`.
/// Call this before any `Vec::with_capacity` sized from input.
pub fn usize_len(len: u64, cap: Limit) -> Result<usize, LimitError> {
    cap.check(len)?;
    usize::try_from(len).map_err(|_| LimitError {
        what: cap.what,
        value: len,
        max: u64::try_from(usize::MAX).unwrap_or(u64::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_bounds() {
        assert_eq!(Span::new(2, 3, 5), Ok(Span { offset: 2, len: 3 }));
        assert_eq!(
            Span::new(2, 4, 5),
            Err(RangeError::OutOfBounds { end: 6, limit: 5 })
        );
        assert_eq!(Span::new(u64::MAX, 1, u64::MAX), Err(RangeError::Overflow));
        assert_eq!(Span::new(5, 0, 5), Ok(Span { offset: 5, len: 0 }));
    }

    #[test]
    fn sub_span_stays_inside() {
        let outer = Span::new(10, 10, 100).unwrap();
        assert_eq!(outer.sub(2, 3), Ok(Span { offset: 12, len: 3 }));
        assert!(outer.sub(8, 3).is_err());
        assert!(outer.sub(u64::MAX, 2).is_err());
    }

    #[test]
    fn overlap() {
        let a = Span { offset: 0, len: 4 };
        assert!(a.overlaps(&Span { offset: 3, len: 1 }));
        assert!(!a.overlaps(&Span { offset: 4, len: 1 }));
        assert!(!a.overlaps(&Span { offset: 2, len: 0 }));
    }

    #[test]
    fn alignment() {
        assert_eq!(align_up(0, 16), Ok(0));
        assert_eq!(align_up(1, 16), Ok(16));
        assert_eq!(align_up(16, 16), Ok(16));
        assert_eq!(align_up(u64::MAX, 16), Err(RangeError::Overflow));
        assert_eq!(align_up(5, 0), Err(RangeError::BadAlignment(0)));
        assert_eq!(align_up(5, 3), Err(RangeError::BadAlignment(3)));
    }

    #[test]
    fn usize_len_checks_cap() {
        let cap = Limit::new("record", 8);
        assert_eq!(usize_len(8, cap), Ok(8));
        assert!(usize_len(9, cap).is_err());
    }
}
