//! M3b against a real wispd: a VM pane runs on its own sprite, survives a
//! daemon restart without losing or repeating output, and takes its sprite
//! with it when it closes. Skips without a wisp token on this host.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const WISP: &str = "http://127.0.0.1:7788";

fn token() -> Option<String> {
    let file = std::env::var_os("ILLOGICAL_WISP_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/wisp/token"));
    std::fs::read_to_string(file).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

struct Daemon {
    child: Option<Child>,
    state: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        // A failed test leaves its machines behind: delete this daemon's.
        if let (Some(token), Ok(id)) = (token(), std::fs::read_to_string(self.state.join("daemon-id"))) {
            let auth = format!("Authorization: Bearer {token}");
            let list = Command::new("curl")
                .args(["-s", "-H", &auth])
                .arg(format!("{WISP}/v1/sprites?prefix=illogical-eph-{}-", id.trim()))
                .output()
                .map(|o| o.stdout)
                .unwrap_or_default();
            let names: Value = serde_json::from_slice(&list).unwrap_or_default();
            for n in names["sprites"].as_array().into_iter().flatten().filter_map(|s| s["name"].as_str()) {
                let _ = Command::new("curl")
                    .args(["-s", "-o", "/dev/null", "-X", "DELETE", "-H", &auth])
                    .arg(format!("{WISP}/v1/sprites/{n}"))
                    .status();
            }
        }
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

impl Daemon {
    fn new() -> Self {
        let state = std::env::temp_dir().join(format!("ilg-vm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let mut d = Self { child: None, state };
        d.start();
        d
    }

    fn start(&mut self) {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
            .args(["--listen", &format!("127.0.0.1:{port}"), "--shell", "bash --norc --noprofile", "--no-manager-env"])
            .args(["--wisp-url", WISP])
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

    /// As systemd stops it: SIGTERM, and wait.
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
        let deadline = Instant::now() + Duration::from_secs(20);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn log(&self, pane: u64) -> String {
        let dir = self.state.join("blocks").join(pane.to_string());
        log_text(&dir)
    }
}

fn log_text(dir: &Path) -> String {
    let mut segs: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    segs.retain(|p| p.file_name().unwrap().to_string_lossy().starts_with("seg-"));
    segs.sort();
    segs.iter().map(|p| String::from_utf8_lossy(&std::fs::read(p).unwrap()).into_owned()).collect()
}

/// Whether wispd has a sprite by this name.
fn sprite_exists(token: &str, name: &str) -> bool {
    let out = Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "-H", &format!("Authorization: Bearer {token}")])
        .arg(format!("{WISP}/v1/sprites/{name}"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout) == "200"
}

#[test]
fn a_vm_pane_survives_a_restart_and_takes_its_machine_when_it_closes() {
    let Some(token) = token() else {
        eprintln!("skipping: no wisp token on this host");
        return;
    };
    let mut d = Daemon::new();
    let pane = d.post("/api/run", json!({"vm": true}))["pane"].as_u64().unwrap();
    let machine = d.get("/api/machines")[0].clone();
    let sprite = machine["sprite"].as_str().unwrap().to_owned();
    assert_eq!(machine["owner"], json!({ "pane": pane }));
    // The guest's shell, with the integration (OSC 7 reports its home).
    d.wait_for("the guest prompt", || {
        d.get("/api/panes").as_array().unwrap().iter().any(|p| p["id"] == pane && p["cwd"] == "/home/sprite")
    });
    assert!(sprite_exists(&token, &sprite));

    // Output made while the daemon is down arrives once it's back, and
    // nothing already logged arrives twice.
    let send =
        |d: &Daemon, text: &str| d.post(&format!("/api/panes/{pane}/send"), json!({"text": text, "enter": true}));
    send(&d, "echo BEFORE-$((6*7)); (sleep 2; echo LATE-$((6*7))) &");
    d.wait_for("BEFORE", || d.log(pane).contains("BEFORE-42"));
    d.stop();
    std::thread::sleep(Duration::from_secs(3));
    d.start();
    d.wait_for("LATE", || d.log(pane).contains("LATE-42"));
    send(&d, "echo AFTER-$((6*7))");
    d.wait_for("AFTER", || d.log(pane).contains("AFTER-42"));
    let log = d.log(pane);
    for marker in ["BEFORE-42", "LATE-42", "AFTER-42"] {
        assert_eq!(log.matches(marker).count(), 1, "{marker} in:\n{log}");
    }
    let p = d.get(&format!("/api/panes/{pane}/process"));
    assert_eq!(p["comm"], "bash", "{p}");

    // Closing the pane deletes the machine; its output stays.
    d.post(&format!("/api/panes/{pane}/close"), json!({}));
    d.wait_for("the machine to go", || !sprite_exists(&token, &sprite));
    assert_eq!(d.get("/api/machines"), json!([]));
    let (status, tail) = d.raw("GET", &format!("/api/panes/{pane}/tail?from=0&text=1"), None);
    assert_eq!(status, 200);
    assert!(tail.contains("AFTER-42"), "{tail}");
}
