//! M3b against a real wispd: a VM pane runs on its own sprite, survives a
//! daemon restart without losing or repeating output, and takes its sprite
//! with it when it closes. Skips without a wisp token on this host.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use illogical_testkit::illogicald;
use serde_json::{Value, json};

const WISP: &str = "http://127.0.0.1:7788";

fn token() -> Option<String> {
    let file = std::env::var_os("ILLOGICAL_WISP_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/wisp/token"));
    std::fs::read_to_string(file).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// A testkit daemon that deletes the machines it left behind.
struct Daemon(illogical_testkit::Daemon);

impl std::ops::Deref for Daemon {
    type Target = illogical_testkit::Daemon;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Daemon {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.0.halt();
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
    }
}

impl Daemon {
    fn new() -> Self {
        Self(illogicald!("vm").args(["--wisp-url", WISP]).wait_secs(20).start())
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
        eprintln!("SKIP: no wisp token on this host (ILLOGICAL_WISP_TOKEN_FILE or ~/.local/share/wisp/token)");
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

/// M70: an upload into a VM pane lands on its machine, in the user's
/// private folder there, readable by them, and its path pastes in.
#[test]
fn an_upload_into_a_vm_pane_lands_on_its_machine() {
    use std::{io::Read, io::Write, os::unix::net::UnixStream};
    if token().is_none() {
        eprintln!("SKIP: no wisp token on this host (ILLOGICAL_WISP_TOKEN_FILE or ~/.local/share/wisp/token)");
        return;
    }
    let d = Daemon::new();
    let pane = d.post("/api/run", json!({"vm": true}))["pane"].as_u64().unwrap();
    d.wait_for("the guest prompt", || {
        d.get("/api/panes").as_array().unwrap().iter().any(|p| p["id"] == pane && p["cwd"] == "/home/sprite")
    });
    let chunk = |path: &str, body: &[u8]| -> Value {
        let mut s = UnixStream::connect(d.sock()).unwrap();
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        s.write_all(head.as_bytes()).unwrap();
        s.write_all(body).unwrap();
        let mut res = String::new();
        s.read_to_string(&mut res).unwrap();
        assert!(res.starts_with("HTTP/1.1 200"), "{res}");
        serde_json::from_str(res.split_once("\r\n\r\n").unwrap().1).unwrap()
    };
    let up = format!("/api/panes/{pane}/upload?id=0c&ext=png");
    chunk(&format!("{up}&offset=0"), b"first half, ");
    let v = chunk(&format!("{up}&offset=12&last=true"), b"second half");
    let path = v["path"].as_str().unwrap().to_owned();
    assert_eq!(path, format!("/home/sprite/.cache/illogical/uploads/{pane}/0c.png"));

    // The user there reads it; its folder is theirs alone.
    let send = |text: &str| d.post(&format!("/api/panes/{pane}/send"), json!({"text": text, "enter": true}));
    send(&format!("echo GOT-$(cat {path}) MODE-$(stat -c %a%U $(dirname {path}))"));
    d.wait_for("the file on the machine", || d.log(pane).contains("GOT-first half, second half MODE-700sprite"));
    let v = d.post(&format!("/api/panes/{pane}/paste"), json!({"paths": [path]}));
    assert_eq!(v["pasted"], true, "{v}");
    d.post(&format!("/api/panes/{pane}/close"), json!({}));
}
