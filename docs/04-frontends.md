# Frontends, apps and CLI

The GUI is **not** the current priority (D7). This document defines the seams
now, so that the core needs no change when GTK and GPUI arrive.

## 1. Product matrix

| God | Library | CLI binary | `pantheon` subcommand | GTK app | GPUI app | Hub section |
| --- | --- | --- | --- | --- | --- | --- |
| Athena | `athena` | `athena` | `pantheon athena` | Athena | Athena | Overview + All Info |
| Hades | `hades` | `hades` | `pantheon hades` | Hades | Hades | Explore |
| Hermes | `hermes` | `hermes` | `pantheon hermes` | Hermes | Hermes | Send |
| Proteus | `proteus` | `proteus` | `pantheon proteus` | Proteus | Proteus | Convert (PS4) |
| Dionysus | `dionysus` | `dionysus` | `pantheon dionysus` | Dionysus | Dionysus | Liberate (PS3) |
| Nike | `nike` | `nike` | `pantheon nike` | Nike | Nike | Trophies |
| Hephaestus | `hephaestus` | `hephaestus` | `pantheon hephaestus` | Hephaestus | Hephaestus | Forge (PS4) |
| — | — | `pantheon` | — | **Pantheon** | **Pantheon** | (the hub) |

Binaries ship with bare names (D17). A distribution whose `PATH` already has
another `hermes` (or `hades`, …) may rename ours, and `pantheon <god>` always works.

## 2. How hub and standalone apps share code

```text
pantheon-gtk/
  src/lib.rs            shell: window, header bar, sidebar, file drop, job/progress dialog
  src/gods/athena.rs    fn page(ctx: &Shell, model: athena::Model) -> gtk::Widget
  src/gods/hades.rs     …one module per god
  src/bin/pantheon.rs   Shell::run(Registry::all())
  src/bin/hermes.rs     Shell::run(Registry::only(HERMES))
  …
```

- A **god page** is the only god-specific UI code. It renders `model.view()` and
  sends `Intent`s.
- The **shell** owns everything shared: the open-file action, drag-and-drop, the
  progress/cancel dialog, error alerts, the about dialog and settings. A
  standalone app is the shell with one god and no sidebar.
- In the hub, the **session** (the loaded package) is shared. Opening a PS4 PKG once
  makes it available to Athena, Hades, Hermes, Proteus, Nike and Hephaestus.
  Gods whose `accepts(kind)` rejects the package show as disabled, and their
  tooltip says why (e.g. "Dionysus: PS3 packages only").
- Drag-and-drop routing: the hub sends a dropped file to the session. A
  standalone app sends it to its god, which may reject it with a message. Drops
  are ignored while a job runs (current pkg-tools behaviour).
- One shared settings file. Each app reads its own section and the shell section
  (theme, artwork preference such as *Use PS4 pic1.png*). The hub and standalone
  apps can run at the same time, so a save re-reads the file, replaces only the
  writer's own sections, and publishes by atomic replace (P16). Last writer wins
  per section, and no section is lost.

The GPUI frontend has the **same structure**: `pantheon-gpui/src/gods/*.rs`
plus `src/bin/*.rs`. Because both frontends render the same `View` data, a
feature added to a model appears in both. Parity is reviewed per god in the
roadmap.

## 3. GTK frontend (`pantheon-gtk`)

- GTK 4.10+, libadwaita 1.5+ (current pkg-tools minimums). Linux is the target.
  GTK on Windows/macOS is not promised, because GPUI covers those (D7).
- Widgets are built in Rust code (P10), so no `blueprint-compiler` is needed.
- Long work goes through the `pantheon-app` job runner. Results come back to the
  main loop through `glib::MainContext::channel` or an idle source that polls
  the `mpsc` receiver (whichever the pinned gtk-rs version supports).
- Carried over from pkg-tools' UI behaviour: modal progress with written/total
  MB, MB/s and Cancel; an indeterminate bar until the total is known; partial
  output paths in errors; virtualised Explore rows with expansion-time child
  models; selection with keyboard and Ctrl/Shift-click.
- Desktop files: one per app (hub + 7 gods) with app IDs `io.github.antoniohauren.Pantheon` and `io.github.antoniohauren.<God>` (D23). A distribution
  may install only some of them.
- **Flatpak (D24):** file access goes through the document portal, so
  `pantheon-fs` paths may sit on a FUSE mount where hard links fail. The fallback
  chain in [foundation §3](03-foundation.md#3-output-files-single-file-publication) covers this, and the Phase 9 smoke
  test runs inside the Flatpak. Hermes needs `--share=network`.

## 4. GPUI frontend (`pantheon-gpui`)

- Targets Linux, Windows and macOS (D7), the way Zed does.
- GPUI is pre-1.0. Pinned to an exact crates.io release (D25). The adapter stays thin, so an API change touches
  only `pantheon-gpui`.
- The job runner integration uses GPUI's executor to poll the `mpsc` receiver
  (spawn on the foreground executor). There is no GPUI type in `pantheon-app`.
- Native file dialogs come from GPUI's platform layer. The model emits
  `Effect::PickFile`/`PickFolder` with filters, and each frontend shows the dialog.
- Large lists (Explore, trophies, batch send) need virtualised list support in
  GPUI (`uniform_list` or equivalent). This is a Phase 10 spike item.

## 5. What the model must expose (checklist for every god)

- Inputs chosen, with validation messages.
- Action enablement with **reasons** (a disabled button explains itself).
- Job state: idle, preparing (indeterminate), running(done, total, item),
  cancelling, finished(report), failed(error, incomplete path?).
- Report data for the result screen (e.g. "3 of 5 packages sent", "base saved at…").
- Nothing the frontend must compute from format knowledge.

## 6. CLI family (`pantheon-cli`)

### 6.1 Shape

```text
<god> <verb> [ARGS] [OPTIONS]
pantheon <god> <verb> [ARGS] [OPTIONS]
pantheon help [<god> [<verb>]]
pantheon --version
```

`pantheon hermes send …` and `hermes send …` run the **same** function
(`hermes::cli::run`). The multiplexer only strips the first argument. Per-god
binaries are thin `main`s (P11).

### 6.2 Common options

| Option | Meaning |
| --- | --- |
| `--json` | Machine-readable output on stdout using the god's versioned schema; diagnostics stay on stderr |
| `--quiet` / `-q` | No progress |
| `--progress=auto\|bar\|lines\|none` | `auto` = bar on a TTY, none otherwise; `lines` for scripts that log |
| `--` | End of options |
| `-h`, `--help` | Help for that god/verb |

Ctrl+C sets the job's `Cancel`. A second Ctrl+C exits immediately, and the
incomplete marker remains. `std` has no portable signal API, so the handler
comes from the `ctrlc` crate (D15), which covers SIGINT/SIGTERM on Unix and
console control events on Windows. Only `pantheon-cli` depends on it. The
handler only sets the `Cancel` flag (or exits on the second press). It never
touches output.

### 6.3 Argument parsing

Our own small parser (P4), in `pantheon-args` (D36): it handles long/short flags, `--key=value` and
`--key value`, positionals, `--`, and non-UTF-8 paths through `OsString`. Help
text is generated from a static per-verb spec, so `help` and parsing cannot drift
apart.

### 6.4 Command map (pkg-tools to Pantheon)

| pkg-tools | Pantheon |
| --- | --- |
| `inspect INPUT [--json]` | `athena inspect INPUT [--json]` |
| (GUI only) | `athena artwork INPUT --icon0\|--pic1 OUT.png` (D29) |
| `extract INPUT OUT` | `hades extract INPUT OUT [--select PATH]…` (P12) |
| (GUI Explore) | `hades list INPUT [--json]` |
| (GUI Send) | `hermes send --ps4 IP PKG… [--loader goldhen\|tcp:PORT] [--payload FILE]` |
| — | `hermes probe --ps4 IP` (checks that the loader is reachable and our payload answers; D29) |
| `pack DIR OUT.zar` | `proteus pack DIR OUT.zar` |
| `convert GAME.pkg OUT.zar [--update U.pkg]` | `proteus convert GAME.pkg OUT.zar [--update U.pkg]` |
| `liberate GAME.pkg LICENSE.rap OUT.pkg` | `dionysus liberate SRC --rap R (--to-pkg OUT.pkg \| --to-folder PARENT)`; `--rap` optional, auto-found as in GUI |
| (GUI pre-check) | `dionysus check SRC` (needs liberation? which RAP matches?) |
| `trophies INPUT [--json]` | `nike list INPUT [--json]`, `nike icons INPUT OUTDIR` (D29) |
| `gp4 create/preflight/image` | `hephaestus gp4 create/preflight/image` |
| `fpkg preflight/rebuild` | `hephaestus fpkg preflight/rebuild` |

The rows marked D29 are new commands, each a small addition on top of existing
library functions.

### 6.5 Exit codes

From `ErrorKind` ([architecture §4.1](01-architecture.md#41-errors)). `0` means the
operation completed. For inspect-style commands, missing or encrypted optional
metadata is reported and still exits `0` (current pkg-tools rule).

Batch commands (`hermes send` with several packages, `nike list` with several
sets, `proteus convert --update`) exit `0` only if every item succeeded, the
error's own code if every item failed the same way, and `10` (`Partial`, P13)
otherwise. With `--json`, the report lists every item with its own status, and
it is written even when the exit code is non-zero.
