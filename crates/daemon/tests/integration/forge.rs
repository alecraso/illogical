//! M36: Forgejo pull request blocks, against a fake Forgejo served here
//! and a stand-in `tea` on the daemon's PATH.
//!
//! The fake answers Forgejo's API with S23's recorded responses
//! (`fixtures/forgejo/`), changed as a test needs (who asked whom for a
//! review, statuses red, a comment that mentions you), for the token the
//! stand-in `tea` hands out, and takes comments, reviews and merges, noting
//! the token each came with. Nothing here talks to a real forge.
//!
//! What's checked: attention for each of S23's rules; polls that re-read
//! reviews and the timeline only when the item moves; an agent's writes as
//! drafts (queued, edited, sent by the owner and by an editor and recorded
//! as theirs, refused to a viewer, dropped); a person's writes going
//! straight out; matching a remote's host to a login, or asking which; and
//! the PR's code as a worktree, a diff block and a terminal.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    collections::HashMap,
    io::{Read, Write as _},
    net::TcpStream,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use agentd::*;
use axum::{
    Json, Router,
    extract::{Path as UrlPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

const OWNER: &str = "owner@example.com";
const FRIEND: &str = "friend@example.com";
const TOKEN: &str = "fake-forgejo-token-123";
const REPO: &str = "jhgaylor/illogical";

fn fixture(f: &str) -> Value {
    let p = format!("{}/tests/fixtures/forgejo/forgejo-illogical-84/{f}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[derive(Default)]
struct Inner {
    item: Value,
    reviews: Vec<Value>,
    statuses: Vec<Value>,
    timeline: Vec<Value>,
    /// What `GET repos/O/R` says the clone URLs are.
    ssh_url: String,
    /// Requests by route, and the writes with the token they came with.
    gets: HashMap<&'static str, u32>,
    writes: Vec<(String, Value, String)>,
    next: u64,
}

#[derive(Clone)]
struct Fake {
    inner: Arc<Mutex<Inner>>,
    origin: Arc<Mutex<String>>,
}

impl Fake {
    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        f(&mut self.inner.lock().unwrap())
    }
    /// Something changed on the PR: Forgejo moves `updated_at`.
    fn touch(i: &mut Inner) {
        i.next += 1;
        i.item["updated_at"] = json!(format!("2026-10-03T01:{:02}:{:02}Z", i.next / 60 % 60, i.next % 60));
    }
    fn gets(&self, route: &str) -> u32 {
        self.with(|i| i.gets.get(route).copied().unwrap_or(0))
    }
    fn writes(&self) -> Vec<(String, Value, String)> {
        self.with(|i| i.writes.clone())
    }
}

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|a| a.to_str().ok()) == Some(&format!("token {TOKEN}"))
}

fn denied() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "message": "token is required" }))).into_response()
}

macro_rules! guard {
    ($h:expr) => {
        if !authed(&$h) {
            return denied();
        }
    };
}

async fn user(h: HeaderMap) -> Response {
    guard!(h);
    Json(json!({ "id": 1, "login": "jhgaylor" })).into_response()
}

async fn teams(h: HeaderMap) -> Response {
    guard!(h);
    Json(json!([{ "name": "Owners", "organization": { "name": "NestedData" } }])).into_response()
}

async fn repo(State(f): State<Fake>, h: HeaderMap) -> Response {
    guard!(h);
    let origin = f.origin.lock().unwrap().clone();
    let ssh = f.with(|i| i.ssh_url.clone());
    Json(json!({ "full_name": REPO, "ssh_url": ssh, "clone_url": format!("{origin}/{REPO}.git") })).into_response()
}

async fn pull(State(f): State<Fake>, h: HeaderMap) -> Response {
    guard!(h);
    f.with(|i| {
        *i.gets.entry("pull").or_default() += 1;
        Json(i.item.clone()).into_response()
    })
}

async fn get_reviews(State(f): State<Fake>, h: HeaderMap) -> Response {
    guard!(h);
    f.with(|i| {
        *i.gets.entry("reviews").or_default() += 1;
        Json(json!(i.reviews)).into_response()
    })
}

async fn status(State(f): State<Fake>, h: HeaderMap) -> Response {
    guard!(h);
    f.with(|i| {
        *i.gets.entry("status").or_default() += 1;
        Json(json!({ "state": "pending", "total_count": i.statuses.len(), "statuses": i.statuses })).into_response()
    })
}

async fn timeline(State(f): State<Fake>, h: HeaderMap) -> Response {
    guard!(h);
    f.with(|i| {
        *i.gets.entry("timeline").or_default() += 1;
        ([("x-total-count", i.timeline.len().to_string())], Json(json!(i.timeline))).into_response()
    })
}

fn token_of(h: &HeaderMap) -> String {
    h.get("authorization").and_then(|a| a.to_str().ok()).unwrap_or("").to_owned()
}

async fn comment(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    guard!(h);
    let origin = f.origin.lock().unwrap().clone();
    f.with(|i| {
        i.writes.push(("comment".into(), body.clone(), token_of(&h)));
        i.next += 1;
        let id = 5000 + i.next;
        i.timeline.push(json!({ "id": id, "type": "comment", "created_at": "2026-10-03T02:00:00Z",
            "user": { "login": "jhgaylor" }, "body": body["body"] }));
        Fake::touch(i);
        i.item["comments"] = json!(i.item["comments"].as_u64().unwrap_or(0) + 1);
        Json(json!({ "id": id, "html_url": format!("{origin}/{REPO}/pulls/84#issuecomment-{id}") })).into_response()
    })
}

async fn review(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    guard!(h);
    let origin = f.origin.lock().unwrap().clone();
    f.with(|i| {
        i.writes.push(("review".into(), body.clone(), token_of(&h)));
        i.next += 1;
        let id = 9000 + i.next;
        i.reviews
            .push(json!({ "id": id, "state": body["event"], "user": { "login": "jhgaylor" }, "body": body["body"],
            "submitted_at": "2026-10-03T02:00:00Z", "html_url": format!("/{REPO}/pulls/84#issuecomment-{id}") }));
        Fake::touch(i);
        Json(json!({ "id": id, "html_url": format!("{origin}/{REPO}/pulls/84#issuecomment-{id}") })).into_response()
    })
}

async fn merge(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    guard!(h);
    f.with(|i| {
        i.writes.push(("merge".into(), body.clone(), token_of(&h)));
        i.item["merged"] = json!(true);
        i.item["state"] = json!("closed");
        Fake::touch(i);
    });
    StatusCode::OK.into_response()
}

async fn missing(UrlPath(p): UrlPath<String>) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "message": format!("no {p} here") }))).into_response()
}

/// The fake Forgejo, on a runtime of its own, and a `tea` that knows it.
struct Forge {
    _rt: tokio::runtime::Runtime,
    f: Fake,
    origin: String,
    bin: PathBuf,
}

impl Forge {
    /// #84 as recorded, open again, by `author`, a review asked of
    /// `requested` (if any), its checks as recorded (green).
    fn start(dir: &Path, author: &str, requested: Option<&str>) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let mut item = fixture("item.json");
        item["state"] = json!("open");
        item["merged"] = json!(false);
        item["merged_at"] = Value::Null;
        item["user"]["login"] = json!(author);
        let mut reviews = vec![];
        if let Some(r) = requested {
            item["requested_reviewers"] = json!([{ "login": r }]);
            reviews.push(json!({ "id": 1, "state": "REQUEST_REVIEW", "user": { "login": r }, "submitted_at": "2026-10-02T23:40:00Z" }));
        }
        let f = Fake {
            inner: Arc::new(Mutex::new(Inner {
                item,
                reviews,
                statuses: fixture("statuses.json")["statuses"].as_array().unwrap().clone(),
                timeline: fixture("timeline.json").as_array().unwrap()[..1].to_vec(),
                ssh_url: format!("ssh://git@git.inevitable.fyi/{REPO}.git"),
                ..Inner::default()
            })),
            origin: Arc::default(),
        };
        let origin = rt.block_on(async {
            let p = "/api/v1/repos/{o}/{r}";
            let api = Router::new()
                .route("/api/v1/user", get(user))
                .route("/api/v1/user/teams", get(teams))
                .route(p, get(repo))
                .route(&format!("{p}/pulls/{{n}}"), get(pull))
                .route(&format!("{p}/pulls/{{n}}/reviews"), get(get_reviews).post(review))
                .route(&format!("{p}/pulls/{{n}}/merge"), post(merge))
                .route(&format!("{p}/commits/{{sha}}/status"), get(status))
                .route(&format!("{p}/issues/{{n}}/timeline"), get(timeline))
                .route(&format!("{p}/issues/{{n}}/comments"), post(comment))
                .route("/{*rest}", get(missing))
                .with_state(f.clone());
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let at = format!("http://{}", l.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(l, api).await.unwrap() });
            at
        });
        *f.origin.lock().unwrap() = origin.clone();
        // The recorded item's links are the real site's: the fake's here.
        f.with(|i| i.item["html_url"] = json!(format!("{origin}/{REPO}/pulls/84")));
        let bin = dir.join("bin");
        let forge = Self { _rt: rt, f, origin, bin };
        forge.tea(&[("forgejo", &forge.origin.clone())]);
        forge
    }

    /// A stand-in `tea` with these logins, whose credential helper hands
    /// out the fake's token (and notes each time it's asked).
    fn tea(&self, logins: &[(&str, &str)]) {
        std::fs::create_dir_all(&self.bin).unwrap();
        let list: Vec<Value> = logins
            .iter()
            .map(|(n, u)| json!({ "name": n, "url": u, "ssh_host": "", "user": "jhgaylor", "default": "false" }))
            .collect();
        std::fs::write(self.bin.join("logins.json"), Value::Array(list).to_string()).unwrap();
        let tea = self.bin.join("tea");
        std::fs::write(
            &tea,
            format!(
                r#"#!/bin/sh
b='{bin}'
case "$1 $2 $3" in
  "logins list -o") cat "$b/logins.json" ;;
  "login helper get") cat > "$b/asked"; echo protocol=http; echo username=jhgaylor; echo "password={TOKEN}" ;;
  *) echo "the stand-in doesn't know $*" >&2; exit 2 ;;
esac
"#,
                bin = self.bin.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&tea, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn daemon(&self) -> Daemon {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default());
        Daemon::child_env(
            &["--wisp-token-file", "/nonexistent", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
            &[("PATH", &path), ("ILLOGICAL_FORGE_POLL_MS", "250,250")],
        )
    }
}

fn scratch(tag: &str) -> Scratch {
    Scratch::new(&format!("forge-{tag}"))
}

fn open(d: &Daemon, config: Value) -> u64 {
    d.post("/api/blocks", json!({ "type": "forge", "config": config, "local": true }))["block"].as_u64().unwrap()
}

fn open_pr(d: &Daemon, forge: &Forge) -> u64 {
    open(d, json!({ "pr": format!("{}/{REPO}/pulls/84", forge.origin) }))
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn read(d: &Daemon, block: u64) -> Value {
    d.wait_for("the first read", || d.state(block)["updated_ms"].as_u64().is_some_and(|t| t > 0));
    d.state(block)
}

/// A request from a tailnet user (as `tailscale serve` passes them on).
fn as_friend(d: &Daemon, method: &str, path: &str, body: Value) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", d.port)).unwrap();
    let body = body.to_string();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nTailscale-User-Login: {FRIEND}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        d.port,
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, out.split_once("\r\n\r\n").map(|(_, b)| b.to_owned()).unwrap_or_default())
}

fn share(d: &Daemon, block: u64, role: &str) {
    let session = info(d, block)["session"].as_u64().unwrap();
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": role }));
}

#[test]
fn a_review_asked_of_you_is_a_gate_you_approve_as_yourself() {
    let dir = scratch("review");
    let forge = Forge::start(&dir, "someone", Some("jhgaylor"));
    let d = forge.daemon();
    let block = open_pr(&d, &forge);
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!((st["login"].as_str(), st["me"].as_str()), (Some("forgejo"), Some("jhgaylor")));
    assert_eq!(st["api"], format!("{}/api/v1", forge.origin));
    assert_eq!(st["pr"]["item"]["requested"], json!([{ "user": "jhgaylor" }]));
    // The token is tea's, and held nowhere a client or the layout sees.
    assert!(std::fs::read_to_string(forge.bin.join("asked")).unwrap().contains("protocol=http"));
    for text in [st.to_string(), d.get("/api/panes").to_string(), d.get(&format!("/api/blocks/{block}")).to_string()] {
        assert!(!text.contains(TOKEN), "the token leaked: {text}");
    }

    d.wait_for("attention", || info(&d, block)["reason"]["kind"] == "gate");
    let r = info(&d, block)["reason"].clone();
    assert_eq!(info(&d, block)["attention"], "needs_input");
    assert_eq!(r["gate"]["source"]["kind"], "forge", "{r}");
    assert_eq!(r["gate"]["op"], "#84");
    assert!(r["headline"].as_str().unwrap().starts_with("jhgaylor/illogical#84 review requested from you"), "{r}");
    assert_eq!(r["actions"], json!(["allow", "dismiss"]));
    let host = forge.origin.trim_start_matches("http://");
    assert_eq!(r["bundle"], format!("forge:{host}/{REPO}"));
    assert_eq!(info(&d, block)["kind"], "pr", "the swarm's kind");

    // Approved from the rail (the phone's sheet does the same): a review
    // goes to the forge with the owner's token, and the card says who.
    let out = d.post("/api/attention/act", json!({ "action": "allow", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    let w = forge.f.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!((w[0].0.as_str(), w[0].1["event"].as_str()), ("review", Some("APPROVED")));
    assert_eq!(w[0].2, format!("token {TOKEN}"));
    // Reviewed: no longer asked, so no longer attention.
    d.wait_for("the gate to go", || info(&d, block)["reason"].is_null());
    assert_eq!(info(&d, block)["answered"]["name"], OWNER);
    let hist = d.get(&format!("/api/history?pane={block}"));
    assert!(
        hist.as_array()
            .unwrap()
            .iter()
            .any(|h| h["text"] == "an approval on jhgaylor/illogical#84" && h["by"] == OWNER),
        "{hist}"
    );
}

#[test]
fn your_prs_checks_changes_mentions_and_merge() {
    let dir = scratch("mine");
    let forge = Forge::start(&dir, "jhgaylor", None);
    forge.f.with(|i| i.statuses[1]["status"] = json!("pending"));
    let d = forge.daemon();
    let block = open_pr(&d, &forge);
    read(&d, block);
    // Running: nothing yet. Going green: done.
    assert_eq!(d.state(block)["pr"]["rollup"], "running");
    assert_eq!(info(&d, block)["attention"], "idle");
    forge.f.with(|i| i.statuses[1]["status"] = json!("success"));
    d.wait_for("done", || info(&d, block)["reason"]["kind"] == "done");
    assert!(info(&d, block)["reason"]["headline"].as_str().unwrap().contains("checks green"));

    // A check goes red: failed, with the run linked (Forgejo can't rerun).
    forge.f.with(|i| i.statuses[0]["status"] = json!("failure"));
    d.wait_for("failed", || info(&d, block)["reason"]["kind"] == "failed");
    let i = info(&d, block);
    assert_eq!(i["attention"], "done");
    assert!(i["reason"]["headline"].as_str().unwrap().contains("1 check failed: check / macos (push)"), "{i}");
    assert_eq!(i["reason"]["actions"], json!(["dismiss"]), "no rerun on Forgejo");
    let rerun = &d.state(block)["rerun"];
    assert_eq!(rerun["api"], false);
    assert_eq!(rerun["url"], format!("{}/jhgaylor/illogical/actions/runs/111/jobs/1", forge.origin));

    // Green again, then changes requested: input.
    forge.f.with(|i| {
        i.statuses[0]["status"] = json!("success");
        i.reviews.push(
            json!({ "id": 7, "state": "REQUEST_CHANGES", "user": { "login": "reviewer" }, "body": "please split it",
            "submitted_at": "2026-10-03T00:10:00Z" }),
        );
        Fake::touch(i);
    });
    d.wait_for("input", || info(&d, block)["reason"]["kind"] == "input");
    assert!(info(&d, block)["reason"]["headline"].as_str().unwrap().contains("changes requested by reviewer"));
    let st = d.state(block);
    assert!(st["wants"].as_array().unwrap().iter().any(|w| w["kind"] == "changes"), "{st}");

    // They approve; then a mention, which looking at the block sees.
    forge.f.with(|i| {
        i.reviews.push(json!({ "id": 8, "state": "APPROVED", "user": { "login": "reviewer" }, "submitted_at": "2026-10-03T00:20:00Z" }));
        i.timeline.push(json!({ "id": 2001, "type": "comment", "created_at": "2099-01-01T00:00:00Z", "user": { "login": "sam" },
            "body": "@jhgaylor can you look at the CI bit?" }));
        Fake::touch(i);
    });
    d.wait_for("the mention", || {
        info(&d, block)["reason"]["headline"].as_str().is_some_and(|h| h.contains("mentioned by sam"))
    });
    // The timeline is in the block's log: search finds it.
    let hits = d.get("/api/search?re=CI%20bit");
    assert!(hits.as_array().unwrap().iter().any(|h| h["pane"] == block), "{hits}");

    // Merged: done says so.
    forge.f.with(|i| {
        i.timeline.pop();
        i.item["merged"] = json!(true);
        i.item["state"] = json!("closed");
        Fake::touch(i);
    });
    d.wait_for("merged", || info(&d, block)["reason"]["headline"].as_str().is_some_and(|h| h.contains("merged")));
    assert_eq!(info(&d, block)["attention"], "done");
    let text = d.raw("GET", &format!("/api/panes/{block}/capture?format=text"), None).1;
    assert!(text.contains("jhgaylor/illogical#84 CI: delete the kept build"), "{text}");
    assert!(text.contains("reviewer changes requested: please split it"), "{text}");
    assert!(text.contains("jhgaylor pushed 1 commit"), "{text}");
}

#[test]
fn a_pr_opened_already_done_isnt_news() {
    let dir = scratch("seen");
    let forge = Forge::start(&dir, "jhgaylor", None);
    let d = forge.daemon();
    let block = open_pr(&d, &forge);
    let st = read(&d, block);
    assert!(st["wants"].as_array().unwrap().iter().any(|w| w["kind"] == "done"), "{st}");
    // A few polls on, still nothing raised: it was green when opened.
    let polls = st["polls"].as_u64().unwrap();
    d.wait_for("polls", || d.state(block)["polls"].as_u64().unwrap() >= polls + 3);
    assert_eq!(info(&d, block)["attention"], "idle");
    // Merged later: that is news.
    forge.f.with(|i| {
        i.item["merged"] = json!(true);
        i.item["state"] = json!("closed");
        Fake::touch(i);
    });
    d.wait_for("merged", || info(&d, block)["reason"]["headline"].as_str().is_some_and(|h| h.contains("merged")));
}

#[test]
fn a_poll_rereads_the_rest_only_when_the_item_moves() {
    let dir = scratch("poll");
    let forge = Forge::start(&dir, "someone", None);
    let d = forge.daemon();
    let block = open_pr(&d, &forge);
    read(&d, block);
    let reviews = forge.f.gets("reviews");
    let polls = forge.f.gets("pull");
    // Several polls (every 250 ms here), nothing moved: no re-reads.
    d.wait_for("polls", || forge.f.gets("pull") >= polls + 4);
    assert_eq!(forge.f.gets("reviews"), reviews, "reviews re-read with nothing changed");
    assert_eq!(forge.f.gets("timeline"), forge.f.gets("reviews"));
    // (The item's GET is counted before the status's is made.)
    d.wait_for("the status polled with the item", || forge.f.gets("status") >= polls + 4);
    // A new status shows without a re-read: it's in the poll.
    forge.f.with(|i| i.statuses.push(json!({ "context": "lint", "status": "pending" })));
    d.wait_for("the new check", || d.state(block)["pr"]["checks"].as_array().unwrap().len() == 3);
    assert_eq!(d.state(block)["pr"]["rollup"], "running");
    assert_eq!(forge.f.gets("reviews"), reviews);
    // A review bumps updated_at: then the rest is read.
    forge.f.with(|i| {
        i.reviews.push(json!({ "id": 3, "state": "COMMENT", "user": { "login": "x" }, "body": "nit", "submitted_at": "2026-10-03T00:00:00Z" }));
        Fake::touch(i);
    });
    d.wait_for("the review", || d.state(block)["pr"]["reviews"].as_array().unwrap().len() == 1);
    assert_eq!(forge.f.gets("reviews"), reviews + 1);
    let st = d.state(block);
    assert!(st["reads"].as_u64().unwrap() < st["polls"].as_u64().unwrap(), "{st}");
}

#[test]
fn an_agents_writes_are_drafts_a_person_sends() {
    let dir = scratch("drafts");
    let forge = Forge::start(&dir, "someone", None);
    let d = forge.daemon();
    let block = open_pr(&d, &forge);
    read(&d, block);

    // Two drafts (the CLI under an agent sends agent: true; MCP's are
    // `mcp:<client>`): nothing reaches the forge.
    let one = d.call(block, "comment", json!({ "body": "LGTM, one nit", "agent": true }));
    assert_eq!(one["status"], "waiting", "{one}");
    let two =
        d.call(block, "review", json!({ "event": "request_changes", "body": "please add a test", "agent": true }));
    let three = d.call(block, "comment", json!({ "body": "and another", "agent": true }));
    let four = d.call(block, "merge", json!({ "agent": true }));
    assert!(forge.f.writes().is_empty(), "an agent's write went out");
    let st = d.state(block);
    let waiting: Vec<&Value> = st["drafts"].as_array().unwrap().iter().filter(|d| d["status"] == "waiting").collect();
    assert_eq!(waiting.len(), 4, "{st}");

    // The first is on the card: a form with the text to edit.
    d.wait_for("the card", || info(&d, block)["ask"]["id"] == one["draft"]);
    let ask = info(&d, block)["ask"].clone();
    assert_eq!((ask["kind"].as_str(), ask["source"].as_str()), (Some("form"), Some("forge")));
    assert_eq!(ask["message"], "an agent drafted a comment on jhgaylor/illogical#84");
    assert_eq!(ask["schema"]["properties"]["body"]["default"], "LGTM, one nit");
    assert_eq!(info(&d, block)["attention"], "needs_input");
    assert_eq!(info(&d, block)["reason"]["kind"], "ask");

    // The owner edits and sends it: the edited text goes, with the token.
    let call = format!("/api/blocks/{block}/call/answer");
    d.post(&call, json!({ "id": one["draft"], "content": { "body": "LGTM, one small nit" } }));
    d.wait_for("the comment", || forge.f.writes().len() == 1);
    let w = forge.f.writes();
    assert_eq!((w[0].0.as_str(), w[0].1["body"].as_str()), ("comment", Some("LGTM, one small nit")));
    assert_eq!(w[0].2, format!("token {TOKEN}"));
    d.wait_for("the next card", || info(&d, block)["ask"]["id"] == two["draft"]);
    let sent = d.state(block)["drafts"].as_array().unwrap().iter().find(|x| x["id"] == one["draft"]).cloned().unwrap();
    assert_eq!((sent["status"].as_str(), sent["settled_by"].as_str()), (Some("sent"), Some(OWNER)), "{sent}");
    assert!(sent["url"].as_str().unwrap().contains("#issuecomment-"), "{sent}");
    assert_eq!(info(&d, block)["ask"]["schema"]["properties"]["event"]["default"], "request_changes");

    // A viewer sees the card and can't send it, by either route.
    share(&d, block, "viewer");
    let (status, body) = as_friend(&d, "POST", &call, json!({ "id": two["draft"], "content": { "body": "x" } }));
    assert_eq!(status, 403, "{body}");
    let (status, _) = as_friend(&d, "POST", &format!("/api/blocks/{block}/call/comment"), json!({ "body": "hi" }));
    assert_eq!(status, 403);
    assert_eq!(forge.f.writes().len(), 1, "a viewer's send reached the forge");

    // An editor sends the next: it posts with the owner's login, and the
    // draft and the log say the editor sent it.
    share(&d, block, "editor");
    let (status, body) =
        as_friend(&d, "POST", &call, json!({ "id": two["draft"], "content": { "body": "please add a test for it" } }));
    assert_eq!(status, 200, "{body}");
    d.wait_for("the review", || forge.f.writes().len() == 2);
    let w = forge.f.writes();
    assert_eq!(
        (w[1].1["event"].as_str(), w[1].1["body"].as_str()),
        (Some("REQUEST_CHANGES"), Some("please add a test for it"))
    );
    assert_eq!(w[1].2, format!("token {TOKEN}"));
    d.wait_for("the third card", || info(&d, block)["ask"]["id"] == three["draft"]);
    let sent = d.state(block)["drafts"].as_array().unwrap().iter().find(|x| x["id"] == two["draft"]).cloned().unwrap();
    assert_eq!(sent["settled_by"], FRIEND, "{sent}");
    let hist = d.get(&format!("/api/history?pane={block}"));
    assert!(
        hist.as_array().unwrap().iter().any(|h| h["by"] == FRIEND
            && h["text"] == "a review asking for changes on jhgaylor/illogical#84 (drafted by an agent): please add a test for it"),
        "{hist}"
    );

    // Dropped: nothing goes, and the draft says who dropped it.
    d.post(&format!("/api/blocks/{block}/call/decline"), json!({ "id": three["draft"] }));
    d.wait_for("the merge card", || info(&d, block)["ask"]["id"] == four["draft"]);
    let dropped =
        d.state(block)["drafts"].as_array().unwrap().iter().find(|x| x["id"] == three["draft"]).cloned().unwrap();
    assert_eq!((dropped["status"].as_str(), dropped["settled_by"].as_str()), (Some("dropped"), Some(OWNER)));
    assert_eq!(forge.f.writes().len(), 2);
    d.post(&format!("/api/blocks/{block}/call/decline"), json!({ "id": four["draft"] }));
    d.wait_for("no card", || info(&d, block)["ask"].is_null());
    assert_eq!(forge.f.writes().len(), 2, "the dropped merge went out");
}

#[test]
fn a_persons_writes_go_straight_out() {
    let dir = scratch("direct");
    let forge = Forge::start(&dir, "jhgaylor", None);
    let d = forge.daemon();
    let block = open_pr(&d, &forge);
    read(&d, block);
    let out = d.call(block, "comment", json!({ "body": "Merging after CI" }));
    assert!(out["url"].as_str().unwrap().contains("#issuecomment-"), "{out}");
    assert_eq!(out["by"], OWNER);
    assert_eq!(forge.f.writes()[0].1["body"], "Merging after CI");
    // The block reads it back: the comment's in its timeline.
    d.wait_for("the comment in the timeline", || {
        d.state(block)["pr"]["events"].as_array().unwrap().iter().any(|e| e["body"] == "Merging after CI")
    });
    // An editor's own write goes out too, named.
    share(&d, block, "editor");
    let (status, body) =
        as_friend(&d, "POST", &format!("/api/blocks/{block}/call/merge"), json!({ "style": "squash" }));
    assert_eq!(status, 200, "{body}");
    let w = forge.f.writes();
    assert_eq!((w[1].0.as_str(), w[1].1["Do"].as_str()), ("merge", Some("squash")));
    let hist = d.get(&format!("/api/history?pane={block}"));
    assert!(
        hist.as_array().unwrap().iter().any(|h| h["by"] == FRIEND && h["text"] == "a merge on jhgaylor/illogical#84"),
        "{hist}"
    );
    // Bad writes are refused before the forge sees them; there's no approve.
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{block}/call/review"), Some(json!({ "event": "request_changes" })));
    assert_eq!(status, 400, "{body}");
    let (status, body) = d.raw("POST", &format!("/api/blocks/{block}/call/approve"), Some(json!({})));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("no method"), "{body}");
    assert_eq!(forge.f.writes().len(), 2);
}

#[test]
fn a_remotes_host_is_matched_to_a_login_or_you_pick_one() {
    let dir = scratch("logins");
    let forge = Forge::start(&dir, "someone", None);
    // Two logins, neither on the remote's (tailnet, SSH-only) host; the
    // fake says its repo's ssh_url is there.
    forge.tea(&[("other", "http://127.0.0.1:9"), ("forgejo", &forge.origin)]);
    forge.f.with(|i| i.ssh_url = format!("ssh://git@git.tailnet.example/{REPO}.git"));
    let d = forge.daemon();
    let block = open(&d, json!({ "repo": REPO, "number": 84, "host": "git.tailnet.example" }));
    let st = read(&d, block);
    assert_eq!((st["error"].as_str(), st["login"].as_str()), (None, Some("forgejo")), "{st}");

    // A host nobody knows: the block says so, and lists the logins.
    let block = open(&d, json!({ "repo": REPO, "number": 84, "host": "nowhere.example" }));
    let st = read(&d, block);
    assert!(st["error"].as_str().unwrap().starts_with("no tea login for nowhere.example"), "{st}");
    let names: Vec<&str> = st["logins"].as_array().unwrap().iter().filter_map(|l| l["name"].as_str()).collect();
    assert_eq!(names, ["other", "forgejo"]);
    // Picking one: kept, and it reads.
    d.call(block, "login", json!({ "name": "forgejo" }));
    let st = d.state(block);
    assert_eq!((st["error"].as_str(), st["login"].as_str()), (None, Some("forgejo")), "{st}");
    assert!(st["logins"].as_array().unwrap().is_empty());
    // Only the owner picks a login.
    share(&d, block, "editor");
    let (status, _) = as_friend(&d, "POST", &format!("/api/blocks/{block}/call/login"), json!({ "name": "other" }));
    assert_eq!(status, 403);
}

#[test]
fn no_tea_says_so() {
    let dir = scratch("notea");
    let forge = Forge::start(&dir, "someone", None);
    let d = Daemon::child_env(
        &["--wisp-token-file", "/nonexistent"],
        &[("PATH", "/usr/bin:/bin"), ("ILLOGICAL_FORGE_POLL_MS", "250,250")],
    );
    let block = open_pr(&d, &forge);
    let st = read(&d, block);
    assert!(st["error"].as_str().unwrap().starts_with("no tea here"), "{st}");
    assert_eq!(info(&d, block)["attention"], "idle");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn the_prs_code_as_a_worktree_a_diff_and_a_terminal() {
    let dir = scratch("code");
    let forge = Forge::start(&dir, "someone", None);
    // A remote with the PR's head where Forgejo keeps it, and a clone.
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "t")] {
        git(&work, &["config", k, v]);
    }
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "base"]);
    let base = git(&work, &["rev-parse", "HEAD"]);
    git(&work, &["checkout", "-qb", "feature"]);
    std::fs::write(work.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(work.join("b.txt"), "new\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "feature"]);
    let head = git(&work, &["rev-parse", "HEAD"]);
    let bare = dir.join("remote.git");
    git(&dir, &["clone", "-q", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    git(&bare, &["update-ref", "refs/pull/84/head", &head]);
    git(&bare, &["update-ref", "-d", "refs/heads/feature"]);
    let clone = dir.join("clone");
    git(&dir, &["clone", "-q", bare.to_str().unwrap(), clone.to_str().unwrap()]);
    git(&clone, &["remote", "set-url", "origin", &format!("ssh://git@git.inevitable.fyi/{REPO}.git")]);
    git(&clone, &["remote", "add", "pr", bare.to_str().unwrap()]);
    forge.f.with(|i| {
        i.item["head"]["sha"] = json!(head);
        i.item["base"]["sha"] = json!(base);
        i.item["merge_base"] = json!(base);
    });
    // `dir` names the clone; its remote for the repo is `origin`, which
    // isn't reachable here, so the test points it at the bare repo.
    git(&clone, &["remote", "set-url", "origin", bare.to_str().unwrap()]);
    let d = forge.daemon();
    let block = open(&d, json!({ "pr": format!("{}/{REPO}/pulls/84", forge.origin), "dir": clone }));
    read(&d, block);
    let out = d.call(block, "diff", json!({ "dir": clone }));
    // git reports the clone by its real path (/private/var on macOS).
    let wt = clone.canonicalize().unwrap().join(".illogical/worktrees/pr-84");
    assert_eq!(out["worktree"], wt.display().to_string(), "{out}");
    assert_eq!(out["rev_a"], base);
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&clone, &["rev-parse", "refs/illogical/pr/84"]), head);
    // The main checkout doesn't see it as untracked.
    assert_eq!(git(&clone, &["status", "--porcelain"]), "");
    let diff = out["block"].as_u64().unwrap();
    d.wait_for("the diff", || d.state(diff)["files"].as_array().is_some_and(|f| f.len() == 2));
    let files: Vec<String> =
        d.state(diff)["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_owned()).collect();
    assert_eq!(files, ["a.txt", "b.txt"]);
    // A terminal there, beside it.
    let out = d.call(block, "checkout", json!({}));
    let pane = out["pane"].as_u64().unwrap();
    d.wait_for("the shell's cwd", || info(&d, pane)["cwd"].as_str().is_some_and(|c| c.ends_with("pr-84")));
    // Again: the same worktree, no error.
    let again = d.call(block, "diff", json!({}));
    assert_eq!(again["worktree"], wt.display().to_string());
}

#[test]
fn a_restart_keeps_the_login_the_drafts_and_what_was_seen() {
    let dir = scratch("restart");
    let forge = Forge::start(&dir, "jhgaylor", None);
    forge.f.with(|i| i.ssh_url = format!("ssh://git@git.tailnet.example/{REPO}.git"));
    let mut d = forge.daemon();
    let block = open(&d, json!({ "repo": REPO, "number": 84, "host": "git.tailnet.example" }));
    read(&d, block);
    let draft = d.call(block, "comment", json!({ "body": "kept across a restart", "agent": true }));
    d.wait_for("the card", || info(&d, block)["ask"]["id"] == draft["draft"]);
    d.wait_for("the layout saved", || {
        std::fs::read_to_string(d.state.join("layout.json")).is_ok_and(|l| l.contains("kept across a restart"))
    });
    let layout = std::fs::read_to_string(d.state.join("layout.json")).unwrap();
    let saved: Value = serde_json::from_str(&layout).unwrap();
    assert_eq!(saved["panes"][block.to_string()]["config"]["login"], "forgejo", "{layout}");
    assert!(!layout.contains(TOKEN), "the token was saved");
    d.stop();
    // The repo lookup that found the login would fail now: the kept login
    // is used as it is.
    forge.f.with(|i| i.ssh_url = "ssh://git@elsewhere.example/x/y.git".into());
    d.start();
    let st = read(&d, block);
    assert_eq!((st["error"].as_str(), st["login"].as_str()), (None, Some("forgejo")), "{st}");
    d.wait_for("the draft's card again", || info(&d, block)["ask"]["id"] == draft["draft"]);
    assert_eq!(info(&d, block)["ask"]["schema"]["properties"]["body"]["default"], "kept across a restart");
    assert!(forge.f.writes().is_empty());
}
