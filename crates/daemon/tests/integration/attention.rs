//! M24: attention reasons and actions.
//!
//! A failing test run, a long build, Claude Code's question (through its
//! hook, fed S13's recorded input) and an agent block's permission request
//! each show the right reason and headline in `illogical attention --json`;
//! acting on a list allows or dismisses several at once; a push carries the
//! reason's actions.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use agentd::*;
use serde_json::{Value, json};

fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

/// `illogical attention --json`.
fn attention(d: &Daemon) -> Vec<Value> {
    let out = Command::new(cli_bin()).arg("--socket").arg(d.sock()).args(["attention", "--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice::<Value>(&out.stdout).unwrap().as_array().cloned().unwrap()
}

fn reason_of(d: &Daemon, pane: u64) -> Option<Value> {
    attention(d).into_iter().find(|i| i["pane"] == pane).map(|i| i["reason"].clone())
}

/// A shell pane with `name` defined as a function that takes `secs` and
/// exits `code`, then runs `name ARGS`.
fn run_fake(d: &Daemon, def: &str, line: &str) -> u64 {
    let pane = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    d.wait_for("a prompt", || {
        d.get("/api/panes").as_array().unwrap().iter().any(|p| p["id"] == pane && p["running"] == true)
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    d.post(&format!("/api/panes/{pane}/send"), json!({ "text": def, "enter": true }));
    d.post(&format!("/api/panes/{pane}/send"), json!({ "text": line, "enter": true }));
    pane
}

#[test]
fn failures_and_long_builds_say_what_happened() {
    let d = Daemon::child();
    let events = follow_events(&d);
    let test = run_fake(&d, "cargo() { sleep 3.3; echo 'test result: FAILED'; return 101; }", "cargo test");
    let build = run_fake(&d, "make() { sleep 5.3; echo built; }", "make build");
    let quick = run_fake(&d, "oops() { return 2; }", "oops now");
    d.wait_for("the failure", || reason_of(&d, test).is_some());
    let r = reason_of(&d, test).unwrap();
    assert_eq!(r["kind"], "failed", "{r}");
    assert_eq!((r["command"].as_str(), r["exit"].as_i64()), (Some("cargo test"), Some(101)), "{r}");
    assert!(r["headline"].as_str().unwrap().starts_with("cargo test failed (exit 101) after 3s"), "{r}");
    assert_eq!(r["bundle"], "failed:here");
    assert!(r["duration_ms"].as_u64().unwrap() >= 3000);
    // M11: it can be typed again.
    assert_eq!(r["actions"], json!(["rerun", "dismiss"]));

    d.wait_for("the build", || reason_of(&d, build).is_some());
    let r = reason_of(&d, build).unwrap();
    assert_eq!(r["kind"], "done", "{r}");
    assert!(r["headline"].as_str().unwrap().starts_with("make build finished after 5s"), "{r}");
    assert!(r["bundle"].is_null());
    // A quick failure is something you were typing at anyway.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(reason_of(&d, quick).is_none());

    // The panes say so too, and the event stream.
    let info = d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == test).cloned().unwrap();
    assert_eq!(info["attention"], "done");
    assert_eq!(info["reason"]["kind"], "failed");
    let events = events.join().unwrap();
    assert!(events.contains(&format!("\"pane\":{test}")) && events.contains("\"kind\":\"failed\""), "{events}");

    // Dismissing both at once clears them for everyone.
    let r = d.post("/api/attention/act", json!({ "action": "dismiss", "panes": [test, build] }));
    assert_eq!(r["results"].as_array().unwrap().iter().filter(|x| x["ok"] == true).count(), 2, "{r}");
    assert!(attention(&d).is_empty());
}

#[test]
fn claude_codes_question_is_an_ask_answered_by_act() {
    let d = Daemon::child();
    let dir = d.sessions.join("hook");
    std::fs::create_dir_all(&dir).unwrap();
    let input = format!("{}/tests/fixtures/s13-hook-ask.json", env!("CARGO_MANIFEST_DIR"));
    let cmd = format!(
        "{bin} ask < {input} > {dir}/out; echo done > {dir}/exit",
        bin = cli_bin().display(),
        dir = dir.display()
    );
    let pane = d.post("/api/run", json!({ "command": cmd }))["pane"].as_u64().unwrap();
    d.wait_for("the ask", || reason_of(&d, pane).is_some_and(|r| r["kind"] == "ask"));
    let r = reason_of(&d, pane).unwrap();
    assert_eq!(r["headline"], "Which colour do you prefer?", "{r}");
    assert_eq!(r["ask"]["what"], "question");
    assert_eq!(r["ask"]["id"], "toolu_01VjPcYqkckCw26ZfpWqGFQf");
    assert_eq!(r["ask"]["agent"], "claude");
    assert!(r["bundle"].as_str().unwrap().starts_with("ask:") && r["bundle"].as_str().unwrap().ends_with(":claude"));
    assert_eq!(r["actions"], json!(["answer", "deny", "dismiss"]));
    // Allow means nothing for a question.
    let (status, body) = d.raw("POST", "/api/attention/act", Some(json!({ "action": "allow", "pane": pane })));
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("asks a question"), "{body}");
    let content = json!({ "question_0": "Blue", "question_1": ["Pear"], "question_2": "Cat" });
    let r = d.post("/api/attention/act", json!({ "action": "answer", "pane": pane, "content": content }));
    assert_eq!(r["results"][0]["ok"], true, "{r}");
    d.wait_for("the hook's answer", || dir.join("exit").exists());
    let out: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("out")).unwrap()).unwrap();
    assert_eq!(out["hookSpecificOutput"]["updatedInput"]["answers"]["Which colour do you prefer?"], "Blue");
    assert!(reason_of(&d, pane).is_none_or(|r| r["kind"] != "ask"));
}

#[test]
fn agents_asking_to_run_tools_are_allowed_together() {
    let d = Daemon::child();
    let phone = Phone::subscribe(&d);
    let a = d.open("run cargo test");
    let b = d.open("run cargo build");
    let c = d.open("run rm -rf target");
    for id in [a, b, c] {
        d.wait_for("the approval", || reason_of(&d, id).is_some_and(|r| r["kind"] == "ask"));
    }
    let r = reason_of(&d, a).unwrap();
    assert_eq!(r["ask"]["what"], "approve", "{r}");
    assert!(r["headline"].as_str().unwrap().contains("cargo test"), "{r}");
    assert_eq!(r["actions"], json!(["allow", "deny", "dismiss"]));
    // Agents in the same directory bundle together.
    assert_eq!(r["bundle"], reason_of(&d, b).unwrap()["bundle"]);
    // The push has the headline and the reason's actions.
    let push = phone.needs_you();
    assert_eq!(push["reason"]["kind"], "ask", "{push}");
    assert_eq!(push["reason"]["actions"], json!(["allow", "deny", "dismiss"]));
    assert!(push["approve"]["id"].is_string());

    // "Allow all": two at once; the third is denied with a message.
    let r = d.post("/api/attention/act", json!({ "action": "allow", "panes": [a, b] }));
    assert!(r["results"].as_array().unwrap().iter().all(|x| x["ok"] == true), "{r}");
    let r = d.post("/api/attention/act", json!({ "action": "deny", "pane": c, "message": "not that" }));
    assert_eq!(r["results"][0]["ok"], true, "{r}");
    for id in [a, b] {
        d.wait_for("it ran", || last_msg_is(&d, id, "Ran it."));
    }
    d.wait_for("it was refused", || last_msg_is(&d, c, "Not allowed."));
    // Answered: a second answer finds nothing.
    let (status, body) = d.raw("POST", "/api/attention/act", Some(json!({ "action": "allow", "pane": a })));
    assert_eq!(status, 409, "{body}");
}

/// `illogical events --follow --type attention` until a failure shows up.
fn follow_events(d: &Daemon) -> std::thread::JoinHandle<String> {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(d.sock()).unwrap();
    write!(s, "GET /api/events?follow=1&type=attention HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(30))).unwrap();
    std::thread::spawn(move || {
        let mut seen = String::new();
        let mut buf = [0u8; 4096];
        while !seen.contains("\"failed\"") {
            match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
        seen
    })
}

fn last_msg_is(d: &Daemon, id: u64, text: &str) -> bool {
    entries(&d.state(id)).iter().rev().find(|e| e["type"] == "agent").is_some_and(|e| e["text"] == text)
}

/// #234: an agent's invite card is pushed to the owner alone, opening at
/// the card and with no buttons to send it from; an editor who asked to
/// hear about everything hears nothing of it.
#[test]
fn an_invite_card_pushes_the_owner_only() {
    const FRIEND: &str = "friend@example.com";
    let args =
        ["--owner", "me@example.com", "--tailscale-socket", "/nonexistent/sock", "--wisp-token-file", "/nonexistent"];
    let d = Daemon::child_with(&args);
    let p = &d.get("/api/panes")[0];
    let (pane, session) = (p["id"].as_u64().unwrap(), p["session"].as_u64().unwrap());
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "editor" }));
    let owner = Phone::subscribe(&d);
    let friend = Phone::subscribe_as(&d, Some(FRIEND));
    let (status, _) = d.raw_as(FRIEND, "POST", "/api/notify", Some(json!({ "on": true })));
    assert_eq!(status, 200);

    let m = Mcp::bridge(&d, Some(pane));
    let r = m.call("invite_person", json!({ "who": "tailnet:sam@example.com", "note": "the flaky test" })).unwrap();
    let block = r["block"].as_u64().unwrap();
    let push = owner.needs_you();
    assert_eq!(push["pane"], block, "it opens at the card: {push}");
    assert!(push["body"].as_str().unwrap().contains("wants to bring sam@example.com"), "{push}");
    assert!(push.get("approve").is_none() && push.get("ask").is_none(), "nothing sends it from the push: {push}");
    assert!(friend.quiet(1500), "not to an editor, opted in or not");
}
