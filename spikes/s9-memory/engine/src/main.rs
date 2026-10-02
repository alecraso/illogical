//! Memory of one libghostty terminal (as the daemon configures it) in a
//! given state, isolated from the rest of the daemon: build K engines, bring
//! each to the state, and divide the RSS growth by K.
//!
//!     s9-engine <state> <k> <cols> <rows> [scrollback_bytes]
//!
//! States: empty, prompt, alt (alt screen entered), full16 and full
//! (work/screen16.ans, work/screen.ans), sb10k, sb200k (work/lines*.ans),
//! checkpoint (full + one checkpoint taken and dropped), ring (pane.rs's
//! ring fed `rows` x 64 KiB), threads (a pane's five threads; BARE=1 to
//! allocate nothing in them). With scrollback_bytes, a raw libghostty
//! Terminal with that limit instead of GhosttyEngine. HOLD=1 sleeps 30s at
//! the end so /proc/PID/smaps can be read.
//!
//! Build (from spikes/s9-memory/engine):
//!     CARGO_TARGET_DIR=../work/target mise exec -- cargo build --release
//! Add LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast and/or
//! GHOSTTY_SOURCE_DIR=<ghostty 22d13172 + ../ghostty-no-signal-stack.patch>
//! (each in its own CARGO_TARGET_DIR) for the variants.

use std::collections::VecDeque;

use illogical_vt::{GhosttyEngine, VtEngine};

fn statm() -> (u64, u64) {
    let s = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap();
    let get = |k: &str| {
        s.lines().find(|l| l.starts_with(k)).map(|l| l.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap()).unwrap_or(0)
    };
    (get("Rss:"), get("Anonymous:"))
}

fn work(name: &str) -> Vec<u8> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work").join(name);
    std::fs::read(&dir).unwrap_or_else(|e| panic!("{}: {e} (run bench.py once for fixtures)", dir.display()))
}

/// Feed in PTY-sized pieces, with \n -> \r\n as the tty would.
fn feed(e: &mut dyn FnMut(&[u8]), bytes: &[u8]) {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 50);
    for &b in bytes {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    for c in out.chunks(4096) {
        e(c);
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let state = a[1].as_str();
    let k: usize = a[2].parse().unwrap();
    let cols: u16 = a[3].parse().unwrap();
    let rows: u16 = a[4].parse().unwrap();
    let limit: Option<usize> = a.get(5).map(|s| s.parse().unwrap());
    let input = match state {
        "full" | "checkpoint" => work("screen.ans"),
        "full16" => work("screen16.ans"),
        "alt" => b"\x1b[?1049h".to_vec(),
        "prompt" => b"$ ".to_vec(),
        "sb10k" => work("lines10000.ans"),
        "sb200k" => work("lines200000.ans"),
        _ => Vec::new(),
    };
    let (rss0, anon0) = statm();
    let mut keep_e: Vec<GhosttyEngine> = Vec::new();
    let mut keep_t: Vec<libghostty_vt::Terminal<'static, 'static>> = Vec::new();
    let mut keep_r: Vec<VecDeque<u8>> = Vec::new();
    for _ in 0..k {
        if state == "threads" {
            // A pane's five threads as pane.rs makes them: vt (blocked on a
            // channel), read (64 KiB buffer, blocked in read), write, wait,
            // reaper; each allocates a little first, as the real ones do.
            for i in 0..5 {
                let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
                std::thread::Builder::new()
                    .name(format!("t{i}"))
                    .spawn(move || {
                        let n = if std::env::var_os("BARE").is_some() { 0 } else if i == 1 { 64 * 1024 } else { 256 };
                        let buf = vec![0u8; n];
                        std::hint::black_box(&buf);
                        let _ = rx.recv();
                    })
                    .unwrap();
                std::mem::forget(tx);
            }
            continue;
        }
        if state == "ring" {
            // pane.rs Ring::push: extend, then drain past RING_BYTES.
            let mut r = VecDeque::new();
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..(rows as usize) {
                r.extend(&chunk);
                let excess = r.len().saturating_sub(2 * 1024 * 1024);
                r.drain(..excess);
            }
            keep_r.push(r);
            continue;
        }
        if let Some(limit) = limit {
            // Raw libghostty with a different scrollback limit.
            let mut t = libghostty_vt::Terminal::new(cols, rows).unwrap();
            t.set_scrollback_max_bytes(Some(limit)).unwrap();
            feed(&mut |c| t.vt_write(c), &input);
            keep_t.push(t);
            continue;
        }
        let mut e = GhosttyEngine::new(cols, rows);
        feed(&mut |c| e.feed(c), &input);
        if state == "checkpoint" {
            let c = e.checkpoint();
            std::hint::black_box(&c);
        }
        let _ = e.take_replies();
        keep_e.push(e);
    }
    let (rss1, anon1) = statm();
    println!(
        "{state} k={k} {cols}x{rows} limit={limit:?}: per item rss {:.0} KiB anon {:.0} KiB (total {:.1} MiB)",
        (rss1 - rss0) as f64 / k as f64,
        (anon1 - anon0) as f64 / k as f64,
        (rss1 - rss0) as f64 / 1024.0
    );
    if std::env::var_os("HOLD").is_some() {
        println!("pid {}", std::process::id());
        std::thread::sleep(std::time::Duration::from_secs(30));
    }
    std::hint::black_box((&keep_e, &keep_t, &keep_r));
}
