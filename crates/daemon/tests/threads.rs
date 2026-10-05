//! M61: threads on panes and sessions, held by the daemon.
//!
//! Who reads and who posts follows the pane: watchers read, drivers post,
//! a private pane's thread is its owner's, and a "from now" share starts
//! when it was made. Each person's unread count comes in their state.
//! `@agent` in a pane's thread reaches the agent there as a follow-up, and
//! threads outlive a daemon restart.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use illogical_testkit::illogicald;
use serde_json::{Value, json};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const OWNER: &str = "me@example.com";
const WATCHER: &str = "watcher@example.com";
const DRIVER: &str = "driver@example.com";
const LATE: &str = "late@example.com";

fn daemon(tag: &str) -> illogical_testkit::Daemon {
    illogicald!(tag).no_wisp().args(["--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"]).start()
}

/// A request as a tailnet guest: status and JSON body.
fn guest(d: &illogical_testkit::Daemon, who: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let body = body.map(|b| b.to_string());
    let (status, _, text) =
        d.tcp(method, path, &[("tailscale-user-login", who), ("content-type", "application/json")], body.as_deref());
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

fn share(d: &illogical_testkit::Daemon, session: u64, who: &str, role: &str, history: bool) {
    d.post(
        "/api/acl",
        json!({ "session": session, "principal": format!("tailnet:{who}"), "role": role, "history": history }),
    );
}

fn texts(v: &Value) -> Vec<String> {
    v["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap().to_owned()).collect()
}

/// The `threads` a guest's client is sent when it connects.
async fn threads_seen(d: &illogical_testkit::Daemon, who: &str) -> Value {
    let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(format!(
        "ws://127.0.0.1:{}/ws",
        d.port
    ))
    .unwrap();
    req.headers_mut().insert("tailscale-user-login", who.parse().unwrap());
    let (mut ws, _) = connect_async(req).await.unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout(Duration::from_secs(5), ws.next()).await else {
            break;
        };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["type"] == "hello" {
            return v["state"]["threads"].clone();
        }
    }
    panic!("no hello for {who}");
}

fn unread(threads: &Value, key: &str, id: u64) -> Option<u64> {
    threads.as_array()?.iter().find(|t| t["target"][key] == id).map(|t| t["unread"].as_u64().unwrap_or(0))
}

#[tokio::test(flavor = "multi_thread")]
async fn watchers_read_drivers_post_and_each_has_their_unread() {
    let d = daemon("threads-roles");
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    share(&d, session, WATCHER, "viewer", true);
    share(&d, session, DRIVER, "editor", true);

    let r = d.post(&format!("/api/threads/pane-{pane}"), json!({ "text": "this build is flaky, @driver" }));
    assert_eq!(r["message"]["id"], 1);
    assert_eq!(r["message"]["mentions"], json!([format!("tailnet:{DRIVER}")]));
    d.post(&format!("/api/threads/session-{session}"), json!({ "text": "standup at 10" }));

    // The driver sees it's for them.
    let theirs = threads_seen(&d, DRIVER).await;
    let t = theirs.as_array().unwrap().iter().find(|t| t["target"]["pane"] == pane).cloned().unwrap();
    assert_eq!((t["unread"].as_u64(), t["mention"].as_bool()), (Some(1), Some(true)), "{theirs}");

    // A watcher reads both, and can't post.
    let (s, v) = guest(&d, WATCHER, "GET", &format!("/api/threads/pane-{pane}"), None);
    assert_eq!((s, texts(&v)), (200, vec!["this build is flaky, @driver".to_owned()]));
    let (s, _) = guest(&d, WATCHER, "GET", &format!("/api/threads/session-{session}"), None);
    assert_eq!(s, 200);
    let (s, v) = guest(&d, WATCHER, "POST", &format!("/api/threads/pane-{pane}"), Some(json!({ "text": "me too" })));
    assert_eq!(s, 403, "{v}");

    // A driver posts, quoting the pane.
    let (s, v) = guest(
        &d,
        DRIVER,
        "POST",
        &format!("/api/threads/pane-{pane}"),
        Some(json!({ "text": "it's the cache", "quote": { "pane": pane, "text": "error: stale cache" } })),
    );
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["message"]["name"], DRIVER);
    assert_eq!(v["message"]["quote"]["text"], "error: stale cache");

    // Each person's unread: the watcher has read neither; posting reads
    // the thread up to what you posted.
    let mine = d.get(&format!("/api/threads/pane-{pane}"));
    assert_eq!(texts(&mine).len(), 2);
    assert_eq!(unread(&threads_seen(&d, WATCHER).await, "pane", pane), Some(2));
    assert_eq!(unread(&threads_seen(&d, WATCHER).await, "session", session), Some(1));
    assert_eq!(unread(&threads_seen(&d, DRIVER).await, "pane", pane), Some(0));

    // Reading clears it.
    let (s, _) = guest(&d, WATCHER, "POST", &format!("/api/threads/pane-{pane}/read"), Some(json!({ "upto": 2 })));
    assert_eq!(s, 200);
    d.wait_for("the read to land", || {
        let (_, v) = guest(&d, WATCHER, "GET", &format!("/api/threads/pane-{pane}"), None);
        v["messages"].as_array().is_some_and(|m| m.len() == 2)
    });
    let until = Instant::now() + Duration::from_secs(5);
    while unread(&threads_seen(&d, WATCHER).await, "pane", pane) != Some(0) {
        assert!(Instant::now() < until, "the watcher's unread never cleared");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Someone with no share sees nothing.
    let (s, _) = guest(&d, "stranger@example.com", "GET", &format!("/api/threads/pane-{pane}"), None);
    assert_ne!(s, 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_panes_thread_is_its_owners_and_from_now_starts_now() {
    let d = daemon("threads-private");
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    d.post(&format!("/api/threads/pane-{pane}"), json!({ "text": "before the share" }));
    share(&d, session, DRIVER, "editor", true);
    share(&d, session, LATE, "editor", false);
    d.post(&format!("/api/threads/pane-{pane}"), json!({ "text": "after the share" }));

    // "From now" sees only what came after it was made.
    let (_, v) = guest(&d, LATE, "GET", &format!("/api/threads/pane-{pane}"), None);
    assert_eq!(texts(&v), ["after the share"]);
    let (_, v) = guest(&d, DRIVER, "GET", &format!("/api/threads/pane-{pane}"), None);
    assert_eq!(texts(&v), ["before the share", "after the share"]);

    // Private: only the owner.
    let (mut ws, _) = connect_async(d.ws("/ws")).await.unwrap();
    let op = json!({ "type": "pane", "pane": pane, "op": { "op": "set_private", "on": true } });
    ws.send(Message::Text(op.to_string().into())).await.unwrap();
    d.wait_for("the pane to go private", || {
        guest(&d, DRIVER, "GET", &format!("/api/threads/pane-{pane}"), None).0 == 404
    });
    assert!(unread(&threads_seen(&d, DRIVER).await, "pane", pane).is_none());
    let (s, _) = guest(&d, DRIVER, "POST", &format!("/api/threads/pane-{pane}"), Some(json!({ "text": "hi" })));
    assert_eq!(s, 404);
    assert_eq!(texts(&d.get(&format!("/api/threads/pane-{pane}"))).len(), 2);
}

#[test]
fn threads_and_reads_outlive_a_restart() {
    let mut d = daemon("threads-restart");
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    share(&d, session, DRIVER, "editor", true);
    d.post(&format!("/api/threads/pane-{pane}"), json!({ "text": "one" }));
    d.post(&format!("/api/threads/session-{session}"), json!({ "text": "two" }));
    guest(&d, DRIVER, "POST", &format!("/api/threads/pane-{pane}/read"), Some(json!({ "upto": 1 })));
    // Reads are saved with the layout (debounced): give it time.
    std::thread::sleep(Duration::from_secs(3));
    d.stop();
    d.start();
    assert_eq!(texts(&d.get(&format!("/api/threads/pane-{pane}"))), ["one"]);
    assert_eq!(texts(&d.get(&format!("/api/threads/session-{session}"))), ["two"]);
    let r = d.post(&format!("/api/threads/pane-{pane}"), json!({ "text": "three" }));
    assert_eq!(r["message"]["id"], 2, "numbering carries on");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let seen = rt.block_on(threads_seen(&d, DRIVER));
    assert_eq!(unread(&seen, "pane", pane), Some(1), "the driver read 1 of 2: {seen}");
    // And search finds what was said.
    let hits = d.get("/api/search?re=three");
    assert!(hits.as_array().unwrap().iter().any(|h| h["thread"] == format!("pane-{pane}")), "{hits}");
}

#[test]
fn at_agent_reaches_the_panes_agent_from_someone_who_drives_it() {
    let d = daemon("threads-agent");
    let pane = d.get("/api/panes")[0]["id"].as_u64().unwrap();
    // Claude Code's Stop hook, waiting for a follow-up (`illogical inbox`).
    let sock = d.sock();
    let waiter = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut s = std::os::unix::net::UnixStream::connect(sock).unwrap();
        let body = std::fs::read_to_string(format!("{}/tests/fixtures/s18-hook-stop.json", env!("CARGO_MANIFEST_DIR")))
            .unwrap();
        let req = format!(
            "POST /api/panes/{pane}/inbox HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    });
    d.wait_for("the waiter", || {
        d.get("/api/panes").as_array().unwrap().iter().any(|p| p["id"] == pane && p["inbox"] == true)
    });
    let r = d.post(&format!("/api/threads/pane-{pane}"), json!({ "text": "@agent please rerun the tests" }));
    assert_eq!(r["message"]["to_agent"], true);
    assert_eq!(r["agent"]["delivered"], true, "{r}");
    let got = waiter.join().unwrap();
    assert!(got.contains("wrote in this pane's thread: @agent please rerun the tests"), "{got}");
    assert!(got.contains("post_thread"), "{got}");

    // Plain mentions don't.
    let r = d.post(&format!("/api/threads/pane-{pane}"), json!({ "text": "the agent is slow" }));
    assert_eq!(r["message"]["to_agent"], Value::Null);
}
