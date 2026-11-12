//! Panic-free fixed-width integer reads from byte slices.

use crate::RangeError;

/// `N` bytes of `buf` starting at `at`, or `OutOfBounds`.
fn take<const N: usize>(buf: &[u8], at: u64) -> Result<[u8; N], RangeError> {
    let limit = u64::try_from(buf.len()).map_err(|_| RangeError::Overflow)?;
    let width = u64::try_from(N).map_err(|_| RangeError::Overflow)?;
    let end = at.checked_add(width).ok_or(RangeError::Overflow)?;
    let out_of_bounds = RangeError::OutOfBounds { end, limit };
    let start = usize::try_from(at).map_err(|_| out_of_bounds)?;
    buf.get(start..)
        .and_then(|rest| rest.get(..N))
        .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
        .ok_or(out_of_bounds)
}

/// The byte at `at`.
pub fn u8_at(buf: &[u8], at: u64) -> Result<u8, RangeError> {
    take::<1>(buf, at).map(|[b]| b)
}

macro_rules! int_readers {
    ($($le:ident $be:ident $ty:ty;)*) => {$(
        #[doc = concat!("Little-endian `", stringify!($ty), "` at byte offset `at`.")]
        pub fn $le(buf: &[u8], at: u64) -> Result<$ty, RangeError> {
            take(buf, at).map(<$ty>::from_le_bytes)
        }
        #[doc = concat!("Big-endian `", stringify!($ty), "` at byte offset `at`.")]
        pub fn $be(buf: &[u8], at: u64) -> Result<$ty, RangeError> {
            take(buf, at).map(<$ty>::from_be_bytes)
        }
    )*};
}

int_readers! {
    u16_le u16_be u16;
    u32_le u32_be u32;
    u64_le u64_be u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_endians() {
        let buf = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(u8_at(&buf, 7), Ok(0x08));
        assert_eq!(u16_le(&buf, 0), Ok(0x0201));
        assert_eq!(u16_be(&buf, 0), Ok(0x0102));
        assert_eq!(u32_le(&buf, 4), Ok(0x0807_0605));
        assert_eq!(u32_be(&buf, 4), Ok(0x0506_0708));
        assert_eq!(u64_be(&buf, 0), Ok(0x0102_0304_0506_0708));
        assert_eq!(u64_le(&buf, 0), Ok(0x0807_0605_0403_0201));
    }

    #[test]
    fn never_panics_out_of_bounds() {
        let buf = [0u8; 4];
        assert!(u8_at(&buf, 4).is_err());
        assert!(u32_le(&buf, 1).is_err());
        assert!(u64_be(&buf, 0).is_err());
        assert_eq!(u16_le(&buf, u64::MAX), Err(RangeError::Overflow));
        assert!(u16_le(&[], 0).is_err());
    }
}
