//! M23: pane summaries. `arugula ls --json` shows each pane's kind,
//! project and activity; kind comes from the foreground process (so an
//! alias reads as what it runs); and a client gets field-level deltas, not
//! a whole `State` per change.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::{Duration, Instant},
};

use agentd::*;
use arugula_proto::{ClientMsg, ServerMsg, State};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Built once per run: `ls` is polled, and a `cargo build` per poll (each
/// waiting on cargo's lock under a full test run) made a poll take seconds.
fn cli_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "arugula"]).status().unwrap();
        assert!(status.success(), "building the CLI");
        Path::new(env!("CARGO_BIN_EXE_arugulad")).with_file_name("arugula")
    })
}

fn ls(d: &Daemon) -> Vec<Value> {
    let out = Command::new(cli_bin()).arg("--socket").arg(d.sock()).args(["ls", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice::<Value>(&out.stdout).unwrap().as_array().cloned().unwrap()
}

fn pane_in(d: &Daemon, id: u64) -> Value {
    ls(d).into_iter().find(|p| p["id"] == id).unwrap_or(Value::Null)
}

/// A shell pane in `cwd` that runs `line` (typed, so the shell integration
/// sees it). `perl -e '$0 = "..."'` stands in for real programs: /proc
/// shows the command line it sets, as it would theirs.
fn typed(d: &Daemon, cwd: &Path, setup: &str, line: &str) -> u64 {
    let pane = d.post("/api/run", json!({ "cwd": cwd.display().to_string() }))["pane"].as_u64().unwrap();
    d.wait_for("a prompt", || {
        d.get("/api/panes").as_array().unwrap().iter().any(|p| p["id"] == pane && p["cwd"].is_string())
    });
    if !setup.is_empty() {
        d.post(&format!("/api/panes/{pane}/send"), json!({ "text": setup, "enter": true }));
        d.wait(pane, "command-end");
    }
    d.post(&format!("/api/panes/{pane}/send"), json!({ "text": line, "enter": true }));
    pane
}

fn fake(argv: &str) -> String {
    format!("perl -e '$0 = \"{argv}\"; sleep 600'")
}

#[test]
fn ls_shows_kind_project_and_activity_for_real_commands() {
    let d = Daemon::child();
    let root = d.sessions.join("work");
    let repo = root.join("myrepo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("crates/core")).unwrap();
    std::fs::create_dir_all(root.join("loose")).unwrap();
    let sub = repo.join("crates/core");
    let loose = root.join("loose");

    let cases = [
        (typed(&d, &sub, "", &fake("cargo test")), "test", Some("myrepo")),
        (typed(&d, &sub, "", &fake("npm run dev")), "server", Some("myrepo")),
        // An alias: the typed text says `c`, the process says claude.
        (
            typed(&d, &repo, &format!("alias c='{}'", fake("claude --continue").replace('\'', "'\\''")), "c"),
            "agent",
            Some("myrepo"),
        ),
        (typed(&d, &sub, "", &fake("nvim src/main.rs")), "editor", Some("myrepo")),
        (typed(&d, &loose, "", "tail -f /dev/null"), "logs", None),
        (typed(&d, &loose, "", &fake("journalctl -fu arugulad")), "logs", None),
        (typed(&d, &loose, "", "true"), "shell", None),
    ];
    for (pane, kind, project) in cases {
        d.wait_for(&format!("%{pane} to be {kind}"), || pane_in(&d, pane)["kind"] == kind);
        let p = pane_in(&d, pane);
        assert_eq!(p["project"]["name"].as_str(), project, "%{pane}: {p}");
        if project.is_some() {
            // The real path: on macOS the temp dir is under /var, a link
            // to /private/var, and a process's cwd is the resolved one.
            let root = std::fs::canonicalize(&repo).unwrap();
            assert_eq!(p["project"]["root"], root.display().to_string());
        }
    }

    // `arugula run` panes have no shell above them: their own process
    // says what they are.
    let run = d.post("/api/run", json!({ "command": fake("pytest -x"), "cwd": sub.display().to_string() }))["pane"]
        .as_u64()
        .unwrap();
    d.wait_for("a run pane's kind", || pane_in(&d, run)["kind"] == "test");

    // Activity: a pane that prints shows bytes a second; once it stops, 0.
    // It prints until told to stop, not for a fixed time: under load one
    // poll of `ls` can take longer than a short burst lasts.
    let stop = loose.join("stop");
    let busy = typed(
        &d,
        &loose,
        "",
        &format!("i=0; while [ ! -e {} ]; do i=$((i+1)); echo some output line $i; sleep 0.05; done", stop.display()),
    );
    d.wait_for("bytes a second", || pane_in(&d, busy)["activity"]["bps"].as_u64().unwrap_or(0) > 100);
    let last = pane_in(&d, busy)["activity"]["last_ms"].as_u64().unwrap();
    assert!(last > 0);
    std::fs::write(&stop, "").unwrap();
    d.wait(busy, "command-end");
    d.wait_for("quiet again", || pane_in(&d, busy)["activity"]["bps"] == 0);
    // Idle shells never printed since the daemon started... except their
    // prompt; either way none of them is busy.
    for p in ls(&d) {
        if p["id"] != busy {
            assert!(p["activity"]["bps"].as_u64().unwrap_or(0) < 2000, "{p}");
        }
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_msg(ws: &mut Ws, until: Instant) -> Option<(usize, ServerMsg)> {
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => return Some((t.len(), serde_json::from_str(&t).unwrap())),
            Ok(Some(Ok(_))) => {}
            _ => return None,
        }
    }
}

/// Busy panes running commands send small deltas, not a `State` per
/// command: guards M23's 1,400-fold saving from coming back.
#[tokio::test(flavor = "multi_thread")]
async fn busy_panes_send_deltas_not_states() {
    let d = Daemon::child();
    // 40 panes, 4 of them printing four lines a second.
    let mut panes = vec![];
    for _ in 0..39 {
        panes.push(d.post("/api/run", json!({}))["pane"].as_u64().unwrap());
    }
    for p in panes.iter().take(4) {
        d.post(
            &format!("/api/panes/{p}/send"),
            json!({ "text": "for i in $(seq 1 60); do echo build $i; sleep 0.25; done", "enter": true }),
        );
    }
    let (mut ws, _) = connect_async(d.ws("/ws")).await.unwrap();
    let start = Instant::now() + Duration::from_secs(10);
    let Some((_, ServerMsg::Hello { state, .. })) = next_msg(&mut ws, start).await else { panic!("no hello") };
    assert!(state.panes.len() >= 40);
    let mut mirror: State = state;
    // Commands start and end in the other panes too, by the API.
    for p in panes.iter().skip(4).take(8) {
        d.post(&format!("/api/panes/{p}/send"), json!({ "text": "sleep 1; echo done", "enter": true }));
    }
    let window = Duration::from_secs(5);
    let until = Instant::now() + window;
    let (mut bytes, mut states, mut deltas) = (0usize, 0, 0);
    while let Some((n, m)) = next_msg(&mut ws, until).await {
        bytes += n;
        match m {
            ServerMsg::State { state } => {
                states += 1;
                mirror = state;
            }
            ServerMsg::Delta { delta } => {
                deltas += 1;
                mirror.apply(&delta);
            }
            _ => {}
        }
    }
    let rate = bytes as f64 / window.as_secs_f64();
    eprintln!("{bytes} bytes in {window:?} ({rate:.0} B/s), {states} states, {deltas} deltas");
    assert_eq!(states, 0, "no layout changed, so no whole State");
    assert!(deltas > 0, "the busy panes' changes arrived");
    assert!(rate < 20_000.0, "{rate:.0} B/s");
    // The mirror kept up: the busy panes are working.
    let busy = mirror.panes.iter().filter(|p| p.activity.is_some_and(|a| a.bps > 0)).count();
    assert!(busy >= 1, "activity reached the client");

    // A Ping answers after everything before it was sent, deltas included.
    ws.send(Message::Text(serde_json::to_string(&ClientMsg::Ping { id: 9 }).unwrap().into())).await.unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match next_msg(&mut ws, until).await {
            Some((_, ServerMsg::Pong { id: 9 })) => break,
            Some(_) => {}
            None => panic!("no pong"),
        }
    }

    // Summaries only: a fresh State without the fields only terminals need.
    let sub = serde_json::to_string(&ClientMsg::Subscribe { summary: true }).unwrap();
    ws.send(Message::Text(sub.into())).await.unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout(left, ws.next()).await else { panic!("no state") };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["type"] == "state" {
            let p = &v["state"]["panes"][0];
            assert!(p.get("epoch").is_none() && p.get("policy").is_none() && p.get("integration").is_none(), "{p}");
            assert!(p.get("kind").is_some(), "{p}");
            break;
        }
    }
}
