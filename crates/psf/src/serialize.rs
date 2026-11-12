use std::collections::HashSet;

use pantheon_core::{Limit, Limits, align_up};

use crate::parse::{HEADER_LEN, INDEX_LEN};
use crate::{Error, MAGIC, MAX_KEY_BYTES, Sfo, VERSION_1_1, value_from_raw};

/// Tables and value starts are aligned to this many bytes.
const ALIGNMENT: u64 = 4;

/// Serializes SFO 1.1 into a new buffer.
///
/// Keeps entry order, keys, formats, raw used bytes, capacities and padding.
/// `raw` is authoritative: `value` must match it for `format`, and
/// `raw.len() + padding.len()` must equal `max_len`. Tables and value starts are
/// 4-byte aligned with zero-filled gaps; original offsets and gaps are not kept.
/// SFO 1.0 must be changed to 1.1 explicitly first. The output size is checked
/// against `limits.max_sfo` and the entry count against `limits.max_records`.
pub fn serialize(sfo: &Sfo<'_>, limits: &Limits) -> Result<Vec<u8>, Error> {
    if sfo.version != VERSION_1_1 {
        return Err(Error::UnsupportedVersion(sfo.version));
    }
    let size_limit = Limit::new("serialized PARAM.SFO size", limits.max_sfo);
    let count =
        u64::try_from(sfo.entries.len()).map_err(|_| Error::Malformed("too many entries"))?;
    Limit::new("PARAM.SFO entries", u64::from(limits.max_records)).check(count)?;
    let add = |a: u64, b: u64| -> Result<u64, Error> {
        let sum = a.checked_add(b).ok_or(Error::Malformed("size overflows"))?;
        size_limit.check(sum)?;
        Ok(sum)
    };
    let align = |n: u64| -> Result<u64, Error> {
        let aligned = align_up(n, ALIGNMENT)?;
        size_limit.check(aligned)?;
        Ok(aligned)
    };

    // Pass 1: validate the model and lay out key and value offsets.
    let keys = add(HEADER_LEN, count.saturating_mul(INDEX_LEN))?;
    let mut key_len = 0u64;
    let mut data_len = 0u64;
    let mut seen = HashSet::with_capacity(sfo.entries.len());
    let mut layout = Vec::with_capacity(sfo.entries.len());
    for entry in &sfo.entries {
        if entry.key.is_empty() || entry.key.contains(&0) || !seen.insert(entry.key) {
            return Err(Error::Malformed("empty, duplicate or NUL-containing key"));
        }
        if entry.key.len() > MAX_KEY_BYTES {
            return Err(Error::Malformed("key exceeds 1024 bytes"));
        }
        let key_offset =
            u16::try_from(key_len).map_err(|_| Error::Malformed("key offset exceeds u16"))?;
        key_len = add(add(key_len, len64(entry.key))?, 1)?;
        let capacity = add(len64(entry.raw), len64(entry.padding))?;
        if capacity != u64::from(entry.max_len) {
            return Err(Error::Malformed(
                "raw bytes and padding do not match capacity",
            ));
        }
        if value_from_raw(entry.format, entry.raw)? != entry.value {
            return Err(Error::Malformed(
                "typed value disagrees with raw bytes or format",
            ));
        }
        let value_offset = align(data_len)?;
        data_len = add(value_offset, capacity)?;
        layout.push((key_offset, value_offset));
    }
    let values = align(add(keys, key_len)?)?;
    let total = add(values, data_len)?;

    // Pass 2: write. Every offset is <= total <= max_sfo, which fits u32 for any
    // sane limit; the conversions still fail cleanly rather than truncate.
    let mut out = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&sfo.version.to_le_bytes());
    out.extend_from_slice(&u32_of(keys)?.to_le_bytes());
    out.extend_from_slice(&u32_of(values)?.to_le_bytes());
    out.extend_from_slice(&u32_of(count)?.to_le_bytes());
    for (entry, (key_offset, value_offset)) in sfo.entries.iter().zip(&layout) {
        out.extend_from_slice(&key_offset.to_le_bytes());
        out.extend_from_slice(&entry.format.to_le_bytes());
        out.extend_from_slice(&u32_of(len64(entry.raw))?.to_le_bytes());
        out.extend_from_slice(&entry.max_len.to_le_bytes());
        out.extend_from_slice(&u32_of(*value_offset)?.to_le_bytes());
    }
    for entry in &sfo.entries {
        out.extend_from_slice(entry.key);
        out.push(0);
    }
    pad_to(&mut out, values);
    for (entry, (_, value_offset)) in sfo.entries.iter().zip(&layout) {
        pad_to(&mut out, values.saturating_add(*value_offset));
        out.extend_from_slice(entry.raw);
        out.extend_from_slice(entry.padding);
    }
    Ok(out)
}

fn len64(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

fn u32_of(n: u64) -> Result<u32, Error> {
    u32::try_from(n).map_err(|_| Error::Malformed("offset exceeds u32"))
}

fn pad_to(out: &mut Vec<u8>, at: u64) {
    out.resize(usize::try_from(at).unwrap_or(out.len()).max(out.len()), 0);
}
