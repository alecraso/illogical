//! Agents in terminal panes, read off their screens (#145), live: the
//! replay agent plays recorded Claude Code and Codex sessions with their
//! timing in a pane, and the pane's attention follows what the screen
//! shows. No hooks are installed.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;
use crate::replay;

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
    send(&d, 1, &format!("ARUGULA_REPLAY_PAUSE=working=6 {}", r.path()));

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

/// It stays as it is for `secs`, wanting you or not.
fn holds(d: &Daemon, pane: u64, want: &str, secs: u64) {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        assert_eq!(
            attention(d, pane),
            want,
            "it moved while quiet: {}",
            d.get(&format!("/api/panes/{pane}/detection"))
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// #145/#255: going quiet isn't a stall for an agent with screen rules.
/// Quiet (no output for two seconds) used to make an agent "need you";
/// now an agent whose screen is read keeps what its screen says, at every
/// point of a turn: asking, thinking, and after it.
#[test]
fn quiet_changes_nothing_for_an_agent_with_rules() {
    let d = Daemon::child();
    let scratch = Scratch::new("replay-quiet");
    let r = Replay::install(&scratch.join("bin"), "claude", "claude_turn");
    send(&d, 1, &format!("ARUGULA_REPLAY_PAUSE=working=6 {}", r.path()));

    // Asking, and quiet: the question stays, and says what it asks.
    r.reached("m blocked", 1);
    until(&d, 1, "needs_input");
    let asked = card(&d, 1)["reason"]["headline"].clone();
    holds(&d, 1, "needs_input", 3);
    assert_eq!(card(&d, 1)["reason"]["headline"], asked);
    keys(&d, 1, &["Down", "Enter"]);
    r.reached("m idle", 1);
    until(&d, 1, "idle");

    // Thinking, and quiet for six seconds: working throughout.
    send(&d, 1, "Run this shell command: sleep 4 && touch made-by-claude.txt");
    r.reached("m working", 1);
    until(&d, 1, "working");
    holds(&d, 1, "working", 5);

    // Its permission prompt, left a while: the same card.
    r.reached("m blocked", 2);
    until(&d, 1, "needs_input");
    let asked = card(&d, 1)["reason"]["headline"].clone();
    assert_eq!(asked, "Claude Code asks to run `sleep 4 && touch made-by-claude.txt`");
    holds(&d, 1, "needs_input", 3);
    assert_eq!(card(&d, 1)["reason"]["headline"], asked);

    // Done, and quiet after: still done, for the turn it finished.
    keys(&d, 1, &["Enter"]);
    r.reached("m idle", 2);
    until(&d, 1, "done");
    holds(&d, 1, "done", 3);
    assert_eq!(card(&d, 1)["reason"]["headline"], "Claude Code finished its turn");
}

/// A stand-in chant that prints a recorded `chant audit --agents --format
/// json` document (`fixtures/chant/audit-agents.json`, chant 0.95.0 with
/// only Claude Code configured) and logs how it was run. `doc` replaces
/// the document.
struct Chant {
    bin: std::path::PathBuf,
}

impl Chant {
    fn install(dir: &std::path::Path) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let bin = dir.join("chant");
        std::fs::write(&bin, "#!/bin/sh\necho \"$*\" >> \"$0.argv\"\ncat \"$0.json\"\n").unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let c = Self { bin };
        c.doc(&Self::recorded());
        c
    }

    fn recorded() -> Value {
        let f = format!("{}/tests/fixtures/chant/audit-agents.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap()
    }

    fn doc(&self, doc: &Value) {
        std::fs::write(format!("{}.json", self.bin.display()), doc.to_string()).unwrap();
    }

    fn runs(&self) -> Vec<String> {
        std::fs::read_to_string(format!("{}.argv", self.bin.display()))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// #145/#255: the machine's agents, as `chant audit --agents` finds them,
/// decide whose screen rules run here. Only Claude Code is configured:
/// its screen is read, and Codex's isn't (it gets the activity heuristic,
/// and `describe --detection` says why) until chant finds it too.
#[test]
fn chants_inventory_decides_which_rules_run() {
    let scratch = Scratch::new("inventory");
    let chant = Chant::install(&scratch.join("chant"));
    let d = Daemon::child_env(&[], &[("ARUGULA_CHANT", chant.bin.to_str().unwrap())]);

    let deadline = Instant::now() + Duration::from_secs(15);
    let inv = loop {
        let v = d.get("/api/hosts/self/agents");
        if v["state"] != "reading" {
            break v;
        }
        assert!(Instant::now() < deadline, "chant was never asked: {v}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(inv["state"], "read", "{inv}");
    assert_eq!(inv["version"], "0.95.0");
    assert_eq!(inv["sites"][0]["runtime"], "claude");
    assert_eq!(inv["sites"][0]["scope"], "user");
    assert_eq!(inv["rules"], json!({"run": ["claude"], "off": ["codex"]}));
    assert_eq!(chant.runs(), ["audit --agents --scope system,user --format json --fail-on none"]);

    // Claude Code is configured: its screen is read.
    let claude = Replay::install(&scratch.join("bin"), "claude", "claude_turn");
    send(&d, 1, &claude.path());
    claude.reached("m blocked", 1);
    until(&d, 1, "needs_input");
    assert_eq!(card(&d, 1)["reason"]["headline"], "Claude Code asks whether to trust this folder");

    // Codex isn't: its trust prompt is only output, then quiet.
    let codex = Replay::install(&scratch.join("bin"), "codex", "codex_turn");
    let pane = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    send(&d, pane, &codex.path());
    codex.reached("m blocked", 1);
    let deadline = Instant::now() + Duration::from_secs(15);
    while d.get(&format!("/api/panes/{pane}/detection"))["unread"] != true {
        assert!(Instant::now() < deadline, "{}", d.get(&format!("/api/panes/{pane}/detection")));
        std::thread::sleep(Duration::from_millis(50));
    }
    let v = d.get(&format!("/api/panes/{pane}/detection"));
    assert_eq!((v["agent"].as_str(), v["fired"].as_str()), (Some("codex"), None), "{v}");
    assert_eq!(v["configured"], json!(["claude"]));
    until(&d, pane, "idle");
    holds(&d, pane, "idle", 1);
    // An agent chant didn't list asks it again (it may have just been
    // set up), but not within a minute of the last time.
    assert_eq!(chant.runs().len(), 1, "{:?}", chant.runs());

    // Now chant finds Codex too: its screen is read from then on, the
    // prompt it's showing included.
    let mut doc = Chant::recorded();
    let mut site = doc["sites"][0].clone();
    site["id"] = json!("user-codex");
    site["runtime"] = json!("codex");
    site["summary"] = json!("1 skill");
    doc["sites"].as_array_mut().unwrap().push(site);
    chant.doc(&doc);
    let inv = d.post("/api/hosts/self/agents/refresh", json!({}));
    assert_eq!(inv["rules"], json!({"run": ["claude", "codex"], "off": []}), "{inv}");
    until(&d, pane, "needs_input");
    assert_eq!(card(&d, pane)["reason"]["headline"], "Codex asks whether to trust this folder");
    let v = d.get(&format!("/api/panes/{pane}/detection"));
    assert_eq!((v["agent"].as_str(), v["unread"].as_bool()), (Some("codex"), None), "{v}");
}
