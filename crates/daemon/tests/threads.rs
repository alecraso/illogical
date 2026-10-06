//! M61: threads on panes and sessions, held by the daemon.
//!
//! Who reads and who posts follows the pane: watchers read, drivers post,
//! a private pane's thread is its owner's, and a "from now" share starts
//! when it was made. Each person's unread count comes in their state.
//! `@agent` in a pane's thread reaches the agent there as a follow-up, and
//! threads outlive a daemon restart. A mention reaches team members who
//! aren't connected, on a notification of its thread's own, and a role change
//! keeps what a "from now" share already reads. An owner's @ that names
//! someone who can't read the thread offers to invite them (#297): into
//! that thread, from that message, and no other thread.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

mod agentd;

use std::time::{Duration, Instant};

use agentd::Phone;
use futures_util::{SinkExt, StreamExt};
use illogical_e2e::{DeviceKeys, Kind, cert::Cert};
use illogical_testkit::{Scratch, illogicald};
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

/// Which `@` tokens landed, and what is said to the poster about the rest.
#[tokio::test(flavor = "multi_thread")]
async fn only_what_landed_is_marked_and_the_rest_is_told_to_the_poster_alone() {
    let d = daemon("threads-landed");
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    share(&d, session, WATCHER, "viewer", true);
    share(&d, session, DRIVER, "editor", true);
    let reasons = |r: &Value| -> Vec<(String, String)> {
        r["unreached"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| (u["token"].as_str().unwrap().to_owned(), u["why"].as_str().unwrap().to_owned()))
            .collect()
    };
    let pair = |t: &str, w: &str| (t.to_owned(), w.to_owned());
    let key = format!("/api/threads/pane-{pane}");

    // Only the resolved token lands; the unknown name is told to the poster.
    let r = d.post(&key, json!({ "text": "@driver and @notreal, mail a@example.com" }));
    assert_eq!(r["message"]["landed"], json!(["driver"]), "{r}");
    assert_eq!(reasons(&r), [pair("notreal", "nobody")]);

    // The owner driving a pane: @claude lands too.
    let r = d.post(&key, json!({ "text": "@Claude look, @agent" }));
    assert_eq!(r["message"]["landed"], json!(["claude", "agent"]), "{r}");
    assert_eq!(r["unreached"], json!([]));

    // Yourself, and nothing at all, say nothing and land nothing.
    let r = d.post(&key, json!({ "text": "@me hello" }));
    assert_eq!((r["message"]["landed"].clone(), r["unreached"].clone()), (Value::Null, json!([])), "{r}");

    // In a session thread an @agent has no pane to go to.
    let r = d.post(&format!("/api/threads/session-{session}"), json!({ "text": "@claude ping @driver" }));
    assert_eq!(r["message"]["landed"], json!(["driver"]), "{r}");
    assert_eq!(r["message"]["to_agent"], Value::Null);
    assert_eq!(reasons(&r), [pair("claude", "agent_needs_pane")]);

    // A guest editor isn't trusted to drive the owner's pane, so their
    // @agent is refused.
    let (s, v) = guest(&d, DRIVER, "POST", &key, Some(json!({ "text": "@agent run it, @notreal" })));
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["message"]["to_agent"], Value::Null, "{v}");
    assert_eq!(v["message"]["landed"], Value::Null, "{v}");
    assert_eq!(reasons(&v), [pair("agent", "may_not_drive"), pair("notreal", "nobody")]);

    // A name that exists but can't read this thread reads like one that
    // doesn't: same reason, nothing to tell them apart.
    let (mut ws, _) = connect_async(d.ws("/ws")).await.unwrap();
    let op = json!({ "type": "pane", "pane": pane, "op": { "op": "set_private", "on": true } });
    ws.send(Message::Text(op.to_string().into())).await.unwrap();
    d.wait_for("the pane to go private", || guest(&d, DRIVER, "GET", &key, None).0 == 404);
    let r = d.post(&key, json!({ "text": "@driver hi" }));
    let r2 = d.post(&key, json!({ "text": "@nosuchperson hi" }));
    assert_eq!(reasons(&r), [pair("driver", "nobody")], "{r}");
    assert_eq!(reasons(&r2), [pair("nosuchperson", "nobody")], "{r2}");
    assert_eq!(r["message"]["mentions"], r2["message"]["mentions"]);
    assert_eq!(r["message"]["landed"], r2["message"]["landed"]);

    // Only the poster's response carries it: nothing is kept in the thread.
    let got = d.get(&key);
    assert!(got["messages"].as_array().unwrap().iter().all(|m| m.get("unreached").is_none()), "{got}");
    let (_, theirs) = guest(&d, WATCHER, "GET", &format!("/api/threads/session-{session}"), None);
    assert!(!theirs.to_string().contains("unreached"), "{theirs}");
}

/// A role change (the share dialog's select) keeps what a "from now" share
/// has read; a share made again after being removed starts again.
#[test]
fn a_role_change_keeps_the_floor_and_a_fresh_share_moves_it() {
    let d = daemon("threads-floor");
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    let key = format!("/api/threads/pane-{pane}");
    let pause = || std::thread::sleep(Duration::from_millis(20));
    d.post(&key, json!({ "text": "before" }));
    pause();
    share(&d, session, LATE, "viewer", false);
    pause();
    d.post(&key, json!({ "text": "one" }));
    d.post(&key, json!({ "text": "two" }));
    assert_eq!(texts(&guest(&d, LATE, "GET", &key, None).1), ["one", "two"]);
    pause();

    // To an editor, as the dialog does it (with history, which keeps the
    // share "from now"): still everything since the share, and no more.
    share(&d, session, LATE, "editor", true);
    assert_eq!(texts(&guest(&d, LATE, "GET", &key, None).1), ["one", "two"]);

    // Removed, then shared again: it starts now.
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{LATE}"), "role": null }));
    pause();
    share(&d, session, LATE, "viewer", false);
    pause();
    d.post(&key, json!({ "text": "three" }));
    assert_eq!(texts(&guest(&d, LATE, "GET", &key, None).1), ["three"]);
}

const TEAM_BOB: &str = "account:bob";
const SHARED_CAROL: &str = "account:carol";

fn member(account: &str, role: &str) -> Value {
    json!({ "account": account, "root": "r", "role": role, "name": account })
}

fn roster(team: &str, members: Vec<Value>) -> Value {
    json!({ "v": 1, "team": team, "name": team, "version": 1, "at": 1, "members": members, "by": "", "sig": "" })
}

/// A mention reaches team members who aren't connected: a team daemon's
/// roster, and the members of a team the session was shared with. The push
/// is tagged for its thread, apart from the pane's own; it's still only for
/// those who can read the thread.
#[tokio::test]
async fn a_mention_reaches_team_members_offline_on_a_tag_of_its_own() {
    let dir = Scratch::new("threads-team");
    let (bob, carol) = (Phone::bind(), Phone::bind());
    let state = dir.join("state");
    // Their phones are subscribed before the daemon starts (nobody here
    // connects as an account).
    std::fs::create_dir_all(state.join("push")).unwrap();
    let subs: Vec<Value> = [(&bob, TEAM_BOB), (&carol, SHARED_CAROL)]
        .iter()
        .map(|(p, who)| {
            let mut s = p.subscription();
            s["who"] = json!(who);
            s
        })
        .collect();
    std::fs::write(state.join("push/subscriptions.json"), serde_json::to_vec(&subs).unwrap()).unwrap();
    let d = illogicald!("threads-team")
        .state_dir(&state)
        .no_wisp()
        .args(["--owner", OWNER, "--tailscale-socket", "/nonexistent/sock", "--no-relay"])
        .start();
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    let key = format!("pane-{pane}");

    // The session is shared with a team, then this daemon joins its own: a
    // roster of bob (editor), and the team's member carol (viewer).
    d.post("/api/acl", json!({ "session": session, "principal": "team:t9", "role": "viewer", "root": "f.r" }));
    let keys = DeviceKeys::generate();
    let mut cert = Cert::new(&keys, "owner-acct", Kind::Daemon, "x");
    cert.sign_with(&keys);
    let saved = json!({
        "url": "http://127.0.0.1:1",
        "trust": { "account": "owner-acct", "root": cert.device },
        "cert": cert,
        "roster": roster("t1", vec![member("bob", "editor")]),
        "shared_teams": { "t9": { "roster": roster("t9", vec![member("carol", "viewer")]) } },
    });
    keys.save(&state.join("daemon.key")).unwrap();
    std::fs::write(state.join("control.json"), serde_json::to_vec(&saved).unwrap()).unwrap();

    // control.json is picked up within a few seconds.
    let text = "@bob and @carol, look at this";
    let until = Instant::now() + Duration::from_secs(20);
    let mentioned = loop {
        let r = d.post(&format!("/api/threads/{key}"), json!({ "text": text }));
        if r["message"]["mentions"].as_array().is_some_and(|m| m.len() == 2) || Instant::now() > until {
            break r["message"]["mentions"].clone();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(mentioned, json!([TEAM_BOB, SHARED_CAROL]));
    for phone in [&bob, &carol] {
        let push = phone.next();
        assert_eq!(
            (push["tag"].as_str(), push["thread"].as_str()),
            (Some(format!("thread-{key}").as_str()), Some(key.as_str())),
            "{push}"
        );
        assert_eq!(push["pane"], pane);
    }

    // The pane's own notifications keep the pane's tag.
    let owner = Phone::bind();
    d.post("/api/push/subscribe", owner.subscription());
    d.post("/api/push/test", json!({}));
    assert_eq!(owner.next()["tag"], "pane-0");

    // Whoever can't read the thread isn't mentioned, listed or not: a
    // private pane's is its owner's.
    let (mut ws, _) = connect_async(d.ws("/ws")).await.unwrap();
    let op = json!({ "type": "pane", "pane": pane, "op": { "op": "set_private", "on": true } });
    ws.send(Message::Text(op.to_string().into())).await.unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let r = d.post(&format!("/api/threads/{key}"), json!({ "text": text }));
        if r["message"]["mentions"].as_array().is_none_or(|m| m.is_empty()) {
            break;
        }
        assert!(Instant::now() < until, "still mentioning them on a private pane: {r}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

const SAM: &str = "sam@example.com";
const KIM: &str = "kim@example.com";

/// A second session ("other"), with a pane: someone shared only that is
/// nameable here and reads none of the first.
fn other_session(d: &illogical_testkit::Daemon) -> u64 {
    let pane = d.post("/api/run", json!({ "session": "other" }))["pane"].as_u64().unwrap();
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).unwrap()["session"].as_u64().unwrap()
}

/// The owner's @ that names someone known but without access offers them;
/// nobody else's says a thing about who exists.
#[tokio::test(flavor = "multi_thread")]
async fn an_owners_at_name_offers_to_invite_whom_it_missed() {
    let d = daemon("threads-offer");
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    let other = other_session(&d);
    share(&d, other, SAM, "viewer", true);
    share(&d, session, DRIVER, "editor", true);
    share(&d, session, WATCHER, "viewer", true);
    let key = format!("/api/threads/pane-{pane}");

    let r = d.post(&key, json!({ "text": "@sam look" }));
    assert_eq!(r["invitable"], json!([{ "token": "sam", "who": format!("tailnet:{SAM}"), "name": SAM }]), "{r}");
    assert_eq!(r["message"]["mentions"], Value::Null, "{r}");
    assert_eq!(r["unreached"], json!([]), "the offer says it: {r}");

    // An editor's identical post: no offer, and nothing to tell Sam from a
    // name nobody has.
    let (s, v) = guest(&d, DRIVER, "POST", &key, Some(json!({ "text": "@sam look" })));
    let (s2, v2) = guest(&d, DRIVER, "POST", &key, Some(json!({ "text": "@nosuchperson look" })));
    assert_eq!((s, s2), (200, 200));
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&v), keys(&v2));
    assert!(v.get("invitable").is_none(), "{v}");
    assert_eq!(v["unreached"], json!([{ "token": "sam", "why": "nobody" }]));
    assert_eq!(v2["unreached"], json!([{ "token": "nosuchperson", "why": "nobody" }]));

    // An unknown name, and a reader: nothing to offer.
    let r = d.post(&key, json!({ "text": "@nosuchperson and @watcher" }));
    assert_eq!(r["invitable"], json!([]), "{r}");
    assert_eq!(r["message"]["mentions"], json!([format!("tailnet:{WATCHER}")]));
    assert_eq!(r["unreached"], json!([{ "token": "nosuchperson", "why": "nobody" }]));

    // A session thread offers too.
    let r = d.post(&format!("/api/threads/session-{session}"), json!({ "text": "@sam?" }));
    assert_eq!(r["invitable"][0]["who"], format!("tailnet:{SAM}"), "{r}");

    // Two people for one @: both are offered, for the owner to pick.
    share(&d, other, "sam@elsewhere.org", "viewer", true);
    let r = d.post(&key, json!({ "text": "@sam again" }));
    let who: Vec<&str> = r["invitable"].as_array().unwrap().iter().map(|o| o["who"].as_str().unwrap()).collect();
    assert_eq!(who, [format!("tailnet:{SAM}").as_str(), "tailnet:sam@elsewhere.org"], "{r}");

    // A private pane's thread is its owner's: no grant opens it.
    let (mut ws, _) = connect_async(d.ws("/ws")).await.unwrap();
    let op = json!({ "type": "pane", "pane": pane, "op": { "op": "set_private", "on": true } });
    ws.send(Message::Text(op.to_string().into())).await.unwrap();
    d.wait_for("the pane to go private", || guest(&d, DRIVER, "GET", &key, None).0 == 404);
    let r = d.post(&key, json!({ "text": "@sam look" }));
    assert_eq!(r["invitable"], json!([]), "{r}");
    assert_eq!(r["unreached"], json!([{ "token": "sam", "why": "nobody" }]));
}

/// One person known as a login and as an account is offered once: the
/// account, which control reaches.
#[test]
fn one_person_known_twice_is_offered_once_as_their_account() {
    let dir = Scratch::new("threads-offer-twice");
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let d = illogicald!("threads-offer-twice")
        .state_dir(&state)
        .no_wisp()
        .args(["--owner", OWNER, "--tailscale-socket", "/nonexistent/sock", "--no-relay"])
        .start();
    let pane = d.get("/api/panes")[0]["id"].as_u64().unwrap();
    let other = other_session(&d);
    d.post("/api/acl", json!({ "session": other, "principal": "tailnet:sam", "role": "viewer" }));
    // A team this machine checked (pinned, shared with nothing here): sam's
    // in it.
    let keys = DeviceKeys::generate();
    let mut cert = Cert::new(&keys, "owner-acct", Kind::Daemon, "x");
    cert.sign_with(&keys);
    let saved = json!({
        "url": "http://127.0.0.1:1",
        "trust": { "account": "owner-acct", "root": cert.device },
        "cert": cert,
        "shared_teams": { "t9": { "roster": roster("t9", vec![member("sam", "editor")]) } },
    });
    keys.save(&state.join("daemon.key")).unwrap();
    std::fs::write(state.join("control.json"), serde_json::to_vec(&saved).unwrap()).unwrap();
    let key = format!("/api/threads/pane-{pane}");
    let until = Instant::now() + Duration::from_secs(20);
    let r = loop {
        let r = d.post(&key, json!({ "text": "@sam look" }));
        if r["invitable"][0]["who"] == "account:sam" || Instant::now() > until {
            break r;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let offer = json!([{ "token": "sam", "who": "account:sam", "name": "sam", "merged": "tailnet:sam" }]);
    assert_eq!(r["invitable"], offer, "{r}");
}

/// What `who` reads of a thread, and its status.
fn reads(d: &illogical_testkit::Daemon, who: &str, thread: &str) -> (u16, Vec<String>) {
    let (s, v) = guest(d, who, "GET", &format!("/api/threads/{thread}"), None);
    (s, if s == 200 { texts(&v) } else { Vec::new() })
}

/// The invite from an offer (#297): Sam's one push opens the thread; there
/// he reads the message that named him and what follows, and in every
/// other thread nothing from before he was let in. "Whole thread" opens
/// that thread's history, and still no other's. A role change keeps it.
#[test]
fn an_invite_from_a_mention_opens_that_thread_from_that_message() {
    let d = daemon("threads-invite");
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    let beside = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    let other = other_session(&d);
    share(&d, other, SAM, "viewer", true);
    share(&d, other, KIM, "viewer", true);
    let (here, there, talk) = (format!("pane-{pane}"), format!("pane-{beside}"), format!("session-{session}"));
    let post = |t: &str, text: &str| d.post(&format!("/api/threads/{t}"), json!({ "text": text }));
    let pause = || std::thread::sleep(Duration::from_millis(20));
    post(&here, "before it");
    post(&there, "the other pane");
    post(&talk, "the session's talk");
    pause();
    let r = post(&here, "@sam look");
    assert_eq!(r["invitable"][0]["who"], format!("tailnet:{SAM}"), "{r}");
    let msg = r["message"]["id"].as_u64().unwrap();
    pause();
    post(&here, "after it");
    post(&there, "the other pane, later");
    assert_eq!(reads(&d, SAM, &here).0, 404, "Sam can't read it yet");

    // What the offer's button sends. Wrong places are refused first.
    let invite = |b: Value| {
        let (s, t) = d.raw("POST", "/api/invite", Some(b));
        (s, serde_json::from_str::<Value>(&t).unwrap_or(Value::String(t)))
    };
    let sam = format!("tailnet:{SAM}");
    for (thread, m, s) in [(&there, 3, session), (&format!("session-{other}"), 1, session), (&here, msg, other)] {
        let (status, v) = invite(json!({ "session": s, "who": sam, "thread": thread, "msg": m }));
        assert_eq!(status, 400, "{thread} {m} in {s}: {v}");
    }
    let (status, v) = invite(json!({ "session": session, "who": sam, "thread": here, "msg": 99 }));
    assert_eq!(status, 400, "{v}");
    assert_eq!(reads(&d, SAM, &here).0, 404, "nothing granted by any of that");

    let phone = Phone::bind();
    let (s, v) = guest(&d, SAM, "POST", "/api/push/subscribe", Some(phone.subscription()));
    assert_eq!(s, 200, "{v}");
    let (push, (status, r)) = std::thread::scope(|sc| {
        let got = sc.spawn(|| phone.next());
        let r = invite(json!({ "session": session, "who": sam, "thread": here, "msg": msg, "note": "@sam look" }));
        (got.join().unwrap(), r)
    });
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["delivery"], "sent", "{r}");
    assert_eq!(push["tag"], format!("invite-{}", r["invite"].as_str().unwrap()), "{push}");
    assert_eq!((push["thread"].as_str(), push["pane"].as_u64()), (Some(here.as_str()), Some(pane)), "{push}");
    assert!(phone.quiet(1500), "one push");

    // That message on; nothing older there, nothing from before elsewhere.
    let only = |who: &str, sees: &[&str]| {
        assert_eq!(reads(&d, who, &here), (200, sees.iter().map(|s| s.to_string()).collect()), "{who} in {here}");
        assert_eq!(reads(&d, who, &there), (200, vec![]), "{who} in {there}");
        assert_eq!(reads(&d, who, &talk), (200, vec![]), "{who} in {talk}");
    };
    only(SAM, &["@sam look", "after it"]);
    // What comes after the grant, in any thread, is theirs as ever.
    pause();
    post(&there, "news");
    assert_eq!(reads(&d, SAM, &there).1, ["news"]);

    // A role change keeps it: the share dialog's, and an invite's upgrade.
    share(&d, session, SAM, "editor", true);
    assert_eq!(reads(&d, SAM, &here).1, ["@sam look", "after it"]);
    d.post("/api/acl", json!({ "session": session, "principal": sam, "role": "viewer" }));
    let (status, r) = invite(json!({ "session": session, "who": sam, "role": "editor" }));
    assert_eq!((status, r["grant"]["granted"].as_bool()), (200, Some(true)), "{r}");
    assert_eq!(reads(&d, SAM, &here).1, ["@sam look", "after it"]);
    assert_eq!(reads(&d, SAM, &talk).1, Vec::<String>::new());

    // The whole thread: all of it, and still no other thread's past.
    let (status, r) = invite(
        json!({ "session": session, "who": format!("tailnet:{KIM}"), "thread": here, "msg": msg, "whole_thread": true }),
    );
    assert_eq!(status, 200, "{r}");
    assert_eq!(reads(&d, KIM, &here).1, ["before it", "@sam look", "after it"],);
    assert_eq!(reads(&d, KIM, &there).1, Vec::<String>::new());
    assert_eq!(reads(&d, KIM, &talk).1, Vec::<String>::new());
}
