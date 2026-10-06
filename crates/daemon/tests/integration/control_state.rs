//! #325: a machine control drops says so. Against a fake control served
//! here (its certificate refresh, its relay socket, joins and leave), the
//! daemon's `/api/host` `control_state` goes from joined and connected to
//! dropped (what control said, and when) once control answers 401 and
//! confirms it has no such machine; dropped survives a restart with the
//! time it was first heard; the Getting started status says it's not
//! joined, so the page offers to join again; and `illogicald leave` makes
//! it not joined. A key removed from a browser (410, #330) is dropped too,
//! with the join the daemon asks for by itself.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{State, ws::WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use illogical_e2e::{Cert, DeviceKeys, Kind, cert::join_code};
use illogical_testkit::illogicald;
use serde_json::{Value, json};

const SAID: &str = "not an enrolled daemon (left, or revoked?)";
const REMOVED: &str = "this machine was removed from its account on 2026-10-03 by laptop, so its key can't join again: it needs a new key";

/// How the fake control treats the machine now.
const KNOWN: u8 = 0;
/// It left, or its account was deleted: 401.
const FORGOTTEN: u8 = 1;
/// Its key was removed from a browser: 410 (#330).
const REVOKED: u8 = 2;

#[derive(Clone, Default)]
struct Fake {
    now: Arc<AtomicU8>,
    /// The enrolled key's device id: removed, once `now` says so.
    old: Arc<std::sync::Mutex<String>>,
}

impl Fake {
    fn refusal(&self) -> Option<Response> {
        match self.now.load(Ordering::SeqCst) {
            FORGOTTEN => Some((StatusCode::UNAUTHORIZED, Json(json!({ "error": SAID }))).into_response()),
            REVOKED => Some(
                (
                    StatusCode::GONE,
                    Json(json!({ "error": REMOVED, "removed": { "at": 1_790_000_000_000u64, "by": "laptop" } })),
                )
                    .into_response(),
            ),
            _ => None,
        }
    }
}

async fn trust(State(f): State<Fake>) -> Response {
    f.refusal().unwrap_or_else(|| Json(json!({ "certs": [], "revocations": [] })).into_response())
}

async fn dial(State(f): State<Fake>, up: WebSocketUpgrade) -> Response {
    if let Some(r) = f.refusal() {
        return r;
    }
    up.on_upgrade(move |ws| async move {
        // Held open until control stops knowing the machine, then closed.
        while f.now.load(Ordering::SeqCst) == KNOWN {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        drop(ws);
    })
}

/// A join: the old key (whose control.json is still there) is refused as
/// removed; a new one gets its code, and waits.
async fn join(State(f): State<Fake>, Json(body): Json<Value>) -> Response {
    let cert: Cert = serde_json::from_value(body["cert"].clone()).unwrap();
    if f.now.load(Ordering::SeqCst) == REVOKED && cert.device == *f.old.lock().unwrap() {
        return f.refusal().unwrap();
    }
    Json(json!({ "code": join_code(&cert), "poll": "p", "expires_in_secs": 900 })).into_response()
}

fn serve(f: Fake) -> (tokio::runtime::Runtime, String) {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let url = rt.block_on(async {
        let app = Router::new()
            .route("/control.json", get(|| async { Json(json!({ "daemon_auth": 2 })) }))
            .route("/api/daemon/trust", get(trust))
            // Control names the account's own login with its peers (M30),
            // which the daemon keeps as what to call the account.
            .route("/api/daemon/peers", get(|| async { Json(json!({ "a1": { "name": "lex00" } })) }))
            .route("/api/daemon/push-subs", get(|| async { Json(json!({ "subs": [] })) }))
            .route("/api/daemon/access", post(|| async { Json(json!({})) }))
            .route("/api/daemon/leave", post(|| async { Json(json!({})) }))
            .route("/api/relay/dial", get(dial))
            .route("/api/join", post(join))
            .route("/api/join/{code}", get(|| async { Json(json!({ "approved": false })) }))
            .with_state(f);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        at
    });
    (rt, url)
}

/// Joined to the fake control, as `join` leaves it.
fn enroll(state: &std::path::Path, control: &str, fake: &Fake) {
    let keys = DeviceKeys::load_or_create(&state.join("daemon.key")).unwrap();
    *fake.old.lock().unwrap() = keys.id();
    let mut cert = Cert::new(&keys, "a1", Kind::Daemon, "test");
    cert.sign_with(&keys);
    let saved =
        json!({ "url": control, "trust": { "account": "a1", "root": keys.id() }, "cert": cert, "login": "lex00" });
    std::fs::write(state.join("control.json"), saved.to_string()).unwrap();
}

fn state(d: &illogical_testkit::Daemon) -> Value {
    d.get("/api/host")["control_state"].clone()
}

#[test]
fn a_machine_control_drops_says_so_until_it_leaves_or_joins_again() {
    let fake = Fake::default();
    let (_rt, url) = serve(fake.clone());
    let mut d = illogicald!("ctlstate")
        .no_wisp()
        .no_tailscale()
        .args(["--direct-url", "http://127.0.0.1:0", "--control", &url])
        .wait_secs(30)
        .start();
    assert_eq!(state(&d)["state"], "not_joined");

    enroll(&d.state, &url, &fake);
    d.wait_for("joined and connected", || {
        let s = state(&d);
        s["state"] == "joined" && s["connected"] == true
    });
    let s = state(&d);
    assert_eq!(
        (s["url"].as_str(), s["kind"].as_str(), s["name"].as_str()),
        (Some(url.as_str()), Some("account"), Some("lex00"))
    );
    assert!(s["seen_ms"].as_u64().is_some());

    // Control forgets it: the relay closes, which pokes a refresh, and
    // control confirms it has no such machine.
    fake.now.store(FORGOTTEN, Ordering::SeqCst);
    d.wait_for("dropped", || state(&d)["state"] == "dropped");
    let s = state(&d);
    assert_eq!(s["said"], SAID);
    assert_eq!(s["connected"], false);
    let at = s["dropped_ms"].as_u64().expect("when");
    assert!(d.state.join("control-dropped.json").is_file());
    assert!(d.state.join("control.json").is_file(), "kept: control may have been wrong");
    // Getting started: not joined, so its button joins again, to the same control.
    let setup = d.get("/api/setup?part=control")["control"].clone();
    assert!(setup["joined"].is_null(), "{setup}");
    assert_eq!(setup["state"]["state"], "dropped");
    assert_eq!(setup["url"], url.as_str());

    // A restart remembers it, and when it was first heard.
    d.stop();
    d.start();
    d.wait_for("dropped again", || state(&d)["state"] == "dropped");
    assert_eq!(state(&d)["dropped_ms"].as_u64(), Some(at));

    // Leaving takes it off: not joined, not dropped.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_illogicald"))
        .args(["leave", "--state-dir"])
        .arg(&d.state)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    d.wait_for("not joined", || state(&d)["state"] == "not_joined");
    assert!(!d.state.join("control-dropped.json").exists());
    assert!(!d.state.join("control-left.json").exists(), "the daemon read the note (and logged who left)");
}

/// A key removed from a browser (#330): dropped, with what control said,
/// and the join the daemon asked for with a new key; the old key and
/// enrollment set aside, and the log told why control.json went.
#[test]
fn a_removed_key_is_dropped_with_the_join_it_asks_for() {
    let fake = Fake::default();
    let (_rt, url) = serve(fake.clone());
    let d = illogicald!("ctlremoved")
        .no_wisp()
        .no_tailscale()
        .args(["--direct-url", "http://127.0.0.1:0", "--control", &url])
        .wait_secs(30)
        .start();
    enroll(&d.state, &url, &fake);
    d.wait_for("joined and connected", || state(&d)["connected"] == true);

    fake.now.store(REVOKED, Ordering::SeqCst);
    d.wait_for("dropped, with a code to approve", || {
        let s = state(&d);
        s["state"] == "dropped" && s["code"].is_string()
    });
    let s = state(&d);
    assert_eq!(s["said"], REMOVED);
    let code = s["code"].as_str().unwrap();
    assert_eq!(s["approve"].as_str(), Some(format!("{url}/#join={code}").as_str()));
    let setup = d.get("/api/setup?part=control")["control"].clone();
    assert_eq!(setup["pending"]["code"].as_str(), Some(code), "{setup}");
    assert_eq!(setup["removed"]["by"], "laptop");
    let aside: Vec<String> = std::fs::read_dir(&d.state)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.contains(".removed-"))
        .collect();
    assert_eq!(aside.len(), 2, "the key and its enrollment: {aside:?}");
    assert!(!d.state.join("control.json").exists());
    d.wait_for("the note read", || !d.state.join("control-left.json").exists());
}

/// Only the owner hears how the machine stands with control.
#[test]
fn a_guest_doesnt_see_the_control_state() {
    let d = illogicald!("ctlguest").no_wisp().no_tailscale().start();
    let v: Value = d.get("/api/host");
    assert_eq!(v["control_state"]["state"], "not_joined");
    let (status, _, body) = d.tcp("GET", "/api/host", &[], None);
    // Without the token: refused, or answered without the state.
    if status == 200 {
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v["control_state"].is_null(), "{v}");
    }
}
