//! M38: GitHub pull request blocks, against a fake GitHub served here and
//! a stand-in `gh` on the daemon's PATH.
//!
//! The fake answers GitHub's REST API with S23's recorded `cli/cli#13788`
//! (open, a review asked of babakks, three builds red, blocked by branch
//! protection), changed as a test needs. Every GET carries an ETag, and a
//! request that sends it back (`If-None-Match`) gets a bodiless 304, as
//! GitHub does; the fake counts both per route. It takes comments, reviews,
//! merges and reruns, noting the token each came with. `ARUGULA_GITHUB_API`
//! points github.com's API at it, so the blocks open from real
//! `https://github.com/…/pull/N` links. A stand-in `tea` with no logins
//! keeps Forgejo's side away from the person's own. Nothing here talks to
//! GitHub or any other forge.
//!
//! What's checked: a second poll sends `If-None-Match`, gets 304s, and
//! re-reads no reviews or timeline; a change re-reads them (conditionally);
//! the token comes from `gh auth token --hostname github.com`, is asked
//! for again after a 401, and is held nowhere a client sees; a review asked
//! of your team; red checks with a *Rerun* that posts
//! `rerun-failed-jobs`; an agent's writes as drafts and a person's going
//! out in GitHub's shapes; the rate limit backing off; the timeline's last
//! page; and the PR's code from `refs/pull/N/head`.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use agentd::*;
use axum::{
    Json, Router,
    extract::{Path as UrlPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde_json::{Value, json};

const OWNER: &str = "owner@example.com";
const TOKEN: &str = "fake-github-token-456";
const REPO: &str = "cli/cli";
const N: u64 = 13788;
const RUN: &str = "28656994029";

fn fixture(f: &str) -> Value {
    let p = format!("{}/tests/fixtures/github/github-cli-cli-13788/{f}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[derive(Default)]
struct Inner {
    item: Value,
    reviews: Vec<Value>,
    runs: Vec<Value>,
    statuses: Vec<Value>,
    timeline: Vec<Value>,
    teams: Value,
    /// 200s and 304s by route, and how many asked with `If-None-Match`.
    full: HashMap<&'static str, u32>,
    not_modified: HashMap<&'static str, u32>,
    conditional: HashMap<&'static str, u32>,
    writes: Vec<(String, Value, String)>,
    remaining: u64,
    /// Answer reads as a secondary rate limit does (403, Retry-After).
    spent: bool,
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
    /// Something changed on the PR: GitHub moves `updated_at`.
    fn touch(i: &mut Inner) {
        i.next += 1;
        i.item["updated_at"] = json!(format!("2026-10-03T01:{:02}:{:02}Z", i.next / 60 % 60, i.next % 60));
    }
    fn full(&self, r: &str) -> u32 {
        self.with(|i| i.full.get(r).copied().unwrap_or(0))
    }
    fn not_modified(&self, r: &str) -> u32 {
        self.with(|i| i.not_modified.get(r).copied().unwrap_or(0))
    }
    fn conditional(&self, r: &str) -> u32 {
        self.with(|i| i.conditional.get(r).copied().unwrap_or(0))
    }
    fn writes(&self) -> Vec<(String, Value, String)> {
        self.with(|i| i.writes.clone())
    }
}

fn token_of(h: &HeaderMap) -> String {
    h.get("authorization").and_then(|a| a.to_str().ok()).unwrap_or("").to_owned()
}

fn limits(i: &Inner, r: &mut Response) {
    let h = r.headers_mut();
    h.insert("x-ratelimit-limit", HeaderValue::from_static("5000"));
    h.insert("x-ratelimit-remaining", HeaderValue::from_str(&i.remaining.to_string()).unwrap());
    h.insert("x-ratelimit-used", HeaderValue::from_str(&(5000 - i.remaining).to_string()).unwrap());
    let reset = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 3600;
    h.insert("x-ratelimit-reset", HeaderValue::from_str(&reset.to_string()).unwrap());
    h.insert("x-ratelimit-resource", HeaderValue::from_static("core"));
}

/// A GET as GitHub answers it: an ETag of the body, a 304 when it's sent
/// back, the rate limit's headers; 401 without the token.
fn answer(f: &Fake, route: &'static str, h: &HeaderMap, body: Value, link: Option<String>) -> Response {
    f.with(|i| {
        if token_of(h) != format!("Bearer {TOKEN}") {
            return (StatusCode::UNAUTHORIZED, Json(json!({ "message": "Bad credentials" }))).into_response();
        }
        if i.spent {
            let mut r = (StatusCode::FORBIDDEN, Json(json!({ "message": "API rate limit exceeded" }))).into_response();
            limits(i, &mut r);
            r.headers_mut().insert("retry-after", HeaderValue::from_static("2"));
            return r;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        body.to_string().hash(&mut hasher);
        let etag = format!("\"{:016x}\"", hasher.finish());
        let asked = h.get("if-none-match").and_then(|v| v.to_str().ok()).map(str::to_owned);
        if asked.is_some() {
            *i.conditional.entry(route).or_default() += 1;
        }
        let mut r = if asked.as_deref() == Some(etag.as_str()) {
            *i.not_modified.entry(route).or_default() += 1;
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            *i.full.entry(route).or_default() += 1;
            i.remaining = i.remaining.saturating_sub(1);
            Json(body).into_response()
        };
        r.headers_mut().insert("etag", HeaderValue::from_str(&etag).unwrap());
        if let Some(l) = link {
            r.headers_mut().insert("link", HeaderValue::from_str(&l).unwrap());
        }
        limits(i, &mut r);
        r
    })
}

fn guard(h: &HeaderMap) -> Option<Response> {
    (token_of(h) != format!("Bearer {TOKEN}"))
        .then(|| (StatusCode::UNAUTHORIZED, Json(json!({ "message": "Bad credentials" }))).into_response())
}

async fn user(State(f): State<Fake>, h: HeaderMap) -> Response {
    answer(&f, "user", &h, json!({ "id": 1731794, "login": "jhgaylor" }), None)
}

async fn user_teams(State(f): State<Fake>, h: HeaderMap) -> Response {
    let t = f.with(|i| i.teams.clone());
    answer(&f, "teams", &h, t, None)
}

async fn pull(State(f): State<Fake>, h: HeaderMap) -> Response {
    let it = f.with(|i| i.item.clone());
    answer(&f, "pull", &h, it, None)
}

async fn get_reviews(State(f): State<Fake>, h: HeaderMap) -> Response {
    let r = f.with(|i| json!(i.reviews));
    answer(&f, "reviews", &h, r, None)
}

async fn check_runs(State(f): State<Fake>, h: HeaderMap) -> Response {
    let r = f.with(|i| json!({ "total_count": i.runs.len(), "check_runs": i.runs }));
    answer(&f, "check-runs", &h, r, None)
}

async fn status(State(f): State<Fake>, h: HeaderMap) -> Response {
    // As GitHub: "pending" when there are no statuses at all.
    let r = f.with(|i| json!({ "state": "pending", "total_count": i.statuses.len(), "statuses": i.statuses }));
    answer(&f, "status", &h, r, None)
}

/// 100 a page, oldest first, with a `Link` to the last.
async fn timeline(State(f): State<Fake>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    let origin = f.origin.lock().unwrap().clone();
    let per: usize = q.get("per_page").and_then(|p| p.parse().ok()).unwrap_or(30);
    let page: usize = q.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
    let (body, pages) = f.with(|i| {
        let pages = i.timeline.len().div_ceil(per).max(1);
        (json!(i.timeline.iter().skip((page - 1) * per).take(per).collect::<Vec<_>>()), pages)
    });
    let route = if page == 1 { "timeline" } else { "timeline-page" };
    let link = (pages > 1)
        .then(|| format!(r#"<{origin}/repositories/1/issues/{N}/timeline?per_page={per}&page={pages}>; rel="last""#));
    answer(&f, route, &h, body, link)
}

async fn moved_timeline(
    State(f): State<Fake>,
    h: HeaderMap,
    UrlPath(_): UrlPath<(String, String)>,
    q: Query<HashMap<String, String>>,
) -> Response {
    timeline(State(f), h, q).await
}

async fn repo(State(f): State<Fake>, h: HeaderMap) -> Response {
    answer(
        &f,
        "repo",
        &h,
        json!({ "full_name": REPO, "ssh_url": "git@github.com:cli/cli.git", "clone_url": "https://github.com/cli/cli.git" }),
        None,
    )
}

async fn comment(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Some(r) = guard(&h) {
        return r;
    }
    f.with(|i| {
        i.writes.push(("comment".into(), body.clone(), token_of(&h)));
        i.next += 1;
        let id = 5000 + i.next;
        i.timeline.push(json!({ "id": id, "event": "commented", "created_at": "2026-10-03T02:00:00Z",
            "actor": { "login": "jhgaylor" }, "user": { "login": "jhgaylor" }, "body": body["body"] }));
        i.item["comments"] = json!(i.item["comments"].as_u64().unwrap_or(0) + 1);
        Fake::touch(i);
        (
            StatusCode::CREATED,
            Json(json!({ "id": id, "html_url": format!("https://github.com/{REPO}/pull/{N}#issuecomment-{id}") })),
        )
            .into_response()
    })
}

async fn review(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Some(r) = guard(&h) {
        return r;
    }
    f.with(|i| {
        i.writes.push(("review".into(), body.clone(), token_of(&h)));
        i.next += 1;
        let id = 9000 + i.next;
        let state = match body["event"].as_str() {
            Some("APPROVE") => "APPROVED",
            Some("REQUEST_CHANGES") => "CHANGES_REQUESTED",
            _ => "COMMENTED",
        };
        i.reviews.push(json!({ "id": id, "state": state, "user": { "login": "jhgaylor" }, "body": body["body"],
            "commit_id": i.item["head"]["sha"], "submitted_at": "2026-10-03T02:00:00Z",
            "html_url": format!("https://github.com/{REPO}/pull/{N}#pullrequestreview-{id}") }));
        // GitHub drops the request once they've reviewed.
        i.item["requested_reviewers"] = json!([]);
        i.item["requested_teams"] = json!([]);
        Fake::touch(i);
        Json(json!({ "id": id, "html_url": format!("https://github.com/{REPO}/pull/{N}#pullrequestreview-{id}") }))
            .into_response()
    })
}

async fn merge(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Some(r) = guard(&h) {
        return r;
    }
    f.with(|i| {
        i.writes.push(("merge".into(), body.clone(), token_of(&h)));
        i.item["merged"] = json!(true);
        i.item["merged_at"] = json!("2026-10-03T03:00:00Z");
        i.item["state"] = json!("closed");
        Fake::touch(i);
    });
    Json(json!({ "sha": "abc", "merged": true, "message": "Pull Request successfully merged" })).into_response()
}

async fn rerun(
    State(f): State<Fake>,
    h: HeaderMap,
    UrlPath((_, _, id)): UrlPath<(String, String, String)>,
) -> Response {
    if let Some(r) = guard(&h) {
        return r;
    }
    f.with(|i| {
        i.writes.push((format!("rerun {id}"), Value::Null, token_of(&h)));
        for c in i.runs.iter_mut().filter(|c| c["conclusion"] == "failure") {
            c["status"] = json!("queued");
            c["conclusion"] = Value::Null;
        }
    });
    StatusCode::CREATED.into_response()
}

/// M37: the PR's number as an issue, given to jhgaylor.
async fn issue(State(f): State<Fake>, h: HeaderMap) -> Response {
    let mut it = f.with(|i| i.item.clone());
    if let Some(o) = it.as_object_mut() {
        o.remove("pull_request");
    }
    it["state"] = json!("open");
    it["html_url"] = json!(format!("https://github.com/{REPO}/issues/{N}"));
    it["assignees"] = json!([{ "login": "jhgaylor" }]);
    answer(&f, "issue", &h, it, None)
}

/// M37: a new issue.
async fn new_issue(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Some(r) = guard(&h) {
        return r;
    }
    f.with(|i| i.writes.push(("new issue".into(), body.clone(), token_of(&h))));
    (StatusCode::CREATED, Json(json!({ "number": N, "html_url": format!("https://github.com/{REPO}/issues/{N}") })))
        .into_response()
}

async fn missing(UrlPath(p): UrlPath<String>) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "message": format!("Not Found: {p}") }))).into_response()
}

/// The fake GitHub, on a runtime of its own, and a `gh` that knows it.
struct Hub {
    _rt: tokio::runtime::Runtime,
    f: Fake,
    origin: String,
    bin: PathBuf,
}

impl Hub {
    /// #13788 as recorded, by `author`, with these requests.
    fn start(dir: &Path, author: &str, reviewers: &[&str], teams: &[&str]) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let mut item = fixture("item.json");
        item["user"]["login"] = json!(author);
        item["requested_reviewers"] = json!(reviewers.iter().map(|r| json!({ "login": r })).collect::<Vec<_>>());
        item["requested_teams"] = json!(teams.iter().map(|t| json!({ "slug": t, "name": t })).collect::<Vec<_>>());
        let f = Fake {
            inner: Arc::new(Mutex::new(Inner {
                item,
                reviews: fixture("reviews.json").as_array().unwrap().clone(),
                runs: fixture("check_runs.json")["check_runs"].as_array().unwrap().clone(),
                statuses: vec![],
                timeline: fixture("timeline.json").as_array().unwrap().clone(),
                teams: json!([{ "slug": "code-reviewers", "organization": { "login": "cli" } }]),
                remaining: 4900,
                ..Inner::default()
            })),
            origin: Arc::default(),
        };
        let origin = rt.block_on(async {
            let p = "/repos/{o}/{r}";
            let api = Router::new()
                .route("/user", get(user))
                .route("/user/teams", get(user_teams))
                .route(p, get(repo))
                .route(&format!("{p}/pulls/{{n}}"), get(pull))
                .route(&format!("{p}/pulls/{{n}}/reviews"), get(get_reviews).post(review))
                .route(&format!("{p}/pulls/{{n}}/merge"), put(merge))
                .route(&format!("{p}/commits/{{sha}}/check-runs"), get(check_runs))
                .route(&format!("{p}/commits/{{sha}}/status"), get(status))
                .route(&format!("{p}/issues/{{n}}/timeline"), get(timeline))
                .route(&format!("{p}/issues/{{n}}"), get(issue))
                .route(&format!("{p}/issues"), post(new_issue))
                .route("/repositories/{id}/issues/{n}/timeline", get(moved_timeline))
                .route(&format!("{p}/issues/{{n}}/comments"), post(comment))
                .route(&format!("{p}/actions/runs/{{id}}/rerun-failed-jobs"), post(rerun))
                .route("/{*rest}", get(missing))
                .with_state(f.clone());
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let at = format!("http://{}", l.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(l, api).await.unwrap() });
            at
        });
        *f.origin.lock().unwrap() = origin.clone();
        let bin = dir.join("bin");
        let hub = Self { _rt: rt, f, origin, bin };
        hub.gh();
        hub
    }

    /// A stand-in `gh`: `auth token` hands out the fake's token (or, once,
    /// a stale one when `stale` exists), noting how it was asked.
    fn gh(&self) {
        std::fs::create_dir_all(&self.bin).unwrap();
        let gh = self.bin.join("gh");
        std::fs::write(
            &gh,
            format!(
                r#"#!/bin/sh
b='{bin}'
case "$1 $2" in
  "auth token") echo "$*" >> "$b/asked"
    [ "$4" = nowhere.example ] && exit 1
    if [ -e "$b/stale" ]; then rm "$b/stale"; echo stale-token; else echo "{TOKEN}"; fi ;;
  "auth status") echo '{{"hosts":{{"github.com":[{{"state":"success","active":true,"host":"github.com","login":"jhgaylor"}}]}}}}' ;;
  *) echo "the stand-in doesn't know $*" >&2; exit 2 ;;
esac
"#,
                bin = self.bin.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        // And a `tea` with no logins, so no Forgejo block here reaches the
        // person's own.
        let tea = self.bin.join("tea");
        std::fs::write(&tea, "#!/bin/sh\ncase \"$1 $2\" in \"logins list\") echo '[]' ;; *) exit 2 ;; esac\n").unwrap();
        std::fs::set_permissions(&tea, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn asked(&self) -> Vec<String> {
        std::fs::read_to_string(self.bin.join("asked")).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn daemon(&self) -> Daemon {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default());
        Daemon::child_env(
            &["--wisp-token-file", "/nonexistent", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
            &[("PATH", &path), ("ARUGULA_FORGE_POLL_MS", "250,250"), ("ARUGULA_GITHUB_API", &self.origin)],
        )
    }
}

fn scratch(tag: &str) -> Scratch {
    Scratch::new(&format!("github-{tag}"))
}

fn open(d: &Daemon, config: Value) -> u64 {
    d.post("/api/blocks", json!({ "type": "forge", "config": config, "local": true }))["block"].as_u64().unwrap()
}

fn open_pr(d: &Daemon) -> u64 {
    open(d, json!({ "pr": format!("https://github.com/{REPO}/pull/{N}") }))
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn read(d: &Daemon, block: u64) -> Value {
    d.wait_for("the first read", || d.state(block)["updated_ms"].as_u64().is_some_and(|t| t > 0));
    d.state(block)
}

#[test]
fn a_second_poll_is_304s_and_rereads_nothing() {
    let dir = scratch("etag");
    let hub = Hub::start(&dir, "someone", &[], &[]);
    // The first token gh hands out is refused: it's asked for again.
    std::fs::write(hub.bin.join("stale"), "").unwrap();
    let d = hub.daemon();
    let block = open_pr(&d);
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!(
        (st["provider"].as_str(), st["login"].as_str(), st["me"].as_str()),
        (Some("github"), Some("github.com"), Some("jhgaylor"))
    );
    assert_eq!(st["api"], hub.origin);
    assert_eq!(hub.asked().first().map(String::as_str), Some("auth token --hostname github.com"));
    assert!(hub.asked().len() >= 2, "a 401 asks gh again: {:?}", hub.asked());
    // The token is held nowhere a client or the layout sees.
    for text in [st.to_string(), d.get("/api/panes").to_string(), d.get(&format!("/api/blocks/{block}")).to_string()] {
        assert!(!text.contains(TOKEN), "the token leaked: {text}");
    }
    let it = &st["pr"]["item"];
    assert_eq!(
        (it["title"].as_str(), it["blocked"].as_bool()),
        (Some("fix: preserve percent-encoded path in DisplayURL"), Some(true))
    );
    assert_eq!(it["head"]["repo"], "happysnaker/cli", "from a fork");
    assert_eq!(st["pr"]["rollup"], "failure");
    assert_eq!(st["pr"]["checks"].as_array().unwrap().len(), 11, "check runs, and no statuses");
    assert_eq!(st["rate"]["remaining"].as_u64().map(|r| r > 4800), Some(true), "{}", st["rate"]);

    // Polls (every 250 ms here) with nothing changed: the item, check
    // runs and status are asked with If-None-Match and answered 304; the
    // reviews and timeline aren't asked at all.
    let (reviews, timeline) = (hub.f.full("reviews"), hub.f.full("timeline"));
    assert_eq!((reviews, timeline), (1, 1));
    // A poll asks the item, then check runs, then status: wait for all
    // three, or the last can be one behind.
    d.wait_for("polls", || ["pull", "check-runs", "status"].iter().all(|r| hub.f.conditional(r) >= 4));
    for r in ["pull", "check-runs", "status"] {
        assert_eq!(hub.f.full(r), 1, "{r} read whole again");
        assert!(hub.f.conditional(r) >= 4, "{r} not conditional");
    }
    assert_eq!(hub.f.full("reviews") + hub.f.not_modified("reviews"), 1, "reviews re-read");
    assert_eq!(hub.f.full("timeline") + hub.f.not_modified("timeline"), 1, "the timeline re-read");
    // The block's state is published after the poll the fake counted.
    d.wait_for("the 304s in the state", || d.state(block)["rate"]["not_modified"].as_u64().unwrap_or(0) >= 12);
    let st = d.state(block);
    let used = st["rate"]["used"].as_u64().unwrap();
    d.wait_for("more polls", || hub.f.not_modified("pull") >= 8);
    assert_eq!(d.state(block)["rate"]["used"].as_u64(), Some(used), "an unchanged poll cost points");

    // A comment moves the item: the rest is read again, conditionally —
    // the reviews didn't change (304), the timeline did (200).
    hub.f.with(|i| {
        i.timeline.push(json!({ "id": 7001, "event": "commented", "created_at": "2099-01-01T00:00:00Z",
            "actor": { "login": "sam" }, "user": { "login": "sam" }, "body": "a new comment" }));
        Fake::touch(i);
    });
    d.wait_for("the comment", || {
        d.state(block)["pr"]["events"].as_array().unwrap().iter().any(|e| e["body"] == "a new comment")
    });
    assert_eq!((hub.f.full("reviews"), hub.f.not_modified("reviews")), (1, 1));
    assert_eq!(hub.f.full("timeline"), 2);
    // In the log, so search finds it.
    let hits = d.get("/api/search?re=a%20new%20comment");
    assert!(hits.as_array().unwrap().iter().any(|h| h["pane"] == block), "{hits}");
}

#[test]
fn a_review_asked_of_your_team_approved_from_the_rail() {
    let dir = scratch("team");
    let hub = Hub::start(&dir, "someone", &[], &["code-reviewers"]);
    let d = hub.daemon();
    let block = open_pr(&d);
    let st = read(&d, block);
    assert_eq!(st["pr"]["item"]["requested"], json!([{ "team": "cli/code-reviewers" }]), "{st}");
    d.wait_for("attention", || info(&d, block)["reason"]["kind"] == "gate");
    let r = info(&d, block)["reason"].clone();
    assert!(
        r["headline"].as_str().unwrap().starts_with("cli/cli#13788 review requested from cli/code-reviewers"),
        "{r}"
    );
    assert_eq!(r["bundle"], format!("forge:{}/{REPO}", hub.origin.trim_start_matches("http://")));
    let out = d.post("/api/attention/act", json!({ "action": "allow", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    let w = hub.f.writes();
    assert_eq!((w[0].0.as_str(), w[0].1["event"].as_str()), ("review", Some("APPROVE")));
    assert_eq!(w[0].1.get("body"), None, "an approval with no text sends none");
    assert_eq!(w[0].2, format!("Bearer {TOKEN}"));
    d.wait_for("the gate to go", || info(&d, block)["reason"].is_null());
    assert_eq!(info(&d, block)["answered"]["name"], OWNER);
}

#[test]
fn red_checks_on_your_pr_rerun_from_the_rail() {
    let dir = scratch("rerun");
    let hub = Hub::start(&dir, "jhgaylor", &[], &[]);
    let d = hub.daemon();
    let block = open_pr(&d);
    read(&d, block);
    d.wait_for("failed", || info(&d, block)["reason"]["kind"] == "failed");
    let i = info(&d, block);
    assert_eq!(i["reason"]["actions"], json!(["rerun", "dismiss"]), "{i}");
    assert!(i["reason"]["headline"].as_str().unwrap().contains("3 checks failed: build (ubuntu-latest)"), "{i}");
    let rerun = d.state(block)["rerun"].clone();
    assert_eq!((rerun["api"].as_bool(), rerun["runs"].as_u64()), (Some(true), Some(3)), "{rerun}");

    let out = d.post("/api/attention/act", json!({ "action": "rerun", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    let w = hub.f.writes();
    assert_eq!(w.len(), 1, "one workflow run, rerun once: {w:?}");
    assert_eq!(w[0].0, format!("rerun {RUN}"));
    assert_eq!(w[0].2, format!("Bearer {TOKEN}"));
    // The jobs queue again: running, not failed.
    d.wait_for("running", || d.state(block)["pr"]["rollup"] == "running");
    d.wait_for("the reason to go", || info(&d, block)["reason"]["kind"] != "failed");
    let hist = d.get(&format!("/api/history?pane={block}"));
    assert!(
        hist.as_array()
            .unwrap()
            .iter()
            .any(|h| h["by"] == OWNER && h["text"] == "a rerun of the checks on cli/cli#13788"),
        "{hist}"
    );
}

#[test]
fn an_agents_writes_are_drafts_and_a_persons_go_out() {
    let dir = scratch("writes");
    let hub = Hub::start(&dir, "jhgaylor", &[], &[]);
    let d = hub.daemon();
    let block = open_pr(&d);
    read(&d, block);

    // An agent's comment and rerun: drafts, nothing sent.
    let one = d.call(block, "comment", json!({ "body": "Looks right to me", "agent": true }));
    let two = d.call(block, "rerun_checks", json!({ "agent": true }));
    assert_eq!((one["status"].as_str(), two["status"].as_str()), (Some("waiting"), Some("waiting")));
    assert!(hub.f.writes().is_empty(), "an agent's write went out");
    d.wait_for("the card", || info(&d, block)["ask"]["id"] == one["draft"]);
    d.post(
        &format!("/api/blocks/{block}/call/answer"),
        json!({ "id": one["draft"], "content": { "body": "Looks right to me, thanks" } }),
    );
    d.wait_for("the comment", || hub.f.writes().len() == 1);
    let w = hub.f.writes();
    assert_eq!((w[0].0.as_str(), w[0].1["body"].as_str()), ("comment", Some("Looks right to me, thanks")));
    d.wait_for("the rerun's card", || info(&d, block)["ask"]["id"] == two["draft"]);
    assert_eq!(info(&d, block)["ask"]["message"], "an agent drafted a rerun of the checks on cli/cli#13788");
    d.post(&format!("/api/blocks/{block}/call/decline"), json!({ "id": two["draft"] }));
    d.wait_for("no card", || info(&d, block)["ask"].is_null());
    assert_eq!(hub.f.writes().len(), 1, "the dropped rerun went out");

    // A person's go straight out, in GitHub's shapes.
    let out = d.call(block, "review", json!({ "event": "request_changes", "body": "split it" }));
    assert!(out["url"].as_str().unwrap().contains("#pullrequestreview-"), "{out}");
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{block}/call/merge"), Some(json!({ "style": "fast-forward-only" })));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("merge, squash or rebase"), "{body}");
    d.call(block, "merge", json!({ "style": "squash" }));
    let w = hub.f.writes();
    assert_eq!((w[1].1["event"].as_str(), w[1].1["body"].as_str()), (Some("REQUEST_CHANGES"), Some("split it")));
    assert_eq!((w[2].0.as_str(), w[2].1["merge_method"].as_str()), ("merge", Some("squash")));
    assert_eq!(w.len(), 3);
    d.wait_for("merged", || d.state(block)["pr"]["item"]["state"] == "merged");
    // A GitHub block's login is gh's, not picked.
    let (status, body) = d.raw("POST", &format!("/api/blocks/{block}/call/login"), Some(json!({ "name": "x" })));
    assert_eq!(status, 400, "{body}");
}

#[test]
fn a_low_rate_limit_backs_off_and_says_so() {
    let dir = scratch("limit");
    let hub = Hub::start(&dir, "someone", &[], &[]);
    hub.f.with(|i| i.remaining = 40);
    let d = hub.daemon();
    let block = open_pr(&d);
    let st = read(&d, block);
    let polls = hub.f.conditional("pull") + hub.f.full("pull");
    assert!(st["rate"]["backoff"].as_str().unwrap().starts_with("GitHub's rate limit is low ("), "{}", st["rate"]);
    // Polls go on (every 250 ms) but ask GitHub nothing for a minute.
    let p = st["polls"].as_u64().unwrap();
    d.wait_for("polls", || d.state(block)["polls"].as_u64().unwrap() >= p + 4);
    assert_eq!(hub.f.conditional("pull") + hub.f.full("pull"), polls, "it polled GitHub while low");
    assert_eq!(d.state(block)["error"], Value::Null);
}

#[test]
fn a_spent_rate_limit_waits_for_retry_after() {
    let dir = scratch("spent");
    let hub = Hub::start(&dir, "someone", &[], &[]);
    let d = hub.daemon();
    let block = open_pr(&d);
    read(&d, block);
    hub.f.with(|i| {
        i.spent = true;
        Fake::touch(i);
    });
    d.wait_for("the limit", || {
        d.state(block)["rate"]["backoff"].as_str().is_some_and(|b| b.contains("API rate limit exceeded"))
    });
    let st = d.state(block);
    assert!(st["error"].as_str().is_none_or(|e| e.contains("rate limit")), "{st}");
    assert!(st["pr"]["item"]["title"].is_string(), "what it read stands");
    // Retry-After passes: it reads again.
    let full = hub.f.full("pull");
    hub.f.with(|i| i.spent = false);
    d.wait_for("reading again", || d.state(block)["rate"]["backoff"].is_null());
    d.wait_for("the item read", || hub.f.full("pull") > full);
    assert_eq!(d.state(block)["error"], Value::Null);
}

#[test]
fn the_timelines_newest_page() {
    let dir = scratch("pages");
    let hub = Hub::start(&dir, "someone", &[], &[]);
    // 120 comments: two pages of 100; the newest 50 are 70..119.
    hub.f.with(|i| {
        i.timeline = (0..120)
            .map(|n| json!({ "id": 100 + n, "event": "commented", "created_at": format!("2026-10-01T00:{:02}:{:02}Z", n / 60, n % 60),
                "actor": { "login": "a" }, "user": { "login": "a" }, "body": format!("comment {n}") }))
            .collect();
    });
    let d = hub.daemon();
    let block = open_pr(&d);
    let st = read(&d, block);
    let events = st["pr"]["events"].as_array().unwrap();
    assert_eq!(events.len(), 50);
    assert_eq!((events[0]["body"].as_str(), events[49]["body"].as_str()), (Some("comment 70"), Some("comment 119")));
    assert_eq!(hub.f.full("timeline-page"), 1);
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn a_forks_pr_from_its_pull_ref_and_owner_repo_from_a_github_clone() {
    let dir = scratch("code");
    let hub = Hub::start(&dir, "someone", &[], &[]);
    // A remote with the fork's PR head where GitHub keeps it (no branch
    // of it there), and a clone whose origin says github.com.
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "trunk"]);
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "t")] {
        git(&work, &["config", k, v]);
    }
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "base"]);
    let base = git(&work, &["rev-parse", "HEAD"]);
    git(&work, &["checkout", "-qb", "fork-branch"]);
    std::fs::write(work.join("b.txt"), "new\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "feature"]);
    let head = git(&work, &["rev-parse", "HEAD"]);
    git(&work, &["checkout", "-q", "trunk"]);
    std::fs::write(work.join("c.txt"), "later on trunk\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "trunk moves on"]);
    let bare = dir.join("remote.git");
    git(&dir, &["clone", "-q", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    git(&bare, &["update-ref", &format!("refs/pull/{N}/head"), &head]);
    git(&bare, &["update-ref", "-d", "refs/heads/fork-branch"]);
    let clone = dir.join("clone");
    git(&dir, &["clone", "-q", bare.to_str().unwrap(), clone.to_str().unwrap()]);
    hub.f.with(|i| {
        i.item["head"]["sha"] = json!(head);
        i.item["base"]["ref"] = json!("trunk");
    });
    let d = hub.daemon();

    // OWNER/REPO#N in a clone whose remote is GitHub's: a GitHub block.
    git(&clone, &["remote", "set-url", "origin", "git@github.com:someone/elsewhere.git"]);
    let block = open(&d, json!({ "pr": format!("{REPO}#{N}"), "dir": clone }));
    let st = read(&d, block);
    assert_eq!((st["provider"].as_str(), st["error"].as_str()), (Some("github"), None), "{st}");
    // N alone in a clone of cli/cli on github.com: the same.
    git(&clone, &["remote", "set-url", "origin", "https://github.com/cli/cli.git"]);
    let block = open(&d, json!({ "pr": N.to_string(), "dir": clone }));
    let st = read(&d, block);
    assert_eq!(
        (st["provider"].as_str(), st["repo"].as_str(), st["dir"].as_str()),
        (Some("github"), Some(REPO), clone.to_str()),
        "{st}"
    );

    // The code: refs/pull/N/head, the merge base worked out (GitHub
    // doesn't say), and the diff is the fork's change only.
    git(&clone, &["remote", "set-url", "origin", bare.to_str().unwrap()]);
    let out = d.call(block, "diff", json!({}));
    let wt = clone.canonicalize().unwrap().join(format!(".arugula/worktrees/pr-{N}"));
    assert_eq!(out["worktree"], wt.display().to_string(), "{out}");
    assert_eq!(out["rev_a"], base, "the merge base, not trunk's tip");
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), head);
    let diff = out["block"].as_u64().unwrap();
    d.wait_for("the diff", || d.state(diff)["files"].as_array().is_some_and(|f| !f.is_empty()));
    let files: Vec<String> =
        d.state(diff)["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_owned()).collect();
    assert_eq!(files, ["b.txt"]);
}

#[test]
fn a_host_gh_is_logged_in_to_is_github_enterprise() {
    let dir = scratch("ghe");
    let hub = Hub::start(&dir, "someone", &[], &[]);
    let d = hub.daemon();
    // No tea login names it, but gh has one there: GitHub Enterprise.
    let block = open(&d, json!({ "repo": "o/r", "number": 3, "host": "ghe.example.test" }));
    d.wait_for("connected", || d.state(block)["login"] == "ghe.example.test");
    let st = d.state(block);
    assert_eq!(
        (st["provider"].as_str(), st["api"].as_str()),
        (Some("github"), Some("https://ghe.example.test/api/v3")),
        "{st}"
    );
    assert!(hub.asked().iter().any(|a| a == "auth token --hostname ghe.example.test"), "{:?}", hub.asked());
    // A host gh doesn't know stays Forgejo's (and tea's to resolve).
    let block = open(&d, json!({ "repo": "o/r", "number": 3, "host": "nowhere.example" }));
    d.wait_for("read", || d.state(block)["updated_ms"].as_u64().is_some_and(|t| t > 0));
    let st = d.state(block);
    assert_eq!(st["provider"], "forgejo");
    assert!(st["error"].as_str().unwrap().starts_with("no tea login here"), "{st}");
}

#[test]
fn an_issue_on_github_and_a_new_one() {
    // M37: GitHub's issues through the same adapter.
    let dir = scratch("issue");
    let hub = Hub::start(&dir, "someone", &[], &[]);
    let d = hub.daemon();
    let block = open(&d, json!({ "issue": format!("https://github.com/{REPO}/issues/{N}") }));
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!((st["kind"].as_str(), st["provider"].as_str()), (Some("issue"), Some("github")));
    assert_eq!(st["issue"]["item"]["assignees"], json!(["jhgaylor"]));
    d.wait_for("assigned", || info(&d, block)["reason"]["kind"] == "input");
    assert!(info(&d, block)["reason"]["headline"].as_str().unwrap().contains("assigned to you"));
    let out = d.call(block, "comment", json!({ "body": "Looking." }));
    assert!(out["url"].as_str().unwrap().contains("#issuecomment-"), "{out}");
    // A person's new issue goes out with gh's token.
    let config = json!({ "issue": "new", "repo": REPO, "provider": "github", "title": "A new one", "body": "Text." });
    let made = open(&d, config);
    d.wait_for("opened", || d.state(made)["number"] == N);
    let w = hub.f.writes();
    assert!(
        w.iter().any(|(r, b, t)| r == "new issue" && b["title"] == "A new one" && *t == format!("Bearer {TOKEN}")),
        "{w:?}"
    );
}
