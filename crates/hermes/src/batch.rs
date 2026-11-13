//! Batch validation (hermes.md "Scope"): every package is checked before
//! anything is sent. Identity comes from `pantheon-package`, not our own parsing.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use hermes_wire::{DIGEST_LEN, MAX_ICON, limits};
use pantheon_core::Limits;
use pantheon_package::{Package, ps4};

use crate::ops::{Error, ItemReport};

/// pkg-tools' category when PARAM.SFO is missing or encrypted.
const FALLBACK_CATEGORY: &str = "gd";

/// One validated package, ready to become a `Job`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub path: PathBuf,
    pub name: String,
    pub content_id: String,
    /// `"PS4"` + upper-cased category.
    pub package_type: String,
    pub size: u64,
    pub digest: [u8; DIGEST_LEN],
    pub icon: Option<Vec<u8>>,
}

impl Item {
    pub(crate) fn report(&self) -> ItemReport {
        ItemReport {
            path: self.path.clone(),
            name: self.name.clone(),
            size: self.size,
            bytes_transferred: 0,
            transferred: false,
            task_id: None,
            state: None,
            installed: false,
            failure: None,
        }
    }
}

/// Validates every package; the first bad one fails the whole batch.
pub(crate) fn validate(paths: &[PathBuf], limits: &Limits) -> Result<Vec<Item>, Error> {
    if paths.is_empty() {
        return Err(Error::EmptyBatch);
    }
    paths.iter().map(|p| identity(p, limits)).collect()
}

fn identity(path: &Path, limits: &Limits) -> Result<Item, Error> {
    let package_error = |source| Error::Package {
        path: path.to_owned(),
        source,
    };
    let file = File::open(path).map_err(|e| package_error(pantheon_package::Error::Io(e)))?;
    let pkg = Package::open(BufReader::new(file), limits).map_err(package_error)?;
    let Package::Ps4(mut pkg) = pkg else {
        return Err(Error::NotPs4 {
            path: path.to_owned(),
        });
    };
    let field = |field, problem| Error::Field {
        path: path.to_owned(),
        field,
        problem,
    };
    let content_id = std::str::from_utf8(pkg.header().content_id_bytes())
        .map_err(|_| field("content ID", "is not UTF-8"))?
        .to_owned();
    let size = pkg.header().package_size;

    let ps4_error = |e| package_error(pantheon_package::Error::Ps4(e));
    // pkg-tools: a missing or encrypted PARAM.SFO falls back to the file name and "gd".
    let sfo = match pkg.read_param_sfo() {
        Ok(sfo) => sfo,
        Err(ps4::Error::DecryptionUnavailable { .. }) => None,
        Err(e) => return Err(ps4_error(e)),
    };
    let (title, category) = match &sfo {
        Some(bytes) => {
            let sfo = pantheon_psf::parse(bytes, limits).map_err(|source| Error::Sfo {
                path: path.to_owned(),
                source,
            })?;
            (
                sfo.text(b"TITLE").map(str::to_owned),
                sfo.text(b"CATEGORY").map(str::to_owned),
            )
        }
        None => (None, None),
    };
    let name = title.unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let category = category.unwrap_or_else(|| FALLBACK_CATEGORY.to_owned());

    // shortcut: any icon0 problem (absent, encrypted, too large) sends no icon,
    // as pkg-tools does; it also checks PNG dimensions, which needs a PNG reader
    // below L2 (Athena has one). Add it when the GUI shares that code.
    let icon = pkg.read_icon0().ok().flatten();
    let digest = pkg.read_digest().map_err(ps4_error)?;

    for (name_, value, max) in [
        ("package name", name.as_bytes(), limits::NAME),
        ("content ID", content_id.as_bytes(), limits::CONTENT_ID),
        ("package category", category.as_bytes(), limits::CATEGORY),
    ] {
        if value.is_empty() {
            return Err(field(name_, "is empty"));
        }
        if u64::try_from(value.len()).unwrap_or(u64::MAX) > u64::from(max) {
            return Err(field(name_, "is too long for the installer"));
        }
    }
    if icon
        .as_ref()
        .is_some_and(|i| u64::try_from(i.len()).unwrap_or(u64::MAX) > u64::from(MAX_ICON))
    {
        return Err(field("icon", "is too large for the installer"));
    }
    Ok(Item {
        path: path.to_owned(),
        name,
        content_id,
        package_type: format!(
            "{}{}",
            limits::PACKAGE_TYPE_PREFIX,
            category.to_ascii_uppercase()
        ),
        size,
        digest,
        icon,
    })
}
