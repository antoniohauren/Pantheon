# Hermes provenance and spikes

Started 2026-11-12, for Phase 4 ([Hermes](gods/hermes.md), D27). This file
records, for every part of Hermes, **where its knowledge came from**, in the
same way as pkg-tools' `docs/fpkg-authoring-provenance.md`. It also holds the
spike results (S5 below) and the plans for the spikes that need a console
(S1–S4). Console results themselves go in
[`verification-log.md`](verification-log.md) (real-target evidence only);
this file only links to them.

## Clean-room rule (D13, D27)

DirectPackageInstaller's payload is GPL-3. Its source was **not read,
searched or fetched**, and no code was copied from it. No web research was
done for this work at all.

Allowed sources: public documentation of PS4 system interfaces (for example
psdevwiki), our own console experiments, the **observed** behaviour of
pkg-tools' host side (`~/Projects/pkg-tools` at `20e3c3a`, which is ours), and
this repository's docs. Every future source gets a row in the table below
before anything taken from it is committed.

## Sources used

| Part | Source | What was taken |
| --- | --- | --- |
| Scope, protocol, payload rules, security | [`gods/hermes.md`](gods/hermes.md), [`00-decisions.md`](00-decisions.md) (D10, D11, D21, D26, D27, D33, D34) | The design. |
| Host behaviour | pkg-tools `crates/ps4-send/src/lib.rs` at `20e3c3a` (our code; behaviour observed, not ported line by line) | Batch validation limits (name ≤ 0x259, content ID ≤ 0x30, category ≤ 12, icon fits `u32`), digest at `0xFE0`; GoldHEN ports 9090, 9021, 9020; 3 s loader connect and 5 s write timeouts; 15 s connect-back wait; 10 min HTTP read/write timeouts; local address by UDP `connect`; package type `"PS4"` + upper-cased category; the download manifest's JSON shape (`originalFileSize`, `packageDigest` in upper-case hex, `numberOfSplitFiles`, one piece with a zero `hashValue`); single-range parsing, `416` with `Content-Range: bytes */N`; counting unique served bytes across interrupted requests; which client errors are disconnects. |
| Package identity fallbacks | pkg-tools `crates/pkg-gtk/src/package.rs` at `20e3c3a` | Title from SFO `TITLE`, else the file name; category from SFO `CATEGORY`, else `gd`; icon from `icon0.png` when readable. |
| Payload build | The pinned Rust toolchain and `rust-lld`, by local experiment (S5 below) | Nothing copied; only what the toolchain does. |

What is **ours, new**: the v1 wire protocol and config block
(`console/hermes-wire`), the payload's session loop and its console
abstraction (`console/hermes-payload`), the host session, HTTP server, peer
check and session token (`crates/hermes`), and the ELF checks in
`xtask payload`. None of these use any console ABI detail: there is none in
the repository yet.

## Implementation details (accepted as D37)

`hermes.md` left these open. The owner accepted items 1–4 as proposed and
chose the fixes in items 5 and 6 (`00-decisions.md` D37).

1. **Config block has a build-ID field.** The payload must send its build ID
   in `Hello`, so it has to carry it: the block is
   `b"HRMSCFG1" | version u32 | host IPv4 | control port u16 | token [16] | build ID [16]`.
   `xtask payload` stamps the build ID; the host patches the rest.
2. **Build ID** = first 16 bytes of SHA-256 of the flat image as built
   (config block unpatched, build-ID field zero).
3. **Frame length counts the type byte** (`length = 1 + body`), so length 0
   is malformed. Limits apply to that value.
4. **Task ID is `u64` on the wire** (its native width is S3); `Status` always
   carries a `u32` error code; optional fields use a `0`/`1` flag byte.
5. **Idle timeout vs. long transfers.** D27's 5-minute idle timeout would end
   the payload while the installer is still downloading a large package. The
   host now sends `Keepalive` (type 7, empty) every 60 s while a job is in
   flight, so the payload only times out when its host is gone. If the control
   connection is lost anyway, the host keeps serving the current, already
   registered package to the end, and stops the batch only before the next
   `Job` (reported as `Partial`). The HTTP server also closes a connection
   that has sent nothing for 60 s when all four are taken, so half-open
   sockets cannot lock the installer out of resuming.
6. **Receive buffer.** The payload receives into a fixed arena of
   `MAX_JOB_FRAME` (4 MiB + 64 KiB), `RECV_ARENA_LEN`, provided by
   `sys::recv_arena`. S1 decides where that memory comes from; until then the
   stub returns none and the payload ends at once.

## S5: is `build-std` needed, and does `rust-src` go into `rust-toolchain.toml`?

**Result: `build-std` is not needed and `rust-src` does not go into
`rust-toolchain.toml`**, provided the toolchain has the `x86_64-unknown-none`
target's precompiled `core`, which `rust-toolchain.toml` already requests
(`targets = ["x86_64-unknown-none"]`). `rust-toolchain.toml` was not changed.
**Evidence kind:** local experiment, with a host-target proxy for the link
step. Confirmation on the real target is still open (see below).

Experiment, 2026-11-12, Linux (Arch), `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
installed as the distribution package (no rustup, so `rust-toolchain.toml` is
not applied and the `x86_64-unknown-none` standard library is not installed):

1. `cargo build --release --manifest-path console/hermes-payload/Cargo.toml --target x86_64-unknown-none`
   fails: `error[E0463]: can't find crate for 'core'` / "the
   `x86_64-unknown-none` target may not be installed". So `cargo xtask payload`
   cannot run on this machine; it stops with that message and exit code 8.
2. `RUSTC_BOOTSTRAP=1 cargo build -Zbuild-std=core …` fails:
   `"/usr/lib/rustlib/src/rust/library/Cargo.lock" does not exist … try: rustup component add rust-src`.
   `build-std` would need nightly (or the bootstrap override) **and**
   `rust-src`, so it is the worse option.
3. Proxy: the same payload built for the host target
   `x86_64-unknown-linux-gnu`, whose precompiled `core` is installed, linked
   by the toolchain's `rust-lld` with `link.ld`
   (`RUSTFLAGS="-C linker=/usr/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld -C linker-flavor=ld.lld -C relocation-model=pic -C link-arg=-T…/link.ld -C link-arg=-pie -C link-arg=--no-dynamic-linker -C link-arg=-znorelro -C link-arg=--gc-sections"`).
   It needs only `core` (no `alloc`, no `std`). `xtask`'s ELF reader and
   checks on the result: flat image 344 bytes, entry at offset 0, 0
   relocations, no `PT_INTERP`, config marker exactly once
   (`strings | grep -c HRMSCFG1` = 1), and two clean builds were
   byte-identical (`cmp`). With every console call a stub, the optimiser
   reduces the session to the volatile config read and a return, so the size
   says nothing about the finished payload.

Still open for S5: run `cargo xtask payload` once on a rustup toolchain (which
installs the pinned target) and record the flat size and build ID here.
The payload build stays out of the default gates (`cargo build`/`test`) as
D21 requires.

## S1–S4: plans for the owner's console

General rules for every spike:

- Console: the owner's PS4 with **GoldHEN BinLoader** (the S1 assumption, set
  by the owner on 2026-11-12). Record firmware version, GoldHEN version, the
  payload's build ID (`hermes probe` prints it) and the host OS.
- Packages: homebrew or self-built test packages only. No game data in the
  repository, ever.
- Sources: write each document you used (URL, revision or date read, what you
  took from it) into the table above **before** committing code based on it.
  Never DPI's source.
- Results: a dated row per experiment in `verification-log.md` (evidence kind
  "real target"), linked from the spike's section here. Raw console values
  (return codes, register values) are written down exactly, as hex.
- Code: each finding replaces exactly one stub in
  `console/hermes-payload/src/sys.rs` (listed below), with a `// SAFETY:`
  comment per `unsafe` block.

### S1: loader entry contract (GoldHEN BinLoader)

Question: what does BinLoader pass to the entry point, where and how is the
image loaded, and how must a payload end?

1. **Return test.** Build a payload whose `_start` returns at once (the
   current binary does exactly that, since every call is a stub). Send it
   with `hermes probe --ps4 IP --payload target/hermes-payload/hermes-payload.bin`.
   Observe: does the loader accept it, does the console stay responsive, can
   a second payload be sent right after? Expected host output: "the payload
   did not connect back" (it has no sockets yet).
2. **Spin test.** A variant whose `_start` loops forever. Observe whether the
   loader port still accepts a second payload (it then runs payloads on their
   own thread or process) and whether the system stays usable.
3. **Register dump** (needs S2's socket calls). Once a payload can connect to
   the host, send `rdi`, `rsi`, `rdx`, `rcx`, `r8`, `r9`, `rsp` and the entry
   address as raw bytes to a host listener, and record them. Observe the
   stack alignment and how much stack is usable.
4. **Memory.** Observe whether the image is writable (the config block is
   read-only today), whether bytes past the file size (`.bss`) are mapped and
   zero, and the largest image the loader accepts; this decides
   where the 4 MiB + 64 KiB receive arena (`RECV_ARENA_LEN`) can live.

Record: per test, what was sent (build ID), what the console did, what the
host printed.

### S2: finding system functions without OpenOrbis or C

Question: how does the payload reach sockets, a clock, module loading and
symbol lookup?

1. From public documentation (psdevwiki pages on the PS4 kernel interface),
   list the candidate mechanism: direct `syscall` instructions, or symbol
   lookup through what S1 found at the entry point. Write down every number
   or name you take, with its source row above.
2. Smallest proof: one harmless call (for example the process ID) whose
   result is visible from the host. With sockets not yet working, make the
   result observable by behaviour (return vs spin, as in S1).
3. Then `socket` / `connect` / `send` / `recv` with a timeout to the host's
   control port: `hermes probe` succeeds when `Hello` arrives with the right
   token, and prints the build ID.
4. A monotonic millisecond clock for the idle timeout.

Record: each function, how it was found, its source, and the probe output.

### S3: the installer call that registers a download task

Question: which call registers a download from a manifest URL, with which
arguments, and which codes does it return?

1. From public documentation, identify the installer (background download)
   interface the manifest format implies. Record names, argument layouts and
   their source.
2. Register one small homebrew package with `hermes send --ps4 IP PKG --payload …`.
   Record the return value and the task ID, and whether the console's
   download queue shows the package with its title and icon.
3. Provoke failures and record each raw code: an unreachable URL (stop the
   host), a package whose content ID is already installed, insufficient
   space if practical, a malformed manifest. These codes are shown raw by
   Hermes and are never interpreted.
4. Observe how long the installer pauses between requests (the host allows
   10 min per connection) and whether it uses range requests and resumes.

### S4: can task status be polled after registration?

Question: can the payload read a task's state and progress after
registering it? If not, Hermes never reports *installed*.

1. From public documentation, look for a task status or progress query for
   the interface found in S3.
2. Poll it once per second during a transfer; record every state value and
   byte count seen, and how they map to `queued` / `downloading` /
   `installing` / `done` / `error`.
3. Check what it reports after installation finishes, after a failure, and
   after the payload reconnects in a new session.
4. If no such query exists or it is unreliable, leave `poll` returning
   `NotImplemented(S4)`: the host then never claims *installed*.

## Stubs waiting for S1–S4

All in `console/hermes-payload/`; each returns
`SysError::NotImplemented(spike)` (or does the minimum noted), so the session
ends cleanly instead of guessing.

| Stub | Spike | Now |
| --- | --- | --- |
| `sys.rs` `_start` | S1 | Takes no arguments; runs one session; returns to the loader. |
| `sys.rs` `exit` | S1 | Does nothing; `_start` returns. |
| `sys.rs` `halt` (panic handler) | S1 | Spins; no abort primitive known. |
| `sys.rs` `recv_arena` | S1 | Returns `None`: no memory source for the 4 MiB + 64 KiB arena is known yet. |
| `link.ld` | S1 | Flat image at address 0, entry first. |
| `sys.rs` `Socket::recv`, `Socket::send_all` | S2 | `NotImplemented(S2)`. |
| `sys.rs` `Ps4::connect`, `Ps4::now_ms` | S2 | `NotImplemented(S2)`. |
| `sys.rs` `Ps4::register` | S3 | `NotImplemented(S3)`: the session ends; no `Failed` frame with an invented code. |
| `sys.rs` `Ps4::poll` | S4 | `NotImplemented(S4)`: polling turns off; *installed* is never reported. |
