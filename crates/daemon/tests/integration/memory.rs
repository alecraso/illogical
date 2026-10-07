//! M9's regression check: what an idle pane costs in daemon memory (S9's
//! `idle` scenario at 50 panes, against the real binary and real bash).
//!
//! Before M9 step 1 an idle pane cost 3.1 to 3.3 MB. The bounds leave
//! headroom over what step 1 measured, but not enough to hide either big
//! fix coming undone: Zig's signal stack (+1.3 MB a pane) or glibc keeping
//! what closed panes freed. Linux only: it reads /proc.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]
#![cfg(any(target_os = "linux", target_os = "android"))]

use std::time::Duration;

use arugula_testkit::{Daemon, arugulad};
use serde_json::{Value, json};

/// Panes open at the measurement, the default one included.
const PANES: u64 = 50;
/// Most daemon RSS an idle 80x24 pane may add. Step 1 measured about
/// 1.9 MB (most of it libghostty's ReleaseSafe page fill).
const MAX_PER_PANE_KB: u64 = 2600;
/// Most the daemon may keep, over its one-pane start, once 49 of the 50
/// panes have closed. Without the malloc fixes it kept about 15 MB.
const MAX_KEPT_KB: u64 = 8 * 1024;

/// All the daemon keeps of the test's environment. Every pane copies the
/// daemon's, and CI's job adds kilobytes: with 17 KB of it, the 49 closed
/// panes kept 8.2-8.4 MB, over MAX_KEPT_KB, where they keep 6-7.5 MB.
const KEEP_ENV: &[&str] = &["PATH", "HOME", "USER", "LOGNAME", "LANG", "TMPDIR", "XDG_RUNTIME_DIR", "ARUGULA_CHANT"];

fn start() -> Daemon {
    let mut b = arugulad!("mem");
    for (k, _) in std::env::vars_os() {
        if !KEEP_ENV.iter().any(|keep| k == *keep) {
            b = b.env_remove(k);
        }
    }
    let d = b
        .env("PS1", "$ ")
        // No systemd scopes or FD store, as S9 measured.
        .env_remove("LISTEN_FDS")
        .wait_secs(60)
        .start();
    d.wait_for("the first prompt", || d.panes().iter().all(|p| p["cwd"].is_string()));
    d
}

trait Measure {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> Value;
    fn panes(&self) -> Vec<Value>;
    fn rss(&self) -> u64;
    fn settled_rss(&self) -> u64;
}

impl Measure for Daemon {
    /// A request that must answer 200: its JSON, or null.
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (status, text) = self.raw(method, path, body);
        assert_eq!(status, 200, "{method} {path}: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    fn panes(&self) -> Vec<Value> {
        self.request("GET", "/api/panes", None).as_array().cloned().unwrap_or_default()
    }

    /// The daemon's RSS in KiB.
    fn rss(&self) -> u64 {
        let rollup = std::fs::read_to_string(format!("/proc/{}/smaps_rollup", self.pid().unwrap())).unwrap();
        rollup
            .lines()
            .find_map(|l| l.strip_prefix("Rss:"))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .unwrap()
    }

    /// RSS once it has stopped moving (as S9's bench.py waits).
    fn settled_rss(&self) -> u64 {
        let mut last = self.rss();
        for _ in 0..15 {
            std::thread::sleep(Duration::from_secs(1));
            let now = self.rss();
            if now.abs_diff(last) <= 64.max(last / 200) {
                return now;
            }
            last = now;
        }
        last
    }
}

#[test]
fn idle_panes_stay_small_and_closed_ones_give_memory_back() {
    let d = start();
    let base = d.settled_rss();
    for _ in 1..PANES {
        d.request("POST", "/api/run", Some(json!({})));
    }
    // Every shell is at its prompt (shell integration has reported in).
    d.wait_for("every shell's prompt", || {
        let panes = d.panes();
        panes.len() as u64 == PANES && panes.iter().all(|p| p["cwd"].is_string())
    });
    let full = d.settled_rss();
    let per_pane = full.saturating_sub(base) / (PANES - 1);
    eprintln!("daemon rss: {base} KiB with 1 pane, {full} KiB with {PANES}: {per_pane} KiB a pane");
    assert!(per_pane <= MAX_PER_PANE_KB, "an idle pane costs {per_pane} KiB (at most {MAX_PER_PANE_KB})");

    let panes = d.panes();
    for p in &panes[1..] {
        d.request("POST", &format!("/api/panes/{}/close", p["id"]), Some(json!({})));
    }
    d.wait_for("the panes to close", || d.panes().len() == 1);
    let after = d.settled_rss();
    let kept = after.saturating_sub(base);
    eprintln!("daemon rss: {after} KiB after closing {} panes: {kept} KiB kept", PANES - 1);
    assert!(kept <= MAX_KEPT_KB, "closed panes left {kept} KiB behind (at most {MAX_KEPT_KB})");
}
