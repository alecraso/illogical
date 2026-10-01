//! M6's block contract, through the first non-terminal type (a browser
//! block on ordinary pages): open, describe, call, capture, restore after a
//! restart, close.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

struct Daemon {
    child: Option<Child>,
    state: PathBuf,
    port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

impl Daemon {
    fn new() -> Self {
        let state = std::env::temp_dir().join(format!("ilg-blk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut d = Self { child: None, state, port };
        d.start();
        d
    }

    fn start(&mut self) {
        let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
            .args(["--listen", &format!("127.0.0.1:{}", self.port), "--shell", "bash --norc --noprofile"])
            .args(["--no-manager-env", "--wisp-token-file", "/nonexistent"])
            .arg("--state-dir")
            .arg(&self.state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(self.sock()).is_err() {
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop(&mut self) {
        let mut c = self.child.take().unwrap();
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(c.id() as i32), nix::sys::signal::SIGTERM).unwrap();
        c.wait().unwrap();
    }

    fn sock(&self) -> PathBuf {
        self.state.join("sock")
    }

    fn raw(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String) {
        let mut s = UnixStream::connect(self.sock()).unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        while {
            line.clear();
            r.read_line(&mut line).unwrap();
            !line.trim().is_empty()
        } {}
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        (status, out)
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

    fn wait_for(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A page that allows framing, with a title, on a port of its own.
fn serve_page() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let body = "<html><head><title>Plain page</title></head><body>hi</body></html>";
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

#[test]
fn a_browser_block_opens_describes_calls_restores_and_closes() {
    let mut d = Daemon::new();
    let first = d.get("/api/panes")[0]["id"].as_u64().unwrap();
    let page = serve_page();

    // The daemon's own page refuses to be framed; it's shown as a card.
    let app = format!("http://127.0.0.1:{}/", d.port);
    let id = d.post("/api/blocks", json!({"type": "browser", "config": {"url": app}, "split": first}))["block"]
        .as_u64()
        .unwrap();
    let state = |d: &Daemon| d.get(&format!("/api/blocks/{id}"))["state"].clone();
    d.wait_for("the probe", || state(&d)["framable"].is_boolean());
    assert_eq!(state(&d)["framable"], false);
    let described = d.get(&format!("/api/blocks/{id}"));
    assert_eq!(described["info"]["type"], "browser");
    assert_eq!(described["info"]["tab"], d.get("/api/panes")[0]["tab"], "split beside the first pane");

    // Methods: navigate, then back.
    let plain = format!("http://127.0.0.1:{page}/x");
    d.post(&format!("/api/blocks/{id}/call/navigate"), json!({"url": plain}));
    d.wait_for("the new page", || state(&d)["title"] == "Plain page");
    assert_eq!(state(&d)["framable"], true);
    assert_eq!(state(&d)["back"], json!([app]));
    let (status, err) = d.raw("POST", &format!("/api/blocks/{id}/call/fly"), Some(json!({})));
    assert_eq!(status, 400, "{err}");
    let text = d.raw("GET", &format!("/api/panes/{id}/capture"), None).1;
    assert_eq!(text, format!("Plain page\n{plain}\n"));
    // Listed with the terminals, in the one id space.
    let ls = d.get("/api/panes");
    assert!(ls.as_array().unwrap().iter().any(|p| p["id"] == id && p["type"] == "browser"), "{ls}");

    // Terminals answer the same call route.
    d.post(&format!("/api/blocks/{first}/call/send"), json!({"text": "echo via-call-$((1+1))", "enter": true}));
    d.wait_for("the terminal", || {
        d.post(&format!("/api/blocks/{first}/call/capture"), json!({}))["text"].as_str().unwrap().contains("via-call-2")
    });

    // A restart brings it back from its config, where it was.
    d.stop();
    d.start();
    d.wait_for("the restored block", || {
        d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200 && state(&d)["title"] == "Plain page"
    });

    // Closing it keeps its history like a pane's.
    d.post(&format!("/api/panes/{id}/close"), json!({}));
    d.wait_for("it to go", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 404);
    let closed: Vec<_> = std::fs::read_dir(d.state.join("closed")).unwrap().flatten().collect();
    assert!(closed.iter().any(|e| e.file_name().to_string_lossy().starts_with(&format!("{id}-"))));
}
