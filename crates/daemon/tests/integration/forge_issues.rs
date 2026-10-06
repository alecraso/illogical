//! M37: issue blocks and *Agent on this*, against a fake Forgejo served
//! here, a stand-in `tea` on the daemon's PATH, and the stand-in agent
//! (`fake_acp.py` as Claude Code's adapter). Nothing here talks to a real
//! forge or a real agent.
//!
//! The fake answers with recorded issues (`fixtures/forgejo/`: Codeberg
//! #14556, open, assigned, a linked PR), changed as a test needs, lists
//! pull requests (for the agent's, by head branch), and takes comments and
//! new issues, noting the token each came with.
//!
//! What's checked: attention for an issue given to you, a mention, and
//! closing; comments as a person and as an agent's draft; *Agent on this*
//! making the branch and worktree from the default branch, the agent with
//! the issue as its prompt, the two in a tab of their own, and the PR block
//! joining them once (and only once) the fake lists a PR from that branch;
//! a new issue from a person going out at once, and an agent's waiting as a
//! draft that's edited and sent, or dropped.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    io::{Read, Write as _},
    os::unix::{fs::PermissionsExt, net::UnixStream},
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
    routing::get,
};
use serde_json::{Value, json};

const OWNER: &str = "owner@example.com";
const TOKEN: &str = "fake-forgejo-token-456";
const REPO: &str = "jhgaylor/illogical";
const N: u64 = 14556;

fn fixture(dir: &str, f: &str) -> Value {
    let p = format!("{}/tests/fixtures/forgejo/{dir}/{f}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[derive(Default)]
struct Inner {
    /// Issues by number.
    issues: Vec<Value>,
    timelines: Vec<(u64, Vec<Value>)>,
    pulls: Vec<Value>,
    writes: Vec<(String, Value, String)>,
    next: u64,
    pull_lists: u32,
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
    fn writes(&self) -> Vec<(String, Value, String)> {
        self.with(|i| i.writes.clone())
    }
}

impl Inner {
    fn issue(&mut self, n: u64) -> Option<&mut Value> {
        self.issues.iter_mut().find(|i| i["number"] == n)
    }
    fn timeline(&mut self, n: u64) -> &mut Vec<Value> {
        if !self.timelines.iter().any(|(k, _)| *k == n) {
            self.timelines.push((n, vec![]));
        }
        &mut self.timelines.iter_mut().find(|(k, _)| *k == n).unwrap().1
    }
    /// Something changed: Forgejo moves `updated_at`.
    fn touch(&mut self, n: u64) {
        self.next += 1;
        let at = format!("2026-10-03T02:{:02}:{:02}Z", self.next / 60 % 60, self.next % 60);
        if let Some(i) = self.issue(n) {
            i["updated_at"] = json!(at);
        }
    }
    /// An event on issue `n`, by `who`, after everything before it.
    fn event(&mut self, n: u64, mut e: Value) {
        self.next += 1;
        e["id"] = json!(90_000 + self.next);
        e["created_at"] = json!(format!("2099-01-01T00:{:02}:{:02}Z", self.next / 60 % 60, self.next % 60));
        self.timeline(n).push(e);
        self.touch(n);
    }
}

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|a| a.to_str().ok()) == Some(&format!("token {TOKEN}"))
}

macro_rules! guard {
    ($h:expr) => {
        if !authed(&$h) {
            return (StatusCode::UNAUTHORIZED, Json(json!({ "message": "token is required" }))).into_response();
        }
    };
}

fn token_of(h: &HeaderMap) -> String {
    h.get("authorization").and_then(|a| a.to_str().ok()).unwrap_or("").to_owned()
}

async fn user(h: HeaderMap) -> Response {
    guard!(h);
    Json(json!({ "id": 1, "login": "jhgaylor" })).into_response()
}

async fn teams(h: HeaderMap) -> Response {
    guard!(h);
    Json(json!([])).into_response()
}

async fn repo(State(f): State<Fake>, h: HeaderMap) -> Response {
    guard!(h);
    let origin = f.origin.lock().unwrap().clone();
    Json(json!({ "full_name": REPO, "default_branch": "main", "ssh_url": format!("ssh://git@git.example/{REPO}.git"),
        "clone_url": format!("{origin}/{REPO}.git") }))
    .into_response()
}

async fn issue(State(f): State<Fake>, h: HeaderMap, UrlPath((_, _, n)): UrlPath<(String, String, u64)>) -> Response {
    guard!(h);
    f.with(|i| match i.issue(n) {
        Some(v) => Json(v.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "message": "no such issue" }))).into_response(),
    })
}

async fn timeline(State(f): State<Fake>, h: HeaderMap, UrlPath((_, _, n)): UrlPath<(String, String, u64)>) -> Response {
    guard!(h);
    f.with(|i| {
        let tl = i.timeline(n).clone();
        ([("x-total-count", tl.len().to_string())], Json(json!(tl))).into_response()
    })
}

async fn pulls(State(f): State<Fake>, h: HeaderMap) -> Response {
    guard!(h);
    f.with(|i| {
        i.pull_lists += 1;
        Json(json!(i.pulls)).into_response()
    })
}

async fn pull(State(f): State<Fake>, h: HeaderMap, UrlPath((_, _, n)): UrlPath<(String, String, u64)>) -> Response {
    guard!(h);
    f.with(|i| match i.pulls.iter().find(|p| p["number"] == n) {
        Some(p) => Json(p.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "message": "no such pull" }))).into_response(),
    })
}

async fn status(h: HeaderMap) -> Response {
    guard!(h);
    Json(json!({ "state": "pending", "total_count": 0, "statuses": [] })).into_response()
}

async fn reviews(h: HeaderMap) -> Response {
    guard!(h);
    Json(json!([])).into_response()
}

async fn comment(
    State(f): State<Fake>,
    h: HeaderMap,
    UrlPath((_, _, n)): UrlPath<(String, String, u64)>,
    Json(body): Json<Value>,
) -> Response {
    guard!(h);
    let origin = f.origin.lock().unwrap().clone();
    f.with(|i| {
        i.writes.push((format!("comment #{n}"), body.clone(), token_of(&h)));
        i.event(n, json!({ "type": "comment", "user": { "login": "jhgaylor" }, "body": body["body"] }));
        let id = i.next;
        if let Some(x) = i.issue(n) {
            x["comments"] = json!(x["comments"].as_u64().unwrap_or(0) + 1);
        }
        (
            StatusCode::CREATED,
            Json(json!({ "id": id, "html_url": format!("{origin}/{REPO}/issues/{n}#issuecomment-{id}") })),
        )
            .into_response()
    })
}

async fn new_issue(State(f): State<Fake>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    guard!(h);
    let origin = f.origin.lock().unwrap().clone();
    f.with(|i| {
        i.writes.push(("new issue".into(), body.clone(), token_of(&h)));
        let n = 15_000 + i.writes.len() as u64;
        let mut it = fixture("codeberg-forgejo-14556", "item.json");
        it["number"] = json!(n);
        it["title"] = body["title"].clone();
        it["body"] = body["body"].clone();
        it["user"]["login"] = json!("jhgaylor");
        it["assignees"] = json!([]);
        it["labels"] = json!([]);
        it["comments"] = json!(0);
        it["html_url"] = json!(format!("{origin}/{REPO}/issues/{n}"));
        i.issues.push(it);
        (StatusCode::CREATED, Json(json!({ "number": n, "html_url": format!("{origin}/{REPO}/issues/{n}") })))
            .into_response()
    })
}

async fn missing(UrlPath(p): UrlPath<String>) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "message": format!("no {p} here") }))).into_response()
}

/// The fake Forgejo, a `tea` that knows it, and a stand-in Claude Code.
struct Forge {
    _rt: tokio::runtime::Runtime,
    f: Fake,
    origin: String,
    bin: PathBuf,
    agents: PathBuf,
}

impl Forge {
    /// Codeberg's #14556 as recorded, but on this repo: open, assigned to
    /// jhgaylor by sam, a mention of them gone (a test adds its own).
    fn start(dir: &Path) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let mut item = fixture("codeberg-forgejo-14556", "item.json");
        item["title"] = json!("Add a frobnicator to the CLI");
        item["body"] = json!("The CLI needs a `frobnicate` command.\n\nIt should frob.");
        item["assignees"] = json!([{ "login": "jhgaylor" }]);
        let mut tl: Vec<Value> = fixture("codeberg-forgejo-14556", "timeline.json").as_array().unwrap().clone();
        tl.retain(|e| e["type"] != "comment");
        for e in &mut tl {
            if e["type"] == "assignees" {
                e["user"]["login"] = json!("sam");
                e["assignee"]["login"] = json!("jhgaylor");
            }
        }
        let f = Fake {
            inner: Arc::new(Mutex::new(Inner { issues: vec![item], timelines: vec![(N, tl)], ..Inner::default() })),
            origin: Arc::default(),
        };
        let origin = rt.block_on(async {
            let p = "/api/v1/repos/{o}/{r}";
            let api = Router::new()
                .route("/api/v1/user", get(user))
                .route("/api/v1/user/teams", get(teams))
                .route(p, get(repo))
                .route(&format!("{p}/issues"), axum::routing::post(new_issue))
                .route(&format!("{p}/issues/{{n}}"), get(issue))
                .route(&format!("{p}/issues/{{n}}/timeline"), get(timeline))
                .route(&format!("{p}/issues/{{n}}/comments"), axum::routing::post(comment))
                .route(&format!("{p}/pulls"), get(pulls))
                .route(&format!("{p}/pulls/{{n}}"), get(pull))
                .route(&format!("{p}/pulls/{{n}}/reviews"), get(reviews))
                .route(&format!("{p}/commits/{{sha}}/status"), get(status))
                .route("/{*rest}", get(missing))
                .with_state(f.clone());
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let at = format!("http://{}", l.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(l, api).await.unwrap() });
            at
        });
        *f.origin.lock().unwrap() = origin.clone();
        f.with(|i| i.issues[0]["html_url"] = json!(format!("{origin}/{REPO}/issues/{N}")));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let logins =
            json!([{ "name": "forgejo", "url": origin, "ssh_host": "", "user": "jhgaylor", "default": "false" }]);
        std::fs::write(bin.join("logins.json"), logins.to_string()).unwrap();
        let tea = bin.join("tea");
        std::fs::write(
            &tea,
            format!(
                r#"#!/bin/sh
b='{bin}'
case "$1 $2 $3" in
  "logins list -o") cat "$b/logins.json" ;;
  "login helper get") cat >/dev/null; echo protocol=http; echo username=jhgaylor; echo "password={TOKEN}" ;;
  *) echo "the stand-in doesn't know $*" >&2; exit 2 ;;
esac
"#,
                bin = bin.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&tea, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Claude Code's adapter is the fake ACP agent, so no test reaches a
        // real Claude.
        let agents = dir.join("agents");
        let adapter = agents.join("claude/node_modules/.bin");
        std::fs::create_dir_all(&adapter).unwrap();
        let acp = adapter.join("claude-agent-acp");
        std::fs::write(&acp, format!("#!/bin/sh\nexec python3 {} \"$@\"\n", fake())).unwrap();
        std::fs::set_permissions(&acp, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { _rt: rt, f, origin, bin, agents }
    }

    fn daemon(&self) -> Daemon {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default());
        Daemon::child_env(
            &["--wisp-token-file", "/nonexistent", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
            &[
                ("PATH", &path),
                ("ILLOGICAL_FORGE_POLL_MS", "250,250"),
                ("ILLOGICAL_AGENTS_DIR", &self.agents.display().to_string()),
            ],
        )
    }

    fn url(&self, n: u64) -> String {
        format!("{}/{REPO}/issues/{n}", self.origin)
    }
}

fn scratch(tag: &str) -> Scratch {
    Scratch::new(&format!("issues-{tag}"))
}

fn open(d: &Daemon, req: Value) -> u64 {
    d.post("/api/blocks", req)["block"].as_u64().unwrap()
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn read(d: &Daemon, block: u64) -> Value {
    d.wait_for("the first read", || d.state(block)["updated_ms"].as_u64().is_some_and(|t| t > 0));
    d.state(block)
}

/// A POST with the header the CLI sends under an agent.
fn as_agent(d: &Daemon, path: &str, body: Value) -> (u16, String) {
    let mut s = UnixStream::connect(d.sock()).unwrap();
    let body = body.to_string();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nX-Illogical-Agent: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, out.split_once("\r\n\r\n").map(|(_, b)| b.to_owned()).unwrap_or_default())
}

/// A tool called by the stand-in agent through its own MCP server (its
/// `mcp TOOL JSON` prompt), as a real agent would.
fn agent_mcp(d: &Daemon, agent: u64, tool: &str, args: Value) -> Result<Value, String> {
    let answers = || -> Vec<String> {
        entries(&d.state(agent))
            .into_iter()
            .filter_map(|e| e["text"].as_str().and_then(|t| t.strip_prefix("MCP ")).map(str::to_owned))
            .collect()
    };
    let before = answers().len();
    d.call(agent, "send", json!({ "text": format!("mcp {tool} {args}") }));
    d.wait_for(&format!("{tool}'s answer"), || answers().len() > before);
    let r: Value = serde_json::from_str(answers().last().unwrap()).unwrap();
    if r["isError"] == true || r.get("error").is_some() {
        return Err(r.to_string());
    }
    Ok(r["structuredContent"].clone())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn an_issue_given_to_you_a_mention_and_closing() {
    let dir = scratch("attention");
    let forge = Forge::start(&dir);
    let d = forge.daemon();
    let block = open(&d, json!({ "type": "forge", "config": { "issue": forge.url(N) }, "local": true }));
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!((st["kind"].as_str(), st["me"].as_str()), (Some("issue"), Some("jhgaylor")));
    assert_eq!(st["issue"]["item"]["title"], "Add a frobnicator to the CLI");
    assert_eq!(st["issue"]["linked"][0]["number"], 14571, "the PR its timeline names");
    assert_eq!(info(&d, block)["kind"], "issue", "the swarm's kind");

    // Given to you by sam: input, bundled with the repo's other items.
    d.wait_for("attention", || info(&d, block)["reason"]["kind"] == "input");
    let i = info(&d, block);
    assert_eq!(i["attention"], "needs_input");
    let h = i["reason"]["headline"].as_str().unwrap();
    assert!(h.starts_with(&format!("{REPO}#{N} assigned to you by sam: Add a frobnicator")), "{h}");
    let host = forge.origin.trim_start_matches("http://");
    assert_eq!(i["reason"]["bundle"], format!("forge:{host}/{REPO}"));

    // A mention after it: in what it wants too; searchable in its log.
    forge.f.with(|i| {
        i.event(N, json!({ "type": "comment", "user": { "login": "kim" }, "body": "@jhgaylor which CLI flag should it take?" }))
    });
    d.wait_for("the mention", || {
        d.state(block)["wants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["kind"] == "mention" && w["why"] == "mentioned by kim")
    });
    let hits = d.get("/api/search?re=which%20CLI%20flag");
    assert!(hits.as_array().unwrap().iter().any(|h| h["pane"] == block), "{hits}");

    // A person's comment goes out; an agent's waits as a draft.
    let out = d.call(block, "comment", json!({ "body": "On it." }));
    assert!(out["url"].as_str().unwrap().contains("#issuecomment-"), "{out}");
    let draft = d.call(block, "comment", json!({ "body": "I'd take --level", "agent": true }));
    assert_eq!(draft["status"], "waiting");
    let w = forge.f.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!((w[0].0.as_str(), w[0].1["body"].as_str()), (format!("comment #{N}").as_str(), Some("On it.")));
    assert_eq!(w[0].2, format!("token {TOKEN}"));
    d.wait_for("the draft's card", || info(&d, block)["ask"]["id"] == draft["draft"]);
    assert_eq!(info(&d, block)["ask"]["message"], format!("an agent drafted a comment on {REPO}#{N}"));
    d.post(
        &format!("/api/blocks/{block}/call/answer"),
        json!({ "id": draft["draft"], "content": { "body": "I'd take --level=N" } }),
    );
    d.wait_for("the draft sent", || forge.f.writes().len() == 2);
    assert_eq!(forge.f.writes()[1].1["body"], "I'd take --level=N");
    // No reviews or merges on an issue.
    let (status, body) = d.raw("POST", &format!("/api/blocks/{block}/call/merge"), Some(json!({})));
    assert_eq!(status, 400, "{body}");

    // Closed while it's yours: done, once.
    d.wait_for("no card", || info(&d, block)["ask"].is_null());
    forge.f.with(|i| {
        i.issue(N).unwrap()["state"] = json!("closed");
        i.event(N, json!({ "type": "close", "user": { "login": "sam" } }));
    });
    d.wait_for("done", || info(&d, block)["reason"]["kind"] == "done");
    let i = info(&d, block);
    assert_eq!(i["attention"], "done");
    assert!(i["reason"]["headline"].as_str().unwrap().contains("closed"), "{i}");
    let text = d.raw("GET", &format!("/api/panes/{block}/capture?format=text"), None).1;
    assert!(text.starts_with(&format!("{REPO}#{N} Add a frobnicator to the CLI\nissue · closed")), "{text}");
    assert!(text.contains("assigned to: jhgaylor"), "{text}");
    assert!(text.contains("#14571 open"), "{text}");
    assert!(text.contains("kim commented: @jhgaylor which CLI flag"), "{text}");
}

#[test]
fn agent_on_this_makes_a_branch_an_agent_and_a_tab_and_its_pr_joins_them() {
    let dir = scratch("agent");
    let forge = Forge::start(&dir);
    // The repository: main on a remote, and the person's clone of it.
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    for (k, v) in [("user.email", "t@example.com"), ("user.name", "t")] {
        git(&work, &["config", k, v]);
    }
    std::fs::write(work.join("a.txt"), "one\n").unwrap();
    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "base"]);
    let bare = dir.join("remote.git");
    git(&dir, &["clone", "-q", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()]);
    let clone = dir.join("clone");
    git(&dir, &["clone", "-q", bare.to_str().unwrap(), clone.to_str().unwrap()]);
    // main moves on the remote after the clone: the branch starts from the
    // remote's main, not the clone's.
    std::fs::write(work.join("a.txt"), "one\ntwo\n").unwrap();
    git(&work, &["commit", "-qam", "more"]);
    git(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);
    let main = git(&work, &["rev-parse", "HEAD"]);

    let d = forge.daemon();
    // A terminal first: the issue opens beside it, in its tab.
    let term = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    let block = open(
        &d,
        json!({ "type": "forge", "config": { "issue": format!("{REPO}#{N}"), "dir": clone, "host": "git.example" },
            "split": term, "local": true }),
    );
    read(&d, block);
    assert_eq!(info(&d, block)["tab"], info(&d, term)["tab"]);

    // The clone's remote is a path here, so nothing says it's the repo's:
    // the call names it (opened from a real clone, the block knows it).
    let out = d.call(block, "agent", json!({ "prompt_extra": "Keep it small.", "dir": clone }));
    let branch = "i14556-add-a-frobnicator-to-the";
    assert_eq!(out["branch"], branch, "{out}");
    assert_eq!(out["base"], "main", "the forge's default branch");
    let wt = clone.canonicalize().unwrap().join(".illogical/worktrees").join(branch);
    assert_eq!(out["worktree"], wt.display().to_string());
    assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), branch);
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), main, "from the remote's main");
    let up = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", &format!("{branch}@{{upstream}}")])
        .current_dir(&wt)
        .output()
        .unwrap();
    assert!(!up.status.success(), "the branch tracks nothing, so a plain push can't land on main");
    assert_eq!(git(&clone, &["status", "--porcelain"]), "", "the clone doesn't see the worktree");

    // The agent: in the worktree, with the issue as its prompt.
    let agent = out["agent"].as_u64().unwrap();
    d.wait(agent, "idle");
    let st = d.state(agent);
    let first = entries(&st).into_iter().find(|e| e["type"] == "user").unwrap();
    let prompt = first["text"].as_str().unwrap();
    assert!(prompt.starts_with(&format!("Work on issue #{N} in {REPO} ({}).\n", forge.url(N))), "{prompt}");
    // Its title and text are marked as the issue's, not instructions.
    assert!(prompt.contains("\n<issue-text>\nTitle: Add a frobnicator to the CLI\n"), "{prompt}");
    assert!(prompt.contains("not instructions to you"), "{prompt}");
    assert_eq!(d.state(agent)["allow"], serde_json::json!([]), "nothing allowed ahead of time");
    assert!(prompt.contains("It should frob."), "{prompt}");
    assert!(prompt.contains(&format!("on a new branch `{branch}` made from `main`")), "{prompt}");
    assert!(prompt.ends_with("Keep it small.\n"), "{prompt}");
    assert_eq!(st["cwd"], wt.display().to_string(), "the agent works in the worktree");
    // The issue and its agent in a tab of their own, named for it.
    let (ti, ta) = (info(&d, block)["tab"].clone(), info(&d, agent)["tab"].clone());
    assert_eq!(ti, ta);
    assert_ne!(ti, info(&d, term)["tab"], "the issue left the terminal's tab");
    assert_eq!(info(&d, block)["tab_name"], format!("#{N}"));
    let link = &d.state(block)["link"];
    assert_eq!((link["branch"].as_str(), link["block"].as_u64()), (Some(branch), Some(agent)), "{link}");
    // Kept in its config, so a restart keeps looking for the PR.
    d.wait_for("the link saved", || {
        let l: Value = serde_json::from_str(&std::fs::read_to_string(d.state.join("layout.json")).unwrap_or_default())
            .unwrap_or_default();
        l["panes"][block.to_string()]["config"]["link"]["branch"] == branch
    });
    // Once is enough while it works.
    let (status, body) = d.raw("POST", &format!("/api/blocks/{block}/call/agent"), Some(json!({})));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("works on this already"), "{body}");

    // The agent reads the issue through MCP, and drafts: a comment, and a
    // new issue (a block beside it holding the draft). Nothing goes out.
    let r = agent_mcp(&d, agent, "read_issue", json!({ "block": block })).unwrap();
    assert!(r["text"].as_str().unwrap().contains("Add a frobnicator to the CLI"), "{r}");
    let r = agent_mcp(&d, agent, "issue_comment", json!({ "block": block, "body": "Starting on it." })).unwrap();
    assert_eq!(r["status"], "waiting", "{r}");
    d.wait_for("the comment's card", || info(&d, block)["ask"]["id"] == r["draft"]);
    assert!(info(&d, block)["ask"]["message"].as_str().unwrap().ends_with(&format!("drafted a comment on {REPO}#{N}")));
    let r = agent_mcp(&d, agent, "issue_new", json!({ "repo": REPO, "title": "Frobnicate needs docs" })).unwrap();
    let drafted = r["block"].as_u64().unwrap();
    assert_eq!(r["status"], "waiting", "{r}");
    d.wait_for("the new issue's card", || info(&d, drafted)["ask"]["id"] == "new");
    assert!(info(&d, drafted)["ask"]["message"].as_str().unwrap().ends_with(&format!("drafted a new issue on {REPO}")));
    assert_eq!(info(&d, drafted)["tab"], ta, "beside the agent");
    assert_eq!(d.state(drafted)["new"]["by"].as_str().map(|b| b.starts_with("mcp:")), Some(true));
    d.post(&format!("/api/blocks/{drafted}/call/decline"), json!({ "id": "new" }));
    d.post(&format!("/api/blocks/{block}/call/decline"), json!({ "id": info(&d, block)["ask"]["id"] }));
    d.post(&format!("/api/panes/{drafted}/close"), json!({}));

    // Its PR shows up on the forge (a fork's branch of the same name
    // doesn't count): the PR block joins the tab, once.
    let lists = forge.f.with(|i| i.pull_lists);
    d.wait_for("polls of the pulls", || forge.f.with(|i| i.pull_lists) >= lists + 2);
    let mut pr = fixture("forgejo-illogical-84", "item.json");
    pr["number"] = json!(91);
    pr["state"] = json!("open");
    pr["merged"] = json!(false);
    pr["head"]["ref"] = json!(branch);
    pr["head"]["repo"]["full_name"] = json!(REPO);
    pr["html_url"] = json!(format!("{}/{REPO}/pulls/91", forge.origin));
    let mut fork = pr.clone();
    fork["number"] = json!(92);
    fork["head"]["repo"]["full_name"] = json!("someone/illogical");
    forge.f.with(|i| i.pulls = vec![fork, pr]);
    d.wait_for("the PR found", || d.state(block)["link"]["pr"] == 91);
    let pr_block = d.state(block)["link"]["pr_block"].as_u64().unwrap();
    let st = read(&d, pr_block);
    assert_eq!((st["kind"].as_str(), st["number"].as_u64()), (Some("pr"), Some(91)), "{st}");
    assert_eq!(info(&d, pr_block)["tab"], ti, "in the issue's tab");
    let lists = forge.f.with(|i| i.pull_lists);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let forges: Vec<Value> =
        d.get("/api/panes").as_array().unwrap().iter().filter(|p| p["type"] == "forge").cloned().collect();
    assert_eq!(forges.len(), 2, "one issue, one PR: {forges:?}");
    assert_eq!(forge.f.with(|i| i.pull_lists), lists, "no more looking once it's found");
    let text = d.raw("GET", &format!("/api/panes/{block}/capture?format=text"), None).1;
    assert!(text.contains(&format!("agent: %{agent} (claude) on branch {branch}")), "{text}");
    assert!(text.contains(&format!("its pull request: #91 (%{pr_block})")), "{text}");
    assert!(forge.f.writes().is_empty(), "nothing was written to the forge");
}

#[test]
fn a_new_issue_from_a_person_goes_out_and_an_agents_waits() {
    let dir = scratch("new");
    let forge = Forge::start(&dir);
    let d = forge.daemon();

    // A person's: out at once, and the block is the issue.
    let config = json!({ "issue": "new", "repo": REPO, "host": "git.example", "title": "Frobs are slow", "body": "Seconds each." });
    let block = open(&d, json!({ "type": "forge", "config": config, "local": true }));
    d.wait_for("the issue opened", || d.state(block)["number"].as_u64().is_some_and(|n| n > 0));
    let w = forge.f.writes();
    assert_eq!(w.len(), 1);
    assert_eq!((w[0].1["title"].as_str(), w[0].1["body"].as_str()), (Some("Frobs are slow"), Some("Seconds each.")));
    assert_eq!(w[0].2, format!("token {TOKEN}"));
    let n = d.state(block)["number"].as_u64().unwrap();
    d.wait_for("it read back", || d.state(block)["issue"]["item"]["number"] == n);
    let st = d.state(block);
    assert_eq!((st["new"]["status"].as_str(), st["new"]["settled_by"].as_str()), (Some("sent"), Some(OWNER)), "{st}");
    let hist = d.get(&format!("/api/history?pane={block}"));
    assert!(
        hist.as_array()
            .unwrap()
            .iter()
            .any(|h| h["by"] == OWNER && h["text"] == format!("a new issue on {REPO}: Frobs are slow")),
        "{hist}"
    );

    // An agent's (the CLI's header under one): a draft on the block.
    let config =
        json!({ "issue": "new", "repo": REPO, "host": "git.example", "title": "Frobs leak", "body": "Memory grows." });
    let (status, body) = as_agent(&d, "/api/blocks", json!({ "type": "forge", "config": config, "local": true }));
    assert_eq!(status, 200, "{body}");
    let draft: u64 = serde_json::from_str::<Value>(&body).unwrap()["block"].as_u64().unwrap();
    d.wait_for("the card", || info(&d, draft)["ask"]["id"] == "new");
    let i = info(&d, draft);
    assert_eq!(i["ask"]["message"], format!("an agent drafted a new issue on {REPO}"));
    assert_eq!(i["ask"]["schema"]["properties"]["title"]["default"], "Frobs leak");
    assert_eq!(i["attention"], "needs_input");
    assert_eq!(forge.f.writes().len(), 1, "an agent's issue went out");
    assert_eq!(d.state(draft)["loading"], false);
    // Edited and sent by the owner: the edits go, and it becomes the issue.
    d.post(
        &format!("/api/blocks/{draft}/call/answer"),
        json!({ "id": "new", "content": { "title": "Frobs leak memory", "body": "Memory grows per frob." } }),
    );
    d.wait_for("sent", || forge.f.writes().len() == 2);
    assert_eq!(forge.f.writes()[1].1, json!({ "title": "Frobs leak memory", "body": "Memory grows per frob." }));
    d.wait_for("it's the issue", || d.state(draft)["issue"]["item"]["title"] == "Frobs leak memory");
    let st = d.state(draft);
    assert_eq!((st["new"]["by"].as_str(), st["new"]["settled_by"].as_str()), (Some("an agent"), Some(OWNER)));
    let hist = d.get(&format!("/api/history?pane={draft}"));
    assert!(
        hist.as_array()
            .unwrap()
            .iter()
            .any(|h| h["text"] == format!("a new issue on {REPO} (drafted by an agent): Frobs leak memory")),
        "{hist}"
    );

    // Another, dropped: nothing goes, and it says who dropped it.
    let config = json!({ "issue": "new", "repo": REPO, "host": "git.example", "title": "Nope", "agent": true });
    let dropped = open(&d, json!({ "type": "forge", "config": config, "local": true }));
    d.wait_for("its card", || info(&d, dropped)["ask"]["id"] == "new");
    d.post(&format!("/api/blocks/{dropped}/call/decline"), json!({ "id": "new" }));
    d.wait_for("dropped", || d.state(dropped)["new"]["status"] == "dropped");
    assert_eq!(d.state(dropped)["new"]["settled_by"], OWNER);
    assert!(info(&d, dropped)["ask"].is_null());
    assert_eq!(forge.f.writes().len(), 2);
}
