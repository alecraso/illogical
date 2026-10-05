//! #233: inviting a person into a session. `POST /api/invite` grants the
//! role and pushes that person alone, and says how it went from what the
//! subscriptions did: a tailnet guest through this daemon's own push, an
//! account through a control of the test's own (`Fake`), which routes,
//! refuses and relays as the real one does. Team pins come from the
//! owner's browser; the daemon checks the roster against the pin itself.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

mod agentd;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use agentd::*;
use base64::Engine;
use illogical_e2e::{
    Cert, DeviceKeys, Kind,
    push::PushSub,
    team::{AccountCerts, Member, Roster, TeamPin, TeamRole},
};
use serde_json::{Value, json};

const OWNER: &str = "me@example.com";
const FRIEND: &str = "friend@example.com";
const OTHER: &str = "other@example.com";

fn tailnet_daemon(env: &[(&str, &str)]) -> Daemon {
    let args = ["--owner", OWNER, "--tailscale-socket", "/nonexistent/sock", "--wisp-token-file", "/nonexistent"];
    Daemon::child_env(&args, env)
}

/// The first pane, and its session.
fn first(d: &Daemon) -> (u64, u64) {
    let p = &d.get("/api/panes")[0];
    (p["id"].as_u64().unwrap(), p["session"].as_u64().unwrap())
}

fn share(d: &Daemon, session: u64, login: &str, role: &str) {
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{login}"), "role": role }));
}

fn invite(d: &Daemon, body: Value) -> (u16, Value) {
    let (status, text) = d.raw("POST", "/api/invite", Some(body));
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

#[test]
fn a_re_invite_pushes_that_guest_alone() {
    let d = tailnet_daemon(&[]);
    let (pane, session) = first(&d);
    share(&d, session, FRIEND, "viewer");
    share(&d, session, OTHER, "editor");
    let owner = Phone::subscribe(&d);
    let friend = Phone::subscribe_as(&d, Some(FRIEND));
    let other = Phone::subscribe_as(&d, Some(OTHER));
    // Someone else here who asked to hear about everything (M29).
    let (status, _) = d.raw_as(OTHER, "POST", "/api/notify", Some(json!({ "on": true })));
    assert_eq!(status, 200);

    // The push service answers while the invite waits on it.
    let (msg, (status, r)) = std::thread::scope(|s| {
        let phone = s.spawn(|| friend.next());
        let r = invite(&d, json!({ "session": session, "who": FRIEND, "note": "take a look at the flaky test" }));
        (phone.join().unwrap(), r)
    });
    assert_eq!(status, 200, "{r}");
    assert_eq!((r["delivery"].as_str(), r["grant"]["granted"].as_bool()), (Some("sent"), Some(false)), "{r}");
    assert!(msg["tag"].as_str().unwrap().starts_with("invite-"), "{msg}");
    assert_eq!(msg["tag"], format!("invite-{}", r["invite"].as_str().unwrap()));
    assert_eq!(msg["pane"], pane, "opens at the session's first pane");
    assert!(msg["title"].as_str().unwrap().contains("brought you into"), "{msg}");
    assert_eq!(msg["body"], "take a look at the flaky test");
    // Exactly one, and to nobody else: not the owner, not someone who
    // opted in to everything.
    assert!(friend.quiet(1500), "one push");
    assert!(owner.quiet(200) && other.quiet(200), "nobody else");

    // A push service that doesn't take it: not sent, though it was tried.
    drop(friend);
    let (status, r) = invite(&d, json!({ "session": session, "who": FRIEND }));
    assert_eq!(status, 200, "{r}");
    assert_eq!(
        (r["delivery"].as_str(), r["reason"].as_str()),
        (Some("unreachable"), Some("their push service turned it down"))
    );
}

#[test]
fn a_fresh_guest_is_granted_and_unreachable() {
    let d = tailnet_daemon(&[]);
    let (_, session) = first(&d);
    let (status, r) = invite(&d, json!({ "session": session, "who": format!("tailnet:{FRIEND}"), "role": "editor" }));
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["delivery"], "unreachable");
    assert_eq!(r["reason"], "they haven't turned on notifications here");
    assert_eq!(r["grant"]["granted"], true);
    let grants = d.get("/api/acl")["grants"].clone();
    let g = grants.as_array().unwrap().iter().find(|g| g["principal"] == format!("tailnet:{FRIEND}")).unwrap();
    assert_eq!((g["session"].as_u64(), g["role"].as_str()), (Some(session), Some("editor")));
    assert!(g["from"].is_object(), "from now on, by default: {g}");
}

#[test]
fn who_may_invite_whom_and_as_what() {
    let d = tailnet_daemon(&[]);
    let (_, session) = first(&d);
    share(&d, session, FRIEND, "editor");

    // Only the owner invites.
    let (status, body) = d.raw_as(FRIEND, "POST", "/api/invite", Some(json!({ "session": session, "who": OTHER })));
    assert_eq!(status, 403, "{body}");
    let (status, _) = d.raw_as(FRIEND, "POST", "/api/team-pins", Some(json!({ "pins": {} })));
    assert_eq!(status, 403);
    // Never as an owner.
    let (status, r) = invite(&d, json!({ "session": session, "who": format!("tailnet:{OTHER}"), "role": "owner" }));
    assert_eq!(status, 400, "{r}");
    // A stranger isn't nameable.
    let (status, r) = invite(&d, json!({ "session": session, "who": "sam" }));
    assert_eq!(status, 404, "{r}");
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("share once from the web (it checks their fingerprint), then invite works")
    );
    // Not lower than they hold.
    let (status, r) = invite(&d, json!({ "session": session, "who": FRIEND, "role": "viewer" }));
    assert_eq!(status, 409, "{r}");
    assert!(r["error"].as_str().unwrap().contains("revoke first"), "{r}");
    // Driving is an editor's.
    let (status, _) = invite(&d, json!({ "session": session, "who": FRIEND, "drive_minutes": 5 }));
    assert_eq!(status, 400);
    let (status, _) = invite(&d, json!({ "session": session, "who": FRIEND, "role": "editor", "drive_minutes": 0 }));
    assert_eq!(status, 400);
    // A pane elsewhere.
    let (status, _) = invite(&d, json!({ "session": session, "who": FRIEND, "role": "editor", "pane": 999 }));
    assert_eq!(status, 404);
    // Nothing was granted by any of that.
    let grants = d.get("/api/acl")["grants"].clone();
    assert_eq!(grants.as_array().unwrap().len(), 1, "{grants}");
}

#[test]
fn drive_trusts_a_guest_for_so_long() {
    // A minute of trust is a second and a half here.
    let d = tailnet_daemon(&[("ILLOGICAL_TRUST_MINUTE_MS", "1500")]);
    let (pane, session) = first(&d);
    let send = || d.raw_as(FRIEND, "POST", &format!("/api/panes/{pane}/send"), Some(json!({ "text": "true" }))).0;
    share(&d, session, FRIEND, "editor");
    assert_eq!(send(), 403, "the owner's own machine");
    let (status, r) = invite(&d, json!({ "session": session, "who": FRIEND, "role": "editor", "drive_minutes": 1 }));
    assert_eq!(status, 200, "{r}");
    assert_eq!((r["drive"].as_bool(), r["grant"]["granted"].as_bool()), (Some(true), Some(false)));
    assert_eq!(send(), 200);
    std::thread::sleep(Duration::from_millis(1700));
    assert_eq!(send(), 403, "expired");
}

// ---------------------------------------------------------------- control

/// What the test's control knows. It routes to the daemon only the
/// accounts in `routed` (as if teammates, or they accepted), hands over
/// subscriptions only of those it routes and the daemon let in, and
/// refuses to relay to anyone else (or to anyone, with `refuse`).
#[derive(Default)]
struct Fake {
    own: Vec<Cert>,
    /// The daemon's own team (a team daemon): its rosters and certificates.
    team: Option<(Vec<Roster>, AccountCerts)>,
    /// Teams it may ask about.
    teams: BTreeMap<String, (Vec<Roster>, AccountCerts)>,
    people: BTreeMap<String, (String, Cert)>,
    routed: BTreeSet<String>,
    published: Vec<String>,
    subs: Vec<PushSub>,
    refuse: bool,
    relayed: Vec<(String, Vec<u8>)>,
}

type Shared = Arc<Mutex<Fake>>;

fn serve(fake: Shared) -> String {
    use axum::{
        Json, Router,
        extract::{Query, State},
        http::StatusCode,
        routing::{get, post},
    };
    type Q = Query<BTreeMap<String, String>>;
    let app =
        Router::new()
            .route(
                "/api/daemon/trust",
                get(|State(f): State<Shared>| async move {
                    Json(json!({ "certs": f.lock().unwrap().own, "revocations": [] }))
                }),
            )
            .route(
                "/api/daemon/team",
                get(|State(f): State<Shared>, Query(q): Q| async move {
                    let since: u64 = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
                    let f = f.lock().unwrap();
                    let (rosters, certs) = f.team.clone().unwrap_or_default();
                    let rosters: Vec<Roster> = rosters.into_iter().filter(|r| r.version > since).collect();
                    Json(json!({ "locked": false, "rosters": rosters, "certs": certs }))
                }),
            )
            .route(
                "/api/daemon/teams",
                get(|State(f): State<Shared>, Query(q): Q| async move {
                    let f = f.lock().unwrap();
                    let mut out = serde_json::Map::new();
                    for id in q.get("ids").map(String::as_str).unwrap_or("").split(',') {
                        if let Some((rosters, certs)) = f.teams.get(id) {
                            out.insert(id.into(), json!({ "locked": false, "rosters": rosters, "certs": certs }));
                        }
                    }
                    Json(Value::Object(out))
                }),
            )
            .route(
                "/api/daemon/peers",
                get(|State(f): State<Shared>, Query(q): Q| async move {
                    let f = f.lock().unwrap();
                    let mut out = serde_json::Map::new();
                    for a in q.get("accounts").map(String::as_str).unwrap_or("").split(',') {
                        if let Some((name, root)) = f.people.get(a).filter(|_| a == "a1" || f.routed.contains(a)) {
                            out.insert(a.into(), json!({ "name": name, "certs": [root], "revocations": [] }));
                        }
                    }
                    Json(Value::Object(out))
                }),
            )
            .route(
                "/api/daemon/access",
                post(|State(f): State<Shared>, Json(b): Json<Value>| async move {
                    f.lock().unwrap().published = serde_json::from_value(b["accounts"].clone()).unwrap();
                    Json(json!({}))
                }),
            )
            .route(
                "/api/daemon/push-subs",
                get(|State(f): State<Shared>| async move {
                    let f = f.lock().unwrap();
                    let served = |a: &String| f.routed.contains(a) && f.published.contains(a);
                    let subs: Vec<&PushSub> = f.subs.iter().filter(|s| served(&s.account)).collect();
                    Json(json!({ "subs": subs }))
                }),
            )
            .route(
                "/api/daemon/push",
                post(|State(f): State<Shared>, Json(b): Json<Value>| async move {
                    let mut f = f.lock().unwrap();
                    let endpoint = b["endpoint"].as_str().unwrap_or_default().to_owned();
                    let Some(account) = f.subs.iter().find(|s| s.endpoint == endpoint).map(|s| s.account.clone())
                    else {
                        return StatusCode::NOT_FOUND;
                    };
                    if f.refuse || !(f.routed.contains(&account) && f.published.contains(&account)) {
                        return StatusCode::FORBIDDEN;
                    }
                    let body = base64::engine::general_purpose::STANDARD.decode(b["body"].as_str().unwrap()).unwrap();
                    f.relayed.push((endpoint, body));
                    StatusCode::OK
                }),
            )
            .with_state(fake);
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::from_std(l).unwrap();
            axum::serve(l, app).await.unwrap();
        });
    });
    url
}

fn device(account: &str) -> (DeviceKeys, Cert) {
    let keys = DeviceKeys::generate();
    let mut cert = Cert::new(&keys, account, Kind::Browser, "x");
    cert.sign_with(&keys);
    (keys, cert)
}

/// A person with an account on the test's control, and a phone.
struct Person {
    account: &'static str,
    name: &'static str,
    keys: DeviceKeys,
    root: Cert,
    phone: p256::SecretKey,
}

impl Person {
    fn new(account: &'static str, name: &'static str, n: u8) -> Self {
        let (keys, root) = device(account);
        Self { account, name, keys, root, phone: p256::SecretKey::from_slice(&[n; 32]).unwrap() }
    }

    fn endpoint(&self) -> String {
        format!("https://push.test/{}", self.account)
    }

    fn sub(&self) -> PushSub {
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mut s = PushSub {
            v: 1,
            account: self.account.into(),
            device: String::new(),
            endpoint: self.endpoint(),
            p256dh: b64.encode(self.phone.public_key().to_sec1_bytes()),
            auth: b64.encode([7u8; 16]),
            at: illogical_e2e::now_ms(),
            sig: String::new(),
        };
        s.sign_with(&self.keys);
        s
    }

    fn member(&self, role: TeamRole) -> Member {
        Member { account: self.account.into(), root: self.root.device.clone(), role, name: self.name.into() }
    }

    /// What their phone got, through control.
    fn got(&self, fake: &Shared) -> Vec<Value> {
        let f = fake.lock().unwrap();
        f.relayed
            .iter()
            .filter(|(e, _)| *e == self.endpoint())
            .map(|(_, b)| open_push(&self.phone, &[7u8; 16], b))
            .collect()
    }
}

/// Alex (account `a1`) owns the machine; Bea, Cy and Dee are in Alex's
/// team `t1` (Bea and Dee with phones); `t2` claims Alex founded it, but
/// someone else signed its roster.
struct World {
    fake: Shared,
    url: String,
    alex_root: Cert,
    daemon: DeviceKeys,
    daemon_cert: Cert,
    bea: Person,
    cy: Person,
    dee: Person,
    t1: (Vec<Roster>, AccountCerts),
}

fn world() -> World {
    let (alex, alex_root) = device("a1");
    let daemon = DeviceKeys::generate();
    let mut daemon_cert = Cert::new(&daemon, "a1", Kind::Daemon, "box");
    daemon_cert.sign_with(&alex);
    let (bea, cy, dee) = (Person::new("b1", "bea", 7), Person::new("c1", "cy", 8), Person::new("d1", "dee", 9));
    let owner =
        Member { account: "a1".into(), root: alex_root.device.clone(), role: TeamRole::Owner, name: "alex".into() };
    let mut roster = Roster {
        v: 1,
        team: "t1".into(),
        name: "Acme".into(),
        version: 1,
        at: 1,
        members: vec![
            owner.clone(),
            bea.member(TeamRole::Editor),
            cy.member(TeamRole::Viewer),
            dee.member(TeamRole::Viewer),
        ],
        spent: vec![],
        redeem: None,
        by: String::new(),
        sig: String::new(),
    };
    roster.sign_with(&alex);
    let mut certs: AccountCerts = AccountCerts::new();
    certs.insert("a1".into(), (vec![alex_root.clone()], vec![]));
    for p in [&bea, &cy, &dee] {
        certs.insert(p.account.into(), (vec![p.root.clone()], vec![]));
    }
    let (mallory, _) = device("m1");
    let mut forged = roster.clone();
    forged.team = "t2".into();
    forged.sign_with(&mallory);

    let fake: Shared = Arc::new(Mutex::new(Fake {
        own: vec![alex_root.clone(), daemon_cert.clone()],
        subs: vec![bea.sub(), dee.sub()],
        ..Default::default()
    }));
    {
        let mut f = fake.lock().unwrap();
        f.people.insert("a1".into(), ("alex".into(), alex_root.clone()));
        for p in [&bea, &cy, &dee] {
            f.people.insert(p.account.into(), (p.name.into(), p.root.clone()));
        }
        f.teams.insert("t1".into(), (vec![roster.clone()], certs.clone()));
        f.teams.insert("t2".into(), (vec![forged], certs.clone()));
    }
    let url = serve(fake.clone());
    World { fake, url, alex_root, daemon, daemon_cert, bea, cy, dee, t1: (vec![roster], certs) }
}

impl World {
    /// `t1`'s pin, as Alex's browser holds it.
    fn pin(&self) -> String {
        format!("a1.{}", self.alex_root.device)
    }

    /// A daemon joined to Alex's account (or, with `team`, to `t1`).
    fn daemon(&self, team: bool) -> Daemon {
        let saved = json!({
            "url": self.url,
            "trust": { "account": "a1", "root": self.alex_root.device },
            "cert": self.daemon_cert,
            "certs": [self.alex_root],
            "revocations": [],
            "team": team.then(|| TeamPin { team: "t1".into(), founder: "a1".into(), founder_root: self.alex_root.device.clone() }),
        });
        Daemon::child_in(&["--wisp-token-file", "/nonexistent"], &[], |state| {
            std::fs::write(state.join("control.json"), saved.to_string()).unwrap();
            self.daemon.save(&state.join("daemon.key")).unwrap();
        })
    }

    fn route(&self, who: &Person) {
        self.fake.lock().unwrap().routed.insert(who.account.into());
    }
}

/// Invite, once the daemon has had its first look at control (until then
/// nobody is known: a 404 changes nothing).
fn invite_when_known(d: &Daemon, body: Value) -> (u16, Value) {
    let mut out = invite(d, body.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while out.0 == 404 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
        out = invite(d, body.clone());
    }
    out
}

/// Something that makes the daemon refresh soon (a grant).
fn poke(d: &Daemon, session: u64) {
    share(d, session, "nobody@example.com", "viewer");
}

#[test]
fn team_pins_make_members_nameable_and_delivery_is_what_control_did() {
    let w = world();
    let d = w.daemon(false);
    let (pane, session) = first(&d);
    // Not before a pin: control's word isn't enough.
    std::thread::sleep(Duration::from_millis(500));
    let (status, r) = invite(&d, json!({ "session": session, "who": "bea" }));
    assert_eq!(status, 404, "{r}");

    // A pin whose roster doesn't check out: still nobody.
    let r = d.post("/api/team-pins", json!({ "pins": { "t2": w.pin() } }));
    assert_eq!(r["checked"], json!([]), "{r}");
    let (status, _) = invite(&d, json!({ "session": session, "who": "bea" }));
    assert_eq!(status, 404);

    // Alex's browser pins t1: its members are known, with no grant to t1.
    let r = d.post("/api/team-pins", json!({ "pins": { "t1": w.pin() } }));
    assert_eq!(r["checked"], json!(["t1"]), "{r}");
    assert!(d.get("/api/acl")["grants"].as_array().unwrap().is_empty());

    // Bea is a teammate (control routes her) with a phone: sent, within
    // one refresh, to her alone.
    w.route(&w.bea);
    w.route(&w.dee);
    let (status, r) = invite(&d, json!({ "session": session, "who": "bea", "note": "the flaky test" }));
    assert_eq!(status, 200, "{r}");
    assert_eq!((r["delivery"].as_str(), r["grant"]["principal"].as_str()), (Some("sent"), Some("account:b1")), "{r}");
    let grant = d.get("/api/acl")["grants"][0].clone();
    assert_eq!(grant["root"], w.bea.root.device, "the root the roster names");
    let got = w.bea.got(&w.fake);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(
        (got[0]["tag"].clone(), got[0]["pane"].as_u64()),
        (json!(format!("invite-{}", r["invite"].as_str().unwrap())), Some(pane))
    );
    assert_eq!(got[0]["body"], "the flaky test");
    assert!(w.dee.got(&w.fake).is_empty(), "nobody else");

    // Dee is reachable, with notifications off: unreachable.
    w.fake.lock().unwrap().subs.retain(|s| s.account != "d1");
    let (status, r) = invite(&d, json!({ "session": session, "who": "account:d1" }));
    assert_eq!(status, 200, "{r}");
    assert_eq!(
        (r["delivery"].as_str(), r["reason"].as_str()),
        (Some("unreachable"), Some("they haven't turned on notifications"))
    );

    // Cy isn't routed yet (hasn't accepted): pending, and it goes out after
    // a later refresh that finds him reachable.
    let (status, r) = invite(&d, json!({ "session": session, "who": "cy" }));
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["delivery"], "pending", "{r}");
    assert!(r["reason"].as_str().unwrap().contains("accepted"), "{r}");
    assert!(w.cy.got(&w.fake).is_empty());
    w.fake.lock().unwrap().subs.push(w.cy.sub());
    w.route(&w.cy);
    poke(&d, session);
    d.wait_for("the waiting invite", || w.cy.got(&w.fake).len() == 1);
    assert!(w.cy.got(&w.fake)[0]["tag"].as_str().unwrap().starts_with("invite-"));

    // Control refuses the relay: pending, never sent.
    w.fake.lock().unwrap().refuse = true;
    let (status, r) = invite(&d, json!({ "session": session, "who": "bea", "role": "editor" }));
    assert_eq!(status, 200, "{r}");
    assert_eq!((r["delivery"].as_str(), r["grant"]["granted"].as_bool()), (Some("pending"), Some(true)), "{r}");
    assert_eq!(w.bea.got(&w.fake).len(), 1);
    w.fake.lock().unwrap().refuse = false;
    poke(&d, session);
    d.wait_for("the refused invite, again", || w.bea.got(&w.fake).len() == 2);
    // Once out, it isn't sent again.
    poke(&d, session);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(w.bea.got(&w.fake).len(), 2);
}

#[test]
fn on_a_team_daemon_a_members_role_is_enough() {
    let w = world();
    w.fake.lock().unwrap().team = Some(w.t1.clone());
    w.route(&w.bea);
    let d = w.daemon(true);
    let (_, session) = first(&d);

    // Bea drives everything here already: no grant, just the push.
    let (status, r) =
        invite_when_known(&d, json!({ "session": session, "who": "bea", "role": "editor", "drive_minutes": 30 }));
    assert_eq!(status, 200, "{r}");
    assert_eq!(r["grant"]["granted"], false, "{r}");
    assert_eq!(r["drive"], false, "a team's machine: no one's trust needed");
    assert_eq!(r["delivery"], "sent", "{r}");
    assert!(d.get("/api/acl")["grants"].as_array().unwrap().is_empty());
    let panes = d.get("/api/panes");
    assert!(panes[0]["trusted"].as_array().is_none_or(|t| t.is_empty()), "{panes}");
    assert_eq!(w.bea.got(&w.fake).len(), 1);
    // The owner is the owner already.
    let (status, _) = invite(&d, json!({ "session": session, "who": "account:a1" }));
    assert_eq!(status, 404, "this account is never named");
}

/// #234, end to end: Claude Code in a terminal pane on Alex's own machine
/// (`illogical mcp` with that pane's `$ILLOGICAL_PANE`) asks to bring Bea,
/// a teammate with no access yet, into the session. Nothing is shared
/// until Alex sends the card; then, through control, Bea's phone gets
/// exactly one invite, opening at the pane, and the agent reads `sent`.
#[test]
fn an_agent_in_a_terminal_asks_and_the_owner_sends() {
    let w = world();
    let d = w.daemon(false);
    let (pane, session) = first(&d);
    let r = d.post("/api/team-pins", json!({ "pins": { "t1": w.pin() } }));
    assert_eq!(r["checked"], json!(["t1"]), "{r}");
    w.route(&w.bea);

    let m = Mcp::bridge(&d, Some(pane));
    let r = m.call("invite_person", json!({ "who": "bea", "note": "the flaky test needs your eyes" })).unwrap();
    assert_eq!((r["status"].as_str(), r["who"].as_str()), (Some("waiting"), Some("account:b1")), "{r}");
    let (draft, block) = (r["draft"].as_str().unwrap().to_owned(), r["block"].as_u64().unwrap());
    assert!(d.get("/api/acl")["grants"].as_array().unwrap().is_empty(), "nothing shared yet");
    std::thread::sleep(Duration::from_millis(500));
    assert!(w.bea.got(&w.fake).is_empty());

    // Alex sends it from the card (here, as the swarm's rail would).
    let card = d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == block).unwrap()["ask"].clone();
    assert_eq!(card["source"], "invite", "{card}");
    let act = json!({ "action": "answer", "pane": block, "id": card["id"], "content": { "role": "viewer" } });
    let r = d.post("/api/attention/act", act);
    assert_eq!(r["results"][0]["ok"], true, "{r}");
    let sent = std::time::Instant::now();
    while w.bea.got(&w.fake).is_empty() {
        assert!(sent.elapsed() < Duration::from_secs(10), "Bea's phone heard nothing in 10s");
        std::thread::sleep(Duration::from_millis(50));
    }
    let got = w.bea.got(&w.fake);
    assert!(got[0]["tag"].as_str().unwrap().starts_with("invite-"), "{got:?}");
    assert_eq!(got[0]["pane"].as_u64(), Some(pane));
    assert_eq!(got[0]["session"].as_u64(), Some(session));
    assert_eq!(got[0]["body"], "the flaky test needs your eyes");

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let r = loop {
        let r = m.call("read_invite", json!({ "draft": draft })).unwrap();
        if r["status"] != "waiting" {
            break r;
        }
        assert!(std::time::Instant::now() < deadline, "{r}");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!((r["status"].as_str(), r["delivery"].as_str()), (Some("sent"), Some("sent")), "{r}");
    assert_eq!(d.get("/api/acl")["grants"][0]["principal"], "account:b1");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(w.bea.got(&w.fake).len(), 1, "exactly one");
}
