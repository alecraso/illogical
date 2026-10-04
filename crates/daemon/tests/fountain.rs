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
//! M45b's runner view, against the same fake with recorded (scrubbed)
//! `/api/runners` and `/api/sandboxes`, a unit file, a stand-in
//! `systemctl`, and a stand-in `sudo` that logs its argv and runs bash as
//! the test's own user (no test calls sudo): status and the host's line;
//! sandbox → directory; *Shell*'s exact command with a hostile path;
//! *Changes*' git through the sudo form; *Follow*'s `session/load`;
//! attention for an offline runner and for another one online; and every
//! one of them refused to an editor.

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
    /// M45b: `/api/runners` and `/api/sandboxes`, and the queries asked.
    runners: Value,
    sandboxes: Value,
    queries: Vec<String>,
    /// `/api/runners` answers 500.
    fail_runners: bool,
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
    let a = f.with(|i| i.agents["data"].as_array().unwrap().iter().find(|a| a["id"] == id.as_str()).cloned());
    match a {
        Some(a) => Json(json!({ "data": a })).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
    }
}

async fn runners(State(f): State<Fake>, h: HeaderMap) -> Response {
    if let Some(r) = guard(&f, &h, "runners") {
        return r;
    }
    if f.with(|i| i.fail_runners) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "down" }))).into_response();
    }
    Json(f.with(|i| i.runners.clone())).into_response()
}

async fn sandboxes(State(f): State<Fake>, h: HeaderMap, q: axum::extract::RawQuery) -> Response {
    if let Some(r) = guard(&f, &h, "sandboxes") {
        return r;
    }
    Json(f.with(|i| {
        i.queries.push(q.0.unwrap_or_default());
        i.sandboxes.clone()
    }))
    .into_response()
}

async fn environments(State(f): State<Fake>, h: HeaderMap) -> Response {
    if let Some(r) = guard(&f, &h, "environments") {
        return r;
    }
    Json(f.with(|i| i.envs.clone())).into_response()
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
                .route("/api/runners", get(runners))
                .route("/api/sandboxes", get(sandboxes))
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
        std::fs::write(
            &fountain,
            format!(
                "#!/bin/sh\n[ \"$1\" = --version ] && {{ echo 'fountain version v0.21.0'; exit 0; }}\nexec python3 {} \"$@\"\n",
                fake()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fountain, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { _rt: rt, f, origin, home, bin }
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

    // Run here: M44's. Until then it says so, and why not for some.
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{block}/call/run_here"), Some(json!({ "agent": "games" })));
    assert_eq!(status, 400);
    assert!(body.contains("M44"), "{body}");
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
        ("spec", json!({ "agent": "pr-reviewer" })),
        ("profile", json!({ "name": "other" })),
        ("specs", json!({ "dir": "/" })),
    ] {
        let (status, body) = call(m, args);
        assert_eq!(status, 403, "{m}: {body}");
    }
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

// ---------------------------------------------------------------- M45b: the runner view

/// This host's runner in the recorded fixture.
const RUNNER: &str = "edbc518d-70b9-4f45-8db2-73f57b1de3f0";
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// A runner host in a scratch directory: its unit file, a `systemctl` that
/// says what `unit-state` holds, a `sudo` that logs its argv (one JSON list
/// a line) and runs bash as this user, and the recorded runner and
/// sandboxes with their directories under `root`.
struct RunnerHost {
    root: PathBuf,
    sudo: PathBuf,
    log: PathBuf,
    state: PathBuf,
    env: Vec<(String, String)>,
}

impl RunnerHost {
    fn new(dir: &Path, fz: &Fountain) -> Self {
        let root = dir.join("sandboxes");
        std::fs::create_dir_all(&root).unwrap();
        let unit = dir.join("fountain-runner.service");
        std::fs::write(
            &unit,
            format!(
                "[Service]\nUser=fountain\nExecStart=/usr/local/bin/fountain runner --name runner-1 --root {}\n",
                root.display()
            ),
        )
        .unwrap();
        let state = dir.join("unit-state");
        std::fs::write(&state, "active\n").unwrap();
        let script = |name: &str, body: String| {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let systemctl = script(
            "systemctl",
            format!("#!/bin/sh\n[ \"$1 $2\" = 'is-active fountain-runner' ] || exit 4\ncat '{}'\n", state.display()),
        );
        let log = dir.join("sudo.log");
        let sudo = script(
            "sudo",
            format!(
                "#!/bin/sh\npython3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' \"$@\" >> '{}'\n[ \"$1 $2 $3 $4\" = '-n -u fountain /bin/bash' ] || {{ echo 'sudo: a password is required' >&2; exit 1; }}\nshift 4\nexec /bin/bash \"$@\"\n",
                log.display()
            ),
        );
        let mut runners = fixture("runner-view-runners.json");
        runners["data"][0]["root"] = json!(root.display().to_string());
        let text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/fountain/runner-view-sandboxes.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
        .replace("/srv/fountain/sandboxes", &root.display().to_string());
        let sandboxes: Value = serde_json::from_str(&text).unwrap();
        for s in sandboxes["data"].as_array().unwrap() {
            if let Some(p) = s["runner"]["path"].as_str() {
                std::fs::create_dir_all(p).unwrap();
            }
        }
        fz.f.with(|i| {
            i.runners = runners;
            i.sandboxes = sandboxes;
        });
        let env = vec![
            ("ILLOGICAL_FOUNTAIN_UNIT_FILE".into(), unit.display().to_string()),
            ("ILLOGICAL_FOUNTAIN_SYSTEMCTL".into(), systemctl.display().to_string()),
            ("ILLOGICAL_FOUNTAIN_SUDO".into(), sudo.display().to_string()),
            ("ILLOGICAL_FOUNTAIN_POLL_MS".into(), "300".into()),
            ("ILLOGICAL_FOUNTAIN_RUNNER_GRACE_MS".into(), "1500".into()),
        ];
        Self { root, sudo, log, state, env }
    }

    fn daemon(&self, fz: &Fountain) -> Daemon {
        let env: Vec<(&str, &str)> = self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        fz.daemon(&env)
    }

    /// Every sudo call so far, as its argv.
    fn sudo_calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

/// `s` as one shell word, as the daemon quotes it.
fn quoted(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./-:=@%+,".contains(&b)) {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn runner_view(d: &Daemon) -> (u64, Value) {
    let block = open(d, json!({ "view": "runner" }));
    d.wait_for("the runner read", || d.state(block)["runner"].is_object());
    (block, d.state(block))
}

fn capture(d: &Daemon, pane: u64) -> String {
    d.raw("GET", &format!("/api/panes/{pane}/capture"), None).1
}

#[test]
fn the_runner_view_its_sandboxes_and_what_opens_from_them() {
    let dir = Scratch::new("fountain-runner");
    let fz = Fountain::start(&dir);
    let host = RunnerHost::new(&dir, &fz);
    // A hostile directory name: quotes, a command substitution, a space.
    let hostile_name = format!("runner-{}-it's $(touch pwned) x", RUNNER.replace('-', ""));
    let hostile = host.root.join(&hostile_name);
    std::fs::create_dir_all(&hostile).unwrap();
    fz.f.with(|i| {
        i.sandboxes["data"].as_array_mut().unwrap().push(json!({
            "id": "s-hostile", "sprite_name": hostile_name, "status": "suspended", "provider": "runner",
            "mode": "ephemeral", "agent_id": null, "inserted_at": "2026-10-04T00:00:00Z",
            "runner": { "id": RUNNER, "name": "runner-1", "path": hostile.display().to_string(), "online": true },
            "conversations": []
        }))
    });
    let d = host.daemon(&fz);
    let (block, st) = runner_view(&d);
    let r = &st["runner"];
    assert_eq!(st["error"], Value::Null, "{st}");
    assert_eq!((r["this"]["name"].as_str(), r["this"]["online"].as_bool()), (Some("runner-1"), Some(true)), "{r}");
    assert_eq!((r["unit"]["name"].as_str(), r["unit_active"].as_bool()), (Some("runner-1"), Some(true)));
    assert_eq!((r["this"]["version"].as_str(), r["local_version"].as_str()), (Some("v0.21.0"), Some("v0.21.0")));
    assert_eq!(r["others"], json!([]));
    assert_eq!(r["attention"], Value::Null);
    // Sandbox → directory: this runner's only (two recorded, the hostile one).
    let sbs = r["sandboxes"].as_array().unwrap();
    assert_eq!(sbs.len(), 3, "{r}");
    for s in sbs {
        assert_eq!(s["path"], host.root.join(s["name"].as_str().unwrap()).display().to_string());
    }
    let a = sbs.iter().find(|s| s["name"].as_str().unwrap().ends_with("-2972e1a2")).unwrap().clone();
    let b = sbs.iter().find(|s| s["name"].as_str().unwrap().ends_with("-9ef02d05")).unwrap().clone();
    assert_eq!(a["agent"], "hud-playground", "its agent's name, from /api/agents/ID");
    assert!(sbs.iter().any(|s| s["parked"] == true));
    assert!(r["note"].as_str().unwrap().contains("parking the sandbox doesn't stop it"));
    // Only sandboxes that may have a directory are asked for.
    assert!(fz.f.with(|i| i.queries.iter().all(|q| q == "status=pending,starting,ready,suspended")));
    // capture --text.
    let text = capture(&d, block);
    assert!(
        text.contains("this host: runner-1 (unit fountain-runner active), online, v0.21.0 (as installed)"),
        "{text}"
    );
    assert!(text.contains("3 sandboxes") && text.contains("other runners: none"), "{text}");
    assert!(text.contains(&format!(
        "  {} ready hud-playground {}",
        a["name"].as_str().unwrap(),
        a["path"].as_str().unwrap()
    )));
    // The machine's line.
    d.wait_for("the host's line", || d.get("/api/host")["fountain_runner"]["sandboxes"] == 3);
    let h = d.get("/api/host")["fountain_runner"].clone();
    assert_eq!(
        (h["name"].as_str(), h["online"].as_bool(), h["version"].as_str()),
        (Some("runner-1"), Some(true), Some("v0.21.0"))
    );
    // Nothing went through sudo just to look.
    assert!(host.sudo_calls().is_empty(), "{:?}", host.sudo_calls());

    // Shell: the exact command, the hostile path one quoted word.
    let out = d.call(block, "shell", json!({ "sandbox": "s-hostile" }));
    let cmd = out["command"].as_str().unwrap();
    let head = format!(
        "exec {} -n -u fountain /bin/bash -c 'real=$(cd -P -- \"$2\"",
        quoted(&host.sudo.display().to_string())
    );
    let tail = format!(
        "HOME=/home/fountain INPUTRC=/dev/null HISTFILE=/dev/null exec bash --noprofile --norc' _ {} {}",
        quoted(&host.root.display().to_string()),
        quoted(&hostile.display().to_string())
    );
    assert!(cmd.starts_with(&head) && cmd.ends_with(&tail), "{cmd}");
    assert_eq!(out["parked"], true);
    let pane = out["pane"].as_u64().unwrap();
    assert_eq!(info(&d, pane)["tab"], info(&d, block)["tab"], "beside the view");
    d.wait_for("the shell's sudo", || !host.sudo_calls().is_empty());
    let call = host.sudo_calls()[0].clone();
    assert_eq!(&call[..5], ["-n", "-u", "fountain", "/bin/bash", "-c"]);
    assert!(
        call[5].ends_with(
            "cd -- \"$real\" && HOME=/home/fountain INPUTRC=/dev/null HISTFILE=/dev/null exec bash --noprofile --norc"
        ),
        "{call:?}"
    );
    assert_eq!(&call[6..], ["_", host.root.to_str().unwrap(), hostile.to_str().unwrap()]);
    let at = format!("at={} home=/home/fountain", hostile.display());
    d.wait_for("the shell's prompt", || !capture(&d, pane).trim().is_empty());
    std::thread::sleep(std::time::Duration::from_millis(300));
    d.post(&format!("/api/panes/{pane}/send"), json!({ "text": "echo \"at=$(pwd) home=$HOME\"", "enter": true }));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !capture(&d, pane).replace('\n', "").contains(&at) {
        assert!(std::time::Instant::now() < deadline, "the shell in the sandbox: {}", capture(&d, pane));
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(!hostile.join("pwned").exists() && !dir.join("pwned").exists(), "the path ran as a command");

    // Changes: a checkout of the agent's own (all of it is its edits) and a
    // clone (its edits since upstream), git run through the sudo form.
    let a_dir = PathBuf::from(a["path"].as_str().unwrap());
    let repo = a_dir.join("r1-check");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("hello.txt"), "hello\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "first"]);
    std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();
    let out = d.call(block, "changes", json!({ "sandbox": a["id"] }));
    let co = out["checkouts"].as_array().unwrap();
    assert_eq!(co.len(), 1, "{out}");
    assert_eq!((co[0]["repo"].as_str(), co[0]["rev_a"].as_str()), (Some(repo.to_str().unwrap()), Some(EMPTY_TREE)));
    let diff = co[0]["block"].as_u64().unwrap();
    d.wait_for("the diff", || d.state(diff)["files"].as_array().is_some_and(|f| f.len() == 2));
    let ds = d.state(diff);
    let files: Vec<(String, String)> = ds["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap().to_owned(), f["status"].as_str().unwrap().to_owned()))
        .collect();
    assert_eq!(files, [("hello.txt".into(), "added".into()), ("new.txt".into(), "untracked".into())], "{ds}");
    assert_eq!(ds["run_as"], "fountain");
    assert_eq!(info(&d, diff)["tab"], info(&d, block)["tab"]);
    d.wait_for("the diff's config saved", || layout_config(&d, diff)["run_as"] == "fountain");
    let calls = host.sudo_calls();
    let find = calls.iter().find(|c| c[5].contains("find . -maxdepth 3 -name .git")).expect("the checkouts' search");
    assert_eq!(
        (&find[..5], &find[6..]),
        (
            &["-n", "-u", "fountain", "/bin/bash", "-c"].map(String::from)[..],
            &["_", host.root.to_str().unwrap(), a_dir.to_str().unwrap()].map(String::from)[..]
        )
    );
    assert!(find[5].starts_with("export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1"), "hardened git");
    let g = calls
        .iter()
        .find(|c| c[5].contains("illogical-untracked") && c[5].starts_with("export GIT_CONFIG_GLOBAL=/dev/null"))
        .expect("the diff's git, hardened");
    assert_eq!(
        (g[0].as_str(), g[2].as_str(), g[3].as_str(), g[7].as_str()),
        ("-n", "fountain", "/bin/bash", repo.to_str().unwrap())
    );
    assert_eq!(g.last().unwrap(), EMPTY_TREE);
    // A clone: counted from its upstream.
    let b_dir = PathBuf::from(b["path"].as_str().unwrap());
    git(&b_dir, &["clone", "-q", repo.to_str().unwrap(), "clone"]);
    let clone = b_dir.join("clone");
    let base = git(&clone, &["rev-parse", "HEAD"]);
    std::fs::write(clone.join("hello.txt"), "hello, edited\n").unwrap();
    git(&clone, &["commit", "-qam", "edit"]);
    let out = d.call(block, "changes", json!({ "sandbox": b["name"] }));
    let co = out["checkouts"].as_array().unwrap();
    assert_eq!((co.len(), co[0]["rev_a"].as_str()), (1, Some(base.as_str())), "{out}");
    let diff = co[0]["block"].as_u64().unwrap();
    d.wait_for("the clone's diff", || d.state(diff)["files"].as_array().is_some_and(|f| f.len() == 1));
    assert_eq!(d.state(diff)["files"][0]["path"], "hello.txt");
    // No checkout: said so.
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{block}/call/changes"), Some(json!({ "sandbox": "s-hostile" })));
    assert_eq!(status, 400);
    assert!(body.contains("no git checkout"), "{body}");
    // run_as: only fountain, only an absolute directory.
    for config in [json!({ "repo": repo, "run_as": "root" }), json!({ "repo": "rel/dir", "run_as": "fountain" })] {
        let (status, body) =
            d.raw("POST", "/api/blocks", Some(json!({ "type": "diff", "config": config, "local": true })));
        assert_eq!(status, 400, "{config}: {body}");
    }

    // Follow: an agent block on the conversation, through session/load.
    let conv = a["conversations"][0]["id"].as_str().unwrap().to_owned();
    std::fs::write(
        d.sessions.join(format!("{conv}.json")),
        json!({ "updates": [{ "sessionUpdate": "agent_message_chunk", "messageId": "m1",
            "content": { "type": "text", "text": "made r1-check" } }], "cwd": "/" })
        .to_string(),
    )
    .unwrap();
    let out = d.call(block, "follow", json!({ "conversation": conv }));
    assert_eq!(out["agent"], "hud-playground");
    let agent = out["block"].as_u64().unwrap();
    d.wait_for("the conversation loaded", || {
        entries(&d.state(agent)).iter().any(|e| e["text"].as_str().is_some_and(|t| t.contains("made r1-check")))
    });
    d.wait_for("the agent's config saved", || layout_config(&d, agent)["session_id"] == conv.as_str());
    assert_eq!(layout_config(&d, agent)["fountain_agent"], "hud-playground");
    let (status, _) =
        d.raw("POST", &format!("/api/blocks/{block}/call/follow"), Some(json!({ "conversation": "nope" })));
    assert_eq!(status, 400);

    // MCP: open_fountain with view "runner", beside an agent.
    let me = d.open("hello");
    assert_eq!(d.wait(me, "idle"), "done");
    let r = agent_mcp(&d, me, "open_fountain", json!({ "view": "runner" })).unwrap();
    assert!(r["text"].as_str().unwrap().contains("this host: runner-1 (unit fountain-runner active), online"), "{r}");
    let rb = r["block"].as_u64().unwrap();
    d.wait_for("its view saved", || layout_config(&d, rb)["view"] == "runner");
    assert!(agent_mcp(&d, me, "open_fountain", json!({ "view": "nope" })).is_err());
}

#[test]
fn runner_attention_offline_and_another_online() {
    let dir = Scratch::new("fountain-runner-attention");
    let fz = Fountain::start(&dir);
    let host = RunnerHost::new(&dir, &fz);
    // Offline, with no last-seen time: counted from when the view saw it.
    fz.f.with(|i| {
        i.runners["data"][0]["online"] = false.into();
        i.runners["data"][0]["last_seen_at"] = Value::Null;
    });
    let d = host.daemon(&fz);
    let (block, st) = runner_view(&d);
    assert_eq!(st["runner"]["attention"], Value::Null, "within the grace: {st}");
    assert!(st["runner"]["offline_since_ms"].is_u64());
    // Nobody draws it: it still reads, and raises once the grace is over.
    d.wait_for("offline attention", || info(&d, block)["reason"]["kind"] == "failed");
    let r = info(&d, block)["reason"].clone();
    assert_eq!(r["bundle"], "failed:fountain-runner");
    assert_eq!(r["headline"], "Fountain runner offline: runner-1 (its fountain-runner unit is active)");
    assert!(capture(&d, block).contains("! Fountain runner offline: runner-1"));
    assert!(d.get("/api/host")["fountain_runner"]["problem"].as_str().is_some_and(|p| p.contains("offline")));
    // Back: cleared.
    fz.f.with(|i| i.runners["data"][0]["online"] = true.into());
    d.wait_for("cleared", || info(&d, block)["reason"].is_null());
    // Another runner online would win placement.
    fz.f.with(|i| {
        let mut other = fixture("runners.json")["data"][0].clone();
        other["name"] = "laptop".into();
        i.runners["data"].as_array_mut().unwrap().push(other);
    });
    d.wait_for("another runner's attention", || {
        info(&d, block)["reason"]["headline"]
            == "Another Fountain runner is online: laptop (it would win placement over runner-1)"
    });
    assert_eq!(info(&d, block)["reason"]["bundle"], "failed:fountain-runner");
    let st = d.state(block);
    assert_eq!(st["runner"]["others"][0]["name"], "laptop");
    // The unit stopped on purpose and the other gone: nothing wants anyone.
    fz.f.with(|i| {
        i.runners["data"].as_array_mut().unwrap().truncate(1);
        i.runners["data"][0]["online"] = false.into();
    });
    std::fs::write(&host.state, "inactive\n").unwrap();
    d.wait_for("nothing raised", || {
        info(&d, block)["reason"].is_null() && d.state(block)["runner"]["unit_active"] == false
    });
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert!(info(&d, block)["reason"].is_null());
    assert!(host.sudo_calls().is_empty());
}

#[test]
fn an_editor_cant_reach_the_runner() {
    let dir = Scratch::new("fountain-runner-editor");
    let fz = Fountain::start(&dir);
    let host = RunnerHost::new(&dir, &fz);
    let d = host.daemon(&fz);
    let (block, st) = runner_view(&d);
    let a = st["runner"]["sandboxes"][0].clone();
    let session = info(&d, block)["session"].as_u64().unwrap();
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "editor" }));
    let call = |m: &str, args: Value| as_friend(&d, "POST", &format!("/api/blocks/{block}/call/{m}"), args);
    let (status, body) = call("refresh", json!({}));
    assert_eq!(status, 200, "{body}");
    let panes = d.get("/api/panes").as_array().unwrap().len();
    for (m, args) in [
        ("shell", json!({ "sandbox": a["id"] })),
        ("changes", json!({ "sandbox": a["id"] })),
        ("follow", json!({ "conversation": a["conversations"][0]["id"] })),
        ("view", json!({ "view": "catalog" })),
    ] {
        let (status, body) = call(m, args);
        assert_eq!(status, 403, "{m}: {body}");
    }
    // Nor a diff of its own as fountain: guests open agents only.
    let (status, _) = as_friend(
        &d,
        "POST",
        "/api/blocks",
        json!({ "type": "diff", "config": { "repo": a["path"], "run_as": "fountain" }, "split": block }),
    );
    assert!(status == 400 || status == 403, "{status}");
    assert_eq!(d.get("/api/panes").as_array().unwrap().len(), panes, "nothing opened");
    assert!(host.sudo_calls().is_empty(), "no sudo for an editor");
    // The owner may.
    let out = d.call(block, "shell", json!({ "sandbox": a["id"] }));
    assert!(out["pane"].is_u64(), "{out}");
}

#[test]
fn a_hostile_repository_runs_nothing_and_paths_stay_in_the_root() {
    let dir = Scratch::new("fountain-runner-hostile");
    let fz = Fountain::start(&dir);
    let host = RunnerHost::new(&dir, &fz);
    // Markers any of its commands would leave.
    let marks = dir.join("marks");
    std::fs::create_dir_all(&marks).unwrap();
    let evil = dir.join("evil.sh");
    std::fs::write(
        &evil,
        format!("#!/bin/sh\ntouch '{}'/\"$1\"\ncase $1 in lazyfetch) exit 1 ;; esac\ncat\n", marks.display()),
    )
    .unwrap();
    std::fs::set_permissions(&evil, std::fs::Permissions::from_mode(0o755)).unwrap();
    let e = |what: &str| format!("{} {what}", evil.display());
    // A global config (the daemon's HOME) that would run one too.
    std::fs::write(fz.home.join(".gitconfig"), format!("[core]\n\tfsmonitor = {}\n", e("global"))).unwrap();
    let sbs = fz.f.with(|i| i.sandboxes.clone());
    let a = sbs["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["sprite_name"].as_str().unwrap().ends_with("-2972e1a2"))
        .unwrap()
        .clone();
    let repo = PathBuf::from(a["runner"]["path"].as_str().unwrap()).join("evil");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    for f in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(repo.join(f), "one\n").unwrap();
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "first"]);
    // Now the repository turns hostile: filters, fsmonitor, pager, external
    // diff and textconv, each a command that leaves a marker.
    for (k, v) in [
        ("filter.evil.clean", e("clean")),
        ("filter.evil.smudge", e("smudge")),
        ("filter.evil.required", "true".into()),
        ("filter.Proc.process", e("process")),
        ("core.fsmonitor", e("fsmonitor")),
        ("core.pager", e("pager")),
        ("diff.external", e("external")),
        ("diff.evil.textconv", e("textconv")),
        ("diff.evil.command", e("command")),
    ] {
        git(&repo, &["config", k, &v]);
    }
    std::fs::write(
        repo.join(".gitattributes"),
        "a.txt filter=evil diff=evil\nb.txt filter=Proc\n*.new filter=evil diff=evil\n",
    )
    .unwrap();
    std::fs::write(repo.join("a.txt"), "two\n").unwrap();
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    std::fs::write(repo.join("u.new"), "untracked\n").unwrap();
    // A partial clone whose missing blobs a read would fetch, through an
    // upload-pack that leaves a marker.
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    std::fs::write(src.join("p.txt"), "promised\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-qm", "src"]);
    git(&src, &["config", "uploadpack.allowFilter", "true"]);
    let part = repo.parent().unwrap().join("part");
    git(
        repo.parent().unwrap(),
        &["clone", "-q", "--no-checkout", "--filter=blob:none", &format!("file://{}", src.display()), "part"],
    );
    git(&part, &["config", "remote.origin.uploadpack", &e("lazyfetch")]);
    // It is hostile: a plain `git diff` runs its clean filter, and the
    // partial clone's read fetches.
    let _ = std::process::Command::new("git").args(["diff", "--stat"]).current_dir(&repo).output();
    assert!(marks.join("clean").exists(), "the fixture's filter didn't run: the test proves nothing");
    let _ = std::process::Command::new("git").args(["diff", "HEAD"]).current_dir(&part).output();
    assert!(marks.join("lazyfetch").exists(), "the partial clone didn't fetch: the test proves nothing");
    std::fs::remove_dir_all(&marks).unwrap();
    std::fs::create_dir_all(&marks).unwrap();
    // Touch them again, so git must look (stat-dirty).
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(repo.join("a.txt"), "three\n").unwrap();
    std::fs::write(repo.join("b.txt"), "three\n").unwrap();

    // Hostile places: a path Fountain gives outside the root, and a sandbox
    // that is a symlink out of it.
    let id = RUNNER.replace('-', "");
    let outside = dir.join("outside");
    std::fs::create_dir_all(outside.join("r/.git")).unwrap();
    let link = host.root.join(format!("runner-{id}-link"));
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    fz.f.with(|i| {
        let data = i.sandboxes["data"].as_array_mut().unwrap();
        for (sid, name, path) in [
            ("s-elsewhere", format!("runner-{id}-elsewhere"), "/etc".to_owned()),
            ("s-link", format!("runner-{id}-link"), link.display().to_string()),
        ] {
            data.push(json!({ "id": sid, "sprite_name": name, "status": "ready", "provider": "runner",
                "runner": { "id": RUNNER, "path": path }, "conversations": [] }));
        }
    });
    let d = host.daemon(&fz);
    let (block, st) = runner_view(&d);
    let rows = st["runner"]["sandboxes"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap().clone();
    assert_eq!(row("s-elsewhere")["path"], Value::Null, "Fountain's /etc isn't the root's");
    assert_eq!(row("s-link")["path"], link.display().to_string());

    // Changes: the hostile repository's diff, and none of its commands.
    let out = d.call(block, "changes", json!({ "sandbox": a["id"] }));
    let co = out["checkouts"].as_array().unwrap();
    let evil_co = co.iter().find(|c| c["repo"] == repo.display().to_string()).expect("the hostile checkout");
    let diff = evil_co["block"].as_u64().unwrap();
    d.wait_for("the hostile diff", || d.state(diff)["files"].as_array().is_some_and(|f| f.len() >= 4));
    for f in ["a.txt", "b.txt"] {
        d.call(diff, "file", json!({ "path": f, "open": true }));
    }
    d.call(diff, "refresh", json!({}));
    let files: Vec<String> =
        d.state(diff)["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_owned()).collect();
    assert!(["a.txt", "b.txt", "u.new"].iter().all(|f| files.contains(&f.to_string())), "{files:?}");
    // The partial clone: read, and nothing fetched.
    let part_co = co.iter().find(|c| c["repo"] == part.display().to_string()).expect("the partial clone");
    let pd = part_co["block"].as_u64().unwrap();
    d.wait_for("the partial clone's diff read", || d.state(pd)["loading"] == false);
    d.call(pd, "refresh", json!({})).to_string();
    let ran: Vec<_> = std::fs::read_dir(&marks).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(ran.is_empty(), "the repository ran {ran:?}");
    // Its summary has no cwd (a sandbox isn't yours to start things in).
    let p = d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == diff).cloned().unwrap();
    assert!(p["cwd"].is_null(), "{p}");

    // Outside the root: refused by name, and by real path.
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{block}/call/changes"), Some(json!({ "sandbox": "s-elsewhere" })));
    assert_eq!(status, 400, "{body}");
    let (status, body) =
        d.raw("POST", &format!("/api/blocks/{block}/call/changes"), Some(json!({ "sandbox": "s-link" })));
    assert_eq!(status, 400);
    assert!(body.contains("inside the runner's sandboxes"), "{body}");
    let out = d.call(block, "shell", json!({ "sandbox": "s-link" }));
    let pane = out["pane"].as_u64().unwrap();
    assert!(pane > 0);
    // Its sudo call, run again here: it refuses before any cd.
    let shell_call = || {
        host.sudo_calls()
            .into_iter()
            .find(|c| c[5].contains("--noprofile") && c.last() == Some(&link.display().to_string()))
    };
    d.wait_for("the shell's sudo", || shell_call().is_some());
    let call = shell_call().unwrap();
    let out = std::process::Command::new("/bin/bash").args(&call[4..]).output().unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("not inside the runner's sandboxes"), "{out:?}");
    // A diff as fountain outside the root can't even be opened.
    let (status, body) = d.raw(
        "POST",
        "/api/blocks",
        Some(json!({ "type": "diff", "config": { "repo": outside.join("r"), "run_as": "fountain" }, "local": true })),
    );
    assert_eq!(status, 400, "{body}");
}

#[test]
fn a_failing_runners_read_doesnt_spin() {
    let dir = Scratch::new("fountain-runner-spin");
    let fz = Fountain::start(&dir);
    let host = RunnerHost::new(&dir, &fz);
    // Offline with no last-seen time: an early re-read is due at the grace.
    fz.f.with(|i| {
        i.runners["data"][0]["online"] = false.into();
        i.runners["data"][0]["last_seen_at"] = Value::Null;
    });
    let d = host.daemon(&fz);
    let (block, _) = runner_view(&d);
    fz.f.with(|i| i.fail_runners = true);
    d.wait_for("a failed read", || d.state(block)["error"].is_string());
    let before = fz.f.gets("runners");
    std::thread::sleep(std::time::Duration::from_secs(4));
    let n = fz.f.gets("runners") - before;
    // Not drawn: every 1.5 s at most (5 × the 300 ms poll).
    assert!(n <= 4, "{n} reads of /api/runners in 4 s");
    // Back: it reads again, and judges.
    fz.f.with(|i| i.fail_runners = false);
    d.wait_for("read again", || d.state(block)["error"].is_null());
}

#[test]
fn follow_never_starts_a_new_conversation() {
    let dir = Scratch::new("fountain-follow");
    let fz = Fountain::start(&dir);
    let d = fz.daemon(&[]);
    let news = |d: &Daemon| {
        std::fs::read_dir(&d.sessions)
            .map(|r| r.filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("fake-")).count())
            .unwrap_or(0)
    };
    for (agent, why) in [("hud-playground", "couldn't load the conversation"), ("no-load-session", "no loadSession")] {
        let block = d.post(
            "/api/blocks",
            json!({ "type": "agent", "local": true, "config": {
                "agent": "fountain", "fountain_agent": agent, "session_id": "no-such-conversation", "follow": true } }),
        )["block"]
            .as_u64()
            .unwrap();
        d.wait_for("it stopped", || d.state(block)["status"] == "exited");
        let st = d.state(block);
        let err = st["error"].as_str().unwrap_or_default();
        assert!(err.contains(why) && err.contains("Follow never starts a new conversation"), "{agent}: {st}");
        assert!(entries(&st).iter().any(|e| e["text"].as_str().is_some_and(|t| t.contains(why))), "{st}");
        assert_eq!(st["session_id"], "no-such-conversation");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(news(&d), 0, "{agent}: a new session was made");
        assert_eq!(d.state(block)["status"], "exited");
    }
}

#[test]
fn a_root_reached_through_a_symlink_still_works() {
    // As on macOS, where /var is /private/var: the unit names the root by a
    // path through a symlink, and the scripts see real paths.
    let dir = Scratch::new("fountain-runner-alias");
    let fz = Fountain::start(&dir);
    let host = RunnerHost::new(&dir, &fz);
    let alias = dir.join("alias");
    std::os::unix::fs::symlink(&host.root, &alias).unwrap();
    let unit = PathBuf::from(&host.env.iter().find(|(k, _)| k == "ILLOGICAL_FOUNTAIN_UNIT_FILE").unwrap().1);
    let text = std::fs::read_to_string(&unit).unwrap().replace(host.root.to_str().unwrap(), alias.to_str().unwrap());
    std::fs::write(&unit, text).unwrap();
    fz.f.with(|i| {
        let t = i.sandboxes.to_string().replace(host.root.to_str().unwrap(), alias.to_str().unwrap());
        i.sandboxes = serde_json::from_str(&t).unwrap();
    });
    let d = host.daemon(&fz);
    let (block, st) = runner_view(&d);
    let a = st["runner"]["sandboxes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"].as_str().unwrap().ends_with("-2972e1a2"))
        .unwrap()
        .clone();
    assert!(a["path"].as_str().unwrap().starts_with(alias.to_str().unwrap()), "{a}");
    let repo = host.root.join(a["name"].as_str().unwrap()).join("r");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("x.txt"), "x\n").unwrap();
    // Changes: found by its real path, and its diff (under the canonical
    // root) opens and reads.
    let out = d.call(block, "changes", json!({ "sandbox": a["id"] }));
    let co = &out["checkouts"][0];
    assert_eq!(co["repo"], repo.display().to_string(), "{out}");
    let diff = co["block"].as_u64().unwrap();
    d.wait_for("the diff", || d.state(diff)["files"].as_array().is_some_and(|f| f.len() == 1));
    // Shell: its check passes (run here, as its sudo would).
    d.call(block, "shell", json!({ "sandbox": a["id"] }));
    let call = || host.sudo_calls().into_iter().find(|c| c[5].contains("--noprofile"));
    d.wait_for("the shell's sudo", || call().is_some());
    let mut argv = call().unwrap()[4..].to_vec();
    argv[1] = argv[1].replace("exec bash --noprofile --norc", "pwd -P");
    let out = std::process::Command::new("/bin/bash").args(&argv).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        host.root.join(a["name"].as_str().unwrap()).display().to_string(),
        "{out:?}"
    );
}
