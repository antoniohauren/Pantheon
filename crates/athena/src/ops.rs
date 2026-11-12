//! Athena's library API: what an input is, without changing anything.

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};

use pantheon_core::{Classify, ErrorKind, Limit, Limits, read_exact_capped};
use pantheon_package::{Artwork, Kind, Package, ps3, ps4};

/// The result of [`inspect`]. Typed data only; frontends render it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inspection {
    pub kind: Kind,
    pub summary: Summary,
    pub sfo: SfoStatus,
    pub details: Details,
}

/// Overview rows. `None` means "not known" (no SFO, or the value is not UTF-8).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub title: Option<String>,
    pub title_id: Option<String>,
    pub app_version: Option<String>,
    pub category: Option<String>,
    pub content: ContentKind,
    pub package_size: Option<u64>,
    pub content_id: Option<String>,
}

/// What the SFO `CATEGORY` says the content is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContentKind {
    Game,
    Patch,
    /// PS3 `GD`: game data, often a patch.
    GameData,
    AddOn,
    Other,
    #[default]
    Unknown,
}

impl ContentKind {
    pub fn from_category(category: &[u8]) -> ContentKind {
        match category {
            b"gd" | b"HG" | b"DG" => ContentKind::Game,
            b"gp" => ContentKind::Patch,
            b"GD" => ContentKind::GameData,
            b"ac" => ContentKind::AddOn,
            _ => ContentKind::Other,
        }
    }

    /// Stable machine name.
    pub const fn name(self) -> &'static str {
        match self {
            ContentKind::Game => "game",
            ContentKind::Patch => "patch",
            ContentKind::GameData => "game_data",
            ContentKind::AddOn => "add_on",
            ContentKind::Other => "other",
            ContentKind::Unknown => "unknown",
        }
    }
}

/// Whether the SFO could be read. Missing and encrypted are not errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SfoStatus {
    Available(SfoTable),
    Missing,
    /// PS4: the SFO record is encrypted and no key is available.
    Encrypted {
        entry_id: u32,
        key_index: u8,
    },
}

/// An owned copy of a parsed PARAM.SFO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SfoTable {
    pub version: u32,
    pub entries: Vec<SfoEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SfoEntry {
    pub key: Vec<u8>,
    pub format: u16,
    pub value: SfoValue,
    /// Used bytes, text terminator included.
    pub raw: Vec<u8>,
    pub max_len: u32,
    pub padding: Vec<u8>,
}

/// The interpreted value; binary and unknown values are their `raw` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SfoValue {
    /// Bytes before the NUL, not decoded.
    Text(Vec<u8>),
    Integer(u32),
    Binary,
    Unknown,
}

impl SfoTable {
    fn from_parsed(sfo: &pantheon_psf::Sfo<'_>) -> SfoTable {
        let entries = sfo
            .entries
            .iter()
            .map(|e| SfoEntry {
                key: e.key.to_vec(),
                format: e.format,
                value: match e.value {
                    pantheon_psf::Value::Text(t) => SfoValue::Text(t.to_vec()),
                    pantheon_psf::Value::Integer(n) => SfoValue::Integer(n),
                    pantheon_psf::Value::Binary(_) => SfoValue::Binary,
                    pantheon_psf::Value::Unknown { .. } => SfoValue::Unknown,
                },
                raw: e.raw.to_vec(),
                max_len: e.max_len,
                padding: e.padding.to_vec(),
            })
            .collect();
        SfoTable {
            version: sfo.version,
            entries,
        }
    }

    /// The UTF-8 text value of `key`.
    pub fn text(&self, key: &[u8]) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.key == key)
            .and_then(|e| match &e.value {
                SfoValue::Text(t) => std::str::from_utf8(t).ok(),
                _ => None,
            })
    }

    fn category(&self) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|e| e.key == b"CATEGORY")
            .and_then(|e| match &e.value {
                SfoValue::Text(t) => Some(t.as_slice()),
                _ => None,
            })
    }
}

/// Platform-specific structure, never preformatted text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Details {
    /// A bare PARAM.SFO has nothing beyond the SFO itself.
    Sfo,
    Ps3 {
        header: ps3::Header,
        metadata: Vec<ps3::Metadata>,
        entries: Vec<ps3::FileEntry>,
    },
    Ps4 {
        header: ps4::Header,
        entries: Vec<ps4::Entry>,
    },
}

#[derive(Debug)]
pub enum Error {
    Package(pantheon_package::Error),
    Sfo(pantheon_psf::Error),
    Limit(pantheon_core::LimitError),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Package(e) => e.fmt(f),
            Error::Sfo(e) => e.fmt(f),
            Error::Limit(e) => e.fmt(f),
            Error::Io(e) => write!(f, "I/O failure: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Package(e) => Some(e),
            Error::Sfo(e) => Some(e),
            Error::Limit(e) => Some(e),
            Error::Io(e) => Some(e),
        }
    }
}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Package(e) => e.kind(),
            Error::Sfo(e) => e.kind(),
            Error::Limit(_) => ErrorKind::Limit,
            Error::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof => ErrorKind::Malformed,
            Error::Io(_) => ErrorKind::Io,
        }
    }
}

impl From<pantheon_package::Error> for Error {
    fn from(e: pantheon_package::Error) -> Self {
        Error::Package(e)
    }
}

impl From<pantheon_psf::Error> for Error {
    fn from(e: pantheon_psf::Error) -> Self {
        Error::Sfo(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<pantheon_core::Error> for Error {
    fn from(e: pantheon_core::Error) -> Self {
        match e {
            pantheon_core::Error::Limit(e) => Error::Limit(e),
            pantheon_core::Error::Io(e) => Error::Io(e),
            other => Error::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                other.to_string(),
            )),
        }
    }
}

fn utf8(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

/// Inspects a PS3 PKG, PS4 PKG or bare PARAM.SFO, detected by magic.
pub fn inspect<R: Read + Seek>(mut src: R, limits: &Limits) -> Result<Inspection, Error> {
    let kind = pantheon_package::detect(&mut src)?;
    if kind == Kind::Sfo {
        let total = src.seek(SeekFrom::End(0))?;
        src.seek(SeekFrom::Start(0))?;
        let data = read_exact_capped(
            &mut src,
            total,
            Limit::new("PARAM.SFO size", limits.max_sfo),
        )?;
        let table = SfoTable::from_parsed(&pantheon_psf::parse(&data, limits)?);
        return Ok(Inspection {
            kind,
            summary: summary(Some(&table), None, None),
            sfo: SfoStatus::Available(table),
            details: Details::Sfo,
        });
    }
    let mut pkg = Package::open(src, limits)?;
    let sfo = match pkg.read_param_sfo() {
        Ok(Some(data)) => {
            SfoStatus::Available(SfoTable::from_parsed(&pantheon_psf::parse(&data, limits)?))
        }
        Ok(None) => SfoStatus::Missing,
        Err(pantheon_package::Error::Ps4(ps4::Error::DecryptionUnavailable {
            entry_id,
            key_index,
        })) => SfoStatus::Encrypted {
            entry_id,
            key_index,
        },
        Err(e) => return Err(e.into()),
    };
    let table = match &sfo {
        SfoStatus::Available(t) => Some(t),
        _ => None,
    };
    let summary = summary(table, Some(pkg.package_size()), Some(pkg.content_id()));
    let details = match pkg {
        Package::Ps3(p) => Details::Ps3 {
            header: p.header().clone(),
            metadata: p.metadata().to_vec(),
            entries: p.entries().to_vec(),
        },
        Package::Ps4(p) => Details::Ps4 {
            header: p.header().clone(),
            entries: p.entries().to_vec(),
        },
    };
    Ok(Inspection {
        kind,
        summary,
        sfo,
        details,
    })
}

fn summary(
    sfo: Option<&SfoTable>,
    package_size: Option<u64>,
    content_id: Option<&[u8]>,
) -> Summary {
    let text = |key: &[u8]| sfo.and_then(|t| t.text(key)).map(str::to_owned);
    let category = sfo.and_then(SfoTable::category);
    Summary {
        title: text(b"TITLE"),
        title_id: text(b"TITLE_ID"),
        app_version: text(b"APP_VER"),
        category: category.and_then(utf8),
        content: category.map_or(ContentKind::Unknown, ContentKind::from_category),
        package_size,
        content_id: content_id.and_then(utf8),
    }
}

/// The artwork bytes of a package (bounded by `Limits::max_artwork`), `None`
/// if it has none. A bare SFO has no artwork.
pub fn artwork<R: Read + Seek>(
    src: R,
    which: Artwork,
    limits: &Limits,
) -> Result<Option<Vec<u8>>, Error> {
    Ok(Package::open(src, limits)?.artwork(which)?)
}

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const IHDR_LEN: [u8; 4] = 13u32.to_be_bytes();
/// Largest width or height a frontend should decode.
pub const MAX_ARTWORK_SIDE: u32 = 2048;

/// True for a PNG whose header declares 1..=2048 pixels on both sides; frontends
/// show only such images, bounding decoded memory as well as file size.
pub fn displayable_png(bytes: &[u8]) -> bool {
    let side = |at: usize| {
        bytes
            .get(at..at.saturating_add(4))
            .and_then(|b| b.try_into().ok())
            .map(u32::from_be_bytes)
            .is_some_and(|n| (1..=MAX_ARTWORK_SIDE).contains(&n))
    };
    bytes.get(..8) == Some(&PNG_SIGNATURE[..])
        && bytes.get(8..12) == Some(&IHDR_LEN[..])
        && bytes.get(12..16) == Some(&b"IHDR"[..])
        && side(16)
        && side(20)
}
