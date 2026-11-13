//! Links the payload binary with `link.ld` (entry first, at address 0) when it
//! is built for the console target. Host builds of the library are unaffected.

const CONSOLE_TARGET: &str = "x86_64-unknown-none";

fn main() {
    println!("cargo:rerun-if-changed=link.ld");
    let target = std::env::var("TARGET").unwrap_or_default();
    if target == CONSOLE_TARGET {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        println!("cargo:rustc-link-arg-bins=-T{dir}/link.ld");
    }
}
