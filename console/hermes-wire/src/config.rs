//! The payload's config block (hermes.md "Config block").
//!
//! Layout, all integers little endian:
//!
//! ```text
//! b"HRMSCFG1" | version u32 | host IPv4 [4] | control port u16 | token [16] | build ID [16]
//! ```
//!
//! The payload is built with [`template`] (marker + version, the rest zero).
//! `xtask payload` stamps the build ID, and the host patches the host address,
//! port and token before sending; the build ID is left as stamped.
//!
//! The payload reads its block with [`Config::decode_fields`], which never
//! compares the marker, so the marker bytes appear in the binary exactly once.

use crate::{BUILD_ID_LEN, Error, TOKEN_LEN, VERSION};

pub const MARKER: [u8; 8] = *b"HRMSCFG1";
pub const MARKER_LEN: usize = MARKER.len();
/// Bytes after the marker.
pub const FIELDS_LEN: usize = 4 + 4 + 2 + TOKEN_LEN + BUILD_ID_LEN;
/// The whole block.
pub const LEN: usize = MARKER_LEN + FIELDS_LEN;

const VERSION_AT: usize = 0;
const HOST_AT: usize = VERSION_AT + 4;
const PORT_AT: usize = HOST_AT + 4;
const TOKEN_AT: usize = PORT_AT + 2;
const BUILD_ID_AT: usize = TOKEN_AT + TOKEN_LEN;

/// The decoded fields of a config block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub version: u32,
    pub host: [u8; 4],
    pub port: u16,
    pub token: [u8; TOKEN_LEN],
    pub build_id: [u8; BUILD_ID_LEN],
}

/// The block as the payload is built: marker and version, everything else zero.
// Evaluated at compile time: an out-of-range index fails the build, never runs.
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
pub const fn template() -> [u8; LEN] {
    let mut out = [0u8; LEN];
    let version = VERSION.to_le_bytes();
    let mut i = 0;
    while i < MARKER_LEN {
        out[i] = MARKER[i];
        i += 1;
    }
    let mut j = 0;
    while j < version.len() {
        out[MARKER_LEN + VERSION_AT + j] = version[j];
        j += 1;
    }
    out
}

impl Config {
    /// Reads the fields after the marker. A block that was never patched
    /// (port 0) or is from another version is an error.
    pub fn decode_fields(fields: &[u8; FIELDS_LEN]) -> Result<Config, Error> {
        let at = |start: usize, len: usize| {
            fields
                .get(start..start.saturating_add(len))
                .ok_or(Error::Malformed("config block"))
        };
        let arr4 = |start| -> Result<[u8; 4], Error> {
            at(start, 4)?
                .try_into()
                .map_err(|_| Error::Malformed("config block"))
        };
        let version = u32::from_le_bytes(arr4(VERSION_AT)?);
        if version != VERSION {
            return Err(Error::VersionMismatch {
                got: version,
                expected: VERSION,
            });
        }
        let port = at(PORT_AT, 2)?
            .try_into()
            .map(u16::from_le_bytes)
            .map_err(|_| Error::Malformed("config block"))?;
        if port == 0 {
            return Err(Error::Malformed("config block was never patched"));
        }
        Ok(Config {
            version,
            host: arr4(HOST_AT)?,
            port,
            token: at(TOKEN_AT, TOKEN_LEN)?
                .try_into()
                .map_err(|_| Error::Malformed("config block"))?,
            build_id: at(BUILD_ID_AT, BUILD_ID_LEN)?
                .try_into()
                .map_err(|_| Error::Malformed("config block"))?,
        })
    }
}

/// The offset of the only config block in `image`.
pub fn find(image: &[u8]) -> Result<usize, Error> {
    let mut found = None;
    for (offset, window) in image.windows(MARKER_LEN).enumerate() {
        if window == MARKER {
            if found.is_some() {
                return Err(Error::Malformed("config marker appears more than once"));
            }
            found = Some(offset);
        }
    }
    let offset = found.ok_or(Error::Malformed("config marker not found"))?;
    if offset.checked_add(LEN).is_none_or(|end| end > image.len()) {
        return Err(Error::Malformed("config block is truncated"));
    }
    Ok(offset)
}

/// The block's fields at `offset` (as returned by [`find`]).
fn fields_mut(image: &mut [u8], offset: usize) -> Result<&mut [u8], Error> {
    let start = offset
        .checked_add(MARKER_LEN)
        .ok_or(Error::Malformed("config block"))?;
    image
        .get_mut(start..start.saturating_add(FIELDS_LEN))
        .ok_or(Error::Malformed("config block is truncated"))
}

/// Reads the build ID `xtask payload` stamped into `image`.
pub fn build_id(image: &[u8]) -> Result<[u8; BUILD_ID_LEN], Error> {
    let start = find(image)?
        .saturating_add(MARKER_LEN)
        .saturating_add(BUILD_ID_AT);
    image
        .get(start..start.saturating_add(BUILD_ID_LEN))
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::Malformed("config block is truncated"))
}

/// Writes the build ID (`xtask payload`). The version must be this one.
pub fn stamp_build_id(image: &mut [u8], build_id: &[u8; BUILD_ID_LEN]) -> Result<(), Error> {
    let offset = find(image)?;
    let fields = fields_mut(image, offset)?;
    check_version(fields)?;
    copy(fields, BUILD_ID_AT, build_id)
}

/// Patches host address, control port and token (host, before sending).
pub fn patch(
    image: &mut [u8],
    host: [u8; 4],
    port: u16,
    token: &[u8; TOKEN_LEN],
) -> Result<(), Error> {
    let offset = find(image)?;
    let fields = fields_mut(image, offset)?;
    check_version(fields)?;
    copy(fields, HOST_AT, &host)?;
    copy(fields, PORT_AT, &port.to_le_bytes())?;
    copy(fields, TOKEN_AT, token)
}

fn check_version(fields: &[u8]) -> Result<(), Error> {
    let got = fields
        .get(VERSION_AT..HOST_AT)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(Error::Malformed("config block"))?;
    if got == VERSION {
        Ok(())
    } else {
        Err(Error::VersionMismatch {
            got,
            expected: VERSION,
        })
    }
}

fn copy(fields: &mut [u8], at: usize, data: &[u8]) -> Result<(), Error> {
    fields
        .get_mut(at..at.saturating_add(data.len()))
        .ok_or(Error::Malformed("config block"))?
        .copy_from_slice(data);
    Ok(())
}
