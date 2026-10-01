//! M6b against the real agent servers. These cost money (a few cents on
//! haiku each), so they only run when asked:
//!
//! ```sh
//! ILLOGICAL_REAL_AGENTS=claude,codex,fountain,vm,questions,tui,mcp cargo test -p illogicald --test agents_real
//! ```
//!
//! - `questions` (M6c): Claude Code's AskUserQuestion in an agent block,
//!   answered from its card.
//! - `tui` (M6c): Claude Code's TUI in a terminal pane with the
//!   AskUserQuestion hook (`illogical ask`): answered from the card, left to
//!   the terminal, and withdrawn by Esc. Runs in `target/m6c-tui`.
//! - `mcp` (M6c): an MCP server's form and sign-in link (`fake_mcp.py`)
//!   through Claude Code in an agent block.
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

/// M6c: Claude Code's AskUserQuestion in an agent block (one haiku turn):
/// the question card, answered by its fields, and the agent goes on with
/// the answer.
#[test]
fn claude_code_asks_a_question_in_an_agent_block() {
    if !wanted("questions") || !adapter("claude", "claude-agent-acp") {
        return;
    }
    let d = Daemon::child();
    let cwd = scratch(&d);
    let prompt = "Before anything else, call the AskUserQuestion tool once with one question, header 'Colour': \
                  which colour I prefer, Red or Blue (single choice). Then reply with just the colour I chose.";
    let config = json!({ "agent": "claude", "model": "haiku", "cwd": cwd, "prompt": prompt });
    let id = d.open_with(json!({ "type": "agent", "config": config }));
    let w = d.get(&format!("/api/panes/{id}/wait?until=needs-input&timeout=120"));
    let ask = &w["ask"];
    assert_eq!(ask["kind"], "questions", "{w} {}", text(&d, id));
    assert_eq!(ask["questions"][0]["header"], "Colour");
    d.call(id, "answer", json!({ "id": ask["id"], "content": { "question_0": "Blue" } }));
    assert_eq!(d.wait_secs(id, "idle", 120), "done", "{}", text(&d, id));
    let t = text(&d, id);
    assert!(t.contains("_Answered: ") && t.contains("→ Blue"), "{t}");
    let reply = t.rsplit("_Answered:").next().unwrap_or_default().to_lowercase();
    assert!(reply.contains("blue"), "{t}");
}

/// The environment Claude Code sets for its own children, which would make
/// a nested `claude` think it's a subagent.
const CLAUDE_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "AI_AGENT",
];

fn screen(d: &Daemon, pane: u64) -> String {
    d.raw("GET", &format!("/api/panes/{pane}/capture"), None).1
}

fn until(d: &Daemon, pane: u64, what: &str, secs: u64, f: impl Fn(&str) -> bool) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let s = screen(d, pane);
        if f(&s) {
            return s;
        }
        assert!(std::time::Instant::now() < deadline, "waiting for {what}:\n{s}");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn pane_ask(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes")
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == pane)
        .map(|p| p["ask"].clone())
        .unwrap_or_default()
}

/// M6c: Claude Code's TUI in a terminal pane, with the hook from README (a
/// settings file of its own; your settings aren't read). Three haiku turns:
/// answered from the card (no picker), "Answer in terminal" (the picker),
/// and Esc in the TUI (the card withdrawn).
#[test]
fn claude_code_in_a_terminal_asks_through_the_hook() {
    if !wanted("tui") {
        return;
    }
    let d = Daemon::child();
    let cli = std::path::Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    assert!(
        std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap().success()
    );
    // Under target/ (inside a repo you trust), so there's no trust dialog.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/m6c-tui");
    std::fs::create_dir_all(&dir).unwrap();
    let settings = d.sessions.join("hook-settings.json");
    let hook = json!({ "hooks": { "PreToolUse": [{ "matcher": "AskUserQuestion",
        "hooks": [{ "type": "command", "command": format!("{} ask", cli.display()), "timeout": 604800 }] }] } });
    std::fs::write(&settings, hook.to_string()).unwrap();
    let unset: String = CLAUDE_ENV.iter().map(|v| format!("-u {v} ")).collect();
    // The reply is the colour in capitals, which the prompt never shows.
    let ask = |n: u32| {
        format!(
            "Question {n}: call the AskUserQuestion tool once with one question, header 'Colour': which colour I \
             prefer, Red or Blue (single choice). Then reply with just the colour I chose, in capital letters."
        )
    };
    let cmd = format!(
        "cd {} && env {unset}claude --model haiku --setting-sources local --settings {} '{}'",
        dir.canonicalize().unwrap().display(),
        settings.display(),
        ask(1)
    );
    let pane = d.post("/api/run", json!({ "command": cmd }))["pane"].as_u64().unwrap();
    let send = |text: &str| {
        d.post(&format!("/api/panes/{pane}/send"), json!({ "text": text }));
        std::thread::sleep(std::time::Duration::from_millis(300));
        d.post(&format!("/api/panes/{pane}/keys"), json!({ "keys": ["Enter"] }));
    };

    // 1. Answered from the card: Claude Code never shows its picker.
    d.wait_for("the card", || pane_ask(&d, pane).is_object() || screen(&d, pane).contains("Enter to select"));
    let a = pane_ask(&d, pane);
    assert_eq!(a["questions"][0]["header"], "Colour", "{a}\n{}", screen(&d, pane));
    assert!(!screen(&d, pane).contains("Enter to select"));
    d.call(pane, "answer", json!({ "id": a["id"], "content": { "question_0": "Blue" } }));
    let s = until(&d, pane, "the reply", 120, |s| s.contains("BLUE") || s.contains("RED"));
    assert!(s.contains("BLUE"), "{s}");
    assert!(!s.contains("Enter to select"), "{s}");

    // 2. "Answer in terminal": the picker comes back; Enter picks Red.
    send(&ask(2));
    d.wait_for("the second card", || pane_ask(&d, pane).is_object());
    d.call(pane, "terminal", json!({}));
    until(&d, pane, "the picker", 60, |s| s.contains("Enter to select"));
    d.post(&format!("/api/panes/{pane}/keys"), json!({ "keys": ["Enter"] }));
    until(&d, pane, "the reply", 120, |s| s.matches("RED").count() + s.matches("BLUE").count() >= 2);

    // 3. Esc in the TUI interrupts the hook: the card goes.
    send(&ask(3));
    d.wait_for("the third card", || pane_ask(&d, pane).is_object());
    d.post(&format!("/api/panes/{pane}/keys"), json!({ "keys": ["Escape"] }));
    d.wait_for("the card withdrawn", || pane_ask(&d, pane).is_null());
    d.post(&format!("/api/panes/{pane}/keys"), json!({ "keys": ["C-c", "C-c"] }));
}

/// M6c: an MCP server's form and sign-in link (`fake_mcp.py`) through
/// Claude Code in an agent block (one haiku turn): its tools are approved,
/// the form is filled in, the link opened, and its card closes when the
/// server says the sign-in is complete.
#[test]
fn an_mcp_servers_form_and_sign_in_link() {
    if !wanted("mcp") || !adapter("claude", "claude-agent-acp") {
        return;
    }
    let d = Daemon::child();
    let cwd = scratch(&d);
    let log = d.sessions.join("mcp.log");
    let server = format!("fake=python3 {}/tests/fake_mcp.py {}", env!("CARGO_MANIFEST_DIR"), log.display());
    let prompt = "Call the mcp__fake__pick_size tool, then the mcp__fake__sign_in tool, then report both results \
                  in one line each and stop.";
    let config = json!({ "agent": "claude", "model": "haiku", "cwd": cwd, "prompt": prompt, "mcp_servers": [server] });
    let id = d.open_with(json!({ "type": "agent", "config": config }));
    let (mut form, mut link) = (false, false);
    loop {
        let state = d.wait_secs(id, "idle", 120);
        let s = d.state(id);
        if state != "needs_input" {
            break;
        }
        if s["pending"].as_array().is_some_and(|p| !p.is_empty()) {
            d.call(id, "approve", json!({}));
            continue;
        }
        let Some(a) = s["asks"].as_array().and_then(|a| a.iter().find(|a| a["accepted"] != true)).cloned() else {
            panic!("needs input, but for what? {}", text(&d, id));
        };
        match a["kind"].as_str() {
            Some("form") => {
                assert_eq!(a["schema"]["properties"]["size"]["enumNames"], json!(["Small", "Medium", "Large"]), "{a}");
                d.call(id, "answer", json!({ "id": a["id"], "content": { "size": "M", "qty": 2, "gift": true } }));
                form = true;
            }
            Some("url") => {
                assert_eq!(a["url"], "https://example.com/fake-signin");
                d.call(id, "answer", json!({ "id": a["id"] }));
                link = true;
                // Closed when the server says it's complete.
                d.wait_for("elicitation/complete", || d.state(id)["asks"].as_array().is_none_or(|a| a.is_empty()));
            }
            k => panic!("unexpected {k:?}: {a}"),
        }
    }
    let t = text(&d, id);
    let got = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(form && link, "{t}\nthe MCP server got:\n{got}");
    assert!(t.contains(r#""size":"M""#) || t.contains(r#""size": "M""#), "{t}");
}
