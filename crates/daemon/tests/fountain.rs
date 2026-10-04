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
