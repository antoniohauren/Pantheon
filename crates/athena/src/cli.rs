//! `athena inspect` and `athena artwork`: argument specs and text/JSON rendering.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::Path;

use pantheon_args::{Command, Invocation, Io, JSON, Opt, Positional, Spec, fail};
use pantheon_core::{Classify, ErrorKind, Limits};
use pantheon_json::JsonWriter;
use pantheon_package::{Artwork, Kind, ps3};

use crate::ops::{self, Details, Inspection, SfoStatus, SfoValue};

/// The `--json` schema of `athena inspect`.
pub const SCHEMA: &str = "pantheon.athena.inspection/1";
const NAME: &str = "athena";

static INSPECT: Spec = Spec {
    verb: "inspect",
    summary: "Show what a PS3/PS4 package or PARAM.SFO is, without changing it",
    positionals: &[Positional {
        name: "INPUT",
        help: "PS3 PKG, PS4 PKG or PARAM.SFO",
    }],
    options: &[JSON],
    one_of: &[],
};

static ARTWORK: Spec = Spec {
    verb: "artwork",
    summary: "Save a package's icon0.png or pic1.png (PS4 only) to a new file",
    positionals: &[
        Positional {
            name: "INPUT",
            help: "PS3 or PS4 PKG",
        },
        Positional {
            name: "OUT.png",
            help: "new file; never overwritten",
        },
    ],
    options: &[
        Opt::flag("icon0", "icon0.png (PS3 ICON0.PNG)"),
        Opt::flag("pic1", "pic1.png (PS4)"),
    ],
    one_of: &["icon0", "pic1"],
};

pub static COMMAND: Command = Command {
    name: NAME,
    summary: "tells you what a package is, without changing anything",
    verbs: &[&INSPECT, &ARTWORK],
};

/// Runs `athena` with `argv` (program name excluded); returns the exit code (D31).
pub fn run(argv: &[OsString], io: &mut Io<'_>) -> u8 {
    let (spec, args) = match pantheon_args::parse(&COMMAND, argv) {
        Ok(Invocation::Run(spec, args)) => (spec, args),
        Ok(Invocation::Help(text)) => {
            return write_or_fail(io, |out| out.write_all(text.as_bytes()));
        }
        Err(e) => return fail(io, NAME, &e),
    };
    // The parser checked the positional count.
    let input = Path::new(args.positional(0).unwrap_or_default());
    let file = match File::open(input) {
        Ok(f) => BufReader::new(f),
        Err(e) => return fail(io, NAME, &OpenError(input.display().to_string(), e)),
    };
    let limits = Limits::DEFAULT;
    if std::ptr::eq(spec, &INSPECT) {
        match ops::inspect(file, &limits) {
            Ok(report) if args.flag("json") => {
                write_or_fail(io, |out| write_json(out, &report, input).map_err(json_io))
            }
            Ok(report) => write_or_fail(io, |out| write_text(out, &report, input)),
            Err(e) => fail(io, NAME, &e),
        }
    } else {
        let which = if args.flag("pic1") {
            Artwork::Pic1
        } else {
            Artwork::Icon0
        };
        let out = Path::new(args.positional(1).unwrap_or_default());
        save_artwork(io, file, which, out, &limits)
    }
}

fn save_artwork(
    io: &mut Io<'_>,
    src: BufReader<File>,
    which: Artwork,
    out: &Path,
    limits: &Limits,
) -> u8 {
    let bytes = match ops::artwork(src, which, limits) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            let _ = writeln!(io.err, "{NAME}: the package has no {}", artwork_name(which));
            return ErrorKind::Unsupported.exit_code();
        }
        Err(e) => return fail(io, NAME, &e),
    };
    let result = pantheon_fs::OutputFile::create_new(out, pantheon_fs::Access::Default).and_then(
        |mut file| {
            file.write_all(&bytes).map_err(|e| file.error(e))?;
            file.publish()
        },
    );
    match result {
        Ok(()) => write_or_fail(io, |o| {
            writeln!(o, "Saved {} to {}", artwork_name(which), out.display())
        }),
        Err(e) => fail(io, NAME, &e),
    }
}

fn artwork_name(which: Artwork) -> &'static str {
    match which {
        Artwork::Icon0 => "icon0.png",
        Artwork::Pic1 => "pic1.png",
    }
}

#[derive(Debug)]
struct OpenError(String, std::io::Error);

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot open {}: {}", self.0, self.1)
    }
}

impl std::error::Error for OpenError {}

impl Classify for OpenError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Io
    }
}

fn json_io(e: pantheon_json::Error) -> std::io::Error {
    match e {
        pantheon_json::Error::Io(e) => e,
        other => std::io::Error::other(other.to_string()),
    }
}

fn write_or_fail(io: &mut Io<'_>, f: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) -> u8 {
    match f(&mut *io.out).and_then(|()| io.out.flush()) {
        Ok(()) => 0,
        Err(e) => fail(io, NAME, &OpenError("stdout".into(), e)),
    }
}

/// Human-readable report.
pub fn write_text(out: &mut dyn Write, r: &Inspection, source: &Path) -> std::io::Result<()> {
    let s = &r.summary;
    let unknown = |v: &Option<String>| v.clone().unwrap_or_else(|| "unknown".into());
    writeln!(out, "Format: {}", r.kind.name())?;
    writeln!(out, "Source: {}", source.display())?;
    writeln!(out, "Title: {}", unknown(&s.title))?;
    writeln!(out, "Title ID: {}", unknown(&s.title_id))?;
    writeln!(out, "Version: {}", unknown(&s.app_version))?;
    match &s.category {
        Some(c) => writeln!(out, "Type: {} ({c})", s.content.name())?,
        None => writeln!(out, "Type: {}", s.content.name())?,
    }
    if let Some(id) = &s.content_id {
        writeln!(out, "Content ID: {id}")?;
    }
    if let Some(size) = s.package_size {
        writeln!(out, "Package size: {size} bytes")?;
    }
    match &r.details {
        Details::Sfo => {}
        Details::Ps4 { header, .. } => {
            writeln!(out, "FPKG: unverified (key recovery not performed)")?;
            writeln!(
                out,
                "Content type: {:#x}; flags: {:#x}",
                header.content_type, header.content_flags
            )?;
            writeln!(out, "Container records: {}", header.entry_count)?;
        }
        Details::Ps3 {
            header, entries, ..
        } => {
            let release = if header.is_retail() {
                "retail"
            } else {
                "debug"
            };
            writeln!(out, "Release: {release}; signatures unverified")?;
            writeln!(
                out,
                "Package entries: {} ({} with implied folders)",
                header.entry_count,
                entries.len()
            )?;
        }
    }
    match &r.sfo {
        SfoStatus::Missing => writeln!(out, "SFO: missing")?,
        SfoStatus::Encrypted { key_index, .. } => {
            writeln!(
                out,
                "SFO: encrypted; decryption material unavailable (key index {key_index})"
            )?;
        }
        SfoStatus::Available(t) => {
            writeln!(out, "SFO: version {:#x}", t.version)?;
            for e in &t.entries {
                let key = match std::str::from_utf8(&e.key) {
                    Ok(k) => format!("{k:?}"),
                    Err(_) => format!("hex:{}", hex(&e.key)),
                };
                let value = match &e.value {
                    SfoValue::Integer(n) => n.to_string(),
                    SfoValue::Text(t) => match std::str::from_utf8(t) {
                        Ok(t) => format!("{t:?}"),
                        Err(_) => format!("hex:{}", hex(&e.raw)),
                    },
                    // shortcut: text output shows at most 64 bytes; --json carries complete hex.
                    SfoValue::Binary | SfoValue::Unknown => {
                        let shown = e.raw.get(..PREVIEW).unwrap_or(&e.raw);
                        let more = if e.raw.len() > PREVIEW { "…" } else { "" };
                        format!("hex:{}{more}", hex(shown))
                    }
                };
                writeln!(out, "  {key}: {value} ({})", value_kind(&e.value))?;
            }
        }
    }
    Ok(())
}

const PREVIEW: usize = 64;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(
        String::with_capacity(bytes.len().saturating_mul(2)),
        |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        },
    )
}

fn value_kind(v: &SfoValue) -> &'static str {
    match v {
        SfoValue::Text(_) => "text",
        SfoValue::Integer(_) => "integer",
        SfoValue::Binary => "binary",
        SfoValue::Unknown => "unknown",
    }
}

type Json<'a> = JsonWriter<&'a mut dyn Write>;
type JResult = Result<(), pantheon_json::Error>;

fn num(w: &mut Json<'_>, key: &str, v: impl Into<u64>) -> JResult {
    w.key(key)?;
    w.u64(v.into())
}

fn text(w: &mut Json<'_>, key: &str, v: Option<&str>) -> JResult {
    w.key(key)?;
    match v {
        Some(s) => w.string(s),
        None => w.null(),
    }
}

fn hex_field(w: &mut Json<'_>, key: &str, bytes: &[u8]) -> JResult {
    w.key(key)?;
    w.hex(bytes)
}

/// The `pantheon.athena.inspection/1` document. Field names follow pkg-tools'
/// `inspect --json`; Pantheon adds `schema`, `source`, `summary` and the PS4
/// mount-image fields (docs/gods/athena.md, field mapping).
pub fn write_json(out: &mut dyn Write, r: &Inspection, source: &Path) -> JResult {
    let mut w = JsonWriter::new(out);
    w.begin_object()?;
    text(&mut w, "schema", Some(SCHEMA))?;
    text(&mut w, "format", Some(r.kind.name()))?;
    text(&mut w, "source", source.to_str())?;
    summary_json(&mut w, r)?;
    match r.kind {
        Kind::Ps4Pkg => text(&mut w, "fpkg_status", Some("unverified"))?,
        Kind::Ps3Pkg => text(&mut w, "authentication", Some("unverified"))?,
        _ => {}
    }
    w.key("sfo")?;
    sfo_json(&mut w, &r.sfo)?;
    match &r.details {
        Details::Sfo => {}
        Details::Ps3 {
            header,
            metadata,
            entries,
        } => ps3_json(&mut w, header, metadata, entries)?,
        Details::Ps4 { header, entries } => {
            w.key("header")?;
            w.begin_object()?;
            num(&mut w, "flags", header.flags)?;
            text(
                &mut w,
                "content_id",
                std::str::from_utf8(header.content_id_bytes()).ok(),
            )?;
            hex_field(&mut w, "content_id_hex", &header.content_id)?;
            num(&mut w, "drm_type", header.drm_type)?;
            num(&mut w, "content_type", header.content_type)?;
            num(&mut w, "content_flags", header.content_flags)?;
            num(&mut w, "entry_count", header.entry_count)?;
            num(&mut w, "entry_table_offset", header.entry_table_offset)?;
            num(&mut w, "body_offset", header.body_offset)?;
            num(&mut w, "body_size", header.body_size)?;
            num(&mut w, "pfs_flags", header.pfs_flags)?;
            num(&mut w, "pfs_offset", header.pfs_offset)?;
            num(&mut w, "pfs_size", header.pfs_size)?;
            num(&mut w, "mount_image_offset", header.mount_image_offset)?;
            num(&mut w, "mount_image_size", header.mount_image_size)?;
            num(&mut w, "package_size", header.package_size)?;
            w.end()?;
            w.key("entries")?;
            w.begin_array()?;
            for e in entries {
                w.begin_object()?;
                num(&mut w, "id", e.id)?;
                num(&mut w, "name_offset", e.name_offset)?;
                num(&mut w, "flags1", e.flags1)?;
                num(&mut w, "flags2", e.flags2)?;
                num(&mut w, "offset", e.offset)?;
                num(&mut w, "size", e.size)?;
                w.key("encrypted")?;
                w.bool(e.encrypted())?;
                num(&mut w, "key_index", e.key_index())?;
                w.end()?;
            }
            w.end()?;
        }
    }
    w.end()?;
    let out = w.finish()?;
    out.write_all(b"\n")?;
    Ok(())
}

fn summary_json(w: &mut Json<'_>, r: &Inspection) -> JResult {
    let s = &r.summary;
    w.key("summary")?;
    w.begin_object()?;
    text(w, "title", s.title.as_deref())?;
    text(w, "title_id", s.title_id.as_deref())?;
    text(w, "app_version", s.app_version.as_deref())?;
    text(w, "category", s.category.as_deref())?;
    text(w, "content", Some(s.content.name()))?;
    w.key("package_size")?;
    match s.package_size {
        Some(n) => w.u64(n)?,
        None => w.null()?,
    }
    text(w, "content_id", s.content_id.as_deref())?;
    w.end()
}

fn sfo_json(w: &mut Json<'_>, sfo: &SfoStatus) -> JResult {
    w.begin_object()?;
    match sfo {
        SfoStatus::Missing => text(w, "status", Some("missing"))?,
        SfoStatus::Encrypted {
            entry_id,
            key_index,
        } => {
            text(w, "status", Some("decryption_unavailable"))?;
            num(w, "entry_id", *entry_id)?;
            num(w, "key_index", *key_index)?;
        }
        SfoStatus::Available(t) => {
            text(w, "status", Some("available"))?;
            num(w, "version", t.version)?;
            w.key("entries")?;
            w.begin_array()?;
            for e in &t.entries {
                w.begin_object()?;
                text(w, "key", std::str::from_utf8(&e.key).ok())?;
                hex_field(w, "key_hex", &e.key)?;
                num(w, "format", e.format)?;
                text(w, "kind", Some(value_kind(&e.value)))?;
                w.key("value")?;
                match &e.value {
                    SfoValue::Integer(n) => w.u64(u64::from(*n))?,
                    SfoValue::Text(t) => match std::str::from_utf8(t) {
                        Ok(s) => w.string(s)?,
                        Err(_) => w.null()?,
                    },
                    SfoValue::Binary | SfoValue::Unknown => w.null()?,
                }
                hex_field(w, "raw_hex", &e.raw)?;
                num(w, "length", u64::try_from(e.raw.len()).unwrap_or(u64::MAX))?;
                num(w, "max_length", e.max_len)?;
                hex_field(w, "padding_hex", &e.padding)?;
                w.end()?;
            }
            w.end()?;
        }
    }
    w.end()
}

fn ps3_json(
    w: &mut Json<'_>,
    h: &ps3::Header,
    metadata: &[ps3::Metadata],
    entries: &[ps3::FileEntry],
) -> JResult {
    w.key("header")?;
    w.begin_object()?;
    num(w, "release_type", h.release_type)?;
    num(w, "platform", h.platform)?;
    text(
        w,
        "content_id",
        std::str::from_utf8(h.content_id_bytes()).ok(),
    )?;
    hex_field(w, "content_id_hex", &h.content_id)?;
    num(w, "package_size", h.package_size)?;
    num(w, "entry_count", h.entry_count)?;
    num(w, "metadata_offset", h.metadata_offset)?;
    num(w, "metadata_count", h.metadata_count)?;
    num(w, "metadata_size", h.metadata_size)?;
    num(w, "data_offset", h.data_offset)?;
    num(w, "data_size", h.data_size)?;
    hex_field(w, "qa_digest_hex", &h.qa_digest)?;
    hex_field(w, "nonce_hex", &h.nonce)?;
    w.end()?;
    w.key("metadata")?;
    w.begin_array()?;
    for m in metadata {
        w.begin_object()?;
        num(w, "id", m.id)?;
        hex_field(w, "raw_hex", &m.data)?;
        w.end()?;
    }
    w.end()?;
    w.key("entries")?;
    w.begin_array()?;
    for e in entries {
        w.begin_object()?;
        text(w, "path", Some(&e.path))?;
        num(w, "offset", e.offset)?;
        num(w, "size", e.size)?;
        num(w, "flags", e.flags)?;
        let kind = match e.kind() {
            ps3::FileKind::Directory => "directory",
            ps3::FileKind::File => "file",
        };
        text(w, "kind", Some(kind))?;
        w.end()?;
    }
    w.end()
}
