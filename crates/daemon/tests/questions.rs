//! M6c: questions and forms from agents.
//!
//! Agent blocks, against the fake ACP server (`fake_acp.py`), which sends
//! AskUserQuestion, MCP forms, sign-in links and Codex's form exactly as S13
//! recorded the real adapters doing: the card's answer, skipping, Stop,
//! history and search, the push notification, and (under systemd) a
//! question that outlives a daemon restart.
//!
//! Claude Code in a terminal: `illogical ask` run in a pane with the hook
//! input S13 recorded from Claude Code 2.1.286, answered, skipped, left to
//! the terminal, withdrawn by SIGTERM, and (under systemd) across a daemon
//! restart.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

mod agentd;

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use agentd::*;
use serde_json::{Value, json};

fn asks(s: &Value) -> Vec<Value> {
    s["asks"].as_array().cloned().unwrap_or_default()
}

fn last_msg(s: &Value) -> String {
    entries(s).iter().rev().find(|e| e["type"] == "agent").map(|e| e["text"].as_str().unwrap_or("").to_owned()).unwrap()
}

fn notes(s: &Value) -> Vec<String> {
    entries(s).iter().filter(|e| e["type"] == "note").filter_map(|e| e["text"].as_str().map(str::to_owned)).collect()
}

#[test]
fn an_agent_question_is_a_card_answered_by_its_fields() {
    let d = Daemon::child();
    let id = d.open("ask");
    // `wait --needs-input` says what it asks.
    let w = d.get(&format!("/api/panes/{id}/wait?until=needs-input&timeout=20"));
    assert_eq!(w["state"], "needs_input", "{w}");
    let ask = &w["ask"];
    assert_eq!((ask["kind"].as_str(), ask["source"].as_str()), (Some("questions"), Some("agent")), "{ask}");
    assert_eq!(ask["message"], "Please answer the following questions.");
    let qs = ask["questions"].as_array().unwrap();
    assert_eq!(qs.len(), 3);
    assert_eq!((qs[1]["header"].as_str(), qs[1]["multiSelect"].as_bool()), (Some("Fruit"), Some(true)));
    assert!(ask["tool_call_id"].as_str().unwrap().starts_with("toolu_"));
    let s = d.state(id);
    assert_eq!(asks(&s).len(), 1);
    assert_eq!(s["attention"], "needs_input");
    let panes = d.get("/api/panes");
    assert!(panes.as_array().unwrap().iter().any(|p| p["id"] == id && p["attention"] == "needs_input"));
    let text = d.raw("GET", &format!("/api/panes/{id}/capture"), None).1;
    assert!(text.contains("**Waiting for your answer:** Which colour do you prefer?"), "{text}");

    // A pick with a note, a multi-select plus "Other", and "Other" alone.
    let content = json!({ "question_0": "Red", "question_0_custom": "dark red please",
        "question_1": ["Apple", "Plum"], "question_1_custom": "kiwi", "question_2_custom": "a parrot" });
    d.call(id, "answer", json!({ "id": ask["id"], "content": content }));
    assert_eq!(d.wait(id, "idle"), "done");
    let s = d.state(id);
    assert!(asks(&s).is_empty());
    assert_eq!(
        last_msg(&s),
        "You answered: Which colour do you prefer? Red (dark red please); Which fruits do you like? Apple, Plum, kiwi; \
         Which pet do you prefer? a parrot"
    );
    // First answer wins.
    let (status, err) =
        d.raw("POST", &format!("/api/blocks/{id}/call/answer"), Some(json!({ "id": ask["id"], "content": {} })));
    assert_eq!(status, 400, "{err}");

    // The transcript has the question and the answer.
    let tool = last_tool(&s);
    assert_eq!((tool["name"].as_str(), tool["status"].as_str()), (Some("AskUserQuestion"), Some("completed")));
    let text = d.raw("GET", &format!("/api/panes/{id}/capture"), None).1;
    assert!(text.contains("**Asked** `Asking for your input` (completed)"), "{text}");
    assert!(text.contains("- Fruit: Which fruits do you like? (Apple / Pear / Plum, any of)"), "{text}");
    assert!(
        text.contains("_Answered: Which colour do you prefer? → Red (dark red please); Which fruits do you like? → Apple, Plum, kiwi; Which pet do you prefer? → a parrot_"),
        "{text}"
    );
    assert!(text.contains("The user answered: "), "{text}");
    // History and search.
    let h = d.get(&format!("/api/history?pane={id}"));
    let texts: Vec<&str> = h.as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect();
    assert!(
        texts.iter().any(|t| t.starts_with("Which colour do you prefer? → Which colour do you prefer? → Red")),
        "{h}"
    );
    let hits = d.get("/api/search?re=dark%20red%20please");
    assert!(hits.as_array().unwrap().iter().any(|h| h["pane"] == id), "{hits}");

    // The answer as the client sent it, in the log.
    let log = std::fs::read_dir(d.state.join("blocks").join(id.to_string())).unwrap();
    let mut found = false;
    for f in log.flatten() {
        let bytes = std::fs::read(f.path()).unwrap_or_default();
        found |= String::from_utf8_lossy(&bytes).contains(r#""content":{"question_0":"Red""#);
    }
    assert!(found, "the answer is in the block's log");

    // `call %N answer '{"question_0": …}'`: the fields alone, the open one.
    d.call(id, "send", json!({ "text": "ask one" }));
    d.wait(id, "needs-input");
    d.call(id, "answer", json!({ "question_0": "Blue" }));
    d.wait(id, "idle");
    assert_eq!(last_msg(&d.state(id)), "You answered: Which colour do you prefer? Blue");
}

#[test]
fn skip_declines_and_stop_ends_the_turn_at_once() {
    let d = Daemon::child();
    let id = d.open("ask one");
    let ask = d.get(&format!("/api/panes/{id}/wait?until=needs-input&timeout=20"))["ask"].clone();
    assert_eq!(ask["message"], "Which colour do you prefer?", "one question: the message is it");
    d.call(id, "decline", json!({ "id": ask["id"] }));
    assert_eq!(d.wait(id, "idle"), "done");
    let s = d.state(id);
    assert_eq!(last_msg(&s), "You didn't answer the question.");
    assert!(notes(&s).contains(&"Skipped: Which colour do you prefer?".to_owned()), "{s}");
    let failed = d.get(&format!("/api/history?pane={id}&failed=1"));
    assert!(
        failed.as_array().unwrap().iter().any(|c| c["text"] == "Which colour do you prefer? → skipped"),
        "{failed}"
    );

    // Stop with a question open: session/cancel alone; the agent withdraws
    // its request and the turn ends.
    d.call(id, "send", json!({ "text": "ask" }));
    d.wait(id, "needs-input");
    let t = Instant::now();
    d.call(id, "cancel", json!({}));
    d.wait_for("the turn to end", || d.state(id)["status"] == "ready");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert_eq!(d.wait(id, "idle"), "idle");
    let s = d.state(id);
    assert_eq!(s["last_stop"], "cancelled");
    assert!(asks(&s).is_empty());
    assert!(notes(&s).contains(&"The question was withdrawn".to_owned()), "{s}");
    // Nothing answered it: the block never sends `cancel` for a question.
    let log = std::fs::read_dir(d.state.join("blocks").join(id.to_string())).unwrap();
    for f in log.flatten() {
        let bytes = std::fs::read(f.path()).unwrap_or_default();
        assert!(!String::from_utf8_lossy(&bytes).contains(r#""action":"cancel""#));
    }
    // And it carries on.
    d.call(id, "send", json!({ "text": "hello" }));
    assert_eq!(d.wait(id, "idle"), "done");
}

#[test]
fn mcp_forms_sign_in_links_and_codex_forms() {
    let d = Daemon::child();
    // An MCP server's form: no tool call, the old enum + enumNames.
    let id = d.open("form");
    let ask = d.get(&format!("/api/panes/{id}/wait?until=needs-input&timeout=20"))["ask"].clone();
    assert_eq!((ask["kind"].as_str(), ask["message"].as_str()), (Some("form"), Some("Order details")), "{ask}");
    assert!(ask["tool_call_id"].is_null());
    assert_eq!(ask["schema"]["properties"]["size"]["enumNames"], json!(["Small", "Medium", "Large"]));
    assert_eq!(ask["schema"]["required"], json!(["size"]));
    d.call(id, "answer", json!({ "content": { "size": "M", "qty": 2, "gift": true } }));
    d.wait(id, "idle");
    let s = d.state(id);
    assert_eq!(last_msg(&s), r#"Order: {"action": "accept", "content": {"gift": true, "qty": 2, "size": "M"}}"#);
    assert!(notes(&s).contains(&"Answered: gift: true, qty: 2, size: M".to_owned()), "{s}");

    // A sign-in link: opening it accepts; the card stays until the agent
    // says it's complete.
    d.call(id, "send", json!({ "text": "signin" }));
    let ask = d.get(&format!("/api/panes/{id}/wait?until=needs-input&timeout=20"))["ask"].clone();
    assert_eq!((ask["kind"].as_str(), ask["url"].as_str()), (Some("url"), Some("https://example.com/fake-signin")));
    d.call(id, "answer", json!({ "id": ask["id"] }));
    let s = d.state(id);
    assert!(asks(&s).is_empty() || asks(&s)[0]["accepted"] == true, "{s}");
    d.wait(id, "idle");
    let s = d.state(id);
    assert!(asks(&s).is_empty(), "elicitation/complete closed it: {s}");
    assert_eq!(last_msg(&s), "Signed in.");
    // Dismissing one declines it.
    d.call(id, "send", json!({ "text": "signin" }));
    d.wait(id, "needs-input");
    d.call(id, "decline", json!({}));
    d.wait(id, "idle");
    assert_eq!(last_msg(&d.state(id)), "Not signed in.");

    // Codex's plan-mode question is a generic form (no AskUserQuestion tool).
    d.call(id, "send", json!({ "text": "codex ask" }));
    let ask = d.get(&format!("/api/panes/{id}/wait?until=needs-input&timeout=20"))["ask"].clone();
    assert_eq!(ask["kind"], "form");
    assert!(ask["schema"]["properties"]["colour_note"].is_object());
    d.call(id, "answer", json!({ "content": { "colour": "Blue" } }));
    d.wait(id, "idle");
    assert_eq!(last_msg(&d.state(id)), r#"Codex got: {"action": "accept", "content": {"colour": "Blue"}}"#);
}

/// One question with two options can be answered from the notification:
/// it carries the options, and answering by its id works.
#[test]
fn a_two_option_question_is_pushed_with_its_answers() {
    let d = Daemon::child();
    let phone = Phone::subscribe(&d);
    let id = d.open("ask one");
    let msg = phone.needs_you();
    assert_eq!(msg["pane"], id);
    assert_eq!(msg["body"], "Which colour do you prefer?");
    assert_eq!(msg["ask"]["field"], "question_0");
    assert_eq!(msg["ask"]["options"], json!(["Red", "Blue"]));
    assert!(msg.get("approve").is_none());
    // As the service worker does.
    let field = msg["ask"]["field"].as_str().unwrap();
    d.call(id, "answer", json!({ "id": msg["ask"]["id"], "content": { field: "Blue" } }));
    d.wait(id, "idle");
    assert_eq!(last_msg(&d.state(id)), "You answered: Which colour do you prefer? Blue");

    // Three questions only open the block.
    d.call(id, "send", json!({ "text": "ask" }));
    let msg = phone.needs_you();
    assert_eq!(msg["body"], "Which colour do you prefer?");
    assert!(msg.get("ask").is_none(), "{msg}");
    d.call(id, "decline", json!({}));
    d.wait(id, "idle");
}

/// Under systemd: a daemon restart with a question open. The agent server
/// lives through it, the card comes back from the log, and answering the
/// new daemon finishes the turn.
#[test]
fn a_pending_question_survives_a_daemon_restart() {
    let Some(d) = Daemon::service() else { return };
    let id = d.open("ask two");
    d.wait(id, "needs-input");
    let pid = d.state(id)["pid"].as_u64().unwrap();
    let mut before = asks(&d.state(id))[0].clone();
    // (When it came: live it's when it was read, rebuilt when it was logged.)
    before.as_object_mut().unwrap().remove("at_ms");

    d.restart_service();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    let s = d.state(id);
    assert_eq!(s["pid"].as_u64(), Some(pid), "the same agent server");
    let mut after = asks(&s)[0].clone();
    after.as_object_mut().unwrap().remove("at_ms");
    assert_eq!((asks(&s).len(), after), (1, before.clone()), "the same card");
    assert_eq!(d.wait(id, "needs-input"), "needs_input");
    d.call(
        id,
        "answer",
        json!({ "id": before["id"], "content": { "question_0": ["Pear"], "question_1_custom": "a parrot" } }),
    );
    assert_eq!(d.wait(id, "idle"), "done");
    assert_eq!(
        last_msg(&d.state(id)),
        "You answered: Which fruits do you like? Pear; Which pet do you prefer? a parrot"
    );
    d.post(&format!("/api/panes/{id}/close"), json!({}));
}

/// A client that doesn't declare elicitation gets no questions (the
/// adapter's behaviour, which the fake copies): the block does declare it.
/// The ACP line with this method in a block's log. It can land a little
/// after the block reports idle, so look for a few seconds (#58).
fn logged(d: &Daemon, id: u64, method: &str) -> Option<String> {
    let needle = format!(r#""method":"{method}""#);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for f in std::fs::read_dir(d.state.join("blocks").join(id.to_string())).unwrap().flatten() {
            let text = String::from_utf8_lossy(&std::fs::read(f.path()).unwrap_or_default()).into_owned();
            if let Some(line) = text.lines().find(|l| l.contains(&needle)) {
                return Some(line.to_owned());
            }
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_block_declares_form_and_url_elicitation() {
    let d = Daemon::child();
    let id = d.open("hello");
    d.wait(id, "idle");
    let init = logged(&d, id, "initialize").expect("initialize in the log");
    let init: Value = serde_json::from_str(&init).unwrap();
    assert_eq!(init["m"]["params"]["clientCapabilities"]["elicitation"], json!({ "form": {}, "url": {} }));

    // MCP servers (`--mcp NAME=COMMAND`) go to the session.
    let config = json!({ "agent": "acp", "command": ["python3", fake()], "cwd": d.sessions, "prompt": "hello",
        "mcp_servers": ["forms=python3 /srv/forms.py --log 'a b'"] });
    let id = d.open_with(json!({ "type": "agent", "config": config }));
    d.wait(id, "idle");
    let new = logged(&d, id, "session/new").expect("session/new in the log");
    let new: Value = serde_json::from_str(&new).unwrap();
    assert_eq!(
        new["m"]["params"]["mcpServers"][0],
        json!({ "name": "forms", "command": "python3", "args": ["/srv/forms.py", "--log", "a b"], "env": [] })
    );
    // And illogical's own (M16), its token kept out of the log.
    let ours = &new["m"]["params"]["mcpServers"][1];
    assert_eq!((ours["name"].as_str(), ours["type"].as_str()), (Some("illogical"), Some("http")), "{new}");
    assert_eq!(ours["headers"], json!([{ "name": "Authorization", "value": "<redacted>" }]));
}

// ---------------------------------------------------------------- terminals

fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

fn fixture() -> String {
    format!("{}/tests/fixtures/s13-hook-ask.json", env!("CARGO_MANIFEST_DIR"))
}

/// Runs `illogical ask` in a new pane as Claude Code's hook would (the
/// recorded hook input on stdin); its output and pid go in `dir`.
fn hook_in_pane(d: &Daemon, dir: &Path) -> u64 {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let cmd = format!(
        "sh -c 'echo $$ > {dir}/pid; exec {bin} ask < {input} > {dir}/out 2> {dir}/err'; echo done > {dir}/exit",
        dir = dir.display(),
        bin = cli_bin().display(),
        input = fixture(),
    );
    d.post("/api/run", json!({ "command": cmd }))["pane"].as_u64().unwrap()
}

fn pane_info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn wait_file(d: &Daemon, path: &Path) -> String {
    d.wait_for(&path.display().to_string(), || path.exists());
    std::fs::read_to_string(path).unwrap()
}

/// Under systemd a pane starts through systemd-run, which would expand
/// `$VAR` and `$$` itself (the hook's `echo $$` came out as `$`). The shell
/// gets the command as written, and none of the service's own environment.
#[test]
fn a_command_reaches_its_shell_as_written_under_systemd() {
    let Some(d) = Daemon::service() else { return };
    let out = d.sessions.join("written");
    let cmd = format!("echo \"$$ ${{NOTIFY_SOCKET:-none}}\" '$HOME' > {}.tmp; mv {0}.tmp {0}", out.display());
    d.post("/api/run", json!({ "command": cmd }));
    let got = wait_file(&d, &out);
    let words: Vec<&str> = got.split_whitespace().collect();
    assert!(words[0].parse::<u32>().is_ok(), "{got:?}");
    assert_eq!(words[1..], ["none", "$HOME"], "{got:?}");
}

#[test]
fn claude_code_in_a_terminal_asks_through_its_hook() {
    let d = Daemon::child();
    let dir = d.sessions.join("hook1");
    let pane = hook_in_pane(&d, &dir);
    d.wait_for("the card", || pane_info(&d, pane)["ask"].is_object());
    let info = pane_info(&d, pane);
    assert_eq!(info["attention"], "needs_input");
    let ask = &info["ask"];
    assert_eq!((ask["kind"].as_str(), ask["source"].as_str()), (Some("questions"), Some("hook")));
    assert_eq!(ask["id"], "toolu_01VjPcYqkckCw26ZfpWqGFQf");
    assert_eq!(ask["questions"][2]["header"], "Pet");
    // `wait --needs-input` prints it.
    let w = d.get(&format!("/api/panes/{pane}/wait?until=needs-input&timeout=5"));
    assert_eq!(w["ask"]["id"], ask["id"]);

    let content = json!({ "question_0": "Red", "question_0_custom": "dark shade please",
        "question_1": ["Apple", "Pear"], "question_2_custom": "a parrot" });
    d.call(pane, "answer", json!({ "id": ask["id"], "content": content }));
    wait_file(&d, &dir.join("exit"));
    let out: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("out")).unwrap()).unwrap();
    let hook = &out["hookSpecificOutput"];
    assert_eq!(
        (hook["hookEventName"].as_str(), hook["permissionDecision"].as_str()),
        (Some("PreToolUse"), Some("allow"))
    );
    let input: Value = serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    assert_eq!(hook["updatedInput"]["questions"], input["tool_input"]["questions"]);
    assert_eq!(
        hook["updatedInput"]["answers"],
        json!({ "Which colour do you prefer?": "Red", "Which fruits do you like?": "Apple, Pear",
                "Which pet do you prefer?": "a parrot" })
    );
    assert_eq!(
        hook["updatedInput"]["annotations"],
        json!({ "Which colour do you prefer?": { "notes": "dark shade please" } })
    );
    let info = pane_info(&d, pane);
    assert!(info["ask"].is_null());
    assert_ne!(info["attention"], "needs_input");

    // "Answer in terminal": no output, so Claude Code shows its picker.
    let dir = d.sessions.join("hook2");
    let pane = hook_in_pane(&d, &dir);
    d.wait_for("the card", || pane_info(&d, pane)["ask"].is_object());
    d.call(pane, "terminal", json!({}));
    wait_file(&d, &dir.join("exit"));
    assert_eq!(std::fs::read_to_string(dir.join("out")).unwrap(), "");
    assert!(pane_info(&d, pane)["ask"].is_null());

    // Skip: the tool is denied with a reason.
    let dir = d.sessions.join("hook3");
    let pane = hook_in_pane(&d, &dir);
    d.wait_for("the card", || pane_info(&d, pane)["ask"].is_object());
    d.call(pane, "decline", json!({}));
    wait_file(&d, &dir.join("exit"));
    let out: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("out")).unwrap()).unwrap();
    assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");

    // Esc in the TUI: Claude Code sends the hook SIGTERM; the card goes.
    let dir = d.sessions.join("hook4");
    let pane = hook_in_pane(&d, &dir);
    d.wait_for("the card", || pane_info(&d, pane)["ask"].is_object());
    let pid: i32 = wait_file(&d, &dir.join("pid")).trim().parse().unwrap();
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::SIGTERM).unwrap();
    d.wait_for("the card to go", || pane_info(&d, pane)["ask"].is_null());
    wait_file(&d, &dir.join("exit"));
    assert_eq!(std::fs::read_to_string(dir.join("out")).unwrap(), "");
    assert_ne!(pane_info(&d, pane)["attention"], "needs_input");
    // Answering it now is too late.
    let (status, _) = d.raw("POST", &format!("/api/blocks/{pane}/call/answer"), Some(json!({ "question_0": "Red" })));
    assert_eq!(status, 400);
}

#[test]
fn outside_illogical_the_hook_does_nothing() {
    let out = Command::new(cli_bin())
        .arg("ask")
        .env_remove("ILLOGICAL_PANE")
        .env("ILLOGICAL_SOCK", "/nonexistent")
        .stdin(std::fs::File::open(fixture()).unwrap())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    // Not AskUserQuestion's input: nothing either, even in a pane.
    let mut child = Command::new(cli_bin())
        .arg("ask")
        .env("ILLOGICAL_PANE", "1")
        .env("ILLOGICAL_SOCK", "/nonexistent")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(br#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success() && out.stdout.is_empty());
}

/// Under systemd the pane, and the hook waiting in it, live through a
/// daemon restart: the hook asks the new daemon, and the card is back.
#[test]
fn a_terminal_question_survives_a_daemon_restart() {
    let Some(d) = Daemon::service() else { return };
    let dir = d.sessions.join("hook-restart");
    let pane = hook_in_pane(&d, &dir);
    d.wait_for("the card", || pane_info(&d, pane)["ask"].is_object());
    d.restart_service();
    d.wait_for("the card again", || pane_info(&d, pane)["ask"].is_object());
    d.call(pane, "answer", json!({ "question_0": "Blue" }));
    wait_file(&d, &dir.join("exit"));
    let out: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("out")).unwrap()).unwrap();
    assert_eq!(out["hookSpecificOutput"]["updatedInput"]["answers"], json!({ "Which colour do you prefer?": "Blue" }));
}
