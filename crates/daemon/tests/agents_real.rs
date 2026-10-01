//! M6b against the real agent servers. These cost money (a few cents on
//! haiku each), so they only run when asked:
//!
//! ```sh
//! ILLOGICAL_REAL_AGENTS=claude,codex,fountain,vm cargo test -p illogicald --test agents_real
//! ```
//!
//! - `claude`: Claude Code through the pinned `claude-agent-acp`, on your
//!   own login, in a scratch git repo: a command it asks to run, approved.
//! - `codex`: `codex-acp` against your `codex`.
//! - `fountain`: the Fountain agent in `ILLOGICAL_FOUNTAIN_AGENT` (an
//!   existing one; nothing is created but a conversation, deleted after).
//! - `vm`: Claude Code in a throwaway wisp VM, with the token in
//!   `~/.config/illogical/claude-oauth-token` (or an API key in
//!   `…/anthropic-key`); skipped if neither exists.
//!
//! Adapters are found in `~/.local/share/illogical/agents/` (see README).

mod agentd;

use std::path::PathBuf;

use agentd::*;
use serde_json::{Value, json};

fn wanted(what: &str) -> bool {
    let on = std::env::var("ILLOGICAL_REAL_AGENTS").unwrap_or_default();
    let yes = on.split(',').any(|w| w.trim() == what);
    if !yes {
        eprintln!("skipping: set ILLOGICAL_REAL_AGENTS={what} to run (it costs money)");
    }
    yes
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
}

fn adapter(dir: &str, bin: &str) -> bool {
    let ok = home().join(".local/share/illogical/agents").join(dir).join("node_modules/.bin").join(bin).exists();
    if !ok {
        eprintln!("skipping: {bin} isn't installed in ~/.local/share/illogical/agents/{dir}");
    }
    ok
}

/// A scratch git repo, so nothing the agent writes lands in ours.
fn scratch(d: &Daemon) -> String {
    let dir = d.sessions.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    std::process::Command::new("git").arg("init").arg("-q").arg(&dir).status().unwrap();
    dir.display().to_string()
}

fn text(d: &Daemon, id: u64) -> String {
    d.raw("GET", &format!("/api/panes/{id}/capture"), None).1
}

/// Ask it to run a command with a side effect; approve; see the output.
fn approve_a_command(d: &Daemon, id: u64, secs: u64) {
    assert_eq!(d.wait_secs(id, "needs-input", secs), "needs_input", "{}", text(d, id));
    let s = d.state(id);
    assert!(s["pending"][0]["title"].as_str().is_some_and(|t| t.contains("marker.txt")), "{s}");
    d.call(id, "approve", json!({}));
    assert_eq!(d.wait_secs(id, "idle", secs), "done", "{}", text(d, id));
    let t = text(d, id);
    assert!(t.contains("m6b-ok"), "the command's output is in its card: {t}");
}

const PROMPT: &str = "Use the Bash tool to run exactly this command: echo m6b-ok > marker.txt; cat marker.txt   Then reply with just DONE.";

#[test]
fn claude_code_asks_and_runs() {
    if !wanted("claude") || !adapter("claude", "claude-agent-acp") {
        return;
    }
    let d = Daemon::child();
    let cwd = scratch(&d);
    let config = json!({ "agent": "claude", "model": "haiku", "cwd": cwd, "prompt": PROMPT });
    let id = d.open_with(json!({ "type": "agent", "config": config }));
    approve_a_command(&d, id, 120);
    let s = d.state(id);
    assert_eq!(s["server"]["name"], "@agentclientprotocol/claude-agent-acp");
    assert!(s["cost"]["last_turn"].as_f64().unwrap() > 0.0, "{s}");
    assert!(!PathBuf::from(&cwd).join(".claude/settings.local.json").exists(), "never the agent's allow_always");
}

#[test]
fn codex_runs() {
    if !wanted("codex") || !adapter("codex", "codex-acp") {
        return;
    }
    let d = Daemon::child();
    let cwd = scratch(&d);
    let prompt = "Run this shell command: echo m6b-ok > marker.txt; cat marker.txt   Then reply with just DONE.";
    let id = d.open_with(json!({ "type": "agent", "config": { "agent": "codex", "cwd": cwd, "prompt": prompt } }));
    // Codex runs it in its own sandbox without asking.
    let state = d.wait_secs(id, "idle", 180);
    if state == "needs_input" && !d.state(id)["pending"].as_array().unwrap().is_empty() {
        d.call(id, "approve", json!({}));
        d.wait_secs(id, "idle", 180);
    }
    let t = text(&d, id);
    assert!(t.contains("m6b-ok"), "{t}");
    assert_eq!(d.state(id)["server"]["name"], "@agentclientprotocol/codex-acp");
}

#[test]
fn a_fountain_agent_asks_and_runs() {
    if !wanted("fountain") {
        return;
    }
    let Ok(agent) = std::env::var("ILLOGICAL_FOUNTAIN_AGENT") else {
        eprintln!("skipping: set ILLOGICAL_FOUNTAIN_AGENT to an existing agent");
        return;
    };
    let d = Daemon::child();
    let id = d.open_with(
        json!({ "type": "agent", "config": { "agent": "fountain", "fountain_agent": agent, "prompt": PROMPT } }),
    );
    approve_a_command(&d, id, 240);
    let session = d.state(id)["session_id"].as_str().unwrap().to_owned();
    let _ = std::process::Command::new(home().join(".local/bin/fountain")).args(["conv", "delete", &session]).status();
}

#[test]
fn claude_code_in_a_vm() {
    if !wanted("vm") {
        return;
    }
    let config = home().join(".config/illogical");
    if !config.join("claude-oauth-token").exists() && !config.join("anthropic-key").exists() {
        eprintln!("skipping: no credentials for VM agents in ~/.config/illogical");
        return;
    }
    let token = home().join(".local/share/wisp/token");
    if !token.exists() {
        eprintln!("skipping: no wisp");
        return;
    }
    let d = Daemon::child_with(&["--wisp-token-file", token.to_str().unwrap()]);
    let config = json!({ "agent": "claude", "model": "haiku", "prompt": PROMPT });
    let id = d.open_with(json!({ "type": "agent", "vm": true, "config": config }));
    // The first start installs Node and the adapter in the machine.
    approve_a_command(&d, id, 300);
    let machines: Value = d.get("/api/machines");
    let sprite = machines[0]["sprite"].as_str().unwrap().to_owned();
    assert!(sprite.starts_with("illogical-eph-"), "{machines}");
    // Closing it deletes its machine.
    d.post(&format!("/api/panes/{id}/close"), json!({}));
    d.wait_for("its machine to go", || d.get("/api/machines").as_array().unwrap().is_empty());
}
