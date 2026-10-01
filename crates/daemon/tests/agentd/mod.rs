//! A daemon for agent block tests, driven over its Unix socket.

#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

pub fn fake() -> String {
    format!("{}/tests/fake_acp.py", env!("CARGO_MANIFEST_DIR"))
}

/// How the daemon runs: a child of the test (no systemd: everything it
/// started goes with it), or a transient systemd user service.
pub enum How {
    Child(Option<Child>),
    Service(String),
}

pub struct Daemon {
    pub how: How,
    pub port: u16,
    pub state: PathBuf,
    pub sessions: PathBuf,
    args: Vec<String>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Machines go with what owns them: close those first, so a failed
        // test leaves no VM behind.
        if self.raw("GET", "/api/machines", None).0 == 200 {
            let machines: Value = serde_json::from_str(&self.raw("GET", "/api/machines", None).1).unwrap_or_default();
            for m in machines.as_array().into_iter().flatten() {
                if let Some(p) = m["owner"]["pane"].as_u64() {
                    self.raw("POST", &format!("/api/panes/{p}/close"), None);
                }
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline && self.raw("GET", "/api/machines", None).1.trim() != "[]" {
                std::thread::sleep(Duration::from_millis(200));
            }
            if !machines.as_array().is_none_or(|m| m.is_empty()) {
                // The delete itself runs in the background.
                std::thread::sleep(Duration::from_secs(2));
            }
        }
        match &mut self.how {
            How::Child(c) => {
                if let Some(mut c) = c.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
            How::Service(unit) => {
                systemctl(&["stop", unit]);
                systemctl(&["reset-failed", unit]);
            }
        }
        // Anything it left running.
        let _ = Command::new("pkill").args(["-f", &self.sessions.display().to_string()]).status();
        if std::env::var_os("ILLOGICAL_KEEP_TEST_STATE").is_some() {
            eprintln!("kept {}", self.state.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.state);
        let _ = std::fs::remove_dir_all(&self.sessions);
    }
}

pub fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl").arg("--user").args(args).output().is_ok_and(|o| o.status.success())
}

pub fn dirs(tag: &str) -> (PathBuf, PathBuf, u16) {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let state = std::env::temp_dir().join(format!("ilg-agt-{tag}-{}-{n}", std::process::id()));
    let sessions = std::env::temp_dir().join(format!("ilg-agt-sessions-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let _ = std::fs::remove_dir_all(&sessions);
    std::fs::create_dir_all(&sessions).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    (state, sessions, port)
}

impl Daemon {
    pub fn child() -> Self {
        Self::child_with(&[])
    }

    /// With extra daemon arguments (`--wisp-token-file …`).
    pub fn child_with(args: &[&str]) -> Self {
        let (state, sessions, port) = dirs("c");
        let mut d =
            Self { how: How::Child(None), port, state, sessions, args: args.iter().map(|s| s.to_string()).collect() };
        d.start();
        d
    }

    /// Under systemd (FD store, scopes); `None` without a user manager.
    pub fn service() -> Option<Self> {
        if !systemctl(&["show-environment"]) {
            eprintln!("no systemd user manager; skipping");
            return None;
        }
        let (state, sessions, port) = dirs("s");
        let unit = format!("illogical-test-agent-{}-{port}", std::process::id());
        let ok = Command::new("systemd-run")
            .args(["--user", "--quiet", &format!("--unit={unit}")])
            .args(["-p", "Type=notify", "-p", "NotifyAccess=main", "-p", "FileDescriptorStoreMax=64"])
            .args(["-p", "KillMode=mixed", "-p", "Restart=on-failure", "-p", "RestartSec=100ms"])
            .arg(format!("--setenv=FAKE_ACP_DIR={}", sessions.display()))
            .arg(format!("--setenv=PATH={}", std::env::var("PATH").unwrap_or_default()))
            .arg("--")
            .arg(env!("CARGO_BIN_EXE_illogicald"))
            .args(["--listen", &format!("127.0.0.1:{port}"), "--shell", "bash --norc --noprofile"])
            .args(["--no-manager-env", "--wisp-token-file", "/nonexistent", "--state-dir"])
            .arg(&state)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "systemd-run failed");
        let d = Self { how: How::Service(format!("{unit}.service")), port, state, sessions, args: vec![] };
        d.wait_up();
        Some(d)
    }

    pub fn start(&mut self) {
        let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
            .args(["--listen", &format!("127.0.0.1:{}", self.port), "--shell", "bash --norc --noprofile"])
            .arg("--no-manager-env")
            .args(if self.args.is_empty() {
                vec!["--wisp-token-file".into(), "/nonexistent".into()]
            } else {
                self.args.clone()
            })
            .arg("--state-dir")
            .arg(&self.state)
            .env("FAKE_ACP_DIR", &self.sessions)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.how = How::Child(Some(child));
        self.wait_up();
    }

    /// Stop it the way a reboot would: what it started goes too.
    pub fn stop(&mut self) {
        if let How::Child(c) = &mut self.how {
            let mut c = c.take().unwrap();
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(c.id() as i32), nix::sys::signal::SIGTERM).unwrap();
            c.wait().unwrap();
        }
    }

    pub fn restart_service(&self) {
        if let How::Service(unit) = &self.how {
            assert!(systemctl(&["restart", unit]));
            std::thread::sleep(Duration::from_millis(300));
            self.wait_up();
        }
    }

    pub fn wait_up(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while UnixStream::connect(self.sock()).is_err() || self.raw("GET", "/api/panes", None).0 != 200 {
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn sock(&self) -> PathBuf {
        match std::fs::read_to_string(self.state.join("sock.path")) {
            Ok(p) => PathBuf::from(p.trim()),
            Err(_) => self.state.join("sock"),
        }
    }

    pub fn raw(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String) {
        let Ok(mut s) = UnixStream::connect(self.sock()) else { return (0, String::new()) };
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let _ = write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut r = BufReader::new(s);
        let mut line = String::new();
        if r.read_line(&mut line).is_err() || line.is_empty() {
            return (0, String::new());
        }
        let status = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
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
        (status, String::from_utf8_lossy(&out).into_owned())
    }

    pub fn get(&self, path: &str) -> Value {
        let (status, body) = self.raw("GET", path, None);
        assert_eq!(status, 200, "{path}: {body}");
        serde_json::from_str(&body).unwrap_or(Value::String(body))
    }

    pub fn post(&self, path: &str, body: Value) -> Value {
        let (status, text) = self.raw("POST", path, Some(body));
        assert_eq!(status, 200, "{path}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    pub fn call(&self, id: u64, method: &str, args: Value) -> Value {
        self.post(&format!("/api/blocks/{id}/call/{method}"), args)
    }

    pub fn state(&self, id: u64) -> Value {
        self.get(&format!("/api/blocks/{id}"))["state"].clone()
    }

    pub fn wait(&self, id: u64, until: &str) -> String {
        self.wait_secs(id, until, 20)
    }

    pub fn wait_secs(&self, id: u64, until: &str, secs: u64) -> String {
        let v = self.get(&format!("/api/panes/{id}/wait?until={until}&timeout={secs}"));
        assert_ne!(v["result"], "timeout", "waiting for {until}: {}", self.state(id));
        v["state"].as_str().unwrap_or("").to_owned()
    }

    pub fn wait_for(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn open(&self, prompt: &str) -> u64 {
        let config = json!({ "agent": "acp", "command": ["python3", fake()], "cwd": self.sessions, "prompt": prompt });
        self.open_with(json!({ "type": "agent", "config": config }))
    }

    pub fn open_with(&self, req: Value) -> u64 {
        self.post("/api/blocks", req)["block"].as_u64().unwrap()
    }
}

pub fn entries(state: &Value) -> Vec<Value> {
    state["entries"].as_array().cloned().unwrap_or_default()
}

pub fn last_tool(state: &Value) -> Value {
    entries(state).into_iter().rev().find(|e| e["type"] == "tool").unwrap_or_default()
}

pub fn alive(pid: u64) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
        && !std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default().contains(") Z ")
}
