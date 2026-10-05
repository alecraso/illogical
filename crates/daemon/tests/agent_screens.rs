//! Agents in terminal panes, read off their screens (#145), live: the
//! replay agent plays recorded Claude Code and Codex sessions with their
//! timing in a pane, and the pane's attention follows what the screen
//! shows. No hooks are installed.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

mod agentd;
mod replay;

use std::time::{Duration, Instant};

use agentd::*;
use replay::Replay;
use serde_json::{Value, json};

fn send(d: &Daemon, pane: u64, text: &str) {
    d.post(&format!("/api/panes/{pane}/send"), json!({"text": text, "enter": true}));
}

fn keys(d: &Daemon, pane: u64, keys: &[&str]) {
    d.post(&format!("/api/panes/{pane}/keys"), json!({ "keys": keys }));
}

fn attention(d: &Daemon, pane: u64) -> String {
    let panes = d.get("/api/panes");
    let p = panes.as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default();
    p["attention"].as_str().unwrap_or_default().to_owned()
}

/// The pane's card in the "needs you" list.
fn card(d: &Daemon, pane: u64) -> Value {
    d.get("/api/attention").as_array().unwrap().iter().find(|i| i["pane"] == pane).cloned().unwrap_or_default()
}

fn until(d: &Daemon, pane: u64, want: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while attention(d, pane) != want {
        assert!(
            Instant::now() < deadline,
            "not {want} but {}: {}\n{}",
            attention(d, pane),
            d.get(&format!("/api/panes/{pane}/capture")),
            d.get(&format!("/api/panes/{pane}/detection"))
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// It never asks for you while `f` runs.
fn never_needs_you<T>(d: &Daemon, pane: u64, f: impl FnOnce() -> T) -> T {
    let stop = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        let watcher = s.spawn(|| {
            let mut seen = vec![];
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                seen.push(attention(d, pane));
                std::thread::sleep(Duration::from_millis(50));
            }
            seen
        });
        let out = f();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let seen = watcher.join().unwrap();
        assert!(!seen.iter().any(|a| a == "needs_input"), "it wanted you: {seen:?}");
        out
    })
}

#[test]
fn claude_code_working_blocked_and_idle_from_its_screen() {
    let d = Daemon::child();
    let scratch = Scratch::new("replay-claude");
    let r = Replay::install(&scratch.join("bin"), "claude", "claude_turn");
    // Six seconds of silence at its first turn: a long think.
    send(&d, 1, &format!("ILLOGICAL_REPLAY_PAUSE=working=6 {}", r.path()));

    // The folder trust dialog.
    r.reached("m blocked", 1);
    until(&d, 1, "needs_input");
    assert_eq!(card(&d, 1)["reason"]["headline"], "Claude Code asks whether to trust this folder");
    keys(&d, 1, &["Down", "Enter"]);
    r.reached("m idle", 1);
    until(&d, 1, "idle");

    // A prompt: working, through a long quiet think.
    send(&d, 1, "Run this shell command: sleep 4 && touch made-by-claude.txt");
    r.reached("m working", 1);
    until(&d, 1, "working");
    never_needs_you(&d, 1, || {
        let quiet = Instant::now();
        while Instant::now() - quiet < Duration::from_secs(5) {
            assert_eq!(attention(&d, 1), "working", "a quiet agent stopped working");
            std::thread::sleep(Duration::from_millis(100));
        }
    });

    // Its permission prompt wants you, and says for what.
    r.reached("m blocked", 2);
    until(&d, 1, "needs_input");
    assert_eq!(card(&d, 1)["reason"]["headline"], "Claude Code asks to run `sleep 4 && touch made-by-claude.txt`");

    // Approved: working through the command, then the turn ends. Nobody
    // watched it, so it's done.
    keys(&d, 1, &["Enter"]);
    r.reached("m working", 2);
    never_needs_you(&d, 1, || r.reached("m idle", 2));
    until(&d, 1, "done");
    assert_eq!(card(&d, 1)["reason"]["headline"], "Claude Code finished its turn");

    // The transcript view, idle and back: nothing changes.
    never_needs_you(&d, 1, || {
        keys(&d, 1, &["C-o"]);
        r.reached("m idle", 3);
        keys(&d, 1, &["C-o"]);
        r.reached("m idle", 4);
        std::thread::sleep(Duration::from_millis(800));
    });

    // Another turn, watched from the transcript view while it works.
    send(&d, 1, "Run this shell command: sleep 6 && touch made-again.txt");
    r.reached("m blocked", 3);
    until(&d, 1, "needs_input");
    keys(&d, 1, &["Enter"]);
    r.reached("m working", 3);
    until(&d, 1, "working");
    keys(&d, 1, &["C-o"]);
    r.reached("m working", 4);
    never_needs_you(&d, 1, || {
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(attention(&d, 1), "working");
        keys(&d, 1, &["C-o"]);
        r.reached("end", 1);
    });
    until(&d, 1, "done");

    // How it read the screen, rule by rule.
    let v = d.get("/api/panes/1/detection");
    assert_eq!(
        (v["agent"].as_str(), v["shown"].as_str(), v["fired"].as_str()),
        (Some("claude"), Some("idle"), Some("prompt_box")),
        "{v}"
    );
    let fired = v["rules"].as_array().unwrap().iter().find(|r| r["rule"] == "prompt_box").unwrap();
    assert_eq!(fired["region"], "prompt box");
    assert_eq!(fired["matched"], true);
}

#[test]
fn codex_from_its_screen() {
    let d = Daemon::child();
    let scratch = Scratch::new("replay-codex");
    let r = Replay::install(&scratch.join("bin"), "codex", "codex_turn");
    send(&d, 1, &r.path());
    r.reached("m blocked", 1);
    until(&d, 1, "needs_input");
    assert_eq!(card(&d, 1)["reason"]["headline"], "Codex asks whether to trust this folder");
    keys(&d, 1, &["Enter"]);
    r.reached("m idle", 1);
    until(&d, 1, "idle");
    send(&d, 1, "Run exactly this shell command: curl -sI https://example.com");
    r.reached("m working", 1);
    until(&d, 1, "working");
    r.reached("m blocked", 2);
    until(&d, 1, "needs_input");
    assert_eq!(card(&d, 1)["reason"]["headline"], "Codex asks to run `curl -sI https://example.com`");
    keys(&d, 1, &["Enter"]);
    r.reached("m working", 2);
    until(&d, 1, "working");
    r.reached("end", 1);
    until(&d, 1, "done");
}

/// A program that only mentions an agent isn't one: `python -c codex` runs
/// Python, so its screen isn't read as Codex's (it was, by the word `codex`
/// among its first three).
#[test]
fn python_dash_c_codex_is_not_codex() {
    let d = Daemon::child();
    let scratch = Scratch::new("not-codex");
    // `codex` there is Python code; Python prints what Codex would while it
    // works, from its startup hook, and stays.
    std::fs::write(
        scratch.join("sitecustomize.py"),
        "import time\nprint('• Working (1s • esc to interrupt)', flush=True)\ntime.sleep(30)\n",
    )
    .unwrap();
    send(&d, 1, &format!("PYTHONPATH={} python3 -c codex", scratch.display()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !d.get("/api/panes/1/capture").as_str().unwrap_or_default().contains("• Working") {
        assert!(Instant::now() < deadline, "it never printed");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(1));
    let v = d.get("/api/panes/1/detection");
    assert_eq!(v["agent"], Value::Null, "{v}");
    assert!(v["command"].as_str().unwrap_or_default().ends_with("-c codex"), "{v}");
}
