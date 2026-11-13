//! Streaming ZArchive creation into an [`OutputFile`].

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::PathBuf;

use pantheon_core::{CHUNK, Ctx, Phase, Progress};
use pantheon_crypto::hash::Sha256;
use pantheon_crypto::zstd;
use pantheon_fs::{OutputFile, Reason, ScratchFile};

use crate::format::{
    BLOCK, DATA_ALIGN, FILE_FLAG, Footer, HIGH_MASK, HIGH_SHIFT, LOW_BITS, MAX_LOGICAL, MAX_NAMES,
    MAX_TABLE, NO_NAME, NODE_LEN_U64, RECORD_BLOCKS_U64, RECORD_LEN, RECORD_LEN_U64, Section,
    compare_names,
};
use crate::{Error, check_path};

/// The root directory's node index.
const ROOT: u32 = 0;
/// Zero bytes for block and alignment padding.
static ZEROS: [u8; BLOCK] = [0; BLOCK];

/// What a finished archive holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// The published archive.
    pub path: PathBuf,
    pub files: u64,
    /// Directories below the root.
    pub directories: u64,
    /// Bytes of file payload.
    pub bytes: u64,
    /// Size of the archive itself.
    pub archive_bytes: u64,
}

enum Kind {
    Dir(Vec<u32>),
    File { offset: u64, size: u64 },
}

struct Node {
    name: String,
    kind: Kind,
}

/// Everything appended to the archive passes through here: counted and hashed.
struct Sink {
    out: OutputFile,
    hash: Sha256,
    position: u64,
}

impl Sink {
    fn emit(&mut self, data: &[u8]) -> Result<(), Error> {
        self.out
            .write_all(data)
            .map_err(|e| Error::from(self.out.error(e)))?;
        self.hash.update(data);
        self.position = self
            .position
            .checked_add(u64::try_from(data.len()).map_err(|_| Error::TooLarge("archive size"))?)
            .ok_or(Error::TooLarge("archive size"))?;
        Ok(())
    }
}

/// A streaming ZArchive writer.
///
/// Add directories and files with parents first, then [`Writer::finish`] to
/// publish. File bytes are compressed as they arrive; RAM holds one block, one
/// offset record and the tree metadata (proportional to entry count and name
/// lengths). Offset records spill to a scratch file beside the output (40 bytes
/// per MiB of payload). Any failed addition poisons the writer. Dropping it
/// unfinished removes the scratch output and the spill file; nothing appears
/// under the final name.
pub struct Writer {
    sink: Sink,
    spill: ScratchFile,
    nodes: Vec<Node>,
    /// Directory path to node index.
    dirs: HashMap<String, u32>,
    /// `(parent, ASCII-folded name)` of every node, for collision checks.
    folded: HashSet<(u32, String)>,
    block: Vec<u8>,
    record: [u8; RECORD_LEN],
    blocks: u64,
    logical: u64,
    files: u64,
    poisoned: bool,
    /// The zstd frame of an all-zero block, once one was seen.
    zero_frame: Option<Vec<u8>>,
}

impl Writer {
    /// Starts an archive that [`Writer::finish`] publishes as `out`.
    pub fn create_new(out: OutputFile) -> Result<Writer, Error> {
        let spill = ScratchFile::create_beside(out.path())?;
        Ok(Writer {
            sink: Sink {
                out,
                hash: Sha256::new(),
                position: 0,
            },
            spill,
            nodes: vec![Node {
                name: String::new(),
                kind: Kind::Dir(Vec::new()),
            }],
            dirs: HashMap::new(),
            folded: HashSet::new(),
            block: Vec::with_capacity(BLOCK),
            record: [0; RECORD_LEN],
            blocks: 0,
            logical: 0,
            files: 0,
            poisoned: false,
            zero_frame: None,
        })
    }

    /// Adds the directory `path` (`/`-separated; its parent must exist).
    pub fn add_directory(&mut self, path: &str) -> Result<(), Error> {
        self.guarded(|w| w.insert(path, Kind::Dir(Vec::new())).map(drop))
    }

    /// Adds the file `path`, whose `reader` must yield exactly `size` bytes and
    /// then end. Cancel is checked and progress reported (logical bytes of the
    /// whole archive so far, total unknown) at every 64 KiB block.
    pub fn add_file(
        &mut self,
        path: &str,
        size: u64,
        reader: &mut dyn Read,
        ctx: &Ctx<'_>,
    ) -> Result<(), Error> {
        self.guarded(|w| w.stream_file(path, size, reader, ctx))
    }

    /// Writes the indexes, tree and footer, syncs, and publishes without
    /// overwriting. Not interruptible.
    pub fn finish(mut self) -> Result<Summary, Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.logical == 0 {
            return Err(Error::EmptyPayload);
        }
        if !self.block.is_empty() {
            let pad = BLOCK.saturating_sub(self.block.len());
            self.block
                .extend_from_slice(ZEROS.get(..pad).unwrap_or_default());
            self.store_block()?;
        }
        let data = Section {
            offset: 0,
            size: self.sink.position,
        };
        let misaligned = self.sink.position.checked_rem(DATA_ALIGN).unwrap_or(0);
        if misaligned != 0 {
            let pad = usize::try_from(DATA_ALIGN.saturating_sub(misaligned)).unwrap_or(0);
            self.sink.emit(ZEROS.get(..pad).unwrap_or_default())?;
        }
        let records = self.copy_records()?;
        let order = self.order()?;
        let (names, name_offsets) = self.write_names(&order)?;
        let tree = self.write_tree(&order, &name_offsets)?;
        let empty = Section {
            offset: self.sink.position,
            size: 0,
        };
        let mut footer = Footer {
            data,
            records,
            names,
            tree,
            meta_directory: empty,
            meta_data: empty,
            digest: [0; 32],
            total: self
                .sink
                .position
                .checked_add(crate::format::FOOTER_LEN_U64)
                .ok_or(Error::TooLarge("archive size"))?,
        };
        // The digest covers the whole archive with its own field zeroed.
        let mut hash = std::mem::take(&mut self.sink.hash);
        hash.update(&footer.encode());
        footer.digest = hash.finalize();
        self.sink.emit(&footer.encode())?;
        let directories = u64::try_from(self.dirs.len()).unwrap_or(u64::MAX);
        let Writer {
            sink,
            files,
            logical,
            ..
        } = self;
        let mut out = sink.out;
        let path = out.path().to_path_buf();
        out.sync()?;
        out.publish()?;
        Ok(Summary {
            path,
            files,
            directories,
            bytes: logical,
            archive_bytes: footer.total,
        })
    }

    fn guarded(&mut self, f: impl FnOnce(&mut Writer) -> Result<(), Error>) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let result = f(self);
        self.poisoned = result.is_err();
        result
    }

    /// Validates `path` and adds its node under the existing parent.
    fn insert(&mut self, path: &str, kind: Kind) -> Result<u32, Error> {
        check_path(path)?;
        let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = if parent_path.is_empty() {
            ROOT
        } else {
            *self
                .dirs
                .get(parent_path)
                .ok_or_else(|| Error::MissingParent {
                    path: path.to_owned(),
                })?
        };
        if !self.folded.insert((parent, name.to_ascii_lowercase())) {
            let exact = self.children(parent).iter().any(|&c| {
                self.nodes
                    .get(usize::try_from(c).unwrap_or(usize::MAX))
                    .is_some_and(|n| n.name == name)
            });
            return Err(Error::Name {
                path: path.to_owned(),
                reason: if exact {
                    Reason::Exists
                } else {
                    Reason::CaseCollision
                },
            });
        }
        // The file tree, including the root, must fit a u32-sized section.
        let count = u64::try_from(self.nodes.len())
            .ok()
            .and_then(|n| n.checked_add(1))
            .filter(|n| n.saturating_mul(NODE_LEN_U64) <= MAX_TABLE)
            .ok_or(Error::TooLarge("number of entries"))?;
        let index =
            u32::try_from(count.saturating_sub(1)).map_err(|_| Error::TooLarge("entries"))?;
        let is_dir = matches!(kind, Kind::Dir(_));
        self.nodes.push(Node {
            name: name.to_owned(),
            kind,
        });
        if let Some(Node {
            kind: Kind::Dir(children),
            ..
        }) = self
            .nodes
            .get_mut(usize::try_from(parent).unwrap_or(usize::MAX))
        {
            children.push(index);
        }
        if is_dir {
            self.dirs.insert(path.to_owned(), index);
        }
        Ok(index)
    }

    fn children(&self, dir: u32) -> &[u32] {
        match self.nodes.get(usize::try_from(dir).unwrap_or(usize::MAX)) {
            Some(Node {
                kind: Kind::Dir(children),
                ..
            }) => children,
            _ => &[],
        }
    }

    fn stream_file(
        &mut self,
        path: &str,
        size: u64,
        reader: &mut dyn Read,
        ctx: &Ctx<'_>,
    ) -> Result<(), Error> {
        self.logical
            .checked_add(size)
            .filter(|end| *end <= MAX_LOGICAL)
            .ok_or(Error::TooLarge("total payload over 256 TiB"))?;
        let offset = self.logical;
        self.insert(path, Kind::File { offset, size })?;
        self.files = self.files.saturating_add(1);
        let short = || Error::SizeMismatch {
            path: path.to_owned(),
            expected: size,
        };
        let mut left = size;
        while left > 0 {
            ctx.cancel.check()?;
            let start = self.block.len();
            let room = BLOCK.saturating_sub(start);
            let want = room.min(usize::try_from(left).unwrap_or(usize::MAX));
            self.block.resize(start.saturating_add(want), 0);
            let got = fill(reader, self.block.get_mut(start..).unwrap_or_default())?;
            if got < want {
                return Err(short());
            }
            let step = u64::try_from(want).unwrap_or(u64::MAX);
            left = left.saturating_sub(step);
            self.logical = self.logical.saturating_add(step);
            if self.block.len() == BLOCK {
                self.store_block()?;
            }
            ctx.progress.report(&Progress {
                phase: Phase::Writing,
                done: self.logical,
                total: None,
                item: Some(path),
            });
        }
        let mut probe = [0u8; 1];
        if fill(reader, &mut probe)? != 0 {
            return Err(short());
        }
        Ok(())
    }

    /// Stores the full block raw or as a zstd frame, whichever is smaller.
    fn store_block(&mut self) -> Result<(), Error> {
        let slot =
            usize::try_from(self.blocks.checked_rem(RECORD_BLOCKS_U64).unwrap_or(0)).unwrap_or(0);
        if slot == 0 {
            if self.blocks != 0 {
                self.spill_record()?;
            }
            self.record = [0; RECORD_LEN];
            self.record[..8].copy_from_slice(&self.sink.position.to_be_bytes());
        }
        // Zero blocks (sparse files, padding) are common: compress one once.
        let zero = self.block.iter().all(|b| *b == 0);
        let fresh;
        let packed = match (&self.zero_frame, zero) {
            (Some(frame), true) => frame,
            _ => {
                fresh = zstd::compress(&self.block);
                if zero {
                    self.zero_frame = Some(fresh.clone());
                }
                &fresh
            }
        };
        // A stored size of exactly one block means "raw" to readers.
        let stored = if packed.len() < BLOCK {
            packed
        } else {
            &self.block
        };
        let size_field = u16::try_from(stored.len().saturating_sub(1))
            .map_err(|_| Error::TooLarge("compressed block"))?;
        let at = slot.saturating_mul(2).saturating_add(8);
        if let Some(field) = self.record.get_mut(at..at.saturating_add(2)) {
            field.copy_from_slice(&size_field.to_be_bytes());
        }
        self.sink.emit(stored)?;
        self.blocks = self.blocks.saturating_add(1);
        self.block.clear();
        Ok(())
    }

    fn spill_record(&mut self) -> Result<(), Error> {
        self.spill.write_all(&self.record).map_err(Error::Io)
    }

    /// Streams the spilled offset records (plus the open one) into the archive.
    fn copy_records(&mut self) -> Result<Section, Error> {
        self.spill_record()?;
        self.spill.flush()?;
        let offset = self.sink.position;
        let mut back = self.spill.reopen()?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = fill(&mut back, &mut buf)?;
            if n == 0 {
                break;
            }
            self.sink.emit(buf.get(..n).unwrap_or_default())?;
        }
        let size = self.sink.position.saturating_sub(offset);
        let expected = self
            .blocks
            .div_ceil(RECORD_BLOCKS_U64)
            .saturating_mul(RECORD_LEN_U64);
        if size != expected {
            return Err(Error::Io(io::Error::other(
                "the spilled offset records changed size",
            )));
        }
        if size > MAX_TABLE {
            return Err(Error::TooLarge("offset records"));
        }
        Ok(Section { offset, size })
    }

    /// Breadth-first node order, children sorted the upstream way.
    fn order(&mut self) -> Result<Vec<u32>, Error> {
        let mut order = vec![ROOT];
        let mut next = 0usize;
        while let Some(&index) = order.get(next) {
            next = next.saturating_add(1);
            let at = usize::try_from(index).map_err(|_| Error::TooLarge("entries"))?;
            let mut children = match self.nodes.get_mut(at).map(|n| &mut n.kind) {
                Some(Kind::Dir(children)) => std::mem::take(children),
                _ => continue,
            };
            let name = |i: &u32| {
                self.nodes
                    .get(usize::try_from(*i).unwrap_or(usize::MAX))
                    .map_or("", |n| n.name.as_str())
            };
            children.sort_by(|a, b| compare_names(name(a), name(b)));
            order.extend_from_slice(&children);
            if let Some(Node {
                kind: Kind::Dir(slot),
                ..
            }) = self.nodes.get_mut(at)
            {
                *slot = children;
            }
        }
        Ok(order)
    }

    /// Writes one length-prefixed name per node (BFS order) and returns each
    /// node's name offset, indexed by node.
    fn write_names(&mut self, order: &[u32]) -> Result<(Section, Vec<u32>), Error> {
        let offset = self.sink.position;
        let mut offsets = vec![NO_NAME; self.nodes.len()];
        let mut table = 0u64;
        for &index in order.iter().skip(1) {
            let at = usize::try_from(index).map_err(|_| Error::TooLarge("entries"))?;
            let name = self.nodes.get(at).map_or("", |n| n.name.as_str());
            let len = u8::try_from(name.len()).map_err(|_| Error::TooLarge("name"))?;
            let here = u32::try_from(table)
                .ok()
                .filter(|o| *o < NO_NAME)
                .ok_or(Error::TooLarge("name table"))?;
            if let Some(slot) = offsets.get_mut(at) {
                *slot = here;
            }
            let mut entry = Vec::with_capacity(name.len().saturating_add(1));
            entry.push(len);
            entry.extend_from_slice(name.as_bytes());
            self.sink.emit(&entry)?;
            table = table.saturating_add(u64::try_from(entry.len()).unwrap_or(u64::MAX));
        }
        if table > MAX_NAMES {
            return Err(Error::TooLarge("name table"));
        }
        Ok((
            Section {
                offset,
                size: table,
            },
            offsets,
        ))
    }

    fn write_tree(&mut self, order: &[u32], name_offsets: &[u32]) -> Result<Section, Error> {
        let offset = self.sink.position;
        // Children of each directory start right after all earlier BFS entries.
        let mut first_child = 1u32;
        let mut table = Vec::with_capacity(order.len().saturating_mul(16));
        for &index in order {
            let at = usize::try_from(index).map_err(|_| Error::TooLarge("entries"))?;
            let name = name_offsets.get(at).copied().unwrap_or(NO_NAME);
            let words: [u32; 4] = match self.nodes.get(at).map(|n| &n.kind) {
                Some(Kind::Dir(children)) => {
                    let count =
                        u32::try_from(children.len()).map_err(|_| Error::TooLarge("folder"))?;
                    let start = first_child;
                    first_child = first_child
                        .checked_add(count)
                        .ok_or(Error::TooLarge("entries"))?;
                    [name, start, count, 0]
                }
                Some(Kind::File { offset, size }) => [
                    FILE_FLAG | name,
                    low(*offset),
                    low(*size),
                    (high(*size) << HIGH_SHIFT) | high(*offset),
                ],
                None => return Err(Error::TooLarge("entries")),
            };
            for w in words {
                table.extend_from_slice(&w.to_be_bytes());
            }
        }
        self.sink.emit(&table)?;
        Ok(Section {
            offset,
            size: self.sink.position.saturating_sub(offset),
        })
    }
}

/// Low 32 bits of a 48-bit value.
fn low(v: u64) -> u32 {
    u32::try_from(v & u64::from(u32::MAX)).unwrap_or(0)
}

/// Bits 32..48 of a 48-bit value.
fn high(v: u64) -> u32 {
    u32::try_from((v >> LOW_BITS) & u64::from(HIGH_MASK)).unwrap_or(0)
}

/// Reads until `buf` is full or the reader ends; returns the bytes read.
fn fill(reader: &mut dyn Read, buf: &mut [u8]) -> Result<usize, Error> {
    let mut got = 0usize;
    while let Some(rest) = buf.get_mut(got..).filter(|r| !r.is_empty()) {
        match reader.read(rest) {
            Ok(0) => break,
            Ok(n) => got = got.saturating_add(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(got)
}
