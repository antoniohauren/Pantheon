# Roadmap

Each phase ends usable on its own and has exit criteria. Phases 2–8 each deliver
one god's library + CLI. GUI work waits until the core reaches parity (P8).

| Phase | Delivers | Exit criteria |
| --- | --- | --- |
| **0. Bootstrap** | Repository, workspace, lints, `rust-toolchain.toml`, CI on 3 OSes, `xtask` skeleton, `deps-allowlist.txt`, empty `docs/verification-log.md` (format: date, versions, evidence kind, outcome), decision log reviewed | `LICENSE-MIT` + `LICENSE-APACHE` committed (D13); public GitHub repo with Actions on 3 OSes (D18); empty workspace green on all CI runners |
| **1. Foundation** | `pantheon-core`, `-fs`, `-crypto`, `-json`, `-xml`; `xtask deps`; fuzz setup | [Foundation exit criteria](03-foundation.md#phase-1-exit-criteria-for-the-foundation) |
| **2. Athena** | `pantheon-psf`, PS3/PS4 header + SFO reading, `pantheon-package` detection, CLI framework + `pantheon` multiplexer, `athena` CLI | Differential inspect passes on all local fixtures |
| **3. Hades** | PS4 PFS/PFSC, PS3 streams, listing, full/selected extraction, `hades` CLI | Manifest parity on both PS4 fixtures and the PS3 fixture; FS safety suite green on 3 OSes |
| **4. Hermes** | Send to PS4 at parity, without any third-party payload | Spikes S1–S5 recorded in `docs/hermes-provenance.md` (see note); batch send verified on the owner's console and logged |
| **5. Proteus** | `pantheon-zar` writer + reader, `pack`, `convert` | Upstream-reader checks pass; > 4 GiB synthetic file; base + update conversion is reader-equivalent to pkg-tools (D16) |
| **6. Dionysus** | PS3 NPDRM SELF/EDAT, PS3 PKG writer, `check` / `liberate` | Synthetic + reference checks pass; SSX payload parity; RPCS3 check logged (new evidence, not carried) |
| **7. Nike** | `pantheon-trp`, ESFM decryption, `nike` CLI | SSX (37) and TMNT (31 + 31) definitions match pkg-tools |
| **8. Hephaestus** | GP4, preflight, inner/outer images, container helpers, rebuild | Full reference ladder from pkg-tools passes; rebuild parity on synthetic sources; Windows/macOS claimed only if checks pass there |
| **— Parity gate** | CLI parity across all gods | Every row of pkg-tools' command list has a Pantheon equivalent with passing differential tests |
| **9. App runtime + GTK** | `pantheon-app`, all god models, `pantheon-gtk` hub + 7 standalone apps | App IDs per D23; model tests; GTK smoke test on synthetic fixtures; behaviour checklist from pkg-tools README "GNOME application" met |
| **10. GPUI** | `pantheon-gpui` hub + standalone apps | GPUI pinned per D25; same model-driven pages; runs on Linux, Windows, macOS |
| **11. Release + retire** | Packaging per D24 (Flatpak, AppImage, CLI tarball; Windows zip; unsigned macOS `.app` zip), user docs, migration notes (settings, marker names) | pkg-tools archived with a pointer to Pantheon |

## Ordering notes

- Athena → Hades → Proteus is the critical path: Proteus depends on PS4 reading,
  and Phase 2 sets the CLI conventions every later god reuses.
- Hermes depends only on Phase 2 (package identity) and can run in parallel with
  Phases 3 or 5 if desired.
- Dionysus, Nike and Hephaestus are independent of each other once Hades is done.

## Note on Hermes

[`gods/hermes.md`](gods/hermes.md) is **approved** (D27). Phase 4 begins with its spikes S1–S5 (loader entry, symbol
lookup, installer registration, status polling, `build-std`), each recorded in
`docs/hermes-provenance.md` under the clean-room rule, before any production
payload code is written.
