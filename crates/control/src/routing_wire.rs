//! Control's routes as daemons and people use them, over HTTP: what a
//! daemon's signature covers, joining with a machine's key, which accounts
//! control routes to a daemon, rosters, and the relay socket's limits.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use illogical_e2e::{
    Cert, DeviceKeys, Kind, now_ms,
    team::{Member, Roster, TeamRole},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

use crate::{App, auth::hash, db::Team};

struct Control {
    app: Arc<App>,
    base: String,
    http: reqwest::Client,
}

async fn control(tweak: impl FnOnce(&mut App)) -> Control {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let mut app = App::for_tests(&base);
    tweak(&mut app);
    let app = Arc::new(app);
    let svc = crate::router(app.clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(async move { axum::serve(l, svc).await.unwrap() });
    Control { app, base, http: reqwest::Client::new() }
}

/// An account with an approved first device (its root).
fn person(app: &App, login: &str, id: &str) -> DeviceKeys {
    app.db.account_for("github", &format!("gh-{id}"), login, id, now_ms()).unwrap();
    let keys = DeviceKeys::generate();
    let mut c = Cert::new(&keys, id, Kind::Browser, "laptop");
    c.sign_with(&keys);
    app.db.put_device(&c, true, now_ms()).unwrap();
    keys
}

/// A daemon joined to `account`.
fn daemon(app: &App, account: &str, name: &str) -> DeviceKeys {
    let keys = DeviceKeys::generate();
    let cert = Cert::new(&keys, account, Kind::Daemon, name);
    app.db.put_device(&cert, true, now_ms()).unwrap();
    app.db.put_daemon(account, &cert.device, name, &[]).unwrap();
    keys
}

/// A signed-in session's cookie.
fn session(app: &App, account: &str) -> String {
    let t = crate::auth::token();
    app.db.add_session(&hash(&t), account, now_ms(), now_ms() + 3_600_000).unwrap();
    format!("{}={t}", crate::auth::SESSION_COOKIE)
}

fn v2(keys: &DeviceKeys, method: &str, path_and_query: &str, body: &[u8]) -> String {
    let ms = now_ms();
    let nonce = hex::encode(illogical_e2e::random::<16>());
    let msg = crate::auth::daemon_auth_message_v2(method, path_and_query, ms, &nonce, body);
    format!("v2 {} {ms} {nonce} {}", keys.id(), hex::encode(keys.signature(msg.as_bytes())))
}

fn v1(keys: &DeviceKeys, method: &str, path: &str) -> String {
    let ms = now_ms();
    let msg = crate::auth::daemon_auth_message(method, path, ms);
    format!("{} {ms} {}", keys.id(), hex::encode(keys.signature(msg.as_bytes())))
}

impl Control {
    async fn daemon_get(&self, keys: &DeviceKeys, pq: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("{}{pq}", self.base))
            .header("x-illogical-auth", v2(keys, "GET", pq, b""))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or_default())
    }

    async fn daemon_post(&self, keys: &DeviceKeys, path: &str, body: &Value) -> (u16, Value) {
        let b = serde_json::to_vec(body).unwrap();
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .header("x-illogical-auth", v2(keys, "POST", path, &b))
            .header("content-type", "application/json")
            .body(b)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or_default())
    }

    async fn as_person(&self, cookie: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .header("cookie", cookie)
            .header("origin", &self.base);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or_default())
    }
}

#[tokio::test]
async fn daemon_signatures_cover_the_request_and_are_good_once() {
    let c = control(|_| {}).await;
    person(&c.app, "jake", "a1");
    let d = daemon(&c.app, "a1", "geek");
    let get = |h: String, pq: &str| c.http.get(format!("{}{pq}", c.base)).header("x-illogical-auth", h).send();

    // Signed over the path and query: good once.
    let pq = "/api/daemon/trust?features=presigned-invites";
    let h = v2(&d, "GET", pq, b"");
    assert_eq!(get(h.clone(), pq).await.unwrap().status(), 200);
    let again = get(h, pq).await.unwrap();
    assert_eq!(again.status(), 401, "a copy of a request does nothing");
    assert!(again.text().await.unwrap().contains("used already"));
    // Another query than the one signed.
    let h = v2(&d, "GET", pq, b"");
    assert_eq!(get(h, "/api/daemon/trust?features=other").await.unwrap().status(), 401);

    // A body other than the one signed.
    let signed = serde_json::to_vec(&json!({ "accounts": [] })).unwrap();
    let h = v2(&d, "POST", "/api/daemon/access", &signed);
    let r = c
        .http
        .post(format!("{}/api/daemon/access", c.base))
        .header("x-illogical-auth", h)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&json!({ "accounts": ["someone"] })).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(c.daemon_post(&d, "/api/daemon/access", &json!({ "accounts": [] })).await.0, 200);

    // Another daemon's key, or no signature at all.
    let other = DeviceKeys::generate();
    let forged = v2(&other, "GET", pq, b"").replacen(&other.id(), &d.id(), 1);
    assert_eq!(get(forged, pq).await.unwrap().status(), 401);
    assert_eq!(c.http.get(format!("{}{pq}", c.base)).send().await.unwrap().status(), 401);

    // A daemon from before 0.17 signs the old way: taken, once each.
    let h = v1(&d, "GET", "/api/daemon/trust");
    assert_eq!(get(h.clone(), "/api/daemon/trust").await.unwrap().status(), 200);
    assert_eq!(get(h, "/api/daemon/trust").await.unwrap().status(), 401);

    // A control that refuses the old way says to update.
    let strict = control(|a| a.cfg.old_daemon_signatures = false).await;
    person(&strict.app, "jake", "a1");
    let d = daemon(&strict.app, "a1", "geek");
    let r = strict
        .http
        .get(format!("{}/api/daemon/trust", strict.base))
        .header("x-illogical-auth", v1(&d, "GET", "/api/daemon/trust"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 426);
    assert!(r.text().await.unwrap().contains("update illogical"));
    assert_eq!(strict.daemon_get(&d, "/api/daemon/trust").await.0, 200);
    let ctl: Value =
        strict.http.get(format!("{}/control.json", strict.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(ctl["daemon_auth"], 2);
}

/// A join request as a daemon sends it; `proof` signs it with its key.
fn join_body(keys: &DeviceKeys, proof: bool) -> Value {
    let ask = Cert { account: String::new(), ..Cert::new(keys, "", Kind::Daemon, "box") };
    let mut b = json!({ "cert": ask, "urls": [], "features": "presigned-invites" });
    if proof {
        let ms = now_ms();
        let sig = hex::encode(keys.signature(illogical_e2e::cert::join_proof_body(&ask, ms).as_bytes()));
        b["proof"] = json!({ "ms": ms, "sig": sig });
    }
    b
}

#[tokio::test]
async fn joining_again_needs_the_machines_key() {
    let c = control(|_| {}).await;
    let root = person(&c.app, "jake", "a1");
    person(&c.app, "mallory", "a2");
    let joined = daemon(&c.app, "a1", "geek");
    let post = |b: Value| c.http.post(format!("{}/api/join", c.base)).json(&b).send();

    // Someone with only a joined machine's certificate can't ask for it.
    let r = post(join_body(&joined, false)).await.unwrap();
    assert_eq!(r.status(), 426);
    assert!(r.text().await.unwrap().contains("update illogical"));
    let mut bad = join_body(&joined, true);
    bad["proof"]["sig"] = json!(hex::encode(DeviceKeys::generate().signature(b"x")));
    assert_eq!(post(bad).await.unwrap().status(), 401);
    // Its holder can (to move it, say), with a fresh proof each time.
    let proven = join_body(&joined, true);
    assert_eq!(post(proven.clone()).await.unwrap().status(), 200);
    assert_eq!(post(proven).await.unwrap().status(), 401, "a proof is good once");
    // A new machine on an older illogical still joins.
    let fresh = DeviceKeys::generate();
    assert_eq!(post(join_body(&fresh, false)).await.unwrap().status(), 200);

    // A code asked for without the key, for a machine that became known
    // since, isn't approved: nothing of the joined one changes.
    let other = DeviceKeys::generate();
    let ask = Cert { account: String::new(), ..Cert::new(&other, "", Kind::Daemon, "box") };
    let code = illogical_e2e::cert::join_code(&ask);
    c.app.db.add_join(&code, &ask, &hash("p"), &[], None, None, "", false, now_ms()).unwrap();
    c.app.db.put_device(&Cert::new(&other, "a1", Kind::Daemon, "box"), true, now_ms()).unwrap();
    c.app.db.put_daemon("a1", &other.id(), "box", &[]).unwrap();
    let mut approval = Cert { account: "a1".into(), ..ask.clone() };
    approval.sign_with(&root);
    let cookie = session(&c.app, "a1");
    let (st, _) =
        c.as_person(&cookie, "POST", &format!("/api/joins/{code}/approve"), Some(json!({ "cert": approval }))).await;
    assert_eq!(st, 409);
    assert_eq!(c.app.db.daemon_row(&other.id()).unwrap().unwrap().0, "a1");
}

fn roster(team: &str, version: u64, members: Vec<Member>, by: &DeviceKeys) -> Roster {
    let mut r = Roster {
        v: 1,
        team: team.into(),
        name: "Acme".into(),
        version,
        at: now_ms(),
        members,
        spent: vec![],
        redeem: None,
        by: String::new(),
        sig: String::new(),
    };
    r.sign_with(by);
    r
}

fn member(account: &str, root: &DeviceKeys, role: TeamRole) -> Member {
    Member { account: account.into(), root: root.id(), role, name: account.into() }
}

#[tokio::test]
async fn control_routes_a_daemon_only_to_accounts_with_a_say_in_it() {
    let c = control(|_| {}).await;
    let owner_root = person(&c.app, "owner", "own1");
    let mate_root = person(&c.app, "mate", "mate1");
    person(&c.app, "victim", "vic1");
    person(&c.app, "other", "oth1");
    let d = daemon(&c.app, "own1", "box");
    // The owner and a teammate share a team.
    let team = "0123456789abcdef";
    let r = roster(
        team,
        1,
        vec![member("own1", &owner_root, TeamRole::Owner), member("mate1", &mate_root, TeamRole::Editor)],
        &owner_root,
    );
    c.app
        .db
        .add_team(
            &Team {
                id: team.into(),
                name: "Acme".into(),
                founder: "own1".into(),
                founder_root: owner_root.id(),
                locked: false,
            },
            1,
            &serde_json::to_string(&r).unwrap(),
            now_ms(),
        )
        .unwrap();
    for (a, e) in [("own1", "o"), ("mate1", "m"), ("vic1", "v"), ("oth1", "x")] {
        let body = json!({ "account": a, "endpoint": format!("https://fcm.googleapis.com/fcm/send/{e}") });
        c.app.db.put_push_sub(&format!("https://fcm.googleapis.com/fcm/send/{e}"), a, &body.to_string()).unwrap();
    }

    // The daemon says it lets in its owner's teammate and two strangers.
    let (st, _) = c.daemon_post(&d, "/api/daemon/access", &json!({ "accounts": ["mate1", "vic1", "oth1"] })).await;
    assert_eq!(st, 200);
    let subs = |v: &Value| -> Vec<String> {
        let mut a: Vec<String> =
            v["subs"].as_array().unwrap().iter().map(|s| s["account"].as_str().unwrap().to_owned()).collect();
        a.sort();
        a
    };
    let (_, v) = c.daemon_get(&d, "/api/daemon/push-subs").await;
    assert_eq!(subs(&v), vec!["mate1", "own1"], "not strangers' subscriptions");
    let (st, _) = c
        .daemon_post(
            &d,
            "/api/daemon/push",
            &json!({ "endpoint": "https://fcm.googleapis.com/fcm/send/v", "body": "AAAA" }),
        )
        .await;
    assert_eq!(st, 403, "no notification to someone who never accepted");
    // Certificates only for those, and the others become offers.
    let (_, v) = c.daemon_get(&d, "/api/daemon/peers?accounts=own1,mate1,vic1,oth1,nobody").await;
    let mut got: Vec<&String> = v.as_object().unwrap().keys().collect();
    got.sort();
    assert_eq!(got, vec!["mate1", "own1"]);
    let vic = session(&c.app, "vic1");
    let (_, dir) = c.as_person(&vic, "GET", "/api/directory", None).await;
    assert_eq!(dir["daemons"], json!([]), "not listed for a stranger");
    assert_eq!(dir["offers"][0]["daemon"], d.id());
    assert_eq!(dir["offers"][0]["owner_login"], "owner");
    assert!(!crate::teams::may_reach(&c.app, "vic1", &d.id()).unwrap());
    let mate = session(&c.app, "mate1");
    let (_, dir) = c.as_person(&mate, "GET", "/api/directory", None).await;
    assert_eq!(dir["daemons"][0]["id"], d.id(), "a teammate needs no prompt");
    assert_eq!(dir["offers"], json!([]));

    // Accepted: listed, reachable, its notifications and certificates.
    let (st, _) = c.as_person(&vic, "POST", &format!("/api/shares/{}", d.id()), Some(json!({ "accept": true }))).await;
    assert_eq!(st, 200);
    let (_, dir) = c.as_person(&vic, "GET", "/api/directory", None).await;
    assert_eq!(dir["daemons"][0]["id"], d.id());
    assert_eq!(dir["offers"], json!([]));
    assert!(crate::teams::may_reach(&c.app, "vic1", &d.id()).unwrap());
    let (_, v) = c.daemon_get(&d, "/api/daemon/push-subs").await;
    assert_eq!(subs(&v), vec!["mate1", "own1", "vic1"]);
    let (_, v) = c.daemon_get(&d, "/api/daemon/peers?accounts=vic1,oth1").await;
    assert!(v.get("vic1").is_some() && v.get("oth1").is_none());

    // Turned down: nothing, and no more prompt.
    let oth = session(&c.app, "oth1");
    let (st, _) = c.as_person(&oth, "POST", &format!("/api/shares/{}", d.id()), Some(json!({ "accept": false }))).await;
    assert_eq!(st, 200);
    let (_, dir) = c.as_person(&oth, "GET", "/api/directory", None).await;
    assert_eq!((dir["daemons"].clone(), dir["offers"].clone()), (json!([]), json!([])));
    // Nobody accepts what wasn't offered.
    let lone = daemon(&c.app, "own1", "quiet");
    let (st, _) =
        c.as_person(&oth, "POST", &format!("/api/shares/{}", lone.id()), Some(json!({ "accept": true }))).await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn rosters_add_only_people_who_asked() {
    let c = control(|_| {}).await;
    let owner_root = person(&c.app, "owner", "own1");
    let vic_root = person(&c.app, "victim", "vic1");
    let cookie = session(&c.app, "own1");
    let team = "fedcba9876543210";
    // A new team is its founder alone.
    let two = roster(
        team,
        1,
        vec![member("own1", &owner_root, TeamRole::Owner), member("vic1", &vic_root, TeamRole::Editor)],
        &owner_root,
    );
    let (st, _) = c.as_person(&cookie, "POST", "/api/teams", Some(json!({ "roster": two }))).await;
    assert_eq!(st, 400);
    let one = roster(team, 1, vec![member("own1", &owner_root, TeamRole::Owner)], &owner_root);
    assert_eq!(c.as_person(&cookie, "POST", "/api/teams", Some(json!({ "roster": one.clone() }))).await.0, 200);
    // Adding someone who didn't ask is refused; once they asked, it isn't.
    let add = roster(
        team,
        2,
        vec![member("own1", &owner_root, TeamRole::Owner), member("vic1", &vic_root, TeamRole::Editor)],
        &owner_root,
    );
    let path = format!("/api/teams/{team}/roster");
    let (st, _) = c.as_person(&cookie, "POST", &path, Some(json!({ "roster": add.clone() }))).await;
    assert_eq!(st, 403);
    let (_, inv) =
        c.as_person(&cookie, "POST", &format!("/api/teams/{team}/invites"), Some(json!({ "role": "editor" }))).await;
    let vic = session(&c.app, "vic1");
    let code = inv["code"].as_str().unwrap();
    assert_eq!(c.as_person(&vic, "POST", &format!("/api/invites/{team}/{code}/accept"), Some(json!({}))).await.0, 200);
    assert_eq!(c.as_person(&cookie, "POST", &path, Some(json!({ "roster": add }))).await.0, 200);
}

#[tokio::test]
async fn looking_people_up_is_rate_limited() {
    let c = control(|_| {}).await;
    person(&c.app, "jake", "a1");
    let cookie = session(&c.app, "a1");
    let mut last = 0;
    for _ in 0..=crate::limit::PEOPLE.1 {
        last = c.as_person(&cookie, "GET", "/api/people?login=nobody", None).await.0;
    }
    assert_eq!(last, 429);
}

#[tokio::test]
async fn a_daemons_relay_socket_takes_only_mux_sized_messages() {
    let c = control(|_| {}).await;
    person(&c.app, "jake", "a1");
    let d = daemon(&c.app, "a1", "geek");
    let mut req = format!("{}/api/relay/dial", c.base.replace("http://", "ws://")).into_client_request().unwrap();
    req.headers_mut().insert("x-illogical-auth", v2(&d, "GET", "/api/relay/dial", b"").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let _ = ws.send(Message::Binary(vec![0u8; 4 << 20].into())).await;
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(closed, "a 4 MB message closes the socket");
}

#[test]
fn join_proofs_and_signatures_are_what_daemons_sign() {
    // The daemon's side (crates/daemon) signs these same strings.
    let msg = crate::auth::daemon_auth_message_v2("POST", "/api/x?y=1", 5, "ab", b"{}");
    let body = hex::encode(Sha256::digest(b"{}"));
    assert_eq!(msg, format!("illogical daemon auth v2\nPOST\n/api/x?y=1\n5\nab\n{body}\n"));
}
