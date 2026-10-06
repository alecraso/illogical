//! A pane's exact agent conversation back after a reboot (#146): the daemon
//! is stopped the way a reboot stops it (what it started goes too) and
//! started again on the same state. `claude` is the replay agent, which
//! keeps Claude Code's records of its conversation in the test's own
//! `CLAUDE_CONFIG_DIR` and logs the argv of every start.
//!
//! The same with a real reboot of a box (`docker restart` on the testnet)
//! is Track A's: it can drive these helpers in a box-systemd container.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;
use crate::replay;

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use agentd::*;
use replay::Replay;
use serde_json::{Value, json};

const A: &str = "0f3c2a9e-1b7d-4c8e-9f00-00000000000a";
const B: &str = "0f3c2a9e-1b7d-4c8e-9f00-00000000000b";

struct Rig {
    d: Daemon,
    claude: Replay,
    repo: PathBuf,
    config: PathBuf,
    _scratch: Scratch,
}

/// A daemon whose panes find the replayed `claude` first on PATH, and a
/// repository to run it in.
fn start(tag: &str) -> Rig {
    let scratch = Scratch::new(tag);
    let claude = Replay::install(&scratch.join("bin"), "claude", "claude_turn");
    let repo = scratch.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let config = scratch.join("claude-config");
    let path = format!("{}:{}", scratch.join("bin").display(), std::env::var("PATH").unwrap());
    let config_dir = config.to_str().unwrap();
    let env = [("PATH", path.as_str()), ("CLAUDE_CONFIG_DIR", config_dir), ("ILLOGICAL_REPLAY_CONFIG", config_dir)];
    let d = Daemon::child_env(&[], &env);
    Rig { d, claude, repo, config, _scratch: scratch }
}

fn send(d: &Daemon, pane: u64, text: &str) {
    d.post(&format!("/api/panes/{pane}/send"), json!({"text": text, "enter": true}));
}

fn pane(d: &Daemon, id: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == id).cloned().unwrap_or_default()
}

/// The pane's screen with its lines run together: what's longer than the
/// pane is wide wraps.
fn screen(d: &Daemon, id: u64) -> String {
    d.get(&format!("/api/panes/{id}/capture")).as_str().unwrap_or_default().replace('\n', "")
}

fn until(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The transcript the replay keeps for a conversation started in `cwd`.
fn transcript(config: &Path, cwd: &Path, id: &str) -> PathBuf {
    let slug: String =
        cwd.display().to_string().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    config.join("projects").join(slug).join(format!("{id}.jsonl"))
}

/// Starts of `claude` in a pane after `from`: their argv.
fn starts_in(r: &Replay, pane: u64, from: usize) -> Vec<Value> {
    r.starts()
        .into_iter()
        .skip(from)
        .filter(|s| s["pane"].as_str() == Some(pane.to_string().as_str()))
        .map(|s| s["argv"].clone())
        .collect()
}

/// Claude Code (the replay) in `pane`, holding conversation `id`.
fn claude_in(b: &Rig, pane: u64, id: &str) {
    send(&b.d, pane, &format!("cd {} && ILLOGICAL_REPLAY_SESSION={id} claude", b.repo.display()));
    until("its trust dialog", || screen(&b.d, pane).contains("trust this folder"));
}

/// Its hooks say which conversation it holds, as `illogical hook` would.
fn hook(b: &Rig, pane: u64, id: &str) {
    let t = transcript(&b.config, &b.repo, id);
    let hook = json!({"hook_event_name": "SessionStart", "source": "startup", "session_id": id,
                      "transcript_path": t, "cwd": b.repo});
    b.d.post(&format!("/api/panes/{pane}/hook"), hook);
}

#[test]
fn two_panes_in_one_repo_each_resume_their_own_conversation() {
    let mut b = start("resume-two");
    // Pane 1 says through its hooks; pane 2 has none, and is found by its
    // process.
    claude_in(&b, 1, A);
    hook(&b, 1, A);
    let two = b.d.post("/api/run", json!({"split": 1}))["pane"].as_u64().unwrap();
    claude_in(&b, two, B);
    until("both conversations known", || {
        pane(&b.d, 1)["resumes"] == format!("Claude Code conversation {}", &A[..8])
            && pane(&b.d, two)["resumes"] == format!("Claude Code conversation {}", &B[..8])
    });
    assert_eq!(pane(&b.d, 1)["policy"]["kind"], "resume");
    let before = b.claude.starts().len();

    // The reboot.
    std::thread::sleep(Duration::from_millis(1500));
    b.d.stop();
    b.d.start();
    until("both resumed", || {
        starts_in(&b.claude, 1, before).len() == 1 && starts_in(&b.claude, two, before).len() == 1
    });
    assert_eq!(starts_in(&b.claude, 1, before), vec![json!(["--resume", A])]);
    assert_eq!(starts_in(&b.claude, two, before), vec![json!(["--resume", B])]);
    for p in [1, two] {
        until("its conversation on screen", || screen(&b.d, p).contains("trust this folder"));
    }
    // Still known as running after the daemon has looked at the panes again
    // (it does each second): the resumed agent is in the foreground (#376).
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(pane(&b.d, 1)["resumes"], format!("Claude Code conversation {}", &A[..8]));
    assert_eq!(pane(&b.d, two)["resumes"], format!("Claude Code conversation {}", &B[..8]));

    // And a second reboot resumes them again.
    let before = b.claude.starts().len();
    b.d.stop();
    b.d.start();
    until("both resumed again", || {
        starts_in(&b.claude, 1, before).len() == 1 && starts_in(&b.claude, two, before).len() == 1
    });
    assert_eq!(starts_in(&b.claude, 1, before), vec![json!(["--resume", A])]);
    assert_eq!(starts_in(&b.claude, two, before), vec![json!(["--resume", B])]);
}

#[test]
fn a_deleted_transcript_comes_back_as_a_shell_that_says_so() {
    let mut b = start("resume-gone");
    claude_in(&b, 1, A);
    hook(&b, 1, A);
    until("its conversation known", || pane(&b.d, 1)["resumes"].is_string());
    std::fs::remove_file(transcript(&b.config, &b.repo, A)).unwrap();
    let before = b.claude.starts().len();
    std::thread::sleep(Duration::from_millis(1500));
    b.d.stop();
    b.d.start();
    until("the note", || screen(&b.d, 1).contains("its transcript is gone"));
    let s = screen(&b.d, 1);
    assert!(s.contains(&format!("[can't resume Claude Code conversation {A}: its transcript is gone]")), "{s}");
    // A shell there, and nothing started.
    send(&b.d, 1, "echo here-$((6*7)) $PWD");
    until("a shell", || screen(&b.d, 1).contains(&format!("here-42 {}", b.repo.display())));
    assert!(starts_in(&b.claude, 1, before).is_empty());
}

#[test]
fn a_session_id_never_reaches_a_shell() {
    let mut b = start("resume-meta");
    let pwned = b.repo.join("pwned");
    let evil = format!("x'; touch {}; '$(touch {})", pwned.display(), pwned.display());
    claude_in(&b, 1, A);
    // A hook with it is refused: the conversation stays the one it was.
    hook(&b, 1, A);
    until("its conversation known", || pane(&b.d, 1)["resumes"].is_string());
    b.d.post("/api/panes/1/hook", json!({"hook_event_name": "SessionStart", "session_id": evil}));
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(pane(&b.d, 1)["resumes"], format!("Claude Code conversation {}", &A[..8]));
    b.d.stop();
    // Written into the layout by hand, it isn't run either.
    let path = b.d.state.join("layout.json");
    let mut layout: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    layout["panes"]["1"]["session"]["id"] = json!(evil);
    std::fs::write(&path, serde_json::to_vec_pretty(&layout).unwrap()).unwrap();
    let before = b.claude.starts().len();
    b.d.start();
    until("the note", || screen(&b.d, 1).contains("not resuming Claude Code: its session id isn't one"));
    std::thread::sleep(Duration::from_millis(500));
    assert!(!pwned.exists());
    assert!(starts_in(&b.claude, 1, before).is_empty());
}
