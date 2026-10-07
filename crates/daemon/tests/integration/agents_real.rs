//! M6b against the real agent servers. These cost money (a few cents on
//! haiku each), so they only run when asked:
//!
//! ```sh
//! ARUGULA_REAL_AGENTS=claude,codex,fountain,vm,questions,tui,mcp,mcp-cc,screen cargo test -p arugulad --test integration agents_real::
//! ```
//!
//! - `screen` (#145, #146, #147): Claude Code's TUI in a terminal pane with
//!   no hooks at all (`--setting-sources local`). Its approval read off the
//!   screen as NeedsInput naming the command, `prompt` waited through to
//!   the end of the turn, then a daemon restart resumes the same
//!   conversation (`claude --resume <id>`, the id from its session file).
//!   With `ANTHROPIC_API_KEY` set (CI's secret) it runs in a
//!   `CLAUDE_CONFIG_DIR` of its own; without, on your login. The replayed
//!   versions of all of this run every time (`agent_screens`, `prompt`,
//!   `resume`).
//! - `questions` (M6c): Claude Code's AskUserQuestion in an agent block,
//!   answered from its card.
//! - `tui` (M6c): Claude Code's TUI in a terminal pane with the
//!   AskUserQuestion hook (`arugula ask`): answered from the card, left to
//!   the terminal, and withdrawn by Esc. Runs in `target/m6c-tui`.
//! - `team` (M29): Claude Code's TUI in a terminal pane with the team
//!   answers hooks (`arugula hook`, `arugula inbox`): a tool permission
//!   allowed from its card, then a follow-up through the inbox wakes it.
//!   Runs in `target/m29-tui`.
//! - `mcp` (M6c): an MCP server's form and sign-in link (`fake_mcp.py`)
//!   through Claude Code in an agent block.
//! - `mcp-cc` (M16): Claude Code outside Arugula (`claude -p`, haiku),
//!   with `arugula mcp` as its MCP server: a build that fails after a
//!   while, waited through; it reads why, fixes it and reruns, and the
//!   pane says "started by mcp:claude-code". With `mcp-vm` too, the build
//!   runs in a throwaway wisp VM pane (needs wisp).
//! - `claude`: Claude Code through the pinned `claude-agent-acp`, on your
//!   own login, in a scratch git repo: a command it asks to run, approved.
//!   And (M44) Claude Code wearing your Fountain account's `games` agent
//!   (read with your `fountain` login; inline skills only, no MCP, so no
//!   secrets): it names its three skills.
//! - `codex`: `codex-acp` against your `codex`.
//! - `fountain`: the Fountain agent in `ARUGULA_FOUNTAIN_AGENT` (an
//!   existing one; nothing is created but a conversation, deleted after).
//! - `vm`: Claude Code in a throwaway wisp VM, with the token in
//!   `~/.config/arugula/claude-oauth-token` (or an API key in
//!   `…/anthropic-key`); skipped if neither exists.
//!
//! Adapters are found in `~/.local/share/arugula/agents/` (see README).

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::path::PathBuf;

use agentd::*;
use serde_json::{Value, json};

fn wanted(what: &str) -> bool {
    let on = std::env::var("ARUGULA_REAL_AGENTS").unwrap_or_default();
    let yes = on.split(',').any(|w| w.trim() == what);
    if !yes {
        eprintln!("SKIP: set ARUGULA_REAL_AGENTS={what} to run (it costs money)");
    }
    yes
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
}

fn adapter(dir: &str, bin: &str) -> bool {
    let ok = home().join(".local/share/arugula/agents").join(dir).join("node_modules/.bin").join(bin).exists();
    if !ok {
        eprintln!("SKIP: {bin} isn't installed in ~/.local/share/arugula/agents/{dir}");
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

/// M44: a Fountain agent worn here: `games`' skills come as a plugin.
#[test]
fn claude_code_wears_games() {
    if !wanted("claude") || !adapter("claude", "claude-agent-acp") {
        return;
    }
    let d = Daemon::child();
    let cwd = scratch(&d);
    let prompt = "Without using any tools: list, one per line, the names of the skills you can use whose names \
                  contain love, pixi or screenshot. Then say DONE.";
    let config = json!({ "agent": "claude", "as_fountain": "games", "model": "haiku", "cwd": cwd, "prompt": prompt });
    let id = d.open_with(json!({ "type": "agent", "config": config }));
    assert_eq!(d.wait_secs(id, "idle", 180), "done", "{}", text(&d, id));
    let s = d.state(id);
    assert_eq!(s["worn"]["skills"], json!(["love2d", "pixijs", "screenshots-in-prs"]), "{s}");
    let t = text(&d, id);
    let reply = t.rsplit("## You").next().unwrap_or("");
    for skill in ["love2d", "pixijs", "screenshots-in-prs"] {
        assert!(reply.contains(skill), "{skill} not named: {t}");
    }
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
    let Ok(agent) = std::env::var("ARUGULA_FOUNTAIN_AGENT") else {
        eprintln!("SKIP: set ARUGULA_FOUNTAIN_AGENT to an existing agent");
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
    let config = home().join(".config/arugula");
    if !config.join("claude-oauth-token").exists() && !config.join("anthropic-key").exists() {
        eprintln!("SKIP: no credentials for VM agents (~/.config/arugula/claude-oauth-token or anthropic-key)");
        return;
    }
    let token = home().join(".local/share/wisp/token");
    if !token.exists() {
        eprintln!("SKIP: no wisp token (~/.local/share/wisp/token)");
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
    let cli = std::path::Path::new(env!("CARGO_BIN_EXE_arugulad")).with_file_name("arugula");
    assert!(
        std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "arugula"]).status().unwrap().success()
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

/// M29: Claude Code's TUI in a terminal pane with the team answers hooks,
/// from a settings file of its own (your settings aren't read). Two haiku
/// turns: a Bash permission allowed from the card (and recorded as
/// allowed), then a follow-up delivered by the background Stop hook, which
/// wakes the idle agent without typing into its prompt.
#[test]
fn claude_code_permission_and_follow_up_through_the_hooks() {
    if !wanted("team") {
        return;
    }
    let d = Daemon::child();
    let cli = std::path::Path::new(env!("CARGO_BIN_EXE_arugulad")).with_file_name("arugula");
    assert!(
        std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "arugula"]).status().unwrap().success()
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/m29-tui");
    std::fs::create_dir_all(&dir).unwrap();
    let settings = d.sessions.join("team-settings.json");
    let hook = |cmd: &str| json!([{ "hooks": [{ "type": "command", "command": format!("{} {cmd}", cli.display()), "timeout": 604800 }] }]);
    let inbox = json!([{ "hooks": [{ "type": "command", "command": format!("{} inbox", cli.display()), "asyncRewake": true,
        "timeout": 86400 }] }]);
    let hooks = json!({ "hooks": {
        "PermissionRequest": hook("hook"), "PreToolUse": hook("hook"), "PostToolUse": hook("hook"),
        "PostToolUseFailure": hook("hook"), "UserPromptSubmit": hook("hook"), "Stop": inbox, "SessionStart": inbox,
    } });
    std::fs::write(&settings, hooks.to_string()).unwrap();
    let unset: String = CLAUDE_ENV.iter().map(|v| format!("-u {v} ")).collect();
    // A command that writes, so Claude Code asks (S18).
    let prompt = "Use the Bash tool to run exactly this command: touch m29-marker.txt   Then reply with just DONE.";
    let cmd = format!(
        "cd {} && env {unset}claude --model haiku --setting-sources local --settings {} '{prompt}'",
        dir.canonicalize().unwrap().display(),
        settings.display(),
    );
    let pane = d.post("/api/run", json!({ "command": cmd }))["pane"].as_u64().unwrap();
    until(&d, pane, "the card", 90, |_| pane_ask(&d, pane)["kind"] == "permission");
    let a = pane_ask(&d, pane);
    assert_eq!(a["tool"], "Bash", "{a}");
    assert!(a["tool_call_id"].as_str().is_some_and(|t| t.starts_with("toolu_")), "matched to its PreToolUse: {a}");
    let r = d.post("/api/attention/act", json!({ "action": "allow", "pane": pane }));
    assert_eq!(r["results"][0]["ok"], true, "{r}");
    until(&d, pane, "DONE", 120, |s| s.contains("DONE"));
    let info = d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap();
    assert_eq!(info["answered"]["how"], "allowed", "{info}");
    d.wait_for("the inbox", || {
        d.get("/api/panes").as_array().unwrap().iter().any(|p| p["id"] == pane && p["inbox"] == true)
    });
    let r = d.post(&format!("/api/panes/{pane}/followup"), json!({ "text": "Reply with just the word PINEAPPLE." }));
    assert_eq!(r["delivered"], true, "{r}");
    until(&d, pane, "the follow-up's reply", 120, |s| s.contains("PINEAPPLE"));
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

/// M16: Claude Code outside Arugula uses it through `arugula mcp`.
#[test]
fn claude_code_outside_runs_a_build_through_mcp() {
    if !wanted("mcp-cc") {
        return;
    }
    let on_vm = std::env::var("ARUGULA_REAL_AGENTS").unwrap_or_default().split(',').any(|w| w.trim() == "mcp-vm");
    let wisp = home().join(".local/share/wisp/token");
    if on_vm && !wisp.exists() {
        eprintln!("SKIP: mcp-vm needs a wisp token (~/.local/share/wisp/token)");
        return;
    }
    let d = if on_vm { Daemon::child_with(&["--wisp-token-file", wisp.to_str().unwrap()]) } else { Daemon::child() };
    let cli = std::path::Path::new(env!("CARGO_BIN_EXE_arugulad")).with_file_name("arugula");
    assert!(
        std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "arugula"]).status().unwrap().success()
    );
    let repo = scratch(&d);
    let config = d.sessions.join("mcp.json");
    let server = json!({ "mcpServers": { "arugula": { "command": cli, "args": ["--socket", d.sock(), "mcp"] } } });
    std::fs::write(&config, server.to_string()).unwrap();
    // A build that takes a while and fails until a file exists.
    let marker = if on_vm { "/tmp/ready".to_owned() } else { format!("{repo}/ready") };
    let build = format!(
        "sleep 20; test -f {marker} && echo BUILD-OK || {{ echo 'error: {marker} is missing (touch it)'; false; }}"
    );
    let place =
        if on_vm { "with vm: true (a throwaway VM pane; run the fix in that same pane with send_input)" } else { "" };
    let prompt = format!(
        "Use the Arugula MCP tools. Run this build with the run tool {place}, with wait true: `{build}`. \
         If it fails, read why, fix it, and run the build again until it succeeds (wait again if it's still running). \
         Then reply with just the word DONE."
    );
    let mut cmd = std::process::Command::new("claude");
    for v in CLAUDE_ENV {
        cmd.env_remove(v);
    }
    let out = cmd
        .current_dir(&repo)
        .args(["-p", "--model", "haiku", "--strict-mcp-config", "--setting-sources", "local", "--mcp-config"])
        .arg(&config)
        .args(["--allowedTools", "mcp__arugula__*", "--output-format", "text"])
        .arg(&prompt)
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("DONE"), "{said}\n{}", String::from_utf8_lossy(&out.stderr));
    // History: the build failed, then (after the fix) passed, as Claude's.
    let h = d.get("/api/history?limit=50");
    let builds: Vec<&Value> =
        h.as_array().unwrap().iter().filter(|c| c["text"].as_str().is_some_and(|t| t.contains("BUILD-OK"))).collect();
    assert!(builds.len() >= 2, "{h}");
    assert_ne!(builds[0]["exit"], 0, "{h}");
    assert_eq!(builds.last().unwrap()["exit"], 0, "{h}");
    assert!(builds.iter().all(|c| c["by"] == "mcp:claude-code"), "{h}");
    let panes = d.get("/api/panes");
    assert!(panes.as_array().unwrap().iter().any(|p| p["started_by"]["by"] == "mcp:claude-code"), "{panes}");
    if on_vm {
        assert!(!d.get("/api/machines").as_array().unwrap().is_empty(), "on a VM");
    }
}

/// #145, #146, #147 against the real Claude Code: no hooks, so everything
/// it knows comes from the screen and Claude Code's own session files.
#[test]
fn claude_code_by_its_screen_prompted_and_resumed() {
    if !wanted("screen") {
        return;
    }
    let scratch = Scratch::new("real-screen");
    let repo = scratch.join("repo");
    std::process::Command::new("git").arg("init").arg("-q").arg(&repo).status().unwrap();
    // With an API key (CI), a Claude Code config of its own.
    let config = scratch.join("claude");
    let mut env: Vec<(&str, String)> = vec![];
    if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
        env.push(("CLAUDE_CONFIG_DIR", config.display().to_string()));
    }
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut d = Daemon::child_env(&[], &env);
    let unset: String = CLAUDE_ENV.iter().map(|v| format!("-u {v} ")).collect();
    let cmd = format!("cd {} && env {unset}claude --model haiku --setting-sources local", repo.display());
    d.post("/api/panes/1/send", json!({ "text": cmd, "enter": true }));
    let s = until(&d, 1, "its prompt or the trust dialog", 60, |s| {
        s.contains("? for shortcuts") || s.contains("trust this folder")
    });
    if s.contains("trust this folder") {
        // Read off the screen as a question for you.
        d.wait_for("needs input", || d.get("/api/panes")[0]["attention"] == "needs_input");
        d.post("/api/panes/1/keys", json!({ "keys": ["Down", "Enter"] }));
        until(&d, 1, "its prompt", 60, |s| s.contains("? for shortcuts"));
    }
    let prompt = |text: &str, answering: bool| {
        d.post("/api/panes/1/prompt", json!({ "text": text, "answering": answering, "timeout": 180 }))
    };
    let v = prompt("Run this shell command: touch made-by-claude.txt", false);
    assert_eq!(v["result"], "needs_input", "{v}\n{}", screen(&d, 1));
    assert!(v["question"].as_str().unwrap_or_default().contains("touch made-by-claude.txt"), "{v}");
    let v = prompt("", true);
    assert_eq!(v["result"], "done", "{v}\n{}", screen(&d, 1));
    assert!(repo.join("made-by-claude.txt").exists(), "{}", screen(&d, 1));
    let det = d.get("/api/panes/1/detection");
    assert_eq!(det["shown"], "idle", "{det}");

    // Its conversation, from its session file, resumed after a restart.
    d.wait_for("its conversation known", || d.get("/api/panes")[0]["resumes"].is_string());
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let layout: Value = serde_json::from_str(&std::fs::read_to_string(d.state.join("layout.json")).unwrap()).unwrap();
    let id = layout["panes"]["1"]["session"]["id"].as_str().unwrap().to_owned();
    d.stop();
    d.start();
    let resumed = || d.raw("GET", "/api/panes/1/process", None).1.contains(&id);
    d.wait_for("claude --resume <id>", resumed);
    until(&d, 1, "the conversation back", 60, |s| s.contains("made-by-claude.txt"));
}
