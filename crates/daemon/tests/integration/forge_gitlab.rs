//! M39: GitLab merge requests as forge blocks, against a fake GitLab served
//! here and a stand-in `glab` on the daemon's PATH.
//!
//! The fake answers GitLab's v4 API with S23's recording of
//! `gitlab-org/cli!3941` (`fixtures/gitlab/`), moved to a project in a
//! subgroup (`group/sub/proj`) and changed per test. Like gitlab.com it
//! sends weak ETags and answers 304s, lets anyone read a public project but
//! keeps discussions and `GET /user` for a login, and takes notes,
//! approvals, merges and pipeline retries only with the token the stand-in
//! `glab` hands out, noting the token each came with. Nothing here talks
//! to a real forge.
//!
//! What's checked: a review asked of you as a gate approved with glab's
//! token; a failed pipeline offering *Rerun*, which retries it; changes
//! requested, a mention in a discussion, merged; polls that are one
//! conditional request when nothing moved; an agent's writes as drafts a
//! person sends; and the anonymous, read-only block refusing writes, with
//! glab knowing no login for the host and with no glab at all.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use agentd::*;
use axum::{
    Json, Router,
    extract::{Path as UrlPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde_json::{Value, json};

const OWNER: &str = "owner@example.com";
const TOKEN: &str = "fake-gitlab-token-456";
const REPO: &str = "group/sub/proj";
const N: u64 = 3941;

fn fixture(f: &str) -> Value {
    let p = format!("{}/tests/fixtures/gitlab/gitlab-cli-3941/{f}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[derive(Default)]
struct Inner {
    item: Value,
    reviewers: Vec<Value>,
    approvals: Value,
    jobs: Vec<Value>,
    discussions: Vec<Value>,
    /// Requests by route, 304s, and the writes with the token they came
    /// with.
    gets: HashMap<&'static str, u32>,
    not_modified: u32,
    writes: Vec<(String, Value, String)>,
    next: u64,
}

#[derive(Clone)]
struct Fake {
    inner: Arc<Mutex<Inner>>,
}

impl Fake {
    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        f(&mut self.inner.lock().unwrap())
    }
    /// Something changed on the MR: GitLab moves `updated_at`.
    fn touch(i: &mut Inner) {
        i.next += 1;
        i.item["updated_at"] = json!(format!("2026-10-03T01:{:02}:{:02}.000Z", i.next / 60 % 60, i.next % 60));
    }
    /// The head pipeline moved (its jobs did).
    fn pipeline(i: &mut Inner, status: &str) {
        i.next += 1;
        i.item["head_pipeline"]["status"] = json!(status);
        i.item["head_pipeline"]["updated_at"] = json!(format!("2026-10-03T02:00:{:02}.000Z", i.next % 60));
    }
    fn gets(&self, route: &str) -> u32 {
        self.with(|i| i.gets.get(route).copied().unwrap_or(0))
    }
    fn writes(&self) -> Vec<(String, Value, String)> {
        self.with(|i| i.writes.clone())
    }
}

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|a| a.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

fn token_of(h: &HeaderMap) -> String {
    h.get("authorization").and_then(|a| a.to_str().ok()).unwrap_or("").to_owned()
}

fn denied() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "message": "401 Unauthorized" }))).into_response()
}

/// A GET's answer with a weak ETag; 304 when the client has it.
fn tagged(f: &Fake, route: &'static str, h: &HeaderMap, v: Value, pages: Option<u64>) -> Response {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    v.to_string().hash(&mut hasher);
    let etag = format!("W/\"{:016x}\"", hasher.finish());
    f.with(|i| *i.gets.entry(route).or_default() += 1);
    if h.get("if-none-match").and_then(|e| e.to_str().ok()) == Some(&etag) {
        f.with(|i| i.not_modified += 1);
        return (StatusCode::NOT_MODIFIED, [("etag", etag)]).into_response();
    }
    let pages = pages.unwrap_or(1).to_string();
    ([("etag", etag), ("x-total-pages", pages)], Json(v)).into_response()
}

fn ours(project: &str) -> bool {
    project == REPO
}

async fn user(h: HeaderMap) -> Response {
    if !authed(&h) {
        return denied();
    }
    Json(json!({ "id": 1, "username": "jhgaylor" })).into_response()
}

async fn mr(State(f): State<Fake>, UrlPath((p, _n)): UrlPath<(String, u64)>, h: HeaderMap) -> Response {
    if !ours(&p) {
        return missing().await;
    }
    let v = f.with(|i| i.item.clone());
    tagged(&f, "mr", &h, v, None)
}

async fn get_reviewers(State(f): State<Fake>, h: HeaderMap) -> Response {
    let v = f.with(|i| json!(i.reviewers));
    tagged(&f, "reviewers", &h, v, None)
}

async fn get_approvals(State(f): State<Fake>, h: HeaderMap) -> Response {
    let v = f.with(|i| i.approvals.clone());
    tagged(&f, "approvals", &h, v, None)
}

async fn discussions(State(f): State<Fake>, h: HeaderMap) -> Response {
    f.with(|i| *i.gets.entry("discussions-asked").or_default() += 1);
    // Even on a public project, as gitlab.com does.
    if !authed(&h) {
        return denied();
    }
    let v = f.with(|i| json!(i.discussions));
    tagged(&f, "discussions", &h, v, None)
}

async fn get_jobs(State(f): State<Fake>, h: HeaderMap) -> Response {
    let v = f.with(|i| json!(i.jobs));
    tagged(&f, "jobs", &h, v, None)
}

async fn note(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !authed(&h) {
        return denied();
    }
    f.with(|i| {
        i.writes.push(("note".into(), body.clone(), token_of(&h)));
        i.next += 1;
        let id = 7000 + i.next;
        i.discussions.push(json!({ "id": format!("d{id}"), "individual_note": true, "notes": [
            { "id": id, "body": body["body"], "system": false, "author": { "username": "jhgaylor" }, "created_at": "2026-10-03T03:00:00.000Z" }
        ] }));
        i.item["user_notes_count"] = json!(i.item["user_notes_count"].as_u64().unwrap_or(0) + 1);
        Fake::touch(i);
        (StatusCode::CREATED, Json(json!({ "id": id, "body": body["body"] }))).into_response()
    })
}

async fn approve(State(f): State<Fake>, h: HeaderMap) -> Response {
    if !authed(&h) {
        return denied();
    }
    f.with(|i| {
        i.writes.push(("approve".into(), json!({}), token_of(&h)));
        i.approvals = json!({ "approved_by": [{ "user": { "id": 1, "username": "jhgaylor" }, "approved_at": "2026-10-03T03:00:00.000Z" }] });
        for r in i.reviewers.iter_mut().filter(|r| r["user"]["username"] == "jhgaylor") {
            r["state"] = json!("approved");
        }
        Fake::touch(i);
        (StatusCode::CREATED, Json(i.approvals.clone())).into_response()
    })
}

async fn merge(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !authed(&h) {
        return denied();
    }
    f.with(|i| {
        i.writes.push(("merge".into(), body.clone(), token_of(&h)));
        i.item["state"] = json!("merged");
        i.item["merged_by"] = json!({ "username": "jhgaylor" });
        Fake::touch(i);
        Json(i.item.clone()).into_response()
    })
}

async fn retry(State(f): State<Fake>, UrlPath((pid, id)): UrlPath<(u64, u64)>, h: HeaderMap) -> Response {
    if !authed(&h) {
        return denied();
    }
    f.with(|i| {
        i.writes.push(("retry".into(), json!({ "project": pid, "pipeline": id }), token_of(&h)));
        for j in i.jobs.iter_mut().filter(|j| j["status"] == "failed") {
            j["status"] = json!("pending");
        }
        Fake::pipeline(i, "running");
        (
            StatusCode::CREATED,
            Json(json!({ "id": id, "status": "running", "web_url": format!("https://fake/pipelines/{id}") })),
        )
            .into_response()
    })
}

async fn missing() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "message": "404 Project Not Found" }))).into_response()
}

/// The fake GitLab, on a runtime of its own, and a `glab` that knows it.
struct Forge {
    _rt: tokio::runtime::Runtime,
    f: Fake,
    origin: String,
    bin: PathBuf,
}

impl Forge {
    /// !3941 as recorded, open again, by `author`, a review asked of
    /// `requested` (if any), its pipeline green.
    fn start(dir: &Path, author: &str, requested: Option<&str>) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let mut item = fixture("item.json");
        item["state"] = json!("opened");
        item["merged_at"] = Value::Null;
        item["merged_by"] = Value::Null;
        item["merge_user"] = Value::Null;
        item["detailed_merge_status"] = json!("mergeable");
        item["author"]["username"] = json!(author);
        item["references"]["full"] = json!(format!("{REPO}!{N}"));
        let mut reviewers = vec![];
        if let Some(r) = requested {
            reviewers.push(json!({ "user": { "id": 1, "username": r }, "state": "unreviewed", "created_at": "2026-10-02T23:40:00.000Z" }));
        }
        let f = Fake {
            inner: Arc::new(Mutex::new(Inner {
                item,
                reviewers,
                approvals: json!({ "approved_by": [] }),
                jobs: fixture("pipeline_jobs.json").as_array().unwrap().clone(),
                ..Inner::default()
            })),
        };
        let origin = rt.block_on(async {
            let p = "/api/v4/projects/{p}/merge_requests/{n}";
            let api = Router::new()
                .route("/api/v4/user", get(user))
                .route(p, get(mr))
                .route(&format!("{p}/reviewers"), get(get_reviewers))
                .route(&format!("{p}/approvals"), get(get_approvals))
                .route(&format!("{p}/discussions"), get(discussions))
                .route(&format!("{p}/notes"), post(note))
                .route(&format!("{p}/approve"), post(approve))
                .route(&format!("{p}/merge"), put(merge))
                .route("/api/v4/projects/{pid}/pipelines/{id}/jobs", get(get_jobs))
                .route("/api/v4/projects/{pid}/pipelines/{id}/retry", post(retry))
                .fallback(missing)
                .with_state(f.clone());
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let at = format!("http://{}", l.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(l, api).await.unwrap() });
            at
        });
        f.with(|i| i.item["web_url"] = json!(format!("{origin}/{REPO}/-/merge_requests/{N}")));
        let bin = dir.join("bin");
        let forge = Self { _rt: rt, f, origin, bin };
        let host = forge.host();
        forge.glab(Some(&host));
        forge
    }

    fn host(&self) -> String {
        self.origin.trim_start_matches("http://").to_owned()
    }

    /// A stand-in `glab` whose default host is `host` (with a token for
    /// it), or which knows only gitlab.com and has no token (`None`).
    fn glab(&self, host: Option<&str>) {
        std::fs::create_dir_all(&self.bin).unwrap();
        let (default, token) = match host {
            Some(h) => (h.to_owned(), TOKEN),
            None => ("gitlab.com".to_owned(), ""),
        };
        let glab = self.bin.join("glab");
        std::fs::write(
            &glab,
            format!(
                r#"#!/bin/sh
b='{bin}'
case "$1 $2 $3" in
  "config get host") echo '{default}' ;;
  "config get token") echo "$5" >> "$b/asked"; [ "$5" = '{default}' ] && echo '{token}' ;;
  *) echo "the stand-in doesn't know $*" >&2; exit 2 ;;
esac
exit 0
"#,
                bin = self.bin.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&glab, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn daemon(&self) -> Daemon {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default());
        // No real glab config is read: it'd be the person's.
        let cfg = self.bin.join("no-config");
        Daemon::child_env(
            &["--wisp-token-file", "/nonexistent", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
            &[("PATH", &path), ("ARUGULA_FORGE_POLL_MS", "250,250"), ("GLAB_CONFIG_DIR", cfg.to_str().unwrap())],
        )
    }

    fn url(&self) -> String {
        format!("{}/{REPO}/-/merge_requests/{N}", self.origin)
    }
}

fn scratch(tag: &str) -> Scratch {
    Scratch::new(&format!("gitlab-{tag}"))
}

fn open(d: &Daemon, forge: &Forge) -> u64 {
    d.post("/api/blocks", json!({ "type": "forge", "config": { "pr": forge.url() }, "local": true }))["block"]
        .as_u64()
        .unwrap()
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn read(d: &Daemon, block: u64) -> Value {
    d.wait_for("the first read", || d.state(block)["updated_ms"].as_u64().is_some_and(|t| t > 0));
    d.state(block)
}

#[test]
fn a_review_asked_of_you_is_a_gate_approved_with_glabs_token() {
    let dir = scratch("review");
    let forge = Forge::start(&dir, "someone", Some("jhgaylor"));
    let d = forge.daemon();
    let block = open(&d, &forge);
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!(st["provider"], "gitlab");
    assert_eq!(st["repo"], REPO, "a project in a subgroup");
    assert_eq!(st["read_only"], Value::Null, "{st}");
    assert_eq!((st["login"].as_str(), st["me"].as_str()), (Some(&*format!("glab:{}", forge.host())), Some("jhgaylor")));
    assert_eq!(st["api"], format!("{}/api/v4", forge.origin));
    assert_eq!(st["pr"]["item"]["requested"], json!([{ "user": "jhgaylor" }]));
    assert_eq!(st["pr"]["item"]["head_ref"], format!("refs/merge-requests/{N}/head"));
    assert_eq!(st["pr"]["item"]["merge_base"], "5e811abcc71af2ab423477e4f58c903a7b2317bd");
    assert_eq!(st["pr"]["checks"].as_array().unwrap().len(), 23);
    assert_eq!(st["pr"]["rollup"], "success");
    // The token is glab's, asked for the link's host, and held nowhere a
    // client or the layout sees.
    assert!(std::fs::read_to_string(forge.bin.join("asked")).unwrap().contains(&forge.host()));
    for text in [st.to_string(), d.get("/api/panes").to_string(), d.get(&format!("/api/blocks/{block}")).to_string()] {
        assert!(!text.contains(TOKEN), "the token leaked: {text}");
    }

    d.wait_for("attention", || info(&d, block)["reason"]["kind"] == "gate");
    let r = info(&d, block)["reason"].clone();
    assert_eq!(r["gate"]["source"]["kind"], "forge", "{r}");
    assert_eq!(r["gate"]["op"], format!("#{N}"));
    assert!(r["headline"].as_str().unwrap().starts_with(&format!("{REPO}#{N} review requested from you")), "{r}");
    assert_eq!(r["bundle"], format!("forge:{}/{REPO}", forge.host()));

    // Approved from the rail: POST …/approve with glab's token.
    let out = d.post("/api/attention/act", json!({ "action": "allow", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    let w = forge.f.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!((w[0].0.as_str(), w[0].2.as_str()), ("approve", &*format!("Bearer {TOKEN}")));
    d.wait_for("the gate to go", || info(&d, block)["reason"].is_null());
    assert_eq!(info(&d, block)["answered"]["name"], OWNER);
    let st = d.state(block);
    let rs = st["pr"]["reviews"].as_array().unwrap();
    assert_eq!((rs[0]["author"].as_str(), rs[0]["state"].as_str()), (Some("jhgaylor"), Some("approved")), "{st}");
    let hist = d.get(&format!("/api/history?pane={block}"));
    assert!(
        hist.as_array().unwrap().iter().any(|h| h["text"] == format!("an approval on {REPO}#{N}") && h["by"] == OWNER),
        "{hist}"
    );
}

#[test]
fn your_failed_pipeline_reruns_then_changes_a_mention_and_merged() {
    let dir = scratch("mine");
    let forge = Forge::start(&dir, "jhgaylor", None);
    forge.f.with(|i| {
        i.jobs[8]["status"] = json!("failed"); // tests:integration
        i.jobs[19]["status"] = json!("failed"); // allowed to fail
        i.item["head_pipeline"]["status"] = json!("failed");
    });
    let d = forge.daemon();
    let block = open(&d, &forge);
    read(&d, block);
    // Opened red: a failure is news (only done isn't).
    d.wait_for("failed", || info(&d, block)["reason"]["kind"] == "failed");
    let i = info(&d, block);
    assert_eq!(i["attention"], "done");
    assert!(i["reason"]["headline"].as_str().unwrap().contains("1 check failed: tests:integration"), "{i}");
    assert_eq!(i["reason"]["actions"], json!(["rerun", "dismiss"]), "GitLab retries pipelines");
    let rerun = d.state(block)["rerun"].clone();
    assert_eq!((rerun["api"].as_bool(), rerun["pipeline"].as_str()), (Some(true), Some("2892163626")), "{rerun}");

    // Rerun from the rail: the pipeline is retried with glab's token, and
    // running again isn't attention.
    let out = d.post("/api/attention/act", json!({ "action": "rerun", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    let w = forge.f.writes();
    assert_eq!(w[0].0, "retry");
    assert_eq!(w[0].1, json!({ "project": 34675721, "pipeline": 2892163626u64 }));
    assert_eq!(w[0].2, format!("Bearer {TOKEN}"));
    d.wait_for("running", || d.state(block)["pr"]["rollup"] == "running");
    d.wait_for("no reason", || info(&d, block)["reason"].is_null());
    // Green: done.
    forge.f.with(|i| {
        for j in i.jobs.iter_mut().filter(|j| j["status"] == "pending") {
            j["status"] = json!("success");
        }
        Fake::pipeline(i, "success");
    });
    d.wait_for("done", || info(&d, block)["reason"]["kind"] == "done");
    assert!(info(&d, block)["reason"]["headline"].as_str().unwrap().contains("checks green"));

    // A reviewer asks for changes: input.
    forge.f.with(|i| {
        i.reviewers.push(json!({ "user": { "id": 3, "username": "rev" }, "state": "requested_changes", "created_at": "2026-10-03T00:10:00.000Z" }));
        i.item["detailed_merge_status"] = json!("requested_changes");
        Fake::touch(i);
    });
    d.wait_for("input", || info(&d, block)["reason"]["kind"] == "input");
    assert!(info(&d, block)["reason"]["headline"].as_str().unwrap().contains("changes requested by rev"));

    // They approve, and a thread mentions you.
    forge.f.with(|i| {
        i.reviewers[0]["state"] = json!("approved");
        i.item["detailed_merge_status"] = json!("mergeable");
        i.discussions.push(json!({ "id": "t1", "individual_note": false, "notes": [
            { "id": 501, "body": "@jhgaylor can you look at the CI bit?", "system": false, "type": "DiscussionNote",
              "author": { "username": "sam" }, "created_at": "2099-01-01T00:00:00.000Z" }] }));
        i.discussions.push(json!({ "id": "t2", "individual_note": true, "notes": [
            { "id": 502, "body": "requested review from @jhgaylor", "system": true,
              "author": { "username": "sam" }, "created_at": "2026-10-03T00:30:00.000Z" }] }));
        Fake::touch(i);
    });
    d.wait_for("the mention", || {
        info(&d, block)["reason"]["headline"].as_str().is_some_and(|h| h.contains("mentioned by sam"))
    });
    let hits = d.get("/api/search?re=CI%20bit");
    assert!(hits.as_array().unwrap().iter().any(|h| h["pane"] == block), "{hits}");

    // Merged: done says so.
    forge.f.with(|i| {
        i.discussions.retain(|x| x["id"] != "t1");
        i.item["state"] = json!("merged");
        Fake::touch(i);
    });
    d.wait_for("merged", || info(&d, block)["reason"]["headline"].as_str().is_some_and(|h| h.contains("merged")));
    let text = d.raw("GET", &format!("/api/panes/{block}/capture?format=text"), None).1;
    assert!(text.contains(&format!("{REPO}#{N} fix(issue): reject invalid list output flags")), "{text}");
    assert!(text.contains("rev approved"), "{text}");
    assert!(text.contains("sam asked jhgaylor for a review"), "{text}");
}

#[test]
fn an_unchanged_poll_is_one_conditional_request() {
    let dir = scratch("poll");
    let forge = Forge::start(&dir, "someone", None);
    let d = forge.daemon();
    let block = open(&d, &forge);
    read(&d, block);
    // The first read: the MR, its jobs, reviewers, approvals, discussions.
    let (mr, jobs, rv) = (forge.f.gets("mr"), forge.f.gets("jobs"), forge.f.gets("reviewers"));
    assert_eq!((jobs, rv, forge.f.gets("approvals"), forge.f.gets("discussions")), (1, 1, 1, 1));
    d.wait_for("polls", || forge.f.gets("mr") >= mr + 4);
    assert_eq!(forge.f.gets("jobs"), 1, "jobs re-read with the pipeline unchanged");
    assert_eq!(forge.f.gets("reviewers"), rv, "the rest re-read with nothing changed");
    assert!(forge.f.with(|i| i.not_modified) >= 4, "polls are conditional");
    // The pipeline moves: its jobs are read again, nothing else.
    forge.f.with(|i| {
        i.jobs[0]["status"] = json!("running");
        i.jobs[0]["allow_failure"] = json!(false);
        Fake::pipeline(i, "running");
    });
    d.wait_for("running", || d.state(block)["pr"]["rollup"] == "running");
    assert_eq!(forge.f.gets("jobs"), 2);
    assert_eq!(forge.f.gets("reviewers"), rv);
    // A note bumps the MR: then the rest is read.
    forge.f.with(|i| {
        i.item["user_notes_count"] = json!(18);
        Fake::touch(i);
    });
    d.wait_for("the re-read", || forge.f.gets("reviewers") == rv + 1);
    let st = d.state(block);
    assert!(st["reads"].as_u64().unwrap() < st["polls"].as_u64().unwrap(), "{st}");
}

#[test]
fn an_agents_writes_are_drafts_a_person_sends() {
    let dir = scratch("drafts");
    let forge = Forge::start(&dir, "someone", None);
    let d = forge.daemon();
    let block = open(&d, &forge);
    read(&d, block);
    let one = d.call(block, "comment", json!({ "body": "LGTM, one nit", "agent": true }));
    let two = d.call(block, "merge", json!({ "style": "squash", "agent": true }));
    assert_eq!(one["status"], "waiting", "{one}");
    assert!(forge.f.writes().is_empty(), "an agent's write went out");
    d.wait_for("the card", || info(&d, block)["ask"]["id"] == one["draft"]);
    assert_eq!(info(&d, block)["ask"]["message"], format!("an agent drafted a comment on {REPO}#{N}"));
    // The owner edits and sends it: a note, with glab's token.
    let call = format!("/api/blocks/{block}/call/answer");
    d.post(&call, json!({ "id": one["draft"], "content": { "body": "LGTM, one small nit" } }));
    d.wait_for("the note", || forge.f.writes().len() == 1);
    let w = forge.f.writes();
    assert_eq!((w[0].0.as_str(), w[0].1["body"].as_str()), ("note", Some("LGTM, one small nit")));
    assert_eq!(w[0].2, format!("Bearer {TOKEN}"));
    d.wait_for("the next card", || info(&d, block)["ask"]["id"] == two["draft"]);
    let sent = d.state(block)["drafts"].as_array().unwrap().iter().find(|x| x["id"] == one["draft"]).cloned().unwrap();
    assert_eq!((sent["status"].as_str(), sent["settled_by"].as_str()), (Some("sent"), Some(OWNER)), "{sent}");
    assert_eq!(sent["url"], format!("{}/{REPO}/-/merge_requests/{N}#note_7001", forge.origin), "{sent}");
    // The note reads back in the timeline (discussions, with the login).
    d.wait_for("the note in the timeline", || {
        d.state(block)["pr"]["events"].as_array().unwrap().iter().any(|e| e["body"] == "LGTM, one small nit")
    });
    // The squash merge: PUT …/merge {squash: true}.
    d.post(&call, json!({ "id": two["draft"], "content": {} }));
    d.wait_for("the merge", || forge.f.writes().len() == 2);
    assert_eq!(forge.f.writes()[1].0, "merge");
    assert_eq!(forge.f.writes()[1].1, json!({ "squash": true }));
    // A person's own write goes straight out; a style GitLab lacks doesn't.
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{block}/call/merge"), Some(json!({ "style": "rebase-merge" })));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("merge or squash"), "{body}");
    assert_eq!(forge.f.writes().len(), 2);
}

#[test]
fn with_no_glab_login_it_reads_anonymously_and_refuses_writes() {
    let dir = scratch("anon");
    let forge = Forge::start(&dir, "jhgaylor", Some("jhgaylor"));
    forge.glab(None);
    forge.f.with(|i| {
        i.jobs[8]["status"] = json!("failed");
        i.item["head_pipeline"]["status"] = json!("failed");
    });
    let d = forge.daemon();
    let block = open(&d, &forge);
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    let ro = st["read_only"].as_str().unwrap_or_default();
    assert!(ro.starts_with(&format!("read-only: no glab login for {}", forge.host())), "{st}");
    assert!(ro.contains("glab doesn't know this host"), "{ro}");
    assert_eq!((st["login"].as_str(), st["me"].as_str()), (None, None));
    // The MR, its checks and reviews read; discussions were refused.
    assert_eq!(st["pr"]["checks"].as_array().unwrap().len(), 23);
    assert_eq!(st["pr"]["rollup"], "failure");
    assert_eq!(forge.f.gets("discussions-asked"), 1);
    assert!(st["pr"]["events"].as_array().unwrap().is_empty());
    // Nobody is "you": nothing on the rail, and no rerun through the API.
    assert_eq!(st["rerun"]["api"], false, "{st}");
    assert!(st["wants"].as_array().unwrap().is_empty(), "{st}");
    assert_eq!(info(&d, block)["attention"], "idle");
    // Writes are refused, a person's and an agent's, before the forge.
    for (method, args) in [
        ("comment", json!({ "body": "hi" })),
        ("comment", json!({ "body": "hi", "agent": true })),
        ("review", json!({ "event": "approve" })),
        ("rerun_checks", json!({})),
    ] {
        let (status, body) = d.raw("POST", &format!("/api/blocks/{block}/call/{method}"), Some(args));
        assert_eq!(status, 400, "{method}: {body}");
        assert!(body.contains("read-only: no glab login"), "{body}");
    }
    assert!(forge.f.writes().is_empty());
    assert!(d.state(block)["drafts"].as_array().unwrap().is_empty(), "no draft for a write that can't go");
    let text = d.raw("GET", &format!("/api/panes/{block}/capture?format=text"), None).1;
    assert!(text.contains(&format!("{REPO}#{N}")), "{text}");
}

#[test]
fn with_no_glab_at_all_it_says_so() {
    let dir = scratch("noglab");
    let forge = Forge::start(&dir, "someone", None);
    let d = Daemon::child_env(
        &["--wisp-token-file", "/nonexistent"],
        &[("PATH", "/usr/bin:/bin"), ("ARUGULA_FORGE_POLL_MS", "250,250")],
    );
    let block = open(&d, &forge);
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert!(st["read_only"].as_str().unwrap().contains("glab isn't installed here"), "{st}");
    assert_eq!(st["pr"]["item"]["number"], N);
    let (status, body) = d.raw("POST", &format!("/api/blocks/{block}/call/login"), Some(json!({ "name": "x" })));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("glab"), "{body}");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn the_mrs_code_from_its_merge_request_ref_on_the_merge_base() {
    let dir = scratch("code");
    let forge = Forge::start(&dir, "someone", None);
    // A project whose MR head is where GitLab keeps it, and whose target
    // moved on after the MR branched (start_sha is that tip: diffing from
    // it would show main's new file too).
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
    git(&work, &["checkout", "-q", "main"]);
    std::fs::write(work.join("main-only.txt"), "later\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "main moves on"]);
    let tip = git(&work, &["rev-parse", "HEAD"]);
    let bare = dir.join("proj.git");
    git(&dir, &["clone", "-q", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    git(&bare, &["update-ref", &format!("refs/merge-requests/{N}/head"), &head]);
    git(&bare, &["update-ref", "-d", "refs/heads/feature"]);
    let clone = dir.join("clone");
    git(&dir, &["clone", "-q", bare.to_str().unwrap(), clone.to_str().unwrap()]);
    forge.f.with(|i| {
        i.item["sha"] = json!(head);
        i.item["target_branch"] = json!("main");
        i.item["diff_refs"] = json!({ "base_sha": base, "head_sha": head, "start_sha": tip });
    });
    let d = forge.daemon();
    let block = d
        .post("/api/blocks", json!({ "type": "forge", "config": { "pr": forge.url(), "dir": clone }, "local": true }))
        ["block"]
        .as_u64()
        .unwrap();
    read(&d, block);
    let out = d.call(block, "diff", json!({ "dir": clone }));
    let wt = clone.canonicalize().unwrap().join(format!(".arugula/worktrees/pr-{N}"));
    assert_eq!(out["worktree"], wt.display().to_string(), "{out}");
    assert_eq!(out["rev_a"], base, "diff_refs.base_sha, not start_sha");
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&clone, &["rev-parse", &format!("refs/arugula/pr/{N}")]), head);
    let diff = out["block"].as_u64().unwrap();
    d.wait_for("the diff", || d.state(diff)["files"].as_array().is_some_and(|f| !f.is_empty()));
    let files: Vec<String> =
        d.state(diff)["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_owned()).collect();
    assert_eq!(files, ["a.txt", "b.txt"]);
    let out = d.call(block, "checkout", json!({}));
    let pane = out["pane"].as_u64().unwrap();
    d.wait_for("the shell's cwd", || info(&d, pane)["cwd"].as_str().is_some_and(|c| c.ends_with(&format!("pr-{N}"))));
}
