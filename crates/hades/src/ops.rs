//! Hades operations: open a package's tree and extract all or part of it.

use std::fmt;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use pantheon_core::{CHUNK, Classify, Ctx, ErrorKind, Limits, Phase, Progress};
use pantheon_fs::{Access, OutputTree};
use pantheon_package::{Contents, NodeKind, Package, Selection};

/// RPCS3 caps generated folder names at 31 UTF-8 bytes.
const MAX_FOLDER_NAME: usize = 31;
const FOLDER_SUFFIX: &str = "-extracted";
const FALLBACK_STEM: &str = "package";

/// What an extraction wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The new tree.
    pub path: PathBuf,
    pub files: u64,
    pub directories: u64,
    /// Decoded file bytes written.
    pub bytes: u64,
}

#[derive(Debug)]
pub enum Error {
    Package(pantheon_package::Error),
    Fs(pantheon_fs::Error),
    Cancelled,
    /// `cause` happened after output was created; the tree stays, marked, at `path`.
    Incomplete {
        path: PathBuf,
        cause: Box<Error>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Package(e) => e.fmt(f),
            Error::Fs(e) => e.fmt(f),
            Error::Cancelled => f.write_str("cancelled"),
            Error::Incomplete { path, cause } => write!(
                f,
                "{cause}; incomplete output left at {} (remove it before retrying)",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Package(e) => Some(e),
            Error::Fs(e) => Some(e),
            Error::Cancelled => None,
            Error::Incomplete { cause, .. } => Some(cause.as_ref()),
        }
    }
}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Package(e) => e.kind(),
            Error::Fs(e) => e.kind(),
            Error::Cancelled => ErrorKind::Cancelled,
            Error::Incomplete { cause, .. } => cause.kind(),
        }
    }

    fn incomplete(&self) -> Option<&Path> {
        match self {
            Error::Fs(e) => e.incomplete(),
            Error::Incomplete { path, .. } => Some(path),
            _ => None,
        }
    }
}

impl From<pantheon_package::Error> for Error {
    fn from(e: pantheon_package::Error) -> Self {
        Error::Package(e)
    }
}

impl From<pantheon_fs::Error> for Error {
    fn from(e: pantheon_fs::Error) -> Self {
        match e {
            pantheon_fs::Error::Cancelled => Error::Cancelled,
            e => Error::Fs(e),
        }
    }
}

impl From<pantheon_core::Cancelled> for Error {
    fn from(_: pantheon_core::Cancelled) -> Self {
        Error::Cancelled
    }
}

/// Detects and opens a package for listing and extraction.
pub fn open<R: Read + Seek>(src: R, limits: &Limits) -> Result<Contents<R>, Error> {
    Ok(Package::open(src, limits)?.into_contents()?)
}

/// `<stem>-extracted`, shortened to at most 31 UTF-8 bytes (the RPCS3 rule).
pub fn default_folder_name(stem: &str) -> String {
    let keep = stem.floor_char_boundary(MAX_FOLDER_NAME.saturating_sub(FOLDER_SUFFIX.len()));
    let stem = stem
        .get(..keep)
        .unwrap_or_default()
        .trim_end_matches(['.', ' ']);
    let stem = if stem.is_empty() { FALLBACK_STEM } else { stem };
    format!("{stem}{FOLDER_SUFFIX}")
}

/// Extracts `selection` into the new directory `parent/name`.
///
/// Empty, unknown and stale selections fail before any output exists. Cancel is
/// checked before output and between entries and 64 KiB chunks; progress counts
/// decoded bytes of the selected files. On failure after creation the tree stays
/// with its incomplete marker ([`Error::Incomplete`]).
pub fn extract<R: Read + Seek>(
    contents: &mut Contents<R>,
    selection: &Selection,
    parent: &Path,
    name: &str,
    ctx: &Ctx<'_>,
) -> Result<Report, Error> {
    let resolved = contents.tree().resolve(selection)?;
    ctx.cancel.check()?;
    let mut tree = OutputTree::create_new(parent, name)?;
    let path = tree.path().to_path_buf();
    let mut written = 0u64;
    let total = Some(resolved.bytes);
    ctx.progress.report(&Progress {
        phase: Phase::Writing,
        done: 0,
        total,
        item: None,
    });
    let mut buf = vec![0u8; CHUNK];
    let left = |cause: Error| match cause {
        e @ (Error::Incomplete { .. } | Error::Fs(_)) if e.incomplete().is_some() => e,
        cause => Error::Incomplete {
            path: path.clone(),
            cause: Box::new(cause),
        },
    };
    for &index in &resolved.indices {
        ctx.cancel.check().map_err(|e| left(e.into()))?;
        let Some(node) = contents.tree().node(index) else {
            return Err(left(pantheon_package::Error::UnknownIndex(index).into()));
        };
        let (rel, size, kind) = (node.path.clone(), node.size, node.kind);
        if kind == NodeKind::Directory {
            tree.mkdir(&rel)?;
            continue;
        }
        let mut file = tree.create_file(&rel, size, Access::Default)?;
        let mut reader = contents.open_entry(index).map_err(|e| left(e.into()))?;
        loop {
            ctx.cancel.check().map_err(|e| left(e.into()))?;
            let n = reader.read_chunk(&mut buf).map_err(|e| left(e.into()))?;
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                break;
            };
            file.write_all(chunk).map_err(|e| file.error(e))?;
            written = written.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
            ctx.progress.report(&Progress {
                phase: Phase::Writing,
                done: written,
                total,
                item: Some(&rel),
            });
        }
    }
    tree.finish()?;
    Ok(Report {
        path,
        files: resolved.files,
        directories: resolved.directories,
        bytes: written,
    })
}
