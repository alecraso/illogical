//! M9's regression check: what an idle pane costs in daemon memory (S9's
//! `idle` scenario at 50 panes, against the real binary and real bash).
//!
//! Before M9 step 1 an idle pane cost 3.1 to 3.3 MB. The bounds leave
//! headroom over what step 1 measured, but not enough to hide either big
//! fix coming undone: Zig's signal stack (+1.3 MB a pane) or glibc keeping
//! what closed panes freed. Linux only: it reads /proc.
#![cfg(target_os = "linux")]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

/// Panes open at the measurement, the default one included.
const PANES: u64 = 50;
/// Most daemon RSS an idle 80x24 pane may add. Step 1 measured about
/// 1.9 MB (most of it libghostty's ReleaseSafe page fill).
const MAX_PER_PANE_KB: u64 = 2600;
/// Most the daemon may keep, over its one-pane start, once 49 of the 50
/// panes have closed. Without the malloc fixes it kept about 15 MB.
const MAX_KEPT_KB: u64 = 8 * 1024;

struct Daemon {
    child: Child,
    state: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

impl Daemon {
    fn start() -> Daemon {
        // Short: Unix socket paths are limited to ~100 bytes.
        let state = std::env::temp_dir().join(format!("ilg-mem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
            .args(["--listen", &format!("127.0.0.1:{port}"), "--shell", "bash --norc --noprofile", "--no-manager-env"])
            .arg("--state-dir")
            .arg(&state)
            .env("PS1", "$ ")
            // No systemd scopes or FD store, as S9 measured.
            .env_remove("NOTIFY_SOCKET")
            .env_remove("LISTEN_FDS")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let d = Daemon { child, state };
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(d.sock()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        d.wait_for(|| d.panes().iter().all(|p| p["cwd"].is_string()));
        d
    }

    fn sock(&self) -> PathBuf {
        match std::fs::read_to_string(self.state.join("sock.path")) {
            Ok(p) => PathBuf::from(p.trim()),
            Err(_) => self.state.join("sock"),
        }
    }

    fn request(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let mut s = UnixStream::connect(self.sock()).unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.contains(" 200 "), "{method} {path}: {line}");
        let mut chunked = false;
        loop {
            line.clear();
            r.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            chunked |= line.to_ascii_lowercase().starts_with("transfer-encoding: chunked");
        }
        let mut out = Vec::new();
        if chunked {
            loop {
                line.clear();
                r.read_line(&mut line).unwrap();
                let n = usize::from_str_radix(line.trim(), 16).unwrap_or(0);
                if n == 0 {
                    break;
                }
                let mut chunk = vec![0; n + 2];
                r.read_exact(&mut chunk).unwrap();
                out.extend_from_slice(&chunk[..n]);
            }
        } else {
            r.read_to_end(&mut out).unwrap();
        }
        serde_json::from_slice(&out).unwrap_or(Value::Null)
    }

    fn panes(&self) -> Vec<Value> {
        self.request("GET", "/api/panes", None).as_array().cloned().unwrap_or_default()
    }

    fn wait_for(&self, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !f() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The daemon's RSS in KiB.
    fn rss(&self) -> u64 {
        let rollup = std::fs::read_to_string(format!("/proc/{}/smaps_rollup", self.child.id())).unwrap();
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
    let d = Daemon::start();
    let base = d.settled_rss();
    for _ in 1..PANES {
        d.request("POST", "/api/run", Some(json!({})));
    }
    // Every shell is at its prompt (shell integration has reported in).
    d.wait_for(|| {
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
    d.wait_for(|| d.panes().len() == 1);
    let after = d.settled_rss();
    let kept = after.saturating_sub(base);
    eprintln!("daemon rss: {after} KiB after closing {} panes: {kept} KiB kept", PANES - 1);
    assert!(kept <= MAX_KEPT_KB, "closed panes left {kept} KiB behind (at most {MAX_KEPT_KB})");
}
