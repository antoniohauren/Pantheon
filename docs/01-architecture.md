# Architecture

## 1. Goals the architecture must serve

1. **Several frontends, one core.** GTK, GPUI and CLI frontends call the same
   functions and get the same behaviour. Adding a frontend touches no format code.
2. **Per-god apps and a hub.** Any god can be built and shipped alone, and the hub
   is the sum of the gods, not a separate implementation.
3. **Cross-platform core.** Every crate below the frontend layer builds and passes
   its tests on Linux, Windows and macOS. Platform-specific code is confined to
   one crate (`pantheon-fs`). Exceptions: `pantheon-cli` gets its Ctrl+C handling
   from `ctrlc` (D15), and the `pantheon-app` settings paths use `std::env` per OS.
4. **Few dependencies.** Each third-party crate is allowed for a stated reason, in
   one place ([dependency policy](02-dependencies.md)).
5. **Safe by construction.** Untrusted input is validated at the format layer, and
   filesystem safety is enforced in one shared place, so no god can reimplement
   or bypass it.

## 2. Layers

```text
 L4  frontends      pantheon-cli      pantheon-gtk       pantheon-gpui
                    (bins: pantheon,  (bins: hub +       (bins: hub +
                     athena, hades…)   per-god apps)      per-god apps)
                         │                 │                   │
 L3  app runtime         │            pantheon-app  ◄──────────┘
                         │      (jobs, settings, recent files, model glue;
                         │       no toolkit dependency)
                         ▼                 ▼
 L2  gods           athena  hades  hermes  proteus  dionysus  nike  hephaestus
                    (each: ops + model + cli modules; no presentation)
                         │
 L1b package         pantheon-package   (detect by magic, unified open/list/read)
                         │
 L1  formats        pantheon-psf  pantheon-ps3  pantheon-ps4  pantheon-zar  pantheon-trp
                         │
 L0  foundation     pantheon-core  pantheon-fs  pantheon-crypto  pantheon-json  pantheon-xml

 console (separate target)   hermes-wire (no_std, shared)   hermes-payload (no_std)
 tooling                     xtask (payload builder, dependency audit, fixtures)
```

**Rule: a crate depends only on crates in lower layers.** Gods do not depend on
other gods. When two gods need the same thing, that code moves down to L1b or
lower. The one exception is the hub frontends, which depend on every god by design.
`hermes-wire` counts as L0 for this rule: the `hermes` god may depend on it, and it
depends on nothing in the workspace.

## 3. Crates

### L0: foundation (details in [03-foundation.md](03-foundation.md))

| Crate | Responsibility | Third-party |
| --- | --- | --- |
| `pantheon-core` | Checked integer/offset math, bounded readers, limits, `Cancel`, `Progress`, shared `ErrorKind` | none |
| `pantheon-fs` | Portable name rules, safe output trees, no-overwrite publication, incomplete markers, source stability checks | `rustix` (Unix only) |
| `pantheon-crypto` | The only crate that touches crypto/compression crates; exposes the narrow primitives formats need | RustCrypto, deflate, zstd |
| `pantheon-json` | Streaming JSON **writer** (no parser needed) | none |
| `pantheon-xml` | Bounded, non-validating XML subset **reader** and a small writer (GP4, trophy XML) | none |

### L1: formats

| Crate | Responsibility | pkg-tools reference |
| --- | --- | --- |
| `pantheon-psf` | PARAM.SFO 1.0/1.1 parse + serialize; raw-preserving | `crates/psf`, `docs/compatibility.md` §3 |
| `pantheon-ps3` | PS3 PKG header, metadata, entries, retail/debug streams; NPDRM SELF/EDAT; PKG writing for liberation | `crates/ps3-pkg`, `docs/ps3.md` |
| `pantheon-ps4` | PS4 PKG container, fake-key chain, PFS/PFSC reading; GP4; inner/outer image writers; container layout; rebuild | `crates/ps4-pkg`, `docs/extraction.md`, `docs/fpkg-authoring*.md` |
| `pantheon-zar` | ZArchive v1 streaming writer (and a reader, for verification) | `crates/zarchive`, `docs/zarchive.md` |
| `pantheon-trp` | TRP archive reading, TROPCONF/TROP*.SFM parsing, ESFM decryption | `crates/pkg-ops/src/trophies.rs` |

Formats read from `Read + Seek` and write to a `pantheon-fs` output handle. They
never print or prompt, and they never use the host's paths for anything other
than the output handle they are given.

### L1b: `pantheon-package`

Replaces the dispatch half of `pkg-ops`. It owns:

- `detect(reader) -> Kind` (PS3 PKG, PS4 PKG, PARAM.SFO, ZAR, unknown), based on magic
  bytes, never the filename.
- `Package` (a closed enum over `Ps3`/`Ps4`) with common queries: identity
  (title ID, content ID, category, version), artwork reads, SFO bytes,
  `list() -> Tree`, `open_entry(id) -> impl Read + Seek`, and `extract(selection, out, ctx)`.
- `Tree`: a flat, index-addressed listing of entries (path, kind, decoded size,
  parent) with a **generation stamp**, so a stale selection from an earlier load is
  rejected (pkg-tools `docs/explore-selection.md`).

### L2: gods

Every god crate has the same three modules:

```text
<god>/
  src/ops.rs     operations: plain functions taking a request + &Ctx, returning Result<Report, Error>
  src/model.rs   frontend-agnostic state machine: State, Intent, Effect (see §6)
  src/cli.rs     pub fn run(args: Args, io: &mut Io) -> ExitCode
```

- `ops` is the public library API. Other programs can use the god without its CLI or model.
- `model` holds what a GUI needs (which inputs are chosen, which actions are
  enabled, progress, last error) as pure data. It has no toolkit types and is unit-tested.
- `cli` holds argument parsing and text/JSON rendering for that god only.

Per-god documents: [Athena](gods/athena.md), [Hades](gods/hades.md),
[Hermes](gods/hermes.md), [Proteus](gods/proteus.md), [Dionysus](gods/dionysus.md),
[Nike](gods/nike.md), [Hephaestus](gods/hephaestus.md).

### L3: `pantheon-app`

The shared runtime for GUI frontends. It has no toolkit dependency.

- **Job runner**: runs an `ops` call on a worker thread, and forwards `Progress`
  and the final result as messages over `std::sync::mpsc`. One `Cancel` per job.
- **Settings**: a small INI-style key/value file in the OS config directory
  (`$XDG_CONFIG_HOME/pantheon`, `%APPDATA%\Pantheon`,
  `~/Library/Application Support/Pantheon`), resolved with `std::env` only.
  Each god gets its own section, and the hub and standalone apps share the file.
- **Session**: the loaded package (shared across gods inside the hub), recent
  files, and per-god model instances.
- **Hub registry**: a static list of `GodDescriptor { id, name, icon, accepts(kind), model factory }`
  that the hub frontends iterate over. A standalone app is the same frontend with a one-entry registry.

### L4: frontends

See [frontends, apps and CLI](04-frontends.md). Toolkit crates (`gtk4`,
`libadwaita`, `gpui`) appear **only** in `pantheon-gtk` / `pantheon-gpui`.

### Console side

| Crate | Target | Responsibility |
| --- | --- | --- |
| `hermes-wire` | `no_std`, host + console | Hermes protocol message types, encode/decode, limits. The same code runs on both ends. |
| `hermes-payload` | `x86_64-unknown-none` (proposed) | The PS4 payload: connect back, receive a job, call the installer service, report status |
| `xtask` | host | `cargo xtask payload` builds and converts the payload into a flat binary, then checks it; `cargo xtask deps` audits dependencies |

Details: [Hermes](gods/hermes.md) (approved, D27).
`hermes` embeds the built payload only with the cargo feature `embed-payload`
(D21), which release builds turn on after `cargo xtask payload`. Default builds
take `--payload FILE`, so they need no console target.

## 4. Shared contracts

### 4.1 Errors

- Each crate defines its own `Error` enum, and it implements `std::error::Error`.
  **No `Box<dyn Error>` or `String` errors in public APIs.** (pkg-tools' `pkg-ops`
  returns both. This redesign removes them.)
- Every error maps to one `pantheon_core::ErrorKind`:

  | Kind | Meaning | CLI exit code (proposed) |
  | --- | --- | --- |
  | `Malformed` | Input violates the format | 3 |
  | `Unsupported` | Valid but outside the supported matrix | 4 |
  | `Limit` | Exceeds an explicit resource policy | 5 |
  | `KeyUnavailable` | Required decryption material is missing or wrong (e.g. wrong RAP) | 6 |
  | `UnsafeOutput` | Output path refused (exists, symlink, collision, traversal) | 7 |
  | `Io` | Operating-system I/O failure | 8 |
  | `Cancelled` | The user cancelled | 130 |
  | `Network` | Hermes transport failure | 9 |
  | `Partial` | Some items completed, others failed (batch send, trophy sets, base without update); the report lists each item (P13) | 10 |
  | `Usage` | Bad arguments (CLI only) | 2 |

- An error that left output behind carries an `Incomplete { path }` detail, so every
  frontend can say where the partial output is.

### 4.2 Context: cancellation and progress

```rust
pub struct Ctx<'a> {
    pub cancel: &'a Cancel,            // cheap clone of Arc<AtomicBool>
    pub progress: &'a dyn ProgressSink, // called from the worker thread
}

pub struct Progress<'a> {
    pub phase: Phase,           // Preparing, Reading, Writing, Verifying, Publishing, Sending
    pub done: u64,              // decoded/output bytes, never compressed input bytes
    pub total: Option<u64>,     // None = indeterminate
    pub item: Option<&'a str>,  // current file or package
}
```

- Cancellation is checked at every bounded chunk (64 KiB) and between entries.
  Phases that cannot be interrupted must say so (`Phase::Publishing` is
  non-interruptible).
- Progress is reported on chunk boundaries only, and sinks must be cheap. The
  frontend throttles the UI, not the library.
- Frontends compute rate and ETA. Libraries report bytes only.

### 4.3 I/O shape

- Inputs are `Read + Seek`. Formats keep **no** payload buffers larger than a
  documented bound (64 KiB default).
- Every length, offset and count from input is checked against both the input
  size and an explicit limit **before** allocation or seek.
- Limits are values in a `Limits` struct with documented defaults (the pkg-tools
  values). Tests can lower them. Callers can only lower them, never raise them
  above the defaults.

### 4.4 Output shape

All output goes through `pantheon-fs`. A format writer receives an `OutputTree`
or `OutputFile` handle and never a raw `Path`. This keeps the no-overwrite,
symlink, collision and incomplete-marker rules in one place.

## 5. Threading

- **Libraries are synchronous.** There is no async runtime (P3).
- Parallelism inside a library is opt-in and bounded. For example, a future
  parallel hash pass uses `std::thread::scope` with a fixed worker count, and
  memory per worker stays bounded.
- GUI frontends never call `ops` on the UI thread. They go through the
  `pantheon-app` job runner.
- Hermes's host side uses one thread per active HTTP connection (bounded, default
  4) plus one control connection. That is plain `std::net`.

## 6. Model pattern (frontend-agnostic UI state)

Each god's `model.rs` is a small state machine:

```rust
pub enum Intent { ChooseInput(PathBuf), SetOption(..), Start, Cancel, JobEvent(JobEvent) }
pub enum Effect { RunJob(JobSpec), PickFile(PickSpec), Notify(Message) }

impl Model {
    pub fn update(&mut self, intent: Intent) -> Vec<Effect>;
    pub fn view(&self) -> View;   // plain data: labels, enabled flags, progress, rows
}
```

- Frontends turn widget events into `Intent`s, carry out `Effect`s (open a file
  dialog, start a job), and render `View`.
- The CLI does **not** use the model. It calls `ops` directly. The model exists
  so that GTK and GPUI do not each reimplement enablement rules, validation
  messages and progress bookkeeping.
- Models are unit-tested without a display server. This replaces most of the
  graphical smoke tests in pkg-tools.

## 7. Platform matrix

| Concern | Linux | macOS | Windows |
| --- | --- | --- | --- |
| No-follow, descriptor-relative opens | `rustix` `openat` + `O_NOFOLLOW` | `rustix` (same API) | `std` + `FILE_FLAG_OPEN_REPARSE_POINT`, then reject reparse points after open |
| File publication without overwrite | `linkat` on the parent descriptor; fallback `renameat2(RENAME_NOREPLACE)`, then `create_new` ([foundation §3](03-foundation.md#3-output-files-single-file-publication)) | `linkat`; fallback `renamex_np(RENAME_EXCL)`, then `create_new` | `std::fs::hard_link`; fallback `MoveFileExW` without replace (D22), then `create_new` |
| Case-insensitive destination | ASCII-case collision check (always on) | Default FS is case-insensitive: check always on | same |
| Reserved names | portable rule set | portable rule set | `CON`, `NUL`, `COM1`…, trailing dot/space rejected |
| Path length | 4096 | 1024 | Use `\\?\` prefixed paths internally; check component limits |
| Config dir | XDG | Application Support | `%APPDATA%` |

The **portable rule set** is used on every OS, not only the host's. A tree
extracted on Linux must still be valid when copied to an exFAT USB drive or a
Windows machine. This is the existing pkg-tools policy (`pkg-fs`) and it stays.

## 8. Repository layout

```text
Pantheon/
  Cargo.toml              workspace; [workspace.lints] forbid unsafe_code (exceptions in §9)
  rust-toolchain.toml     pinned toolchain + x86_64-unknown-none target
  crates/
    core/ fs/ crypto/ json/ xml/
    psf/ ps3/ ps4/ zar/ trp/ package/
    athena/ hades/ hermes/ proteus/ dionysus/ nike/ hephaestus/
    app/ cli/ gtk/ gpui/
    testalloc/            dev-only counting allocator (D14)
  console/
    hermes-wire/ hermes-payload/
  xtask/
  fixtures/               synthetic generators + committed tiny synthetic fixtures
  reference/              independent reference check scripts/projects (carried from pkg-tools tests/reference)
  docs/
```

Default workspace members are L0–L3 plus `pantheon-cli`. GUI crates and console
crates are built explicitly, so `cargo build` and `cargo test` on a fresh
Windows or macOS machine need no system libraries.

## 9. `unsafe` policy

- `unsafe_code = "forbid"` for every crate **except** `hermes-payload`, which
  cannot avoid raw syscalls, function pointers from dynamic symbol lookup, and
  inline assembly.
- In `hermes-payload`, `unsafe` lives in one module (`sys.rs`). Each block has a
  `// SAFETY:` comment, and the rest of the payload calls only safe wrappers.
  `hermes-wire` stays `forbid(unsafe_code)`.
- `forbid` cannot be overridden by an inner `#[allow]`, so the three exception
  crates opt out of the workspace lint and set `deny(unsafe_code)` themselves.
- `pantheon-testalloc` (D14) holds one `unsafe impl GlobalAlloc` that wraps
  `std::alloc::System` and counts current and peak bytes. It is a
  `dev-dependency` only, and `cargo xtask deps --core` fails if it appears in a
  normal (non-dev) dependency graph.
- `pantheon-fs` (D22) has exactly two `unsafe` blocks, both in `src/sys/windows.rs`:
  the `MoveFileExW` no-replace rename and the `BCryptGenRandom` call behind
  `os_random` (D26). The crate uses `deny(unsafe_code)`, with an `#[allow]` on
  only those two functions. Everything else in the crate stays safe.
- That makes three exceptions in total. A fourth needs a decision-log entry.
- Crates we depend on may use `unsafe` internally (RustCrypto, `rustix`,
  `ctrlc`). The forbid rule covers our code. Their use is covered by the allowlist.
