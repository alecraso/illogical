//! #507: control answers at a new URL and the old one, and says which is
//! where it is now. A daemon joined at the old URL follows it to the new
//! one, without joining again, once the new URL agrees the old one is its
//! own; and it doesn't follow when it doesn't.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use arugula_e2e::{Cert, DeviceKeys, Kind};
use arugula_testkit::arugulad;
use axum::{
    Json, Router,
    extract::{State, ws::WebSocketUpgrade},
    response::Response,
    routing::{get, post},
};
use serde_json::{Value, json};

/// What one fake control says in `/control.json`, and the requests it got.
#[derive(Clone, Default)]
struct Fake {
    about: Arc<Mutex<Value>>,
    trust_asked: Arc<Mutex<u32>>,
    /// Closes the relay socket, which makes the daemon refresh at once.
    kick: Arc<tokio::sync::Notify>,
}

async fn about(State(f): State<Fake>) -> Json<Value> {
    Json(f.about.lock().unwrap().clone())
}

async fn trust(State(f): State<Fake>) -> Json<Value> {
    *f.trust_asked.lock().unwrap() += 1;
    Json(json!({ "certs": [], "revocations": [] }))
}

async fn dial(State(f): State<Fake>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |ws| async move {
        f.kick.notified().await;
        drop(ws);
    })
}

fn serve(rt: &tokio::runtime::Runtime, f: Fake) -> String {
    rt.block_on(async {
        let app = Router::new()
            .route("/control.json", get(about))
            .route("/api/daemon/trust", get(trust))
            .route("/api/daemon/peers", get(|| async { Json(json!({ "a1": { "name": "lex00" } })) }))
            .route("/api/daemon/push-subs", get(|| async { Json(json!({ "subs": [] })) }))
            .route("/api/daemon/access", post(|| async { Json(json!({})) }))
            .route("/api/relay/dial", get(dial))
            .with_state(f);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        at
    })
}

fn enroll(state: &std::path::Path, control: &str) {
    let keys = DeviceKeys::load_or_create(&state.join("daemon.key")).unwrap();
    let mut cert = Cert::new(&keys, "a1", Kind::Daemon, "test");
    cert.sign_with(&keys);
    let saved =
        json!({ "url": control, "trust": { "account": "a1", "root": keys.id() }, "cert": cert, "login": "lex00" });
    std::fs::write(state.join("control.json"), saved.to_string()).unwrap();
}

fn saved_url(state: &std::path::Path) -> String {
    let v: Value = serde_json::from_slice(&std::fs::read(state.join("control.json")).unwrap()).unwrap();
    v["url"].as_str().unwrap().to_owned()
}

#[test]
fn a_daemon_follows_control_to_its_new_url() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let (old, new) = (Fake::default(), Fake::default());
    let old_url = serve(&rt, old.clone());
    let new_url = serve(&rt, new.clone());
    // Before it moves, each says it's where it is.
    *old.about.lock().unwrap() = json!({ "control": true, "daemon_auth": 2, "url": old_url });
    *new.about.lock().unwrap() = json!({ "control": true, "daemon_auth": 2, "url": new_url });

    let mut d = arugulad!("ctlmoves")
        .no_wisp()
        .no_tailscale()
        .args(["--direct-url", "http://127.0.0.1:0"])
        .wait_secs(30)
        .start();
    enroll(&d.state, &old_url);
    d.stop();
    d.start();
    d.wait_for("joined at the old URL", || d.get("/api/host")["control_state"]["state"] == "joined");
    assert_eq!(saved_url(&d.state), old_url, "nothing to follow yet");

    // The old one says control is at the new one, but the new one doesn't
    // count the old as its own: not followed.
    let both = json!([new_url, old_url]);
    *old.about.lock().unwrap() =
        json!({ "control": true, "daemon_auth": 2, "url": old_url, "primary": new_url, "urls": both });
    let asked = *old.trust_asked.lock().unwrap();
    old.kick.notify_waiters();
    d.wait_for("a refresh", || *old.trust_asked.lock().unwrap() > asked);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(saved_url(&d.state), old_url, "the new URL must agree");

    // Both agree: followed, and it asks the new one from then on.
    *new.about.lock().unwrap() =
        json!({ "control": true, "daemon_auth": 2, "url": new_url, "primary": new_url, "urls": both });
    d.wait_for("moved to the new URL", || {
        old.kick.notify_waiters();
        std::thread::sleep(Duration::from_millis(200));
        saved_url(&d.state) == new_url
    });
    assert_eq!(d.get("/api/host")["control_state"]["url"], new_url.as_str());
    let before = *new.trust_asked.lock().unwrap();
    d.wait_for("asking the new URL", || {
        old.kick.notify_waiters();
        new.kick.notify_waiters();
        std::thread::sleep(Duration::from_millis(200));
        *new.trust_asked.lock().unwrap() > before
    });
    // And after a restart, it starts there.
    d.stop();
    d.start();
    d.wait_for("joined at the new URL", || d.get("/api/host")["control_state"]["url"] == new_url.as_str());
}
