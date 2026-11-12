//! pantheon-package: input detection by magic bytes and one handle over PS3 and
//! PS4 packages (architecture §3, L1b).
//!
//! shortcut: covers detection, identity and artwork reads only; the `Tree`
//! listing, entry reads and extraction land with Hades (Phase 3). ZAR detection
//! lands with Proteus (its magic is in the footer, not the first bytes).

#![deny(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};

use pantheon_core::{Classify, ErrorKind, Limits};

pub use pantheon_ps3 as ps3;
pub use pantheon_ps4 as ps4;

/// `\0PSF`, the PARAM.SFO magic.
pub const SFO_MAGIC: [u8; 4] = *b"\0PSF";

/// What an input is, decided by its first bytes, never its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Ps3Pkg,
    Ps4Pkg,
    Sfo,
    Unknown,
}

impl Kind {
    /// Stable machine name, as used in JSON output.
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Ps3Pkg => "ps3-pkg",
            Kind::Ps4Pkg => "ps4-pkg",
            Kind::Sfo => "sfo",
            Kind::Unknown => "unknown",
        }
    }
}

/// Reads the first four bytes and rewinds. Inputs shorter than that are `Unknown`.
pub fn detect<R: Read + Seek>(src: &mut R) -> io::Result<Kind> {
    src.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    let mut got = 0;
    while let Some(rest) = magic.get_mut(got..).filter(|r| !r.is_empty()) {
        match src.read(rest) {
            Ok(0) => break,
            Ok(n) => got = got.saturating_add(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    src.seek(SeekFrom::Start(0))?;
    Ok(match magic {
        ps3::MAGIC => Kind::Ps3Pkg,
        ps4::MAGIC => Kind::Ps4Pkg,
        SFO_MAGIC => Kind::Sfo,
        _ => Kind::Unknown,
    })
}

/// Which artwork image to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Artwork {
    Icon0,
    /// PS4 only; PS3 packages have none.
    Pic1,
}

/// An opened package of either platform.
#[derive(Debug)]
pub enum Package<R> {
    Ps3(ps3::Package<R>),
    Ps4(ps4::Package<R>),
}

impl<R: Read + Seek> Package<R> {
    /// Detects and opens a PS3 or PS4 package.
    pub fn open(mut src: R, limits: &Limits) -> Result<Package<R>, Error> {
        match detect(&mut src)? {
            Kind::Ps3Pkg => Ok(Package::Ps3(ps3::Package::open(src, limits)?)),
            Kind::Ps4Pkg => Ok(Package::Ps4(ps4::Package::open(src, limits)?)),
            other => Err(Error::NotAPackage(other)),
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Package::Ps3(_) => Kind::Ps3Pkg,
            Package::Ps4(_) => Kind::Ps4Pkg,
        }
    }

    /// The content ID bytes up to the first NUL.
    pub fn content_id(&self) -> &[u8] {
        match self {
            Package::Ps3(p) => p.header().content_id_bytes(),
            Package::Ps4(p) => p.header().content_id_bytes(),
        }
    }

    pub fn package_size(&self) -> u64 {
        match self {
            Package::Ps3(p) => p.header().package_size,
            Package::Ps4(p) => p.header().package_size,
        }
    }

    /// PARAM.SFO bytes, `None` if absent. An encrypted PS4 SFO is an error of
    /// kind `KeyUnavailable`.
    pub fn read_param_sfo(&mut self) -> Result<Option<Vec<u8>>, Error> {
        Ok(match self {
            Package::Ps3(p) => p.read_param_sfo()?,
            Package::Ps4(p) => p.read_param_sfo()?,
        })
    }

    /// Artwork bytes (bounded by `Limits::max_artwork`), `None` if absent.
    pub fn artwork(&mut self, which: Artwork) -> Result<Option<Vec<u8>>, Error> {
        Ok(match (self, which) {
            (Package::Ps3(p), Artwork::Icon0) => p.read_icon0()?,
            (Package::Ps3(_), Artwork::Pic1) => None,
            (Package::Ps4(p), Artwork::Icon0) => p.read_icon0()?,
            (Package::Ps4(p), Artwork::Pic1) => p.read_pic1()?,
        })
    }
}

#[derive(Debug)]
pub enum Error {
    /// The input is not a PS3 or PS4 package.
    NotAPackage(Kind),
    Ps3(ps3::Error),
    Ps4(ps4::Error),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotAPackage(Kind::Sfo) => {
                f.write_str("input is a bare PARAM.SFO, not a package")
            }
            Error::NotAPackage(_) => {
                f.write_str("input is not a PS3 or PS4 package (unknown magic)")
            }
            Error::Ps3(e) => e.fmt(f),
            Error::Ps4(e) => e.fmt(f),
            Error::Io(e) => write!(f, "I/O failure: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::NotAPackage(_) => None,
            Error::Ps3(e) => Some(e),
            Error::Ps4(e) => Some(e),
            Error::Io(e) => Some(e),
        }
    }
}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::NotAPackage(_) => ErrorKind::Unsupported,
            Error::Ps3(e) => e.kind(),
            Error::Ps4(e) => e.kind(),
            Error::Io(_) => ErrorKind::Io,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<ps3::Error> for Error {
    fn from(e: ps3::Error) -> Self {
        Error::Ps3(e)
    }
}

impl From<ps4::Error> for Error {
    fn from(e: ps4::Error) -> Self {
        Error::Ps4(e)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn detects_by_magic_only() {
        let cases: [(&[u8], Kind); 5] = [
            (b"\x7fPKG....", Kind::Ps3Pkg),
            (b"\x7fCNT", Kind::Ps4Pkg),
            (b"\0PSF\x01\x01", Kind::Sfo),
            (b"\x7fPK", Kind::Unknown),
            (b"", Kind::Unknown),
        ];
        for (bytes, kind) in cases {
            let mut src = Cursor::new(bytes);
            assert_eq!(detect(&mut src).expect("detect"), kind);
            assert_eq!(src.position(), 0);
        }
    }

    #[test]
    fn opens_both_platforms_and_reads_artwork() {
        let ps4 = ps4::testing::package(
            "PS4-ID",
            &[ps4::testing::Record::plain(ps4::id::PIC1, b"pic")],
        );
        let mut pkg = Package::open(Cursor::new(&ps4[..]), &Limits::DEFAULT).expect("ps4");
        assert_eq!(
            (pkg.kind(), pkg.content_id()),
            (Kind::Ps4Pkg, &b"PS4-ID"[..])
        );
        assert_eq!(
            pkg.artwork(Artwork::Pic1).expect("pic1").as_deref(),
            Some(&b"pic"[..])
        );
        assert_eq!(pkg.artwork(Artwork::Icon0).expect("icon0"), None);

        let items = [ps3::testing::Item::file("ICON0.PNG", b"icon")];
        let ps3 = ps3::testing::package(true, "PS3-ID", &[], &items);
        let mut pkg = Package::open(Cursor::new(&ps3[..]), &Limits::DEFAULT).expect("ps3");
        assert_eq!(pkg.package_size(), ps3.len() as u64);
        assert_eq!(
            pkg.artwork(Artwork::Icon0).expect("icon0").as_deref(),
            Some(&b"icon"[..])
        );
        assert_eq!(pkg.artwork(Artwork::Pic1).expect("pic1"), None);
        assert_eq!(pkg.read_param_sfo().expect("sfo"), None);
    }

    #[test]
    fn rejects_non_packages() {
        let err = Package::open(Cursor::new(&b"\0PSF"[..]), &Limits::DEFAULT).unwrap_err();
        assert!(matches!(err, Error::NotAPackage(Kind::Sfo)));
        assert_eq!(err.kind(), ErrorKind::Unsupported);
    }
}
