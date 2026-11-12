# Hephaestus: PS4 authoring and rebuild

> Smith of the gods. Hephaestus works PS4 packages at the forge: GP4 projects,
> PFS images and the source-backed base rebuild.

This is the riskiest god. Everything here is **experimental** in pkg-tools, and
its gates carry over unchanged. Nothing is claimed beyond what pkg-tools
verified, until new evidence is recorded.

## Scope (parity, in build order)

1. **GP4** (`gp4 create`, `gp4 preflight`): a narrow standalone-app GP4 from
   `sce_sys/param.sfo` (`CATEGORY=gd`, `eboot.bin` required). Explicit UTC
   timestamp. The passcode file is outside the source tree (32 printable ASCII
   bytes, plus an optional LF). GP4 output is private, written with `Access::OwnerOnly`
   ([foundation](../03-foundation.md#2-output-trees)): mode 0600 on Unix, and an
   owner-only ACL on Windows once the Phase 1 spike passes (until then it is refused there). Preflight is a read-only inventory with
   explicit blockers.
2. **Package preflight** (`fpkg preflight`): records and authoring roles of a
   supported fake GD package. Capped reads. DK3 metadata decryption only (key index 3).
3. **Inner image** (`gp4 image`): clear D32 PFS and raw-block PFSC writer,
   including lookup/hash collisions and empty directories. Output is only
   `.pfs`/`.pfsc`. Container resources (`param.sfo`, `icon0`, `pic0`, `pic1`,
   `snd0.at9`) are planned, not emitted.
4. **Outer image** (library): signed S32 PFS, raw PFSC, block HMACs, AES-XTS. Needs
   an explicit 32-byte image key (`legacy_image_key(content_id, passcode)`).
5. **Container helpers** (library): `BaseContainerLayout`, PlayGo defaults,
   digests, metadata CBC, deterministic public RSA wrapping.
6. **Source-backed base rebuild** (`fpkg rebuild`, Unix in pkg-tools): legacy and
   2019 newcrypt GD bases with default single-chunk PlayGo, into a fresh legacy
   D[1]/zero-seed output. Preserves SFO, ENTRY_KEYS, licenses, keystone, every
   application file, paired NP records, trophy00 and supported resources. Every
   retained byte is re-read and compared before publication.

Pantheon goal: the rebuild runs on **all three OSes**. The Unix-only parts in
pkg-tools are filesystem calls, which `pantheon-fs` now abstracts. Windows and
macOS support is claimed only after the same independent checks pass there.

## Hard gates (carried, never weakened)

- No generated licenses, NP identity or keystones, no authentic signatures, and no
  new fixed authoring keys.
- Unknown container records, other trophy variants, custom PlayGo, updates,
  remaster and delta are rejected explicitly.
- Sources are read-only. One destination-sized scratch file, 64 KiB payload I/O,
  multiple source validation scans, no-overwrite publication. This is not a
  sandbox against hostile concurrent mutation.
- Passcodes, keys and fingerprints are never printed or logged.
- **No console installation, launch, save, NP or trophy compatibility claim.**

## Library

```rust
pub mod gp4 { pub fn create(..); pub fn parse(..); pub fn preflight(..); pub fn write_new(..); }
pub mod image { pub fn write_inner(..); pub fn write_outer(..); }
pub fn preflight_package(pkg: &mut Package, ctx: &Ctx) -> Result<PackageInventory, Error>;
pub fn rebuild(src: &Path, dst: &Path, passcode: Passcode, ctx: &Ctx) -> Result<Report, Error>;
```

Format code (GP4 XML via `pantheon-xml`, PFS writers, container layout) lives in
`pantheon-ps4::authoring`. Hephaestus holds the workflows and the source/output policy.

## CLI

`hephaestus gp4 create|preflight|image …` and `hephaestus fpkg preflight|rebuild …`,
with the pkg-tools arguments (`--passcode-file`, `--timestamp`, `--source-root`).

## Model / GUI

Last in GUI order. Rebuild first (one source, one output, optional passcode
file), then GP4 tools. Every view states the experimental status.

## Tests

- Carry over the reference ladder: LibOrbisPkg-based C# oracles and the Python
  checks in pkg-tools `tests/reference/` (authoring, container, crypto, GP4 image,
  inner/outer image, native rebuild, rebuild profile) into `reference/`, with
  pinned revisions.
- Synthetic known answers for CBC, XTS, HMAC and RSA wrapping (public inputs only).
- Differential: rebuilt output from Pantheon vs. pkg-tools for the same synthetic
  source. Byte identity is expected where the profile is deterministic.

## References

pkg-tools `docs/fpkg-authoring.md`, `docs/fpkg-authoring-contract.md`,
`docs/fpkg-authoring-provenance.md`, `docs/crypto-unblocking-findings.md`.
