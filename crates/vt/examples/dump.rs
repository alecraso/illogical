//! Print an engine snapshot of a fixture with escapes visible:
//! `cargo run --example dump -- modes`.
use arugula_vt::{GhosttyEngine, VtEngine};

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "modes".into());
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");
    let bytes = std::fs::read(format!("{dir}/{name}.bin")).unwrap();
    let mut e = GhosttyEngine::new(100, 30);
    e.feed(&bytes);
    let snap = e.snapshot();
    let shown: String = String::from_utf8_lossy(&snap).replace('\x1b', "⎋").replace('\r', "␍").replace('\n', "␊\n");
    let start = shown.rfind("⎋]4;255").map(|i| i + 30).unwrap_or(0);
    print!("{}", &shown[start..]);
}
