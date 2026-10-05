//! M34: chant workspace blocks, against a stand-in chant.
//!
//! The stand-in sits where the block looks first (`node_modules/.bin/chant`
//! in the workspace) and answers the read contract with S21's reference
//! workspace (`fixtures/chant/`). A gate waits in `delivery` while the
//! workspace holds `.gate`; `approve` writes where it ran and its arguments
//! to `approvals` and lets the gate go, as chant's does. The real chant
//! runs in `web/e2e/workspace.spec.ts`.
//!
//! A gate is `needs_input` with a `gate` reason; approving it (#75) passes
//! `--approver` with the owner's name or an editor's, and a viewer can't.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

mod agentd;

use std::{
    io::{Read, Write},
    net::TcpStream,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use agentd::*;
use serde_json::{Value, json};

const OWNER: &str = "owner@example.com";
const FRIEND: &str = "friend@example.com";

const ARGS: &[&str] =
    &["--wisp-token-file", "/nonexistent", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"];

fn daemon() -> Daemon {
    Daemon::child_with(ARGS)
}

/// One whose PATH has no chant on it, whatever the host's has: the host's
/// PATH without the directories that hold a `chant`, and a directory of
/// its own with `node` in it (nvm puts the two side by side). `$CHANT` is
/// emptied too.
fn daemon_without_chant(scratch: &Scratch) -> Daemon {
    let host = std::env::var_os("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&host).collect();
    let node = dirs.iter().map(|d| d.join("node")).find(|p| p.is_file()).expect("node on PATH");
    std::os::unix::fs::symlink(&node, scratch.join("node")).unwrap();
    let kept = dirs.into_iter().filter(|d| !d.join("chant").exists());
    let path = std::env::join_paths(std::iter::once(scratch.to_path_buf()).chain(kept)).unwrap();
    Daemon::child_env(ARGS, &[("PATH", path.to_str().unwrap()), ("CHANT", "")])
}

/// A workspace (a git repository with a `chant.workspace.json`) with the
/// stand-in chant installed, and a gate waiting if `gated`.
fn workspace(d: &Daemon, name: &str, gated: bool) -> PathBuf {
    let ws = d.sessions.join(name);
    let fixtures = format!("{}/tests/fixtures/chant", env!("CARGO_MANIFEST_DIR"));
    std::fs::create_dir_all(ws.join("node_modules/.bin")).unwrap();
    for m in ["app", "delivery", "design-client", "design"] {
        std::fs::create_dir_all(ws.join(m)).unwrap();
        std::fs::write(ws.join(m).join("README.md"), m).unwrap();
    }
    std::fs::write(ws.join("chant.workspace.json"), r#"{"name":"reference","schema":1,"members":[]}"#).unwrap();
    std::fs::write(ws.join(".gitignore"), "node_modules\napprovals\n").unwrap();
    let chant = ws.join("node_modules/.bin/chant");
    std::fs::write(
        &chant,
        format!(
            r#"#!/bin/sh
ws='{ws}'; f='{fixtures}'
case "$1 $2" in
  "workspace ls") cat "$f/ls.json" ;;
  "workspace check") cat "$f/check.json" ;;
  "workspace records") cat "$f/records.json" ;;
  "workspace status") if [ -e "$ws/.gate" ]; then cat "$f/status-gated.json"; else cat "$f/status.json"; fi ;;
  approve*) echo "$PWD $*" >> "$ws/approvals"; rm -f "$ws/.gate"; printf '\033[32mGate "%s" on "%s" resolved\033[0m\n' "$3" "$2" ;;
  *) echo "the stand-in doesn't know $*" >&2; exit 2 ;;
esac
"#,
            ws = ws.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&chant, std::fs::Permissions::from_mode(0o755)).unwrap();
    if gated {
        std::fs::write(ws.join(".gate"), "").unwrap();
    }
    git(&ws, &["init", "-q"]);
    ws
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").args(args).current_dir(dir).status().unwrap().success();
    assert!(ok, "git {args:?}");
}

fn open(d: &Daemon, root: &Path) -> u64 {
    let v = d.post("/api/blocks", json!({ "type": "workspace", "config": { "root": root }, "local": true }));
    v["block"].as_u64().unwrap()
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn read(d: &Daemon, block: u64) -> Value {
    d.wait_for("the first read", || d.state(block)["updated_ms"].as_u64().is_some_and(|t| t > 0));
    d.state(block)
}

fn approvals(ws: &Path) -> String {
    std::fs::read_to_string(ws.join("approvals")).unwrap_or_default()
}

/// A request from a tailnet user (as `tailscale serve` passes them on).
fn as_friend(d: &Daemon, method: &str, path: &str, body: Value) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", d.port)).unwrap();
    let body = body.to_string();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nTailscale-User-Login: {FRIEND}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        d.port,
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, out.split_once("\r\n\r\n").map(|(_, b)| b.to_owned()).unwrap_or_default())
}

fn share(d: &Daemon, block: u64, role: &str) {
    let session = info(d, block)["session"].as_u64().unwrap();
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": role }));
}

#[test]
fn a_gate_is_attention_until_the_owner_approves_it() {
    let d = daemon();
    let ws = workspace(&d, "ws", true);
    let block = open(&d, &ws);
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!(st["how"], "workspace");
    assert_eq!(st["members"].as_array().unwrap().len(), 4);
    assert_eq!(st["gates"][0]["source"]["kind"], "chant");
    let delivery = ws.join("delivery").display().to_string();
    assert_eq!(st["gates"][0]["source"]["dir"], delivery);

    // needs_input, with a gate reason made from the gate.
    d.wait_for("attention", || info(&d, block)["attention"] == "needs_input");
    let r = &info(&d, block)["reason"];
    assert_eq!(r["kind"], "gate", "{r}");
    assert_eq!(r["headline"], "delivery: release waits at gate approve-release");
    assert_eq!(r["bundle"], format!("gate:{}", ws.display()));
    assert_eq!(r["gate"]["op"], "release");
    assert_eq!(r["actions"], json!(["allow", "dismiss"]));
    // `illogical attention` lists it.
    let list = d.get("/api/attention");
    assert!(list.as_array().unwrap().iter().any(|a| a["pane"] == block), "{list}");

    // Nothing else waits.
    let (status, body) = d.raw(
        "POST",
        &format!("/api/blocks/{block}/call/approve"),
        Some(json!({ "member": "app", "op": "x", "gate": "y" })),
    );
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("no gate app/x/y is waiting"), "{body}");

    // The owner approves from the rail: chant is told who, by the owner's
    // illogical name (their tailnet login here), not the OS user.
    let r = d.post("/api/attention/act", json!({ "action": "allow", "pane": block }));
    assert_eq!(r["results"][0]["ok"], true, "{r}");
    let said = approvals(&ws);
    assert!(said.trim_end().ends_with(&format!("approve release approve-release --approver {OWNER}")), "{said}");
    // ...in the member's directory.
    assert!(said.starts_with(&format!("{delivery} approve ")), "{said}");
    // Cleared, and the gate's gone from the block.
    d.wait_for("attention to clear", || info(&d, block)["attention"] == "idle");
    assert_eq!(d.state(block)["gates"], json!([]));
    // The card says who; history and the block's log too.
    let i = info(&d, block);
    assert_eq!((i["answered"]["how"].as_str(), i["answered"]["name"].as_str()), (Some("approved"), Some(OWNER)));
    let hist = d.get(&format!("/api/history?pane={block}"));
    assert!(
        hist.as_array()
            .unwrap()
            .iter()
            .any(|h| h["text"] == "approved delivery: release at gate approve-release" && h["by"] == OWNER),
        "{hist}"
    );

    // A gate reached later (`chant run` exiting 3 elsewhere) shows without
    // a refresh: the fingerprint moved (nobody draws this block, so it's the
    // idle poll's).
    std::fs::write(ws.join(".gate"), "").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
    while info(&d, block)["attention"] != "needs_input" {
        assert!(std::time::Instant::now() < deadline, "the new gate never showed");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // Approved by a call this time (`illogical call %N approve`).
    let out = d.call(block, "approve", json!({ "key": "delivery/release/approve-release" }));
    assert_eq!(out["approved"], "delivery/release/approve-release", "{out}");
    assert_eq!(out["said"], "Gate \"approve-release\" on \"release\" resolved");
    assert_eq!(approvals(&ws).lines().count(), 2);
    d.wait_for("attention to clear again", || info(&d, block)["attention"] == "idle");
}

#[test]
fn an_editor_approves_as_themselves_and_a_viewer_cannot() {
    let d = daemon();
    let ws = workspace(&d, "shared", true);
    let block = open(&d, &ws);
    read(&d, block);
    d.wait_for("attention", || info(&d, block)["reason"]["kind"] == "gate");
    let call = format!("/api/blocks/{block}/call/approve");

    // A stranger: nothing.
    let (status, _) = as_friend(&d, "POST", &call, json!({}));
    assert!([403, 404].contains(&status), "{status}");
    // A viewer sees the gate, and can't approve it, by either route.
    share(&d, block, "viewer");
    let (status, body) = as_friend(&d, "GET", &format!("/api/blocks/{block}"), json!({}));
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("approve-release"));
    let (status, body) = as_friend(&d, "POST", &call, json!({}));
    assert_eq!(status, 403, "{body}");
    let (status, body) = as_friend(&d, "POST", "/api/attention/act", json!({ "action": "allow", "pane": block }));
    assert_eq!(status, 403, "{body}");
    assert_eq!(approvals(&ws), "", "a viewer's approval reached chant");
    assert_eq!(info(&d, block)["attention"], "needs_input");

    // An editor can, and chant's ledger names them.
    share(&d, block, "editor");
    let (status, body) = as_friend(&d, "POST", "/api/attention/act", json!({ "action": "allow", "pane": block }));
    assert_eq!(status, 200, "{body}");
    let said = approvals(&ws);
    assert!(said.trim_end().ends_with(&format!("--approver {FRIEND}")), "{said}");
    d.wait_for("attention to clear", || info(&d, block)["attention"] == "idle");
    let i = info(&d, block);
    assert_eq!(i["answered"]["who"], format!("tailnet:{FRIEND}"));
    let acl = d.get("/api/acl");
    assert!(
        acl["audit"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["action"] == "answer" && a["how"] == "approved" && a["name"] == FRIEND),
        "{acl}"
    );
}

#[test]
fn without_chant_or_a_declaration_it_says_so() {
    let bin = Scratch::new("workspace-bin");
    let d = daemon_without_chant(&bin);
    // A workspace whose chant isn't installed (and none on PATH).
    let bare = d.sessions.join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::write(bare.join("chant.workspace.json"), "{}").unwrap();
    let block = open(&d, &bare);
    let st = read(&d, block);
    let e = st["error"].as_str().unwrap_or_default();
    assert!(e.starts_with("no chant here"), "{st}");
    assert_eq!(info(&d, block)["attention"], "idle", "not knowing isn't a gate");
    let (status, body) = d.raw("POST", &format!("/api/blocks/{block}/call/refresh"), Some(json!({})));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("no chant here"), "{body}");

    // A directory that isn't a workspace.
    let plain = d.sessions.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let block = open(&d, &plain);
    let st = read(&d, block);
    assert!(st["error"].as_str().unwrap_or_default().starts_with("no chant.workspace.json here"), "{st}");

    // With nothing waiting, nothing wants you.
    let ws = workspace(&d, "ws", false);
    let block = open(&d, &ws);
    let st = read(&d, block);
    assert_eq!((st["how"].as_str(), st["error"].as_str()), (Some("workspace"), None), "{st}");
    assert_eq!(info(&d, block)["attention"], "idle");
    assert_eq!(d.call(block, "member", json!({ "name": "delivery" }))["kind"], "chant");
    let text = d.raw("GET", &format!("/api/panes/{block}/capture?format=text"), None).1;
    assert!(text.contains("members (4)"), "{text}");
}
