//! Proteus operations: pack a folder, or convert a PS4 base (and optional
//! update) package, into ZArchive files.

use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use pantheon_core::{Classify, Ctx, ErrorKind, Limits, Progress, ProgressSink};
use pantheon_fs::{Access, EntryKind, OutputFile, SourceSnapshot, SourceTree};
use pantheon_package::{Contents, Kind, NodeKind, Package, Tree};
use pantheon_zar::{Summary, Writer, check_path};

/// Conversion output extension (lowercase, as shadPS4 looks for it).
const ZAR_EXTENSION: &str = "zar";
/// Appended to the base output's stem for the update archive.
const UPDATE_SUFFIX: &str = "-UPDATE.zar";
/// SFO `CATEGORY` of a PS4 game base.
const CATEGORY_BASE: &str = "gd";
/// SFO `CATEGORY` of a PS4 game update.
const CATEGORY_UPDATE: &str = "gp";
/// PS4 title IDs are `CUSA` plus five digits.
const TITLE_PREFIX: &str = "CUSA";
const TITLE_LEN: usize = 9;
/// Where the title ID sits inside a content ID (`UP0000-CUSA00000_00-…`).
const CONTENT_TITLE_START: usize = 7;
const CONTENT_TITLE_END: usize = CONTENT_TITLE_START + TITLE_LEN;

/// What one archive holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The published archive.
    pub path: PathBuf,
    pub files: u64,
    pub directories: u64,
    /// File payload bytes.
    pub bytes: u64,
}

impl From<Summary> for Report {
    fn from(s: Summary) -> Self {
        Report {
            path: s.path,
            files: s.files,
            directories: s.directories,
            bytes: s.bytes,
        }
    }
}

/// The archives a conversion published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConvertReport {
    pub base: Report,
    pub update: Option<Report>,
}

#[derive(Debug)]
pub enum Error {
    Zar(pantheon_zar::Error),
    Fs(pantheon_fs::Error),
    Package(pantheon_package::Error),
    Sfo(pantheon_psf::Error),
    /// The input is outside what conversion supports.
    Unsupported(String),
    /// The output name is not acceptable.
    OutputName(String),
    /// The output would land inside the folder being packed.
    InsideSource(PathBuf),
    Open {
        path: PathBuf,
        source: io::Error,
    },
    Cancelled,
    /// The base archive was published, then the update failed (P13). The pair
    /// is not one atomic publication.
    UpdateFailed {
        base: PathBuf,
        cause: Box<Error>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Zar(e) => e.fmt(f),
            Error::Fs(e) => e.fmt(f),
            Error::Package(e) => e.fmt(f),
            Error::Sfo(e) => e.fmt(f),
            Error::Unsupported(why) => f.write_str(why),
            Error::OutputName(why) => f.write_str(why),
            Error::InsideSource(path) => write!(
                f,
                "the output {} is inside the folder being packed",
                path.display()
            ),
            Error::Open { path, source } => write!(f, "cannot open {}: {source}", path.display()),
            Error::Cancelled => f.write_str("cancelled; unpublished archives were removed"),
            Error::UpdateFailed { base, cause } => write!(
                f,
                "{cause}; the base archive was published at {} but the update archive was not",
                base.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Zar(e) => Some(e),
            Error::Fs(e) => Some(e),
            Error::Package(e) => Some(e),
            Error::Sfo(e) => Some(e),
            Error::Open { source, .. } => Some(source),
            Error::UpdateFailed { cause, .. } => Some(cause.as_ref()),
            _ => None,
        }
    }
}

impl Classify for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Error::Zar(e) => e.kind(),
            Error::Fs(e) => e.kind(),
            Error::Package(e) => e.kind(),
            Error::Sfo(e) => e.kind(),
            Error::Unsupported(_) => ErrorKind::Unsupported,
            Error::OutputName(_) => ErrorKind::Usage,
            Error::InsideSource(_) => ErrorKind::UnsafeOutput,
            Error::Open { .. } => ErrorKind::Io,
            Error::Cancelled => ErrorKind::Cancelled,
            Error::UpdateFailed { .. } => ErrorKind::Partial,
        }
    }

    fn incomplete(&self) -> Option<&Path> {
        match self {
            Error::Zar(e) => e.incomplete(),
            Error::Fs(e) => e.incomplete(),
            Error::UpdateFailed { cause, .. } => cause.incomplete(),
            _ => None,
        }
    }
}

impl From<pantheon_zar::Error> for Error {
    fn from(e: pantheon_zar::Error) -> Self {
        match e {
            pantheon_zar::Error::Cancelled => Error::Cancelled,
            e => Error::Zar(e),
        }
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

impl From<pantheon_core::Cancelled> for Error {
    fn from(_: pantheon_core::Cancelled) -> Self {
        Error::Cancelled
    }
}

/// Reports progress of one archive as part of a larger total.
struct Offset<'a> {
    inner: &'a dyn ProgressSink,
    before: u64,
    total: u64,
}

impl ProgressSink for Offset<'_> {
    fn report(&self, p: &Progress<'_>) {
        self.inner.report(&Progress {
            done: self.before.saturating_add(p.done),
            total: Some(self.total),
            ..*p
        });
    }
}

/// Packs the folder `src` into the new archive `out`.
///
/// The source must stay stable: each file is snapshotted before any output
/// exists and rechecked when opened and before publication. Symlinks, special
/// files, unsafe names and case collisions are refused, as are names over 127
/// bytes and trees without any file payload. Cancel removes the unpublished
/// scratch; publication itself cannot be interrupted.
pub fn pack(src: &Path, out: &Path, ctx: &Ctx<'_>) -> Result<Report, Error> {
    let tree = SourceTree::open(src)?;
    refuse_inside(src, out)?;
    let mut files: Vec<(&str, u64, SourceSnapshot)> = Vec::new();
    for e in tree.entries() {
        check_path(&e.path)?;
        if e.kind == EntryKind::File {
            let meta = tree
                .open_file(&e.path)?
                .metadata()
                .map_err(|source| Error::Open {
                    path: src.join(&e.path),
                    source,
                })?;
            files.push((&e.path, meta.len(), SourceSnapshot::of(&meta)));
        }
    }
    let total = files.iter().fold(0u64, |t, f| t.saturating_add(f.1));
    if total == 0 {
        return Err(pantheon_zar::Error::EmptyPayload.into());
    }
    ctx.cancel.check()?;
    let mut w = Writer::create_new(OutputFile::create_new(out, Access::Default)?)?;
    let sink = Offset {
        inner: ctx.progress,
        before: 0,
        total,
    };
    let inner = Ctx::new(ctx.cancel, &sink);
    let mut pending = files.iter();
    for e in tree.entries() {
        if e.kind == EntryKind::Dir {
            w.add_directory(&e.path)?;
            continue;
        }
        let Some(&(rel, size, snapshot)) = pending.next() else {
            break;
        };
        let mut file = tree.open_file(rel)?;
        let now = file.metadata().map_err(|source| Error::Open {
            path: src.join(rel),
            source,
        })?;
        if SourceSnapshot::of(&now) != snapshot {
            return Err(pantheon_fs::Error::SourceChanged {
                path: src.join(rel),
            }
            .into());
        }
        w.add_file(rel, size, &mut file, &inner)?;
    }
    for (rel, _, snapshot) in &files {
        tree.recheck(rel, snapshot)?;
    }
    Ok(w.finish()?.into())
}

/// The sibling update archive of `out`: `<stem>-UPDATE.zar`. `out` must end in
/// lowercase `.zar`.
pub fn update_path(out: &Path) -> Result<PathBuf, Error> {
    if out.extension() != Some(OsStr::new(ZAR_EXTENSION)) {
        return Err(Error::OutputName(format!(
            "conversion output must end in lowercase .{ZAR_EXTENSION}: {}",
            out.display()
        )));
    }
    let stem = out
        .file_stem()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::OutputName(format!("output has no name: {}", out.display())))?;
    let mut name = stem.to_os_string();
    name.push(UPDATE_SUFFIX);
    Ok(out.with_file_name(name))
}

/// Converts the PS4 base package `base` into the new archive `out`, streaming
/// entries straight from the package (no extracted tree). With `update`, also
/// writes the full update as [`update_path`]`(out)`; both packages are checked
/// (base `CATEGORY=gd`, update `gp`, identical title and content IDs) and
/// both outputs reserved before any payload is streamed, and both are streamed
/// before either is published. Publication is per file: if the update fails
/// after the base was published, the base stays and [`Error::UpdateFailed`]
/// names it.
pub fn convert(
    base: &Path,
    update: Option<&Path>,
    out: &Path,
    ctx: &Ctx<'_>,
) -> Result<ConvertReport, Error> {
    let update_out = update_path(out)?;
    let (mut base_pkg, base_id) = open_ps4(base)?;
    let mut update_pkg = update.map(open_ps4).transpose()?;
    check_identities(&base_id, update_pkg.as_ref().map(|(_, id)| id))?;
    let base_bytes = check_tree(base_pkg.tree())?;
    let update_bytes = match &update_pkg {
        Some((c, _)) => check_tree(c.tree())?,
        None => 0,
    };
    ctx.cancel.check()?;
    let base_file = OutputFile::create_new(out, Access::Default)?;
    let update_file = match update_pkg {
        Some(_) => Some(OutputFile::create_new(&update_out, Access::Default)?),
        None => None,
    };
    let total = base_bytes.saturating_add(update_bytes);
    let mut base_writer = Writer::create_new(base_file)?;
    stream(&mut base_pkg, &mut base_writer, ctx, 0, total)?;
    let update_writer = match (update_file, &mut update_pkg) {
        (Some(file), Some((pkg, _))) => {
            let mut w = Writer::create_new(file)?;
            stream(pkg, &mut w, ctx, base_bytes, total)?;
            Some(w)
        }
        _ => None,
    };
    let base_report = Report::from(base_writer.finish()?);
    let update_report = match update_writer {
        Some(w) => Some(Report::from(w.finish().map_err(|e| {
            Error::UpdateFailed {
                base: base_report.path.clone(),
                cause: Box::new(e.into()),
            }
        })?)),
        None => None,
    };
    Ok(ConvertReport {
        base: base_report,
        update: update_report,
    })
}

/// `CATEGORY`, `TITLE_ID` and `CONTENT_ID` from PARAM.SFO.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    category: String,
    title: String,
    content: String,
}

type Opened = (Contents<BufReader<File>>, Identity);

fn open_ps4(path: &Path) -> Result<Opened, Error> {
    let file = File::open(path).map_err(|source| Error::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let mut pkg = Package::open(BufReader::new(file), &Limits::DEFAULT)?;
    if pkg.kind() != Kind::Ps4Pkg {
        return Err(Error::Unsupported(format!(
            "{}: only PS4 packages convert to ZArchive",
            path.display()
        )));
    }
    let id = identity(&mut pkg, path)?;
    Ok((pkg.into_contents()?, id))
}

fn identity<R: Read + Seek>(pkg: &mut Package<R>, path: &Path) -> Result<Identity, Error> {
    let unsupported = |why: &str| Error::Unsupported(format!("{}: {why}", path.display()));
    let bytes = pkg
        .read_param_sfo()?
        .ok_or_else(|| unsupported("conversion needs a plaintext PARAM.SFO"))?;
    let sfo = pantheon_psf::parse(&bytes, &Limits::DEFAULT)?;
    let text = |key: &str| {
        sfo.text(key.as_bytes())
            .filter(|t| !t.is_empty() && t.is_ascii())
            .map(str::to_owned)
            .ok_or_else(|| unsupported(&format!("missing or invalid SFO {key}")))
    };
    let id = Identity {
        category: text("CATEGORY")?,
        title: text("TITLE_ID")?,
        content: text("CONTENT_ID")?,
    };
    if pkg.content_id() != id.content.as_bytes() {
        return Err(unsupported("PKG header and SFO content IDs differ"));
    }
    Ok(id)
}

fn check_identities(base: &Identity, update: Option<&Identity>) -> Result<(), Error> {
    if base.category != CATEGORY_BASE {
        return Err(Error::Unsupported(format!(
            "the base must be a PS4 game (SFO CATEGORY={CATEGORY_BASE})"
        )));
    }
    let title_ok = base.title.len() == TITLE_LEN
        && base.title.starts_with(TITLE_PREFIX)
        && base
            .title
            .get(TITLE_PREFIX.len()..)
            .is_some_and(|d| d.bytes().all(|b| b.is_ascii_digit()))
        && base.content.get(CONTENT_TITLE_START..CONTENT_TITLE_END) == Some(base.title.as_str());
    if !title_ok {
        return Err(Error::Unsupported(
            "the base title ID is not CUSA plus five digits matching its content ID".into(),
        ));
    }
    if let Some(update) = update {
        if update.category != CATEGORY_UPDATE {
            return Err(Error::Unsupported(format!(
                "--update must be a PS4 game update (SFO CATEGORY={CATEGORY_UPDATE})"
            )));
        }
        if update.title != base.title || update.content != base.content {
            return Err(Error::Unsupported(
                "the base and update title/content IDs do not match".into(),
            ));
        }
    }
    Ok(())
}

/// Checks every path against the archive name rules and returns the payload
/// bytes; an all-empty payload is refused before any output exists.
fn check_tree(tree: &Tree) -> Result<u64, Error> {
    let mut bytes = 0u64;
    for n in tree.nodes() {
        check_path(&n.path)?;
        bytes = bytes.saturating_add(n.size);
    }
    if bytes == 0 {
        return Err(pantheon_zar::Error::EmptyPayload.into());
    }
    Ok(bytes)
}

fn stream<R: Read + Seek>(
    contents: &mut Contents<R>,
    w: &mut Writer,
    ctx: &Ctx<'_>,
    before: u64,
    total: u64,
) -> Result<(), Error> {
    let sink = Offset {
        inner: ctx.progress,
        before,
        total,
    };
    let inner = Ctx::new(ctx.cancel, &sink);
    let items: Vec<(String, NodeKind, u64)> = contents
        .tree()
        .nodes()
        .iter()
        .map(|n| (n.path.clone(), n.kind, n.size))
        .collect();
    for (index, (path, kind, size)) in (0u32..).zip(items) {
        ctx.cancel.check()?;
        match kind {
            NodeKind::Directory => w.add_directory(&path)?,
            NodeKind::File => {
                let mut entry = contents.open_entry(index)?;
                w.add_file(&path, size, &mut entry, &inner)?;
            }
        }
    }
    Ok(())
}

/// Refuses an output whose parent lies inside `src` (it would pack itself).
fn refuse_inside(src: &Path, out: &Path) -> Result<(), Error> {
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    // A parent that cannot be resolved is refused later by `OutputFile`.
    if let (Ok(src), Ok(parent)) = (src.canonicalize(), parent.canonicalize())
        && parent.starts_with(&src)
    {
        return Err(Error::InsideSource(out.to_path_buf()));
    }
    Ok(())
}
