//! M6b: agent blocks, as ACP clients, against a scripted fake agent server
//! (`fake_acp.py`): turns, permissions (approve, deny, always, cancel),
//! cost, history, search, push, a reboot (the agent dies with the daemon
//! and the session comes back with `session/resume`), and, under a systemd
//! user manager, a restart with an approval pending that the agent server
//! lives through.
//!
//! Real adapters (Claude Code, Codex, Fountain) are in `agents_real.rs`,
//! which costs money and only runs when asked to.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

mod agentd;

use std::time::Duration;

use agentd::*;
use serde_json::{Value, json};
#[test]
fn an_agent_block_runs_turns_and_asks_before_it_acts() {
    let d = Daemon::child();
    let id = d.open("hello");
    assert_eq!(d.wait(id, "idle"), "done");
    let s = d.state(id);
    assert_eq!(s["status"], "ready");
    assert_eq!(s["server"]["name"], "fake-acp");
    assert!(entries(&s).iter().any(|e| e["type"] == "agent" && e["text"] == "Hello! I am fake."), "{s}");
    assert_eq!(s["cost"]["total"], 0.01);
    assert_eq!(s["cost"]["last_turn"], 0.01);
    let info = d.get(&format!("/api/blocks/{id}"))["info"].clone();
    assert_eq!(info["type"], "agent");

    // It asks before it runs something; approving runs it.
    d.call(id, "send", json!({ "text": "run touch x" }));
    assert_eq!(d.wait(id, "needs-input"), "needs_input");
    let s = d.state(id);
    let p = &s["pending"][0];
    assert_eq!(
        (p["title"].as_str(), p["tool"].as_str(), p["command"].as_str()),
        (Some("touch x"), Some("Bash"), Some("touch x"))
    );
    let summary = d.get("/api/panes");
    assert!(summary.as_array().unwrap().iter().any(|p| p["id"] == id && p["attention"] == "needs_input"), "{summary}");
    let (status, _) = d.raw(
        "POST",
        &format!("/api/blocks/{id}/call/approve"),
        Some(json!({ "id": p["id"], "option": "allow-with-updates" })),
    );
    assert_eq!(status, 400, "the agent's own allow_always is never picked");
    d.call(id, "approve", json!({ "id": p["id"] }));
    assert_eq!(d.wait(id, "idle"), "done");
    let s = d.state(id);
    let tool = last_tool(&s);
    assert_eq!((tool["status"].as_str(), tool["exit"].as_i64()), (Some("completed"), Some(0)), "{tool}");
    assert_eq!(tool["output"], "\u{1b}[32mran: touch x\u{1b}[0m\r\n", "command output, ANSI and all");
    assert!(s["pending"].as_array().unwrap().is_empty());
    assert!((s["cost"]["last_turn"].as_f64().unwrap() - 0.01).abs() < 1e-9, "per-turn delta of a cumulative cost");

    // Denying, with a reason.
    d.call(id, "send", json!({ "text": "run rm -rf y" }));
    d.wait(id, "needs-input");
    d.call(id, "deny", json!({ "reason": "not that" }));
    d.wait(id, "idle");
    let s = d.state(id);
    assert_eq!(last_tool(&s)["status"], "failed");
    assert!(entries(&s).iter().any(|e| e["text"] == "Denied rm -rf y: not that"), "{s}");

    // "Always": the block remembers it and answers next time itself.
    d.call(id, "send", json!({ "text": "run make" }));
    d.wait(id, "needs-input");
    d.call(id, "approve", json!({ "option": "always" }));
    d.wait(id, "idle");
    assert_eq!(d.state(id)["allow"], json!([{ "tool": "Bash", "title": "make" }]));
    d.call(id, "send", json!({ "text": "run make" }));
    assert_eq!(d.wait(id, "idle"), "done", "never asked");
    assert!(entries(&d.state(id)).iter().any(|e| e["text"] == "Allowed make (always allowed)"));
    d.wait_for("the rule saved in its config", || {
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(d.state.join("layout.json")).unwrap()).unwrap();
        saved["panes"][id.to_string()]["config"]["allow"] == json!([{ "tool": "Bash", "title": "make" }])
    });

    // Cancel mid-turn, and with a request open (answered `cancelled`).
    d.call(id, "send", json!({ "text": "slow" }));
    d.wait_for("streaming", || {
        entries(&d.state(id)).iter().any(|e| e["text"].as_str().is_some_and(|t| t.contains("tick 1")))
    });
    d.call(id, "cancel", json!({}));
    assert_eq!(d.wait(id, "idle"), "idle");
    assert_eq!(d.state(id)["last_stop"], "cancelled");
    d.call(id, "send", json!({ "text": "run sleep 100" }));
    d.wait(id, "needs-input");
    d.call(id, "cancel", json!({}));
    assert_eq!(d.wait(id, "idle"), "idle");
    let s = d.state(id);
    assert_eq!((s["last_stop"].as_str(), s["pending"].as_array().unwrap().len()), (Some("cancelled"), 0));

    // The transcript as Markdown; tail prints it too.
    let text = d.raw("GET", &format!("/api/panes/{id}/capture"), None).1;
    assert!(text.contains("## You\n\nrun touch x"), "{text}");
    assert!(text.contains("**Ran** `touch x` (completed, exit 0)\n\n```\nran: touch x\n```"), "{text}");
    assert_eq!(d.raw("GET", &format!("/api/panes/{id}/tail"), None).1, text);

    // History has its commands and turns; search finds what it said.
    let h = d.get(&format!("/api/history?pane={id}"));
    let texts: Vec<&str> = h.as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect();
    assert!(texts.contains(&"touch x") && texts.iter().any(|t| t.ends_with(": hello")), "{h}");
    let failed = d.get(&format!("/api/history?pane={id}&failed=1"));
    assert!(failed.as_array().unwrap().iter().any(|c| c["text"] == "rm -rf y"), "{failed}");
    let hits = d.get("/api/search?re=Hello!%20I%20am");
    assert!(hits.as_array().unwrap().iter().any(|h| h["pane"] == id), "{hits}");

    // Closing it ends the agent server.
    let pid = d.state(id)["pid"].as_u64().unwrap();
    assert!(alive(pid));
    d.post(&format!("/api/panes/{id}/close"), json!({}));
    d.wait_for("the agent server to go", || !alive(pid));
}

#[test]
fn a_block_starts_with_the_rules_and_mode_it_was_given() {
    // #163: a lead pre-authorizes its subagent's tools and mode.
    let d = Daemon::child();
    let config = json!({
        "agent": "acp", "command": ["python3", fake()], "cwd": d.sessions, "prompt": "mode",
        "allow": [{ "tool": "Bash" }], "permission_mode": "acceptEdits",
    });
    let id = d.open_with(json!({ "type": "agent", "config": config }));
    assert_eq!(d.wait(id, "idle"), "done");
    let s = d.state(id);
    assert!(entries(&s).iter().any(|e| e["text"] == "Mode: acceptEdits"), "{s}");
    assert_eq!((s["permission_mode"].as_str(), s["allow"].clone()), (Some("acceptEdits"), json!([{ "tool": "Bash" }])));
    d.call(id, "send", json!({ "text": "run cargo test" }));
    assert_eq!(d.wait(id, "idle"), "done", "never asked");
    assert!(entries(&d.state(id)).iter().any(|e| e["text"] == "Allowed cargo test (always allowed)"));

    // A mode the agent doesn't have is said, not swallowed.
    let config = json!({ "agent": "acp", "command": ["python3", fake()], "cwd": d.sessions, "prompt": "mode", "permission_mode": "yolo" });
    let id = d.open_with(json!({ "type": "agent", "config": config }));
    d.wait(id, "idle");
    let s = d.state(id);
    assert!(entries(&s).iter().any(|e| e["text"] == "Couldn't switch to permission mode yolo: Invalid Mode"), "{s}");
    assert!(entries(&s).iter().any(|e| e["text"] == "Mode: default"), "{s}");
}

#[test]
fn standing_rules_outlive_the_block_that_made_them() {
    // #166: "Always" for a directory or everywhere is the daemon's, not the
    // block's: the next block checks it, it survives a restart, and
    // forgetting it takes effect at once.
    let mut d = Daemon::child();
    let here = d.sessions.join("repo");
    let below = here.join("crates");
    let elsewhere = d.sessions.with_extension("elsewhere");
    for dir in [&below, &elsewhere] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let open_in = |d: &Daemon, cwd: &std::path::Path, prompt: &str| {
        let config = json!({ "agent": "acp", "command": ["python3", fake()], "cwd": cwd, "prompt": prompt });
        d.open_with(json!({ "type": "agent", "config": config }))
    };
    let asked = |d: &Daemon, id: u64, text: &str| -> bool {
        d.call(id, "send", json!({ "text": text }));
        let asked = d.wait(id, "idle") == "needs_input";
        if asked {
            d.call(id, "deny", json!({}));
            d.wait(id, "idle");
        }
        asked
    };

    let a = open_in(&d, &here, "run make");
    d.wait(a, "needs-input");
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{a}/call/approve"), Some(json!({ "option": "once", "scope": "cwd" })));
    assert_eq!(status, 400, "a scope goes with always: {body}");
    d.call(a, "approve", json!({ "option": "always", "scope": "cwd" }));
    d.wait(a, "idle");
    let here_s = here.display().to_string();
    assert!(
        entries(&d.state(a))
            .iter()
            .any(|e| e["text"] == format!("Allowed make, and from now on: Bash (any) in {here_s}"))
    );
    assert_eq!(d.state(a)["allow"], json!([]), "not the block's own rule");
    let rules = d.get("/api/rules");
    assert_eq!(rules["rules"][0]["cwd"], json!(here_s), "{rules}");
    assert_eq!(rules["rules"][0]["text"], json!(format!("Bash (any) in {here_s}")));

    // A new block under that directory never asks; one elsewhere does.
    let b = open_in(&d, &below, "run cargo build");
    assert_eq!(d.wait(b, "idle"), "done", "never asked");
    assert!(
        entries(&d.state(b))
            .iter()
            .any(|e| e["text"] == format!("Allowed cargo build (standing rule: Bash (any) in {here_s})")),
        "{}",
        d.state(b)
    );
    let c = open_in(&d, &elsewhere, "hello");
    d.wait(c, "idle");
    assert!(asked(&d, c, "run make"));

    // Everywhere, for a prefix: that command and its arguments, nothing else.
    d.call(c, "send", json!({ "text": "run cargo test" }));
    d.wait(c, "needs-input");
    d.call(c, "approve", json!({ "option": "always", "scope": "everywhere", "prefix": "cargo test" }));
    d.wait(c, "idle");
    assert!(!asked(&d, c, "run cargo test --workspace"));
    assert!(asked(&d, c, "run cargo testify"));
    assert!(asked(&d, c, "run cargo test; rm -rf x"));
    assert_eq!(d.get("/api/rules")["rules"][1]["text"], "Bash cargo test… everywhere");

    // In the daemon's state, not a block's: they survive a restart.
    assert!(std::fs::read_to_string(d.state.join("rules.json")).unwrap().contains("cargo test"));
    d.stop();
    d.start();
    let e = open_in(&d, &elsewhere, "run cargo test -p x");
    assert_eq!(d.wait(e, "idle"), "done", "never asked after a restart");

    // Forgetting one: the next request asks again.
    d.raw("DELETE", "/api/rules/1", None);
    assert_eq!(d.get("/api/rules")["rules"].as_array().unwrap().len(), 1);
    assert!(asked(&d, e, "run cargo test -p y"));
    assert_eq!(d.raw("DELETE", "/api/rules/7", None).0, 404);
    d.raw("DELETE", "/api/rules", None);
    let f = open_in(&d, &below, "hello");
    d.wait(f, "idle");
    assert!(asked(&d, f, "run make"));
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn after_a_reboot_the_transcript_is_back_and_the_session_resumes() {
    let mut d = Daemon::child();
    let id = d.open("remember kestrel");
    d.wait(id, "idle");
    let pid = d.state(id)["pid"].as_u64().unwrap();
    // Mid-turn, with an approval open: then the daemon (and, without
    // systemd, everything it started) goes away, as in a reboot.
    d.call(id, "send", json!({ "text": "run sleep 1" }));
    d.wait(id, "needs-input");
    d.stop();
    d.wait_for("the agent to die with it", || !alive(pid));

    d.start();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    d.wait_for("the session", || d.state(id)["status"] == "ready");
    let s = d.state(id);
    assert_ne!(s["pid"].as_u64(), Some(pid), "a new agent server");
    assert!(s["pending"].as_array().unwrap().is_empty(), "its request died with it");
    let text = d.raw("GET", &format!("/api/panes/{id}/capture"), None).1;
    assert!(text.contains("remember kestrel") && text.contains("Started the agent again"), "{text}");
    assert_eq!(text.matches("## You\n\nremember kestrel").count(), 1, "resumed, not replayed: {text}");
    d.call(id, "send", json!({ "text": "recall" }));
    d.wait(id, "idle");
    assert!(entries(&d.state(id)).iter().any(|e| e["text"] == "You said kestrel."), "the agent's context came back");

    // With policy none it waits for "Resume". (Clients set policies over
    // the WebSocket; here, in the saved layout while the daemon is down.)
    d.stop();
    let path = d.state.join("layout.json");
    let mut saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    saved["panes"][id.to_string()]["policy"] = json!({ "kind": "none" });
    std::fs::write(&path, saved.to_string()).unwrap();
    d.start();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    assert_eq!(d.state(id)["status"], "stopped");
    assert!(d.state(id)["pid"].is_null());
    d.call(id, "resume", json!({}));
    d.wait_for("the session", || d.state(id)["status"] == "ready");
    d.call(id, "send", json!({ "text": "recall" }));
    d.wait(id, "idle");
}

#[test]
fn an_agent_that_dies_says_so_and_starts_again_on_send() {
    let d = Daemon::child();
    let id = d.open("crash");
    assert_eq!(d.wait(id, "idle"), "needs_input");
    let s = d.state(id);
    assert_eq!(s["status"], "exited");
    assert!(s["error"].as_str().unwrap().contains("exited with code 3"), "{s}");
    d.call(id, "send", json!({ "text": "hello" }));
    assert_eq!(d.wait(id, "idle"), "done");
    assert_eq!(d.state(id)["status"], "ready");
    // Bad configs are refused up front.
    let (status, err) =
        d.raw("POST", "/api/blocks", Some(json!({ "type": "agent", "config": { "agent": "fountain" } })));
    assert_eq!(status, 400, "{err}");
    let (status, err) =
        d.raw("POST", "/api/blocks", Some(json!({ "type": "agent", "vm": true, "config": { "agent": "claude" } })));
    assert_eq!(status, 400);
    assert!(err.contains("VM"), "{err}");
}

/// Under systemd: a restart with an approval pending. The agent server
/// lives through it (its scope; its pipes in the FD store), and the
/// approval, answered to the new daemon, still works.
#[test]
fn a_restart_mid_turn_keeps_the_agent_and_its_pending_approval() {
    let Some(d) = Daemon::service() else { return };
    let id = d.open("hello");
    d.wait(id, "idle");
    let pid = d.state(id)["pid"].as_u64().unwrap();
    d.call(id, "send", json!({ "text": "run make deploy" }));
    d.wait(id, "needs-input");
    let before = d.state(id)["pending"][0].clone();

    d.restart_service();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    let s = d.state(id);
    assert_eq!(s["pid"].as_u64(), Some(pid), "the same agent server");
    assert_eq!(s["status"], "working", "the turn is still running");
    assert_eq!(s["pending"][0]["id"], before["id"], "the same request");
    assert_eq!(d.wait(id, "needs-input"), "needs_input");

    d.call(id, "approve", json!({ "id": before["id"] }));
    assert_eq!(d.wait(id, "idle"), "done", "the turn from before the restart ended");
    let s = d.state(id);
    assert_eq!(last_tool(&s)["output"], "\u{1b}[32mran: make deploy\u{1b}[0m\r\n");
    assert!(entries(&s).iter().any(|e| e["text"] == "Ran it."));
    // And it carries on.
    d.call(id, "send", json!({ "text": "hello" }));
    d.wait(id, "idle");
    assert_eq!(d.state(id)["turns"], 3);
    assert!(alive(pid));

    // A crash too.
    if let Some(unit) = d.unit() {
        assert!(systemctl(&["kill", "--kill-whom=main", "--signal=SIGKILL", unit]));
        std::thread::sleep(Duration::from_millis(300));
        d.wait_up();
    }
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    assert_eq!(d.state(id)["pid"].as_u64(), Some(pid));
    d.call(id, "send", json!({ "text": "recall" }));
    assert_eq!(d.wait(id, "idle"), "done");
    d.post(&format!("/api/panes/{id}/close"), json!({}));
    d.wait_for("the agent server to go", || !alive(pid));
}

/// A permission request reaches a subscribed phone as a push with what to
/// approve, and approving by its id (as the notification's action does)
/// works.
#[test]
fn a_permission_request_is_pushed_with_its_approval() {
    let d = Daemon::child();
    let phone = Phone::subscribe(&d);
    let id = d.open("run git push");
    let msg = phone.needs_you();
    assert_eq!(msg["pane"], id);
    assert_eq!(msg["body"], "wants to run git push");
    assert_eq!(msg["approve"]["title"], "git push");
    d.call(id, "approve", json!({ "id": msg["approve"]["id"] }));
    d.wait(id, "idle");
    assert_eq!(last_tool(&d.state(id))["status"], "completed");
}
