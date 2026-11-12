# Hades: explore and extract

> Lord of what lies beneath. Hades shows the tree inside a package and brings
> it out, whole or in part.

## Scope (parity)

- Listing without extraction: paths, kinds and decoded sizes for PS3 and PS4.
- Full extraction and selected extraction (files and folder subtrees). Selected
  extraction preserves package-relative paths, required parents and empty selected
  folders, and writes each entry once when selections overlap.
- PS4: nested 64 KiB D32/S32 PFS, indexed zlib/raw PFSC, direct/single/double
  indirect blocks, files and offsets > 4 GiB, `sce_sys` layout from both sources
  (container + inner image) merged into one application tree.
- PS3: retail (`0x8000`, AES-128-CTR) and debug (`0`, SHA-1 stream). Entry kinds
  1, 2, 3, 4, 9; flags `0x80000000`, and `0x100` for kind 1 only. SELF/EDAT/SDAT
  bytes are preserved, not decrypted.
- Generated folder name `<stem>-extracted`, capped at 31 UTF-8 bytes (RPCS3 rule).
- Cancel between 64 KiB chunks and entries. Incomplete marker on failure or cancel.

## Library

```rust
pub fn list(pkg: &mut Package) -> Result<Tree, Error>;                    // Tree has a generation stamp
pub fn extract(pkg: &mut Package, sel: Selection, parent: &Path, name: &str, ctx: &Ctx) -> Result<Report, Error>;
pub enum Selection { All, Paths { generation: u64, indices: Vec<u32> } }  // stale generation = error
```

Selection resolution (deduplicate, add parents, expand subtrees, compute the total
of selected decoded bytes) lives in `pantheon-package`, so Hades and the GUI tree
agree exactly. Empty or unknown selections fail **before** any output exists.

## Limits (carried)

1,000,000 entries, 64 MiB path bytes, paths ASCII ≤ 4,096 bytes / 128 components,
255-byte components, 64 MiB per directory's data. The source package length must
equal its declared length.

## CLI

`hades list INPUT [--json]` and `hades extract INPUT OUT [--select PATH]…` (P12).
Progress is in decoded bytes, using decimal MB.

## Model / GUI

Virtualised tree with child models created on expansion, and multi-select with
keyboard and Ctrl/Shift-click. The listing persists across hub page changes and
resets when a new package loads. *Extract All…* / *Extract Selected…* (the latter
disabled while nothing is selected, with that as the reason).

## Tests

- Synthetic PS3/PS4 trees with the shared selection fixture from pkg-tools
  `docs/explore-selection.md`, rebuilt by our own generators.
- Adversarial: traversal, case collisions, duplicate files, special nodes,
  symlinked destination parent, existing output, cancel mid-file, I/O error injection.
- Differential: path, size and SHA-256 manifest vs. pkg-tools on both local PS4
  fixtures (4,583 files / 1,483 directories) and the PS3 fixture.

## Not supported (carried)

PS4 DLC, PS4 retail, delta patches, 64-bit PFS inodes, per-file PFS compression;
PS3 IDU, PSP/Vita/PSM, split packages.

## References

pkg-tools `docs/extraction.md`, `docs/explore-selection.md`, `docs/ps3.md` (Supported boundary).
