//! M4c: read-only share links. A link shows one terminal pane live (its
//! snapshot, then its output) to a tailnet user who isn't the owner, and
//! refuses everything else: input, any message at all, other panes, the
//! API, tagged nodes, after it expires, after it's revoked.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use illogical_proto::{Frame, FrameKind};
use illogical_testkit::{Daemon, illogicald};
use serde_json::{Value, json};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest, protocol::frame::coding::CloseCode},
};

const PUBLIC: &str = "geek.example.ts.net";
const OWNER: &str = "me@example.com";
const FRIEND: (&str, &str) = ("tailscale-user-login", "friend@example.com");

fn start() -> Daemon {
    illogicald!("share")
        .no_tailscale()
        .args(["--public-host", PUBLIC, "--owner", OWNER])
        .env("PS1", "$ ")
        .wait_secs(10)
        .start()
}

trait Sharing {
    fn api(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value);
    fn http(&self, path: &str, headers: &[(&str, &str)]) -> (u16, String);
    fn send(&self, pane: u64, text: &str);
    fn capture(&self, pane: u64) -> String;
    fn share(&self, pane: u64, ttl: u64) -> Value;
}

impl Sharing for Daemon {
    /// The API over the socket (the owner's CLI): status and JSON.
    fn api(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, body) = self.raw(method, path, body);
        (status, serde_json::from_str(&body).unwrap_or(Value::Null))
    }

    /// One GET over TCP with these headers: status and body.
    fn http(&self, path: &str, headers: &[(&str, &str)]) -> (u16, String) {
        let (status, _, body) = self.tcp("GET", path, headers, None);
        (status, body)
    }

    fn send(&self, pane: u64, text: &str) {
        let (s, v) = self.api("POST", &format!("/api/panes/{pane}/send"), Some(json!({"text": text, "enter": true})));
        assert_eq!(s, 200, "{v}");
    }

    fn capture(&self, pane: u64) -> String {
        let mut s = UnixStream::connect(self.sock()).unwrap();
        write!(s, "GET /api/panes/{pane}/capture?scope=scrollback HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    fn share(&self, pane: u64, ttl: u64) -> Value {
        let (s, v) = self.api("POST", "/api/shares", Some(json!({"pane": pane, "ttl_secs": ttl})));
        assert_eq!(s, 200, "{v}");
        v
    }
}

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn viewer(d: &Daemon, token: &str) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = format!("ws://127.0.0.1:{}/share/{token}/ws", d.port).into_client_request().unwrap();
    req.headers_mut().insert("origin", format!("http://127.0.0.1:{}", d.port).parse().unwrap());
    connect_async(req).await.map(|(ws, _)| ws)
}

/// What a viewer gets until `want` shows up: its text, and every frame's
/// pane.
async fn watch_for(ws: &mut Ws, want: &str) -> (String, Vec<u32>) {
    let mut text = String::new();
    let mut panes = vec![];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !text.contains(want) {
        let m =
            tokio::time::timeout_at(deadline, ws.next()).await.unwrap_or_else(|_| panic!("no {want:?} in {text:?}"));
        match m {
            Some(Ok(Message::Binary(b))) => {
                let f = Frame::decode(&b).unwrap();
                assert!(matches!(f.kind, FrameKind::Output | FrameKind::Snapshot));
                panes.push(f.pane);
                text.push_str(&String::from_utf8_lossy(&f.data));
            }
            Some(Ok(Message::Text(t))) => {
                let v: Value = serde_json::from_str(&t).unwrap();
                assert!(v["type"] == "share" || v["type"] == "size", "a viewer gets no layout: {v}");
            }
            Some(Ok(_)) => {}
            other => panic!("viewer ended: {other:?}"),
        }
    }
    (text, panes)
}

/// The close code and reason the viewer was hung up with.
async fn closed(ws: &mut Ws) -> (CloseCode, String) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await.expect("not hung up") {
            Some(Ok(Message::Close(Some(f)))) => return (f.code, f.reason.to_string()),
            Some(Ok(_)) => continue,
            other => panic!("ended without a close frame: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_share_link_shows_one_pane_live_and_refuses_everything_else() {
    let d = std::sync::Arc::new(start());
    let dd = d.clone();
    let (pane, other, share, short) = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let pane = loop {
            let (_, v) = dd.api("GET", "/api/panes", None);
            if let Some(id) = v[0]["id"].as_u64() {
                break id;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(100));
        };
        dd.send(pane, "echo shared-$((6*7))");
        let (_, run) = dd.api("POST", "/api/run", Some(json!({"command": "bash --norc --noprofile", "session": null})));
        let other = run["pane"].as_u64().unwrap();
        // Refusals at minting: no such pane, not a terminal.
        assert_eq!(dd.api("POST", "/api/shares", Some(json!({"pane": 999}))).0, 404);
        let (_, b) =
            dd.api("POST", "/api/blocks", Some(json!({"type": "browser", "config": {"url": "https://example.com"}})));
        let block = b["block"].as_u64().unwrap();
        assert_eq!(dd.api("POST", "/api/shares", Some(json!({"pane": block}))).0, 400);
        (pane, other, dd.share(pane, 3600), dd.share(pane, 1))
    })
    .await
    .unwrap();
    let token = share["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("ils_"));
    assert_eq!(share["url"], format!("https://{PUBLIC}/share/{token}"), "on the tailnet name");

    // A tailnet user who isn't the owner opens the page and its files, and
    // nothing else; tagged nodes and the internet don't even get that.
    let (dd, t) = (d.clone(), token.clone());
    tokio::task::spawn_blocking(move || {
        let friend = [("host", PUBLIC), FRIEND];
        let (s, body) = dd.http(&format!("/share/{t}"), &friend);
        assert_eq!(s, 200, "{body}");
        assert!(body.contains("share") && !body.contains("expired"), "{body}");
        assert_eq!(dd.http("/share/ils_wrong", &friend).0, 404);
        assert_eq!(dd.http("/assets/nothing-here.js", &friend).0, 404, "past the guard, just not there");
        for path in [
            "/api/panes",
            "/api/shares",
            "/ws",
            "/",
            "/api/hosts",
            "/h/x/api/panes",
            &format!("/share/{t}/../api/panes"),
        ] {
            assert_eq!(dd.http(path, &friend).0, 403, "{path}");
        }
        let tagged = [("host", PUBLIC)];
        assert_eq!(dd.http(&format!("/share/{t}"), &tagged).0, 403, "a tagged node (or Funnel)");
        assert_eq!(dd.http(&format!("/share/{t}"), &[("host", "evil.example"), FRIEND]).0, 421);
        // The list never shows tokens.
        let (_, list) = dd.api("GET", "/api/shares", None);
        assert_eq!(list.as_array().unwrap().len(), 2);
        assert!(list[0].get("token").is_none() && list[0].get("url").is_none());
    })
    .await
    .unwrap();

    // The viewer: the snapshot, then live output, of that pane only.
    let mut ws = viewer(&d, &token).await.expect("a viewer connects");
    let first = loop {
        if let Some(Ok(Message::Text(t))) = ws.next().await {
            break serde_json::from_str::<Value>(&t).unwrap();
        }
    };
    assert_eq!((first["type"].as_str(), first["pane"].as_u64()), (Some("share"), Some(pane)));
    watch_for(&mut ws, "shared-42").await;
    let dd = d.clone();
    tokio::task::spawn_blocking(move || {
        dd.send(pane, "echo live-$((3*3))");
        dd.send(other, "echo elsewhere-$((5*5))");
    })
    .await
    .unwrap();
    let (_, panes) = watch_for(&mut ws, "live-9").await;
    assert!(panes.iter().all(|p| u64::from(*p) == pane), "only the shared pane: {panes:?}");

    // Input is refused: the viewer is hung up on, and the pane got nothing.
    let input =
        Frame { kind: FrameKind::Input, pane: pane as u32, offset: 0, data: b"echo typed-by-viewer\r".to_vec() };
    ws.send(Message::Binary(input.encode().into())).await.unwrap();
    assert_eq!(closed(&mut ws).await, (CloseCode::Policy, "read-only".into()));
    // So are control messages: attaching another pane, claiming a size.
    for msg in [
        json!({"type": "attach", "panes": [{"pane": other, "offset": null}]}),
        json!({"type": "view", "tab": 1, "cols": 20, "rows": 5, "zoom": null, "claim": true}),
        json!({"type": "intent", "id": 1, "intent": {"op": "close_pane", "pane": pane}}),
    ] {
        let mut ws = viewer(&d, &token).await.unwrap();
        ws.send(Message::Text(msg.to_string().into())).await.unwrap();
        assert_eq!(closed(&mut ws).await.0, CloseCode::Policy, "{msg}");
    }
    let dd = d.clone();
    tokio::task::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(500));
        assert!(!dd.capture(pane).contains("typed-by-viewer"));
        let (_, panes) = dd.api("GET", "/api/panes", None);
        assert!(panes.as_array().unwrap().iter().any(|p| p["id"] == pane), "still open");
        assert!(!dd.capture(pane).is_empty());
    })
    .await
    .unwrap();

    // Revoked: an open viewer is cut off, and the link is dead.
    let mut ws = viewer(&d, &token).await.unwrap();
    watch_for(&mut ws, "live-9").await;
    let (dd, id) = (d.clone(), share["id"].as_u64().unwrap());
    tokio::task::spawn_blocking(move || {
        assert_eq!(dd.api("DELETE", &format!("/api/shares/{id}"), None).0, 200);
        assert_eq!(dd.api("DELETE", &format!("/api/shares/{id}"), None).0, 404);
    })
    .await
    .unwrap();
    assert_eq!(closed(&mut ws).await.0, CloseCode::Policy);
    assert!(viewer(&d, &token).await.is_err());
    let (dd, t) = (d.clone(), token.clone());
    let s = tokio::task::spawn_blocking(move || dd.http(&format!("/share/{t}"), &[("host", PUBLIC), FRIEND]).0);
    assert_eq!(s.await.unwrap(), 404);

    // Expired: the same.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let short = short["token"].as_str().unwrap().to_owned();
    assert!(viewer(&d, &short).await.is_err());
    let dd = d.clone();
    let s = tokio::task::spawn_blocking(move || dd.http(&format!("/share/{short}"), &[]).0);
    assert_eq!(s.await.unwrap(), 404);
}
