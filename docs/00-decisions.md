# Decision log

Every design choice in these documents is in one of three states:

- **Decided**: the owner chose it explicitly.
- **Proposed**: the author of these docs chose it as a default. It stands until
  the owner changes it, and it must not be treated as an owner decision. Once the
  owner accepts it, its ID stays the same and it moves to *Accepted proposals*.
- **Open**: it needs an owner answer before the phase that depends on it starts.

When a decision changes, edit this file first, then the documents that depend on it.

## Decided (owner, 2026-11-12)

| ID | Decision |
| --- | --- |
| D1 | Product name: **Pantheon**. Each tool has a Greek god's name; **Hermes** sends packages to the PS4. |
| D2 | **Clean redesign** in a fresh repository at `~/Projects/Pantheon`. pkg-tools is a reference, not code to port. |
| D3 | The rewrite is in Rust, in **multiple steps** (phases), each one usable on its own. |
| D4 | Ship **per-god apps and one Pantheon hub app**, all sharing the same libraries. |
| D5 | CLI: **both** per-god CLIs (`hermes …`) and a `pantheon <god> …` multiplexer. |
| D6 | Libraries and CLI target **Linux, Windows and macOS** from day one. |
| D7 | The GUI is not a priority now. Build a **strong foundation** that can drive several frontends: **GTK** (GNOME) and **GPUI** (Linux/Windows/macOS, as Zed does). |
| D8 | Crypto/compression: **keep established crates** (RustCrypto, a zlib/deflate crate, a zstd crate). Remove other third-party crates where practical. |
| D9 | Full parity is required before pkg-tools is retired: inspect + SFO + extract, Send to PS4, ZAR + PS3 liberate, Trophies + FPKG rebuild/GP4. |
| D10 | Hermes must **not** embed DirectPackageInstaller's `payload.bin` or any other third-party console binary. We write and build our own payload. |
| D11 | Hermes's console payload is **Rust `no_std`**, built inside the workspace by our own builder. No OpenOrbis, no C. |
| D12 | The PS4 Trophy Unlocker is **out of scope**. |
| D13 | License: **MIT OR Apache-2.0** (dual, the Rust ecosystem default). This is possible because the GPL-3 DPI payload is never bundled (D10). Closes O1. |
| D14 | Allocation-bound tests use one **dev-only** crate, `pantheon-testalloc`, as the second documented exception to `forbid(unsafe_code)`. It never appears in a release build graph. |
| D15 | CLI Ctrl+C uses the **`ctrlc` crate**, owned by `pantheon-cli` only (closes former O9). |
| D16 | Proteus differential checks are **reader-equivalent**, not byte-identical: the same tree, sizes and file bytes when read back. The writer stays free to choose its chunking. |
| D17 | Per-god binaries ship with **bare names** (`hermes`, `hades`, …). Packagers may rename them, and `pantheon <god>` always works. Closes O2. |
| D18 | Hosting: **public GitHub repository**, CI on **GitHub Actions** hosted Linux, Windows and macOS runners. Closes O4. |
| D19 | pkg-tools is **frozen at `20e3c3a`** while Pantheon is built. It is the fixed differential oracle. Closes O8. |
| D20 | Fuzzing uses **both**: a std-only `Corruptor` mutation harness in every `cargo test`, and `cargo-fuzz` (nightly + `libfuzzer-sys`, dev-only) in a scheduled CI job and before releases. Every crash becomes a committed regression input. Closes O6. |
| D21 | The `hermes` binary embeds the payload behind a cargo feature `embed-payload` (on in release builds). Without it, `--payload FILE` is required, so the default `cargo build`/`test` needs no console target. Supported loaders: **GoldHEN BinLoader** and a **generic TCP loader** (raw bytes on a configurable port). Closes O10. |
| D22 | `pantheon-fs` may use `windows-sys` for **audited `unsafe` calls** on Windows: a `MoveFileExW` no-replace rename wrapper, and the OS random source (D26). It is the third `unsafe` exception. Resolves the P15 Windows gap and amends P5. |
| D23 | App IDs use **`io.github.antoniohauren`**: `io.github.antoniohauren.Pantheon` for the hub and `io.github.antoniohauren.<God>` for each standalone app (e.g. `….Hermes`). Closes O3. |
| D24 | Distribution: Linux ships **Flatpak (Flathub)**, **AppImage** and a **plain CLI tarball**. Windows ships a **zip** of the executables, and macOS a **zipped, unsigned `.app`** (plus the CLI). There is no code signing or notarization for now. Closes O5. |
| D25 | GPUI is pinned to an exact **crates.io release** (`=x.y.z`) and updated deliberately, at most once a quarter, each update in its own commit. Closes O7. |
| D26 | Hermes's session token comes from the **OS random source**: `/dev/urandom` on Unix (std only) and `BCryptGenRandom` (system-preferred RNG) on Windows, via `windows-sys` in `pantheon-fs` (D22). It is exposed as `pantheon_fs::os_random`. |
| D27 | **`gods/hermes.md` is approved** as the Phase 4 design, including the clean-room rule (DPI's GPL source is not read) and the payload defaults: a 64 KiB size budget and a 5-minute idle timeout. Protocol details may still change after the spikes S1–S5, recorded in this log. |

## Accepted proposals (owner, 2026-11-12)

All of P1–P17 were reviewed and accepted by the owner. They now carry the same
weight as the D-entries.

| ID | Proposal | Why |
| --- | --- | --- |
| P1 | God mapping: Athena = inspect, Hades = explore/extract, Hermes = send, Proteus = ZAR, Dionysus = PS3 liberate, Nike = trophies, Hephaestus = PS4 authoring/rebuild. | The myth matches the job (see README). The owner asked for Greek-only proposals. |
| P2 | Library crates use descriptive names (`pantheon-ps4`, `pantheon-psf`). God names are reserved for tools (`hermes`, `hades`). | Format crates are reused by several gods. A crate named after a god would hide what it parses. |
| P3 | No async runtime. Work runs on `std::thread`, with cancellation tokens and progress channels. | Fewer crates. All workloads are I/O-bound streams or CPU-bound crypto. |
| P4 | Our own argument parser, JSON writer and bounded XML reader replace `clap`/`serde_json`/`roxmltree`. | Small, testable code. The XML reader is checked against roxmltree as a **dev-only** oracle. |
| P5 | `rustix` stays as the one platform crate on Unix, for no-follow, descriptor-relative opens. Windows uses `std` plus documented flags. A Windows crate is added only if `std` cannot meet the safety contract (amended by D22: `windows-sys` for one no-replace rename). | The safety contract in [foundation](03-foundation.md#pantheon-fs) cannot be met with plain `std` on Unix. |
| P6 | A `pantheon-crypto` facade is the **only** crate that depends on crypto crates directly. | It puts the allowlist in one place, and a crate can be swapped without touching format code. |
| P7 | Hermes uses a **new wire protocol** of our own (versioned, with a session token, and the console reporting status back), not DPI's protocol. | We own both ends. A status report means "sent" can finally be told apart from "install registered". |
| P8 | The GUI starts only after the CLI reaches parity (Phase 9+). GTK comes before GPUI. | D7. GTK matches the current GNOME users, and GPUI's API changes faster. |
| P9 | Rust edition 2024, pinned toolchain via `rust-toolchain.toml`, MSRV = that toolchain. | Reproducible builds across three OSes and a `no_std` target. |
| P10 | The GTK frontend builds widgets in Rust code instead of Blueprint. | It removes the external `blueprint-compiler` build tool and mirrors how GPUI builds its views, so both frontends follow the same structure. |
| P11 | Per-god binaries are thin `main` functions, not argv[0]/symlink dispatch. | Symlinks are unreliable on Windows, and thin mains cost almost nothing. |
| P12 | `hades extract --select PATH…` is added to the CLI (pkg-tools has selective extraction in the GUI only). | The library already supports it, and parity between frontends is a design goal. |
| P13 | A `Partial` error kind (exit code 10) for operations that completed some items and failed others: a Hermes batch, Nike sets, Proteus base without update. | Batch outcomes must not look like full success (0) or total failure. |
| P14 | `xtask diff` normalises JSON with `serde_json` as a **dev-only** dependency of `xtask`. | `pantheon-json` has no parser by design, and the diff tool never ships. |
| P15 | `OutputTree::create_new` builds under a scratch name and renames it to the final name without replacing (`renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS, `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` on Windows). The incomplete marker is still written first. | Closes the crash window between creating the directory and writing its marker. |
| P16 | Settings are written by atomic replace (scratch file + rename), after re-reading the file and merging only the writer's own section. | The hub and the standalone apps can run at the same time and share one file. |
| P17 | Advisory RUSTSEC-2023-0071 (`rsa`, Marvin attack) is accepted and recorded in the allowlist. | It targets private-key operations on secret keys. Pantheon only uses public, fixed fake keys. |

## Smaller items accepted (owner, 2026-11-12)

| ID | Decision |
| --- | --- |
| D28 | FAT32/exFAT last-resort fallback: write directly to the final name with `create_new`. A failure is `Incomplete`, names the partial file, and the name is never reused ([foundation §3](03-foundation.md#3-output-files-single-file-publication)). |
| D29 | New CLI commands: `athena artwork`, `hermes probe`, `nike icons`, `dionysus check`. |
| D30 | Clippy `arithmetic_side_effects` and `cast_possible_truncation` are `deny` in L0/L1 crates. |
| D31 | CLI exit codes as in [architecture §4.1](01-architecture.md#41-errors): 2 usage, 3 malformed, 4 unsupported, 5 limit, 6 key, 7 unsafe output, 8 I/O, 9 network, 10 partial, 130 cancelled. |
| D32 | Generated top-level folder names (Hades, Dionysus) are capped at 31 UTF-8 bytes with the suffix kept (RPCS3 rule). |
| D33 | The Hermes payload targets `x86_64-unknown-none`. Spike S5 confirms it, and a change needs a log entry. |
| D34 | The Hermes v1 wire format in [hermes.md](gods/hermes.md#wire-protocol-hermes-wire-version-1) is accepted. Changes after spikes S1–S5 are recorded here. |

## Open (owner answer needed)

| ID | Question | Needed before |
| --- | --- | --- |
| O1 | *Closed by D13.* | — |
| O2 | *Closed by D17.* | — |
| O3 | *Closed by D23.* | — |
| O4 | *Closed by D18.* | — |
| O5 | *Closed by D24.* | — |
| O6 | *Closed by D20.* | — |
| O7 | *Closed by D25.* | — |
| O8 | *Closed by D19.* | — |
| O9 | *Closed by D15.* | — |
| O10 | *Closed by D21.* | — |
