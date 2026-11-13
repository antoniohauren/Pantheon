//! The application tree of a GD fake package: the inner PFS (`uroot`) plus the
//! named container records reconstructed under `sce_sys/`.
//!
//! Behavioural reference: pkg-tools `docs/extraction.md` ("Application tree
//! and rejection policy").

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, SeekFrom};

use pantheon_core::{Limit, Limits, Span};

use crate::keys::{self, FAKE_KEY_INDEX, IMAGE_KEY_ID, IMAGE_KEY_LEN, KEYS_ID, KEYS_LEN};
use crate::pfs::{INODE_COMPRESSED, Inode, Pfs, PfsFile, Pfsc, ReadAt, Region};
use crate::{Entry, Error, Header, Package};

/// Game data (`gd`); DLC, themes and others are inspection-only.
const CONTENT_TYPE_GD: u32 = 0x1a;
/// Delta patch content flag.
const CONTENT_FLAG_DELTA: u32 = 0x0100_0000;
/// The PFS image nests an inner image.
const PFS_FLAG_NESTED: u64 = 0x8000_0000_0000_0000;
const OUTER_IMAGE: &str = "pfs_image.dat";
const SCE_SYS: &str = "sce_sys";
/// Records at or above this ID are application metadata; below are container internals.
const APP_METADATA_START: u32 = 0x1000;

type Inner<R> = Pfs<Pfsc<PfsFile<Region<R>>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    Pfs(Inode),
    Record(Entry),
    Directory,
}

/// One application entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Portable `/`-separated path relative to the application root.
    pub path: String,
    /// Decoded bytes; zero for directories.
    pub size: u64,
    source: Source,
}

impl FileEntry {
    pub fn kind(&self) -> FileKind {
        match self.source {
            Source::Directory => FileKind::Directory,
            Source::Pfs(_) | Source::Record(_) => FileKind::File,
        }
    }

    /// The container record ID for a reconstructed `sce_sys` file.
    pub fn record_id(&self) -> Option<u32> {
        match &self.source {
            Source::Record(e) => Some(e.id),
            _ => None,
        }
    }
}

/// The application file name of container record `id`, if it has one.
pub fn record_name(id: u32) -> Option<String> {
    let fixed = match id {
        0x402 => "nptitle.dat",
        0x403 => "npbind.dat",
        0x1000 => "param.sfo",
        0x1001 => "playgo-chunk.dat",
        0x1002 => "playgo-chunk.sha",
        0x1003 => "playgo-manifest.xml",
        0x1004 => "pronunciation.xml",
        0x1005 => "pronunciation.sig",
        0x1006 => "pic1.png",
        0x1007 => "pubtoolinfo.dat",
        0x1008 => "app/playgo-chunk.dat",
        0x1009 => "app/playgo-chunk.sha",
        0x100a => "app/playgo-manifest.xml",
        0x100b => "shareparam.json",
        0x100c => "shareoverlayimage.png",
        0x100d => "save_data.png",
        0x100e => "shareprivacyguardimage.png",
        0x1200 => "icon0.png",
        0x1220 => "pic0.png",
        0x1240 => "snd0.at9",
        0x1260 => "changeinfo/changeinfo.xml",
        0x1280 => "icon0.dds",
        0x12a0 => "pic0.dds",
        0x12c0 => "pic1.dds",
        _ => "",
    };
    if !fixed.is_empty() {
        return Some(fixed.to_owned());
    }
    /// Localized variants: 31 per base record, numbered `00`..`30`.
    const LOCALIZED: u32 = 30;
    for (start, prefix, ext) in [
        (0x1201, "icon0_", "png"),
        (0x1241, "pic1_", "png"),
        (0x1261, "changeinfo/changeinfo_", "xml"),
        (0x1281, "icon0_", "dds"),
        (0x12c1, "pic1_", "dds"),
    ] {
        if let Some(n) = id.checked_sub(start).filter(|n| *n <= LOCALIZED) {
            return Some(format!("{prefix}{n:02}.{ext}"));
        }
    }
    const TROPHY: std::ops::RangeInclusive<u32> = 0x1400..=0x1463;
    if TROPHY.contains(&id) {
        return Some(format!("trophy/trophy{:02}.trp", id.saturating_sub(0x1400)));
    }
    /// Keymap images: groups of 16 IDs, 10 images each.
    const KEYMAP: std::ops::Range<u32> = 0x1600..0x1800;
    const KEYMAP_GROUP: u32 = 16;
    const KEYMAP_IMAGES: u32 = 10;
    if KEYMAP.contains(&id) && id % KEYMAP_GROUP < KEYMAP_IMAGES {
        let group = id.saturating_sub(KEYMAP.start) / KEYMAP_GROUP;
        let image = (id % KEYMAP_GROUP).saturating_add(1);
        return Some(match group.checked_sub(1) {
            None => format!("keymap_rp/{image:03}.png"),
            Some(g) => format!("keymap_rp/{g:02}/{image:03}.png"),
        });
    }
    None
}

/// Validates one application path: ASCII, portable, not a marker name.
fn check_path(path: &str) -> Result<(), Error> {
    if !path.is_ascii() {
        // ASCII avoids filesystem-specific Unicode normalization and case folding.
        return Err(Error::Unsupported("non-ASCII application path"));
    }
    let reason = pantheon_fs::validate_path(path).err().or_else(|| {
        path.split('/')
            .any(|c| c.eq_ignore_ascii_case(pantheon_fs::PKG_TOOLS_MARKER))
            .then_some(pantheon_fs::Reason::ReservedName)
    });
    match reason {
        Some(reason) => Err(Error::UnsafePath {
            path: path.to_owned(),
            reason,
        }),
        None => Ok(()),
    }
}

/// The tree under construction, keyed by ASCII-lowercased path.
struct Tree {
    map: BTreeMap<String, FileEntry>,
    path_bytes: u64,
    limits: Limits,
}

impl Tree {
    fn add(&mut self, path: String, size: u64, source: Source) -> Result<(), Error> {
        check_path(&path)?;
        let key = path.to_ascii_lowercase();
        if let Some(old) = self.map.get(&key) {
            // Only directory reconstruction may repeat, with identical case.
            if old.path == path && source == Source::Directory && old.source == Source::Directory {
                return Ok(());
            }
            return Err(Error::UnsafePath {
                path,
                reason: pantheon_fs::Reason::CaseCollision,
            });
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.add(parent.to_owned(), 0, Source::Directory)?;
        }
        self.path_bytes = self
            .path_bytes
            .saturating_add(u64::try_from(path.len()).unwrap_or(u64::MAX));
        let count = u64::try_from(self.map.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        Limit::new("PS4 tree entries", u64::from(self.limits.max_entries)).check(count)?;
        Limit::new("PS4 path bytes", self.limits.max_path_bytes).check(self.path_bytes)?;
        self.map.insert(key, FileEntry { path, size, source });
        Ok(())
    }
}

/// An opened application tree. Entries keep inode mappings, never payloads.
pub struct Game<R> {
    inner: Inner<R>,
    header: Header,
    dk3: [u8; 32],
    entries: Vec<FileEntry>,
}

impl<R: Read + Seek> Package<R> {
    /// Recovers the public fake-package keys and lists the nested PFS.
    /// Nothing is authenticated beyond the outer PFS header HMAC.
    pub fn into_game(mut self) -> Result<Game<R>, Error> {
        let h = &self.header;
        if h.content_type != CONTENT_TYPE_GD || h.content_flags & CONTENT_FLAG_DELTA != 0 {
            return Err(Error::Unsupported(
                "extraction needs a GD base or full update (no delta, DLC or theme)",
            ));
        }
        if h.pfs_flags & PFS_FLAG_NESTED == 0 {
            return Err(Error::Unsupported("non-nested PFS image"));
        }
        let missing = |id| Error::DecryptionUnavailable {
            entry_id: id,
            key_index: FAKE_KEY_INDEX,
        };
        let keys_entry = self
            .entry(KEYS_ID)
            .copied()
            .ok_or_else(|| missing(KEYS_ID))?;
        let image = self
            .entry(IMAGE_KEY_ID)
            .copied()
            .ok_or_else(|| missing(IMAGE_KEY_ID))?;
        if keys_entry.encrypted()
            || usize::try_from(keys_entry.size).ok() != Some(KEYS_LEN)
            || usize::try_from(image.size).ok() != Some(IMAGE_KEY_LEN)
        {
            return Err(Error::Malformed("fake-key record layout"));
        }
        if !image.encrypted() || image.key_index() != FAKE_KEY_INDEX {
            return Err(Error::DecryptionUnavailable {
                entry_id: IMAGE_KEY_ID,
                key_index: image.key_index(),
            });
        }
        if image.flags1 & keys::NEW_CRYPT_FLAG != 0 {
            return Err(Error::Unsupported("new container encryption algorithm"));
        }
        let mut key_bytes = [0u8; KEYS_LEN];
        let mut image_bytes = [0u8; IMAGE_KEY_LEN];
        self.reader
            .seek(SeekFrom::Start(u64::from(keys_entry.offset)))?;
        self.reader.read_exact(&mut key_bytes)?;
        self.reader.seek(SeekFrom::Start(u64::from(image.offset)))?;
        self.reader.read_exact(&mut image_bytes)?;
        let dk3 = keys::recover_dk3(&key_bytes)?;
        let ekpfs = keys::recover_ekpfs(&mut image_bytes, &image, &dk3)?;

        let span = Span::new(
            self.header.pfs_offset,
            self.header.pfs_size,
            self.header.package_size,
        )
        .map_err(|_| Error::Malformed("PFS image outside package"))?;
        let limits = self.limits;
        let region = Region {
            reader: self.reader,
            span,
        };
        let mut outer = Pfs::open(region, Some(&ekpfs), &limits)?;
        let root = outer.uroot()?;
        let root = outer.inode(root)?;
        let mut images = outer
            .children(&root)?
            .into_iter()
            .filter(|(name, _, dir)| name == OUTER_IMAGE && !dir);
        let number = match (images.next(), images.next()) {
            (Some((_, number, _)), None) => number,
            _ => {
                return Err(Error::Malformed(
                    "outer PFS needs exactly one pfs_image.dat",
                ));
            }
        };
        let inode = outer.inode(number)?;
        if inode.is_dir() {
            return Err(Error::Malformed("outer image inode type"));
        }
        let pfsc = Pfsc::open(PfsFile { pfs: outer, inode })?;
        let mut inner = Pfs::open(pfsc, None, &limits)?;
        let mut tree = Tree {
            map: BTreeMap::new(),
            path_bytes: 0,
            limits,
        };
        walk(&mut inner, &mut tree)?;
        for e in &self.entries {
            if let Some(name) = record_name(e.id) {
                if e.encrypted() && e.flags1 & keys::NEW_CRYPT_FLAG != 0 {
                    return Err(Error::Unsupported("new container encryption algorithm"));
                }
                if e.encrypted() && e.key_index() != FAKE_KEY_INDEX {
                    return Err(Error::DecryptionUnavailable {
                        entry_id: e.id,
                        key_index: e.key_index(),
                    });
                }
                let path = format!("{SCE_SYS}/{name}");
                tree.add(path, u64::from(e.size), Source::Record(*e))?;
            } else if e.id >= APP_METADATA_START {
                return Err(Error::Unsupported("unknown application metadata record ID"));
            }
        }
        Ok(Game {
            inner,
            header: self.header,
            dk3,
            entries: tree.map.into_values().collect(),
        })
    }
}

/// Adds every inner-PFS entry under `uroot` to `tree`.
fn walk<T: ReadAt>(inner: &mut Pfs<T>, tree: &mut Tree) -> Result<(), Error> {
    let mut pending = vec![(String::new(), inner.uroot()?)];
    let mut seen = HashSet::new();
    while let Some((path, number)) = pending.pop() {
        if !seen.insert(number) {
            return Err(Error::Malformed("PFS directory cycle or alias"));
        }
        let dir = inner.inode(number)?;
        let mut names = HashSet::new();
        for (name, number, is_dir) in inner.children(&dir)? {
            let child = if path.is_empty() {
                name
            } else {
                format!("{path}/{name}")
            };
            if !names.insert(child.to_ascii_lowercase()) {
                return Err(Error::UnsafePath {
                    path: child,
                    reason: pantheon_fs::Reason::CaseCollision,
                });
            }
            let inode = inner.inode(number)?;
            if is_dir != inode.is_dir() {
                return Err(Error::Malformed("PFS entry and inode types differ"));
            }
            if is_dir {
                tree.add(child.clone(), 0, Source::Directory)?;
                pending.push((child, number));
            } else {
                if inode.size != inode.compressed_size || inode.flags & INODE_COMPRESSED != 0 {
                    return Err(Error::Unsupported("per-file PFS compression"));
                }
                let size = inode.size;
                tree.add(child, size, Source::Pfs(inode))?;
            }
        }
    }
    Ok(())
}

impl<R: Read + Seek> Game<R> {
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Sorted by ASCII-lowercased path; parents precede their children.
    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    /// Reads decoded bytes of file entry `index` at `offset`; the range must be
    /// inside the file.
    pub fn read_at(&mut self, index: usize, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        let entry = self
            .entries
            .get(index)
            .ok_or(Error::Malformed("application entry index"))?;
        match &entry.source {
            Source::Pfs(inode) => self.inner.read_file(inode, offset, buf),
            Source::Record(e) => {
                let reader = &mut self.inner.source_mut().source_mut().pfs.source_mut().reader;
                keys::read_record(reader, e, &self.dk3, offset, buf)
            }
            Source::Directory => Err(Error::Unsupported("cannot read a directory as a file")),
        }
    }
}
