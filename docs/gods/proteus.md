# Proteus: convert to ZArchive

> The shape-shifter. Proteus turns a PS4 package into a `.zar` archive that
> shadPS4 can run.

## Scope (parity)

- `pack`: a stable directory tree to `.zar`.
- `convert`: a supported PS4 base FPKG streamed directly to `.zar`, with no
  temporary extracted tree. Optional `--update` writes the sibling
  `<name>-UPDATE.zar` after checking categories and matching title/content IDs.
- Streaming writer: chunk indexes spill to disk, output is published without overwrite.
- Name rules: UTF-8, ≤ 127 bytes per component. Trees where every file has an
  empty payload are unsupported (carried limit).
- Two-file publication is **not atomic**. If update publication fails, the
  completed base stays and the error names it.
- Cancel removes unpublished scratch. Preparation and final publication cannot be interrupted.

## Library

```rust
// pantheon-zar
pub struct Writer { … }  // create_new(OutputFile), add_directory(path), add_file(path, size, reader, ctx), finish()
pub struct Reader { … }  // open(src, limits): for verification and tests (random reads)
// proteus
pub fn pack(src: &Path, out: &Path, ctx: &Ctx) -> Result<Report, Error>;
pub fn convert(base: &Path, update: Option<&Path>, out: &Path, ctx: &Ctx) -> Result<ConvertReport, Error>;
pub fn update_path(out: &Path) -> Result<PathBuf, Error>;
```

A `pantheon-zar::Reader` is new. Today's checks use an upstream C++ reader. Our
own reader enables fast round-trip tests on every OS, and the pinned upstream
reader remains the **independent** check.

## CLI

`proteus pack DIR OUT.zar` and `proteus convert GAME.pkg OUT.zar [--update U.pkg]`.

## Model / GUI

Convert page (PS4 only; disabled for PS3 with the reason). Base is the loaded
package, update is optional, and output is a new lowercase `.zar` name. The
report shows both paths.

## Tests

- Round trip: pack, read back and compare; random reads; a synthetic file > 4 GiB
  (sparse source).
- Independent: pinned upstream reader (`reference/zarchive-reader.cpp`, carried
  from pkg-tools) on Linux CI.
- Differential: **reader-equivalent** output vs. pkg-tools for the same inputs
  (D16): the same paths, kinds, sizes and SHA-256 of every file when both archives
  are read back. Byte identity is not required, so the writer may choose its own
  chunking.

## Differential results

`cargo xtask diff proteus INPUT…` (`xtask/src/diff_proteus.rs`) runs
`pkg-tools convert` (or `pack` for a folder) and `proteus convert` on each input.
An input is a base package, `BASE.pkg+UPDATE.pkg` for a pair, or a folder. Both
archives (and both `-UPDATE.zar` files) are read back with `pantheon-zar`, the
footer SHA-256 is checked, and the archives must hold the same paths, kinds,
sizes and SHA-256 of every file. Inputs that both tools refuse also pass.

| Date | pkg-tools | Inputs | Outcome |
| --- | --- | --- | --- |
| 2026-11-12 | `20e3c3a` | 1 local PS4 game + patch pair, the same game alone, the patch alone (both refuse), 2 local synthetic PS4 PKGs (one with > 4 GiB of files), 1 local PS4 add-on (both refuse), 1 local PS3 retail PKG (both refuse) | 7 of 7 pass |

Inputs are the owner's private local fixtures (not committed, not named here).

## Independent results

`reference/check-zarchive.py` (see `reference/README.md`), upstream ZArchive
`965b66c`, Linux, 2026-11-12: the generated tree (18 files, 2 folders) and the
`--large` run (19 files, 2 folders, one sparse 4 GiB + 65,537-byte file) pass,
including footer SHA-256, raw and compressed blocks and the 128-byte refusal.
With `--pkg`/`--update` on the local game + patch pair, the upstream reader
matched every byte and random read against `hades extract` (base 2,737 files /
632 folders, update 1,850 files / 851 folders). This is archive compatibility
only: shadPS4 loading has not been run.

## Implementation notes

- `pantheon-zar` (`crates/zar`) writes into a `pantheon_fs::OutputFile`, so
  publication is the shared no-overwrite path (hard link, then no-replace rename,
  then the D28 direct-write fallback). Offset records spill to a
  `pantheon_fs::ScratchFile` beside the output. All-zero blocks reuse one zstd
  frame, so sparse inputs stay cheap.
- `pack` snapshots every source file before any output exists, rechecks each
  file when it is opened and again before publication, and refuses an output
  inside the source folder.
- `convert` checks both packages (plaintext PARAM.SFO, header/SFO content IDs,
  base `gd` with a `CUSA#####` title matching its content ID, update `gp` with
  identical IDs), every archive name and the payload, then reserves both
  outputs, streams both, and only then publishes base, then update.
- Reading a PS4 entry fails as an I/O error with the package error as its
  message; the typed kind (for example `KeyUnavailable`) is not carried through
  the `Read` boundary.

## References

pkg-tools `docs/zarchive.md`, `docs/compatibility.md` §5 and §7.
