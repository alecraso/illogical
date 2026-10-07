//! #93: forge blocks against real forges, in containers (testnet/forges):
//! Forgejo, and GitLab CE with a shell runner. Each has two bot users,
//! `illo-author` and `illo-reviewer`, whose tokens `testnet/forges/up.sh`
//! makes and writes to `testnet/forges/.state/<forge>.json`.
//!
//! Every test makes a repository of its own, so they run in any order and
//! again. They need the containers, so a plain `cargo test` ignores them;
//! `just forges test` runs them with `--ignored` and
//! `ARUGULA_TESTNET_FORGES` (the directory with those files). Run without
//! the stack, they fail.
//!
//! What's checked, as #93's boxes ask for a person to check by hand:
//! - a review asked of you reaches the rail (and the phone) and is approved
//!   from there; the forge shows the approval as you;
//! - an agent's comment (MCP's `draft`) waits as a card, is edited and sent, and
//!   is on the forge as you, with the history naming who sent it;
//! - *Agent on this* on a real issue: the agent's branch, and its PR block
//!   joining the tab once the branch's PR is opened on the forge;
//! - *Live updates*: the block makes a real webhook, the forge delivers to
//!   the daemon through `host.docker.internal` (the host the forge is told
//!   to allow), and the block hears a comment long before its next poll;
//!   turning it off removes the hook;
//! - a red check, and on GitLab a red pipeline retried from the rail;
//! - GitLab's token from a real `glab` after `glab auth login --token`.
//!
//! The person stands in only for the token: tea and glab are told it, as
//! `tea login add` or `glab auth login` would store it. A stand-in `tea` is
//! used when there's none on PATH.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use crate::agentd;

use std::{
    io::{Read, Write as _},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use agentd::*;
use base64::Engine as _;
use serde_json::{Value, json};

const OWNER: &str = "owner@example.com";
const AUTHOR: &str = "illo-author";
const REVIEWER: &str = "illo-reviewer";

/// One forge of the stack, as up.sh described it.
struct Real {
    kind: &'static str,
    url: String,
    hook_host: String,
    users: Value,
    rt: tokio::runtime::Runtime,
    http: reqwest::Client,
}

impl Real {
    /// The forge, or `None` (and a SKIP line) when the stack isn't up.
    /// The forge, as up.sh left it; a test run without it fails.
    fn load(kind: &'static str) -> Self {
        let dir = std::env::var_os("ARUGULA_TESTNET_FORGES").unwrap_or_else(|| {
            panic!("ARUGULA_TESTNET_FORGES is not set: run these with just forges up {kind} && just forges test {kind}")
        });
        let file = PathBuf::from(dir).join(format!("{kind}.json"));
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("{}: {e} (just forges up {kind})", file.display()));
        let v: Value = serde_json::from_str(&text).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        Self {
            kind,
            url: v["url"].as_str().unwrap().trim_end_matches('/').to_owned(),
            hook_host: v["hook_host"].as_str().unwrap_or("host.docker.internal").to_owned(),
            users: v["users"].clone(),
            rt,
            http: reqwest::Client::new(),
        }
    }

    fn token(&self, user: &str) -> String {
        self.users[user]["token"].as_str().unwrap_or_else(|| panic!("no token for {user}")).to_owned()
    }

    fn api_base(&self) -> String {
        match self.kind {
            "gitlab" => format!("{}/api/v4", self.url),
            _ => format!("{}/api/v1", self.url),
        }
    }

    /// A request to the forge's API as `user`: its status and JSON.
    fn try_api(&self, method: &str, path: &str, user: &str, body: Option<Value>) -> (u16, Value) {
        let url = format!("{}{path}", self.api_base());
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut req = self.http.request(m, &url);
        req = match self.kind {
            "gitlab" => req.header("PRIVATE-TOKEN", self.token(user)),
            _ => req.header("Authorization", format!("token {}", self.token(user))),
        };
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

    fn api(&self, method: &str, path: &str, user: &str, body: Option<Value>) -> Value {
        let (status, v) = self.try_api(method, path, user, body);
        assert!((200..300).contains(&status), "{method} {path} as {user}: {status} {v}");
        v
    }

    /// Stand-ins and config for the daemon's PATH: tea or glab, logged in
    /// as `user`, and Claude Code's adapter as the fake ACP agent.
    fn bin(&self, dir: &Path, user: &str) -> PathBuf {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        if self.kind == "forgejo" {
            let logins =
                json!([{ "name": "testnet", "url": self.url, "ssh_host": "", "user": user, "default": "true" }]);
            std::fs::write(bin.join("logins.json"), logins.to_string()).unwrap();
            script(
                &bin,
                "tea",
                &format!(
                    r#"case "$1 $2 $3" in
  "logins list -o") cat '{b}/logins.json' ;;
  "login helper get") cat > '{b}/asked'; echo protocol=http; echo username={user}; echo 'password={t}' ;;
  *) echo "the stand-in doesn't know $*" >&2; exit 2 ;;
esac
"#,
                    b = bin.display(),
                    t = self.token(user)
                ),
            );
        }
        let adapter = dir.join("agents/claude/node_modules/.bin");
        std::fs::create_dir_all(&adapter).unwrap();
        script(&adapter, "claude-agent-acp", &format!("exec python3 {} \"$@\"\n", fake()));
        bin
    }

    /// A daemon whose forge login is `user`'s.
    fn daemon(&self, dir: &Path, user: &str, env: &[(&str, &str)]) -> Daemon {
        let bin = self.bin(dir, user);
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let agents = dir.join("agents").display().to_string();
        let glab = dir.join("glab-config").display().to_string();
        let mut all = vec![
            ("PATH", path.as_str()),
            ("ARUGULA_FORGE_POLL_MS", "500,500"),
            ("ARUGULA_AGENTS_DIR", agents.as_str()),
            ("GLAB_CONFIG_DIR", glab.as_str()),
        ];
        all.retain(|(k, _)| !env.iter().any(|(e, _)| e == k));
        all.extend_from_slice(env);
        Daemon::child_env(
            &["--wisp-token-file", "/nonexistent", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
            &all,
        )
    }

    /// A webhook base the forge's container reaches the daemon by.
    fn hook_base(&self, d: &Daemon) -> String {
        format!("http://{}:{}", self.hook_host, d.port)
    }
}

fn script(bin: &Path, name: &str, body: &str) {
    let p = bin.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn scratch(tag: &str) -> Scratch {
    Scratch::new(&format!("real-{tag}"))
}

/// A name no earlier run used.
fn unique(tag: &str) -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
    format!("{tag}-{}-{}", n % 100_000_000, std::process::id())
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn read(d: &Daemon, block: u64) -> Value {
    d.wait_for("the first read", || d.state(block)["updated_ms"].as_u64().is_some_and(|t| t > 0));
    d.state(block)
}

fn open(d: &Daemon, config: Value) -> u64 {
    d.post("/api/blocks", json!({ "type": "forge", "config": config, "local": true }))["block"].as_u64().unwrap()
}

/// Waits up to `secs` for `f`.
fn until(what: &str, secs: u64, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out after {secs}s waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A tool called by the stand-in agent through its own MCP server, as a
/// real agent would (forge_issues.rs has the same).
fn agent_mcp(d: &Daemon, agent: u64, tool: &str, args: Value) -> Value {
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
    assert!(r["isError"] != true && r.get("error").is_none(), "{tool}: {r}");
    r["structuredContent"].clone()
}

/// The text of a block's log.
fn capture(d: &Daemon, block: u64) -> String {
    d.raw("GET", &format!("/api/panes/{block}/capture?format=text"), None).1
}

fn no_token_in(d: &Daemon, block: u64, token: &str) {
    for text in [d.state(block).to_string(), d.get("/api/panes").to_string(), capture(d, block)] {
        assert!(!text.contains(token), "the token leaked: {text}");
    }
}

/// A POST over the daemon's socket with the header the CLI sends under an
/// agent.
#[allow(dead_code)]
fn as_agent(d: &Daemon, path: &str, body: Value) -> (u16, String) {
    let mut s = UnixStream::connect(d.sock()).unwrap();
    let body = body.to_string();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nX-Arugula-Agent: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, out.split_once("\r\n\r\n").map(|(_, b)| b.to_owned()).unwrap_or_default())
}

// ---------------------------------------------------------------- Forgejo

/// A Forgejo PR: its number, page and head.
struct Pr {
    repo: String,
    number: u64,
    url: String,
    sha: String,
}

impl Real {
    /// A public repository of the author's, with the reviewer as a
    /// collaborator.
    fn fj_repo(&self, tag: &str) -> String {
        let name = unique(tag);
        self.api(
            "POST",
            "/user/repos",
            AUTHOR,
            Some(json!({ "name": name, "auto_init": true, "default_branch": "main" })),
        );
        let repo = format!("{AUTHOR}/{name}");
        self.api(
            "PUT",
            &format!("/repos/{repo}/collaborators/{REVIEWER}"),
            AUTHOR,
            Some(json!({ "permission": "write" })),
        );
        repo
    }

    /// A branch with one new file, and a PR from it.
    fn fj_pr(&self, repo: &str, branch: &str) -> Pr {
        let content = base64::engine::general_purpose::STANDARD.encode(format!("made by {branch}\n"));
        self.api(
            "POST",
            &format!("/repos/{repo}/contents/{branch}.txt"),
            AUTHOR,
            Some(json!({ "content": content, "message": format!("add {branch}.txt"), "branch": "main", "new_branch": branch })),
        );
        let pr = self.api(
            "POST",
            &format!("/repos/{repo}/pulls"),
            AUTHOR,
            Some(json!({ "head": branch, "base": "main", "title": format!("Add {branch}"), "body": "From the testnet." })),
        );
        Pr {
            repo: repo.to_owned(),
            number: pr["number"].as_u64().unwrap(),
            url: pr["html_url"].as_str().unwrap().to_owned(),
            sha: pr["head"]["sha"].as_str().unwrap().to_owned(),
        }
    }

    fn fj_comments(&self, pr: &Pr) -> Vec<Value> {
        self.api("GET", &format!("/repos/{}/issues/{}/comments", pr.repo, pr.number), AUTHOR, None)
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

#[test]
#[ignore = "needs Forgejo in Docker: just forges up forgejo && just forges test forgejo"]
fn forgejo_a_review_asked_of_you_reaches_the_rail_and_is_approved_as_you() {
    let fj = Real::load("forgejo");
    let dir = scratch("fj-review");
    let pr = fj.fj_pr(&fj.fj_repo("review"), "feature");
    // The reviewer's daemon, with a phone subscribed, and the PR open on it
    // before anyone asks.
    let d = fj.daemon(&dir, REVIEWER, &[]);
    let phone = Phone::subscribe(&d);
    let block = open(&d, json!({ "pr": pr.url }));
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!((st["login"].as_str(), st["me"].as_str()), (Some("testnet"), Some(REVIEWER)), "{st}");
    assert_eq!(st["api"], format!("{}/api/v1", fj.url));
    assert_eq!(info(&d, block)["reason"], Value::Null);

    // The author asks for a review on Forgejo: it reaches the rail, and the
    // phone, on the next poll.
    fj.api(
        "POST",
        &format!("/repos/{}/pulls/{}/requested_reviewers", pr.repo, pr.number),
        AUTHOR,
        Some(json!({ "reviewers": [REVIEWER] })),
    );
    until("the review request on the rail", 30, || info(&d, block)["reason"]["kind"] == "gate");
    let r = info(&d, block)["reason"].clone();
    assert!(
        r["headline"].as_str().unwrap().starts_with(&format!("{}#{} review requested from you", pr.repo, pr.number)),
        "{r}"
    );
    assert_eq!(r["actions"], json!(["allow", "dismiss"]));
    let push = phone.next();
    assert!(push.to_string().contains("review requested from you"), "{push}");
    no_token_in(&d, block, &fj.token(REVIEWER));

    // Approved from the rail: Forgejo has the approval, as the reviewer.
    let out = d.post("/api/attention/act", json!({ "action": "allow", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    let reviews = fj.api("GET", &format!("/repos/{}/pulls/{}/reviews", pr.repo, pr.number), AUTHOR, None);
    let approved: Vec<&Value> = reviews.as_array().unwrap().iter().filter(|r| r["state"] == "APPROVED").collect();
    assert_eq!(approved.len(), 1, "{reviews}");
    assert_eq!(approved[0]["user"]["login"], REVIEWER);
    assert_eq!(approved[0]["commit_id"], pr.sha);
    until("the gate to go", 30, || info(&d, block)["reason"].is_null());
    assert_eq!(info(&d, block)["answered"]["name"], OWNER);
    let hist = d.get(&format!("/api/history?pane={block}"));
    let want = format!("an approval on {}#{}", pr.repo, pr.number);
    assert!(hist.as_array().unwrap().iter().any(|h| h["text"] == want && h["by"] == OWNER), "{hist}");
}

#[test]
#[ignore = "needs Forgejo in Docker: just forges up forgejo && just forges test forgejo"]
fn forgejo_an_agents_pr_comment_waits_and_goes_out_edited_as_you() {
    let fj = Real::load("forgejo");
    let dir = scratch("fj-draft");
    let pr = fj.fj_pr(&fj.fj_repo("draft"), "feature");
    let d = fj.daemon(&dir, AUTHOR, &[]);
    let agent = d.open("hi");
    d.wait(agent, "idle");
    // The agent opens the PR beside itself, as show (kind pr) does.
    let opened = agent_mcp(&d, agent, "show", json!({ "kind": "pr", "pr": pr.url }));
    let block = opened["block"].as_u64().unwrap_or_else(|| panic!("{opened}"));
    read(&d, block);

    // The agent drafts through MCP: nothing reaches Forgejo.
    let r = agent_mcp(&d, agent, "draft", json!({ "kind": "comment", "block": block, "body": "LGTM, one nit" }));
    assert_eq!(r["status"], "waiting", "{r}");
    until("the card", 20, || info(&d, block)["ask"]["id"] == r["draft"]);
    assert_eq!(info(&d, block)["ask"]["schema"]["properties"]["body"]["default"], "LGTM, one nit");
    std::thread::sleep(Duration::from_secs(1));
    assert!(fj.fj_comments(&pr).is_empty(), "an agent's comment went out");

    // Edited and sent: on Forgejo as the author, with the edit.
    d.post(
        &format!("/api/blocks/{block}/call/answer"),
        json!({ "id": r["draft"], "content": { "body": "LGTM, one small nit" } }),
    );
    until("the comment on Forgejo", 20, || !fj.fj_comments(&pr).is_empty());
    let c = fj.fj_comments(&pr);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!((c[0]["body"].as_str(), c[0]["user"]["login"].as_str()), (Some("LGTM, one small nit"), Some(AUTHOR)));
    let draft =
        || d.state(block)["drafts"].as_array().unwrap().iter().find(|x| x["id"] == r["draft"]).cloned().unwrap();
    // Forgejo has it before the daemon has its answer back.
    until("the draft settled", 20, || draft()["status"] != "waiting");
    let sent = draft();
    assert_eq!((sent["status"].as_str(), sent["settled_by"].as_str()), (Some("sent"), Some(OWNER)), "{sent}");
    assert_eq!(sent["url"], c[0]["html_url"], "{sent}");
    let hist = d.get(&format!("/api/history?pane={block}"));
    assert!(
        hist.as_array().unwrap().iter().any(|h| h["by"] == OWNER
            && h["text"]
                .as_str()
                .is_some_and(|t| t.contains("(drafted by mcp:") && t.ends_with("): LGTM, one small nit"))),
        "{hist}"
    );
    // The block reads it back from Forgejo.
    until("the comment in the timeline", 20, || {
        d.state(block)["pr"]["events"].as_array().unwrap().iter().any(|e| e["body"] == "LGTM, one small nit")
    });
}

#[test]
#[ignore = "needs Forgejo in Docker: just forges up forgejo && just forges test forgejo"]
fn forgejo_agent_on_this_works_on_a_branch_and_its_pr_joins_the_tab() {
    let fj = Real::load("forgejo");
    let dir = scratch("fj-agent");
    let repo = fj.fj_repo("agent");
    let issue = fj.api(
        "POST",
        &format!("/repos/{repo}/issues"),
        AUTHOR,
        Some(json!({ "title": "Add a frobnicator", "body": "It should frob." })),
    );
    let n = issue["number"].as_u64().unwrap();
    // The person's clone, over HTTP from Forgejo itself.
    let clone = dir.join("clone");
    git(&dir, &["clone", "-q", &format!("{}/{repo}.git", fj.url), clone.to_str().unwrap()]);
    for (k, v) in [("user.email", "agent@example.test"), ("user.name", "agent")] {
        git(&clone, &["config", k, v]);
    }

    let d = fj.daemon(&dir, AUTHOR, &[]);
    let block = open(&d, json!({ "issue": issue["html_url"], "dir": clone }));
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    let out = d.call(block, "agent", json!({ "dir": clone }));
    let branch = format!("i{n}-add-a-frobnicator");
    assert_eq!(out["branch"], branch, "{out}");
    assert_eq!(out["base"], "main");
    let wt = PathBuf::from(out["worktree"].as_str().unwrap());
    let agent = out["agent"].as_u64().unwrap();
    d.wait(agent, "idle");
    let first = entries(&d.state(agent)).into_iter().find(|e| e["type"] == "user").unwrap();
    assert!(first["text"].as_str().unwrap().contains("It should frob."), "{first}");

    // The agent's work: a commit on its branch, pushed, and its PR opened.
    std::fs::write(wt.join("frob.txt"), "frob\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-qm", "frobnicate"]);
    let auth = format!("http.extraHeader=Authorization: token {}", fj.token(AUTHOR));
    git(&wt, &["-c", &auth, "push", "-q", "origin", &branch]);
    let pr = fj.api(
        "POST",
        &format!("/repos/{repo}/pulls"),
        AUTHOR,
        Some(json!({ "head": branch, "base": "main", "title": "Frobnicate", "body": format!("Closes #{n}") })),
    );
    let pn = pr["number"].as_u64().unwrap();
    until("the PR found", 30, || d.state(block)["link"]["pr"] == pn);
    let pr_block = d.state(block)["link"]["pr_block"].as_u64().unwrap();
    let st = read(&d, pr_block);
    assert_eq!((st["kind"].as_str(), st["number"].as_u64()), (Some("pr"), Some(pn)), "{st}");
    assert_eq!(info(&d, pr_block)["tab"], info(&d, block)["tab"], "in the issue's tab");
    assert_eq!(info(&d, agent)["tab"], info(&d, block)["tab"]);
    assert!(capture(&d, block).contains(&format!("its pull request: #{pn} (%{pr_block})")));
}

#[test]
#[ignore = "needs Forgejo in Docker: just forges up forgejo && just forges test forgejo"]
fn forgejo_live_updates_make_a_real_hook_the_forge_delivers_to() {
    let fj = Real::load("forgejo");
    let dir = scratch("fj-live");
    let pr = fj.fj_pr(&fj.fj_repo("live"), "feature");
    // Polls a minute apart: anything sooner came through the hook.
    let mut d = fj.daemon(&dir, AUTHOR, &[("ARUGULA_FORGE_POLL_MS", "60000,60000")]);
    d.stop();
    let base = fj.hook_base(&d);
    d.set_env("ARUGULA_FORGE_HOOK_BASE", &base);
    d.start();
    let block = open(&d, json!({ "pr": pr.url }));
    read(&d, block);
    d.call(block, "live", json!({ "on": true }));
    until("live", 20, || d.state(block)["live"] == "webhook" && d.state(block)["hook"] == true);
    let hooks = fj.api("GET", &format!("/repos/{}/hooks", pr.repo), AUTHOR, None);
    let hooks = hooks.as_array().unwrap();
    assert_eq!(hooks.len(), 1, "{hooks:?}");
    let url = hooks[0]["config"]["url"].as_str().unwrap();
    assert!(url.starts_with(&format!("{base}/api/forge/hooks/forgejo?k=")), "{url}");
    let reads = d.state(block)["reads"].as_u64().unwrap();

    // The reviewer comments on Forgejo: the delivery reaches the daemon and
    // the block reads it, well inside a poll.
    let t = Instant::now();
    fj.api(
        "POST",
        &format!("/repos/{}/issues/{}/comments", pr.repo, pr.number),
        REVIEWER,
        Some(json!({ "body": "via the hook" })),
    );
    until("the comment through the hook", 20, || {
        d.state(block)["pr"]["events"].as_array().unwrap().iter().any(|e| e["body"] == "via the hook")
    });
    assert!(t.elapsed() < Duration::from_secs(20));
    assert!(d.state(block)["reads"].as_u64().unwrap() > reads);
    // Forgejo says the delivery went through.
    let id = hooks[0]["id"].as_u64().unwrap();
    let (status, _) = fj.try_api("POST", &format!("/repos/{}/hooks/{id}/tests", pr.repo), AUTHOR, None);
    assert_eq!(status, 204, "Forgejo couldn't test the hook");

    // Off: the hook is gone from Forgejo.
    d.call(block, "live", json!({ "on": false }));
    until("the hook removed", 20, || {
        fj.api("GET", &format!("/repos/{}/hooks", pr.repo), AUTHOR, None).as_array().unwrap().is_empty()
    });
    assert_eq!(d.state(block)["hook"], false);
}

#[test]
#[ignore = "needs Forgejo in Docker: just forges up forgejo && just forges test forgejo"]
fn forgejo_a_red_check_is_a_failure_with_its_link() {
    let fj = Real::load("forgejo");
    let dir = scratch("fj-red");
    let pr = fj.fj_pr(&fj.fj_repo("red"), "feature");
    let d = fj.daemon(&dir, AUTHOR, &[]);
    let block = open(&d, json!({ "pr": pr.url }));
    read(&d, block);
    let status = |state: &str| {
        let target = format!("{}/{}/actions/runs/1/jobs/0", fj.url, pr.repo);
        fj.api(
            "POST",
            &format!("/repos/{}/statuses/{}", pr.repo, pr.sha),
            AUTHOR,
            Some(json!({ "state": state, "context": "ci / test (push)", "target_url": target, "description": state })),
        );
    };
    status("pending");
    until("running", 20, || d.state(block)["pr"]["rollup"] == "running");
    status("failure");
    until("failed", 20, || info(&d, block)["reason"]["kind"] == "failed");
    let i = info(&d, block);
    assert!(i["reason"]["headline"].as_str().unwrap().contains("1 check failed: ci / test (push)"), "{i}");
    assert_eq!(i["reason"]["actions"], json!(["dismiss"]), "Forgejo can't rerun");
    assert_eq!(d.state(block)["rerun"]["url"], format!("{}/{}/actions/runs/1/jobs/0", fj.url, pr.repo));
}

// ---------------------------------------------------------------- GitLab

/// A GitLab MR: its project and page, and the project's id.
struct Mr {
    project: u64,
    path: String,
    iid: u64,
    url: String,
}

impl Real {
    fn gl_user_id(&self, user: &str) -> u64 {
        self.api("GET", "/user", user, None)["id"].as_u64().unwrap()
    }

    /// A public project of the author's, with the reviewer as a developer.
    fn gl_project(&self, tag: &str) -> (u64, String) {
        let p = self.api(
            "POST",
            "/projects",
            AUTHOR,
            Some(json!({ "name": unique(tag), "initialize_with_readme": true, "visibility": "public", "default_branch": "main" })),
        );
        let id = p["id"].as_u64().unwrap();
        let reviewer = self.gl_user_id(REVIEWER);
        self.api(
            "POST",
            &format!("/projects/{id}/members"),
            AUTHOR,
            Some(json!({ "user_id": reviewer, "access_level": 30 })),
        );
        (id, p["path_with_namespace"].as_str().unwrap().to_owned())
    }

    /// A branch with these files, and an MR from it.
    fn gl_mr(&self, (project, path): &(u64, String), branch: &str, files: &[(&str, &str)]) -> Mr {
        let actions: Vec<Value> =
            files.iter().map(|(f, c)| json!({ "action": "create", "file_path": f, "content": c })).collect();
        self.api(
            "POST",
            &format!("/projects/{project}/repository/commits"),
            AUTHOR,
            Some(json!({ "branch": branch, "start_branch": "main", "commit_message": format!("add {branch}"), "actions": actions })),
        );
        let mr = self.api(
            "POST",
            &format!("/projects/{project}/merge_requests"),
            AUTHOR,
            Some(json!({ "source_branch": branch, "target_branch": "main", "title": format!("Add {branch}") })),
        );
        Mr {
            project: *project,
            path: path.clone(),
            iid: mr["iid"].as_u64().unwrap(),
            url: mr["web_url"].as_str().unwrap().to_owned(),
        }
    }

    /// A real `glab` logged in as `user` with its token, as `glab auth
    /// login` leaves it; `false` when there's no glab here.
    fn glab_login(&self, dir: &Path, user: &str) -> bool {
        if Command::new("glab").arg("--version").output().is_err() {
            return false;
        }
        let cfg = dir.join("glab-config");
        std::fs::create_dir_all(&cfg).unwrap();
        let out = Command::new("glab")
            .args(["auth", "login", "--hostname", &self.host(), "--token", &self.token(user)])
            .args(["--api-protocol", "http", "--git-protocol", "http"])
            .env("GLAB_CONFIG_DIR", &cfg)
            .env("GLAB_SEND_TELEMETRY", "false")
            .env("GLAB_CHECK_UPDATE", "false")
            .output()
            .unwrap();
        assert!(out.status.success(), "glab auth login: {}", String::from_utf8_lossy(&out.stderr));
        true
    }

    fn host(&self) -> String {
        self.url.trim_start_matches("http://").to_owned()
    }

    /// A daemon whose glab is logged in as `user`: the real one if there
    /// is one (its token checked), else a stand-in that knows the token.
    fn gl_daemon(&self, dir: &Path, user: &str, env: &[(&str, &str)]) -> Daemon {
        if self.glab_login(dir, user) {
            let out = Command::new("glab")
                .args(["config", "get", "token", "--host", &self.host()])
                .env("GLAB_CONFIG_DIR", dir.join("glab-config"))
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), self.token(user), "glab config get token");
        } else {
            eprintln!("no glab here: a stand-in hands out the token");
            script(
                &self.bin(dir, user),
                "glab",
                &format!(
                    "case \"$1 $2 $3\" in\n  \"config get host\") echo '{h}' ;;\n  \"config get token\") [ \"$5\" = '{h}' ] && echo '{t}' ;;\nesac\nexit 0\n",
                    h = self.host(),
                    t = self.token(user)
                ),
            );
        }
        let mut all = vec![("GLAB_SEND_TELEMETRY", "false"), ("GLAB_CHECK_UPDATE", "false")];
        all.extend_from_slice(env);
        self.daemon(dir, user, &all)
    }
}

#[test]
#[ignore = "needs GitLab in Docker: just forges up gitlab && just forges test gitlab"]
fn gitlab_a_review_asked_of_you_is_approved_from_the_rail_with_glabs_token() {
    let gl = Real::load("gitlab");
    let dir = scratch("gl-review");
    let mr = gl.gl_mr(&gl.gl_project("review"), "feature", &[("feature.txt", "one\n")]);
    let d = gl.gl_daemon(&dir, REVIEWER, &[]);
    let phone = Phone::subscribe(&d);
    let block = open(&d, json!({ "pr": mr.url }));
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!(st["read_only"], Value::Null, "{st}");
    assert_eq!((st["provider"].as_str(), st["me"].as_str()), (Some("gitlab"), Some(REVIEWER)), "{st}");
    assert_eq!(st["repo"], mr.path);
    assert_eq!(info(&d, block)["reason"], Value::Null);

    // The author asks the reviewer: on the rail and the phone.
    let reviewer = gl.gl_user_id(REVIEWER);
    gl.api(
        "PUT",
        &format!("/projects/{}/merge_requests/{}", mr.project, mr.iid),
        AUTHOR,
        Some(json!({ "reviewer_ids": [reviewer] })),
    );
    until("the review request on the rail", 30, || info(&d, block)["reason"]["kind"] == "gate");
    let r = info(&d, block)["reason"].clone();
    assert!(
        r["headline"].as_str().unwrap().starts_with(&format!("{}#{} review requested from you", mr.path, mr.iid)),
        "{r}"
    );
    assert!(phone.next().to_string().contains("review requested from you"));
    no_token_in(&d, block, &gl.token(REVIEWER));

    // Approved: GitLab lists the reviewer's approval.
    let out = d.post("/api/attention/act", json!({ "action": "allow", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    let a = gl.api("GET", &format!("/projects/{}/merge_requests/{}/approvals", mr.project, mr.iid), AUTHOR, None);
    let by: Vec<&str> =
        a["approved_by"].as_array().unwrap().iter().filter_map(|x| x["user"]["username"].as_str()).collect();
    assert_eq!(by, [REVIEWER], "{a}");
    until("the gate to go", 30, || info(&d, block)["reason"].is_null());
    assert_eq!(info(&d, block)["answered"]["name"], OWNER);
}

#[test]
#[ignore = "needs GitLab in Docker: just forges up gitlab && just forges test gitlab"]
fn gitlab_a_red_pipeline_is_rerun_from_the_rail() {
    let gl = Real::load("gitlab");
    let dir = scratch("gl-rerun");
    let ci = "test:\n  script:\n    - echo red on purpose\n    - exit 1\n";
    let mr = gl.gl_mr(&gl.gl_project("rerun"), "feature", &[(".gitlab-ci.yml", ci)]);
    let d = gl.gl_daemon(&dir, AUTHOR, &[("ARUGULA_FORGE_POLL_MS", "1000,1000")]);
    let block = open(&d, json!({ "pr": mr.url }));
    read(&d, block);
    // The runner runs it and it fails.
    until("the failed pipeline", 240, || info(&d, block)["reason"]["kind"] == "failed");
    let i = info(&d, block);
    assert!(i["reason"]["headline"].as_str().unwrap().contains("1 check failed: test"), "{i}");
    assert_eq!(i["reason"]["actions"], json!(["rerun", "dismiss"]));
    let pipeline = d.state(block)["rerun"]["pipeline"].as_str().unwrap().to_owned();
    let jobs = |retried: bool| {
        gl.api(
            "GET",
            &format!("/projects/{}/pipelines/{pipeline}/jobs?include_retried={retried}", mr.project),
            AUTHOR,
            None,
        )
        .as_array()
        .unwrap()
        .len()
    };
    assert_eq!(jobs(true), 1);

    // Rerun from the rail: GitLab retries the job.
    let out = d.post("/api/attention/act", json!({ "action": "rerun", "pane": block }));
    assert_eq!(out["results"][0]["ok"], true, "{out}");
    until("the retried job", 30, || jobs(true) == 2);
    until("running, then red again", 240, || {
        info(&d, block)["reason"]["kind"] == "failed" || d.state(block)["pr"]["rollup"] == "running"
    });
}

#[test]
#[ignore = "needs GitLab in Docker: just forges up gitlab && just forges test gitlab"]
fn gitlab_live_updates_make_a_real_hook_the_forge_delivers_to() {
    let gl = Real::load("gitlab");
    let dir = scratch("gl-live");
    let mr = gl.gl_mr(&gl.gl_project("live"), "feature", &[("feature.txt", "one\n")]);
    let mut d = gl.gl_daemon(&dir, AUTHOR, &[("ARUGULA_FORGE_POLL_MS", "60000,60000")]);
    d.stop();
    let base = gl.hook_base(&d);
    d.set_env("ARUGULA_FORGE_HOOK_BASE", &base);
    d.start();
    let block = open(&d, json!({ "pr": mr.url }));
    read(&d, block);
    d.call(block, "live", json!({ "on": true }));
    until("live", 30, || d.state(block)["live"] == "webhook" && d.state(block)["hook"] == true);
    let hooks = gl.api("GET", &format!("/projects/{}/hooks", mr.project), AUTHOR, None);
    let hooks = hooks.as_array().unwrap();
    assert_eq!(hooks.len(), 1, "{hooks:?}");
    assert!(hooks[0]["url"].as_str().unwrap().starts_with(&format!("{base}/api/forge/hooks/gitlab?k=")), "{hooks:?}");

    gl.api(
        "POST",
        &format!("/projects/{}/merge_requests/{}/notes", mr.project, mr.iid),
        REVIEWER,
        Some(json!({ "body": "via the hook" })),
    );
    until("the note through the hook", 30, || {
        d.state(block)["pr"]["events"].as_array().unwrap().iter().any(|e| e["body"] == "via the hook")
    });

    d.call(block, "live", json!({ "on": false }));
    until("the hook removed", 30, || {
        gl.api("GET", &format!("/projects/{}/hooks", mr.project), AUTHOR, None).as_array().unwrap().is_empty()
    });
}
