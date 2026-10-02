// Release builds embed web/dist (rust-embed). Rebuild when it changes, or a
// fresh `just web` would ship the previous bundle.
fn main() {
    println!("cargo:rerun-if-changed=../../web/dist");
    // Debug builds read web/dist at run time; a release build without it
    // would serve no page at all, so stop and say why.
    if std::env::var("PROFILE").as_deref() == Ok("release")
        && !std::path::Path::new("../../web/dist/index.html").exists()
    {
        panic!("web/dist is missing: build the web client first (`just web`, or build with `just build`)");
    }
}
