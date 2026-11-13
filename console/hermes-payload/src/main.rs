//! The Hermes payload binary: `x86_64-unknown-none`, `no_std`, no allocator.
//!
//! The entry point and every other `unsafe` item live in `sys.rs`
//! (architecture §9). This file stays safe.

#![no_std]
#![no_main]

mod sys;

use hermes_wire::config::Config;

/// Reads the patched config block and runs one session. An unpatched or
/// foreign block ends the payload at once.
fn main() {
    let Ok(cfg) = Config::decode_fields(&sys::config_fields()) else {
        return;
    };
    let Some(arena) = sys::recv_arena() else {
        return;
    };
    let exit = hermes_payload::run(&mut sys::Ps4, &cfg, arena);
    sys::exit(exit);
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    sys::halt()
}
