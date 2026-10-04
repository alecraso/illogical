//! M43: the Fountain agent catalog, against a fake Fountain served here
//! with S24's recorded (scrubbed) agents, and a fake `~/.fountain/credentials`
//! in the daemon's scratch HOME. Nothing here talks to a real Fountain: *Run
//! on Fountain* runs the fake ACP agent as `fountain`.
//!
//! What's checked: the login (the file's profile, FOUNTAIN_API_KEY, none);
//! the key and illogical's User-Agent on every request; cards, where each
//! comes from, and the filters (kept in the config); `capture --text`; *Run
//! on Fountain* opening an agent block beside it; *Run here* refused with
//! its reason; *Spec* opening the agent-specs file, or Fountain's page;
//! `GET /api/fountain/agents`; and MCP's `list_agents` and `read_agent`
//! from an agent.
//!
//! M44 (wearing an agent here): *Run here*, `start_agent {as_fountain}`
//! and `as_fountain` on a new block run the fake ACP agent as Claude Code's
//! adapter; it records the `_meta` and MCP servers its `session/new` got.
//! Infisical, `gh` and GitHub are fakes too (a script each, and local git
//! repositories). What's checked: the bundle, the `_meta` (prompt, plugin,
//! model, `settingSources: []`), the servers with their variables resolved
//! in order and the ones left out with why, refusals, a restart, that a
//! known secret is on no surface but the adapter's stdin, and that editors
//! can't wear one.

mod agentd;

use std::{
    collections::HashMap,
    io::{Read as _, Write as _},
    net::TcpStream,
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
    routing::get,
};
use serde_json::{Value, json};

const KEY: &str = "fake-fountain-key-123";
const OWNER: &str = "owner@example.com";
const FRIEND: &str = "friend@example.com";

fn fixture(f: &str) -> Value {
    let p = format!("{}/tests/fixtures/fountain/{f}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[derive(Default)]
struct Inner {
    agents: Value,
    envs: Value,
    /// Requests by route, and the User-Agents they came with.
    gets: HashMap<&'static str, u32>,
    agents_seen: Vec<String>,
    /// Refuse every key (as an expired one is).
    deny: bool,
    /// Rows served after the recorded ones (odd-agents.json's).
    extra: Vec<Value>,
    /// MCP probes (M44), by path.
    probes: Vec<String>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Inner>>);

impl Fake {
    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }
    fn gets(&self, route: &str) -> u32 {
        self.with(|i| i.gets.get(route).copied().unwrap_or(0))
    }
}

fn guard(f: &Fake, h: &HeaderMap, route: &'static str) -> Option<Response> {
    let ua = h.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    let ok = h.get("authorization").and_then(|a| a.to_str().ok()) == Some(&format!("Bearer {KEY}"));
    f.with(|i| {
        *i.gets.entry(route).or_default() += 1;
        i.agents_seen.push(ua);
        (i.deny || !ok).then(|| {
            (StatusCode::UNAUTHORIZED, Json(json!({ "error": { "message": "invalid api key" } }))).into_response()
        })
    })
}

async fn agents(State(f): State<Fake>, h: HeaderMap) -> Response {
    if let Some(r) = guard(&f, &h, "agents") {
        return r;
    }
    Json(f.with(|i| {
        let mut all = i.agents.clone();
        all["data"].as_array_mut().unwrap().extend(i.extra.iter().cloned());
        all
    }))
    .into_response()
}

async fn agent(State(f): State<Fake>, h: HeaderMap, UrlPath(id): UrlPath<String>) -> Response {
    if let Some(r) = guard(&f, &h, "agent") {
        return r;
    }
    let a = f.with(|i| {
        i.agents["data"].as_array().unwrap().iter().chain(i.extra.iter()).find(|a| a["id"] == id.as_str()).cloned()
    });
    match a {
        Some(a) => Json(json!({ "data": a })).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
    }
}

async fn environments(State(f): State<Fake>, h: HeaderMap) -> Response {
    if let Some(r) = guard(&f, &h, "environments") {
        return r;
    }
    Json(f.with(|i| i.envs.clone())).into_response()
}

/// An MCP server that wants an OAuth sign-in (M44's probe).
async fn oauth_mcp(State(f): State<Fake>) -> Response {
    f.with(|i| i.probes.push("oauth".into()));
    (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer resource_metadata=\"x\"")], "").into_response()
}

/// One that doesn't (an initialize gets an answer).
async fn open_mcp(State(f): State<Fake>) -> Response {
    f.with(|i| i.probes.push("open".into()));
    Json(json!({ "jsonrpc": "2.0", "id": 1, "result": {} })).into_response()
}

/// The fake Fountain, on a runtime of its own, and a scratch HOME whose
/// credentials point at it.
struct Fountain {
    _rt: tokio::runtime::Runtime,
    f: Fake,
    origin: String,
    home: PathBuf,
    bin: PathBuf,
}

impl Fountain {
    fn start(dir: &Path) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        let f = Fake::default();
        f.with(|i| {
            i.agents = fixture("agents.json");
            i.envs = fixture("environments.json");
        });
        let origin = rt.block_on(async {
            let api = Router::new()
                .route("/api/agents", get(agents))
                .route("/api/agents/{id}", get(agent))
                .route("/api/environments", get(environments))
                .route("/oauth-mcp", axum::routing::post(oauth_mcp))
                .route("/open-mcp", axum::routing::post(open_mcp))
                .with_state(f.clone());
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let at = format!("http://{}", l.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(l, api).await.unwrap() });
            at
        });
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".fountain")).unwrap();
        std::fs::write(
            home.join(".fountain/credentials"),
            format!(
                "[default]\napi_key = \"{KEY}\"\nbase_url = \"{origin}\"\n\n[other]\napi_key = \"not-this-one\"\nbase_url = \"http://127.0.0.1:9\"\n"
            ),
        )
        .unwrap();
        // `fountain` is the fake ACP agent: Run on Fountain reaches no Fountain.
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fountain = bin.join("fountain");
        std::fs::write(&fountain, format!("#!/bin/sh\nexec python3 {} \"$@\"\n", fake())).unwrap();
        std::fs::set_permissions(&fountain, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { _rt: rt, f, origin, home, bin }
    }

    /// As [`Fountain::daemon`], under systemd (agents outlive a restart).
    fn service(&self, env: &[(&str, &str)]) -> Option<Daemon> {
        let home = self.home.display().to_string();
        let bin = self.bin.join("fountain").display().to_string();
        let mut all = vec![
            ("HOME", home.as_str()),
            ("ILLOGICAL_FOUNTAIN_BIN", bin.as_str()),
            ("ILLOGICAL_FOUNTAIN_POLL_MS", "60000"),
        ];
        all.extend_from_slice(env);
        Daemon::service_env(&all)
    }

    fn daemon(&self, env: &[(&str, &str)]) -> Daemon {
        let home = self.home.display().to_string();
        let bin = self.bin.join("fountain").display().to_string();
        let mut all = vec![
            ("HOME", home.as_str()),
            ("ILLOGICAL_FOUNTAIN_BIN", bin.as_str()),
            ("ILLOGICAL_FOUNTAIN_POLL_MS", "60000"),
        ];
        all.extend_from_slice(env);
        Daemon::child_env(
            &["--wisp-token-file", "/nonexistent", "--owner", OWNER, "--tailscale-socket", "/nonexistent/sock"],
            &all,
        )
    }
}

fn open(d: &Daemon, config: Value) -> u64 {
    d.post("/api/blocks", json!({ "type": "fountain", "config": config, "local": true }))["block"].as_u64().unwrap()
}

fn read(d: &Daemon, block: u64) -> Value {
    d.wait_for("the first read", || d.state(block)["loading"] == false);
    d.state(block)
}

fn info(d: &Daemon, pane: u64) -> Value {
    d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap_or_default()
}

fn layout_config(d: &Daemon, block: u64) -> Value {
    let l: Value = serde_json::from_str(&std::fs::read_to_string(d.state.join("layout.json")).unwrap_or_default())
        .unwrap_or_default();
    l["panes"][block.to_string()]["config"].clone()
}

fn names(st: &Value) -> Vec<String> {
    st["agents"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn the_catalog_filters_and_runs() {
    let dir = Scratch::new("fountain-catalog");
    let fz = Fountain::start(&dir);
    let d = fz.daemon(&[]);
    let block = open(&d, json!({ "view": "catalog" }));
    let st = read(&d, block);
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!(
        (st["total"].as_u64(), st["agents"].as_array().unwrap().len()),
        (Some(108), 108),
        "every agent, app-made too"
    );
    assert_eq!((st["profile"].as_str(), st["base_url"].as_str()), (Some("default"), Some(fz.origin.as_str())));
    assert_eq!(st["key_from"], "file");
    assert_eq!(st["profiles"], json!(["default", "other"]));
    assert_eq!(st["counts"]["source"]["agent-specs"], 23);
    // The key, and illogical's own User-Agent, on every request.
    let uas = fz.f.with(|i| i.agents_seen.clone());
    assert!(!uas.is_empty() && uas.iter().all(|u| u.starts_with("illogical/")), "{uas:?}");
    // No key in the state, the config or the log.
    assert!(!st.to_string().contains(KEY));
    // Cards: agent-specs first; environment names.
    let first = &st["agents"][0];
    assert_eq!(first["source"], "agent-specs", "{first}");
    let games = st["agents"].as_array().unwrap().iter().find(|c| c["name"] == "games").unwrap();
    assert_eq!(games["skills"], json!(["love2d", "pixijs", "screenshots-in-prs"]));
    assert_eq!(games["source"], "hand");
    assert_eq!(games["local"], true);
    let env_id = fixture("agents.json")["data"].as_array().unwrap().iter().find(|a| a["name"] == "games").unwrap()
        ["environment_id"]
        .clone();
    let env = fixture("environments.json")["data"].as_array().unwrap().iter().find(|e| e["id"] == env_id).cloned();
    assert_eq!(games["environment"], env.map_or(env_id, |e| e["name"].clone()), "its environment's name");

    // Filters: agent-specs leaves the curated ones; a skill finds designer.
    let out = d.call(block, "filter", json!({ "source": "agent-specs" }));
    assert_eq!(out["shown"], 23, "{out}");
    let st = d.state(block);
    assert_eq!(names(&st).len(), 23);
    assert!(names(&st).contains(&"pr-reviewer".to_owned()));
    d.call(block, "filter", json!({ "query": "frontend-design" }));
    assert_eq!(names(&d.state(block)), ["designer"]);
    // Kept in the config.
    d.wait_for("the filter saved", || layout_config(&d, block)["filter"]["query"] == "frontend-design");
    assert_eq!(layout_config(&d, block)["filter"]["sources"], json!(["agent-specs"]));
    assert!(!std::fs::read_to_string(d.state.join("layout.json")).unwrap().contains(KEY));
    // capture --text: the filtered list.
    let (_, text) = d.raw("GET", &format!("/api/panes/{block}/capture"), None);
    assert!(text.contains("1 of 108 agents (\"frontend-design\", source agent-specs)"), "{text}");
    assert!(text.contains("  designer [claude"), "{text}");
    d.call(block, "filter", json!({ "clear": true, "runtime": "codex" }));
    assert!(d.state(block)["agents"].as_array().unwrap().iter().all(|c| c["runtime"] == "codex"));
    d.call(block, "filter", json!({ "clear": true }));
    assert_eq!(d.state(block)["agents"].as_array().unwrap().len(), 108);
    let (status, body) = d.raw("POST", &format!("/api/blocks/{block}/call/filter"), Some(json!({ "source": "nope" })));
    assert_eq!(status, 400, "{body}");

    // Run on Fountain: an agent block beside it, as `fountain acp --agent`.
    let out = d.call(block, "run", json!({ "agent": "games" }));
    let agent = out["block"].as_u64().unwrap();
    d.wait_for("the agent block", || d.raw("GET", &format!("/api/blocks/{agent}"), None).0 == 200);
    assert_eq!(info(&d, agent)["tab"], info(&d, block)["tab"], "beside the catalog");
    d.wait_for("the agent's config saved", || layout_config(&d, agent)["fountain_agent"] == "games");
    assert_eq!(layout_config(&d, agent)["agent"], "fountain");
    d.wait_for("the agent ready", || d.state(agent)["status"] == "ready");
    d.call(agent, "send", json!({ "text": "hello" }));
    assert_eq!(d.wait(agent, "idle"), "done");
    let (status, _) = d.raw("POST", &format!("/api/blocks/{block}/call/run"), Some(json!({ "agent": "nobody" })));
    assert_eq!(status, 400);

    // Run here: not for some (the rest is in M44's tests below).
    let codex = d.state(block)["agents"].as_array().unwrap().iter().find(|c| c["runtime"] == "codex").cloned().unwrap();
    let (_, body) =
        d.raw("POST", &format!("/api/blocks/{block}/call/run_here"), Some(json!({ "agent": codex["name"] })));
    assert!(body.contains("is a codex agent"), "{body}");

    // Refresh reads again.
    let before = fz.f.gets("agents");
    d.call(block, "refresh", json!({}));
    assert_eq!(fz.f.gets("agents"), before + 1);
}

#[test]
fn spec_opens_the_file_or_the_page() {
    let dir = Scratch::new("fountain-spec");
    let fz = Fountain::start(&dir);
    // An agent-specs checkout: pr-reviewer declared in a .ts file.
    let specs = dir.join("agent-specs");
    let f = specs.join("src/agents/specialists/engineering/pr-reviewer.ts");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "import { Agent } from \"x\";\n\nexport const prReviewer = Agent({\n  name: \"pr-reviewer\",\n  description: \"reviews\",\n});\n").unwrap();
    std::fs::write(specs.join("src/agents/other.ts"), "// mentions `pr-reviewer` in prose\n").unwrap();
    let d = fz.daemon(&[]);
    let block = open(&d, json!({}));
    let st = read(&d, block);
    // The default checkout isn't in this HOME: Spec asks where it is.
    assert_eq!(st["specs"], Value::Null);
    assert!(st["specs_why"].as_str().unwrap().contains("agent-specs checkout"), "{st}");
    // ...and without one, a chant agent's Spec is its page on Fountain.
    let out = d.call(block, "spec", json!({ "agent": "pr-reviewer" }));
    assert_eq!(out["kind"], "browser", "{out}");
    let id = d.state(block)["agents"].as_array().unwrap().iter().find(|c| c["name"] == "pr-reviewer").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(out["url"], format!("{}/agents/{id}", fz.origin));

    // Told where it is (kept in the config): the file, at its line.
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{block}/call/specs"), Some(json!({ "dir": dir.join("nowhere") })));
    assert_eq!(status, 400, "{body}");
    let out = d.call(block, "specs", json!({ "dir": specs }));
    assert_eq!(out["specs"], specs.display().to_string());
    let out = d.call(block, "spec", json!({ "agent": "pr-reviewer" }));
    assert_eq!(out["kind"], "file", "{out}");
    assert_eq!(out["path"], f.display().to_string());
    assert_eq!(out["line"], 4);
    let fb = out["block"].as_u64().unwrap();
    d.wait_for("the file block", || d.state(fb)["real"].is_string());
    assert_eq!(info(&d, fb)["tab"], info(&d, block)["tab"]);
    d.wait_for("the checkout saved", || layout_config(&d, block)["specs"] == specs.display().to_string());
    // A hand-made agent's Spec is its page.
    let out = d.call(block, "spec", json!({ "agent": "games" }));
    assert_eq!(out["kind"], "browser");
    // A chant agent the checkout doesn't declare: its page, saying why.
    let out = d.call(block, "spec", json!({ "agent": "designer" }));
    assert_eq!(out["kind"], "browser");
    assert!(out["note"].as_str().unwrap().contains("no `name: \"designer\"`"), "{out}");
}

#[test]
fn logins() {
    let dir = Scratch::new("fountain-login");
    let fz = Fountain::start(&dir);
    // FOUNTAIN_API_KEY and FOUNTAIN_BASE_URL win over the file.
    std::fs::write(fz.home.join(".fountain/credentials"), "[default]\napi_key = \"stale\"\n").unwrap();
    let d = fz.daemon(&[("FOUNTAIN_API_KEY", KEY), ("FOUNTAIN_BASE_URL", &fz.origin)]);
    let block = open(&d, json!({}));
    let st = read(&d, block);
    assert_eq!((st["error"].clone(), st["key_from"].as_str()), (Value::Null, Some("env")), "{st}");
    // GET /api/fountain/agents: the same, without a block.
    let v = d.get("/api/fountain/agents?query=frontend-design");
    assert_eq!(v["agents"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["agents"][0]["name"], "designer");
    assert_eq!(v["total"], 108);
    drop(d);

    // The file's key; a profile that isn't there; a key Fountain refuses.
    std::fs::write(
        fz.home.join(".fountain/credentials"),
        format!(
            "[default]\napi_key = \"{KEY}\"\nbase_url = \"{}\"\n[bad]\napi_key = \"wrong\"\nbase_url = \"{}\"\n",
            fz.origin, fz.origin
        ),
    )
    .unwrap();
    let d = fz.daemon(&[]);
    let block = open(&d, json!({ "profile": "nope" }));
    let st = read(&d, block);
    assert!(st["error"].as_str().unwrap().contains("no profile \"nope\""), "{st}");
    assert_eq!(st["profiles"], json!(["bad", "default"]), "offered to pick from");
    let (status, body) = d.raw("POST", &format!("/api/blocks/{block}/call/profile"), Some(json!({ "name": "bad" })));
    assert_eq!(status, 400);
    assert!(body.contains("Fountain refused the key"), "{body}");
    let out = d.call(block, "profile", json!({ "name": "default" }));
    assert_eq!(out["total"], 108);
    d.wait_for("the profile saved", || layout_config(&d, block)["profile"] == "default");
    drop(d);

    // No login at all.
    std::fs::remove_file(fz.home.join(".fountain/credentials")).unwrap();
    let d = fz.daemon(&[]);
    let block = open(&d, json!({}));
    let st = read(&d, block);
    assert!(st["error"].as_str().unwrap().starts_with("no Fountain login here"), "{st}");
    let (_, text) = d.raw("GET", &format!("/api/panes/{block}/capture"), None);
    assert!(text.contains("no Fountain login here"), "{text}");
}

/// The agent's own MCP call, through the server illogical gave it.
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

#[test]
fn agents_through_mcp() {
    let dir = Scratch::new("fountain-mcp");
    let fz = Fountain::start(&dir);
    let d = fz.daemon(&[]);
    let agent = d.open("hello");
    assert_eq!(d.wait(agent, "idle"), "done");
    // A local agent finds designer by its skill.
    let r = agent_mcp(&d, agent, "list_agents", json!({ "query": "frontend-design" })).unwrap();
    assert_eq!(r["agents"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["agents"][0]["name"], "designer");
    assert!(r["agents"][0]["skills"].as_array().unwrap().contains(&json!("frontend-design")));
    let r = agent_mcp(&d, agent, "list_agents", json!({ "source": "agent-specs" })).unwrap();
    assert_eq!(r["agents"].as_array().unwrap().len(), 23);
    // Read within the poll: one read of the list for both.
    assert_eq!(fz.f.gets("agents"), 1);
    // The whole recipe, ${VAR}s as they are.
    let r = agent_mcp(&d, agent, "read_agent", json!({ "name": "pr-reviewer" })).unwrap();
    assert_eq!(r["name"], "pr-reviewer");
    assert_eq!(r["source"], "agent-specs");
    assert_eq!(r["mcp_servers"]["github"]["headers"]["Authorization"], "Bearer ${X}");
    assert!(r["system"].as_str().is_some());
    assert!(agent_mcp(&d, agent, "read_agent", json!({ "name": "nobody" })).unwrap_err().contains("no agent"));
    // open_fountain: the catalog beside the agent, filtered.
    let r = agent_mcp(&d, agent, "open_fountain", json!({ "source": "agent-specs" })).unwrap();
    let block = r["block"].as_u64().unwrap();
    assert_eq!(d.state(block)["agents"].as_array().unwrap().len(), 23);
    assert_eq!(info(&d, block)["tab"], info(&d, agent)["tab"]);
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

#[test]
fn an_editor_filters_but_runs_nothing() {
    let dir = Scratch::new("fountain-editor");
    let fz = Fountain::start(&dir);
    let d = fz.daemon(&[]);
    let block = open(&d, json!({}));
    read(&d, block);
    let session = info(&d, block)["session"].as_u64().unwrap();
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "editor" }));
    let call = |m: &str, args: Value| as_friend(&d, "POST", &format!("/api/blocks/{block}/call/{m}"), args);
    // The filter is shared: an editor may change it.
    let (status, body) = call("filter", json!({ "source": "agent-specs" }));
    assert_eq!(status, 200, "{body}");
    // Running an agent with the owner's login, Spec's blocks on the owner's
    // host, and which login or checkout: the owner's.
    let panes = d.get("/api/panes").as_array().unwrap().len();
    for (m, args) in [
        ("run", json!({ "agent": "games" })),
        ("run_fountain", json!({ "agent": "games" })),
        // M44: a Claude Code here, with the owner's secrets.
        ("run_here", json!({ "agent": "games" })),
        ("spec", json!({ "agent": "pr-reviewer" })),
        ("profile", json!({ "name": "other" })),
        ("specs", json!({ "dir": "/" })),
    ] {
        let (status, body) = call(m, args);
        assert_eq!(status, 403, "{m}: {body}");
    }
    // Nor wear one through a block of their own (which would go on a VM
    // of theirs, but with the owner's secrets).
    let (status, body) = as_friend(
        &d,
        "POST",
        "/api/blocks",
        json!({ "type": "agent", "split": block, "config": { "agent": "claude", "as_fountain": "games" } }),
    );
    assert_eq!(status, 403, "{body}");
    assert_eq!(d.get("/api/panes").as_array().unwrap().len(), panes, "nothing opened");
    // The owner may.
    let out = d.call(block, "run", json!({ "agent": "games" }));
    assert!(out["block"].is_u64(), "{out}");
}

#[test]
fn odd_rows_and_literal_values() {
    let dir = Scratch::new("fountain-odd");
    let fz = Fountain::start(&dir);
    fz.f.with(|i| i.extra = fixture("odd-agents.json")["data"].as_array().unwrap().clone());
    let d = fz.daemon(&[]);
    let block = open(&d, json!({}));
    let st = read(&d, block);
    // One bad row doesn't empty the catalog; the block says so.
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!((st["total"].as_u64(), st["unreadable"].as_u64()), (Some(110), Some(1)), "{}", st["unreadable_note"]);
    assert!(st["unreadable_note"].as_str().unwrap().starts_with("1 agent couldn't be read"));
    let (_, text) = d.raw("GET", &format!("/api/panes/{block}/capture"), None);
    assert!(text.contains("1 agent couldn't be read"), "{text}");
    assert!(names(&st).contains(&"fixture-nulls".to_owned()));
    // A header typed in literally is never shown: not in the block's agent
    // call, its state or its text, nor through MCP's read_agent.
    let r = d.call(block, "agent", json!({ "name": "fixture-literal-header" }));
    let h = &r["mcp_servers"]["tool"]["headers"];
    assert_eq!(
        (h["X-Api-Key"].as_str(), h["Authorization"].as_str()),
        (Some("<redacted>"), Some("Bearer ${X}")),
        "{r}"
    );
    assert!(!d.state(block).to_string().contains("typed-in-literally"));
    assert!(!text.contains("typed-in-literally"));
    let agent = d.open("hello");
    assert_eq!(d.wait(agent, "idle"), "done");
    let r = agent_mcp(&d, agent, "read_agent", json!({ "name": "fixture-literal-header" })).unwrap();
    assert_eq!(r["mcp_servers"]["tool"]["headers"]["X-Api-Key"], "<redacted>", "{r}");
    assert!(!r.to_string().contains("typed-in-literally"));
    let r = agent_mcp(&d, agent, "list_agents", json!({})).unwrap();
    assert_eq!((r["total"].as_u64(), r["unreadable"].as_u64()), (Some(110), Some(1)));
}

#[test]
fn a_new_login_never_gets_the_old_list() {
    let dir = Scratch::new("fountain-cache");
    let fz = Fountain::start(&dir);
    let d = fz.daemon(&[]);
    assert_eq!(d.get("/api/fountain/agents")["total"], 108);
    assert_eq!(fz.f.gets("agents"), 1);
    // Within the poll, the same login is answered from memory...
    d.get("/api/fountain/agents?query=games");
    assert_eq!(fz.f.gets("agents"), 1);
    // ...but another key (a re-login) reads again, and is refused here.
    std::fs::write(
        fz.home.join(".fountain/credentials"),
        format!("[default]\napi_key = \"another-key\"\nbase_url = \"{}\"\n", fz.origin),
    )
    .unwrap();
    let (status, body) = d.raw("GET", "/api/fountain/agents", None);
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("Fountain refused the key"), "{body}");
    assert_eq!(fz.f.gets("agents"), 2);
}

// ---------------------------------------------------------------- M44

/// Known fake secrets: one from Infisical, one from `gh auth token`, one
/// from the shell environment.
const INF: &str = "fake-infisical-secret-4401";
const GH: &str = "fake-gh-token-4402";
const SH: &str = "fake-shell-secret-4403";

/// M44's stand-ins, under the test's scratch dir.
struct Wear {
    specs: PathBuf,
    work: PathBuf,
    cache: PathBuf,
    /// What the fake `infisical` was asked, and where.
    infisical_log: PathBuf,
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

impl Wear {
    fn new(dir: &Path) -> Self {
        // Claude Code's adapter: the fake ACP agent.
        let bin = dir.join("agents/claude/node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        script(&bin.join("claude-agent-acp"), &format!("#!/bin/sh\nexec python3 {} \"$@\"\n", fake()));
        // Infisical has one secret, in env dev, asked for in the checkout.
        let tools = dir.join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        let infisical_log = dir.join("infisical.log");
        script(
            &tools.join("infisical"),
            &format!(
                "#!/bin/sh\necho \"$(pwd) $*\" >> {log}\n[ \"$1 $2 $3 $4 $5 $6 $7\" = \"secrets get WEAR_KEY --env dev --path /\" ] && {{ echo {INF}; exit 0; }}\necho 'Secret not found' >&2\nexit 1\n",
                log = infisical_log.display()
            ),
        );
        script(&tools.join("gh"), &format!("#!/bin/sh\n[ \"$1 $2\" = \"auth token\" ] && echo {GH}\n"));
        // GitHub: a repository of skills.
        let repo = dir.join("git/acme/skills");
        for s in ["code-review", "iterate-pr"] {
            std::fs::create_dir_all(repo.join(s)).unwrap();
            std::fs::write(repo.join(s).join("SKILL.md"), format!("---\nname: {s}\ndescription: {s}\n---\n")).unwrap();
        }
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "skills"]);
        // agent-specs: the agent's environment maps FAKE_WEAR_TOKEN.
        let specs = dir.join("agent-specs");
        std::fs::create_dir_all(specs.join("dist")).unwrap();
        std::fs::write(specs.join(".infisical.json"), "{\"workspaceId\": \"fake\"}\n").unwrap();
        std::fs::write(
            specs.join("dist/fountain.yaml"),
            "apiVersion: fountain.dev/v1\nkind: Agent\nmetadata:\n  name: fixture-wearer\nspec:\n  name: fixture-wearer\n  runtime: claude\n  environment: fixture-env\n---\napiVersion: fountain.dev/v1\nkind: Environment\nmetadata:\n  name: fixture-env\nspec:\n  name: fixture-env\n  secrets:\n    - key: FAKE_WEAR_TOKEN\n      value: \"infisical:///dev/WEAR_KEY\"\n",
        )
        .unwrap();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let cache = dir.join("cache");
        Self { specs, work, cache, infisical_log }
    }

    fn env(&self, dir: &Path) -> Vec<(&'static str, String)> {
        vec![
            ("ILLOGICAL_AGENTS_DIR", dir.join("agents").display().to_string()),
            ("ILLOGICAL_INFISICAL_BIN", dir.join("tools/infisical").display().to_string()),
            ("ILLOGICAL_GH_BIN", dir.join("tools/gh").display().to_string()),
            ("ILLOGICAL_FOUNTAIN_GIT_BASE", format!("file://{}/", dir.join("git").display())),
            ("XDG_CACHE_HOME", self.cache.display().to_string()),
            ("SHELL_ONLY", SH.to_owned()),
            // Not the runner's own.
            ("GITHUB_TOKEN", String::new()),
            ("GH_TOKEN", String::new()),
        ]
    }
}

/// An agent made to be worn: inline and GitHub skills, and a server for
/// each way a variable resolves or a server is left out.
fn wearer(origin: &str) -> Value {
    json!({
        "id": "00000000-0000-4000-8000-000000000044",
        "name": "fixture-wearer",
        "runtime": "claude",
        "model": "anthropic/claude-haiku-4-5",
        "system": "You review fixtures.",
        "updated_at": "2026-10-03T00:00:00Z",
        "environment_id": "not-a-known-environment",
        "skills": [
            { "name": "inline-one", "content": "---\nname: inline-one\ndescription: one\n---\nDo one thing.\n" },
            { "source": "acme/skills", "name": "code-review" },
            { "source": "acme/skills" }
        ],
        "mcp_servers": {
            "from-infisical": { "type": "http", "url": format!("{origin}/open-mcp"), "headers": { "Authorization": "Bearer ${FAKE_WEAR_TOKEN}" } },
            "from-gh": { "type": "http", "url": format!("{origin}/open-mcp"), "headers": { "Authorization": "Bearer ${GITHUB_TOKEN}" } },
            "from-shell": { "command": "python3", "args": ["-c", "pass"], "env": { "TOKEN": "${SHELL_ONLY}" } },
            "on-argv": { "command": "python3", "args": ["-c", "pass", "${SHELL_ONLY}"] },
            "escaped": { "command": "python3", "env": { "KEPT": "$${LITERAL}" } },
            "unset": { "type": "http", "url": format!("{origin}/open-mcp"), "headers": { "X-Key": "${NOT_SET_ANYWHERE}" } },
            "signs-in": { "type": "http", "url": format!("{origin}/oauth-mcp") },
            "open": { "type": "http", "url": format!("{origin}/open-mcp") },
            "connected": { "connection": "c-1" }
        },
        "metadata": { "managed-by": "chant" }
    })
}

/// Every file under `dir` that holds `needle`.
fn files_with(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                stack.push(p);
            } else if t.is_file()
                && let Ok(bytes) = std::fs::read(&p)
                && bytes.windows(needle.len()).any(|w| w == needle.as_bytes())
            {
                out.push(p);
            }
        }
    }
    out
}

/// A process's environment.
fn proc_env(pid: u64) -> HashMap<String, String> {
    let raw = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    raw.split(|b| *b == 0)
        .filter_map(|kv| String::from_utf8_lossy(kv).split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
        .collect()
}

/// What a test expects in the adapter's environment, where /proc can't say.
fn secrets_env() -> HashMap<String, String> {
    [
        ("ILLOGICAL_FTN_FROM_INFISICAL_H_AUTHORIZATION", format!("Bearer {INF}")),
        ("ILLOGICAL_FTN_FROM_GH_H_AUTHORIZATION", format!("Bearer {GH}")),
        ("ILLOGICAL_FTN_FROM_SHELL_E_TOKEN", SH.to_owned()),
        ("ILLOGICAL_MCP_BLOCK_TOKEN", "ilb_".to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}

/// Its command line, arguments joined by spaces.
fn proc_cmdline(pid: u64) -> String {
    String::from_utf8_lossy(&std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default()).replace('\0', " ")
}

/// What the fake adapter's session was opened with (its saved session).
fn session(d: &Daemon, block: u64) -> Value {
    let sid = d.state(block)["session_id"].as_str().unwrap().to_owned();
    serde_json::from_str(&std::fs::read_to_string(d.sessions.join(format!("{sid}.json"))).unwrap()).unwrap()
}

fn last_reply(st: &Value) -> String {
    entries(st).iter().rev().find(|e| e["type"] == "agent").and_then(|e| e["text"].as_str()).unwrap_or("").to_owned()
}

#[test]
fn run_here_wears_the_agent() {
    let dir = Scratch::new("fountain-wear");
    let fz = Fountain::start(&dir);
    fz.f.with(|i| i.extra = vec![wearer(&fz.origin)]);
    let w = Wear::new(&dir);
    let env = w.env(&dir);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let d = fz.daemon(&env);
    let catalog = open(&d, json!({ "specs": w.specs }));
    read(&d, catalog);

    // Run here, in a folder: a Claude Code block beside the catalog.
    let out = d.call(catalog, "run_here", json!({ "agent": "fixture-wearer", "cwd": w.work, "prompt": "hello" }));
    let id = out["block"].as_u64().unwrap();
    assert_eq!(out["cwd"], w.work.display().to_string());
    assert_eq!(d.wait(id, "idle"), "done", "{}", d.state(id));
    assert_eq!(info(&d, id)["tab"], info(&d, catalog)["tab"], "beside the catalog");
    let st = d.state(id);
    assert_eq!(st["label"], "Claude Code as fixture-wearer");
    assert_eq!(st["cwd"], w.work.display().to_string());
    let worn = &st["worn"];
    assert_eq!(worn["agent"], "fixture-wearer");
    assert_eq!(worn["model"], "claude-haiku-4-5");
    assert_eq!(worn["skills"], json!(["inline-one", "code-review", "iterate-pr"]), "{worn}");
    let servers: Vec<&str> = worn["servers"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(servers, ["from-gh", "from-infisical", "from-shell", "open"], "{worn}");
    let vars = |n: &str| worn["servers"].as_array().unwrap().iter().find(|s| s["name"] == n).unwrap()["vars"].clone();
    assert_eq!(vars("from-infisical"), json!(["FAKE_WEAR_TOKEN from Infisical dev/WEAR_KEY"]));
    assert_eq!(vars("from-gh"), json!(["GITHUB_TOKEN from gh auth token"]));
    assert_eq!(vars("from-shell"), json!(["SHELL_ONLY from shell environment"]));
    let left: Vec<(String, String)> = worn["left_out"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["name"].as_str().unwrap().to_owned(), l["why"].as_str().unwrap().to_owned()))
        .collect();
    assert_eq!(
        left.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(),
        ["connected", "escaped", "on-argv", "signs-in", "unset"],
        "{left:?}"
    );
    assert!(left[0].1.contains("Fountain connection"), "{left:?}");
    assert!(left[1].1.contains("literal ${"), "{left:?}");
    assert!(left[2].1.contains("${SHELL_ONLY} in its command line"), "{left:?}");
    assert!(left[3].1.starts_with("it needs an OAuth sign-in"), "{left:?}");
    assert!(left[4].1.contains("${NOT_SET_ANYWHERE} isn't set"), "{left:?}");
    // Infisical was asked in the checkout, mapped through the agent's
    // environment, and for the unmapped ones as themselves in dev.
    let asked = std::fs::read_to_string(&w.infisical_log).unwrap();
    assert!(asked.contains(&format!("{} secrets get WEAR_KEY --env dev --path / --plain --silent", w.specs.display())));
    assert!(asked.contains("secrets get GITHUB_TOKEN --env dev"), "{asked}");
    // A fallback to the variable's own name in dev would say so.
    assert!(!vars("from-infisical")[0].as_str().unwrap().contains("not mapped"));
    assert!(!asked.contains("FAKE_WEAR_TOKEN"), "mapped, not asked as itself: {asked}");

    // What the adapter's session/new got.
    let s = session(&d, id);
    let meta = &s["meta"];
    let opts = &meta["claudeCode"]["options"];
    assert_eq!(opts["settingSources"], json!([]), "the user's hooks stay out: {meta}");
    assert_eq!(opts["model"], "claude-haiku-4-5");
    let plugin = opts["plugins"][0]["path"].as_str().unwrap().to_owned();
    assert_eq!(opts["plugins"][0]["type"], "local");
    assert!(plugin.starts_with(&w.cache.join("illogical/fountain").display().to_string()), "{plugin}");
    assert!(Path::new(&plugin).join("skills/code-review/SKILL.md").is_file());
    assert!(Path::new(&plugin).join("skills/inline-one/SKILL.md").is_file());
    let system = meta["systemPrompt"]["append"].as_str().unwrap();
    assert!(system.starts_with("You are running as the Fountain agent \"fixture-wearer\", but locally"), "{system}");
    assert!(system.ends_with("You review fixtures."));
    // session/new carries references only (the SDK puts it on `claude`'s
    // command line); the values are in the adapter's environment.
    let mcp = s["mcp"].as_array().unwrap();
    let by = |n: &str| mcp.iter().find(|m| m["name"] == n).cloned().unwrap_or_default();
    assert_eq!(
        by("from-infisical")["headers"],
        json!([{ "name": "Authorization", "value": "${ILLOGICAL_FTN_FROM_INFISICAL_H_AUTHORIZATION}" }])
    );
    assert_eq!(by("from-gh")["headers"][0]["value"], "${ILLOGICAL_FTN_FROM_GH_H_AUTHORIZATION}");
    assert_eq!(
        by("from-shell"),
        json!({ "name": "from-shell", "command": "python3", "args": ["-c", "pass"], "env": [{ "name": "TOKEN", "value": "${ILLOGICAL_FTN_FROM_SHELL_E_TOKEN}" }] })
    );
    assert_eq!(by("open")["type"], "http");
    // #128: illogical's own token too.
    assert_eq!(by("illogical")["headers"][0]["value"], "Bearer ${ILLOGICAL_MCP_BLOCK_TOKEN}");
    let pid = d.state(id)["pid"].as_u64().unwrap();
    // (Linux: /proc. The rest holds everywhere.)
    let environ = if Path::new("/proc/self/environ").exists() { proc_env(pid) } else { secrets_env() };
    assert_eq!(
        environ.get("ILLOGICAL_FTN_FROM_INFISICAL_H_AUTHORIZATION").map(String::as_str),
        Some(format!("Bearer {INF}").as_str())
    );
    assert_eq!(
        environ.get("ILLOGICAL_FTN_FROM_GH_H_AUTHORIZATION").map(String::as_str),
        Some(format!("Bearer {GH}").as_str())
    );
    assert_eq!(environ.get("ILLOGICAL_FTN_FROM_SHELL_E_TOKEN").map(String::as_str), Some(SH));
    assert!(environ.get("ILLOGICAL_MCP_BLOCK_TOKEN").is_some_and(|t| t.starts_with("ilb_")));
    let token = environ["ILLOGICAL_MCP_BLOCK_TOKEN"].clone();
    for secret in [INF, GH, SH, token.as_str()] {
        assert!(!proc_cmdline(pid).contains(secret), "{secret} on the adapter's command line");
        assert!(!s.to_string().contains(secret), "{secret} in session/new");
    }
    assert!(by("unset").is_null() && by("signs-in").is_null() && by("connected").is_null());
    assert!(fz.f.with(|i| i.probes.contains(&"oauth".to_owned())), "probed");

    // A known secret is on no surface: the agent says its servers back,
    // as one that leaks what it got would.
    d.call(id, "send", json!({ "text": "servers" }));
    assert_eq!(d.wait(id, "idle"), "done");
    let said = last_reply(&d.state(id));
    assert!(said.starts_with("SERVERS ") && said.contains("<redacted>"), "{said}");
    // An agent in the same tab reads it through MCP.
    let config = json!({ "agent": "acp", "command": ["python3", fake()], "cwd": d.sessions, "prompt": "hello" });
    let plain = d.open_with(json!({ "type": "agent", "split": id, "config": config }));
    assert_eq!(d.wait(plain, "idle"), "done");
    let read_output = agent_mcp(&d, plain, "read_output", json!({ "pane": id })).unwrap();
    let (_, capture) = d.raw("GET", &format!("/api/panes/{id}/capture"), None);
    assert!(
        capture.contains("As the Fountain agent fixture-wearer, locally. Skills: inline-one, code-review, iterate-pr."),
        "{capture}"
    );
    assert!(capture.contains("Didn't carry over: connected"), "{capture}");
    let surfaces = [
        ("state", d.state(id).to_string()),
        ("block", d.raw("GET", &format!("/api/blocks/{id}"), None).1),
        ("panes", d.raw("GET", "/api/panes", None).1),
        ("capture", capture),
        ("tail", d.raw("GET", &format!("/api/panes/{id}/tail"), None).1),
        ("history", d.raw("GET", &format!("/api/history?pane={id}"), None).1),
        ("search", d.raw("GET", "/api/search?re=fake-", None).1),
        ("read_output", read_output.to_string()),
        ("catalog", d.state(catalog).to_string()),
    ];
    for secret in [INF, GH, SH] {
        for (what, text) in &surfaces {
            assert!(!text.contains(secret), "{secret} in {what}: {text}");
        }
        // layout.json, the block's log, everything the daemon keeps; the
        // bundle cache too.
        assert_eq!(files_with(&d.state, secret), Vec::<PathBuf>::new(), "{secret} in the state dir");
        assert_eq!(files_with(&w.cache, secret), Vec::<PathBuf>::new(), "{secret} in the cache");
    }
    // Nor in what the adapter was sent: session/new's servers, as it
    // recorded them (its environment has the values).
    let sid = d.state(id)["session_id"].as_str().unwrap().to_owned();
    let sent = std::fs::read_to_string(d.sessions.join(format!("mcp-{sid}.json"))).unwrap();
    for secret in [INF, GH, SH] {
        assert!(!sent.contains(secret), "{secret} in session/new's mcpServers");
    }
    // The block's log has the session/new frame, its values redacted.
    let logs = files_with(&d.state, "\"session/new\"");
    assert!(!logs.is_empty(), "session/new is in a log");
    for l in &logs {
        let text = String::from_utf8_lossy(&std::fs::read(l).unwrap()).into_owned();
        assert!(text.contains("from-infisical") && text.contains("<redacted>"), "{}", l.display());
    }
    // Only the name in the saved config.
    d.wait_for("the config saved", || layout_config(&d, id)["as_fountain"] == "fixture-wearer");
    let saved = layout_config(&d, id);
    assert_eq!(saved["agent"], "claude");
    assert_eq!(saved["specs"], w.specs.display().to_string());
    assert!(saved.get("mcp_servers").is_none(), "{saved}");
    // The catalog remembers where it ran one.
    assert_eq!(d.state(catalog)["here"], w.work.display().to_string());
}

#[test]
fn a_worn_agent_is_put_on_again_after_a_reboot() {
    let dir = Scratch::new("fountain-wear-reboot");
    let fz = Fountain::start(&dir);
    fz.f.with(|i| i.extra = vec![wearer(&fz.origin)]);
    let w = Wear::new(&dir);
    let env = w.env(&dir);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let mut d = fz.daemon(&env);
    // As `illogical agent --as` opens it.
    let config = json!({ "agent": "claude", "as_fountain": "fixture-wearer", "specs": w.specs, "cwd": w.work, "prompt": "remember kestrel" });
    let id = d.open_with(json!({ "type": "agent", "config": config }));
    assert_eq!(d.wait(id, "idle"), "done", "{}", d.state(id));
    // #127: an ordinary Claude Code block, beside it.
    let plain_config = json!({ "agent": "claude", "cwd": w.work, "prompt": "hello" });
    let plain = d.open_with(json!({ "type": "agent", "split": id, "config": plain_config }));
    assert_eq!(d.wait(plain, "idle"), "done", "{}", d.state(plain));
    // #128: its illogical MCP server works through the reference.
    agent_mcp(&d, plain, "list", json!({})).expect("illogical's MCP server, through ${ILLOGICAL_MCP_BLOCK_TOKEN}");
    // Forget what each session was opened with, to see what reopening sends.
    let forget = |b: u64| {
        let sid = d.state(b)["session_id"].as_str().unwrap().to_owned();
        let path = d.sessions.join(format!("{sid}.json"));
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["meta"] = Value::Null;
        std::fs::write(&path, v.to_string()).unwrap();
    };
    forget(id);
    forget(plain);
    d.stop();
    d.start();
    for b in [id, plain] {
        d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{b}"), None).0 == 200);
        d.wait_for("the session", || d.state(b)["status"] == "ready");
    }
    let st = d.state(id);
    assert_eq!(st["worn"]["agent"], "fixture-wearer", "put on again: {st}");
    d.call(id, "send", json!({ "text": "recall" }));
    assert_eq!(d.wait(id, "idle"), "done");
    assert_eq!(last_reply(&d.state(id)), "You said kestrel.");
    // The session reopened as the agent, with its servers.
    let s = session(&d, id);
    assert_eq!(s["meta"]["claudeCode"]["options"]["settingSources"], json!([]));
    assert!(s["meta"]["systemPrompt"]["append"].as_str().unwrap().ends_with("You review fixtures."));
    assert!(s["mcp"].as_array().unwrap().iter().any(|m| m["name"] == "from-infisical"));
    // #127: and the ordinary one kept settingSources: [] on resume.
    assert_eq!(session(&d, plain)["meta"], json!({ "claudeCode": { "options": { "settingSources": [] } } }));

    // A session that can't be reopened: the new one is still the agent's.
    let pid = d.state(id)["pid"].as_u64().unwrap();
    let old = d.state(id)["session_id"].as_str().unwrap().to_owned();
    d.stop();
    std::fs::remove_file(d.sessions.join(format!("{old}.json"))).unwrap();
    d.wait_for("the agent to go", || !alive(pid));
    d.start();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    d.wait_for("a new session", || d.state(id)["session_id"].as_str().is_some_and(|s| s != old));
    let s = session(&d, id);
    assert!(s["meta"]["systemPrompt"]["append"].as_str().unwrap().ends_with("You review fixtures."), "{s}");
    assert!(s["mcp"].as_array().unwrap().iter().any(|m| m["name"] == "from-infisical"), "{s}");
    assert_eq!(files_with(&d.state, INF), Vec::<PathBuf>::new());
}

/// Under systemd: the daemon restarts and takes over the adapter still
/// running from before. It puts the agent on again before it reads a word
/// from it: the secrets it scrubs are back, and a new session is worn.
#[test]
fn a_worn_agent_taken_over_after_a_restart() {
    let dir = Scratch::new("fountain-wear-takeover");
    let fz = Fountain::start(&dir);
    fz.f.with(|i| i.extra = vec![wearer(&fz.origin)]);
    let w = Wear::new(&dir);
    let env = w.env(&dir);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let Some(d) = fz.service(&env) else { return };
    let config = json!({ "agent": "claude", "as_fountain": "fixture-wearer", "specs": w.specs, "cwd": w.work, "prompt": "hello" });
    let id = d.open_with(json!({ "type": "agent", "config": config }));
    assert_eq!(d.wait(id, "idle"), "done", "{}", d.state(id));
    let pid = d.state(id)["pid"].as_u64().unwrap();

    d.restart_service();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200);
    d.wait_for("worn again", || d.state(id)["worn"]["agent"] == "fixture-wearer");
    d.wait_for("taken over", || d.state(id)["status"] == "ready");
    assert_eq!(d.state(id)["pid"].as_u64(), Some(pid), "the same adapter");
    // What it says is scrubbed as before.
    d.call(id, "send", json!({ "text": "servers" }));
    assert_eq!(d.wait(id, "idle"), "done");
    let said = last_reply(&d.state(id));
    assert!(said.contains("<redacted>") && !said.contains(INF) && !said.contains(GH), "{said}");
    for secret in [INF, GH, SH] {
        assert_eq!(files_with(&d.state, secret), Vec::<PathBuf>::new(), "{secret} in the state dir");
    }
    d.post(&format!("/api/panes/{id}/close"), json!({}));
    d.wait_for("the adapter to go", || !alive(pid));

    // #128, upgrading: an ordinary Claude Code block whose adapter an older
    // daemon started (no token in its environment, so no marker) is
    // started again when taken over, and its illogical MCP still works.
    let config = json!({ "agent": "claude", "cwd": w.work, "prompt": "hello" });
    let plain = d.open_with(json!({ "type": "agent", "config": config }));
    assert_eq!(d.wait(plain, "idle"), "done", "{}", d.state(plain));
    let old = d.state(plain)["pid"].as_u64().unwrap();
    let marker = d.state.join(format!("blocks/{plain}/mcp-token-env"));
    assert!(marker.is_file(), "written when it started with the token in its environment");
    d.restart_service();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{plain}"), None).0 == 200);
    d.wait_for("ready", || d.state(plain)["status"] == "ready");
    assert_eq!(d.state(plain)["pid"].as_u64(), Some(old), "with its marker: taken over");
    std::fs::remove_file(&marker).unwrap();
    d.restart_service();
    d.wait_for("the block", || d.raw("GET", &format!("/api/blocks/{plain}"), None).0 == 200);
    d.wait_for("started again", || d.state(plain)["pid"].as_u64().is_some_and(|p| p != old));
    d.wait_for("ready", || d.state(plain)["status"] == "ready");
    d.wait_for("the old one gone", || !alive(old));
    assert!(marker.is_file());
    agent_mcp(&d, plain, "list", json!({})).expect("illogical's MCP server, through the reference");
}

#[test]
fn what_cant_be_worn_says_why() {
    let dir = Scratch::new("fountain-wear-refused");
    let fz = Fountain::start(&dir);
    // The orchestrators are tagged in agent-specs.
    fz.f.with(|i| {
        for a in i.agents["data"].as_array_mut().unwrap() {
            if a["name"] == "captain-picard" {
                a["metadata"]["illogical.local"] = json!(false);
            }
            if a["name"] == "tech-lead" {
                a["metadata"]["illogical.local"] = json!("false");
            }
        }
    });
    let w = Wear::new(&dir);
    let env = w.env(&dir);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let d = fz.daemon(&env);
    let catalog = open(&d, json!({}));
    let st = read(&d, catalog);
    let card = |n: &str| st["agents"].as_array().unwrap().iter().find(|c| c["name"] == n).cloned().unwrap();
    assert_eq!(card("captain-picard")["local"], false);
    assert_eq!(card("tech-lead")["local"], false, "the string too");
    let panes = d.get("/api/panes").as_array().unwrap().len();
    for (agent, says) in [("captain-picard", "for Fountain only"), ("tech-lead", "for Fountain only")] {
        let (status, body) =
            d.raw("POST", &format!("/api/blocks/{catalog}/call/run_here"), Some(json!({ "agent": agent })));
        assert_eq!(status, 400, "{body}");
        assert!(body.contains(says) && body.contains("Run on Fountain"), "{body}");
    }
    let codex = st["agents"].as_array().unwrap().iter().find(|c| c["runtime"] == "codex").cloned().unwrap();
    let (_, body) =
        d.raw("POST", &format!("/api/blocks/{catalog}/call/run_here"), Some(json!({ "agent": codex["name"] })));
    assert!(body.contains("is a codex agent"), "{body}");
    let (status, body) = d.raw(
        "POST",
        &format!("/api/blocks/{catalog}/call/run_here"),
        Some(json!({ "agent": "games", "cwd": "/nonexistent/x" })),
    );
    assert_eq!(status, 400);
    assert!(body.contains("isn't a directory"), "{body}");
    assert_eq!(d.get("/api/panes").as_array().unwrap().len(), panes, "nothing opened");

    // Opened directly (`illogical agent --as`): it says why, and doesn't start.
    let id = d.open_with(json!({ "type": "agent", "config": { "agent": "claude", "as_fountain": "captain-picard" } }));
    d.wait_for("the refusal", || d.state(id)["status"] == "exited");
    let st = d.state(id);
    assert_eq!(st["attention"], "needs_input");
    assert_eq!(st["status"], "exited");
    assert!(
        st["error"].as_str().unwrap().contains("can't wear captain-picard: captain-picard is for Fountain only"),
        "{st}"
    );
    assert!(st["pid"].is_null());
    // Not on a VM, and not for another agent.
    let (status, _) = d.raw(
        "POST",
        "/api/blocks",
        Some(json!({ "type": "agent", "config": { "agent": "codex", "as_fountain": "games" } })),
    );
    assert_eq!(status, 400);

    // start_agent: refused up front, with the reason; opened for one that can be.
    let plain = d.open("hello");
    assert_eq!(d.wait(plain, "idle"), "done");
    let err =
        agent_mcp(&d, plain, "start_agent", json!({ "prompt": "hi", "as_fountain": "captain-picard" })).unwrap_err();
    assert!(err.contains("for Fountain only"), "{err}");
    let err = agent_mcp(&d, plain, "start_agent", json!({ "agent": "codex", "prompt": "hi", "as_fountain": "games" }))
        .unwrap_err();
    assert!(err.contains("as_fountain is for agent claude"), "{err}");
    let r = agent_mcp(&d, plain, "start_agent", json!({ "prompt": "hello", "as_fountain": "games", "cwd": w.work }))
        .unwrap();
    let games = r["block"].as_u64().unwrap();
    assert_eq!(d.wait(games, "idle"), "done", "{}", d.state(games));
    assert_eq!(d.state(games)["worn"]["skills"], json!(["love2d", "pixijs", "screenshots-in-prs"]));
    d.wait_for("its config saved", || layout_config(&d, games)["as_fountain"] == "games");
}
