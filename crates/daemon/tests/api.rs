//! M3 end to end: shell integration, the HTTP API over the Unix socket, and
//! attention, against the real binary running real bash.

mod listen;
mod strays;

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

struct Daemon {
    child: Child,
    port: u16,
    state: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        strays::remove(&self.state);
    }
}

fn start() -> Daemon {
    static N: AtomicU32 = AtomicU32::new(0);
    // Short: Unix socket paths are limited to ~100 bytes.
    let state =
        std::env::temp_dir().join(format!("ilg-api-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&state);
    let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
        .args(["--listen", listen::ANY, "--shell", "bash --norc --noprofile", "--no-manager-env"])
        .arg("--state-dir")
        .arg(&state)
        .env("PS1", "$ ")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut d = Daemon { child, port: 0, state };
    d.port = listen::wait_port(&d.state);
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(d.sock()).is_err() {
        assert!(Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    // The first shell's first prompt (the integration is loaded).
    d.wait_for(|| d.get("/api/panes")[0]["cwd"].is_string());
    d
}

impl Daemon {
    /// The local token loopback callers show.
    fn token(&self) -> String {
        std::fs::read_to_string(self.state.join("local-token")).unwrap().trim().to_owned()
    }

    fn sock(&self) -> PathBuf {
        match std::fs::read_to_string(self.state.join("sock.path")) {
            Ok(p) => PathBuf::from(p.trim()),
            Err(_) => self.state.join("sock"),
        }
    }

    /// One HTTP request over the socket; the whole body.
    fn raw(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String) {
        let mut s = UnixStream::connect(self.sock()).unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        s.write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
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

    fn send(&self, pane: u64, text: &str) {
        self.post(&format!("/api/panes/{pane}/send"), json!({"text": text, "enter": true}));
    }

    fn pane(&self, id: u64) -> Value {
        self.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == id).cloned().unwrap_or(Value::Null)
    }

    fn wait_for(&self, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn shell_integration_reports_commands_exit_codes_and_cwd() {
    let d = start();
    d.send(1, "cd /tmp && false");
    let w = d.get("/api/panes/1/wait?until=command-end&timeout=10");
    assert_eq!((w["result"].as_str(), w["exit"].as_i64()), (Some("command_end"), Some(1)), "{w}");
    assert_eq!(w["text"], "cd /tmp && false");
    d.wait_for(|| d.pane(1)["cwd"] == "/tmp");
    assert_eq!(d.pane(1)["last"]["exit"], 1);

    // send then wait, back to back, always sees the new command.
    for i in 0..5 {
        d.send(1, &format!("echo round-{i}"));
        let w = d.get("/api/panes/1/wait?until=command-end&timeout=10");
        assert_eq!(w["text"], format!("echo round-{i}"), "round {i}: {w}");
    }

    // A command with quotes and a ';' comes through intact.
    d.send(1, r#"echo "a;b" 'c'"#);
    let w = d.get("/api/panes/1/wait?until=command-end&timeout=10");
    assert_eq!(w["text"], r#"echo "a;b" 'c'"#);
    let out = d.raw("GET", "/api/panes/1/tail?from=last-command&text=1", None).1;
    assert_eq!(out.trim(), "a;b c");

    // `until` bounds the bytes (the TUI's copy mode reads history up to
    // what it has, M32).
    let all = d.raw("GET", "/api/panes/1/tail?from=0", None).1;
    let some = d.raw("GET", "/api/panes/1/tail?from=0&until=10", None).1;
    assert_eq!(some, all[..10]);
}

#[test]
fn run_wait_capture_history_search_export() {
    let d = start();
    let pane =
        d.post("/api/run", json!({"command": "echo first-$((20+1)); echo second; exit 4"}))["pane"].as_u64().unwrap();
    let w = d.get(&format!("/api/panes/{pane}/wait?until=exit&timeout=10"));
    assert_eq!(w, json!({"result": "exit", "code": 4}));
    let last = d.raw("GET", &format!("/api/panes/{pane}/capture?scope=last-command"), None).1;
    assert!(last.contains("first-21") && last.contains("second"), "{last}");
    assert_eq!(d.pane(pane)["attention"], "done", "finished unwatched");

    let h = d.get("/api/history?failed=1");
    let entry = h.as_array().unwrap().iter().find(|e| e["pane"] == pane).unwrap();
    assert_eq!(
        (entry["exit"].as_i64(), entry["text"].as_str()),
        (Some(4), Some("echo first-$((20+1)); echo second; exit 4"))
    );

    let hits = d.get("/api/search?re=first-2%5B0-9%5D");
    let hit = &hits[0];
    assert_eq!((hit["pane"].as_u64(), hit["line"].as_str()), (Some(pane), Some("first-21")));

    let cast = d.raw("GET", &format!("/api/panes/{pane}/export.cast"), None).1;
    let mut lines = cast.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(header["version"], 3);
    assert!(cast.contains("first-21"));

    // Closed panes stay in history.
    d.post("/api/run", json!({"command": "true"}));
    let msg = json!({"text": "\r", "enter": false});
    d.post(&format!("/api/panes/{pane}/send"), msg);
    d.send(pane, "exit");
    d.wait_for(|| d.pane(pane).is_null());
    // The pane leaves the list at once; its history moves to the closed
    // ones once its program has gone.
    d.wait_for(|| d.get(&format!("/api/history?pane={pane}"))[0]["open"] == false);

    // A script can close what it opened, even while it's running.
    let long = d.post("/api/run", json!({"command": "sleep 600"}))["pane"].as_u64().unwrap();
    d.post(&format!("/api/panes/{long}/close"), json!({}));
    d.wait_for(|| d.pane(long).is_null());
    assert_eq!(d.raw("POST", &format!("/api/panes/{long}/close"), Some(json!({}))).0, 404);
}

/// Whether everything a daemon's panes ran (whose command lines name its
/// state dir) is gone within `secs`.
fn gone_within(state: &std::path::Path, secs: u64) -> bool {
    let pattern = format!("{}/", state.display());
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        let found = Command::new("pgrep").args(["-f", &pattern]).stdout(Stdio::null()).status().unwrap();
        if !found.success() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn a_pane_closed_as_it_starts_takes_its_program_even_if_the_daemon_dies() {
    // #35: closed at once, and the daemon killed before it could do more
    // (perhaps before the program had even started).
    let mut d = start();
    let cmd = format!("sleep 600; : {}/", d.state.display());
    let pane = d.post("/api/run", json!({ "command": cmd }))["pane"].as_u64().unwrap();
    d.post(&format!("/api/panes/{pane}/close"), json!({}));
    let _ = d.child.kill();
    let _ = d.child.wait();
    assert!(gone_within(&d.state, 6), "the program outlived its pane");

    // A program that shrugs off the hangup: the shim kills it, with no
    // daemon left to.
    let mut d = start();
    let hup = d.state.join("hup");
    let cmd = format!("trap 'touch {}' HUP; while :; do sleep 0.1; done", hup.display());
    let pane = d.post("/api/run", json!({ "command": cmd }))["pane"].as_u64().unwrap();
    d.wait_for(|| d.pane(pane)["running"] == true);
    std::thread::sleep(Duration::from_millis(300));
    d.post(&format!("/api/panes/{pane}/close"), json!({}));
    d.wait_for(|| hup.exists());
    let _ = d.child.kill();
    let _ = d.child.wait();
    assert!(gone_within(&d.state, 6), "the shim didn't kill what ignored the hangup");
}

#[test]
fn keys_and_mouse_reach_the_program_encoded() {
    let d = start();
    d.send(1, r"printf '\e[?1000h\e[?1006h'; cat -v");
    d.wait_for(|| d.pane(1)["current"]["text"].as_str().is_some_and(|t| t.contains("cat -v")));
    std::thread::sleep(Duration::from_millis(300));
    d.post("/api/panes/1/keys", json!({"keys": ["C-a", "Up", "Enter"]}));
    d.post("/api/panes/1/mouse", json!({"x": 5, "y": 3}));
    d.post("/api/panes/1/keys", json!({"keys": ["Enter"]}));
    d.wait_for(|| d.raw("GET", "/api/panes/1/capture", None).1.contains("^[[<0;5;3M^[[<0;5;3m"));
    let screen = d.raw("GET", "/api/panes/1/capture", None).1;
    assert!(screen.contains("^A^[[A"), "{screen}");
    let p = d.get("/api/panes/1/process");
    assert_eq!(p["comm"], "cat");
    d.post("/api/panes/1/keys", json!({"keys": ["C-c"]}));
}

#[test]
fn events_stream_and_attention() {
    let d = start();
    // A live event stream, read in the background.
    let sock = d.sock();
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    std::thread::spawn(move || {
        let mut s = UnixStream::connect(sock).unwrap();
        write!(s, "GET /api/events?follow=1&pane=1 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        for line in BufReader::new(s).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send(v);
            }
        }
    });
    std::thread::sleep(Duration::from_millis(300));
    d.send(1, r"printf '\e]9;deploy finished\a'");
    let mut seen = vec![];
    let deadline = Instant::now() + Duration::from_secs(10);
    while !seen.iter().any(|e: &Value| e["type"] == "attention" && e["state"] == "needs_input") {
        assert!(Instant::now() < deadline, "events so far: {seen:?}");
        if let Ok(e) = rx.recv_timeout(Duration::from_millis(200)) {
            seen.push(e);
        }
    }
    assert!(seen.iter().any(|e| e["type"] == "notify" && e["body"] == "deploy finished"), "{seen:?}");
    assert!(seen.iter().any(|e| e["type"] == "command_start"));
    assert_eq!(d.pane(1)["attention"], "needs_input");

    // Typing in the pane answers it.
    d.send(1, "true");
    d.wait_for(|| d.pane(1)["attention"] != "needs_input");
    // (After `true` has finished, or its end would reset what follows.)
    d.wait_for(|| d.pane(1)["last"]["text"] == "true");

    // An agent hook sets it directly.
    d.post("/api/panes/1/attention", json!({"state": "done"}));
    assert_eq!(d.pane(1)["attention"], "done");
}

#[test]
fn an_idle_agent_redrawing_wants_you_once_a_turn() {
    let d = start();
    // A stand-in agent: answers a line, then redraws itself after a while,
    // as Claude Code does on a resize or for its status line.
    let bin = d.state.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let agent = bin.join("claude");
    std::fs::write(&agent, "#!/bin/bash\nwhile read -r l; do echo \"ok $l\"; sleep 4; echo redraw; done\n").unwrap();
    std::fs::set_permissions(&agent, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let needs = || d.pane(1)["attention"] == "needs_input";
    let wait = |secs| {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            assert!(!needs(), "an idle agent wanted you again");
            std::thread::sleep(Duration::from_millis(100));
        }
    };

    d.send(1, &agent.display().to_string());
    d.wait_for(needs);
    d.post("/api/panes/1/attention", json!({"state": "idle"}));
    // A turn: it goes quiet after answering, and wants you.
    d.send(1, "go");
    d.wait_for(needs);
    d.post("/api/panes/1/attention", json!({"state": "idle"}));
    // Its redraw (4 s on) and the quiet after it don't want you again.
    wait(7);
    assert_eq!(d.pane(1)["attention"], "idle");
    // The next turn does.
    d.send(1, "again");
    d.wait_for(needs);
}

#[test]
fn the_api_over_tcp_refuses_other_sites() {
    let d = start();
    let mut s = std::net::TcpStream::connect(("127.0.0.1", d.port)).unwrap();
    let body = r#"{"command":"touch /tmp/pwned"}"#;
    s.write_all(
        format!(
            "POST /api/run HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nOrigin: https://evil.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            d.port,
            d.token(),
            body.len()
        )
        .as_bytes(),
    )
    .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 403"), "{resp}");
    // No Origin (a program) is fine.
    let mut s = std::net::TcpStream::connect(("127.0.0.1", d.port)).unwrap();
    write!(
        s,
        "GET /api/panes HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
        d.port,
        d.token()
    )
    .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
}

/// Web Push end to end: a stand-in push service receives what the daemon
/// sends, and we decrypt it as the browser would.
#[test]
fn push_reaches_a_subscribed_browser_encrypted() {
    use aes_gcm::{Aes128Gcm, KeyInit, aead::Aead};
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
    use hkdf::Hkdf;
    use p256::{PublicKey, SecretKey};
    use sha2::Sha256;

    let d = start();
    let service = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://127.0.0.1:{}/push/abc", service.local_addr().unwrap().port());
    // The "browser": its key pair and auth secret.
    let ua = SecretKey::from_slice(&[7u8; 32]).unwrap();
    let ua_public = ua.public_key().to_sec1_bytes().to_vec();
    let auth = [9u8; 16];
    let key = d.get("/api/push/key")["key"].as_str().unwrap().to_owned();
    assert_eq!(B64.decode(&key).unwrap().len(), 65);
    d.post(
        "/api/push/subscribe",
        json!({"endpoint": endpoint, "keys": {"p256dh": B64.encode(&ua_public), "auth": B64.encode(auth)}}),
    );
    d.post("/api/push/test", json!({}));

    let (mut conn, _) = service.accept().unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut r = BufReader::new(conn.try_clone().unwrap());
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
        headers.push(line.trim().to_owned());
    }
    let header = |name: &str| {
        headers.iter().find_map(|h| {
            h.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_owned())
        })
    };
    assert_eq!(header("content-encoding").as_deref(), Some("aes128gcm"));
    assert!(header("authorization").unwrap().starts_with("vapid t="), "{headers:?}");
    assert!(header("authorization").unwrap().ends_with(&format!("k={key}")));
    let len: usize = header("content-length").unwrap().parse().unwrap();
    let mut body = vec![0; len];
    r.read_exact(&mut body).unwrap();
    write!(conn, "HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n").unwrap();

    // RFC 8291, the browser's side.
    let (salt, rest) = body.split_at(16);
    let idlen = rest[4] as usize;
    let (as_public, sealed) = rest[5..].split_at(idlen);
    let shared =
        p256::ecdh::diffie_hellman(ua.to_nonzero_scalar(), PublicKey::from_sec1_bytes(as_public).unwrap().as_affine());
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
    assert_eq!(plain.pop(), Some(2), "last-record padding delimiter");
    let msg: Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!((msg["title"].as_str(), msg["body"].as_str()), (Some("illogical"), Some("Notifications work.")));
}
