//! #93's GitHub boxes against github.com, for the nightly workflow
//! (`.github/workflows/forges-nightly.yml`). They need a test organization
//! of their own and two bot accounts, given as:
//!
//! - `ARUGULA_GH_TEST_REPO`: `org/repo`, a public repository both bots
//!   can write to, with `.github/workflows/arugula-red.yml` on its
//!   default branch (a job that fails on pushes to `red-*` branches; the
//!   test says what to put there if it's missing);
//! - `ARUGULA_GH_AUTHOR_TOKEN`, `ARUGULA_GH_REVIEWER_TOKEN`: the bots'
//!   tokens (contents, pull requests, issues and actions: read and write
//!   on that repository);
//! - for the App: `ARUGULA_GH_APP_ID` and `ARUGULA_GH_APP_PRIVATE_KEY`
//!   (a copy of Arugula's App, installed on the test organization, with
//!   pull request and issue comment events).
//!
//! A test whose variables aren't all set says SKIP and passes, so the
//! workflow and `just check` run green without them. Every test opens its
//! own branch and PR and closes them when done, whatever happens.
//!
//! What's checked: a review asked of you on the rail is approved from it
//! and GitHub has the approval as you; a red Actions check is rerun from
//! the rail and GitHub starts a second attempt; a box with no `gh` login
//! reads a PR through a real installation token of the App (minted as
//! control mints them: one repository, read-only) and refuses writes,
//! and GitHub itself refuses that token a write; GitHub delivers the
//! App's webhook for a comment, and the poke it becomes makes the block
//! read the comment at once.
//!
//! Control itself is stood in for here (its relay socket and token
//! endpoint, as in forge_live.rs), and the App's deliveries are read back
//! through GitHub's API rather than received: a runner has no public URL.
//! What that leaves out is control checking GitHub's signature on a real
//! delivery; that needs a control the App can reach.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use agentd::*;
use arugula_e2e::{Cert, DeviceKeys, Kind};
use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};

const OWNER: &str = "owner@example.com";
const API: &str = "https://api.github.com";
const RED_WORKFLOW: &str = ".github/workflows/arugula-red.yml";

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.trim().is_empty())
}

/// The variables a test needs, or `None` and a SKIP line.
fn need(test: &str, keys: &[&str]) -> Option<Vec<String>> {
    let got: Vec<Option<String>> = keys.iter().map(|k| env(k)).collect();
    let missing: Vec<&str> = keys.iter().zip(&got).filter(|(_, v)| v.is_none()).map(|(k, _)| *k).collect();
    if !missing.is_empty() {
        eprintln!("SKIP {test}: not set: {}", missing.join(", "));
        return None;
    }
    Some(got.into_iter().map(Option::unwrap).collect())
}

/// github.com's API, as one of the bots or as the App.
struct Gh {
    rt: tokio::runtime::Runtime,
    http: reqwest::Client,
    repo: String,
}

impl Gh {
    fn new(repo: &str) -> Self {
        Self {
            rt: tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap(),
            http: reqwest::Client::new(),
            repo: repo.to_owned(),
        }
    }

    /// `auth` is the whole Authorization value (`token …`, `Bearer …`).
    fn try_api(&self, method: &str, path: &str, auth: &str, body: Option<Value>) -> (u16, Value) {
        let url = if path.starts_with("http") { path.to_owned() } else { format!("{API}{path}") };
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut req = self
            .http
            .request(m, &url)
            .header("Authorization", auth)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "arugula-tests");
        if let Some(b) = body {
            req = req.json(&b);
        }
        self.rt.block_on(async {
            let r = req.send().await.unwrap_or_else(|e| panic!("{method} {url}: {e}"));
            let status = r.status().as_u16();
            let text = r.text().await.unwrap_or_default();
            (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
        })
    }

    fn api(&self, method: &str, path: &str, auth: &str, body: Option<Value>) -> Value {
        let (status, v) = self.try_api(method, path, auth, body);
        assert!((200..300).contains(&status), "{method} {path}: {status} {v}");
        v
    }

    fn login(&self, token: &str) -> String {
        self.api("GET", "/user", &format!("token {token}"), None)["login"].as_str().unwrap().to_owned()
    }

    /// A branch off the default one with one new file (or the file given),
    /// and a PR from it, closed again when the guard goes.
    fn pr(&self, author: &str, branch: &str, file: (&str, &str)) -> Pr<'_> {
        let auth = format!("token {author}");
        let repo = self.api("GET", &format!("/repos/{}", self.repo), &auth, None);
        let base = repo["default_branch"].as_str().unwrap().to_owned();
        let sha = self.api("GET", &format!("/repos/{}/git/ref/heads/{base}", self.repo), &auth, None)["object"]["sha"]
            .as_str()
            .unwrap()
            .to_owned();
        self.api(
            "POST",
            &format!("/repos/{}/git/refs", self.repo),
            &auth,
            Some(json!({ "ref": format!("refs/heads/{branch}"), "sha": sha })),
        );
        let mut pr = Pr { gh: self, auth: auth.clone(), branch: branch.to_owned(), number: 0, url: String::new() };
        let content = base64::engine::general_purpose::STANDARD.encode(file.1);
        self.api(
            "PUT",
            &format!("/repos/{}/contents/{}", self.repo, file.0),
            &auth,
            Some(json!({ "message": format!("{branch}: {}", file.0), "content": content, "branch": branch })),
        );
        let made = self.api(
            "POST",
            &format!("/repos/{}/pulls", self.repo),
            &auth,
            Some(json!({ "head": branch, "base": base, "title": format!("Arugula test {branch}"),
                "body": "Opened by Arugula's nightly forge tests; closed when they finish." })),
        );
        pr.number = made["number"].as_u64().unwrap();
        pr.url = made["html_url"].as_str().unwrap().to_owned();
        pr
    }
}

/// A test's PR: closed, and its branch deleted, when dropped.
struct Pr<'a> {
    gh: &'a Gh,
    auth: String,
    branch: String,
    number: u64,
    url: String,
}

impl Drop for Pr<'_> {
    fn drop(&mut self) {
        let repo = &self.gh.repo;
        if self.number > 0 {
            self.gh.try_api(
                "PATCH",
                &format!("/repos/{repo}/pulls/{}", self.number),
                &self.auth,
                Some(json!({ "state": "closed" })),
            );
        }
        self.gh.try_api("DELETE", &format!("/repos/{repo}/git/refs/heads/{}", self.branch), &self.auth, None);
    }
}

fn unique(tag: &str) -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    format!("arugula-test/{tag}-{n}-{}", std::process::id())
}

fn script(bin: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(bin).unwrap();
    let p = bin.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A daemon whose `gh` hands out `token` (none: logged in nowhere). The
/// token is the bot's, as `gh auth login` would have stored it; a stand-in
/// keeps it out of any keyring on the runner.
fn daemon(dir: &Path, token: Option<&str>, env: &[(&str, &str)]) -> Daemon {
    let bin = dir.join("bin");
    match token {
        Some(t) => script(&bin, "gh", &format!("case \"$1 $2\" in \"auth token\") echo '{t}' ;; *) exit 2 ;; esac\n")),
        None => script(&bin, "gh", "echo 'not logged in' >&2; exit 1\n"),
    }
    script(&bin, "tea", "case \"$1 $2\" in \"logins list\") echo '[]' ;; *) exit 2 ;; esac\n");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let mut all = vec![("PATH", path.as_str()), ("ARUGULA_FORGE_POLL_MS", "2000,2000")];
    all.retain(|(k, _)| !env.iter().any(|(e, _)| e == k));
    all.extend_from_slice(env);
    Daemon::child_env(
        &["--wisp-token-file", "/nonexistent", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
        &all,
    )
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn open(d: &Daemon, url: &str) -> u64 {
    d.post("/api/blocks", json!({ "type": "forge", "config": { "pr": url }, "local": true }))["block"].as_u64().unwrap()
}

fn until(what: &str, secs: u64, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out after {secs}s waiting for {what}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
fn github_a_review_asked_of_you_is_approved_from_the_rail_as_you() {
    let Some(v) =
        need("github review", &["ARUGULA_GH_TEST_REPO", "ARUGULA_GH_AUTHOR_TOKEN", "ARUGULA_GH_REVIEWER_TOKEN"])
    else {
        return;
    };
    let (repo, author, reviewer) = (&v[0], &v[1], &v[2]);
    let gh = Gh::new(repo);
    let me = gh.login(reviewer);
    let branch = unique("review");
    let pr = gh.pr(author, &branch, (&format!("{}.txt", branch.replace('/', "-")), "from the nightly\n"));
    gh.api(
        "POST",
        &format!("/repos/{repo}/pulls/{}/requested_reviewers", pr.number),
        &format!("token {author}"),
        Some(json!({ "reviewers": [me] })),
    );

    let dir = Scratch::new("gh-review");
    let d = daemon(&dir, Some(reviewer), &[]);
    let phone = Phone::subscribe(&d);
    let block = open(&d, &pr.url);
    until("the review request on the rail", 60, || info(&d, block)["reason"]["kind"] == "gate");
    let st = d.state(block);
    assert_eq!((st["provider"].as_str(), st["me"].as_str()), (Some("github"), Some(me.as_str())), "{st}");
    let r = info(&d, block)["reason"].clone();
    assert!(r["headline"].as_str().unwrap().contains("review requested from you"), "{r}");
    assert!(phone.next().to_string().contains("review requested from you"));
    assert!(!d.get("/api/panes").to_string().contains(reviewer.as_str()), "the token leaked");

    let out = d.post("/api/attention/act", json!({ "action": "allow", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    let reviews =
        gh.api("GET", &format!("/repos/{repo}/pulls/{}/reviews", pr.number), &format!("token {author}"), None);
    assert!(
        reviews.as_array().unwrap().iter().any(|r| r["state"] == "APPROVED" && r["user"]["login"] == me.as_str()),
        "{reviews}"
    );
    until("the gate to go", 60, || info(&d, block)["reason"].is_null());
    assert_eq!(info(&d, block)["answered"]["name"], OWNER);
}

#[test]
fn github_a_red_actions_check_is_rerun_from_the_rail() {
    let Some(v) = need("github rerun", &["ARUGULA_GH_TEST_REPO", "ARUGULA_GH_AUTHOR_TOKEN"]) else { return };
    let (repo, author) = (&v[0], &v[1]);
    let gh = Gh::new(repo);
    let auth = format!("token {author}");
    let (status, _) = gh.try_api("GET", &format!("/repos/{repo}/contents/{RED_WORKFLOW}"), &auth, None);
    assert_eq!(
        status, 200,
        "{repo} needs {RED_WORKFLOW} on its default branch:\n\
         on: {{ push: {{ branches: ['red-*'] }} }}\n\
         jobs: {{ red: {{ runs-on: ubuntu-latest, steps: [ {{ run: 'echo red on purpose; exit 1' }} ] }} }}"
    );
    let branch = format!("red-{}", unique("rerun").replace('/', "-"));
    let pr = gh.pr(author, &branch, ("red.txt", "red\n"));

    let dir = Scratch::new("gh-rerun");
    let d = daemon(&dir, Some(author), &[("ARUGULA_FORGE_POLL_MS", "5000,5000")]);
    let block = open(&d, &pr.url);
    // GitHub's runners take a while: the check goes red.
    until("the failed check", 600, || info(&d, block)["reason"]["kind"] == "failed");
    let i = info(&d, block);
    assert_eq!(i["reason"]["actions"], json!(["rerun", "dismiss"]), "{i}");
    let runs = || {
        gh.api("GET", &format!("/repos/{repo}/actions/runs?branch={branch}"), &auth, None)["workflow_runs"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    let run = runs().into_iter().next().expect("the red run");
    assert_eq!(run["run_attempt"], 1);
    let id = run["id"].as_u64().unwrap();

    let out = d.post("/api/attention/act", json!({ "action": "rerun", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    until("a second attempt", 120, || {
        gh.api("GET", &format!("/repos/{repo}/actions/runs/{id}"), &auth, None)["run_attempt"].as_u64() >= Some(2)
    });
}

// ------------------------------------------------- the App, as control uses it

fn b64url(b: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// The App's JWT (RS256, `iat` a minute back, nine minutes on), signed
/// with openssl.
fn app_jwt(dir: &Path, app_id: &str, pem: &str) -> String {
    let key = dir.join("app.pem");
    std::fs::write(&key, pem.replace("\\n", "\n")).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let head = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
    let body = b64url(json!({ "iat": now - 60, "exp": now + 540, "iss": app_id }).to_string().as_bytes());
    let signing = format!("{head}.{body}");
    let mut c = Command::new("openssl")
        .args(["dgst", "-sha256", "-sign"])
        .arg(&key)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("openssl");
    use std::io::Write;
    c.stdin.take().unwrap().write_all(signing.as_bytes()).unwrap();
    let out = c.wait_with_output().unwrap();
    assert!(out.status.success(), "openssl couldn't sign with the App's key");
    let _ = std::fs::remove_file(&key);
    format!("{signing}.{}", b64url(&out.stdout))
}

/// What the stand-in control saw and says.
#[derive(Clone)]
struct Control {
    gh: Arc<(String, String, String)>,
    asked: Arc<Mutex<Vec<Value>>>,
    texts: Arc<Mutex<Vec<Value>>>,
    say: tokio::sync::broadcast::Sender<String>,
    dir: Arc<std::path::PathBuf>,
}

/// `POST /api/daemon/github/token`: a real installation token for the one
/// repository, read-only, as control's `daemon_token` mints it.
async fn token(State(c): State<Control>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    if !h.contains_key("x-arugula-auth") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    c.asked.lock().unwrap().push(b.clone());
    let (app_id, pem, login) = (&c.gh.0, &c.gh.1, &c.gh.2);
    let jwt = app_jwt(&c.dir, app_id, pem);
    let repo = b["repo"].as_str().unwrap_or_default().to_owned();
    let http = reqwest::Client::new();
    let get = |u: String| {
        http.get(u)
            .header("Authorization", format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "arugula-tests")
    };
    let inst: Value = get(format!("{API}/repos/{repo}/installation")).send().await.unwrap().json().await.unwrap();
    let Some(id) = inst["id"].as_u64() else {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": format!("the App isn't installed on {repo}: {inst}") })))
            .into_response();
    };
    let name = repo.split('/').nth(1).unwrap_or_default();
    let perms = json!({ "metadata": "read", "contents": "read", "pull_requests": "read", "issues": "read",
        "checks": "read", "statuses": "read", "actions": "read" });
    let t: Value = http
        .post(format!("{API}/app/installations/{id}/access_tokens"))
        .header("Authorization", format!("Bearer {jwt}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "arugula-tests")
        .json(&json!({ "repositories": [name], "permissions": perms }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    Json(json!({ "token": t["token"], "expires_at": t["expires_at"], "login": login, "app": "arugula-test" }))
        .into_response()
}

async fn dial(State(c): State<Control>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |sock| async move {
        let (mut tx, mut rx) = sock.split();
        let mut say = c.say.subscribe();
        loop {
            tokio::select! {
                m = rx.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                            // Control's answer to a watch: live, as it says
                            // for a repository the App is installed on.
                            if v["t"] == "forge.watch" {
                                let repos: Vec<Value> = v["repos"].as_array().into_iter().flatten()
                                    .map(|r| json!({ "repo": r, "live": true })).collect();
                                let _ = tx.send(Message::Text(json!({ "t": "forge.watching", "repos": repos }).to_string().into())).await;
                            }
                            c.texts.lock().unwrap().push(v);
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

#[test]
fn github_a_box_with_no_login_reads_through_the_app_and_its_webhook_pokes_the_block() {
    let Some(v) = need(
        "github app",
        &["ARUGULA_GH_TEST_REPO", "ARUGULA_GH_AUTHOR_TOKEN", "ARUGULA_GH_APP_ID", "ARUGULA_GH_APP_PRIVATE_KEY"],
    ) else {
        return;
    };
    let (repo, author, app_id, pem) = (&v[0], &v[1], &v[2], &v[3]);
    let gh = Gh::new(repo);
    let me = gh.login(author);
    let branch = unique("app");
    let pr = gh.pr(author, &branch, (&format!("{}.txt", branch.replace('/', "-")), "through the App\n"));
    let dir = Scratch::new("gh-app");

    // The stand-in control.
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let c = Control {
        gh: Arc::new((app_id.clone(), pem.clone(), me.clone())),
        asked: Arc::default(),
        texts: Arc::default(),
        say: tokio::sync::broadcast::channel(64).0,
        dir: Arc::new(dir.to_path_buf()),
    };
    let origin = rt.block_on(async {
        let app = Router::new()
            .route("/api/relay/dial", get(dial))
            .route("/api/daemon/github/token", post(token))
            .with_state(c.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        at
    });

    // A box with no gh login, joined to it; polls ten minutes apart.
    let d = daemon(&dir, None, &[("ARUGULA_FORGE_POLL_MS", "600000,600000"), ("ARUGULA_FORGE_LIVE_MS", "600000")]);
    let keys = DeviceKeys::load_or_create(&d.state.join("daemon.key")).unwrap();
    let cert = Cert::new(&keys, "a1", Kind::Daemon, "test");
    let saved = json!({ "url": origin, "trust": { "account": "a1", "root": keys.id() }, "cert": cert });
    std::fs::write(d.state.join("control.json"), saved.to_string()).unwrap();
    let block = open(&d, &pr.url);
    until("a read through the App", 60, || d.state(block)["pr"].is_object());
    let st = d.state(block);
    assert!(st["error"].is_null(), "{st}");
    assert!(st["read_only"].as_str().unwrap().contains("GitHub App"), "{st}");
    assert_eq!(c.asked.lock().unwrap()[0], json!({ "repo": repo }));
    for args in [json!({ "body": "hi" }), json!({ "body": "hi", "agent": true })] {
        let (code, body) = d.raw("POST", &format!("/api/blocks/{block}/call/comment"), Some(args));
        assert_ne!(code, 200, "{body}");
        assert!(body.contains("gh auth login"), "{body}");
    }
    // GitHub refuses the App's token a write too: it's read-only.
    let app_token = rt.block_on(async {
        reqwest::Client::new()
            .post(format!("{origin}/api/daemon/github/token"))
            .header("x-arugula-auth", "test")
            .json(&json!({ "repo": repo }))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned()
    });
    let (code, _) = gh.try_api(
        "POST",
        &format!("/repos/{repo}/issues/{}/comments", pr.number),
        &format!("token {app_token}"),
        Some(json!({ "body": "should not land" })),
    );
    assert_eq!(code, 403, "a read-only installation token wrote");
    until("live", 30, || d.state(block)["live"] == "webhook");

    // The author comments; GitHub delivers the App's webhook. Read back
    // from the App's deliveries, it becomes the poke control would send.
    let since = chrono_now();
    gh.api(
        "POST",
        &format!("/repos/{repo}/issues/{}/comments", pr.number),
        &format!("token {author}"),
        Some(json!({ "body": "through the App's webhook" })),
    );
    let jwt = app_jwt(&dir, app_id, pem);
    let found = std::cell::RefCell::new(Value::Null);
    until("GitHub's delivery of the comment", 120, || {
        let list = gh.api("GET", "/app/hook/deliveries?per_page=50", &format!("Bearer {jwt}"), None);
        for dv in list.as_array().into_iter().flatten() {
            if dv["event"] != "issue_comment" || dv["delivered_at"].as_str().unwrap_or("") < since.as_str() {
                continue;
            }
            let full = gh.api("GET", &format!("/app/hook/deliveries/{}", dv["id"]), &format!("Bearer {jwt}"), None);
            let p = &full["request"]["payload"];
            if p["repository"]["full_name"] == repo.as_str() && p["issue"]["number"] == pr.number {
                *found.borrow_mut() = full;
                return true;
            }
        }
        false
    });
    let full = found.take();
    assert_eq!(full["request"]["payload"]["comment"]["body"], "through the App's webhook");
    assert!(full["request"]["headers"]["X-Hub-Signature-256"].as_str().unwrap_or("").starts_with("sha256="), "{full}");
    let reads = d.state(block)["reads"].as_u64().unwrap_or(0);
    let poke = json!({ "t": "forge.poke", "poke": { "provider": "github", "host": "github.com", "repo": repo,
        "number": pr.number, "event": "issue_comment", "delivery": full["guid"] } });
    c.say.send(poke.to_string()).unwrap();
    until("the comment read after the poke", 30, || {
        d.state(block)["pr"]["events"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|e| e["body"] == "through the App's webhook")
    });
    assert!(d.state(block)["reads"].as_u64().unwrap_or(0) > reads);
    assert!(!d.get(&format!("/api/blocks/{block}")).to_string().contains(&app_token), "the App's token leaked");
}

/// Now, as GitHub writes times (`2026-10-04T12:00:00Z`).
fn chrono_now() -> String {
    let out = Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
