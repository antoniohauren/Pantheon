# Dependency policy

## 1. Rules

1. **Standard library first.** Simple functionality (argument parsing, JSON
   output, INI settings, an HTTP/1.1 range server, a restricted XML reader, ELF
   reading in the builder) is written in this repository.
2. **Established primitives for crypto and compression** (decision D8). We never
   write our own AES, SHA, HMAC, RSA, bignum, deflate or zstd.
3. **Pure Rust** for everything below the frontend layer. Native (C) libraries are
   allowed only in frontend crates (GTK, and whatever GPUI pulls in).
4. **One owner per dependency.** Each allowed crate lists the workspace crates that
   may depend on it. Crypto/compression crates belong to `pantheon-crypto` only.
5. **Exact pins** (`=x.y.z`), `default-features = false`, and only the features we
   use. Updates are deliberate commits, each with a reason.
6. **Dev-only crates** (test oracles, fuzzing) are listed separately. They never
   appear in a release build graph.
7. Adding a crate means a pull request that updates this file, the allowlist file,
   and the reason. `cargo xtask deps` fails CI otherwise.

## 2. Runtime allowlist (libraries + CLI)

| Crate | Owner crate(s) | Used for | Notes |
| --- | --- | --- | --- |
| `aes` | `pantheon-crypto` | AES-128/256 block cipher | PS3 CTR, PS4 CBC/XTS, NPDRM, ESFM |
| `cbc` | `pantheon-crypto` | CBC mode | PS4 metadata/entry keys, ESFM, NPDRM metadata |
| `xts-mode` | `pantheon-crypto` | AES-XTS | PFS images |
| `sha1` | `pantheon-crypto` | SHA-1 | PS3 debug stream, NPDRM/EDAT hashes |
| `sha2` | `pantheon-crypto` | SHA-256 | PS4 digests, ZArchive, Hermes package digest |
| `hmac` | `pantheon-crypto` | HMAC-SHA1/SHA256 | NPDRM sections, PFS block HMACs |
| `rsa` | `pantheon-crypto` | RSA-2048 fake-key unwrap, public wrap | Pulls in `num-bigint-dig` |
| `num-bigint-dig` | `pantheon-crypto` | ECDSA over the PS3 NPDRM curve (custom arithmetic on top of bignum) | Already a transitive dependency of `rsa`, so no new crate |
| `miniz_oxide` | `pantheon-crypto` | zlib inflate (PFSC, SELF sections) and deflate (PKG repack if needed) | **Replaces `flate2`**: we already use its Rust backend, and depending on it directly removes `flate2` and `crc32fast` |
| `ruzstd` | `pantheon-crypto` | Zstandard compression and decompression | ZArchive; pure Rust |
| `rustix` | `pantheon-fs` (Unix targets only) | `openat`, `O_NOFOLLOW`, `fstat`, `linkat`, `mkdirat`, `renameat2` | Features `std`, `fs` only |
| `windows-sys` | `pantheon-fs` (Windows targets only) | `MoveFileExW` no-replace rename and `BCryptGenRandom` (D22, D26) | Features `Win32_Storage_FileSystem` and `Win32_Security_Cryptography` only. Also arrives transitively through `ctrlc` and `rustix`, and that use is recorded separately |
| `ctrlc` | `pantheon-cli` | Ctrl+C / console-close handler on all three OSes (D15) | Brings `nix` (Unix) and `windows-sys` (Windows) transitively. Never used below L4 |

`rsa` carries advisory RUSTSEC-2023-0071 (Marvin attack). It is accepted (P17):
the attack needs a secret private key, and Pantheon only uses public fixed fake
keys. The acceptance is recorded in `deps-allowlist.txt` next to the crate.

Transitive crates (e.g. `cipher`, `digest`, `generic-array`/`hybrid-array`,
`subtle`, `zeroize`, `libm`, `bitflags`, `linux-raw-sys`) are recorded in the allowlist file with the
direct crate that brings them in. The audit fails if a new transitive crate
appears.

CTR mode stays a small helper over the `aes` block cipher, as in pkg-tools
(`crates/ps3-pkg/src/npdrm/crypto.rs:73`). A mode of operation is not a new
primitive, so the `ctr` crate is not needed.

**Budget:** measured on 2026-11-12, `cargo tree -p pkg-cli -e normal` in pkg-tools
lists 62 third-party crates on Linux (69 entries minus 7 local crates). The full
lockfile, with the GUI included, has 138 packages. Phase 1 must measure Pantheon's
L0–L3 count and record it here. Target: **fewer than pkg-tools, with zero non-crypto/compression direct
dependencies apart from `rustix`, `windows-sys` and `ctrlc`.**

**Phase 1 measurement (2026-11-12, Linux host):** `cargo xtask deps --core` counts
**51** third-party crates in the normal+build graph of the L0 crates (46 of them
through `pantheon-crypto`, mostly `rsa` and its `num-bigint-dig`/`rand` chain; the
rest are `rustix` and its transitive crates). `deps-allowlist.txt` lists 63 locked crates:
the 51, plus Windows-only crates, build-only `autocfg`, the dev-only `roxmltree`
and its `memchr`, and `base64ct` (locked but not built). L1–L3 and `ctrlc` are
not in the graph yet.

## 3. What is removed compared with pkg-tools

| pkg-tools crate | Replacement | Where |
| --- | --- | --- |
| `serde_json` | Streaming JSON writer (escaping, numbers, nesting, no parser) | `pantheon-json` |
| `roxmltree` | Bounded XML subset reader: elements, attributes, text, the 5 predefined entities and numeric character references; **no** DTD, external entities or processing-instruction semantics. Limits on depth, attribute count and total size | `pantheon-xml` |
| `flate2` | `miniz_oxide` directly | `pantheon-crypto` |
| DPI `payload.bin` (GPL-3 binary) | Our own `hermes-payload` | `console/` |
| `blueprint-compiler` (build-time tool) | **P10:** widgets built in Rust code (see [frontends](04-frontends.md#3-gtk-frontend-pantheon-gtk)) | `pantheon-gtk` |
| `linuxdeploy` (downloaded by script) | Packaging per D24 (AppImage tool chosen in Phase 11, never downloaded unpinned) | packaging |

`pantheon-xml` is checked by a **differential test** against `roxmltree` as a
dev-dependency. The rule is one-sided, because `pantheon-xml` rejects some
inputs that roxmltree accepts (for example `<!DOCTYPE`):

- if `pantheon-xml` accepts an input, roxmltree must accept it too and give the
  same tree (elements, attributes, text after the documented whitespace rule);
- if `pantheon-xml` rejects an input, its error must belong to a documented reject
  class (DOCTYPE, unknown entity, a limit). An unexplained rejection fails the test.

This is the one place where a removed runtime crate stays as an oracle.

## 4. Frontend-only dependencies

| Crate | Owner | Notes |
| --- | --- | --- |
| `gtk4`, `libadwaita` (+ gtk-rs transitive crates) | `pantheon-gtk` | Needs the system GTK 4.10+ and libadwaita 1.5+. Linux first. |
| `gpui` (+ its transitive crates) | `pantheon-gpui` | Large graph (renderer, windowing). Exact crates.io pin, updated at most quarterly (D25). |

These graphs are large and outside our control. The rule is containment: no
crate in L0–L3 may depend on them, and `cargo xtask deps --core` audits L0–L3
only.

## 5. Dev-only allowlist

| Crate | Purpose |
| --- | --- |
| `roxmltree` | Differential oracle for `pantheon-xml` |
| `libfuzzer-sys` | cargo-fuzz targets in `fuzz/` (D20). Nightly, scheduled CI job only. `fuzz/` is a standalone package with its own lockfile, so `cargo xtask deps` does not audit it; its version is recorded in `fuzz/Cargo.toml` |
| `serde_json` | `xtask` only: parses and normalises JSON for `xtask diff` (P14) |
| `pantheon-testalloc` (ours) | Counting allocator for allocation-bound tests (D14) |

No test framework crates (`proptest`, `rstest`, `insta`). Property-style tests use
a small seeded PRNG in `pantheon-core::testing` (SplitMix64, behind
`#[cfg(any(test, feature = "testing"))]`).

## 6. Enforcement: `cargo xtask deps`

- Reads `Cargo.lock` (simple `[[package]]` blocks; no TOML crate needed) and
  `deps-allowlist.txt`:

  ```text
  # name        version   owner(s)                 reason
  aes           =0.9.3    pantheon-crypto          AES block cipher (D8)
  rustix        =1.1.5    pantheon-fs[unix]        no-follow openat (P5)
  cipher        *         via:aes                  transitive
  ```

- Fails when a locked crate is not listed, when a version does not match, or
  when a crate outside the owner list depends on it (checked through
  `cargo tree -e normal,build -i <crate>`, parsed as plain text). Build
  dependencies and proc-macros count as runtime for this audit, because they run
  at build time. Dev-only crates are checked separately: each one must be
  absent from `cargo tree -e normal,build` for every default member.
- Runs in CI on all three OSes, because target-specific dependencies differ.
- `--core` audits default members only. `--all` includes frontends and reports
  their counts without failing on them.

## 7. Toolchain

- Pinned with `rust-toolchain.toml` (P9), including the `x86_64-unknown-none`
  target and `rust-src` if the payload needs `build-std` (to be decided in the
  Hermes spike).
- No `cc`, `bindgen` or C compiler anywhere in the default build.
  `rust-lld`, which ships with Rust, links the payload.
