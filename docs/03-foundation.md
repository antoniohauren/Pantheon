# Foundation crates (L0)

These five crates are the "strong foundation" (D7). Every later phase depends on
them, so they are designed, tested and frozen first (Phase 1). Changes to them
after Phase 1 need a note in the decision log.

---

## `pantheon-core`

Standard library only.

### Checked arithmetic and ranges

```rust
pub struct Span { pub offset: u64, pub len: u64 }   // validated: offset + len <= limit, no overflow
impl Span {
    pub fn new(offset: u64, len: u64, within: u64) -> Result<Span, RangeError>;
    pub fn sub(&self, offset: u64, len: u64) -> Result<Span, RangeError>;
    pub fn overlaps(&self, other: &Span) -> bool;
}
pub fn align_up(value: u64, align: u64) -> Result<u64, RangeError>;
pub fn usize_len(len: u64, cap: Limit) -> Result<usize, LimitError>; // before any Vec::with_capacity
```

Format crates **never** do `as usize` on input values, and never use `+`/`*` on
offsets without these helpers. Clippy `arithmetic_side_effects` and
`cast_possible_truncation` are set to `deny` in L0/L1 crates (D30).

### Binary readers

- `Le`/`Be` helpers: `u16_le(&[u8], at)` and friends, returning `Result` and never panicking.
- `BoundedRead<R>`: wraps a `Read` and refuses to read past a `Span`.
- `SpanReader<R: Read + Seek>`: presents one `Span` of a seekable source as its own
  `Read + Seek`, and is the base of every "read entry" API.
- `read_exact_capped(reader, len, limit) -> Vec<u8>`: the only sanctioned way to
  buffer a whole metadata record.

### Limits

```rust
pub struct Limits { pub max_entries: u32, pub max_path_bytes: u64, pub max_record: u64, … }
impl Limits { pub const DEFAULT: Limits = …; pub fn lowered(self, f: impl FnOnce(&mut Limits)) -> Limits; }
```

Defaults carry over the pkg-tools values. Examples: 1,000,000 tree entries, 64 MiB
of path bytes, 64 MiB of directory data, 16 MiB SFO, 65,536 container/SFO records,
4 MiB artwork/XML, 64 MiB per TRP and 128 MiB combined. Each format documents
which fields use which limit.

### Cancellation, progress, error kinds

`Cancel`, `ProgressSink`, `Progress`, `Phase` and `ErrorKind` are defined here (see
[architecture §4](01-architecture.md#4-shared-contracts)). `Cancel::check()` returns
`Err(Cancelled)` so the `?` operator can be used at every chunk boundary.

### Testing helpers (feature `testing`)

- `SplitMix64` seeded PRNG.
- `Corruptor`: truncation, bit flips and length inflation over a byte buffer,
  used by every format's robustness tests.
- `VecSink`, a progress sink that records events for assertions.

---

## `pantheon-fs`

Owns **every** write to the host filesystem, and the safety-relevant reads of
source folders. It is the only crate with platform `cfg` blocks.

### 1. Name rules (portable, always on)

A package path component is accepted only if it:

- is non-empty, not `.` or `..`, and contains no `/`, `\` or NUL;
- is at most 255 bytes and is valid UTF-8 (each format applies its own stricter ASCII rule);
- is not a Windows reserved device name (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
  `LPT1`–`LPT9`, with or without extension, case-insensitive);
- does not end in `.` or a space, and contains no `<>:"|?*` or control characters;
- does not collide, ignoring ASCII case, with a sibling already in the same tree;
- is not a reserved Pantheon name: `.pantheon-incomplete` or a name matching the
  scratch patterns below (compared ignoring ASCII case). A package entry with such
  a name is rejected as `UnsafeOutput`, so it can never replace or hide a marker.

Paths are at most 4,096 bytes and 128 components. These rules are pkg-tools'
`pkg-fs` policy, plus Windows rules made explicit.

**D32:** a separate RPCS3 rule, where generated top-level folder names
are capped at 31 UTF-8 bytes with the suffix preserved. It is applied by the
gods that generate names (Hades, Dionysus), not by the general validator.

### 2. Output trees

```rust
let tree = OutputTree::create_new(parent: &Path, name: &str)?; // fails if exists; parent must exist
tree.mkdir(rel)?;                       // parents-first, validated, no-follow
let mut f = tree.create_file(rel, size_hint, Access::Default)?;  // create_new semantics, returns impl Write
tree.finish()?;                         // removes the incomplete marker
```

- `Access` is `Default` or `OwnerOnly`. `OwnerOnly` means mode `0600` on Unix,
  set at creation, never later with `chmod`. On Windows it is an owner-only ACL,
  pending the Phase 1 spike. Until that spike passes, `OwnerOnly` on Windows
  fails with `Unsupported` rather than quietly writing a readable file. Hephaestus
  uses it for GP4 output.
- **P15:** `create_new` creates a scratch directory
  `.<name>.pantheon-<random>.partial` in the parent, writes the marker
  `.pantheon-incomplete` inside it **first**, then renames the directory to `name`
  without replacing anything (`renameat2(RENAME_NOREPLACE)` on Linux,
  `renamex_np(RENAME_EXCL)` on macOS, `MoveFileExW` without
  `MOVEFILE_REPLACE_EXISTING` on Windows). A crash therefore never leaves an
  unmarked directory under the final name. Where the no-replace rename is
  unsupported, `create_new` falls back to creating `name` directly and writing
  the marker immediately. The Phase 1 spike records which filesystems take the
  fallback. On Windows, `std::fs::rename` replaces, so the no-replace rename is
  one `MoveFileExW` call through `windows-sys` (D22). It lives in
  `pantheon-fs/src/sys/windows.rs` as the crate's only `unsafe` block, with a
  `// SAFETY:` comment and its own tests. **Spike check:** confirm that `rustix`'s
  `renameat_with` covers both the Linux and the macOS flags.
- Every operation is relative to the tree's root handle:
  - Unix: a directory file descriptor; `mkdirat`/`openat` with `O_NOFOLLOW | O_CLOEXEC`,
    then `fstat` to confirm the type.
  - Windows: paths built under the root with `\\?\`, opened with
    `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`, then metadata is
    checked and anything that is a reparse point is rejected. This is **weaker
    than Unix**: `std` has no handle-relative open on Windows, so every component
    is re-checked by path, and a parent swapped between the check and the open is
    caught only if it becomes a reparse point. The docs must not claim Unix-level
    race safety on Windows. **Phase 1 spike:** whether `std` alone (via
    `std::os::windows::fs::OpenOptionsExt::custom_flags`) is enough, or a small
    Windows crate is needed (decision P5).
- The destination parent chain is checked for symlinks before creation.
- Errors after `create_new` return `Incomplete { path }`, and the marker remains.
  Nothing we created is deleted on error, because the user may want to inspect it.
  Nothing we did not create is ever touched.
- Pantheon refuses to write into a tree that has a marker, and says why.
  pkg-tools' marker name `.pkg-tools-incomplete` is also recognised as
  "incomplete" when reading folders as input.

### 3. Output files (single-file publication)

```rust
let out = OutputFile::create_new(final_path, Access::Default)?;  // checks final does not exist; creates scratch sibling
out.write_all(..)?; out.sync()?;
out.publish()?;                                  // no-overwrite
```

- Scratch: `.<name>.pantheon-<random>.partial` in the same directory, created with
  `create_new`. The random part comes from `os_random` (below). `create_new`
  already guarantees uniqueness, so the randomness only avoids retries.
- Publish: a hard link from scratch to final, which fails if `final` exists.
  On Unix it is `linkat` relative to the parent directory descriptor that was
  opened and checked at `create_new`, never a second path lookup. On Windows it is
  `std::fs::hard_link` on the `\\?\` path. Then the scratch file is removed.
- **Fallback 1 for filesystems without hard links** (FAT32/exFAT, which are common
  for PS4 USB drives, and some network shares): the no-replace rename used for
  trees (P15). The final name then appears only once the file is complete.
- **Fallback 2**, only where neither works: write directly to `final` opened with
  `create_new`. A failure then leaves `final` in place. It is reported as
  `Incomplete`, and the name is not reused. Accepted by the owner (D28), knowing
  that this fallback means a partial file can carry the final name.
  The Phase 1 spike records which filesystems reach fallback 2.
- A fallback is taken only for the specific "not supported" errors (for example
  `EPERM`/`EOPNOTSUPP`/`EXDEV` on Unix, `ERROR_INVALID_FUNCTION`/`ERROR_NOT_SUPPORTED`
  on Windows). An "already exists" error is always `UnsafeOutput`, never a fallback.
- `drop` without `publish` removes the scratch file (best effort). It never removes `final`.
- Multi-file publication (Proteus base + update) is **not atomic**. The caller
  publishes in a documented order and reports which files completed (pkg-tools
  behaviour).

### 4. Source stability

Long operations read sources several times (for example, Hephaestus validates
and then writes). `SourceSnapshot` records `(len, mtime, inode/file-id)` per file at the
start and re-checks them before publication. This is **not** a guarantee against
hostile concurrent mutation, and the documents must keep saying so. Content
re-verification (hash or byte compare) is the format layer's job where needed.

### 5. OS randomness

`pub fn os_random(buf: &mut [u8]) -> Result<(), Error>` fills `buf` from the OS
cryptographic random source (D26): `/dev/urandom` opened with std on Unix, and
`BCryptGenRandom` with `BCRYPT_USE_SYSTEM_PREFERRED_RNG` on Windows (the second
audited `unsafe` call). It never falls back to a weaker source. If the OS source
fails, the caller gets an `Io` error. Users: Hermes session tokens and scratch
names. It lives here because `pantheon-fs` is the one crate with platform code.

### 6. Reading source folders (Dionysus folder input, Hephaestus GP4 roots, Proteus `pack`)

`SourceTree::open(root)` walks with no-follow opens, and rejects symlinks,
special files, incomplete markers, unsafe names and case collisions. It yields
entries in a deterministic order (byte-wise sorted).

---

## `pantheon-crypto`

The only crate allowed to depend on crypto/compression crates (P6). It exposes
**narrow, purpose-named** functions, not a general crypto API:

```rust
pub mod aes { pub fn cbc_decrypt_in_place(key: &[u8;16], iv: &[u8;16], buf: &mut [u8]) -> Result<()>;
              pub fn ctr_apply(key: &[u8;16], iv: [u8;16], offset: u64, buf: &mut [u8]);
              pub struct Xts128 { … } /* sector encrypt/decrypt */ }
pub mod hash { pub struct Sha1; pub struct Sha256; pub fn hmac_sha1(..); pub fn hmac_sha256(..); }
pub mod rsa  { pub fn fake_unwrap(..); pub fn public_wrap(..); }
pub mod ecdsa_npdrm { pub fn sign(..); pub fn verify(..); }   // curve arithmetic over num-bigint-dig
pub mod zlib { pub struct Inflater; pub fn deflate(..); }
pub mod zstd { pub struct Encoder; pub struct Decoder; }
```

- Every function has known-answer tests that use **public** vectors (NIST/RFC, or
  published PS3/PS4 community vectors with their source recorded).
- **Key material is not in this crate.** Fixed public keys (fake PKG keys, NPDRM
  curve parameters, ESFM keys) live next to the format that uses them, with their
  provenance documented as in pkg-tools' `docs/fpkg-authoring-provenance.md`.
- `zeroize` on key buffers where the crates support it. This is hygiene, not a
  security boundary.

---

## `pantheon-json`

A streaming writer for CLI `--json` output and for machine-readable reports.

```rust
let mut w = JsonWriter::new(&mut stdout);
w.begin_object()?; w.key("title_id")?; w.string("CUSA00001")?; w.key("size")?; w.u64(1234)?; w.end()?;
```

- Escapes per RFC 8259, never emits invalid UTF-8 (byte strings go to `hex` or
  `null` fields, as in pkg-tools), checks nesting, and writes integers exactly.
- **Stable schemas.** Each god documents its JSON output, and the output carries
  `"schema": "pantheon.<god>.<thing>/1"`. A schema change means a version bump.
- No parser. If one is ever needed (e.g. reading back a report), it is added here
  with fuzzing.

---

## `pantheon-xml`

Used by GP4 (read/write), trophy configuration (`TROPCONF.SFM`, `TROP*.SFM`, ESFM
plaintext), and the PlayGo manifest.

- **Reader:** pull parser over `&[u8]` with UTF-8 validation. Supports the XML
  declaration, elements, attributes, text, CDATA, comments (skipped), the five
  predefined entities, and decimal/hex character references.
  **Rejects** `<!DOCTYPE`, unknown entities, unbalanced tags and duplicate attributes.
  A leading UTF-8 BOM is skipped. Any other encoding declaration than UTF-8 is
  rejected. Processing instructions other than the XML declaration are skipped.
- **Limits:** input ≤ 4 MiB (configurable lower), depth ≤ 64, attributes per
  element ≤ 64, names ≤ 256 bytes, and total nodes ≤ 1,000,000.
- **Writer:** indentation-controlled element/attribute/text writer with escaping.
  The GP4 writer must reproduce pkg-tools' byte layout for the supported subset
  (a differential test).
- Differential-tested against `roxmltree` (dev-only) on every fixture and fuzz
  corpus input, using the one-sided rule in
  [dependencies §3](02-dependencies.md#3-what-is-removed-compared-with-pkg-tools).

---

## Phase 1 exit criteria for the foundation

**D35:** these are checked by local runs, not GitHub CI. Phase 1 is accepted on
Linux; the Windows and macOS items stay open (unclaimed) until run on those OSes,
and running cargo-fuzz is deferred until a nightly toolchain is available.

- All five crates build and pass their tests on Linux, Windows and macOS CI.
- `pantheon-fs` has passing adversarial tests on every OS: symlink and junction
  in the destination parent, existing output, case collision, reserved names,
  marker present, reserved names inside a package (`.pantheon-incomplete`),
  hard-link-less filesystem (simulated by injecting the error), no-replace rename
  unsupported, `Access::OwnerOnly` mode check, a crash between scratch creation
  and rename (simulated), and cancellation mid-write.
- `pantheon-crypto` known-answer tests pass. `cargo xtask deps --core` passes and
  the crate count is recorded in [dependencies](02-dependencies.md).
- Both fuzzing layers (D20) are set up: the `Corruptor` harness runs in
  `cargo test`, the cargo-fuzz job runs on schedule, and `pantheon-xml` has a corpus plus a
  differential run.
