//! The console layer: the only module where `unsafe` is allowed (architecture
//! §9), item by item, each with a `SAFETY:` comment.
//!
//! Everything that depends on the PS4 is a stub until its spike is done
//! (docs/hermes-provenance.md). No console ABI detail (entry arguments,
//! syscall numbers, NIDs, installer functions, error codes) is guessed here:
//! a stub returns `SysError::NotImplemented(spike)` and the session ends.
//!
//! S1 assumption (owner, 2026-11-12): the loader is GoldHEN BinLoader. What it
//! passes to `_start`, how much memory and stack the payload gets, and how a
//! payload must end are not known yet.

use hermes_payload::{Conn, Console, Exit, Polled, RegisterError, Spike, SysError};
use hermes_wire::Job;
use hermes_wire::config::{self, FIELDS_LEN, MARKER_LEN};

/// The config block (marker + version, zeros), patched in the flat binary by
/// `xtask payload` (build ID) and the host (address, port, token). `#[used]`
/// keeps it even though only volatile reads touch it.
#[used]
static CONFIG: [u8; config::LEN] = config::template();

/// The block's fields as patched in the loaded image.
pub fn config_fields() -> [u8; FIELDS_LEN] {
    #[allow(unsafe_code)]
    // SAFETY: `&raw const CONFIG` is a valid, aligned pointer to an
    // initialised `[u8; LEN]` static that lives for the whole program and is
    // never written by this program, so a read is always in bounds and never
    // races. The read is volatile because the host patched the bytes in the
    // binary after compilation: the compiler must load them, not fold the
    // `template()` initialiser.
    let block = unsafe { core::ptr::read_volatile(&raw const CONFIG) };
    let mut fields = [0u8; FIELDS_LEN];
    for (dst, src) in fields.iter_mut().zip(block.iter().skip(MARKER_LEN)) {
        *dst = *src;
    }
    fields
}

/// The entry point the loader jumps to, placed first in the image by
/// `link.ld`. S1: the real contract (arguments, return or exit) is unknown;
/// until then it takes nothing and returns to its caller.
#[allow(unsafe_code)]
// SAFETY: `no_mangle` exports the one symbol named `_start` in this binary
// (no libc or runtime is linked), so it cannot clash with another definition.
// `link_section` only names the section `link.ld` places first.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.hermes_entry")]
pub extern "C" fn _start() {
    crate::main();
}

/// Ends the payload after a session. S1: how a payload must end (return to
/// the loader, or an exit call) is unknown; this returns to `_start`.
pub fn exit(_why: Exit) {}

/// Stops after a panic. S1: there is no known abort primitive yet, so this
/// parks the thread. With `panic = "abort"` and no panicking calls in the
/// session, it should be unreachable.
pub fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

/// The receive arena, [`hermes_payload::RECV_ARENA_LEN`] bytes (D37). S1:
/// where the payload's memory comes from (a loader-provided region, a mapped
/// page range) is unknown, so there is none yet and the payload ends at once.
pub fn recv_arena() -> Option<&'static mut [u8]> {
    None
}

/// The PS4 console layer. Every method waits for a spike.
pub struct Ps4;

/// A TCP connection to the host. S2: socket functions are not resolved yet.
pub struct Socket;

impl Conn for Socket {
    fn recv(&mut self, _buf: &mut [u8], _wait_ms: u32) -> Result<usize, SysError> {
        Err(SysError::NotImplemented(Spike::S2))
    }

    fn send_all(&mut self, _data: &[u8]) -> Result<(), SysError> {
        Err(SysError::NotImplemented(Spike::S2))
    }
}

impl Console for Ps4 {
    type Conn = Socket;

    /// S2: needs the socket functions (and S1: how to find them).
    fn connect(&mut self, _host: [u8; 4], _port: u16) -> Result<Socket, SysError> {
        Err(SysError::NotImplemented(Spike::S2))
    }

    /// S2: needs a monotonic clock function.
    fn now_ms(&mut self) -> Result<u64, SysError> {
        Err(SysError::NotImplemented(Spike::S2))
    }

    /// S3: needs the installer call that registers a download task.
    fn register(&mut self, _job: &Job<'_>) -> Result<u64, RegisterError> {
        Err(RegisterError::Sys(SysError::NotImplemented(Spike::S3)))
    }

    /// S4: needs a way to read a task's status; until then, never "installed".
    fn poll(&mut self, _task_id: u64) -> Result<Polled, SysError> {
        Err(SysError::NotImplemented(Spike::S4))
    }
}
