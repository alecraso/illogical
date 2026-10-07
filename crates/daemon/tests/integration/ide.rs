//! M28: arugulad as Claude Code's IDE, against a stand-in for Claude Code
//! (`fake_claude.py`, which does what S17 recorded the real one doing) run
//! in a pane. The daemon registers with no workspace folders and puts its
//! port in every pane; an edit becomes a diff card on that pane, and
//! accepting it (as proposed, or changed first) or rejecting it answers
//! Claude Code, which writes the file or doesn't. The terminal answering
//! first closes the card. A daemon restart keeps the connection and the
//! card. Diffs can go to another IDE instead.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use agentd::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const OWNER: &str = "me@example.com";
const FRIEND: &str = "friend@example.com";

fn fake_claude() -> String {
    format!("{}/tests/fake_claude.py", env!("CARGO_MANIFEST_DIR"))
}

struct Setup {
    d: Daemon,
    locks: PathBuf,
}

/// A daemon whose panes outlive it (for the restart), registering in a
/// lock directory of the test's own.
fn setup(tag: &str) -> Setup {
    let locks = std::env::temp_dir().join(format!("ilg-ide-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&locks);
    let d = Daemon::child_env(
        &[
            "--keep-panes",
            "--wisp-token-file",
            "/nonexistent",
            "--owner",
            OWNER,
            "--tailscale-socket",
            "/nonexistent/sock",
        ],
        &[("ARUGULA_CLAUDE_IDE_DIR", locks.to_str().unwrap()), ("ARUGULA_KEEP_GRACE_MS", "20000")],
    );
    Setup { d, locks }
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.locks);
    }
}

fn capture(d: &Daemon, pane: u64) -> String {
    d.raw("GET", &format!("/api/panes/{pane}/capture?format=text"), None).1
}

fn wait_text(d: &Daemon, pane: u64, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !capture(d, pane).contains(what) {
        assert!(Instant::now() < deadline, "no {what:?} in %{pane}:\n{}", capture(d, pane));
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or(Value::Null)
}

/// A pane running the stand-in Claude Code, connected.
fn claude(d: &Daemon) -> u64 {
    let pane = d.post("/api/run", json!({ "cwd": d.sessions.display().to_string() }))["pane"].as_u64().unwrap();
    d.wait_for("a prompt", || info(d, pane)["cwd"].is_string());
    d.post(&format!("/api/panes/{pane}/send"), json!({ "text": format!("python3 {}", fake_claude()), "enter": true }));
    wait_text(d, pane, "connected");
    pane
}

fn say(d: &Daemon, pane: u64, line: &str) {
    d.post(&format!("/api/panes/{pane}/send"), json!({ "text": line, "enter": true }));
}

fn lockfiles(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "lock")).collect())
        .unwrap_or_default()
}

#[test]
fn edits_wait_as_diffs_and_are_accepted_or_rejected_from_the_card() {
    let s = setup("diffs");
    let d = &s.d;
    // Registered as S17 says: no folders (only our panes pick it), 0600.
    let locks = lockfiles(&s.locks);
    assert_eq!(locks.len(), 1, "{locks:?}");
    let lock: Value = serde_json::from_slice(&std::fs::read(&locks[0]).unwrap()).unwrap();
    assert_eq!(lock["ideName"], "arugula");
    assert_eq!(lock["workspaceFolders"], json!([]));
    assert_eq!(lock["transport"], "ws");
    assert!(lock["authToken"].as_str().unwrap().len() >= 32);
    assert_eq!(std::fs::metadata(&locks[0]).unwrap().permissions().mode() & 0o777, 0o600);
    let port: u16 = locks[0].file_stem().unwrap().to_str().unwrap().parse().unwrap();
    assert_eq!(d.get("/api/ide")["port"], port);

    // Every pane knows the port.
    let pane = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    d.wait_for("a prompt", || info(d, pane)["cwd"].is_string());
    say(d, pane, "echo port=$CLAUDE_CODE_SSE_PORT.");
    wait_text(d, pane, &format!("port={port}."));

    let pane = claude(d);
    d.wait_for("Claude Code found in its pane", || info(d, pane)["claude_ide"] == true);
    let file = d.sessions.join("hello.py");
    std::fs::write(&file, "print('hi')\n").unwrap();
    // Lines from a followed editor, mentioned to it.
    d.post("/api/ide/mention", json!({ "pane": pane, "file": file.display().to_string(), "start": 3, "end": 5 }));
    wait_text(d, pane, "note at_mentioned hello.py 2-4");
    // An edit: a diff card on the pane.
    say(d, pane, &format!("edit {} print('hello')\\nprint('again')\\n", file.display()));
    d.wait_for("a diff", || info(d, pane)["diff"].is_object());
    let diff = info(d, pane)["diff"].clone();
    assert_eq!(diff["file"], "hello.py");
    assert_eq!((diff["added"].as_u64(), diff["removed"].as_u64()), (Some(2), Some(1)));
    assert!(diff["text"].as_str().unwrap().contains("-print('hi')\n+print('hello')\n+print('again')\n"), "{diff}");
    let reason = &info(d, pane)["reason"];
    assert_eq!(reason["kind"], "diff");
    assert_eq!(reason["headline"], "Claude Code wants to edit hello.py (+2 −1)");
    assert_eq!(reason["actions"], json!(["accept", "reject", "dismiss"]));
    let full = d.get(&format!("/api/panes/{pane}/diff"));
    assert_eq!(full["old"], "print('hi')\n");
    assert_eq!(full["new"], "print('hello')\nprint('again')\n");
    // Accepted from the rail: Claude Code writes it.
    d.post("/api/attention/act", json!({ "action": "accept", "pane": pane }));
    wait_text(d, pane, "result FILE_SAVED");
    // The fake prints the result before it writes the file.
    d.wait_for("the edit written", || {
        std::fs::read_to_string(&file).unwrap_or_default() == "print('hello')\nprint('again')\n"
    });
    d.wait_for("the card to go", || info(d, pane)["diff"].is_null() && info(d, pane)["reason"].is_null());
    assert_eq!(info(d, pane)["answered"]["how"], "accepted");

    // Changed before accepting: Claude Code takes the change.
    say(d, pane, &format!("edit {} x = 1\\n", file.display()));
    d.wait_for("a second diff", || info(d, pane)["diff"].is_object());
    d.post("/api/attention/act", json!({ "action": "accept", "pane": pane, "text": "x = 2\n" }));
    d.wait_for("the change written", || std::fs::read_to_string(&file).unwrap() == "x = 2\n");
    assert_eq!(info(d, pane)["answered"]["how"], "accepted with changes");

    // Rejected: nothing written.
    say(d, pane, &format!("edit {} gone\\n", file.display()));
    d.wait_for("a third diff", || info(d, pane)["diff"].is_object());
    let id = info(d, pane)["diff"]["id"].as_str().unwrap().to_owned();
    // A stale id is refused.
    let (status, _) =
        d.raw("POST", "/api/attention/act", Some(json!({ "action": "reject", "pane": pane, "id": "d9-9" })));
    assert_eq!(status, 409);
    d.post("/api/attention/act", json!({ "action": "reject", "pane": pane, "id": id }));
    wait_text(d, pane, "result DIFF_REJECTED");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "x = 2\n");
    d.wait_for("rejected", || info(d, pane)["answered"]["how"] == "rejected");

    // The terminal answered first: Claude Code closes the diff; so does
    // the card, saying so.
    let new = d.sessions.join("new.txt");
    say(d, pane, &format!("edit {} fresh\\n", new.display()));
    d.wait_for("a diff for a new file", || info(d, pane)["diff"]["new"] == true);
    assert_eq!(info(d, pane)["reason"]["headline"], "Claude Code wants to create new.txt (+1 −0)");
    say(d, pane, "close");
    wait_text(d, pane, "result TAB_CLOSED");
    d.wait_for("the card to close", || info(d, pane)["diff"].is_null());
    assert_eq!(info(d, pane)["answered"]["who"], "terminal");
    assert!(!new.exists());
}

#[test]
fn a_daemon_restart_keeps_claude_connected_and_the_card() {
    let mut s = setup("restart");
    let pane = claude(&s.d);
    let file = s.d.sessions.join("kept.txt");
    say(&s.d, pane, &format!("edit {} after the restart\\n", file.display()));
    s.d.wait_for("a diff", || info(&s.d, pane)["diff"].is_object());
    s.d.stop();
    s.d.start();
    // The relay kept the connection; the daemon hears the open call again.
    s.d.wait_for("the card again", || info(&s.d, pane)["diff"].is_object());
    s.d.post("/api/attention/act", json!({ "action": "accept", "pane": pane }));
    wait_text(&s.d, pane, "result FILE_SAVED");
    s.d.wait_for("the edit written", || std::fs::read_to_string(&file).unwrap_or_default() == "after the restart\n");
    assert!(!capture(&s.d, pane).contains("disconnected"));
}

#[tokio::test(flavor = "multi_thread")]
async fn browsers_and_strangers_are_refused() {
    let s = setup("refuse");
    let lock = lockfiles(&s.locks).pop().unwrap();
    let port = lock.file_stem().unwrap().to_str().unwrap().to_owned();
    let token: Value = serde_json::from_slice(&std::fs::read(&lock).unwrap()).unwrap();
    let token = token["authToken"].as_str().unwrap().to_owned();
    let try_ = |origin: Option<&str>, token: Option<&str>| {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://127.0.0.1:{port}").into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        if let Some(t) = token {
            req.headers_mut().insert("x-claude-code-ide-authorization", t.parse().unwrap());
        }
        req.headers_mut().insert("sec-websocket-protocol", "mcp".parse().unwrap());
        tokio_tungstenite::connect_async(req)
    };
    // A page in a browser, even with the token.
    let e = try_(Some("https://evil.example"), Some(&token)).await.unwrap_err().to_string();
    assert!(e.contains("403"), "{e}");
    let e = try_(None, None).await.unwrap_err().to_string();
    assert!(e.contains("401"), "{e}");
    let e = try_(None, Some("nope")).await.unwrap_err().to_string();
    assert!(e.contains("401"), "{e}");
    let (mut ws, res) = try_(None, Some(&token)).await.unwrap();
    assert_eq!(res.headers()["sec-websocket-protocol"], "mcp");
    ws.send(Message::Text(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string().into()))
        .await
        .unwrap();
    let Some(Ok(Message::Text(t))) = ws.next().await else { panic!("no answer") };
    let v: Value = serde_json::from_str(&t).unwrap();
    let names: Vec<&str> =
        v["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"openDiff") && names.contains(&"close_tab"), "{names:?}");
}

/// Another IDE with Claude Code's extension: it accepts every diff, with a
/// line of its own added.
#[allow(clippy::result_large_err)] // tungstenite's handshake callback
async fn other_ide(dir: &Path, name: &str) -> (u16, tokio::task::JoinHandle<()>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    std::fs::create_dir_all(dir).unwrap();
    let lock = json!({ "pid": std::process::id(), "workspaceFolders": ["/somewhere"], "ideName": name,
        "transport": "ws", "runningInWindows": false, "authToken": "other-token" });
    std::fs::write(dir.join(format!("{port}.lock")), lock.to_string()).unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = l.accept().await else { return };
            tokio::spawn(async move {
                let check =
                    |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                     mut res: tokio_tungstenite::tungstenite::handshake::server::Response| {
                        assert_eq!(req.headers()["x-claude-code-ide-authorization"], "other-token");
                        res.headers_mut().insert("sec-websocket-protocol", "mcp".parse().unwrap());
                        Ok(res)
                    };
                let mut ws = tokio_tungstenite::accept_hdr_async(tcp, check).await.unwrap();
                while let Some(Ok(Message::Text(t))) = ws.next().await {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    let result = match v["method"].as_str() {
                        Some("initialize") => {
                            json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": { "name": "x" } })
                        }
                        Some("tools/call") if v["params"]["name"] == "openDiff" => {
                            let text = format!(
                                "{}# from the other IDE\n",
                                v["params"]["arguments"]["new_file_contents"].as_str().unwrap()
                            );
                            json!({ "content": [{ "type": "text", "text": "FILE_SAVED" }, { "type": "text", "text": text }] })
                        }
                        Some(_) if v.get("id").is_some() => json!({ "content": [] }),
                        _ => continue,
                    };
                    let _ = ws
                        .send(Message::Text(
                            json!({ "jsonrpc": "2.0", "id": v["id"], "result": result }).to_string().into(),
                        ))
                        .await;
                }
            });
        }
    });
    (port, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn diffs_can_go_to_another_ide() {
    let s = setup("other");
    let (_port, task) = other_ide(&s.locks, "Visual Studio Code").await;
    let d = &s.d;
    let ide = d.get("/api/ide");
    assert_eq!(ide["diffs"], "arugula");
    let others = ide["others"].as_array().unwrap();
    assert_eq!(others.len(), 1, "{ide}");
    assert_eq!((others[0]["name"].as_str(), others[0]["alive"].as_bool()), (Some("Visual Studio Code"), Some(true)));
    assert!(others[0].get("token").is_none(), "its token stays in the daemon");
    let (status, body) = d.raw("PUT", "/api/ide", Some(json!({ "diffs": "Visual Studio Code" })));
    assert_eq!(status, 200, "{body}");
    assert_eq!(d.get("/api/ide")["diffs"], "Visual Studio Code");

    let pane = tokio::task::block_in_place(|| claude(d));
    let file = d.sessions.join("there.txt");
    tokio::task::block_in_place(|| {
        say(d, pane, &format!("edit {} from claude\\n", file.display()));
        wait_text(d, pane, "result FILE_SAVED");
        // The fake prints the result before it writes the file.
        d.wait_for("the edit written", || {
            std::fs::read_to_string(&file).unwrap_or_default() == "from claude\n# from the other IDE\n"
        });
    });
    assert!(info(d, pane)["diff"].is_null(), "no card here: it went to the other IDE");
    task.abort();
}

/// Watching a session isn't answering for it: a viewer sees the diff, and
/// can't accept it; an editor can (M12).
#[tokio::test(flavor = "multi_thread")]
async fn only_those_who_may_drive_answer_a_diff() {
    let s = setup("roles");
    let d = &s.d;
    let pane = tokio::task::block_in_place(|| claude(d));
    let file = d.sessions.join("shared.txt");
    std::fs::write(&file, "before\n").unwrap();
    tokio::task::block_in_place(|| {
        say(d, pane, &format!("edit {} after\\n", file.display()));
        d.wait_for("a diff", || info(d, pane)["diff"].is_object());
    });
    let session = info(d, pane)["session"].as_u64().unwrap();
    let http = reqwest::Client::new();
    let url = |p: &str| format!("http://127.0.0.1:{}{p}", d.port);
    let accept = || {
        http.post(url("/api/attention/act"))
            .header("tailscale-user-login", FRIEND)
            .json(&json!({ "action": "accept", "pane": pane }))
            .send()
    };
    // A stranger: nothing.
    assert!([403, 404].contains(&accept().await.unwrap().status().as_u16()));
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "viewer" }));
    // A viewer sees it...
    let r =
        http.get(url(&format!("/api/panes/{pane}/diff"))).header("tailscale-user-login", FRIEND).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["new"], "after\n");
    // ...and can't accept it.
    let r = accept().await.unwrap();
    assert_eq!(r.status(), 403, "{}", r.text().await.unwrap());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "before\n");
    // Someone who may drive the session can.
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "editor" }));
    assert_eq!(accept().await.unwrap().status(), 200);
    tokio::task::block_in_place(|| {
        wait_text(d, pane, "result FILE_SAVED");
        d.wait_for("the edit written", || std::fs::read_to_string(&file).unwrap_or_default() == "after\n");
    });
    let answered = &info(d, pane)["answered"];
    assert_eq!(
        (answered["how"].as_str(), answered["who"].as_str()),
        (Some("accepted"), Some(format!("tailnet:{FRIEND}").as_str()))
    );
    // Which IDE gets diffs is the owner's to say.
    let r = http
        .put(url("/api/ide"))
        .header("tailscale-user-login", FRIEND)
        .json(&json!({ "diffs": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}
