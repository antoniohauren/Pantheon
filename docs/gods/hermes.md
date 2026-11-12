# Hermes: send to PS4

> Messenger of the gods. Hermes carries packages from the computer to a PS4 on
> the same network, using a console payload we write ourselves.

**Status: approved (D27).** Decisions it
rests on: D10 (no third-party console binary), D11 (Rust `no_std` payload), D21
(optional embed; GoldHEN BinLoader + generic TCP loader), D26 (token randomness), P7 (own wire protocol).
Items marked **Spike** are unknown until tried on a console.

## Scope (parity)

Everything pkg-tools' *Send to PS4* does (`crates/ps4-send`, README "GNOME
application"):

- Send one or more local PS4 PKGs to a console given by LAN IPv4 address.
- **Validate the whole batch before anything is sent**: package name ≤ 0x259
  bytes, content ID ≤ 0x30, category ≤ 12, icon size fits `u32`, and the
  32-byte package digest at offset `0xFE0` is readable. One bad package means
  nothing is sent.
- One payload per batch. Packages go in the given order, one after another.
- The console downloads each package from a host HTTP server with byte-range
  support. Bytes from interrupted requests count toward progress, because the
  console's installer can resume without asking for them again.
- Cancel or an error stops the batch. Packages already transferred stay on the
  console, and the report says how many.
- Long stalls are tolerated (10 min read/write timeouts per HTTP connection),
  because the installer pauses while it validates metadata.
- IPv4 only. Local files only (no remote URLs or proxying).

New compared with pkg-tools:

- **Our own payload** (D10/D11), and our own protocol (P7).
- **Three distinct outcomes per package**, never merged: *transferred* (every
  byte served), *registered* (the console's installer accepted the task and
  returned a task ID), and *installed* (only if Spike S4 works; otherwise never
  claimed). pkg-tools can only report *transferred*.
- `hermes probe`: is a supported loader reachable, and does our payload answer?
- The host only accepts connections from the console's address (see Security).

## Components

```text
host (hermes crate, std)                         console (hermes-payload, no_std)
───────────────────────────                      ────────────────────────────────
1. validate batch
2. pick local IPv4 (route to PS4)
3. bind control + HTTP listeners
4. patch config block into payload  ──TCP──►  loader (GoldHEN BinLoader / generic)
                                                 runs payload
5. accept control connection        ◄──TCP──  Hello{version, token, build id}
6. Job{package 1}                   ──────►   register install task (Spike S3)
                                    ◄──────   Registered{task id} | Failed{code}
7. HTTP range server                ◄──HTTP── installer downloads package
                                    ◄──────   Status{task, state} (Spike S4)
   … repeat 6–7 per package …
8. Bye                              ──────►   payload exits
```

- `hermes-wire` (`no_std`, `forbid(unsafe_code)`): message types, encode/decode,
  limits. The **same code** runs on both ends, so the host's tests can play the
  console side.
- `hermes` (L2): batch validation, session, control connection, HTTP range server,
  `ops`/`model`/`cli`.
- `hermes-payload`: the console program. All `unsafe` is in `sys.rs` (architecture §9).
- `xtask payload`: builds the payload, converts the ELF to a flat binary with our
  own ELF reader, and checks it (below).

## Wire protocol (`hermes-wire`, version 1)

Accepted as v1 (D34). Frames are `u32 length (LE) + u8 type + body`. Integers are little
endian. Strings are `u32 length + UTF-8 bytes`.

| Type | Direction | Body |
| --- | --- | --- |
| `Hello` | console → host | protocol version, session token (16 bytes), payload build ID (SHA-256 prefix) |
| `Job` | host → console | job index, manifest URL, package name, content ID, package type (`PS4` + category), size, digest, icon (optional) |
| `Registered` | console → host | job index, installer task ID |
| `Failed` | console → host | job index, stage (`register`/`status`), raw console error code (`u32`) |
| `Status` | console → host | job index, state (`queued`/`downloading`/`installing`/`done`/`error`), optional bytes done, error code |
| `Bye` | host → console | none. The payload exits cleanly |

- Limits: frame ≤ 64 KiB, except `Job`, which is ≤ 4 MiB + 64 KiB because of the
  icon. Strings use the batch-validation limits above. Unknown frame types,
  oversize frames and trailing bytes are `Malformed`, and the session closes.
- A version mismatch in `Hello` is a clear error ("payload is version N, host
  expects M"), never a guess.
- The decoder is fuzzed (D20), and must not allocate before the length is checked.

**Config block.** The payload contains one marked block,
`b"HRMSCFG1" + version + host IPv4 + control port + token`, which the host
patches before sending. `xtask payload` checks that the marker appears exactly
once, as pkg-tools' test `payload_has_one_callback_marker` does.

## Payload (`hermes-payload`)

- Target `x86_64-unknown-none`, `no_std`, `panic = "abort"`, no allocator, or a
  fixed arena if a spike shows one is needed. Position-independent. Linked by
  `rust-lld`.
- `xtask payload` output checks: entry point at offset 0 of the flat binary, no
  relocations left that the loader would have to apply, size under a set budget
  (64 KiB, D27), config marker exactly once, and a reproducible build
  (same source + toolchain = same bytes, recorded as the build ID).
- Behaviour: connect to the host, send `Hello`, then loop: receive `Job`, register
  the install task, report `Registered`/`Failed`, poll status if S4 works, then
  exit on `Bye`, on connection loss, or after an idle timeout (5 min, D27).
  It must never leave a thread or socket behind in the host process it runs in.

**Spikes (do these first, before writing production code):**

| ID | Question |
| --- | --- |
| S1 | Loader entry contract: what GoldHEN BinLoader and the generic loader pass to the entry point, and what memory and permissions it gets. |
| S2 | How the payload finds system functions (sockets, module loading, symbol lookup) without OpenOrbis or C. |
| S3 | The installer call that registers a download task from a manifest URL, with its arguments and error codes. |
| S4 | Whether task status/progress can be polled after registration. If not, *installed* is never reported. |
| S5 | Whether `build-std` is needed (`core` only should be enough), and whether `rust-src` goes into `rust-toolchain.toml`. |

**Clean-room rule (because of D13, MIT OR Apache-2.0).** DirectPackageInstaller's
payload is GPL-3. Its **source must not be read while writing `hermes-payload`**,
and no code may be copied from it. We may use: public documentation of PS4 system
interfaces (e.g. psdevwiki), our own console experiments, and the **observed**
behaviour of pkg-tools (the host side, which is ours). Each spike records its
sources in `docs/hermes-provenance.md`, in the same way as
`fpkg-authoring-provenance.md`.

## Security

The host opens two listening ports on the LAN for the length of a batch.

- Both listeners bind to the one local IPv4 address that routes to the console,
  never `0.0.0.0`. They close when the batch ends or is cancelled.
- **Peer check:** connections whose source address is not the console's IP are
  closed at once, and they never count as the payload's connection.
- **Session token:** 128 bits, sent in the patched payload and required in
  `Hello`. HTTP paths are `/<token-hex>/<n>/package.pkg` and `…/package.json`.
  Any other path gets `404`. (pkg-tools uses a timestamp, which can be guessed.)
  The token comes from `pantheon_fs::os_random` (D26, the OS cryptographic
  source). It stops a **blind** attacker on the LAN. It is not secret from anyone
  who can sniff the LAN, because the protocol is plaintext like every PS4 homebrew
  transfer. The docs and the UI must not claim more than that.
- The HTTP server serves only `GET`/`HEAD` for the current job's package and
  manifest. Request header lines ≤ 8 KiB, at most 4 concurrent connections (arch §5).
- Nothing from the console is trusted: frames are bounded (above), console error
  codes are shown as raw numbers, and console strings are never used as paths.
- Logs never include the token or package digests.

## Library

```rust
pub struct Target { pub ps4: Ipv4Addr, pub loader: Loader }        // Loader::GoldHen | Loader::Tcp { port }
pub fn probe(target: &Target, payload: &Payload) -> Result<ProbeReport, Error>;
pub fn send(target: &Target, payload: &Payload, packages: &[PathBuf], ctx: &Ctx) -> Result<SendReport, Error>;
pub struct SendReport { pub items: Vec<ItemReport> }                // per package: transferred / registered / installed / error
pub enum Payload { Embedded, File(PathBuf) }                        // Embedded only with feature `embed-payload` (D21)
```

- `Loader::GoldHen` tries ports 9090, 9021 and 9020 in that order (pkg-tools
  behaviour). `Loader::Tcp` uses only the given port.
- Package identity (name, content ID, category, icon, digest) comes from
  `pantheon-package`, not from Hermes's own parsing.
- An error after the first package is `Partial` (P13), and `SendReport` lists
  every item.

## CLI

```text
hermes send --ps4 IP PKG… [--loader goldhen|tcp:PORT] [--payload FILE] [--json]
hermes probe --ps4 IP [--loader goldhen|tcp:PORT] [--payload FILE]
```

`--payload` is required when the binary was built without `embed-payload`. JSON
schema `pantheon.hermes.send/1`. Exit codes: `0` all registered (or transferred,
if registration cannot be reported), `9` network, `10` partial.

## Model / GUI

Available from startup with no package loaded (pkg-tools behaviour). Console IP
field (remembered in settings), *Send* for the loaded package, *Send Multiple…*,
and a per-package list with state and transferred bytes. The finished screen
states plainly what is known: "Transferred and registered. Check the console for
installation." *Installed* appears only if S4 works.

## Tests

- `hermes-wire`: round trip of every message, limit and truncation sweeps, fuzzing (D20).
- Host session against a **fake console**: an in-process client built on
  `hermes-wire` that plays the payload side over loopback, including resume
  after an interrupted range, a stall, a wrong token, a foreign peer address,
  cancellation mid-batch and console `Failed` frames.
- HTTP range parsing and `416` cases carried over from pkg-tools.
- `xtask payload` checks (above) in Linux CI.
- Real target: batch send to the owner's console, logged in
  `docs/verification-log.md` (date, firmware, loader version, payload build ID,
  outcome per package).

## References

pkg-tools `crates/ps4-send/src/lib.rs` (host behaviour, at `20e3c3a`) and README
"GNOME application" (Send to PS4). The DPI payload is **not** a reference (clean-room rule).
