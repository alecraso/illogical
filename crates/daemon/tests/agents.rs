//! M6b: agent blocks, as ACP clients, against a scripted fake agent server
//! (`fake_acp.py`): turns, permissions (approve, deny, always, cancel),
//! cost, history, search, push, a reboot (the agent dies with the daemon
//! and the session comes back with `session/resume`), and, under a systemd
//! user manager, a restart with an approval pending that the agent server
//! lives through.
//!
//! Real adapters (Claude Code, Codex, Fountain) are in `agents_real.rs`,
//! which costs money and only runs when asked to.

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

fn fake() -> String {
    format!("{}/tests/fake_acp.py", env!("CARGO_MANIFEST_DIR"))
}

/// How the daemon runs: a child of the test (no systemd: everything it
/// started goes with it), or a transient systemd user service.
enum How {
    Child(Option<Child>),
    Service(String),
}

struct Daemon {
    how: How,
    port: u16,
    state: PathBuf,
    sessions: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
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
        let _ = std::fs::remove_dir_all(&self.state);
        let _ = std::fs::remove_dir_all(&self.sessions);
    }
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl").arg("--user").args(args).output().is_ok_and(|o| o.status.success())
}

fn dirs(tag: &str) -> (PathBuf, PathBuf, u16) {
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
    fn child() -> Self {
        let (state, sessions, port) = dirs("c");
        let mut d = Self { how: How::Child(None), port, state, sessions };
        d.start();
        d
    }

    /// Under systemd (FD store, scopes); `None` without a user manager.
    fn service() -> Option<Self> {
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
        let d = Self { how: How::Service(format!("{unit}.service")), port, state, sessions };
        d.wait_up();
        Some(d)
    }

    fn start(&mut self) {
        let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
            .args(["--listen", &format!("127.0.0.1:{}", self.port), "--shell", "bash --norc --noprofile"])
            .args(["--no-manager-env", "--wisp-token-file", "/nonexistent", "--state-dir"])
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
    fn stop(&mut self) {
        if let How::Child(c) = &mut self.how {
            let mut c = c.take().unwrap();
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(c.id() as i32), nix::sys::signal::SIGTERM).unwrap();
            c.wait().unwrap();
        }
    }

    fn restart_service(&self) {
        if let How::Service(unit) = &self.how {
            assert!(systemctl(&["restart", unit]));
            std::thread::sleep(Duration::from_millis(300));
            self.wait_up();
        }
    }

    fn wait_up(&self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while UnixStream::connect(self.sock()).is_err() || self.raw("GET", "/api/panes", None).0 != 200 {
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn sock(&self) -> PathBuf {
        match std::fs::read_to_string(self.state.join("sock.path")) {
            Ok(p) => PathBuf::from(p.trim()),
            Err(_) => self.state.join("sock"),
        }
    }

    fn raw(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String) {
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

    fn get(&self, path: &str) -> Value {
        let (status, body) = self.raw("GET", path, None);
        assert_eq!(status, 200, "{path}: {body}");
        serde_json::from_str(&body).unwrap_or(Value::String(body))
    }

    fn post(&self, path: &str, body: Value) -> Value {
        let (status, text) = self.raw("POST", path, Some(body));
        assert_eq!(status, 200, "{path}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    fn call(&self, id: u64, method: &str, args: Value) -> Value {
        self.post(&format!("/api/blocks/{id}/call/{method}"), args)
    }

    fn state(&self, id: u64) -> Value {
        self.get(&format!("/api/blocks/{id}"))["state"].clone()
    }

    fn wait(&self, id: u64, until: &str) -> String {
        let v = self.get(&format!("/api/panes/{id}/wait?until={until}&timeout=20"));
        assert_ne!(v["result"], "timeout", "waiting for {until}: {}", self.state(id));
        v["state"].as_str().unwrap_or("").to_owned()
    }

    fn wait_for(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn open(&self, prompt: &str) -> u64 {
        let config = json!({ "agent": "acp", "command": ["python3", fake()], "cwd": self.sessions, "prompt": prompt });
        self.post("/api/blocks", json!({ "type": "agent", "config": config }))["block"].as_u64().unwrap()
    }
}

fn entries(state: &Value) -> Vec<Value> {
    state["entries"].as_array().cloned().unwrap_or_default()
}

fn last_tool(state: &Value) -> Value {
    entries(state).into_iter().rev().find(|e| e["type"] == "tool").unwrap_or_default()
}

fn alive(pid: u64) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
        && !std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default().contains(") Z ")
}

#[test]
fn an_agent_block_runs_turns_and_asks_before_it_acts() {
    let d = Daemon::child();
    let id = d.open("hello");
    assert_eq!(d.wait(id, "idle"), "done");
    let s = d.state(id);
    assert_eq!(s["status"], "ready");
    assert_eq!(s["server"]["name"], "fake-acp");
    assert!(entries(&s).iter().any(|e| e["type"] == "agent" && e["text"] == "Hello! I am fake."), "{s}");
    assert_eq!(s["cost"]["total"], 0.01);
    assert_eq!(s["cost"]["last_turn"], 0.01);
    let info = d.get(&format!("/api/blocks/{id}"))["info"].clone();
    assert_eq!(info["type"], "agent");

    // It asks before it runs something; approving runs it.
    d.call(id, "send", json!({ "text": "run touch x" }));
    assert_eq!(d.wait(id, "needs-input"), "needs_input");
    let s = d.state(id);
    let p = &s["pending"][0];
    assert_eq!(
        (p["title"].as_str(), p["tool"].as_str(), p["command"].as_str()),
        (Some("touch x"), Some("Bash"), Some("touch x"))
    );
    let summary = d.get("/api/panes");
    assert!(summary.as_array().unwrap().iter().any(|p| p["id"] == id && p["attention"] == "needs_input"), "{summary}");
    let (status, _) = d.raw(
        "POST",
        &format!("/api/blocks/{id}/call/approve"),
        Some(json!({ "id": p["id"], "option": "allow-with-updates" })),
    );
    assert_eq!(status, 400, "the agent's own allow_always is never picked");
    d.call(id, "approve", json!({ "id": p["id"] }));
    assert_eq!(d.wait(id, "idle"), "done");
    let s = d.state(id);
    let tool = last_tool(&s);
    assert_eq!((tool["status"].as_str(), tool["exit"].as_i64()), (Some("completed"), Some(0)), "{tool}");
    assert_eq!(tool["output"], "\u{1b}[32mran: touch x\u{1b}[0m\r\n", "command output, ANSI and all");
    assert!(s["pending"].as_array().unwrap().is_empty());
    assert!((s["cost"]["last_turn"].as_f64().unwrap() - 0.01).abs() < 1e-9, "per-turn delta of a cumulative cost");

    // Denying, with a reason.
    d.call(id, "send", json!({ "text": "run rm -rf y" }));
    d.wait(id, "needs-input");
    d.call(id, "deny", json!({ "reason": "not that" }));
    d.wait(id, "idle");
    let s = d.state(id);
    assert_eq!(last_tool(&s)["status"], "failed");
    assert!(entries(&s).iter().any(|e| e["text"] == "Denied rm -rf y: not that"), "{s}");

    // "Always": the block remembers it and answers next time itself.
    d.call(id, "send", json!({ "text": "run make" }));
    d.wait(id, "needs-input");
    d.call(id, "approve", json!({ "option": "always" }));
    d.wait(id, "idle");
    assert_eq!(d.state(id)["allow"], json!([{ "tool": "Bash", "title": "make" }]));
    d.call(id, "send", json!({ "text": "run make" }));
    assert_eq!(d.wait(id, "idle"), "done", "never asked");
    assert!(entries(&d.state(id)).iter().any(|e| e["text"] == "Allowed make (always allowed)"));
    d.wait_for("the rule saved in its config", || {
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(d.state.join("layout.json")).unwrap()).unwrap();
        saved["panes"][id.to_string()]["config"]["allow"] == json!([{ "tool": "Bash", "title": "make" }])
    });

    // Cancel mid-turn, and with a request open (answered `cancelled`).
    d.call(id, "send", json!({ "text": "slow" }));
    d.wait_for("streaming", || {
        entries(&d.state(id)).iter().any(|e| e["text"].as_str().is_some_and(|t| t.contains("tick 1")))
    });
    d.call(id, "cancel", json!({}));
    assert_eq!(d.wait(id, "idle"), "idle");
    assert_eq!(d.state(id)["last_stop"], "cancelled");
    d.call(id, "send", json!({ "text": "run sleep 100" }));
    d.wait(id, "needs-input");
    d.call(id, "cancel", json!({}));
    assert_eq!(d.wait(id, "idle"), "idle");
    let s = d.state(id);
    assert_eq!((s["last_stop"].as_str(), s["pending"].as_array().unwrap().len()), (Some("cancelled"), 0));

    // The transcript as Markdown; tail prints it too.
    let text = d.raw("GET", &format!("/api/panes/{id}/capture"), None).1;
    assert!(text.contains("## You\n\nrun touch x"), "{text}");
    assert!(text.contains("**Ran** `touch x` (completed, exit 0)\n\n```\nran: touch x\n```"), "{text}");
    assert_eq!(d.raw("GET", &format!("/api/panes/{id}/tail"), None).1, text);

    // History has its commands and turns; search finds what it said.
    let h = d.get(&format!("/api/history?pane={id}"));
    let texts: Vec<&str> = h.as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect();
    assert!(texts.contains(&"touch x") && texts.iter().any(|t| t.ends_with(": hello")), "{h}");
    let failed = d.get(&format!("/api/history?pane={id}&failed=1"));
    assert!(failed.as_array().unwrap().iter().any(|c| c["text"] == "rm -rf y"), "{failed}");
    let hits = d.get("/api/search?re=Hello!%20I%20am");
    assert!(hits.as_array().unwrap().iter().any(|h| h["pane"] == id), "{hits}");

    // Closing it ends the agent server.
    let pid = d.state(id)["pid"].as_u64().unwrap();
    assert!(alive(pid));
    d.post(&format!("/api/panes/{id}/close"), json!({}));
    d.wait_for("the agent server to go", || !alive(pid));
}

#[test]
fn after_a_reboot_the_transcript_is_back_and_the_session_resumes() {
    let mut d = Daemon::child();
    let id = d.open("remember kestrel");
    d.wait(id, "idle");
    let pid = d.state(id)["pid"].as_u64().unwrap();
    // Mid-turn, with an approval open: then the daemon (and, without
    // systemd, everything it started) goes away, as in a reboot.
    d.call(id, "send", json!({ "text": "run sleep 1" }));
    d.wait(id, "needs-input");
    d.stop();
    d.wait_for("the agent to die with it", || !alive(pid));

    d.start();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    d.wait_for("the session", || d.state(id)["status"] == "ready");
    let s = d.state(id);
    assert_ne!(s["pid"].as_u64(), Some(pid), "a new agent server");
    assert!(s["pending"].as_array().unwrap().is_empty(), "its request died with it");
    let text = d.raw("GET", &format!("/api/panes/{id}/capture"), None).1;
    assert!(text.contains("remember kestrel") && text.contains("Started the agent again"), "{text}");
    assert_eq!(text.matches("## You\n\nremember kestrel").count(), 1, "resumed, not replayed: {text}");
    d.call(id, "send", json!({ "text": "recall" }));
    d.wait(id, "idle");
    assert!(entries(&d.state(id)).iter().any(|e| e["text"] == "You said kestrel."), "the agent's context came back");

    // With policy none it waits for "Resume". (Clients set policies over
    // the WebSocket; here, in the saved layout while the daemon is down.)
    d.stop();
    let path = d.state.join("layout.json");
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    saved["panes"][id.to_string()]["policy"] = json!({ "kind": "none" });
    std::fs::write(&path, saved.to_string()).unwrap();
    d.start();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    assert_eq!(d.state(id)["status"], "stopped");
    assert!(d.state(id)["pid"].is_null());
    d.call(id, "resume", json!({}));
    d.wait_for("the session", || d.state(id)["status"] == "ready");
    d.call(id, "send", json!({ "text": "recall" }));
    d.wait(id, "idle");
}

#[test]
fn an_agent_that_dies_says_so_and_starts_again_on_send() {
    let d = Daemon::child();
    let id = d.open("crash");
    assert_eq!(d.wait(id, "idle"), "needs_input");
    let s = d.state(id);
    assert_eq!(s["status"], "exited");
    assert!(s["error"].as_str().unwrap().contains("exited with code 3"), "{s}");
    d.call(id, "send", json!({ "text": "hello" }));
    assert_eq!(d.wait(id, "idle"), "done");
    assert_eq!(d.state(id)["status"], "ready");
    // Bad configs are refused up front.
    let (status, err) =
        d.raw("POST", "/api/blocks", Some(json!({ "type": "agent", "config": { "agent": "fountain" } })));
    assert_eq!(status, 400, "{err}");
    let (status, err) =
        d.raw("POST", "/api/blocks", Some(json!({ "type": "agent", "vm": true, "config": { "agent": "claude" } })));
    assert_eq!(status, 400);
    assert!(err.contains("VM"), "{err}");
}

/// Under systemd: a restart with an approval pending. The agent server
/// lives through it (its scope; its pipes in the FD store), and the
/// approval, answered to the new daemon, still works.
#[test]
fn a_restart_mid_turn_keeps_the_agent_and_its_pending_approval() {
    let Some(d) = Daemon::service() else { return };
    let id = d.open("hello");
    d.wait(id, "idle");
    let pid = d.state(id)["pid"].as_u64().unwrap();
    d.call(id, "send", json!({ "text": "run make deploy" }));
    d.wait(id, "needs-input");
    let before = d.state(id)["pending"][0].clone();

    d.restart_service();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    let s = d.state(id);
    assert_eq!(s["pid"].as_u64(), Some(pid), "the same agent server");
    assert_eq!(s["status"], "working", "the turn is still running");
    assert_eq!(s["pending"][0]["id"], before["id"], "the same request");
    assert_eq!(d.wait(id, "needs-input"), "needs_input");

    d.call(id, "approve", json!({ "id": before["id"] }));
    assert_eq!(d.wait(id, "idle"), "done", "the turn from before the restart ended");
    let s = d.state(id);
    assert_eq!(last_tool(&s)["output"], "\u{1b}[32mran: make deploy\u{1b}[0m\r\n");
    assert!(entries(&s).iter().any(|e| e["text"] == "Ran it."));
    // And it carries on.
    d.call(id, "send", json!({ "text": "hello" }));
    d.wait(id, "idle");
    assert_eq!(d.state(id)["turns"], 3);
    assert!(alive(pid));

    // A crash too.
    if let How::Service(unit) = &d.how {
        assert!(systemctl(&["kill", "--kill-whom=main", "--signal=SIGKILL", unit]));
        std::thread::sleep(Duration::from_millis(300));
        d.wait_up();
    }
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    assert_eq!(d.state(id)["pid"].as_u64(), Some(pid));
    d.call(id, "send", json!({ "text": "recall" }));
    assert_eq!(d.wait(id, "idle"), "done");
    d.post(&format!("/api/panes/{id}/close"), json!({}));
    d.wait_for("the agent server to go", || !alive(pid));
}

/// A permission request reaches a subscribed phone as a push with what to
/// approve, and approving by its id (as the notification's action does)
/// works.
#[test]
fn a_permission_request_is_pushed_with_its_approval() {
    use aes_gcm::{Aes128Gcm, KeyInit, aead::Aead};
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
    use hkdf::Hkdf;
    use p256::{PublicKey, SecretKey};
    use sha2::Sha256;

    let d = Daemon::child();
    let service = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://127.0.0.1:{}/push/abc", service.local_addr().unwrap().port());
    let ua = SecretKey::from_slice(&[7u8; 32]).unwrap();
    let ua_public = ua.public_key().to_sec1_bytes().to_vec();
    let auth = [9u8; 16];
    d.post(
        "/api/push/subscribe",
        json!({"endpoint": endpoint, "keys": {"p256dh": B64.encode(&ua_public), "auth": B64.encode(auth)}}),
    );
    let id = d.open("run git push");

    // Pushes come for each attention change nobody is looking at; find the
    // one that asks.
    let msg = loop {
        let (mut conn, _) = service.accept().unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut r = BufReader::new(conn.try_clone().unwrap());
        let mut len = 0;
        loop {
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; len];
        r.read_exact(&mut body).unwrap();
        write!(conn, "HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n").unwrap();
        let (salt, rest) = body.split_at(16);
        let idlen = rest[4] as usize;
        let (as_public, sealed) = rest[5..].split_at(idlen);
        let shared = p256::ecdh::diffie_hellman(
            ua.to_nonzero_scalar(),
            PublicKey::from_sec1_bytes(as_public).unwrap().as_affine(),
        );
        let mut info = b"WebPush: info\0".to_vec();
        info.extend_from_slice(&ua_public);
        info.extend_from_slice(as_public);
        let mut ikm = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&auth), shared.raw_secret_bytes().as_ref()).expand(&info, &mut ikm).unwrap();
        let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
        let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
        prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
        prk.expand(b"Content-Encoding: nonce\0", &mut nonce).unwrap();
        let mut plain = Aes128Gcm::new_from_slice(&cek).unwrap().decrypt(&nonce.into(), sealed).unwrap();
        plain.pop();
        let msg: Value = serde_json::from_slice(&plain).unwrap();
        if msg["title"] == "Needs you" {
            break msg;
        }
    };
    assert_eq!(msg["pane"], id);
    assert_eq!(msg["body"], "wants to run git push");
    assert_eq!(msg["approve"]["title"], "git push");
    d.call(id, "approve", json!({ "id": msg["approve"]["id"] }));
    d.wait(id, "idle");
    assert_eq!(last_tool(&d.state(id))["status"], "completed");
}
