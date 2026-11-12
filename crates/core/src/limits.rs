//! Resource limits (architecture §4.3). Defaults carry over the pkg-tools values;
//! callers may lower them, never raise them.

use crate::LimitError;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// One named maximum, checked before allocation or seek.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub what: &'static str,
    pub max: u64,
}

impl Limit {
    pub const fn new(what: &'static str, max: u64) -> Limit {
        Limit { what, max }
    }

    /// `Err` when `value` exceeds this limit.
    pub fn check(self, value: u64) -> Result<(), LimitError> {
        if value > self.max {
            return Err(LimitError {
                what: self.what,
                value,
                max: self.max,
            });
        }
        Ok(())
    }
}

/// Every resource policy a parser applies. Each format documents which field it uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Entries in one listed tree.
    pub max_entries: u32,
    /// Total bytes of all path names in one tree.
    pub max_path_bytes: u64,
    /// Bytes of directory data (PFS dirents, PS3 entry tables).
    pub max_dir_data: u64,
    /// One buffered metadata record.
    pub max_record: u64,
    /// One PARAM.SFO.
    pub max_sfo: u64,
    /// Records in one container table or SFO.
    pub max_records: u32,
    /// One artwork image.
    pub max_artwork: u64,
    /// One XML document.
    pub max_xml: u64,
    /// One TRP archive.
    pub max_trp: u64,
    /// All TRP archives of one package combined.
    pub max_trp_total: u64,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        max_entries: 1_000_000,
        max_path_bytes: 64 * MIB,
        max_dir_data: 64 * MIB,
        max_record: 16 * MIB,
        max_sfo: 16 * MIB,
        max_records: 65_536,
        max_artwork: 4 * MIB,
        max_xml: 4 * MIB,
        max_trp: 64 * MIB,
        max_trp_total: 128 * MIB,
    };

    /// Applies `f`, then clamps every field to at most its current value, so a
    /// caller can only lower limits.
    pub fn lowered(self, f: impl FnOnce(&mut Limits)) -> Limits {
        let mut next = self;
        f(&mut next);
        Limits {
            max_entries: next.max_entries.min(self.max_entries),
            max_path_bytes: next.max_path_bytes.min(self.max_path_bytes),
            max_dir_data: next.max_dir_data.min(self.max_dir_data),
            max_record: next.max_record.min(self.max_record),
            max_sfo: next.max_sfo.min(self.max_sfo),
            max_records: next.max_records.min(self.max_records),
            max_artwork: next.max_artwork.min(self.max_artwork),
            max_xml: next.max_xml.min(self.max_xml),
            max_trp: next.max_trp.min(self.max_trp),
            max_trp_total: next.max_trp_total.min(self.max_trp_total),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowered_never_raises() {
        let limits = Limits::DEFAULT.lowered(|l| {
            l.max_sfo = 1024;
            l.max_xml = u64::MAX;
            l.max_entries = u32::MAX;
        });
        assert_eq!(limits.max_sfo, 1024);
        assert_eq!(limits.max_xml, Limits::DEFAULT.max_xml);
        assert_eq!(limits.max_entries, Limits::DEFAULT.max_entries);
    }

    #[test]
    fn limit_check() {
        let limit = Limit::new("xml size", 4);
        assert!(limit.check(4).is_ok());
        assert_eq!(
            limit.check(5),
            Err(LimitError {
                what: "xml size",
                value: 5,
                max: 4
            })
        );
    }
}
