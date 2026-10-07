//! Time of a wire snapshot: `cargo run --release --example snapshot_bench`.
//! Median of 20 over the fixtures, and over a pane with deep scrollback,
//! whole and capped the way clients ask for it.
use std::{path::Path, time::Instant};

use arugula_vt::{GhosttyEngine, VtEngine};

fn load(name: &str) -> GhosttyEngine {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let bytes = std::fs::read(dir.join(format!("{name}.bin"))).unwrap();
    let meta = std::fs::read_to_string(dir.join(format!("{name}.json"))).unwrap();
    let num = |key: &str, from: &str| -> Vec<usize> {
        from.split(&format!("\"{key}\": "))
            .skip(1)
            .map(|s| s.split(|c: char| !c.is_ascii_digit()).next().unwrap().parse().unwrap())
            .collect()
    };
    let (cols, rows) = (num("cols", &meta), num("rows", &meta));
    let offsets = num("offset", &meta);
    let mut e = GhosttyEngine::new(cols[0] as u16, rows[0] as u16);
    let mut pos = 0;
    for (i, off) in offsets.iter().enumerate() {
        e.feed(&bytes[pos..*off]);
        e.resize(cols[i + 1] as u16, rows[i + 1] as u16);
        pos = *off;
    }
    e.feed(&bytes[pos..]);
    e
}

/// Median time of `snap` on `e`, 20 times: each after a change (blinking
/// the cursor off and on, so that nothing is reused from the last one), or
/// all on an unchanged pane.
fn median(e: &mut GhosttyEngine, changed: bool, snap: impl Fn(&mut GhosttyEngine) -> Vec<u8>) -> (f64, usize) {
    let mut len = 0;
    let mut v: Vec<f64> = (0..20)
        .map(|i| {
            if changed {
                e.feed(if i % 2 == 0 { b"\x1b[?12h" } else { b"\x1b[?12l" });
            }
            let t = Instant::now();
            len = snap(e).len();
            t.elapsed().as_secs_f64() * 1e3
        })
        .collect();
    v.sort_by(f64::total_cmp);
    (v[10], len)
}

fn main() {
    let names = std::env::args().skip(1).collect::<Vec<_>>();
    let fixtures =
        ["seq", "nvim", "top", "modes", "claude", "claude_exit", "decom", "slrm", "decsc_1049", "decsc_47", "kitty"];
    for name in fixtures.iter().filter(|n| names.is_empty() || names.iter().any(|m| m == *n)) {
        let mut e = load(name);
        let (cold, len) = median(&mut e, true, |e| e.snapshot());
        let (warm, _) = median(&mut e, false, |e| e.snapshot());
        println!("{name:14} {cold:8.3} ms {warm:8.3} ms unchanged {len:>10} B");
    }
    // A shell that has printed a lot: 50k colored lines, then a prompt
    // below an empty line, and the same under a full-screen app.
    let mut e = GhosttyEngine::new(120, 40);
    for i in 0..50_000 {
        e.feed(
            format!(
                "\x1b[3{}m{i:06}\x1b[0m some output that fills part of the line, {}\r\n",
                i % 8,
                "x".repeat(i % 60)
            )
            .as_bytes(),
        );
    }
    e.feed(b"\r\n$ ");
    for (label, alt) in [("50k rows", false), ("50k rows, alt", true)] {
        if alt {
            e.feed(b"\x1b[?1049h\x1b[H\x1b[2J\x1b[44mfull screen\x1b[0m\x1b[10;10H");
        }
        for (what, history) in [("all history", None), ("10k history", Some(10_000)), ("screen only", Some(0))] {
            let (cold, len) = median(&mut e, true, |e| e.snapshot_history(history));
            let (warm, _) = median(&mut e, false, |e| e.snapshot_history(history));
            println!("{label:14} {cold:8.3} ms {warm:8.3} ms unchanged {len:>10} B  ({what})");
        }
    }
}
