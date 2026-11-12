# Dionysus: PS3 liberation

> *Dionysus Eleutherios*, the liberator. Dionysus converts a PS3 game's
> local-license payloads so that it no longer needs its RAP file.

Experimental in pkg-tools. It stays experimental until RPCS3 and console checks
are executed and recorded.

## Scope (parity)

- Inputs: a PS3 PKG (metadata content type `5`) or an extracted game folder
  (`PARAM.SFO` + `USRDIR`). Outputs: a new folder `<source>-liberated` or a new `.pkg`.
  Either input works with either output.
- Pre-check: does any payload need conversion? If none, stop before asking for a RAP.
- RAP discovery: `<content-id>.rap`, case-insensitive, next to the source and in
  sibling `exdata`. For folders, also the root and its `exdata` child. Multiple
  matches require a manual choice. A RAP is exactly 16 bytes and is validated
  cryptographically during conversion.
  Content ID comes from `PARAM.SFO`, falling back to the NPDRM header of `USRDIR/EBOOT.BIN`.
- SELF: local-license key revisions `01`, `0A`, `19`, `1C`. Section HMACs are
  checked, the license is set to FREE, NPD hashes regenerated, and the header
  signed (published 3.55 NPDRM key, curve `0x27`). The narrow auxiliary-section
  preservation rules carry over exactly.
- EDAT: local-license common type `0`, versions 2/3/4 with the flag and block
  limits from pkg-tools. The developer key comes from known candidates or from
  HMAC-validated decoded SELF sections only.
- SDAT, already-free files, non-NPDRM SELF and ordinary files are copied unchanged.
  RAP files inside the tree are left out of the output package.

## Library

```rust
pub fn check(src: &Path) -> Result<CheckReport, Error>;              // needs_liberation, content_id, rap candidates
pub fn liberate(src: &Path, rap: Rap, out: Output, ctx: &Ctx) -> Result<Report, Error>;
pub enum Output { Folder { parent: PathBuf }, Pkg { path: PathBuf } }
```

PS3 PKG **writing** (synthesized free-license metadata) lives in `pantheon-ps3`.
NPDRM SELF/EDAT code lives in `pantheon-ps3::npdrm`. Signing goes through
`pantheon-crypto::ecdsa_npdrm`.

## CLI

`dionysus check SRC` and `dionysus liberate SRC [--rap R] (--to-pkg OUT.pkg | --to-folder PARENT)`.

## Model / GUI

Source choice (PKG, loaded package or folder), then the pre-check, then the RAP
(auto-matched, with *Link RAP…* to override), then the output type. PS3 only.
Release builds are recommended for developer-key discovery (this is noted in the UI).

## Tests

- Synthetic SELF/EDAT conversion per supported revision. Wrong-RAP and
  damaged-section rejection. Auxiliary-section edge cases from pkg-tools
  `crates/ps3-pkg/src/npdrm/tests.rs`, re-expressed as behaviour.
- Independent: pkg-tools' `tests/reference/ps3-liberation.py` checks, carried over.
- Differential: converted payload hashes vs. pkg-tools on the local SSX fixture
  (no fingerprints committed).
- **Not yet done anywhere:** RPCS3 boot and console install of the output. This is a
  stated compatibility gate.

## References

pkg-tools `docs/ps3.md` (Liberate, Validation), `docs/compatibility.md` §6.
