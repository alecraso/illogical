//! Prompt an agent and wait in one call (#147): `POST /api/panes/N/prompt`
//! (`illogical send --wait`), against the replay agent playing a recorded
//! Claude Code in a terminal pane, with no hooks.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

mod agentd;
mod replay;

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use agentd::*;
use replay::Replay;
use serde_json::{Value, json};

fn send(d: &Daemon, pane: u64, text: &str) {
    d.post(&format!("/api/panes/{pane}/send"), json!({"text": text, "enter": true}));
}

fn keys(d: &Daemon, pane: u64, keys: &[&str]) {
    d.post(&format!("/api/panes/{pane}/keys"), json!({ "keys": keys }));
}

fn screen(d: &Daemon, pane: u64) -> String {
    d.get(&format!("/api/panes/{pane}/capture")).as_str().unwrap_or_default().to_owned()
}

fn prompt(d: &Daemon, pane: u64, body: Value) -> Value {
    d.post(&format!("/api/panes/{pane}/prompt"), body)
}

fn cli() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

/// Claude Code (recorded) in pane 1, past its trust dialog, at its prompt.
fn claude_at_its_prompt(d: &Daemon, dir: &Path) -> Replay {
    let r = Replay::install(&dir.join("bin"), "claude", "claude_turn");
    send(d, 1, &r.path());
    r.reached("m blocked", 1);
    keys(d, 1, &["Down", "Enter"]);
    r.reached("m idle", 1);
    // Its screen read as idle: what a caller would see before prompting.
    let deadline = Instant::now() + Duration::from_secs(10);
    while d.get("/api/panes/1/detection")["shown"] != "idle" {
        assert!(Instant::now() < deadline, "{}", d.get("/api/panes/1/detection"));
        std::thread::sleep(Duration::from_millis(50));
    }
    r
}

#[test]
fn a_prompt_waits_through_the_turn_and_stops_at_a_question() {
    let d = Daemon::child();
    let scratch = Scratch::new("prompt-claude");
    let r = claude_at_its_prompt(&d, &scratch);

    // One call: it starts working, then asks to run something.
    let started = Instant::now();
    let v = prompt(&d, 1, json!({"text": "Run this shell command: sleep 4 && touch made-by-claude.txt"}));
    assert_eq!(v["result"], "needs_input", "{v}");
    assert_eq!(v["question"], "Claude Code asks to run `sleep 4 && touch made-by-claude.txt`", "{v}");
    // (The recording marks it a moment after drawing it.)
    r.reached("m blocked", 2);
    assert!(started.elapsed() < Duration::from_secs(20));

    // Prompting it again while it waits on that types nothing, and says
    // what it waits on.
    let (before, logged) = (screen(&d, 1), r.log().len());
    let v = prompt(&d, 1, json!({"text": "something else"}));
    assert_eq!(v["result"], "blocked", "{v}");
    assert_eq!(v["question"], "Claude Code asks to run `sleep 4 && touch made-by-claude.txt`", "{v}");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(screen(&d, 1), before, "it typed into the dialog");
    assert_eq!(r.log().len(), logged, "the recording moved on: {:?}", r.log());

    // Answering it (Enter: "1. Yes"), through to the end of the turn: the
    // four-second command and all, with no sleep here.
    let v = prompt(&d, 1, json!({"text": "", "answering": true}));
    assert_eq!(v["result"], "done", "{v}");
    r.reached("m idle", 2);

    // The recording now waits for Ctrl-O, so a prompt starts nothing:
    // stalled, within the stall window, with the screen to see why.
    let started = Instant::now();
    let v = prompt(&d, 1, json!({"text": "are you there?", "stall": 2}));
    assert_eq!(v["result"], "stalled", "{v}");
    assert!(v["why"].as_str().unwrap().contains("no sign of work within 2s"), "{v}");
    assert!(v["screen"].as_str().unwrap().contains("? for shortcuts"), "{v}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn a_pane_without_an_agent_is_not_typed_at() {
    let d = Daemon::child();
    let v = prompt(&d, 1, json!({"text": "echo typed-$((6*7))"}));
    assert_eq!(v["result"], "stalled", "{v}");
    assert!(v["why"].as_str().unwrap().contains("no agent running; nothing was typed"), "{v}");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!screen(&d, 1).contains("typed-"), "{}", screen(&d, 1));
}

#[test]
fn send_wait_from_the_cli() {
    let d = Daemon::child();
    let scratch = Scratch::new("prompt-cli");
    let _r = claude_at_its_prompt(&d, &scratch);
    let illogical = |args: &[&str]| {
        let out = Command::new(cli()).arg("--socket").arg(d.sock()).args(args).output().unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let (code, out) =
        illogical(&["send", "%1", "--wait", "Run this shell command: sleep 4 && touch made-by-claude.txt"]);
    assert_eq!(code, Some(2), "{out}");
    assert_eq!(out.trim(), "%1 asks: Claude Code asks to run `sleep 4 && touch made-by-claude.txt`");
    let (code, out) = illogical(&["send", "%1", "--wait", "anything"]);
    assert_eq!(code, Some(2), "{out}");
    assert!(out.contains("nothing was typed"), "{out}");
    let (code, out) = illogical(&["send", "%1", "--wait", "--answering", ""]);
    assert_eq!((code, out.trim()), (Some(0), "%1 finished its turn"));
}
