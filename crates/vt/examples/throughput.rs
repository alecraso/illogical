//! Feed throughput of the engine: `cargo run --release --example throughput`.
use std::time::Instant;

use arugula_vt::{GhosttyEngine, VtEngine};

fn main() {
    let cases: [(&str, Vec<u8>); 3] = [
        ("one long wrapped line", vec![b'x'; 32 << 20]),
        ("short lines", b"hello world 0123456789\r\n".repeat((32 << 20) / 24)),
        ("colored lines", b"\x1b[31mred\x1b[0m \x1b[1;32mgreen\x1b[0m plain text here\r\n".repeat((32 << 20) / 48)),
    ];
    for (name, data) in cases {
        let mut e = GhosttyEngine::new(120, 40);
        let t = Instant::now();
        for chunk in data.chunks(4096) {
            e.feed(chunk);
        }
        let s = t.elapsed().as_secs_f64();
        println!("{name:22} {:>5} MB in {s:6.2}s = {:7.1} MB/s", data.len() >> 20, data.len() as f64 / s / 1e6);
    }
}
