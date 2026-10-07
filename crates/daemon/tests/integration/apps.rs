//! M35: studio app blocks, against a fake studio and a fake box (a page
//! plus hud's routes), both served here.
//!
//! The studio lists the person's apps and mints entry links for a bearer
//! token. The box lets in whoever follows an entry link (a cookie), and
//! answers hud's tabs, chat stream (`hud-chat-queue` frames with the
//! question a turn waits on) and answer routes. The daemon's follower turns
//! a question into an ask on the app block, sends the answer back (naming
//! who gave it, with a follower credential), withdraws a card when hud
//! settles the question or it expires, follows a dropped stream again, and
//! mints a new way in when hud's session goes. Questions raised on a
//! browser block through `/ask` work the same way, and guests who only
//! watch can't answer. Nothing it keeps holds an entry link.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use agentd::*;
use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use tokio::sync::watch;

const OWNER: &str = "me@example.com";
const FRIEND: &str = "friend@example.com";
const TOKEN: &str = "studio-token-xyz";

#[derive(Default)]
struct Inner {
    /// Entry keys studio minted (the box takes any of them).
    keys: HashSet<String>,
    mints: u32,
    sessions: HashSet<String>,
    next: u32,
    tabs: Vec<String>,
    /// The question each tab's running turn waits on.
    questions: HashMap<String, Value>,
    answers: Vec<(Value, Option<String>)>,
    connects: u32,
    /// Gates pending on the work board, and how often it was read.
    gates: Vec<Value>,
    board_reads: u32,
    approvals: Vec<Value>,
    /// The next approve fails, saying this.
    fail_next: Option<String>,
    /// Prompts to the box's agent (with the Origin they came with).
    prompts: Vec<(Value, Option<String>)>,
}

#[derive(Clone)]
struct Fake {
    inner: Arc<Mutex<Inner>>,
    origin: Arc<Mutex<String>>,
    /// Bumped when a queue changes; and to drop every stream.
    changed: Arc<watch::Sender<u64>>,
    drop: Arc<watch::Sender<u64>>,
    /// Bumped when the workspace moves (hud's live feed).
    live: Arc<watch::Sender<u64>>,
}

impl Fake {
    fn authed(&self, h: &HeaderMap) -> bool {
        let c = h.get("cookie").and_then(|c| c.to_str().ok()).unwrap_or("");
        let inner = self.inner.lock().unwrap();
        c.split("; ").filter_map(|kv| kv.strip_prefix("hud_session=")).any(|s| inner.sessions.contains(s))
    }
    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }
    fn ask(&self, chat: &str, id: &str, expires_in_ms: u64) {
        let now = now();
        self.inner.lock().unwrap().questions.insert(
            chat.into(),
            json!({ "requestId": id, "summary": format!("Which colour ({id})?"), "askedAt": now, "expiresAt": now + expires_in_ms,
                "options": [{ "optionId": "o0", "name": "Green", "description": "Matches arugula" }, { "optionId": "o1", "name": "Blue" }] }),
        );
        self.bump();
    }
    fn settle(&self, chat: &str) {
        self.inner.lock().unwrap().questions.remove(chat);
        self.bump();
    }
    fn gate(&self, member: &str, gate: &str) {
        self.inner
            .lock()
            .unwrap()
            .gates
            .push(json!({ "member": member, "component": "release", "name": gate, "env": "prod",
            "needed": 1, "approvals": 0, "approve": format!("chant approve release {gate} --env prod") }));
        self.live.send_modify(|n| *n += 1);
    }
    fn answers(&self) -> Vec<(Value, Option<String>)> {
        self.inner.lock().unwrap().answers.clone()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

type Q = Query<HashMap<String, String>>;

fn redirect(to: &str, cookie: Option<String>) -> Response {
    let mut r = Response::builder().status(302).header("location", to);
    if let Some(c) = cookie {
        r = r.header("set-cookie", c);
    }
    r.body(Body::empty()).unwrap()
}

async fn enter(State(f): State<Fake>, Query(q): Q) -> Response {
    if !f.inner.lock().unwrap().keys.contains(q.get("k").map(String::as_str).unwrap_or("")) {
        return (StatusCode::FORBIDDEN, "Not an entry link").into_response();
    }
    redirect("/__hud/join?t=owner", Some("box_next=x; Path=/; Max-Age=120; SameSite=None; Secure".into()))
}

async fn join(State(f): State<Fake>, Query(q): Q) -> Response {
    let t = q.get("t").cloned().unwrap_or_default();
    if t != "owner" && t != "follower" {
        return (StatusCode::UNAUTHORIZED, "You need a link to get in").into_response();
    }
    let s = {
        let mut i = f.inner.lock().unwrap();
        i.next += 1;
        let s = format!("{t}{}", i.next);
        i.sessions.insert(s.clone());
        s
    };
    redirect("/", Some(format!("hud_session={s}; Path=/; HttpOnly; SameSite=None; Secure; Partitioned")))
}

async fn page(State(f): State<Fake>, h: HeaderMap) -> Response {
    if !f.authed(&h) {
        return (StatusCode::UNAUTHORIZED, "You need a link to get in").into_response();
    }
    ([("content-type", "text/html")], "<title>Pinboard</title><p id=app>Hello from the box</p>").into_response()
}

async fn tabs(State(f): State<Fake>, h: HeaderMap) -> Response {
    if !f.authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let tabs: Vec<Value> =
        f.inner.lock().unwrap().tabs.iter().map(|t| json!({ "chatKey": t, "title": format!("Tab {t}") })).collect();
    Json(json!({ "tabs": tabs })).into_response()
}

async fn stream(State(f): State<Fake>, h: HeaderMap, Query(q): Q) -> Response {
    if !f.authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let chat = q.get("chatKey").cloned().unwrap_or_default();
    f.inner.lock().unwrap().connects += 1;
    let (rx, d) = (f.changed.subscribe(), f.drop.subscribe());
    let s = futures_util::stream::unfold((rx, d, true, f, chat), |(mut rx, mut d, first, f, chat)| async move {
        if !first {
            // A drop wins over a change that came after it.
            tokio::select! {
                biased;
                _ = d.changed() => return None,
                r = rx.changed() => r.ok()?,
            }
        }
        let q = f.inner.lock().unwrap().questions.get(&chat).cloned();
        let mut m = json!({ "type": "hud-chat-queue", "chatKey": chat, "waiting": [] });
        if let Some(q) = q {
            m["question"] = q;
        }
        let frame = format!("event: message\ndata: {m}\n\n");
        Some((Ok::<_, std::convert::Infallible>(frame), (rx, d, false, f, chat)))
    });
    Response::builder().header("content-type", "text/event-stream").body(Body::from_stream(s)).unwrap()
}

async fn answer(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !f.authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let origin = h.get("origin").and_then(|o| o.to_str().ok()).map(str::to_owned);
    if origin.as_deref() != Some(f.origin.lock().unwrap().as_str()) {
        return (StatusCode::FORBIDDEN, "cross-site").into_response();
    }
    let chat = body["chatKey"].as_str().unwrap_or_default().to_owned();
    f.inner.lock().unwrap().answers.push((body.clone(), origin));
    f.settle(&chat);
    Json(json!({ "answered": true, "requestId": body["requestId"], "optionId": body["optionId"] })).into_response()
}

/// hud's prompt route: an unknown tab is refused; else the turn starts
/// (queued behind one already running), and the box's agent asks.
async fn prompt(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !f.authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let origin = h.get("origin").and_then(|o| o.to_str().ok()).map(str::to_owned);
    if origin.as_deref() != Some(f.origin.lock().unwrap().as_str()) {
        return (StatusCode::FORBIDDEN, "cross-site").into_response();
    }
    let chat = body["chatKey"].as_str().unwrap_or_default().to_owned();
    if !f.inner.lock().unwrap().tabs.contains(&chat) {
        let e = json!({ "error": "this conversation is not a tab on this box", "reason": "unknown-tab" });
        return (StatusCode::NOT_FOUND, Json(e)).into_response();
    }
    let position = {
        let mut inner = f.inner.lock().unwrap();
        let running = inner.questions.contains_key(&chat) as u64;
        inner.prompts.push((body.clone(), origin));
        running
    };
    if position == 0 {
        f.ask(&chat, &format!("q{}", f.inner.lock().unwrap().prompts.len()), 300_000);
    }
    let id = format!("p{}", f.inner.lock().unwrap().prompts.len());
    (StatusCode::ACCEPTED, Json(json!({ "ok": true, "promptId": id, "position": position, "queued": position > 0 })))
        .into_response()
}

async fn work(State(f): State<Fake>, h: HeaderMap) -> Response {
    if !f.authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut i = f.inner.lock().unwrap();
    i.board_reads += 1;
    let items: Vec<Value> = i.gates.iter().map(|g| json!({ "key": format!("gate:{}/{}", g["member"].as_str().unwrap(), g["name"].as_str().unwrap()), "gate": g })).collect();
    Json(json!({ "v": 1, "env": "prod", "groups": [{ "id": "decide", "items": [] }, { "id": "approve", "items": items }] })).into_response()
}

async fn live(State(f): State<Fake>, h: HeaderMap) -> Response {
    if !f.authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let (rx, d) = (f.live.subscribe(), f.drop.subscribe());
    let s = futures_util::stream::unfold((rx, d, true), |(mut rx, mut d, first)| async move {
        if !first {
            tokio::select! {
                biased;
                _ = d.changed() => return None,
                r = rx.changed() => r.ok()?,
            }
        }
        let frame =
            if first { "event: live-snapshot\ndata: {}\n\n" } else { ": keepalive\n\nevent: live-state\ndata: {}\n\n" };
        Some((Ok::<_, std::convert::Infallible>(frame), (rx, d, false)))
    });
    Response::builder().header("content-type", "text/event-stream").body(Body::from_stream(s)).unwrap()
}

async fn approve_gate(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !f.authed(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if h.get("origin").and_then(|o| o.to_str().ok()) != Some(f.origin.lock().unwrap().as_str()) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "cross-site" }))).into_response();
    }
    let mut i = f.inner.lock().unwrap();
    if let Some(e) = i.fail_next.take() {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "code": "chant-refused", "error": e })))
            .into_response();
    }
    let at = i.gates.iter().position(|g| {
        g["member"] == body["member"]
            && g["component"] == body["component"]
            && g["name"] == body["gate"]
            && g["env"] == body["env"]
    });
    let Some(at) = at else {
        return (StatusCode::CONFLICT, Json(json!({ "code": "gate-not-pending", "error": "that gate isn't pending" })))
            .into_response();
    };
    i.gates.remove(at);
    i.approvals.push(body);
    drop(i);
    f.live.send_modify(|n| *n += 1);
    Json(json!({ "v": 1, "approval": { "command": "chant approve", "exitCode": 0, "output": "Gate resolved" } }))
        .into_response()
}

async fn apps(State(f): State<Fake>, h: HeaderMap) -> Response {
    if h.get("authorization").and_then(|a| a.to_str().ok()) != Some(&format!("Bearer {TOKEN}")) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "who are you?" }))).into_response();
    }
    let url = f.origin.lock().unwrap().clone();
    // studio#292's shape.
    Json(json!({ "apps": [{ "name": "pinboard", "title": "Pinboard", "url": url, "createdAt": 1 }] })).into_response()
}

async fn open(State(f): State<Fake>, h: HeaderMap) -> Response {
    if h.get("authorization").and_then(|a| a.to_str().ok()) != Some(&format!("Bearer {TOKEN}")) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let k = {
        let mut i = f.inner.lock().unwrap();
        i.mints += 1;
        let k = format!("key{}x{}", i.mints, now());
        i.keys.insert(k.clone());
        k
    };
    let url = f.origin.lock().unwrap().clone();
    Json(json!({ "url": format!("{url}/__enter?e={}&k={k}", now() + 600_000) })).into_response()
}

/// The fake box and studio, on a runtime of their own.
struct Fakes {
    rt: tokio::runtime::Runtime,
    f: Fake,
    studio: String,
    origin: String,
}

impl Fakes {
    fn start() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let f = Fake {
            inner: Arc::new(Mutex::new(Inner { tabs: vec!["c1".into()], ..Inner::default() })),
            origin: Arc::default(),
            changed: Arc::new(watch::channel(0).0),
            drop: Arc::new(watch::channel(0).0),
            live: Arc::new(watch::channel(0).0),
        };
        let (studio, origin) = rt.block_on(async {
            let boxed = Router::new()
                .route("/", get(page))
                .route("/__enter", get(enter))
                .route("/__hud/join", get(join))
                .route("/__hud/api/tabs", get(tabs))
                .route("/__hud/api/chat/stream", get(stream))
                .route("/__hud/api/chat/answer", post(answer))
                .route("/__hud/api/chat/prompt", post(prompt))
                .route("/__hud/api/work", get(work))
                .route("/__hud/api/live/stream", get(live))
                .route("/__hud/api/work/gates/approve", post(approve_gate))
                .with_state(f.clone());
            let studio = Router::new()
                .route("/api/apps", get(apps))
                .route("/api/apps/pinboard/open", post(open))
                .with_state(f.clone());
            let serve = |r: Router| async move {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let at = format!("http://{}", l.local_addr().unwrap());
                tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
                at
            };
            (serve(studio).await, serve(boxed).await)
        });
        *f.origin.lock().unwrap() = origin.clone();
        Self { rt, f, studio, origin }
    }

    fn login(&self, d: &Daemon) {
        let (status, body) = d.raw("POST", "/api/studio", Some(json!({ "url": self.studio, "token": "wrong" })));
        assert_eq!(status, 400, "{body}");
        let v = d.post("/api/studio", json!({ "url": self.studio, "token": TOKEN }));
        assert_eq!(v["apps"][0]["name"], "pinboard", "{v}");
    }
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn wait_ask(d: &Daemon, pane: u64, id: &str) -> Value {
    d.wait_for(&format!("the card for {id}"), || info(d, pane)["ask"]["id"] == id);
    info(d, pane)
}

fn following(d: &Daemon, b: u64) {
    d.wait_for("the follower", || d.state(b)["follower"]["state"] == "following");
}

/// Every file under `dir`, as bytes.
fn files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && !p.is_symlink() {
            out.extend(files(&p));
        } else if let Ok(b) = std::fs::read(&p) {
            out.push((p.display().to_string(), b));
        }
    }
    out
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// No entry link (nor its key, nor the studio token outside its own file)
/// anywhere in the state directory.
fn no_link_kept(d: &Daemon, f: &Fakes) {
    let keys: Vec<String> = f.f.inner.lock().unwrap().keys.iter().cloned().collect();
    assert!(!keys.is_empty());
    let all = files(&d.state);
    for (path, bytes) in &all {
        assert!(!contains(bytes, "__enter"), "an entry link in {path}");
        for k in &keys {
            assert!(!contains(bytes, k), "an entry key in {path}");
        }
        if !path.ends_with("studio.json") {
            assert!(!contains(bytes, TOKEN), "the studio token in {path}");
        }
    }
    // The grep can see the block's log: its `enter` (or `prompt`) lines are
    // there.
    let logged = |b: &[u8]| contains(b, "\"e\":\"enter\"") || contains(b, "\"e\":\"prompt\"");
    assert!(all.iter().any(|(_, b)| logged(b)), "the block's log wasn't found");
}

#[test]
fn a_box_agents_question_is_an_ask_answered_back_to_hud() {
    let f = Fakes::start();
    let d = Daemon::child();
    // Not logged in: no apps.
    let (status, body) = d.raw("GET", "/api/studio/apps", None);
    assert_eq!(status, 400, "{body}");
    f.login(&d);
    let shown = d.get("/api/studio");
    assert_eq!((shown["url"].as_str(), shown["logged_in"].as_bool()), (Some(f.studio.as_str()), Some(true)));
    assert!(!shown.to_string().contains(TOKEN));
    let file = d.state.join("studio.json");
    let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&file).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);

    let b = d.post("/api/blocks", json!({ "type": "app", "config": { "app": "pinboard" } }))["block"].as_u64().unwrap();
    let i = info(&d, b);
    assert_eq!((i["type"].as_str(), i["kind"].as_str()), (Some("app"), Some("app")), "{i}");
    assert_eq!(i["project"]["name"], "pinboard");
    let st = d.state(b);
    assert_eq!((st["box_url"].as_str(), st["studio"].as_str()), (Some(f.origin.as_str()), Some(f.studio.as_str())));
    assert_eq!((st["title"].as_str(), st["gates"].clone()), (Some("Pinboard"), json!([])));
    following(&d, b);
    assert_eq!(d.get("/api/studio/apps")["apps"][0]["blocks"], json!([b]));

    // A way in for a frame: a fresh link each time, to hud's pages only.
    let e1 = d.call(b, "enter", json!({}))["url"].as_str().unwrap().to_owned();
    let e2 = d.call(b, "enter", json!({ "to": "/__hud/work" }))["url"].as_str().unwrap().to_owned();
    assert!(e1.starts_with(&format!("{}/__enter?", f.origin)), "{e1}");
    assert!(e2.ends_with("&to=%2F__hud%2Fwork") && e1 != e2, "{e2}");
    let (status, _) = d.raw("POST", &format!("/api/blocks/{b}/call/enter"), Some(json!({ "to": "/elsewhere" })));
    assert_eq!(status, 400);

    // hud asks: a card on the block, "hud asks", bundled by the app.
    f.f.ask("c1", "r1", 300_000);
    let i = wait_ask(&d, b, "r1");
    assert_eq!((i["ask"]["source"].as_str(), i["ask"]["agent"].as_str()), (Some("hud"), Some("hud")));
    assert_eq!(i["ask"]["questions"][0]["options"][1]["label"], "Blue");
    assert_eq!(i["attention"], "needs_input");
    assert_eq!(i["reason"]["ask"]["agent"], "hud");
    assert_eq!(i["reason"]["bundle"], format!("ask:{}:hud", f.origin));
    let listed = d.get("/api/attention");
    assert!(listed.as_array().unwrap().iter().any(|a| a["pane"] == b), "{listed}");

    // Answered in arugula: back to hud, as the option hud named.
    let r = d.post("/api/attention/act", json!({ "action": "answer", "pane": b, "content": { "question_0": "Blue" } }));
    assert_eq!(r["results"][0]["ok"], true, "{r}");
    d.wait_for("hud's answer", || !f.f.answers().is_empty());
    let (sent, origin) = f.f.answers()[0].clone();
    assert_eq!(
        sent,
        json!({ "chatKey": "c1", "requestId": "r1", "optionId": "o1" }),
        "no onBehalfOf without a follower credential"
    );
    assert_eq!(origin.as_deref(), Some(f.origin.as_str()), "the box's own Origin");
    let i = info(&d, b);
    assert!(i["ask"].is_null());
    assert_eq!((i["answered"]["how"].as_str(), i["answered"]["who"].as_str()), (Some("answered"), Some("owner")));
    assert_ne!(i["attention"], "needs_input");
    d.wait_for("the follower to hear it", || d.state(b)["follower"]["last_answer"]["ok"] == true);

    // Answered in hud's own panel: the card is withdrawn.
    f.f.ask("c1", "r2", 300_000);
    wait_ask(&d, b, "r2");
    f.f.settle("c1");
    d.wait_for("the card to go", || info(&d, b)["ask"].is_null());
    assert_ne!(info(&d, b)["attention"], "needs_input");

    // A question that expires goes too, even if hud never says so.
    f.f.ask("c1", "r3", 1_500);
    wait_ask(&d, b, "r3");
    d.wait_for("r3 to expire", || info(&d, b)["ask"].is_null());
    f.f.settle("c1");

    f.f.ask("c1", "r4", 300_000);
    wait_ask(&d, b, "r4");

    // The stream drops: followed again, and what changed meanwhile is seen.
    let before = f.f.inner.lock().unwrap().connects;
    f.f.settle("c1");
    f.f.drop.send_modify(|n| *n += 1);
    f.f.ask("c1", "r5", 300_000);
    wait_ask(&d, b, "r5");
    assert!(f.f.inner.lock().unwrap().connects > before, "it followed the stream again");

    // hud's session goes (a restarted box): a new way in is minted.
    let mints = f.f.inner.lock().unwrap().mints;
    f.f.inner.lock().unwrap().sessions.clear();
    f.f.drop.send_modify(|n| *n += 1);
    f.f.settle("c1");
    d.wait_for("a new way in", || f.f.inner.lock().unwrap().mints > mints);
    d.wait_for("the card to go after re-entering", || info(&d, b)["ask"].is_null());
    following(&d, b);
    f.f.ask("c1", "r6", 300_000);
    wait_ask(&d, b, "r6");

    // Skipped in arugula: hud still waits, and the card doesn't come back.
    let r = d.post("/api/attention/act", json!({ "action": "deny", "pane": b }));
    assert_eq!(r["results"][0]["ok"], true, "{r}");
    std::thread::sleep(Duration::from_millis(700));
    assert!(info(&d, b)["ask"].is_null());
    assert_eq!(f.f.answers().len(), 1);

    no_link_kept(&d, &f);
    d.raw("DELETE", "/api/studio", None);
    assert!(!std::fs::read_to_string(&file).unwrap().contains(TOKEN));
}

#[test]
fn a_prompt_to_an_app_block_goes_to_the_box_agent() {
    let f = Fakes::start();
    f.f.inner.lock().unwrap().tabs.push("c2".into());
    let d = Daemon::child();
    f.login(&d);
    let b = d.post("/api/blocks", json!({ "type": "app", "config": { "app": "pinboard" } }))["block"].as_u64().unwrap();
    following(&d, b);

    // No tab named: the box's first, which hud starts on; its agent asks,
    // and the question is on the block.
    let r = d.call(b, "send", json!({ "text": "Pick a header colour" }));
    assert_eq!(
        (r["tab"].as_str(), r["chat"].as_str(), r["queued"].as_bool()),
        (Some("Tab c1"), Some("c1"), Some(false))
    );
    assert_eq!(r["prompt_id"], "p1", "{r}");
    let (sent, origin) = f.f.inner.lock().unwrap().prompts[0].clone();
    assert_eq!(sent, json!({ "chatKey": "c1", "text": "Pick a header colour" }));
    assert_eq!(origin.as_deref(), Some(f.origin.as_str()), "with the box's own Origin");
    wait_ask(&d, b, "q1");

    // A second prompt to that tab waits behind the turn; another tab, by
    // title in any case, or by key.
    let r = d.call(b, "send", json!({ "text": "and the footer", "tab": "c1" }));
    assert_eq!((r["position"].as_u64(), r["queued"].as_bool()), (Some(1), Some(true)), "{r}");
    let r = d.call(b, "send", json!({ "text": "hello", "tab": "tab C2" }));
    assert_eq!(r["chat"], "c2", "{r}");

    // A tab the box hasn't got, and nothing to say: refused, nothing sent.
    let sent = f.f.inner.lock().unwrap().prompts.len();
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{b}/call/send"), Some(json!({ "text": "x", "tab": "c9" })));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("no tab \\\"c9\\\"") && body.contains("Tab c1, Tab c2"), "{body}");
    let (status, _) = d.raw("POST", &format!("/api/blocks/{b}/call/send"), Some(json!({ "text": "  " })));
    assert_eq!(status, 400);
    assert_eq!(f.f.inner.lock().unwrap().prompts.len(), sent);

    // A follow-up (`/followup`) prompts it too.
    d.post(&format!("/api/panes/{b}/followup"), json!({ "text": "a follow-up" }));
    assert_eq!(f.f.inner.lock().unwrap().prompts.last().unwrap().0["text"], "a follow-up");

    // History says who prompted which tab.
    let h = d.get(&format!("/api/history?pane={b}"));
    let texts: Vec<&str> = h.as_array().unwrap().iter().filter_map(|c| c["text"].as_str()).collect();
    assert!(texts.contains(&"prompted Tab c1: Pick a header colour"), "{h}");
    assert!(texts.contains(&"prompted Tab c2: hello"), "{h}");
    no_link_kept(&d, &f);
}

#[test]
fn an_app_block_comes_back_after_a_restart_with_no_link_kept() {
    let f = Fakes::start();
    let mut d = Daemon::child();
    f.login(&d);
    let b = d.post("/api/blocks", json!({ "type": "app", "config": { "app": "pinboard" } }))["block"].as_u64().unwrap();
    following(&d, b);
    d.call(b, "enter", json!({}));
    d.call(b, "enter", json!({ "to": "/__hud/decisions" }));
    let mints = f.f.inner.lock().unwrap().mints;
    d.stop();
    no_link_kept(&d, &f);
    d.start();
    assert_eq!(info(&d, b)["type"], "app");
    following(&d, b);
    assert!(f.f.inner.lock().unwrap().mints > mints, "it minted again to get in");
    f.f.ask("c1", "after", 300_000);
    wait_ask(&d, b, "after");
    let url = d.call(b, "enter", json!({}))["url"].as_str().unwrap().to_owned();
    assert!(url.contains("/__enter?"));
    no_link_kept(&d, &f);
}

#[test]
fn asks_on_a_browser_block_and_who_may_answer() {
    let f = Fakes::start();
    let d = Daemon::child_with(&[
        "--wisp-token-file",
        "/nonexistent",
        "--owner",
        OWNER,
        "--tailscale-socket",
        "/nonexistent/sock",
    ]);
    f.login(&d);
    let pane = d.get("/api/panes")[0]["id"].as_u64().unwrap();
    let session = d.get("/api/panes")[0]["session"].as_u64().unwrap();
    let page =
        d.post("/api/blocks", json!({ "type": "browser", "config": { "url": f.origin }, "split": pane }))["block"]
            .as_u64()
            .unwrap();

    // Something following the page's agent (S22's bridge) asks on the block.
    let ask = |id: &str| {
        let (sock, id) = (d.sock(), id.to_owned());
        std::thread::spawn(move || {
            let mut s = std::os::unix::net::UnixStream::connect(sock).unwrap();
            let body = json!({ "id": id, "source": "bridge", "agent": "hud",
                "questions": [{ "question": format!("Ship {id}?"), "header": "hud", "multiSelect": false,
                    "options": [{ "label": "Yes" }, { "label": "No" }] }] })
            .to_string();
            use std::io::{Read, Write};
            write!(
                s,
                "POST /api/panes/{page}/ask HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            let start = out.find("\r\n\r\n").unwrap() + 4;
            serde_json::from_str::<Value>(out[start..].trim()).unwrap_or(Value::String(out))
        })
    };
    let asked = ask("q1");
    let i = wait_ask(&d, page, "q1");
    assert_eq!((i["ask"]["source"].as_str(), i["ask"]["agent"].as_str()), (Some("bridge"), Some("hud")));
    assert_eq!(i["reason"]["ask"]["agent"], "hud");
    assert!(i["reason"]["bundle"].as_str().unwrap().ends_with(":hud"), "{i}");
    // Withdrawing another question leaves this one.
    let (status, body) = d.raw("POST", &format!("/api/panes/{page}/ask/withdraw"), Some(json!({ "id": "nope" })));
    assert_eq!(status, 200, "{body}");
    assert_eq!(info(&d, page)["ask"]["id"], "q1");

    // A guest who watches can't answer it; one who edits can, as themselves.
    let http = reqwest::Client::new();
    let as_friend = |path: String, body: Value| {
        let r = f.rt.block_on(
            http.post(format!("http://127.0.0.1:{}{path}", d.port))
                .header("tailscale-user-login", FRIEND)
                .json(&body)
                .send(),
        );
        let r = r.unwrap();
        (r.status().as_u16(), f.rt.block_on(r.text()).unwrap())
    };
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "viewer" }));
    let answer = json!({ "action": "answer", "pane": page, "content": { "question_0": "Yes" } });
    let (status, body) = as_friend("/api/attention/act".into(), answer.clone());
    assert_eq!(status, 403, "{body}");
    let (status, body) =
        as_friend(format!("/api/blocks/{page}/call/answer"), json!({ "content": { "question_0": "Yes" } }));
    assert_eq!(status, 403, "{body}");
    assert_eq!(info(&d, page)["ask"]["id"], "q1");
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "editor" }));
    let (status, body) =
        as_friend(format!("/api/blocks/{page}/call/answer"), json!({ "id": "q1", "content": { "question_0": "Yes" } }));
    assert_eq!(status, 200, "{body}");
    let got = asked.join().unwrap();
    assert_eq!(got["action"], "accept", "{got}");
    assert_eq!(got["content"]["question_0"], "Yes");
    assert_eq!(got["by"]["who"], format!("tailnet:{FRIEND}"), "{got}");
    assert_eq!(info(&d, page)["answered"]["who"], format!("tailnet:{FRIEND}"));

    // Withdrawn by its asker.
    let asked = ask("q2");
    wait_ask(&d, page, "q2");
    d.post(&format!("/api/panes/{page}/ask/withdraw"), json!({ "id": "q2" }));
    assert_eq!(asked.join().unwrap()["action"], "withdrawn");
    assert!(info(&d, page)["ask"].is_null());

    // A studio box with a follower credential: hud hears who answered.
    let (status, body) = d.raw(
        "PUT",
        "/api/studio/followers/pinboard",
        Some(json!({ "link": format!("{}/__hud/join?t=follower", f.origin) })),
    );
    assert_eq!(status, 200, "{body}");
    assert!(!d.get("/api/studio").to_string().contains("t=follower"));
    let b = d.post(
        "/api/blocks",
        json!({ "type": "app", "config": { "app": "pinboard", "follower": true }, "split": pane }),
    )["block"]
        .as_u64()
        .unwrap();
    following(&d, b);
    assert_eq!(d.state(b)["follower_credential"], true);
    assert!(f.f.inner.lock().unwrap().sessions.iter().any(|s| s.starts_with("follower")));
    // Only the owner gets a way into the box.
    let (status, _) = as_friend(format!("/api/blocks/{b}/call/enter"), json!({}));
    assert_eq!(status, 403);
    f.f.ask("c1", "rf", 300_000);
    wait_ask(&d, b, "rf");
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "viewer" }));
    let (status, _) = as_friend(
        "/api/attention/act".into(),
        json!({ "action": "answer", "pane": b, "content": { "question_0": "Green" } }),
    );
    assert_eq!(status, 403);
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "editor" }));
    let (status, body) = as_friend(
        "/api/attention/act".into(),
        json!({ "action": "answer", "pane": b, "content": { "question_0": "Green" } }),
    );
    assert_eq!(status, 200, "{body}");
    d.wait_for("hud's answer", || !f.f.answers().is_empty());
    let (sent, _) = f.f.answers()[0].clone();
    assert_eq!(sent["optionId"], "o0");
    assert_eq!(sent["onBehalfOf"]["via"], "arugula", "{sent}");
    assert!(sent["onBehalfOf"]["name"].as_str().is_some_and(|n| !n.is_empty() && n != "owner"), "{sent}");
}

#[test]
fn a_box_gate_is_attention_approved_through_hud_as_whoever_clicked() {
    let f = Fakes::start();
    let d = Daemon::child_with(&[
        "--wisp-token-file",
        "/nonexistent",
        "--owner",
        OWNER,
        "--tailscale-socket",
        "/nonexistent/sock",
    ]);
    f.login(&d);
    let (status, _) = d.raw(
        "PUT",
        "/api/studio/followers/pinboard",
        Some(json!({ "link": format!("{}/__hud/join?t=follower", f.origin) })),
    );
    assert_eq!(status, 200);
    let pane = d.get("/api/panes")[0]["id"].as_u64().unwrap();
    let session = d.get("/api/panes")[0]["session"].as_u64().unwrap();
    let b = d.post(
        "/api/blocks",
        json!({ "type": "app", "config": { "app": "pinboard", "follower": true }, "split": pane }),
    )["block"]
        .as_u64()
        .unwrap();
    following(&d, b);
    d.wait_for("the first read of the board", || f.f.inner.lock().unwrap().board_reads > 0);

    // A release waits at ship: attention, from hud's live feed.
    f.f.gate("delivery", "ship");
    d.wait_for("the gate", || info(&d, b)["reason"]["kind"] == "gate");
    let i = info(&d, b);
    assert_eq!(i["attention"], "needs_input");
    let r = &i["reason"];
    assert_eq!(r["gate"]["source"], json!({ "kind": "hud", "box_url": f.origin, "app": "pinboard" }));
    assert_eq!((r["gate"]["op"].as_str(), r["gate"]["env"].as_str()), (Some("release"), Some("prod")));
    assert_eq!(r["bundle"], format!("gate:{}", f.origin));
    assert_eq!(r["headline"], "delivery: release waits at gate ship");
    assert_eq!(d.state(b)["gates"][0]["gate"], "ship");
    // Read on change, not on a timer.
    let reads = f.f.inner.lock().unwrap().board_reads;
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(f.f.inner.lock().unwrap().board_reads, reads, "the board isn't polled");

    // A question meanwhile comes first; answered, the gate is the reason again.
    f.f.ask("c1", "q1", 300_000);
    wait_ask(&d, b, "q1");
    assert_eq!(info(&d, b)["reason"]["kind"], "ask");
    d.post("/api/attention/act", json!({ "action": "answer", "pane": b, "content": { "question_0": "Green" } }));
    d.wait_for("the gate again", || info(&d, b)["reason"]["kind"] == "gate");
    assert_eq!(info(&d, b)["attention"], "needs_input");

    let http = reqwest::Client::new();
    let as_friend = |body: Value| {
        let r =
            f.rt.block_on(
                http.post(format!("http://127.0.0.1:{}/api/attention/act", d.port))
                    .header("tailscale-user-login", FRIEND)
                    .json(&body)
                    .send(),
            )
            .unwrap();
        (r.status().as_u16(), f.rt.block_on(r.text()).unwrap())
    };
    let allow = json!({ "action": "allow", "pane": b, "id": "delivery/release/ship" });
    // A viewer can't approve it.
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "viewer" }));
    assert_eq!(as_friend(allow.clone()).0, 403);
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "editor" }));

    // hud refuses: the card stays, saying why.
    f.f.inner.lock().unwrap().fail_next = Some("chant approve exited 1".into());
    let (status, body) = as_friend(allow.clone());
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("chant approve exited 1"), "{body}");
    let i = info(&d, b);
    assert_eq!(i["reason"]["kind"], "gate");
    assert!(
        i["reason"]["headline"].as_str().unwrap().ends_with("approving failed: hud: chant approve exited 1"),
        "{i}"
    );
    assert_eq!(i["attention"], "needs_input");

    // Approved by an editor: hud hears who, and the card goes with the gate.
    let (status, body) = as_friend(allow);
    assert_eq!(status, 200, "{body}");
    let sent = f.f.inner.lock().unwrap().approvals[0].clone();
    assert_eq!(
        sent,
        json!({ "member": "delivery", "component": "release", "gate": "ship", "env": "prod",
            "onBehalfOf": { "name": "friend", "via": "arugula" } })
    );
    d.wait_for("the card to go", || info(&d, b)["reason"].is_null());
    let i = info(&d, b);
    assert_ne!(i["attention"], "needs_input");
    assert_eq!(
        (i["answered"]["how"].as_str(), i["answered"]["who"].as_str()),
        (Some("approved"), Some(format!("tailnet:{FRIEND}").as_str()))
    );
    assert!(d.state(b)["gates"].as_array().unwrap().is_empty());
    let hist = d.get(&format!("/api/history?pane={b}"));
    assert!(hist.as_array().unwrap().iter().any(|h| h["text"] == "approved delivery: release at gate ship"), "{hist}");
}
