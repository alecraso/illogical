// Release builds embed web/dist (rust-embed). Rebuild when it changes, or a
// fresh `just web` would ship the previous bundle.
fn main() {
    println!("cargo:rerun-if-changed=../../web/dist");
}
