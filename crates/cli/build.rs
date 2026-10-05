//! The main thread's stack: Windows gives it 1 MiB, and the CLI's command
//! dispatch (one large `main`, unoptimized in debug builds) needs more.
//! Linux and macOS give 8 MiB; so does this, on Windows (M57).
fn main() {
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target == "windows" && env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
