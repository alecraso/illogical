//! M63: huddles, voice calls on a session signaled through its daemon.
//!
//! Anyone with a role in the session may join (watchers too); others can't
//! see or join it. The daemon passes descriptions only between members,
//! and someone whose share is revoked leaves the huddle at once. A huddle
//! ends when its last member leaves.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::time::Duration;

use arugula_testkit::arugulad;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

const OWNER: &str = "me@example.com";
const WATCHER: &str = "watcher@example.com";
const STRANGER: &str = "stranger@example.com";

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn daemon(tag: &str) -> arugula_testkit::Daemon {
    arugulad!(tag).no_wisp().args(["--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"]).start()
}

struct Person {
    ws: Ws,
    client: u64,
    /// The last `calls` this client was sent.
    calls: Value,
}

impl Person {
    async fn connect(d: &arugula_testkit::Daemon, who: &str) -> Self {
        let mut req = format!("ws://127.0.0.1:{}/ws", d.port).into_client_request().unwrap();
        req.headers_mut().insert("tailscale-user-login", who.parse().unwrap());
        let (mut ws, _) = connect_async(req).await.unwrap();
        loop {
            let v = next(&mut ws).await;
            if v["type"] == "hello" {
                let calls = v["state"]["calls"].clone();
                return Person { ws, client: v["client"].as_u64().unwrap(), calls };
            }
        }
    }

    async fn send(&mut self, msg: Value) {
        self.ws.send(Message::Text(msg.to_string().into())).await.unwrap();
    }

    /// Messages until one `want` says yes to (keeping `calls` current).
    async fn until(&mut self, what: &str, mut want: impl FnMut(&Value, &Value) -> bool) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let v = tokio::time::timeout_at(deadline, next(&mut self.ws))
                .await
                .unwrap_or_else(|_| panic!("no {what}; calls are {}", self.calls));
            match v["type"].as_str() {
                Some("state") => self.calls = v["state"]["calls"].clone(),
                Some("delta") if v["delta"].get("calls").is_some() => self.calls = v["delta"]["calls"].clone(),
                _ => {}
            }
            if want(&v, &self.calls) {
                return v;
            }
        }
    }

    /// Wait until the huddle on `session` has these clients in it (none:
    /// there's no huddle).
    async fn members(&mut self, session: u64, clients: &[u64]) {
        let want: Vec<u64> = clients.to_vec();
        if members_of(&self.calls, session) == want {
            return;
        }
        self.until(&format!("members {want:?}"), |_, calls| members_of(calls, session) == want).await;
    }
}

fn members_of(calls: &Value, session: u64) -> Vec<u64> {
    calls
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["session"] == session)
        .flat_map(|c| c["members"].as_array().unwrap().iter().map(|m| m["client"].as_u64().unwrap()))
        .collect()
}

async fn next(ws: &mut Ws) -> Value {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => return serde_json::from_str(&t).unwrap(),
            Some(Ok(_)) => continue,
            other => panic!("connection ended: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn members_signal_each_other_and_revoked_guests_drop_out() {
    let d = daemon("calls");
    let session = d.get("/api/panes")[0]["session"].as_u64().unwrap();
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{WATCHER}"), "role": "viewer" }));
    // Both have another session, so losing this one leaves them connected.
    let other = session + 100;
    for who in [WATCHER, STRANGER] {
        d.post("/api/acl", json!({ "session": other, "principal": format!("tailnet:{who}"), "role": "editor" }));
    }

    let mut me = Person::connect(&d, OWNER).await;
    let mut watcher = Person::connect(&d, WATCHER).await;
    let mut stranger = Person::connect(&d, STRANGER).await;
    assert!(members_of(&me.calls, session).is_empty());

    // The owner starts a huddle; the watcher sees it and joins.
    me.send(json!({ "type": "call_join", "session": session })).await;
    me.members(session, &[me.client]).await;
    watcher.members(session, &[me.client]).await;
    let call = me.calls[0].clone();
    assert_eq!(call["members"][0]["who"], "owner");
    assert!(call["id"].as_str().unwrap().len() >= 16, "{call}");
    watcher.send(json!({ "type": "call_join", "session": session })).await;
    me.members(session, &[me.client, watcher.client]).await;

    // Descriptions go between members, untouched, saying who from. These
    // connections have no device key, so no certificate comes with them.
    let offer = json!({ "type": "offer", "sdp": "v=0\r\na=fingerprint:sha-256 AB:CD\r\n" });
    watcher.send(json!({ "type": "call_signal", "session": session, "to": me.client, "signal": offer })).await;
    let got = me.until("the offer", |v, _| v["type"] == "call_signal").await;
    assert_eq!(got["from"], watcher.client);
    assert_eq!(got["session"], session);
    assert_eq!(got["signal"], offer);
    assert!(got.get("cert").is_none(), "{got}");

    // Muting shows in everyone's list.
    watcher.send(json!({ "type": "call_mute", "session": session, "muted": true })).await;
    me.until("the mute", |_, calls| calls[0]["members"][1]["muted"] == true).await;

    // Someone without the session can't see the huddle, join it, or
    // signal into it, and members can't signal to them.
    stranger.send(json!({ "type": "call_join", "session": session })).await;
    let err = stranger.until("a refusal", |v, _| v["type"] == "error").await;
    assert_eq!(err["message"], "no such session");
    assert!(members_of(&stranger.calls, session).is_empty());
    stranger.send(json!({ "type": "call_signal", "session": session, "to": me.client, "signal": offer })).await;
    me.send(json!({ "type": "call_signal", "session": session, "to": stranger.client, "signal": offer })).await;
    me.send(json!({ "type": "ping", "id": 7 })).await;
    me.until("the pong", |v, _| {
        assert_ne!(v["type"], "call_signal", "a non-member's signal came through: {v}");
        v["type"] == "pong"
    })
    .await;

    // The watcher's share is revoked: they leave the huddle at once, and
    // the owner is told.
    let revoked = std::time::Instant::now();
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{WATCHER}"), "role": null }));
    me.members(session, &[me.client]).await;
    assert!(revoked.elapsed() < Duration::from_secs(1), "took {:?}", revoked.elapsed());

    // The last one out ends it.
    me.send(json!({ "type": "call_leave", "session": session })).await;
    me.members(session, &[]).await;
    me.send(json!({ "type": "call_join", "session": session })).await;
    me.members(session, &[me.client]).await;
    assert_ne!(me.calls[0]["id"], call["id"], "a new huddle gets a new id");

    // Disconnecting leaves too.
    let mut again = Person::connect(&d, OWNER).await;
    again.send(json!({ "type": "call_join", "session": session })).await;
    me.members(session, &[me.client, again.client]).await;
    drop(again);
    me.members(session, &[me.client]).await;
}
