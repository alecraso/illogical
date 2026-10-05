//! M40 end to end inside control: its real router on a port, a fake
//! GitHub API that checks the App's JWT, and daemons on the relay socket
//! (signed as daemons sign) that subscribe and hear pokes.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use futures_util::{SinkExt, StreamExt};
use illogical_e2e::{Cert, DeviceKeys, Kind, now_ms};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

use crate::{App, forge};

const SECRET: &str = "whsec-m40-test";

#[derive(Default)]
struct Hub {
    public: Vec<u8>,
    minted: Vec<Value>,
    asked: Vec<String>,
}

type Shared = Arc<Mutex<Hub>>;

/// A JWT this App signed, unexpired.
fn app_jwt(h: &Shared, headers: &HeaderMap) -> bool {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    let Some(t) = auth.strip_prefix("Bearer ") else { return false };
    let p: Vec<&str> = t.split('.').collect();
    if p.len() != 3 {
        return false;
    }
    let Ok(sig) = B64.decode(p[2]) else { return false };
    let public = h.lock().unwrap().public.clone();
    let ok = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &public)
        .verify(format!("{}.{}", p[0], p[1]).as_bytes(), &sig)
        .is_ok();
    let claims: Value = serde_json::from_slice(&B64.decode(p[1]).unwrap_or_default()).unwrap_or_default();
    let now = now_ms() / 1000;
    ok && claims["iss"] == 42 && claims["exp"].as_u64().is_some_and(|e| e > now && e <= now + 600)
}

async fn installation(State(h): State<Shared>, headers: HeaderMap, Path((o, r)): Path<(String, String)>) -> Response {
    if !app_jwt(&h, &headers) {
        return (StatusCode::UNAUTHORIZED, "bad JWT").into_response();
    }
    h.lock().unwrap().asked.push(format!("installation {o}/{r}"));
    match o.as_str() {
        "jhgaylor" => {
            Json(json!({ "id": 11, "account": { "login": "jhgaylor", "id": 1, "type": "User" } })).into_response()
        }
        "acme" => {
            Json(json!({ "id": 12, "account": { "login": "acme", "id": 900, "type": "Organization" } })).into_response()
        }
        _ => (StatusCode::NOT_FOUND, Json(json!({ "message": "Not Found" }))).into_response(),
    }
}

async fn access_tokens(
    State(h): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Json(body): Json<Value>,
) -> Response {
    if !app_jwt(&h, &headers) {
        return (StatusCode::UNAUTHORIZED, "bad JWT").into_response();
    }
    h.lock().unwrap().minted.push(json!({ "installation": id, "body": body }));
    (StatusCode::CREATED, Json(json!({ "token": format!("ghs_test_{id}"), "expires_at": "2099-01-01T00:00:00Z" })))
        .into_response()
}

async fn permission(headers: HeaderMap, Path((o, r, login)): Path<(String, String, String)>) -> Response {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    if auth != "Bearer ghs_test_12" || (o, r) != ("acme".into(), "tool".into()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // Who has each login now: jhgaylor is GitHub user 1.
    let (p, id) = match login.as_str() {
        "jhgaylor" => ("write", 1),
        "stranger" => ("none", 2),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    Json(json!({ "permission": p, "user": { "login": login, "id": id } })).into_response()
}

async fn fake_github(h: Shared) -> String {
    let r = Router::new()
        .route("/repos/{o}/{r}/installation", get(installation))
        .route("/app/installations/{id}/access_tokens", post(access_tokens))
        .route("/repos/{o}/{r}/collaborators/{login}/permission", get(permission))
        .with_state(h);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
    at
}

fn sign_header(keys: &DeviceKeys, method: &str, path: &str) -> String {
    let ms = now_ms();
    let msg = crate::auth::daemon_auth_message(method, path, ms);
    format!("{} {ms} {}", keys.id(), hex::encode(keys.signature(msg.as_bytes())))
}

fn daemon(app: &App, account: &str, name: &str) -> DeviceKeys {
    let keys = DeviceKeys::generate();
    let cert = Cert::new(&keys, account, Kind::Daemon, name);
    app.db.put_device(&cert, true, now_ms()).unwrap();
    app.db.put_daemon(account, &cert.device, name, &[]).unwrap();
    keys
}

type Sock = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn dial(base: &str, keys: &DeviceKeys) -> Sock {
    let mut req = format!("{}/api/relay/dial", base.replace("http://", "ws://")).into_client_request().unwrap();
    req.headers_mut().insert("x-illogical-auth", sign_header(keys, "GET", "/api/relay/dial").parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// The next text message, or none within `ms`.
async fn text(s: &mut Sock, ms: u64) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        let m = tokio::time::timeout_at(deadline, s.next()).await.ok()??.ok()?;
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).ok();
        }
    }
}

fn sig(body: &str) -> String {
    use hmac::{KeyInit, Mac};
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(SECRET.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

async fn hook(base: &str, event: &str, delivery: &str, body: &Value, signature: Option<String>) -> (u16, Value) {
    let body = body.to_string();
    let mut req = reqwest::Client::new()
        .post(format!("{base}/github/webhook"))
        .header("x-github-event", event)
        .header("x-github-delivery", delivery)
        .header("content-type", "application/json");
    if let Some(s) = signature {
        req = req.header("x-hub-signature-256", s);
    }
    let r = req.body(body).send().await.unwrap();
    (r.status().as_u16(), r.json().await.unwrap_or_default())
}

#[tokio::test]
async fn webhooks_poke_only_subscribed_daemons_of_allowed_accounts() {
    let (pem, public) = forge::tests::throwaway_key();
    let h: Shared = Arc::new(Mutex::new(Hub { public, ..Hub::default() }));
    let api = fake_github(h.clone()).await;
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let mut app = App::for_tests(&base);
    app.github_app = Some(forge::GithubApp::new(
        "42".into(),
        "illogical-test".into(),
        SECRET.into(),
        &api,
        forge::parse_pem(&pem).unwrap(),
    ));
    let app = Arc::new(app);
    // Jake signed in with GitHub; a stranger did too; someone else only
    // with a passkey.
    let now = now_ms();
    app.db.account_for("github", "1", "jhgaylor", "a1", now).unwrap();
    app.db.account_for("github", "2", "stranger", "a2", now).unwrap();
    app.db.account_for("passkey", "p3", "pk", "a3", now).unwrap();
    // Someone who was jhgaylor once (GitHub user 77) and hasn't signed in
    // since they renamed: control still has the old login for them.
    app.db.account_for("github", "77", "jhgaylor", "a4", now).unwrap();
    let k4 = daemon(&app, "a4", "was-jhgaylor");
    let (k1, k2, k3) = (daemon(&app, "a1", "geek"), daemon(&app, "a2", "theirs"), daemon(&app, "a3", "pk"));
    let svc = crate::router(app.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(l, svc).await.unwrap() });
    tokio::spawn(forge::heartbeat(app.clone(), Duration::from_millis(700)));

    let watch = json!({ "t": "forge.watch", "repos": ["jhgaylor/hud", "acme/tool", "nobody/x", "bad/../x"] });
    let mut socks = Vec::new();
    for k in [&k1, &k2, &k3] {
        let mut s = dial(&base, k).await;
        s.send(Message::Text(watch.to_string().into())).await.unwrap();
        socks.push(s);
    }
    let live = |v: &Value| -> Vec<(String, bool)> {
        v["repos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["repo"].as_str().unwrap().to_owned(), r["live"] == true))
            .collect()
    };
    let w1 = text(&mut socks[0], 3000).await.expect("d1 hears where it stands");
    assert_eq!(w1["t"], "forge.watching");
    assert_eq!(
        live(&w1),
        vec![("acme/tool".into(), true), ("jhgaylor/hud".into(), true), ("nobody/x".into(), false)],
        "{w1}"
    );
    assert!(w1["repos"][2]["why"].as_str().unwrap().contains("isn't installed on nobody"), "{w1}");
    let w2 = text(&mut socks[1], 3000).await.unwrap();
    assert!(live(&w2).iter().all(|(_, l)| !l), "a stranger hears nothing: {w2}");
    assert!(w2["repos"][0]["why"].as_str().unwrap().contains("collaborator"), "{w2}");
    let w3 = text(&mut socks[2], 3000).await.unwrap();
    assert!(w3["repos"][0]["why"].as_str().unwrap().contains("sign in to control with GitHub"), "{w3}");
    // The access check minted a token for acme/tool, read-only, that one
    // repository.
    let minted = h.lock().unwrap().minted.clone();
    assert!(
        minted.iter().any(|m| m["installation"] == 12
            && m["body"]["repositories"] == json!(["tool"])
            && m["body"]["permissions"]["pull_requests"] == "read"),
        "{minted:?}"
    );
    assert!(minted.iter().all(|m| m["body"]["permissions"].as_object().unwrap().values().all(|v| v == "read")));

    // A signed event: only Jake's daemon hears, and only the poke.
    let pr = json!({ "action": "review_requested", "repository": { "full_name": "jhgaylor/hud" },
        "pull_request": { "number": 7, "title": "a private title", "body": "private words" } });
    let (st, v) = hook(&base, "pull_request", "del-1", &pr, Some(sig(&pr.to_string()))).await;
    assert_eq!((st, v["relayed"].as_u64()), (202, Some(1)), "{v}");
    let p = loop {
        let m = text(&mut socks[0], 3000).await.unwrap();
        if m["t"] == "forge.poke" {
            break m;
        }
    };
    assert_eq!(
        p["poke"],
        json!({ "provider": "github", "host": "github.com", "repo": "jhgaylor/hud", "number": 7,
            "event": "pull_request", "delivery": "del-1" })
    );
    assert!(!p.to_string().contains("private"));
    // GitHub redelivers: once is enough.
    let (st, v) = hook(&base, "pull_request", "del-1", &pr, Some(sig(&pr.to_string()))).await;
    assert_eq!((st, v["duplicate"].as_bool()), (200, Some(true)));
    // Bad, wrong-secret and missing signatures are refused.
    let (st, _) = hook(&base, "pull_request", "del-2", &pr, Some("sha256=00".into())).await;
    assert_eq!(st, 401);
    let (st, _) = hook(&base, "pull_request", "del-3", &pr, None).await;
    assert_eq!(st, 401);
    let tampered = json!({ "repository": { "full_name": "jhgaylor/hud" }, "pull_request": { "number": 8 } });
    let (st, _) = hook(&base, "pull_request", "del-4", &tampered, Some(sig(&pr.to_string()))).await;
    assert_eq!(st, 401);
    // A ping.
    let ping = json!({ "zen": "Design for failure.", "hook_id": 1 });
    let (st, v) = hook(&base, "ping", "del-5", &ping, Some(sig(&ping.to_string()))).await;
    assert_eq!((st, v["pong"].as_bool()), (200, Some(true)));
    // A check run on an org's repository Jake collaborates on: his daemon,
    // for both PRs it names.
    let run = json!({ "repository": { "full_name": "acme/tool" },
        "check_run": { "head_sha": "abc", "pull_requests": [{ "number": 3 }, { "number": 5 }] } });
    let (st, v) = hook(&base, "check_run", "del-6", &run, Some(sig(&run.to_string()))).await;
    assert_eq!((st, v["relayed"].as_u64()), (202, Some(2)));
    let mut got = vec![];
    while got.len() < 2 {
        let m = text(&mut socks[0], 3000).await.unwrap();
        if m["t"] == "forge.poke" {
            got.push(m["poke"]["number"].as_u64().unwrap());
        }
    }
    assert_eq!(got, vec![3, 5]);
    // Nothing reached the others (beyond heartbeats).
    for s in &mut socks[1..] {
        while let Some(m) = text(s, 300).await {
            assert_eq!(m["t"], "forge.watching", "{m}");
        }
    }
    // The heartbeat comes again.
    let mut beat = false;
    for _ in 0..5 {
        if let Some(m) = text(&mut socks[0], 1500).await
            && m["t"] == "forge.watching"
        {
            beat = true;
            break;
        }
    }
    assert!(beat, "no heartbeat");

    // A hosted box reads through the App: a token for one repository.
    let ask = |k: &DeviceKeys, repo: &str| {
        reqwest::Client::new()
            .post(format!("{base}/api/daemon/github/token"))
            .header("x-illogical-auth", sign_header(k, "POST", "/api/daemon/github/token"))
            .json(&json!({ "repo": repo }))
            .send()
    };
    let r = ask(&k1, "jhgaylor/hud").await.unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!((v["token"].as_str(), v["login"].as_str()), (Some("ghs_test_11"), Some("jhgaylor")));
    assert_eq!(v["expires_at"], "2099-01-01T00:00:00Z");
    assert!(
        h.lock().unwrap().minted.iter().any(|m| m["installation"] == 11 && m["body"]["repositories"] == json!(["hud"]))
    );
    assert_eq!(ask(&k2, "jhgaylor/hud").await.unwrap().status(), 403);
    assert_eq!(ask(&k3, "jhgaylor/hud").await.unwrap().status(), 403);
    assert_eq!(ask(&k1, "nobody/x").await.unwrap().status(), 403);
    // A login that changed hands gets nothing: access is by GitHub's id.
    let r = ask(&k4, "jhgaylor/hud").await.unwrap();
    assert_eq!(r.status(), 403);
    let r = ask(&k4, "acme/tool").await.unwrap();
    assert_eq!(r.status(), 403);
    assert!(r.text().await.unwrap().contains("isn't yours any more"));
    let unsigned = reqwest::Client::new()
        .post(format!("{base}/api/daemon/github/token"))
        .json(&json!({ "repo": "jhgaylor/hud" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), 401);

    // Jake's daemon hangs up: its subscription goes with the socket.
    let s = socks.remove(0);
    drop(s);
    for _ in 0..50 {
        if app.forge.route("jhgaylor/hud").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(app.forge.route("jhgaylor/hud").is_empty());
    let pr2 = json!({ "repository": { "full_name": "jhgaylor/hud" }, "pull_request": { "number": 7 } });
    let (_, v) = hook(&base, "pull_request", "del-7", &pr2, Some(sig(&pr2.to_string()))).await;
    assert_eq!(v["relayed"], 0);
}

/// By hand, once, read-only: the real App (`ILLOGICAL_REAL_GITHUB_APP_ENV`
/// names its env file, as the manifest flow left it): a JWT, the App's
/// installations, an installation token for one repository, and a PR read
/// with it. Prints no secret.
/// `ILLOGICAL_REAL_GITHUB_APP_ENV=… ILLOGICAL_REAL_REPO=o/r ILLOGICAL_REAL_PR=N cargo test -p illogical-control real_app -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn real_app_reads() {
    let env = std::fs::read_to_string(std::env::var("ILLOGICAL_REAL_GITHUB_APP_ENV").unwrap()).unwrap();
    let get = |k: &str| {
        env.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(|v| v.trim().trim_matches('"').to_owned())
    };
    let pem = std::fs::read_to_string(get("GITHUB_APP_PRIVATE_KEY_FILE").unwrap()).unwrap();
    let app = forge::GithubApp::new(
        get("GITHUB_APP_ID").unwrap(),
        get("GITHUB_APP_SLUG").unwrap(),
        String::new(),
        "https://api.github.com",
        forge::parse_pem(&pem).unwrap(),
    );
    let http = reqwest::Client::new();
    let jwt = app.jwt(now_ms() / 1000).unwrap();
    let r = http
        .get("https://api.github.com/app/installations")
        .bearer_auth(&jwt)
        .header("User-Agent", "illogical-control")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .unwrap();
    println!("GET /app/installations: {}", r.status());
    let v: Value = r.json().await.unwrap();
    for i in v.as_array().into_iter().flatten() {
        println!("  installation {} on {} ({})", i["id"], i["account"]["login"], i["repository_selection"]);
    }
    let repo = std::env::var("ILLOGICAL_REAL_REPO").unwrap();
    let pr = std::env::var("ILLOGICAL_REAL_PR").unwrap();
    let install = app.installation(&http, &repo).await.unwrap().expect("installed there");
    println!("installation for {repo}: {} on {}", install.id, install.account);
    let owner = repo.split('/').next().unwrap();
    println!(
        "may {owner} hear {repo}: {:?}",
        app.may(&http, &forge::GithubUser { id: install.account_id, login: owner.into() }, &repo).await.map(|i| i.id)
    );
    let (token, expires) = app.token(&http, install.id, repo.split('/').nth(1).unwrap()).await.unwrap();
    println!("installation token minted, {} chars, expires {expires}", token.len());
    let r = http
        .get(format!("https://api.github.com/repos/{repo}/pulls/{pr}"))
        .bearer_auth(&token)
        .header("User-Agent", "illogical-control")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .unwrap();
    let status = r.status();
    let v: Value = r.json().await.unwrap();
    println!("GET repos/{repo}/pulls/{pr}: {status}: {:?} ({}), head {}", v["title"], v["state"], v["head"]["sha"]);
}
