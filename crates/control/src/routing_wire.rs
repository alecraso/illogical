//! Control's routes as daemons and people use them, over HTTP: what a
//! daemon's signature covers, joining with a machine's key, which accounts
//! control routes to a daemon, rosters, and the relay socket's limits.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use illogical_e2e::{
    Cert, DeviceKeys, Kind, now_ms,
    team::{Invite, Member, Roster, TeamRole},
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

#[tokio::test]
async fn the_cli_joins_with_a_code_and_then_signs_as_its_account() {
    let c = control(|_| {}).await;
    let root = person(&c.app, "jake", "a1");
    daemon(&c.app, "a1", "geek");
    let cli = DeviceKeys::generate();
    let ask = Cert { account: String::new(), ..Cert::new(&cli, "", Kind::Cli, "illogical CLI on mini") };
    let post = |b: Value| c.http.post(format!("{}/api/join", c.base)).json(&b).send();
    let proof = || {
        let ms = now_ms();
        json!({ "ms": ms, "sig": hex::encode(cli.signature(illogical_e2e::cert::join_proof_body(&ask, ms).as_bytes())) })
    };
    // Only with its key's proof, and nothing a machine asks for.
    assert_eq!(post(json!({ "cert": ask })).await.unwrap().status(), 400);
    assert_eq!(post(json!({ "cert": ask, "proof": proof(), "team": "t" })).await.unwrap().status(), 400);
    let r: Value = post(json!({ "cert": ask, "proof": proof() })).await.unwrap().json().await.unwrap();
    let code = r["code"].as_str().unwrap().to_owned();
    assert_eq!(code, illogical_e2e::cert::join_code(&ask));
    let poll = r["poll"].as_str().unwrap().to_owned();

    // Before it's approved its signature reaches nothing.
    assert_eq!(c.daemon_get(&cli, "/api/directory").await.0, 401);

    // Approved as a CLI, it's a device of the account, not a machine.
    let cookie = session(&c.app, "a1");
    let mut as_daemon = Cert { account: "a1".into(), kind: Kind::Daemon, ..ask.clone() };
    as_daemon.sign_with(&root);
    let (st, _) =
        c.as_person(&cookie, "POST", &format!("/api/joins/{code}/approve"), Some(json!({ "cert": as_daemon }))).await;
    assert_eq!(st, 400, "the approval can't change its kind");
    let mut approval = Cert { account: "a1".into(), ..ask.clone() };
    approval.sign_with(&root);
    let (st, _) =
        c.as_person(&cookie, "POST", &format!("/api/joins/{code}/approve"), Some(json!({ "cert": approval }))).await;
    assert_eq!(st, 200);
    assert!(c.app.db.daemon_row(&cli.id()).unwrap().is_none());
    let got: Value =
        c.http.get(format!("{}/api/join/{code}?poll={poll}", c.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(got["approved"], true);
    assert_eq!(got["trust"]["root"], root.id());

    // Its signed requests are a session for its account, with no cookie
    // and no origin.
    let (st, dir) = c.daemon_get(&cli, "/api/directory").await;
    assert_eq!(st, 200);
    assert_eq!(dir["daemons"][0]["name"], "geek");
    // It isn't a daemon, though.
    assert_eq!(c.daemon_get(&cli, "/api/daemon/trust").await.0, 401);

    // Revoked, it's refused.
    let rev = illogical_e2e::Revocation::new("a1", &cli.id(), &root);
    c.app.db.add_revocation(&rev).unwrap();
    assert_eq!(c.daemon_get(&cli, "/api/directory").await.0, 401);
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

    let subs = |v: &Value| -> Vec<String> {
        let mut a: Vec<String> =
            v["subs"].as_array().unwrap().iter().map(|s| s["account"].as_str().unwrap().to_owned()).collect();
        a.sort();
        a
    };
    // Until the daemon says whom it lets in, not even a teammate's: why it
    // says so before it fetches these (#232).
    let (_, v) = c.daemon_get(&d, "/api/daemon/push-subs").await;
    assert_eq!(subs(&v), vec!["own1"]);
    // The daemon says it lets in its owner's teammate and two strangers.
    let (st, _) = c.daemon_post(&d, "/api/daemon/access", &json!({ "accounts": ["mate1", "vic1", "oth1"] })).await;
    assert_eq!(st, 200);
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

/// A team "Acme" founded by `owner` (account `own1`), its first roster
/// alone.
fn acme(app: &App, team: &str, owner_root: &DeviceKeys) {
    let r = roster(team, 1, vec![member("own1", owner_root, TeamRole::Owner)], owner_root);
    let t = Team {
        id: team.into(),
        name: "Acme".into(),
        founder: "own1".into(),
        founder_root: owner_root.id(),
        locked: false,
    };
    app.db.add_team(&t, 1, &serde_json::to_string(&r).unwrap(), now_ms()).unwrap();
}

#[tokio::test]
async fn a_team_daemon_too_old_for_presigned_rosters_is_told_to_update() {
    let c = control(|_| {}).await;
    let owner_root = person(&c.app, "owner", "own1");
    let team = "00112233445566ff";
    acme(&c.app, team, &owner_root);
    let d = daemon(&c.app, "own1", "box");
    c.app.db.set_daemon_team(&d.id(), team).unwrap();
    let new = "/api/daemon/team?since=0&features=presigned-invites";
    let old = "/api/daemon/team?since=0&features=";
    // No presigned version yet: both get the team.
    assert_eq!(c.daemon_get(&d, old).await.0, 200);
    assert_eq!(c.daemon_get(&d, new).await.0, 200);
    // A version written with a presigned invite (v2, an invite spent).
    let mut v2 = roster(team, 2, vec![member("own1", &owner_root, TeamRole::Owner)], &owner_root);
    v2.v = 2;
    v2.spent = vec![illogical_e2e::team::Spent { key: "ab".repeat(32), expires: now_ms() + 3_600_000 }];
    v2.sign_with(&owner_root);
    c.app.db.add_roster(team, 2, &serde_json::to_string(&v2).unwrap()).unwrap();
    // A daemon downgraded since can't check it: it's told why, not handed
    // a roster it would stop at.
    let (st, v) = c.daemon_get(&d, old).await;
    assert_eq!(st, 409);
    assert!(v["error"].as_str().unwrap().contains("update illogical"), "{v}");
    let (st, v) = c.daemon_get(&d, "/api/daemon/team?since=1").await;
    assert_eq!(st, 409, "{v}");
    // One that understands them gets both versions.
    let (st, v) = c.daemon_get(&d, new).await;
    assert_eq!(st, 200);
    assert_eq!(v["rosters"].as_array().unwrap().len(), 2);
}

/// A presigned invite to `team` as an editor, signed by `owner`; and its
/// one-time key.
fn presigned(team: &str, owner: &DeviceKeys) -> (Invite, DeviceKeys) {
    let once = DeviceKeys::generate();
    let mut inv = Invite {
        team: team.into(),
        role: TeamRole::Editor,
        expires: now_ms() + 3_600_000,
        key: hex::encode(once.sign_public()),
        by: String::new(),
        sig: String::new(),
    };
    inv.sign_with(owner);
    (inv, once)
}

#[tokio::test]
async fn owners_list_and_cancel_presigned_invites() {
    let c = control(|_| {}).await;
    let owner_root = person(&c.app, "owner", "own1");
    let mate_root = person(&c.app, "mate", "mate1");
    let joiner_root = person(&c.app, "joiner", "join1");
    let team = "0123456789abcdee";
    // Alice's team, with a member who isn't an owner.
    let owner = session(&c.app, "own1");
    let one = roster(team, 1, vec![member("own1", &owner_root, TeamRole::Owner)], &owner_root);
    assert_eq!(c.as_person(&owner, "POST", "/api/teams", Some(json!({ "roster": one.clone() }))).await.0, 200);
    let mate = session(&c.app, "mate1");
    let (_, inv) =
        c.as_person(&owner, "POST", &format!("/api/teams/{team}/invites"), Some(json!({ "role": "editor" }))).await;
    let code = inv["code"].as_str().unwrap();
    c.as_person(&mate, "POST", &format!("/api/invites/{team}/{code}/accept"), Some(json!({}))).await;
    let two = roster(
        team,
        2,
        vec![member("own1", &owner_root, TeamRole::Owner), member("mate1", &mate_root, TeamRole::Viewer)],
        &owner_root,
    );
    let path = format!("/api/teams/{team}/roster");
    assert_eq!(c.as_person(&owner, "POST", &path, Some(json!({ "roster": two.clone() }))).await.0, 200);

    // Two presigned links.
    let (a, a_key) = presigned(team, &owner_root);
    let (b, _) = presigned(team, &owner_root);
    for inv in [&a, &b] {
        let (st, v) = c
            .as_person(
                &owner,
                "POST",
                &format!("/api/teams/{team}/invites"),
                Some(json!({ "role": "editor", "presigned": inv })),
            )
            .await;
        assert_eq!(st, 200, "{v}");
    }
    // Owners see them; nobody else does.
    let list = format!("/api/teams/{team}/presigned");
    let (st, v) = c.as_person(&owner, "GET", &list, None).await;
    assert_eq!(st, 200);
    let mut keys: Vec<&str> = v["invites"].as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap()).collect();
    keys.sort();
    let mut want = vec![a.key.as_str(), b.key.as_str()];
    want.sort();
    assert_eq!(keys, want);
    assert_eq!(v["invites"][0]["role"], "editor");
    assert_eq!(v["invites"][0]["by_name"], "owner");
    assert!(v["invites"][0].get("sig").is_none() && v["invites"][0].get("body").is_none());
    assert_eq!(c.as_person(&mate, "GET", &list, None).await.0, 403);
    let joiner = session(&c.app, "join1");
    assert_eq!(c.as_person(&joiner, "GET", &list, None).await.0, 403);

    // Only an owner cancels, only this team's, and only once.
    let cancel = format!("/api/teams/{team}/presigned/{}", a.key);
    assert_eq!(c.as_person(&mate, "DELETE", &cancel, None).await.0, 403);
    assert_eq!(
        c.as_person(&owner, "DELETE", &format!("/api/teams/{team}/presigned/{}", "cd".repeat(32)), None).await.0,
        404
    );
    assert_eq!(c.as_person(&owner, "DELETE", &cancel, None).await.0, 200);
    assert_eq!(c.as_person(&owner, "DELETE", &cancel, None).await.0, 404);
    let (_, v) = c.as_person(&owner, "GET", &list, None).await;
    assert_eq!(v["invites"].as_array().unwrap().len(), 1);
    assert_eq!(v["invites"][0]["key"], b.key.as_str());

    // The cancelled link is dead: its page has nothing, and redeeming it
    // is refused.
    let show = format!("/api/presigned/{team}/{}", a.key);
    assert_eq!(c.as_person(&joiner, "GET", &show, None).await.0, 404);
    let me = member("join1", &joiner_root, TeamRole::Editor);
    let mut three = two.clone();
    three.v = 2;
    three.version = 3;
    three.at = now_ms();
    three.members.push(me.clone());
    three.spent = vec![illogical_e2e::team::Spent { key: a.key.clone(), expires: a.expires }];
    three.redeem = Some(illogical_e2e::team::Redeem {
        invite: a.clone(),
        proof: hex::encode(a_key.signature(a.redeem_body(3, &me).as_bytes())),
    });
    three.by = a.key.clone();
    three.sig = hex::encode(a_key.signature(three.body().as_bytes()));
    assert!(three.follows(
        Some(&two),
        &illogical_e2e::team::TeamPin { team: team.into(), founder: "own1".into(), founder_root: owner_root.id() },
        &crate::teams::certs_for_test(&c.app, &["own1".into(), "mate1".into(), "join1".into()])
    ));
    let (st, v) = c.as_person(&joiner, "POST", &path, Some(json!({ "roster": three }))).await;
    assert_eq!(st, 410, "{v}");
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

/// A daemon's relay socket, as `illogicald` dials it.
async fn dial_relay(
    c: &Control,
    d: &DeviceKeys,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut req = format!("{}/api/relay/dial", c.base.replace("http://", "ws://")).into_client_request().unwrap();
    req.headers_mut().insert("x-illogical-auth", v2(d, "GET", "/api/relay/dial", b"").parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws
}

/// Whether the socket closes within two seconds (whatever else comes).
async fn closes<S>(ws: &mut S) -> bool
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn deleting_a_founder_hangs_up_the_teams_machines_and_tells_its_members() {
    // #206: Fran founds Acme; Mo (an owner) put his box in it, and Cy uses
    // it through the relay as a member.
    let c = control(|_| {}).await;
    let fran = person(&c.app, "fran", "f1");
    let mo = person(&c.app, "mo", "m1");
    let cy = person(&c.app, "cy", "c1");
    let team = "fedcba9876543210";
    let r = roster(
        team,
        1,
        vec![
            member("f1", &fran, TeamRole::Owner),
            member("m1", &mo, TeamRole::Owner),
            member("c1", &cy, TeamRole::Editor),
        ],
        &fran,
    );
    let t = Team { id: team.into(), name: "Acme".into(), founder: "f1".into(), founder_root: fran.id(), locked: false };
    c.app.db.add_team(&t, 1, &serde_json::to_string(&r).unwrap(), now_ms()).unwrap();
    let mos_box = daemon(&c.app, "m1", "mos-box");
    c.app.db.set_daemon_team(&mos_box.id(), team).unwrap();
    let mut boxes_socket = dial_relay(&c, &mos_box).await;
    for _ in 0..50 {
        if c.app.relay.online(&mos_box.id()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(c.app.relay.online(&mos_box.id()));
    let cys = session(&c.app, "c1");
    let to_box = |cookie: &str| {
        let mut req = format!("{}/api/relay/c/{}", c.base.replace("http://", "ws://"), mos_box.id())
            .into_client_request()
            .unwrap();
        req.headers_mut().insert("cookie", cookie.parse().unwrap());
        req.headers_mut().insert("origin", c.base.parse().unwrap());
        req
    };
    let (mut cys_socket, _) = tokio_tungstenite::connect_async(to_box(&cys)).await.unwrap();

    // Fran deletes her account, and Acme with it.
    let frans = session(&c.app, "f1");
    let (st, _) = c.as_person(&frans, "POST", "/api/me/delete", Some(json!({ "confirm": "fran" }))).await;
    assert_eq!(st, 200);
    // Both relay sockets end now, not at the box's next check.
    assert!(closes(&mut cys_socket).await, "Cy's relayed connection ends at once");
    assert!(closes(&mut boxes_socket).await, "the box is hung up on");
    // The box, still Mo's, dials back in and is told to check again first.
    let mut back = dial_relay(&c, &mos_box).await;
    let first = tokio::time::timeout(Duration::from_secs(2), back.next()).await.unwrap().unwrap().unwrap();
    assert_eq!(first, Message::Text(crate::relay::NUDGE.into()));
    // Cy isn't routed to it any more.
    let refused = tokio_tungstenite::connect_async(to_box(&cys)).await;
    assert!(refused.is_err(), "no route for a member of a team that's gone");

    // Both members see a notice in the app, once.
    for (cookie, who) in [(&cys, "Cy"), (&session(&c.app, "m1"), "Mo")] {
        let (_, v) = c.as_person(cookie, "GET", "/api/teams", None).await;
        assert_eq!(v["teams"], json!([]), "{who}");
        let n = &v["notices"][0];
        assert_eq!(
            (n["title"].as_str(), n["body"].as_str()),
            (Some("Acme was deleted"), Some("Its founder deleted their account.")),
            "{who}"
        );
        let seen = format!("/api/me/notices/{}/seen", n["id"]);
        assert_eq!(c.as_person(cookie, "POST", &seen, None).await.0, 200);
        assert_eq!(c.as_person(cookie, "POST", &seen, None).await.0, 404);
        let (_, v) = c.as_person(cookie, "GET", "/api/teams", None).await;
        assert_eq!(v["notices"], json!([]), "{who}");
    }
}

#[test]
fn join_proofs_and_signatures_are_what_daemons_sign() {
    // The daemon's side (crates/daemon) signs these same strings.
    let msg = crate::auth::daemon_auth_message_v2("POST", "/api/x?y=1", 5, "ab", b"{}");
    let body = hex::encode(Sha256::digest(b"{}"));
    assert_eq!(msg, format!("illogical daemon auth v2\nPOST\n/api/x?y=1\n5\nab\n{body}\n"));
}
