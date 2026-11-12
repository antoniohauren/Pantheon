//! Foundation types shared by every Pantheon crate (docs/03-foundation.md).
//!
//! Standard library only: checked offset math, panic-free binary readers,
//! resource limits, cancellation, progress, and the shared error classification.

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

mod bytes;
mod ctx;
mod error;
mod limits;
mod read;
mod span;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use bytes::{u8_at, u16_be, u16_le, u32_be, u32_le, u64_be, u64_le};
pub use ctx::{Cancel, Ctx, NoProgress, Phase, Progress, ProgressSink};
pub use error::{Cancelled, Classify, Error, ErrorKind, LimitError, RangeError};
pub use limits::{Limit, Limits};
pub use read::{BoundedRead, CHUNK, SpanReader, read_exact_capped};
pub use span::{Span, align_up, usize_len};
