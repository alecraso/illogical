//! M40: live updates for forge blocks, against fakes served here: a fake
//! illogical control (its relay socket, where the daemon subscribes and
//! hears pokes and heartbeats, and its GitHub App token endpoint), a fake
//! GitHub (S23's `cli/cli#13788`) with a stand-in `gh`, and a fake Forgejo
//! (S23's #84) with a stand-in `tea` that takes webhooks. Nothing here
//! talks to a real forge or a real control.
//!
//! What's checked: a GitHub block subscribes over the relay socket; a poke
//! for its PR makes it poll at once (another PR's doesn't); while control's
//! heartbeat says the path is live it polls slowly though it wants you,
//! and back to fast when the path goes quiet; a box with no `gh` login
//! reads through control's App token, read-only, and refuses writes; on
//! Forgejo, `live` as an agent is a draft and as the owner makes the hook
//! (its secret 0600 in the state directory), the hook route takes only a
//! signed delivery, which pokes the block, and `live off` removes it;
//! GitLab's route takes only its token.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

mod agentd;

use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use agentd::*;
use axum::{
    Json, Router,
    extract::{
        Path as UrlPath, State,
        ws::{Message, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use futures_util::{SinkExt, StreamExt};
use illogical_e2e::{Cert, DeviceKeys, Kind};
use serde_json::{Value, json};

const OWNER: &str = "owner@example.com";
const GH_TOKEN: &str = "fake-gh-token-m40";
const APP_TOKEN: &str = "ghs_fake-app-token-m40";
const FJ_TOKEN: &str = "fake-forgejo-token-m40";
const N: u64 = 13788;

fn fixture(dir: &str, f: &str) -> Value {
    let p = format!("{}/tests/fixtures/{dir}/{f}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn scratch(tag: &str) -> Scratch {
    Scratch::new(&format!("live-{tag}"))
}

fn script(bin: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(bin).unwrap();
    let p = bin.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// What the fakes saw.
#[derive(Default)]
struct Seen {
    /// GitHub: pull reads, and the tokens they came with.
    pulls: u32,
    tokens: Vec<String>,
    /// Control: texts from the daemon, token asks.
    texts: Vec<Value>,
    asked: Vec<Value>,
    /// Forgejo: hooks made and removed.
    hooks: Vec<(String, Value, String)>,
    fj_pulls: u32,
    fj_issues: u32,
}

#[derive(Clone)]
struct Fakes {
    seen: Arc<Mutex<Seen>>,
    say: tokio::sync::broadcast::Sender<String>,
    item: Arc<Value>,
}

fn bearer(h: &HeaderMap) -> String {
    h.get("authorization").and_then(|a| a.to_str().ok()).unwrap_or("").to_owned()
}

fn gh_ok(f: &Fakes, h: &HeaderMap) -> bool {
    let t = bearer(h);
    f.seen.lock().unwrap().tokens.push(t.clone());
    t == format!("Bearer {GH_TOKEN}") || t == format!("Bearer {APP_TOKEN}")
}

fn no() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "message": "Bad credentials" }))).into_response()
}

async fn gh_user(State(f): State<Fakes>, h: HeaderMap) -> Response {
    if bearer(&h) == format!("Bearer {APP_TOKEN}") {
        // As GitHub: an installation token isn't a user.
        return (StatusCode::FORBIDDEN, Json(json!({ "message": "Resource not accessible by integration" })))
            .into_response();
    }
    if !gh_ok(&f, &h) {
        return no();
    }
    Json(json!({ "id": 1, "login": "jhgaylor" })).into_response()
}

async fn gh_list(State(f): State<Fakes>, h: HeaderMap) -> Response {
    if !gh_ok(&f, &h) {
        return no();
    }
    Json(json!([])).into_response()
}

async fn gh_pull(State(f): State<Fakes>, h: HeaderMap) -> Response {
    if !gh_ok(&f, &h) {
        return no();
    }
    f.seen.lock().unwrap().pulls += 1;
    Json((*f.item).clone()).into_response()
}

async fn gh_runs(State(f): State<Fakes>, h: HeaderMap) -> Response {
    if !gh_ok(&f, &h) {
        return no();
    }
    Json(json!({ "total_count": 0, "check_runs": [] })).into_response()
}

async fn gh_status(State(f): State<Fakes>, h: HeaderMap) -> Response {
    if !gh_ok(&f, &h) {
        return no();
    }
    Json(json!({ "state": "pending", "total_count": 0, "statuses": [] })).into_response()
}

async fn gh_write(State(f): State<Fakes>, h: HeaderMap) -> Response {
    f.seen.lock().unwrap().hooks.push(("github write".into(), Value::Null, bearer(&h)));
    StatusCode::CREATED.into_response()
}

async fn dial(State(f): State<Fakes>, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |ws| async move {
        let (mut tx, mut rx) = ws.split();
        let mut say = f.say.subscribe();
        loop {
            tokio::select! {
                m = rx.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(v) = serde_json::from_str(t.as_str()) {
                            f.seen.lock().unwrap().texts.push(v);
                        }
                    }
                    Some(Ok(_)) => {}
                    _ => break,
                },
                s = say.recv() => match s {
                    Ok(s) => if tx.send(Message::Text(s.into())).await.is_err() { break },
                    Err(_) => break,
                },
            }
        }
    })
}

async fn app_token(State(f): State<Fakes>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if !h.contains_key("x-illogical-auth") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    f.seen.lock().unwrap().asked.push(b);
    Json(json!({ "token": APP_TOKEN, "expires_at": "2099-01-01T00:00:00Z", "login": "jhgaylor", "app": "illogical-test" }))
        .into_response()
}

fn fj_ok(h: &HeaderMap) -> bool {
    bearer(h) == format!("token {FJ_TOKEN}")
}

async fn fj_user(h: HeaderMap) -> Response {
    if !fj_ok(&h) {
        return no();
    }
    Json(json!({ "id": 1, "login": "jhgaylor" })).into_response()
}

async fn fj_pull(State(f): State<Fakes>, h: HeaderMap) -> Response {
    if !fj_ok(&h) {
        return no();
    }
    f.seen.lock().unwrap().fj_pulls += 1;
    let mut it = fixture("forgejo/forgejo-illogical-84", "item.json");
    it["state"] = json!("open");
    it["merged"] = json!(false);
    it["merged_at"] = Value::Null;
    Json(it).into_response()
}

async fn fj_issue(State(f): State<Fakes>, h: HeaderMap) -> Response {
    if !fj_ok(&h) {
        return no();
    }
    f.seen.lock().unwrap().fj_issues += 1;
    Json(fixture("forgejo/forgejo-illogical-issue-73", "item.json")).into_response()
}

async fn fj_status(h: HeaderMap) -> Response {
    if !fj_ok(&h) {
        return no();
    }
    Json(json!({ "state": "success", "total_count": 0, "statuses": [] })).into_response()
}

async fn fj_list(h: HeaderMap) -> Response {
    if !fj_ok(&h) {
        return no();
    }
    Json(json!([])).into_response()
}

async fn fj_hook(State(f): State<Fakes>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if !fj_ok(&h) {
        return no();
    }
    f.seen.lock().unwrap().hooks.push(("create".into(), b, bearer(&h)));
    (StatusCode::CREATED, Json(json!({ "id": 77, "type": "forgejo", "active": true }))).into_response()
}

async fn fj_unhook(
    State(f): State<Fakes>,
    h: HeaderMap,
    UrlPath((_, _, id)): UrlPath<(String, String, u64)>,
) -> Response {
    if !fj_ok(&h) {
        return no();
    }
    f.seen.lock().unwrap().hooks.push((format!("delete {id}"), Value::Null, bearer(&h)));
    StatusCode::NO_CONTENT.into_response()
}

/// Every fake on one runtime: (runtime, fakes, origin).
fn serve() -> (tokio::runtime::Runtime, Fakes, String) {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let mut item = fixture("github/github-cli-cli-13788", "item.json");
    // A review asked of you: the block wants you, so it polls fast.
    item["requested_reviewers"] = json!([{ "login": "jhgaylor" }]);
    item["requested_teams"] = json!([]);
    let f = Fakes { seen: Arc::default(), say: tokio::sync::broadcast::channel(64).0, item: Arc::new(item) };
    let origin = rt.block_on(async {
        let p = "/repos/{o}/{r}";
        let fj = "/api/v1/repos/{o}/{r}";
        let app = Router::new()
            // GitHub's API.
            .route("/user", get(gh_user))
            .route("/user/teams", get(gh_list))
            .route(&format!("{p}/pulls/{{n}}"), get(gh_pull))
            .route(&format!("{p}/pulls/{{n}}/reviews"), get(gh_list).post(gh_write))
            .route(&format!("{p}/issues/{{n}}/timeline"), get(gh_list))
            .route(&format!("{p}/issues/{{n}}/comments"), post(gh_write))
            .route(&format!("{p}/commits/{{sha}}/check-runs"), get(gh_runs))
            .route(&format!("{p}/commits/{{sha}}/status"), get(gh_status))
            // Control's.
            .route("/api/relay/dial", get(dial))
            .route("/api/daemon/github/token", post(app_token))
            // Forgejo's.
            .route("/api/v1/user", get(fj_user))
            .route("/api/v1/user/teams", get(fj_list))
            .route(&format!("{fj}/pulls/{{n}}"), get(fj_pull))
            .route(&format!("{fj}/pulls/{{n}}/reviews"), get(fj_list))
            .route(&format!("{fj}/issues/{{n}}"), get(fj_issue))
            .route(&format!("{fj}/issues/{{n}}/timeline"), get(fj_list))
            .route(&format!("{fj}/commits/{{sha}}/status"), get(fj_status))
            .route(&format!("{fj}/hooks"), post(fj_hook))
            .route(&format!("{fj}/hooks/{{id}}"), delete(fj_unhook))
            .with_state(f.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        at
    });
    (rt, f, origin)
}

fn daemon(bin: &Path, origin: &str, poll: &str) -> Daemon {
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    Daemon::child_env(
        &[
            "--wisp-token-file",
            "/nonexistent",
            "--owner",
            OWNER,
            "--tailscale-socket",
            "/nonexistent/sock",
            "--direct-url",
            "http://127.0.0.1:0",
        ],
        &[
            ("PATH", &path),
            ("ILLOGICAL_FORGE_POLL_MS", poll),
            ("ILLOGICAL_FORGE_LIVE_MS", "2000"),
            ("ILLOGICAL_GITHUB_API", origin),
        ],
    )
}

/// Joined to the fake control (its keys and `control.json`, as `join`
/// leaves them; the running daemon notices within seconds).
fn enroll(d: &Daemon, control: &str) {
    let keys = DeviceKeys::load_or_create(&d.state.join("daemon.key")).unwrap();
    let cert = Cert::new(&keys, "a1", Kind::Daemon, "test");
    let saved = json!({ "url": control, "trust": { "account": "a1", "root": keys.id() }, "cert": cert });
    std::fs::write(d.state.join("control.json"), saved.to_string()).unwrap();
}

fn open(d: &Daemon, config: Value) -> u64 {
    d.post("/api/blocks", json!({ "type": "forge", "config": config, "local": true }))["block"].as_u64().unwrap()
}

fn polls(d: &Daemon, b: u64) -> u64 {
    d.state(b)["polls"].as_u64().unwrap_or(0)
}

fn wait_until(what: &str, secs: u64, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[test]
fn a_poke_polls_at_once_and_a_live_path_polls_slowly_until_it_goes_quiet() {
    let dir = scratch("gh");
    let (_rt, f, origin) = serve();
    let bin = dir.join("bin");
    script(&bin, "gh", &format!("case \"$1 $2\" in \"auth token\") echo {GH_TOKEN} ;; *) exit 2 ;; esac\n"));
    script(&bin, "tea", "case \"$1 $2\" in \"logins list\") echo '[]' ;; *) exit 2 ;; esac\n");
    let d = daemon(&bin, &origin, "150,60000");
    let b = open(&d, json!({ "pr": format!("https://github.com/cli/cli/pull/{N}") }));
    wait_until("the first read", 20, || polls(&d, b) > 0);
    let st = d.state(b);
    assert_eq!(st["live"], "polling", "{st}");
    assert!(st["live_why"].as_str().unwrap().contains("not joined"), "{st}");
    // It wants you (a review asked): fast.
    let p0 = polls(&d, b);
    std::thread::sleep(Duration::from_millis(1000));
    assert!(polls(&d, b) >= p0 + 3, "polls fast while it wants you and nothing's live");

    // Joined: it tells control which repositories it watches.
    enroll(&d, &origin);
    wait_until("the subscription", 20, || {
        f.seen.lock().unwrap().texts.iter().any(|t| t["t"] == "forge.watch" && t["repos"] == json!(["cli/cli"]))
    });
    assert_eq!(d.state(b)["live_why"], "control hasn't said yet");
    // Control's heartbeat: live.
    let watching = json!({ "t": "forge.watching", "repos": [{ "repo": "cli/cli", "live": true }] }).to_string();
    f.say.send(watching.clone()).unwrap();
    wait_until("live", 5, || d.state(b)["live"] == "webhook");
    assert_eq!(d.state(b)["live_via"], "github-app");
    assert!(d.raw("GET", &format!("/api/panes/{b}/capture?format=text"), None).1.contains("live: webhook"));
    std::thread::sleep(Duration::from_millis(200));
    let p1 = polls(&d, b);
    std::thread::sleep(Duration::from_millis(800));
    assert!(polls(&d, b) <= p1 + 1, "slow while live: {} then {}", p1, polls(&d, b));

    // A poke for this PR: a poll at once.
    let pulls = f.seen.lock().unwrap().pulls;
    let poke = |n: Option<u64>| {
        json!({ "t": "forge.poke", "poke": { "provider": "github", "host": "github.com", "repo": "cli/cli",
            "number": n, "event": "pull_request_review", "delivery": "x" } })
        .to_string()
    };
    let t0 = Instant::now();
    f.say.send(poke(Some(N))).unwrap();
    wait_until("the poked poll", 3, || f.seen.lock().unwrap().pulls > pulls);
    assert!(t0.elapsed() < Duration::from_millis(1500), "poked poll took {:?}", t0.elapsed());
    assert_eq!(d.state(b)["pokes"], 1);
    // Another PR's poke isn't this block's; a repo-wide one is.
    std::thread::sleep(Duration::from_millis(200));
    let pulls = f.seen.lock().unwrap().pulls;
    f.say.send(poke(Some(1))).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(f.seen.lock().unwrap().pulls, pulls, "another PR's poke");
    f.say.send(poke(None)).unwrap();
    wait_until("the repo-wide poke", 3, || f.seen.lock().unwrap().pulls > pulls);

    // Nothing more from control: after the quiet time, fast again.
    wait_until("polling again", 6, || d.state(b)["live"] == "polling");
    let p2 = polls(&d, b);
    std::thread::sleep(Duration::from_millis(1000));
    assert!(polls(&d, b) >= p2 + 3, "fast again once quiet");
    // Control says no, and why.
    let no = json!({ "t": "forge.watching", "repos": [{ "repo": "cli/cli", "live": false,
        "why": "the GitHub App illogical-test isn't installed on cli" }] });
    f.say.send(no.to_string()).unwrap();
    wait_until("the reason", 5, || d.state(b)["live_why"].as_str().is_some_and(|w| w.contains("isn't installed")));
    // Closing it unsubscribes.
    d.post(&format!("/api/panes/{b}/close"), json!({}));
    wait_until("the unsubscription", 10, || {
        f.seen.lock().unwrap().texts.last().is_some_and(|t| t["t"] == "forge.watch" && t["repos"] == json!([]))
    });
    // The block's token never went to control.
    assert!(!f.seen.lock().unwrap().texts.iter().any(|t| t.to_string().contains(GH_TOKEN)));
}

#[test]
fn a_box_with_no_gh_login_reads_through_the_app_and_writes_nothing() {
    let dir = scratch("app");
    let (_rt, f, origin) = serve();
    let bin = dir.join("bin");
    // gh is here but logged in nowhere.
    script(&bin, "gh", "echo 'not logged in' >&2; exit 1\n");
    script(&bin, "tea", "case \"$1 $2\" in \"logins list\") echo '[]' ;; *) exit 2 ;; esac\n");
    let d = daemon(&bin, &origin, "250,250");
    enroll(&d, &origin);
    let b = open(&d, json!({ "pr": format!("https://github.com/cli/cli/pull/{N}") }));
    wait_until("joined", 20, || !f.seen.lock().unwrap().texts.is_empty());
    // The App's read and what control says (who you are, read-only) come
    // separately, in either order (#138): wait for all of them.
    wait_until("a read, and who you are", 20, || {
        let st = d.state(b);
        st["pr"].is_object()
            && st["me"] == "jhgaylor"
            && st["read_only"].as_str().is_some_and(|r| r.contains("GitHub App"))
            && st["wants"].as_array().is_some_and(|w| w.iter().any(|w| w["kind"] == "review"))
    });
    let st = d.state(b);
    assert!(st["error"].is_null(), "{st}");
    assert_eq!(f.seen.lock().unwrap().asked[0], json!({ "repo": "cli/cli" }));
    assert!(f.seen.lock().unwrap().tokens.iter().any(|t| t == &format!("Bearer {APP_TOKEN}")));
    // It wants you: a review asked of jhgaylor, from the App's read.
    assert!(st["wants"].as_array().unwrap().iter().any(|w| w["kind"] == "review"), "{st}");
    // Writes need the person's own login: refused, a person's or an agent's.
    for args in [json!({ "body": "hi" }), json!({ "body": "hi", "agent": true })] {
        let (code, body) = d.raw("POST", &format!("/api/blocks/{b}/call/comment"), Some(args));
        assert_ne!(code, 200, "{body}");
        assert!(body.contains("gh auth login"), "{body}");
    }
    assert!(d.state(b)["drafts"].as_array().unwrap().is_empty());
    assert!(!f.seen.lock().unwrap().hooks.iter().any(|(w, _, _)| w == "github write"));
    // The App's token is nowhere a client sees.
    assert!(!d.get(&format!("/api/blocks/{b}")).to_string().contains(APP_TOKEN));
    assert!(!std::fs::read_to_string(d.state.join("control.json")).unwrap().contains(APP_TOKEN));
}

fn hmac_hex(secret: &str, body: &str) -> String {
    use hmac::{KeyInit, Mac};
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// A POST to the daemon's TCP listener, as a forge sends it.
fn deliver(d: &Daemon, path_and_query: &str, headers: &[(&str, &str)], body: &str) -> u16 {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", d.port)).unwrap();
    let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    write!(
        s,
        "POST {path_and_query} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{extra}Connection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        d.port,
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
}

#[test]
fn forgejo_live_updates_make_a_signed_hook_that_pokes_the_block() {
    let dir = scratch("fj");
    let (_rt, f, origin) = serve();
    let bin = dir.join("bin");
    let logins = json!([{ "name": "forgejo", "url": origin, "ssh_host": "", "user": "jhgaylor", "default": "false" }]);
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("logins.json"), logins.to_string()).unwrap();
    script(
        &bin,
        "tea",
        &format!(
            "case \"$1 $2 $3\" in\n  \"logins list -o\") cat '{}/logins.json' ;;\n  \"login helper get\") cat >/dev/null; echo protocol=http; echo username=jhgaylor; echo password={FJ_TOKEN} ;;\n  *) exit 2 ;;\nesac\n",
            bin.display()
        ),
    );
    script(&bin, "gh", "exit 1\n");
    let d = daemon(&bin, &origin, "150,60000");
    let b = open(&d, json!({ "pr": format!("{origin}/jhgaylor/illogical/pulls/84") }));
    wait_until("the first read", 20, || d.state(b)["pr"].is_object());
    assert_eq!(d.state(b)["live"], "polling");
    assert_eq!(d.state(b)["hook"], false);

    // An agent's `live` is a draft: nothing reaches Forgejo.
    let v = d.call(b, "live", json!({ "on": true, "agent": true }));
    assert_eq!(v["status"], "waiting", "{v}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(f.seen.lock().unwrap().hooks.is_empty());
    // The owner's makes the hook, with their login.
    let v = d.call(b, "live", json!({ "on": true }));
    assert!(v["said"].as_str().unwrap().contains("turned on live updates"), "{v}");
    let (what, body, token) = f.seen.lock().unwrap().hooks[0].clone();
    assert_eq!((what.as_str(), token.as_str()), ("create", format!("token {FJ_TOKEN}").as_str()));
    assert_eq!((body["type"].as_str(), body["config"]["content_type"].as_str()), (Some("forgejo"), Some("json")));
    let events: Vec<&str> = body["events"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
    for e in ["pull_request", "pull_request_review_approved", "issue_comment", "status"] {
        assert!(events.contains(&e), "{events:?}");
    }
    let url = body["config"]["url"].as_str().unwrap().to_owned();
    let secret = body["config"]["secret"].as_str().unwrap().to_owned();
    assert!(url.starts_with(&format!("http://127.0.0.1:{}/api/forge/hooks/forgejo?k=", d.port)), "{url}");
    assert_eq!(secret.len(), 64);
    // The secret is kept 0600, in the daemon's own state.
    let file = d.state.join("secrets/forge-hooks.json");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(std::fs::read_to_string(&file).unwrap().contains(&secret));
    assert!(!d.get(&format!("/api/blocks/{b}")).to_string().contains(&secret), "the secret isn't in the state");
    wait_until("live", 5, || d.state(b)["live"] == "webhook" && d.state(b)["hook"] == true);

    // Deliveries: only signed ones, for this repository.
    let path = url.split_once(&format!("127.0.0.1:{}", d.port)).unwrap().1.to_owned();
    let payload = json!({ "action": "reviewed", "number": 84, "repository": { "full_name": "jhgaylor/illogical" },
        "pull_request": { "number": 84 } })
    .to_string();
    let ev = ("x-forgejo-event", "pull_request_review_approved");
    assert_eq!(deliver(&d, &path, &[ev], &payload), 401, "unsigned");
    assert_eq!(deliver(&d, &path, &[ev, ("x-forgejo-signature", &hmac_hex("wrong", &payload))], &payload), 401);
    assert_eq!(
        deliver(
            &d,
            "/api/forge/hooks/forgejo?k=nope",
            &[ev, ("x-forgejo-signature", &hmac_hex(&secret, &payload))],
            &payload
        ),
        401,
        "an unknown hook"
    );
    let other = json!({ "repository": { "full_name": "someone/else" }, "pull_request": { "number": 84 } }).to_string();
    assert_eq!(deliver(&d, &path, &[ev, ("x-gitea-signature", &hmac_hex(&secret, &other))], &other), 400);
    std::thread::sleep(Duration::from_millis(200));
    let before = f.seen.lock().unwrap().fj_pulls;
    assert_eq!(deliver(&d, &path, &[ev, ("x-forgejo-signature", &hmac_hex(&secret, &payload))], &payload), 200);
    wait_until("the poked poll", 3, || f.seen.lock().unwrap().fj_pulls > before);
    assert_eq!(d.state(b)["pokes"], 1);
    // An issue block on the same repository hears its own number.
    let i = open(&d, json!({ "issue": format!("{origin}/jhgaylor/illogical/issues/73") }));
    wait_until("the issue's read", 20, || d.state(i)["issue"].is_object());
    std::thread::sleep(Duration::from_millis(200));
    let before = f.seen.lock().unwrap().fj_issues;
    let comment = json!({ "action": "created", "repository": { "full_name": "jhgaylor/illogical" },
        "issue": { "number": 73 }, "comment": { "body": "hm" } })
    .to_string();
    let cev = ("x-forgejo-event", "issue_comment");
    assert_eq!(deliver(&d, &path, &[cev, ("x-forgejo-signature", &hmac_hex(&secret, &comment))], &comment), 200);
    wait_until("the issue's poked read", 3, || f.seen.lock().unwrap().fj_issues > before);
    assert_eq!(d.state(i)["pokes"], 1);
    assert_eq!(d.state(b)["pokes"], 1, "#84 isn't #73");
    // Gitea's header name works too.
    assert_eq!(deliver(&d, &path, &[("x-gitea-signature", &hmac_hex(&secret, &payload))], &payload), 200);
    wait_until("the second poke", 3, || d.state(b)["pokes"] == 2);

    // Off: the hook goes, and so does its secret.
    let v = d.call(b, "live", json!({ "on": false }));
    assert!(v["said"].as_str().unwrap().contains("turned off"), "{v}");
    assert_eq!(f.seen.lock().unwrap().hooks.last().unwrap().0, "delete 77");
    assert!(!std::fs::read_to_string(&file).unwrap().contains(&secret));
    assert_eq!(d.state(b)["hook"], false);
    assert_eq!(deliver(&d, &path, &[("x-forgejo-signature", &hmac_hex(&secret, &payload))], &payload), 401);
    // GitHub blocks say where theirs come from instead.
    let (code, body) = d.raw("POST", &format!("/api/blocks/{b}/call/live"), Some(json!({ "on": "yes" })));
    assert_ne!(code, 200, "{body}");
}

#[test]
fn gitlabs_hook_route_takes_only_its_token() {
    let dir = scratch("gl");
    let (_rt, _f, origin) = serve();
    let bin = dir.join("bin");
    script(&bin, "gh", "exit 1\n");
    let mut d = daemon(&bin, &origin, "250,250");
    // A hook made earlier (as `live` on a GitLab block leaves it).
    d.stop();
    let rec = json!({ "hooks": [{ "k": "gk1", "provider": "gitlab", "host": "gitlab.example", "repo": "g/sub/p",
        "secret": "s3cret-token", "id": 5, "url": "x", "created_ms": 0 }] });
    std::fs::create_dir_all(d.state.join("secrets")).unwrap();
    std::fs::write(d.state.join("secrets/forge-hooks.json"), rec.to_string()).unwrap();
    d.start();
    let mr = json!({ "object_kind": "merge_request", "project": { "path_with_namespace": "g/sub/p" },
        "object_attributes": { "iid": 3 } })
    .to_string();
    let path = "/api/forge/hooks/gitlab?k=gk1";
    let ev = ("x-gitlab-event", "Merge Request Hook");
    assert_eq!(deliver(&d, path, &[ev], &mr), 401, "no token");
    assert_eq!(deliver(&d, path, &[ev, ("x-gitlab-token", "s3cret-tokeN")], &mr), 401);
    assert_eq!(deliver(&d, "/api/forge/hooks/gitlab?k=gk2", &[ev, ("x-gitlab-token", "s3cret-token")], &mr), 401);
    // Forgejo's route doesn't take a GitLab hook's key.
    assert_eq!(deliver(&d, "/api/forge/hooks/forgejo?k=gk1", &[("x-forgejo-signature", "00")], &mr), 401);
    let other = json!({ "object_kind": "note", "project": { "path_with_namespace": "x/y" } }).to_string();
    assert_eq!(deliver(&d, path, &[("x-gitlab-token", "s3cret-token")], &other), 400);
    assert_eq!(deliver(&d, path, &[ev, ("x-gitlab-token", "s3cret-token")], &mr), 200);
    // From the tailnet as someone nothing is shared with (the forge's
    // node): the signature is all that counts, either way.
    let stranger = ("tailscale-user-login", "forge-bot@example.com");
    assert_eq!(deliver(&d, path, &[ev, stranger, ("x-gitlab-token", "s3cret-token")], &mr), 200);
    assert_eq!(deliver(&d, path, &[ev, stranger, ("x-gitlab-token", "nope")], &mr), 401);
    // ...and that's all such a request reaches.
    assert_eq!(deliver(&d, "/api/run", &[stranger], "{}"), 403);
}
