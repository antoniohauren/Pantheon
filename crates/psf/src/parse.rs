use std::collections::{BTreeMap, HashSet};

use pantheon_core::{Limit, Limits, Span, u16_le, u32_le};

use crate::{Entry, Error, MAGIC, MAX_KEY_BYTES, Sfo, VERSION_1_0, VERSION_1_1, value_from_raw};

pub(crate) const HEADER_LEN: u64 = 20;
pub(crate) const INDEX_LEN: u64 = 16;

fn slice(data: &[u8], span: Span) -> &[u8] {
    // `span` was checked against `data.len()`, so the range is in bounds.
    let start = usize::try_from(span.offset).unwrap_or(usize::MAX);
    let len = usize::try_from(span.len).unwrap_or(usize::MAX);
    data.get(start..)
        .and_then(|rest| rest.get(..len))
        .unwrap_or_default()
}

/// Parses SFO 1.0 or 1.1, borrowing keys, values and padding from `data`.
///
/// Rejects empty or duplicate keys, unterminated keys and text, used lengths above
/// capacity, and overlapping nonempty capacities (so output stays bounded by input).
/// Unknown formats and non-UTF-8 bytes are kept. Uses `limits.max_sfo` and
/// `limits.max_records`.
pub fn parse<'a>(data: &'a [u8], limits: &Limits) -> Result<Sfo<'a>, Error> {
    let total = u64::try_from(data.len()).map_err(|_| Error::Malformed("input too large"))?;
    Limit::new("PARAM.SFO size", limits.max_sfo).check(total)?;
    if data.get(..MAGIC.len()) != Some(&MAGIC[..]) {
        return Err(Error::Malformed("bad magic"));
    }
    let version = u32_le(data, 4)?;
    if !matches!(version, VERSION_1_0 | VERSION_1_1) {
        return Err(Error::UnsupportedVersion(version));
    }
    let keys = u64::from(u32_le(data, 8)?);
    let values = u64::from(u32_le(data, 12)?);
    let count = u32_le(data, 16)?;
    Limit::new("PARAM.SFO entries", u64::from(limits.max_records)).check(u64::from(count))?;
    let index = Span::new(
        HEADER_LEN,
        u64::from(count).saturating_mul(INDEX_LEN),
        total,
    )
    .map_err(|_| Error::Malformed("index table outside input"))?;
    if keys < index.end() || values < keys || values > total {
        return Err(Error::Malformed("invalid table layout"));
    }
    let key_table = slice(data, Span::new(keys, values.saturating_sub(keys), total)?);

    // count <= max_records (65 536 by default), so these capacities are bounded.
    let capacity = usize::try_from(count).unwrap_or(0);
    let mut entries = Vec::with_capacity(capacity);
    let mut seen = HashSet::with_capacity(capacity);
    let mut ranges = BTreeMap::<u64, u64>::new();
    let mut record_at = HEADER_LEN;
    for _ in 0..count {
        let key_offset = usize::from(u16_le(data, record_at)?);
        let format = u16_le(data, record_at.saturating_add(2))?;
        let len = u32_le(data, record_at.saturating_add(4))?;
        let max_len = u32_le(data, record_at.saturating_add(8))?;
        let offset = u64::from(u32_le(data, record_at.saturating_add(12))?);
        record_at = record_at.saturating_add(INDEX_LEN);
        if len > max_len {
            return Err(Error::Malformed("value length exceeds capacity"));
        }
        let key = read_key(key_table, key_offset)?;
        if key.is_empty() || !seen.insert(key) {
            return Err(Error::Malformed("empty or duplicate key"));
        }
        let start = values
            .checked_add(offset)
            .ok_or(Error::Malformed("value offset overflows"))?;
        let span = Span::new(start, u64::from(max_len), total)
            .map_err(|_| Error::Malformed("value outside input"))?;
        if span.len != 0 {
            // Neighbours in start order are enough: earlier ranges never overlap each other.
            let before = ranges.range(..=offset).next_back();
            let after = ranges.range(offset..).next();
            let end = offset.saturating_add(span.len);
            if before.is_some_and(|(_, prev_end)| *prev_end > offset)
                || after.is_some_and(|(next, _)| *next < end)
            {
                return Err(Error::Malformed("overlapping value capacities"));
            }
            ranges.insert(offset, end);
        }
        let capacity = slice(data, span);
        let (raw, padding) =
            capacity.split_at(usize::try_from(len).unwrap_or(0).min(capacity.len()));
        entries.push(Entry {
            key,
            format,
            value: value_from_raw(format, raw)?,
            raw,
            max_len,
            padding,
        });
    }
    Ok(Sfo { version, entries })
}

fn read_key(table: &[u8], at: usize) -> Result<&[u8], Error> {
    let tail = table
        .get(at..)
        .ok_or(Error::Malformed("key outside key table"))?;
    let window = tail.get(..=MAX_KEY_BYTES).unwrap_or(tail);
    match window.iter().position(|b| *b == 0) {
        Some(end) => Ok(window.get(..end).unwrap_or_default()),
        None if tail.len() > MAX_KEY_BYTES => Err(Error::Malformed("key exceeds 1024 bytes")),
        None => Err(Error::Malformed("unterminated key")),
    }
}
