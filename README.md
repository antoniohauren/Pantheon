# Pantheon

> A pure-Rust toolkit for PlayStation 3 and PlayStation 4 packages. It ships as a
> set of small tools named after Greek gods, plus one hub app that holds them all.

Status: **design documentation only.** No code exists yet. Pantheon is a clean
redesign of `pkg-tools` (`~/Projects/pkg-tools`, reference commit `20e3c3a`).
`pkg-tools` is the behavioural and format reference. Its code is not ported line
by line.

License: MIT OR Apache-2.0 (decision D13).

## The gods

| God | Domain in myth | Tool | Replaces in pkg-tools |
| --- | --- | --- | --- |
| **Athena** | Wisdom, knowledge | Inspect packages and PARAM.SFO: headers, metadata, container records, artwork | `inspect`, GTK *Overview* and *All Info* |
| **Hades** | The underworld, what lies beneath | Browse a package's file tree and extract all of it or a selection | `extract`, GTK *Explore* |
| **Hermes** | Messenger and god of roads, who delivers | Send packages to a PS4 over the LAN, using **our own** console payload | `ps4-send`, GTK *Send to PS4* |
| **Proteus** | The shape-shifter | Convert PS4 FPKGs (+ update) into ZArchive for shadPS4; pack folders into `.zar` | `convert`, `pack`, GTK *Convert to ZAR* |
| **Dionysus** | *Eleutherios*, "the liberator" | PS3 RAP-less conversion (liberation) to a folder or a new PKG | `liberate`, GTK *Liberate…* |
| **Nike** | Victory | Trophy sets: definitions, icons, grades, groups | `trophies`, GTK *Trophies* |
| **Hephaestus** | The forge | PS4 authoring: GP4 projects, PFS images, source-backed FPKG rebuild | `gp4 …`, `fpkg …` |
| **Pantheon** | All the gods together | The hub: one app and one CLI that hold every god | the single GTK window, `pkg-cli` |

Each god is available as:

1. a **library crate** (its operations, with no presentation code),
2. a **standalone CLI** (`hermes send …`),
3. a **standalone GUI app** (GTK, later GPUI),
4. a **section of the Pantheon hub** (GUI) and a subcommand of `pantheon` (CLI).

All four use the same library code. Frontends never contain format logic.

## Reading order

| # | Document | What it settles |
| --- | --- | --- |
| 0 | [Decision log](docs/00-decisions.md) | What the owner decided, what was proposed, what is still open |
| 1 | [Architecture](docs/01-architecture.md) | Layers, crate graph, error/cancel/progress model, threading, platforms |
| 2 | [Dependency policy](docs/02-dependencies.md) | Allowlist, what gets replaced, how it is enforced |
| 3 | [Foundation crates](docs/03-foundation.md) | `io`, `fs`, `json`, `xml`, `crypto` facades: the base everything stands on |
| 4 | [Frontends, apps and CLI](docs/04-frontends.md) | App model, GTK and GPUI adapters, hub vs single apps, the CLI family |
| 5 | [Testing and verification](docs/05-testing.md) | Synthetic fixtures, differential checks against pkg-tools, references, hardware |
| 6 | [Roadmap](docs/06-roadmap.md) | Phases in order, each with exit criteria; the parity gate that retires pkg-tools |
| — | [Gods](docs/gods/) | One document per tool: scope, contracts, API, CLI, GUI, tests |

## Principles

- **Foundation first.** The GUI comes last. The libraries and the CLI must be
  complete and verified before any frontend work.
- **Pure Rust, few crates.** Only the standard library, plus an explicit allowlist
  of established crypto and compression crates. Frontend toolkits are confined to
  frontend crates. See [dependency policy](docs/02-dependencies.md).
- **Linux, Windows and macOS from day one** for the libraries and the CLI.
- **Never destroy data.** Source packages are read-only, outputs are published
  without overwriting, and incomplete outputs are marked.
- **Bounded memory.** Payloads are streamed, and untrusted lengths and offsets
  are validated before any allocation or I/O.
- **Claim only what was verified.** Synthetic tests, independent reference
  comparisons and real console or emulator checks are kept separate.
- **Privacy.** No game contents, RAPs, keys or private fixture fingerprints go
  into commits or logs.

## Out of scope

- The PS4 Trophy Unlocker (console-side homebrew) from pkg-tools' `docs/trophy-unlocker.md`.
- PS5, Vita/PSP/PSM, PS4 retail decryption, DLC writing, delta patches.
- Any third-party console binary. Hermes's payload is built from our own source.
