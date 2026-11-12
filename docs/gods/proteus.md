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
pub struct Writer { … }  // create_new(OutputFile), add_directory(path), add_file(path, size, reader), finish()
pub struct Reader { … }  // new: for verification and tests (random reads)
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

## References

pkg-tools `docs/zarchive.md`, `docs/compatibility.md` §5 and §7.
