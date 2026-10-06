//! M16: illogical as MCP tools, against the real daemon. An rmcp client
//! through `illogical mcp` (the stdio bridge on the Unix socket) and over
//! HTTP with client tokens; and an agent block (`fake_acp.py`) using the
//! server illogical hands it, scoped to its tab: a dev server and a browser
//! block beside itself, other tabs refused, and a second agent started,
//! waited on and answered. With wisp on this host (skipped without its
//! token), an agent block in a VM too, through the relay the daemon opens
//! into it (#59).

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

mod agentd;
mod replay;

use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use agentd::*;
use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation, NumberOrString,
        ProgressNotificationParam, ProgressToken, RequestMetaObject,
    },
    service::{NotificationContext, RunningService},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};

fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

/// An MCP client that names itself, and counts progress notifications.
#[derive(Clone)]
struct Client {
    name: &'static str,
    progress: Arc<AtomicUsize>,
}

impl Client {
    fn named(name: &'static str) -> Self {
        Self { name, progress: Default::default() }
    }
}

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new(self.name, "1"))
    }

    async fn on_progress(&self, _p: ProgressNotificationParam, _c: NotificationContext<RoleClient>) {
        self.progress.fetch_add(1, Ordering::Relaxed);
    }
}

type Session = RunningService<RoleClient, Client>;

/// `illogical mcp` against the daemon's socket, as Claude Code or Codex
/// would start it.
async fn bridge(d: &Daemon, client: Client) -> Session {
    let mut cmd = tokio::process::Command::new(cli_bin());
    cmd.arg("--socket").arg(d.sock()).arg("mcp").env_remove("ILLOGICAL_PANE");
    client.serve(TokioChildProcess::new(cmd).unwrap()).await.expect("connecting through illogical mcp")
}

/// `/mcp` over TCP, with a bearer token.
async fn http(d: &Daemon, token: &str, client: Client) -> Result<Session, String> {
    let config =
        StreamableHttpClientTransportConfig::with_uri(format!("http://127.0.0.1:{}/mcp", d.port)).auth_header(token);
    client.serve(StreamableHttpClientTransport::from_config(config)).await.map_err(|e| e.to_string())
}

async fn call_raw(s: &Session, tool: &str, args: Value) -> CallToolResult {
    let params =
        CallToolRequestParams::new(tool.to_owned()).with_arguments(args.as_object().cloned().unwrap_or_default());
    let mut params = params;
    params.meta = Some(RequestMetaObject::with_progress_token(ProgressToken(NumberOrString::String("t".into()))));
    s.call_tool(params).await.unwrap_or_else(|e| panic!("{tool}: {e}"))
}

/// A tool's structured result; panics if it failed.
async fn call(s: &Session, tool: &str, args: Value) -> Value {
    let r = call_raw(s, tool, args.clone()).await;
    assert_ne!(r.is_error, Some(true), "{tool} {args}: {:?}", r.content);
    let v = r.structured_content.expect("structured content");
    assert!(v["summary"].is_string(), "every result says what happened: {v}");
    v
}

/// A tool's error sentence; panics if it worked.
async fn refused(s: &Session, tool: &str, args: Value) -> String {
    let r = call_raw(s, tool, args.clone()).await;
    assert_eq!(r.is_error, Some(true), "{tool} {args} should fail: {:?}", r.structured_content);
    r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_through_the_stdio_bridge() {
    let d = Daemon::child_env(&["--wisp-token-file", "/nonexistent"], &[("ILLOGICAL_MCP_PROGRESS_MS", "200")]);
    let client = Client::named("claude-code");
    let progress = client.progress.clone();
    let s = bridge(&d, client).await;

    // The tools, with honest annotations.
    let tools = s.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 39);
    let ro = |n: &str| tools.iter().find(|t| t.name == n).unwrap().annotations.as_ref().unwrap().read_only_hint;
    assert_eq!(
        (ro("read_output"), ro("wait"), ro("run"), ro("close")),
        (Some(true), Some(true), Some(false), Some(false))
    );

    // Run and wait: the exit code and the last lines.
    let dir = d.sessions.join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    let r = call(&s, "run", json!({ "command": "echo built-$((6*7))", "cwd": dir, "wait": true, "timeout": 20 })).await;
    assert_eq!((r["state"].as_str(), r["exit"].as_i64()), (Some("done"), Some(0)), "{r}");
    assert!(r["last_lines"].as_str().unwrap().contains("built-42"), "{r}");
    let pane = r["pane"].as_u64().unwrap();

    // It says who started it, in the pane and in history.
    let info = d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == pane).cloned().unwrap();
    assert_eq!(info["started_by"]["by"], "mcp:claude-code", "{info}");
    let failed = call(&s, "run", json!({ "command": "ls /nonexistent-m16", "cwd": dir, "wait": true })).await;
    // GNU ls exits 2 for a missing path, BSD ls (macOS) 1.
    let ls = Command::new("ls").arg("/nonexistent-m16").output().unwrap().status.code();
    assert_eq!(failed["exit"].as_i64(), ls.map(i64::from), "{failed}");
    let h = call(&s, "history", json!({ "failed": true, "cwd": dir })).await;
    let cmds = h["commands"].as_array().unwrap();
    assert_eq!(cmds.len(), 1, "{h}");
    assert_eq!(
        (cmds[0]["command"].as_str(), cmds[0]["by"].as_str()),
        (Some("ls /nonexistent-m16"), Some("mcp:claude-code"))
    );

    // Paged output: a long command's, a page at a time, all of it.
    let r = call(&s, "run", json!({ "command": "seq 1 4000", "wait": true })).await;
    let p = r["pane"].as_u64().unwrap();
    let mut text = String::new();
    let mut page = call(&s, "read_output", json!({ "pane": p, "last_command": true, "max_chars": 1000 })).await;
    let mut pages = 1;
    while page["more"] == true {
        text.push_str(page["text"].as_str().unwrap());
        assert!(page["text"].as_str().unwrap().len() <= 1000);
        page = call(
            &s,
            "read_output",
            json!({ "pane": p, "last_command": true, "offset": page["next_offset"], "max_chars": 1000 }),
        )
        .await;
        pages += 1;
    }
    text.push_str(page["text"].as_str().unwrap());
    let want: String = (1..=4000).map(|n| format!("{n}\n")).collect();
    assert!(pages > 10, "{pages} pages");
    assert_eq!(text.trim_start_matches("seq 1 4000\n"), want);

    // A long call: progress while it waits, then "still running" with
    // where to pick up; the agent calls again.
    let r = call(&s, "run", json!({ "command": "sleep 30" })).await;
    let p = r["pane"].as_u64().unwrap();
    // Running, not only typed: a ^C while bash is still expanding PS0 (its
    // command-start mark) cancels the line without a command end to wait
    // for.
    d.wait_for("sleep 30 to start", || {
        d.get("/api/panes").as_array().unwrap().iter().any(|x| x["id"] == p && x["current"]["text"] == "sleep 30")
    });
    let before = progress.load(Ordering::Relaxed);
    let w = call(&s, "wait", json!({ "pane": p, "until": "command_end", "timeout": 1.5 })).await;
    assert_eq!(w["state"], "still running", "{w}");
    assert!(w["next_offset"].is_u64() && w["summary"].as_str().unwrap().contains("sleep 30"), "{w}");
    assert!(progress.load(Ordering::Relaxed) > before, "progress while waiting");
    call(&s, "send_input", json!({ "pane": format!("%{p}"), "keys": ["C-c"] })).await;
    let w = call(&s, "wait", json!({ "pane": p, "until": "command_end", "timeout": 10 })).await;
    assert_eq!((w["state"].as_str(), w["exit"].as_i64()), (Some("done"), Some(130)), "{w}");

    // Typing, and matching what comes out.
    call(&s, "send_input", json!({ "pane": p, "text": "echo typed-$((2+2))" })).await;
    let m = call(&s, "wait", json!({ "pane": p, "until": "match", "pattern": "typed-4", "timeout": 10 })).await;
    assert_eq!(m["match"], "typed-4", "{m}");
    let screen = call(&s, "capture_screen", json!({ "pane": p })).await;
    assert!(screen["text"].as_str().unwrap().contains("typed-4"));

    // list, search, and the resources.
    let l = call(&s, "list", json!({})).await;
    assert!(
        l["panes"].as_array().unwrap().iter().any(|e| e["pane"] == p && e["started_by"] == "mcp:claude-code"),
        "{l}"
    );
    let hits = call(&s, "search", json!({ "pattern": "built-42" })).await;
    assert!(hits["hits"].as_array().unwrap().iter().any(|h| h["pane"] == pane), "{hits}");
    let templates = s.list_all_resource_templates().await.unwrap();
    assert_eq!(templates.len(), 3);
    let res = s
        .read_resource(rmcp::model::ReadResourceRequestParams::new(format!("illogical://pane/{pane}/output")))
        .await
        .unwrap();
    assert!(format!("{:?}", res.contents).contains("built-42"));

    // A file.
    std::fs::write(dir.join("notes.txt"), "line one\nline two\n").unwrap();
    let f = call(&s, "read_file", json!({ "path": "notes.txt", "pane": pane })).await;
    assert_eq!(f["text"], "line one\nline two\n", "{f}");

    // Closed: an error that says what happened, and its output still reads.
    call(&s, "close", json!({ "pane": pane })).await;
    d.wait_for("it to close", || !d.get("/api/panes").as_array().unwrap().iter().any(|x| x["id"] == pane));
    let why = refused(&s, "wait", json!({ "pane": pane, "until": "exit" })).await;
    assert!(why.contains(&format!("pane %{pane} is gone")) && why.contains("exited 0"), "{why}");
    let gone = call(&s, "read_output", json!({ "pane": pane })).await;
    assert!(gone["text"].as_str().unwrap().contains("built-42"), "{gone}");
    s.cancel().await.unwrap();
}

/// As Claude Code speaks it (2026-07-28, stateless, `server/discover`), on
/// the daemon's socket.
#[tokio::test(flavor = "multi_thread")]
async fn stateless_clients_get_the_cache_hints_claude_code_wants() {
    use rmcp::{
        ClientLifecycleMode, ClientServiceExt,
        model::{CacheScope, ProtocolVersion},
    };
    let d = Daemon::child();
    let sock = d.sock().display().to_string();
    let transport = StreamableHttpClientTransport::from_unix_socket(sock.as_str(), "http://localhost/mcp");
    let s = Client::named("claude-code")
        .serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
        )
        .await
        .unwrap();
    // Without ttlMs and cacheScope, Claude Code 2.1.287 refuses the list
    // (and retries it, then gives up: no tools).
    let tools = s.list_tools(None).await.unwrap();
    assert_eq!((tools.ttl_ms, tools.cache_scope), (Some(0), Some(CacheScope::Private)));
    assert_eq!(tools.tools.len(), 39);
    let t = s.list_resource_templates(None).await.unwrap();
    assert_eq!((t.ttl_ms, t.cache_scope), (Some(0), Some(CacheScope::Private)));
    let r = call(&s, "run", json!({ "command": "echo stateless", "wait": true })).await;
    assert_eq!(r["exit"], 0);
    let info = d.get(&format!("/api/blocks/{}", r["pane"]))["info"].clone();
    assert_eq!(info["started_by"]["by"], "mcp:claude-code", "the name from each request's _meta");
    s.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bridge_outlives_a_daemon_restart() {
    let mut d = Daemon::child();
    let s = bridge(&d, Client::named("codex")).await;
    call(&s, "list", json!({})).await;
    // Its session is gone with the daemon; the bridge opens another.
    d.stop();
    d.start();
    let l = call(&s, "list", json!({})).await;
    assert!(l["panes"].is_array());
    s.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn http_with_a_token_until_it_is_revoked() {
    let d = Daemon::child();
    // A client on another machine: a token of its own.
    let t = d.post("/api/mcp/tokens", json!({ "name": "laptop" }));
    let token = t["token"].as_str().unwrap().to_owned();
    assert!(!d.get("/api/mcp/tokens").to_string().contains(&token), "listing never shows a token");
    let s = http(&d, &token, Client::named("claude-code")).await.unwrap();
    let r = call(&s, "run", json!({ "command": "echo from-afar", "wait": true })).await;
    assert_eq!(r["exit"], 0);
    let used = d.get("/api/mcp/tokens");
    assert!(used[0]["used_ms"].is_u64(), "{used}");

    // Read-only tokens see and call the read-only tools only.
    let ro =
        d.post("/api/mcp/tokens", json!({ "name": "watcher", "scope": "read" }))["token"].as_str().unwrap().to_owned();
    let w = http(&d, &ro, Client::named("watcher")).await.unwrap();
    assert_eq!(w.list_all_tools().await.unwrap().len(), 15);
    assert!(refused(&w, "run", json!({ "command": "true" })).await.contains("may only read"));
    // #234: nor ask to invite anyone.
    let invite = json!({ "who": "tailnet:sam@example.com", "pane": 1, "note": "x" });
    assert!(refused(&w, "invite_person", invite).await.contains("may only read"));
    call(&w, "list", json!({})).await;

    // Revoked: cut off at its next call, and it can't start again.
    let (status, _) = d.raw("DELETE", "/api/mcp/tokens/laptop", None);
    assert_eq!(status, 200);
    assert!(s.call_tool(CallToolRequestParams::new("list")).await.is_err(), "revoked mid-session");
    assert!(http(&d, &token, Client::named("claude-code")).await.is_err());
    assert!(http(&d, "ilm_0000", Client::named("claude-code")).await.is_err(), "an unknown token");
    // Something that isn't a bearer token is no way around the owner's check.
    assert_eq!(post_mcp(d.port, "Basic b3duZXI6eA=="), 401);
    assert_eq!(post_mcp(d.port, "Bearer "), 401);
    call(&w, "list", json!({})).await;

    // A web page elsewhere can't use it from a browser (exact Origin).
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}"#;
    let res = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/mcp", d.port))
        .header("Authorization", d.bearer())
        .header("Origin", "https://evil.example")
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
}

/// The agent's own MCP call, through the server illogical gave it: the
/// result's structured content, or its error.
fn agent_mcp(d: &Daemon, agent: u64, tool: &str, args: Value) -> Result<Value, String> {
    let answers = || -> Vec<String> {
        entries(&d.state(agent))
            .into_iter()
            .filter_map(|e| e["text"].as_str().and_then(|t| t.strip_prefix("MCP ")).map(str::to_owned))
            .collect()
    };
    let before = answers().len();
    d.call(agent, "send", json!({ "text": format!("mcp {tool} {args}") }));
    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    let said = loop {
        let now = answers();
        if now.len() > before {
            break now.last().cloned().unwrap();
        }
        assert!(std::time::Instant::now() < deadline, "no answer to {tool}: {}", d.state(agent));
        std::thread::sleep(Duration::from_millis(100));
    };
    let r: Value = serde_json::from_str(&said).unwrap();
    if r["isError"] == true || r.get("error").is_some() {
        return Err(r.to_string());
    }
    Ok(r["structuredContent"].clone())
}

/// A `tools/list` to `/mcp` with this Authorization: the HTTP status.
fn post_mcp(port: u16, auth: &str) -> u16 {
    use std::io::{BufRead, BufReader, Write};
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    c.write_all(
        format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: {auth}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(c).read_line(&mut line).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn tab_of(d: &Daemon, pane: u64) -> u64 {
    let panes = d.get("/api/panes");
    panes.as_array().unwrap().iter().find(|p| p["id"] == pane).map(|p| p["tab"].as_u64().unwrap()).expect("open")
}

#[test]
fn an_agent_block_works_in_its_own_tab() {
    let d = Daemon::child_with(&["--wisp-token-file", "/nonexistent", "--block-listen", "127.0.0.1:0"]);
    // Another tab, with work the agent mustn't touch.
    let other = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    d.post(&format!("/api/panes/{other}/send"), json!({ "text": "echo other-tab-secret", "enter": true }));
    let a = d.open("hello");
    d.wait(a, "idle");

    // It was given illogical: loopback /mcp with a token of its own, which
    // its log doesn't keep.
    let servers = std::fs::read_dir(&d.sessions)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("mcp-"))
        .map(|e| serde_json::from_slice::<Value>(&std::fs::read(e.path()).unwrap()).unwrap())
        .expect("the session's MCP servers");
    let ours = servers.as_array().unwrap().iter().find(|s| s["name"] == "illogical").cloned().unwrap();
    assert_eq!(ours["type"], "http");
    assert_eq!(ours["url"], format!("http://127.0.0.1:{}/mcp", d.port));
    let auth = ours["headers"][0]["value"].as_str().unwrap().to_owned();
    assert!(auth.starts_with("Bearer ilb_"), "{auth}");
    let token = auth.trim_start_matches("Bearer ");
    let log = std::fs::read_dir(d.state.join(format!("blocks/{a}")))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("seg-"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect::<String>();
    assert!(log.contains("<redacted>") && !log.contains(token), "the token stays out of the log");

    // Its project's dev server, in a pane beside it... `python3 -m
    // http.server`'s, without its HTTPServer, which looks up 127.0.0.1's
    // name before it serves (35s on a Mac whose DNS doesn't answer that).
    let port = free_port();
    let serve = format!(
        "python3 -c 'import http.server as h, socketserver as s; \
         v = s.TCPServer((\"127.0.0.1\", {port}), h.SimpleHTTPRequestHandler); \
         print(\"Serving HTTP\", flush=True); v.serve_forever()'"
    );
    let r = agent_mcp(&d, a, "run", json!({ "command": serve, "cwd": d.sessions })).unwrap();
    let server = r["pane"].as_u64().unwrap();
    assert_eq!(tab_of(&d, server), tab_of(&d, a), "in its own tab");
    let m =
        agent_mcp(&d, a, "wait", json!({ "pane": server, "until": "match", "pattern": "Serving HTTP", "timeout": 30 }))
            .unwrap();
    assert_eq!(m["state"], "matched", "{m}");
    // ...and the page in a browser block beside itself.
    let b = agent_mcp(&d, a, "open_port", json!({ "port": port })).unwrap()["block"].as_u64().unwrap();
    assert_eq!(tab_of(&d, b), tab_of(&d, a));
    let info = d.get(&format!("/api/blocks/{b}"))["info"].clone();
    assert_eq!((info["type"].as_str(), info["started_by"]["block"].as_u64()), (Some("browser"), Some(a)), "{info}");
    d.wait_for("the page to load", || d.state(b)["title"].as_str().is_some_and(|t| t.contains("Directory listing")));

    // It sees its own tab only.
    let l = agent_mcp(&d, a, "list", json!({})).unwrap();
    let seen: Vec<u64> = l["panes"].as_array().unwrap().iter().map(|p| p["pane"].as_u64().unwrap()).collect();
    assert!(seen.contains(&a) && seen.contains(&server) && seen.contains(&b) && !seen.contains(&other), "{l}");
    // Touching another tab is refused.
    for (tool, args) in [
        ("send_input", json!({ "pane": other, "text": "rm -rf ~" })),
        ("read_output", json!({ "pane": other })),
        ("close", json!({ "pane": other })),
        ("run", json!({ "command": "true", "split": other })),
        ("run", json!({ "command": "true", "vm": true })),
        ("open_port", json!({ "port": port, "beside": other })),
    ] {
        let e = agent_mcp(&d, a, tool, args.clone()).expect_err(&format!("{tool} {args}"));
        assert!(e.contains("own tab") || e.contains("another tab"), "{tool}: {e}");
    }
    let h = agent_mcp(&d, a, "history", json!({})).unwrap();
    assert!(!h.to_string().contains("other-tab-secret"), "{h}");
    // In its tab, it reads what it didn't start, but doesn't drive it.
    let e = agent_mcp(&d, a, "close", json!({ "pane": a })).unwrap_err();
    assert!(e.contains("wasn't started by this agent"), "{e}");
    agent_mcp(&d, a, "close", json!({ "pane": server })).unwrap();

    // Closed, its token goes with it.
    d.post(&format!("/api/panes/{a}/close"), json!({}));
    d.wait_for("the block to close", || d.raw("GET", &format!("/api/blocks/{a}"), None).0 == 404);
    assert_eq!(post_mcp(d.port, &auth), 401);
}

#[test]
fn one_agent_starts_another_and_answers_its_question() {
    let d = Daemon::child();
    let a = d.open("hello");
    d.wait(a, "idle");
    let b = agent_mcp(
        &d,
        a,
        "start_agent",
        json!({ "agent": "acp", "command": format!("python3 {}", fake()), "prompt": "ask one" }),
    )
    .unwrap()["block"]
        .as_u64()
        .unwrap();
    assert_eq!(tab_of(&d, b), tab_of(&d, a), "beside it");
    let w = agent_mcp(&d, a, "wait", json!({ "pane": b, "until": "needs_input", "timeout": 30 })).unwrap();
    assert_eq!(w["state"], "needs_input", "{w}");
    assert_eq!(w["ask"]["questions"][0]["question"], "Which colour do you prefer?", "{w}");
    agent_mcp(&d, a, "agent_respond", json!({ "pane": b, "action": "answer", "answers": { "question_0": "Blue" } }))
        .unwrap();
    let w = agent_mcp(&d, a, "wait", json!({ "pane": b, "until": "idle", "timeout": 30 })).unwrap();
    assert_ne!(w["state"], "needs_input", "{w}");
    let t = agent_mcp(&d, a, "read_output", json!({ "pane": b })).unwrap();
    assert!(t["text"].as_str().unwrap().contains("You answered: Which colour do you prefer? Blue"), "{t}");
    // Who answered is on record.
    let s = d.state(b);
    assert!(s.to_string().contains("mcp:fake-agent"), "{s}");

    // #163: it hands out tools and a mode, but never every check off.
    let start = |mode: &str| {
        let args = json!({ "agent": "acp", "command": format!("python3 {}", fake()), "prompt": "mode",
            "allow": ["Bash"], "permission_mode": mode });
        agent_mcp(&d, a, "start_agent", args)
    };
    let e = start("bypassPermissions").unwrap_err();
    assert!(e.contains("only the user"), "{e}");
    let c = start("auto").unwrap()["block"].as_u64().unwrap();
    d.wait(c, "idle");
    let s = d.state(c);
    assert_eq!((s["allow"].clone(), s["permission_mode"].as_str()), (json!([{ "tool": "Bash" }]), Some("auto")));
    assert!(entries(&s).iter().any(|e| e["text"] == "Mode: auto"), "{s}");
}

/// M71: an agent attaches a file of its own: to an agent it started, as
/// that agent's prompt with an image in it; into a shell it started, its
/// path pasted; not into a program that wouldn't read a path, nor into
/// what it didn't start.
#[test]
fn an_agent_attaches_a_screenshot_to_a_pane() {
    let tmp = std::env::temp_dir().join(format!("ilg-attach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let d = Daemon::child_env(&[], &[("TMPDIR", tmp.to_str().unwrap())]);
    let a = d.open("hello");
    d.wait(a, "idle");

    // To an agent it started, as base64.
    let command = format!("python3 {} --images", fake());
    let b = agent_mcp(&d, a, "start_agent", json!({ "agent": "acp", "command": command, "prompt": "hello" })).unwrap()
        ["block"]
        .as_u64()
        .unwrap();
    d.wait(b, "idle");
    agent_mcp(&d, a, "attach", json!({ "pane": b, "data": PNG_B64, "text": "look" })).unwrap();
    d.wait(b, "idle");
    let said = entries(&d.state(b)).into_iter().rev().find(|e| e["type"] == "agent").unwrap();
    assert_eq!(said["text"], "Saw 1 image(s) ['image/png']; text []");

    // Into a shell it started, by its path here.
    let shot = d.sessions.join("shot.png");
    std::fs::write(&shot, png()).unwrap();
    let sh = agent_mcp(&d, a, "run", json!({ "command": "true", "wait": true })).unwrap()["pane"].as_u64().unwrap();
    let r = agent_mcp(&d, a, "attach", json!({ "pane": sh, "path": shot })).unwrap();
    let pasted = r["path"].as_str().unwrap().to_owned();
    assert!(pasted.ends_with(".png") && pasted.contains("illogical-uploads"), "{r}");
    assert_eq!(std::fs::read(&pasted).unwrap(), png());
    d.wait_for("the path on its screen", || {
        let screen = agent_mcp(&d, a, "capture_screen", json!({ "pane": sh })).unwrap();
        screen["text"].as_str().unwrap_or("").replace('\n', "").contains(&pasted)
    });

    // Not into what wouldn't read a path, nor what it didn't start.
    let busy = agent_mcp(&d, a, "run", json!({ "command": "sleep 60" })).unwrap()["pane"].as_u64().unwrap();
    d.wait_for("sleep in front", || {
        agent_mcp(&d, a, "attach", json!({ "pane": busy, "path": shot })).is_err_and(|e| e.contains("sleep"))
    });
    let e = agent_mcp(&d, a, "attach", json!({ "pane": a, "data": PNG_B64 })).unwrap_err();
    assert!(e.contains("wasn't started by this agent"), "{e}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test(flavor = "multi_thread")]
async fn what_failed_here_yesterday() {
    let d = Daemon::child();
    let s = bridge(&d, Client::named("claude-code")).await;
    let repo = d.sessions.join("repo");
    let elsewhere = d.sessions.join("elsewhere");
    for dir in [&repo, &elsewhere] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut yesterday = vec![];
    for (cmd, dir) in [("cargo-test-m16 || false", &repo), ("true", &repo), ("false", &elsewhere)] {
        yesterday
            .push(call(&s, "run", json!({ "command": cmd, "cwd": dir, "wait": true })).await["pane"].as_u64().unwrap());
    }
    // ...made yesterday's: their history, a day and a bit back.
    let re = regex::Regex::new(r#""at_ms":(\d+)"#).unwrap();
    for p in &yesterday {
        let index = d.state.join(format!("blocks/{p}/index"));
        let text = std::fs::read_to_string(&index).unwrap();
        let shifted = re.replace_all(&text, |c: &regex::Captures| {
            format!("\"at_ms\":{}", c[1].parse::<u64>().unwrap() - 30 * 3600 * 1000)
        });
        std::fs::write(&index, shifted.as_bytes()).unwrap();
    }
    // And one today.
    call(&s, "run", json!({ "command": "false # today", "cwd": repo, "wait": true })).await;

    let h = call(&s, "history", json!({ "failed": true, "cwd": repo, "since": "2d", "before": "1d" })).await;
    let cmds: Vec<&str> = h["commands"].as_array().unwrap().iter().map(|c| c["command"].as_str().unwrap()).collect();
    assert_eq!(cmds, ["cargo-test-m16 || false"], "{h}");
    assert!(h["summary"].as_str().unwrap().starts_with("1 failed command"), "{h}");
    s.cancel().await.unwrap();
}

/// #59: an agent block in a wisp VM gets illogical through the relay the
/// daemon opens into its VM (a guest can't reach the host): scoped to its
/// tab like a local one, its `run` on its own machine, across a daemon
/// restart. Skips without a wisp token on this host.
#[test]
fn an_agent_block_in_a_vm_gets_mcp_through_the_relay() {
    let token = std::env::var_os("ILLOGICAL_WISP_TOKEN_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/wisp/token"));
    if !token.exists() {
        eprintln!("SKIP: no wisp token on this host (ILLOGICAL_WISP_TOKEN_FILE or ~/.local/share/wisp/token)");
        return;
    }
    let mut d = Daemon::child_with(&["--wisp-token-file", token.to_str().unwrap()]);
    let other = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    // fake_acp.py itself, run in the guest.
    let src = std::fs::read_to_string(fake()).unwrap();
    let config = json!({ "agent": "acp", "command": ["python3", "-c", src], "prompt": "hello" });
    let a = d.open_with(json!({ "type": "agent", "vm": true, "config": config }));
    d.wait_secs(a, "idle", 300);

    // Its tab, through the relay.
    let l = agent_mcp(&d, a, "list", json!({})).unwrap();
    let seen: Vec<u64> = l["panes"].as_array().unwrap().iter().map(|p| p["pane"].as_u64().unwrap()).collect();
    assert!(seen.contains(&a) && !seen.contains(&other), "{l}");
    // Its run lands on its own machine, beside it (which makes the machine
    // its tab's).
    let sock = format!("/tmp/illogical-mcp-{a}.sock");
    let r = agent_mcp(&d, a, "run", json!({ "command": format!("test -S {sock} && echo IN-ITS-VM"), "wait": true }))
        .unwrap();
    assert_eq!(r["exit"], 0, "{r}");
    assert!(r.to_string().contains("IN-ITS-VM"), "{r}");
    let pane = r["pane"].as_u64().unwrap();
    assert_eq!(tab_of(&d, pane), tab_of(&d, a));
    let m = d.get("/api/machines");
    assert_eq!(m.as_array().unwrap().len(), 1, "{m}");
    assert_eq!(m[0]["owner"], json!({ "tab": tab_of(&d, a) }), "{m}");
    let h = d.get("/api/history?limit=20");
    assert!(h.as_array().unwrap().iter().any(|c| c["by"] == "mcp:fake-agent"), "{h}");
    // Other tabs are refused, as for a local agent.
    let e = agent_mcp(&d, a, "send_input", json!({ "pane": other, "text": "rm -rf ~" })).unwrap_err();
    assert!(e.contains("own tab") || e.contains("another tab"), "{e}");

    // A restarted daemon opens a new relay; the agent's next client gets
    // through it.
    d.stop();
    d.start();
    d.wait_secs(a, "idle", 120);
    let l = agent_mcp(&d, a, "list", json!({})).unwrap();
    assert!(l["panes"].as_array().unwrap().iter().any(|p| p["pane"] == pane), "{l}");

    // Closed with its tab, the machine (and the relay with it) goes.
    d.post(&format!("/api/panes/{a}/close"), json!({}));
    d.post(&format!("/api/panes/{pane}/close"), json!({}));
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !d.get("/api/machines").as_array().unwrap().is_empty() {
        assert!(std::time::Instant::now() < deadline, "its machine should go");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// #147: prompt_agent, one call per turn, no sleeps here: Claude Code
/// (recorded, replayed) in a terminal pane, and an agent block.
#[tokio::test(flavor = "multi_thread")]
async fn prompt_agent_waits_for_the_turn() {
    let d = Daemon::child();
    let s = bridge(&d, Client::named("claude-code")).await;
    let scratch = Scratch::new("mcp-prompt");
    let r = replay::Replay::install(&scratch.join("bin"), "claude", "claude_turn");
    d.post("/api/panes/1/send", json!({"text": r.path(), "enter": true}));
    let wait = |line: &'static str, n: usize| {
        let r = &r;
        tokio::task::block_in_place(move || r.reached(line, n))
    };
    wait("m blocked", 1);
    d.post("/api/panes/1/keys", json!({"keys": ["Down", "Enter"]}));
    wait("m idle", 1);
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while d.get("/api/panes/1/detection")["shown"] != "idle" {
        let why = (d.get("/api/panes/1/detection"), d.get("/api/panes/1/capture"), r.log());
        assert!(std::time::Instant::now() < deadline, "its screen isn't read as idle: {why:?}");
        std::thread::sleep(Duration::from_millis(50));
    }

    let v = call(
        &s,
        "prompt_agent",
        json!({ "pane": 1, "text": "Run this shell command: sleep 4 && touch made-by-claude.txt" }),
    )
    .await;
    assert_eq!(v["result"], "needs_input", "{v}");
    assert_eq!(v["question"], "Claude Code asks to run `sleep 4 && touch made-by-claude.txt`", "{v}");
    // Waiting on that: nothing typed, the question back.
    let v = call(&s, "prompt_agent", json!({ "pane": 1, "text": "never mind" })).await;
    assert_eq!(v["result"], "blocked", "{v}");
    assert!(v["summary"].as_str().unwrap().contains("nothing was typed"), "{v}");
    // Answered (Enter), and through to the end of the turn.
    let v = call(&s, "prompt_agent", json!({ "pane": 1, "text": "", "answering": true })).await;
    assert_eq!(v["result"], "done", "{v}");
    let screen = d.get("/api/panes/1/capture").as_str().unwrap_or_default().to_owned();
    assert!(screen.contains("has been created") && screen.contains("? for shortcuts"), "{screen}");

    // An agent block: its question comes back with the call.
    let b = d.open("hello");
    d.wait(b, "idle");
    let v = call(&s, "prompt_agent", json!({ "pane": b, "text": "ask one" })).await;
    assert_eq!(v["result"], "needs_input", "{v}");
    assert_eq!(v["ask"]["questions"][0]["question"], "Which colour do you prefer?", "{v}");
    let v = call(&s, "prompt_agent", json!({ "pane": b, "text": "recall" })).await;
    assert_eq!(v["result"], "blocked", "{v}");
}

/// M61: an agent reads the people's thread about a pane and answers in it.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_reads_and_posts_in_threads() {
    let d = Daemon::child();
    let panes = d.get("/api/panes");
    let (pane, session) = (panes[0]["id"].as_u64().unwrap(), panes[0]["session"].as_u64().unwrap());
    d.post(&format!("/api/threads/pane-{pane}"), json!({ "text": "why does the build fail?" }));
    let s = bridge(&d, Client::named("claude-code")).await;
    let r = call(&s, "read_thread", json!({ "pane": format!("%{pane}") })).await;
    assert_eq!(r["messages"][0]["text"], "why does the build fail?");
    assert!(r["summary"].as_str().unwrap().contains("why does the build fail?"), "{r}");
    let p = call(&s, "post_thread", json!({ "pane": pane, "text": "a missing env var: FOO" })).await;
    assert_eq!(
        (p["message"]["agent"].as_bool(), p["message"]["name"].as_str()),
        (Some(true), Some("claude-code (agent)"))
    );
    let r = call(&s, "read_thread", json!({ "pane": pane, "after": 1 })).await;
    assert_eq!(r["messages"].as_array().unwrap().len(), 1, "{r}");
    call(&s, "post_thread", json!({ "session": session, "text": "done for today" })).await;
    assert_eq!(d.get(&format!("/api/threads/session-{session}"))["messages"][0]["text"], "done for today");
    // An @ that goes nowhere comes back to the agent that wrote it.
    let p = call(&s, "post_thread", json!({ "session": session, "text": "@claude @notreal ping" })).await;
    assert_eq!(
        p["unreached"],
        json!([{ "token": "claude", "why": "agent_needs_pane" }, { "token": "notreal", "why": "nobody" }])
    );
    // A client with no pane of its own has to say which thread.
    assert!(refused(&s, "post_thread", json!({ "text": "hi" })).await.contains("which thread"));
    s.cancel().await.unwrap();
}

/// M61: an agent block's own pane is its thread unless it names another.
#[test]
fn an_agent_blocks_thread_is_its_own() {
    let d = Daemon::child_with(&["--wisp-token-file", "/nonexistent", "--block-listen", "127.0.0.1:0"]);
    let other = d.post("/api/run", json!({}))["pane"].as_u64().unwrap();
    let a = d.open("hello");
    d.wait(a, "idle");
    agent_mcp(&d, a, "post_thread", json!({ "text": "starting on it" })).unwrap();
    assert_eq!(d.get(&format!("/api/threads/pane-{a}"))["messages"][0]["text"], "starting on it");
    // Another tab's pane isn't its to post in.
    let e = agent_mcp(&d, a, "post_thread", json!({ "pane": other, "text": "hi" })).unwrap_err();
    assert!(e.contains("another tab"), "{e}");
}

// ---------------------------------------------------------------- #234

const OWNER: &str = "me@example.com";
const FRIEND: &str = "friend@example.com";

/// A daemon on a tailnet (so tests can be someone else), its first pane
/// and session, and FRIEND an editor there.
fn shared_daemon(env: &[(&str, &str)]) -> (Daemon, u64, u64) {
    let args = ["--owner", OWNER, "--tailscale-socket", "/nonexistent/sock", "--wisp-token-file", "/nonexistent"];
    let d = Daemon::child_env(&args, env);
    let p = &d.get("/api/panes")[0];
    let (pane, session) = (p["id"].as_u64().unwrap(), p["session"].as_u64().unwrap());
    d.post("/api/acl", json!({ "session": session, "principal": format!("tailnet:{FRIEND}"), "role": "editor" }));
    (d, pane, session)
}

/// #297: an agent's @name of someone who can't see the thread makes no
/// invite card and no grant; post_thread points it at invite_person.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_mention_invites_nobody() {
    let (d, pane, session) = shared_daemon(&[]);
    let elsewhere = d.post("/api/run", json!({ "session": "other" }))["pane"].as_u64().unwrap();
    let other =
        d.get("/api/panes").as_array().unwrap().iter().find(|p| p["id"] == elsewhere).unwrap()["session"].clone();
    d.post("/api/acl", json!({ "session": other, "principal": "tailnet:sam@example.com", "role": "viewer" }));
    let s = bridge(&d, Client::named("claude-code")).await;
    let tools = s.list_tools(None).await.unwrap();
    let post = tools.tools.iter().find(|t| t.name == "post_thread").unwrap();
    assert!(post.description.as_deref().unwrap_or("").contains("invite_person"), "{post:?}");
    let p = call(&s, "post_thread", json!({ "pane": pane, "text": "@sam look" })).await;
    assert_eq!(p["unreached"], json!([{ "token": "sam", "why": "nobody" }]), "{p}");
    assert!(p.get("invitable").is_none(), "{p}");
    s.cancel().await.unwrap();
    assert!(invite_blocks(&d).is_empty());
    let grants = d.get("/api/acl")["grants"].clone();
    assert!(
        grants
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["principal"] != "tailnet:sam@example.com" || g["session"] != session),
        "{grants}"
    );
}

/// A POST on the socket as the CLI sends it under Claude Code: the HTTP
/// status.
fn as_agent(d: &Daemon, path: &str, body: Value) -> u16 {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(d.sock()).unwrap();
    let body = body.to_string();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nX-Illogical-Agent: 1\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn invite_blocks(d: &Daemon) -> Vec<u64> {
    let panes = d.get("/api/panes");
    panes.as_array().unwrap().iter().filter(|p| p["type"] == "invite").map(|p| p["id"].as_u64().unwrap()).collect()
}

fn grant_of(d: &Daemon, principal: &str) -> Option<Value> {
    d.get("/api/acl")["grants"].as_array().unwrap().iter().find(|g| g["principal"] == principal).cloned()
}

/// The card on a block (or pane), if one is open.
fn card_on(d: &Daemon, id: u64) -> Value {
    let panes = d.get("/api/panes");
    panes.as_array().unwrap().iter().find(|p| p["id"] == id).map(|p| p["ask"].clone()).unwrap_or(Value::Null)
}

/// `read_invite` until it isn't waiting.
fn settled(m: &Mcp, draft: &str, pane: Option<u64>) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let r = m.call("read_invite", json!({ "draft": draft, "pane": pane })).unwrap();
        if r["status"] != "waiting" {
            return r;
        }
        assert!(std::time::Instant::now() < deadline, "still waiting: {r}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn an_invite_waits_for_the_owner_and_only_they_send_it() {
    let (d, pane, _) = shared_daemon(&[]);
    // A full client outside any pane (no $ILLOGICAL_PANE).
    let m = Mcp::bridge(&d, None);
    let note = "the flaky test needs their eyes";

    // Where is unsaid; whom nobody knows; never an owner: no card.
    let e = m.call("invite_person", json!({ "who": "tailnet:sam@example.com", "note": note })).unwrap_err();
    assert!(e.contains("pane:"), "{e}");
    let e = m.call("invite_person", json!({ "who": "sam", "pane": pane, "note": note })).unwrap_err();
    assert!(e.contains("share once from the web"), "{e}");
    let e = m.call("invite_person", json!({ "who": FRIEND, "pane": pane, "role": "owner", "note": note })).unwrap_err();
    assert!(e.contains("never makes an owner"), "{e}");
    let e = m.call("invite_person", json!({ "who": FRIEND, "pane": pane, "note": " " })).unwrap_err();
    assert!(e.contains("note"), "{e}");
    assert!(invite_blocks(&d).is_empty(), "no card for any of that");

    // A draft, at once; nothing shared.
    let sam = "tailnet:sam@example.com";
    let r = m.call("invite_person", json!({ "who": sam, "pane": pane, "role": "editor", "note": note })).unwrap();
    assert_eq!(r["status"], "waiting", "{r}");
    let draft = r["draft"].as_str().unwrap().to_owned();
    let block = r["block"].as_u64().unwrap();
    assert!(grant_of(&d, sam).is_none());
    assert_eq!(tab_of(&d, block), tab_of(&d, pane), "beside the pane");
    let card = card_on(&d, block);
    assert_eq!(card["source"], "invite", "{card}");
    let name = d.get("/api/panes")[0]["session_name"].as_str().unwrap().to_owned();
    assert_eq!(
        card["message"],
        format!(
            "claude-code (pane %{pane}, your own client) wants to bring sam@example.com [{sam}] (editor) into {name} at pane %{pane}: {note}"
        )
    );
    assert!(card_on(&d, pane).is_null(), "never on the pane itself");
    assert_eq!(m.call("read_invite", json!({ "draft": draft, "pane": pane })).unwrap()["status"], "waiting");

    // An editor may answer other cards, not this one: by the card, or the
    // push's action. Nor an agent, through agent_respond.
    let body = json!({ "id": draft, "content": { "role": "editor" } });
    let (status, text) = d.raw_as(FRIEND, "POST", &format!("/api/blocks/{block}/call/answer"), Some(body));
    assert_eq!(status, 403, "{text}");
    let act = json!({ "action": "answer", "pane": block, "content": { "role": "editor" } });
    let (status, text) = d.raw_as(FRIEND, "POST", "/api/attention/act", Some(act));
    assert_eq!(status, 403, "{text}");
    let (status, _) = d.raw_as(FRIEND, "POST", "/api/attention/act", Some(json!({ "action": "deny", "pane": block })));
    assert_eq!(status, 403);
    let e = m.call("agent_respond", json!({ "pane": block, "action": "answer", "answers": {} })).unwrap_err();
    assert!(e.contains("only the session's owner"), "{e}");
    // Nor Claude Code on the owner's own CLI (it says so).
    assert_eq!(as_agent(&d, &format!("/api/blocks/{block}/call/answer"), json!({ "content": {} })), 403);
    assert_eq!(as_agent(&d, "/api/attention/act", json!({ "action": "answer", "pane": block, "content": {} })), 403);
    // Nor may it skip the card: invite on the CLI, or make a card of its
    // own (whose words needn't be what it does), or pin a team.
    let session = d.get("/api/panes")[0]["session"].clone();
    let direct = json!({ "session": session, "who": sam, "role": "editor", "drive_minutes": 60 });
    assert_eq!(as_agent(&d, "/api/invite", direct), 403);
    assert_eq!(as_agent(&d, "/api/team-pins", json!({ "pins": {} })), 403);
    let forged = json!({ "type": "invite", "config": { "drafter": "%1", "drafts": [] } });
    let (status, text) = d.raw("POST", "/api/blocks", Some(forged));
    assert_eq!(status, 403, "{text}");
    assert_eq!(invite_blocks(&d), [block], "no other invite block");
    assert!(grant_of(&d, sam).is_none(), "nothing granted");
    assert_eq!(card_on(&d, block)["id"], draft.as_str(), "the card waits still");

    // The owner sends it: #233's invite, as them.
    let content = json!({ "role": "editor", "note": "edited: the flaky test" });
    let (status, text) =
        d.raw("POST", &format!("/api/blocks/{block}/call/answer"), Some(json!({ "content": content })));
    assert_eq!(status, 200, "{text}");
    let r = settled(&m, &draft, Some(pane));
    assert_eq!(r["status"], "sent", "{r}");
    assert_eq!((r["delivery"].as_str(), r["grant"]["principal"].as_str()), (Some("unreachable"), Some(sam)), "{r}");
    assert!(r["delivery_reason"].is_string() && r["settled_by"].is_string(), "{r}");
    assert_eq!(r["note"], "edited: the flaky test");
    assert_eq!(grant_of(&d, sam).unwrap()["role"], "editor");
    let audit = d.get("/api/acl")["audit"].clone();
    let line = audit.as_array().unwrap().iter().find(|a| a["action"] == "invite").cloned().unwrap();
    assert_eq!(
        (line["by"].as_str(), line["approved_by"].as_str(), line["drafted_by"].as_str(), line["drafted_in"].as_u64()),
        (Some("owner"), r["settled_by"].as_str(), Some("mcp:claude-code"), Some(pane)),
        "{line}"
    );

    // Declined, with a reason the agent reads; and without one.
    let kim = "tailnet:kim@example.com";
    let r = m.call("invite_person", json!({ "who": kim, "pane": pane, "note": note })).unwrap();
    let draft = r["draft"].as_str().unwrap().to_owned();
    assert_eq!(r["block"].as_u64(), Some(block), "the same block for the same caller");
    d.wait_for("its card", || card_on(&d, block)["id"] == draft.as_str());
    let content = json!({ "decline": true, "reason": "not this week" });
    d.post(&format!("/api/blocks/{block}/call/answer"), json!({ "content": content }));
    let r = settled(&m, &draft, Some(pane));
    assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some("declined"), Some("not this week")), "{r}");
    assert!(r["summary"].as_str().unwrap().contains("not this week"));
    let r = m.call("invite_person", json!({ "who": kim, "pane": pane, "note": note })).unwrap();
    let draft = r["draft"].as_str().unwrap().to_owned();
    d.wait_for("its card", || card_on(&d, block)["id"] == draft.as_str());
    d.post(&format!("/api/blocks/{block}/call/decline"), json!({}));
    let r = settled(&m, &draft, Some(pane));
    assert_eq!((r["status"].as_str(), r["reason"].as_str()), (Some("declined"), None), "{r}");
    assert!(grant_of(&d, kim).is_none());

    // Five may wait; the sixth is refused.
    for _ in 0..5 {
        m.call("invite_person", json!({ "who": kim, "pane": pane, "note": note })).unwrap();
    }
    let e = m.call("invite_person", json!({ "who": kim, "pane": pane, "note": note })).unwrap_err();
    assert!(e.contains("wait for the user already"), "{e}");
    // Another caller reads none of them.
    let other = Mcp::bridge(&d, Some(block));
    let e = other.call("read_invite", json!({ "draft": draft })).unwrap_err();
    assert!(e.contains("no invite"), "{e}");
}

#[test]
fn an_unanswered_invite_is_dropped() {
    let (d, pane, _) = shared_daemon(&[("ILLOGICAL_INVITE_TTL_MS", "1500")]);
    let m = Mcp::bridge(&d, Some(pane));
    let r = m.call("invite_person", json!({ "who": "tailnet:sam@example.com", "note": "x" })).unwrap();
    assert_eq!(r["pane"].as_u64(), Some(pane), "the pane illogical mcp runs in, by default");
    let block = r["block"].as_u64().unwrap();
    let r = settled(&m, r["draft"].as_str().unwrap(), None);
    assert_eq!(r["status"], "dropped", "{r}");
    assert!(card_on(&d, block).is_null(), "its card went with it");
    assert!(grant_of(&d, "tailnet:sam@example.com").is_none());
}

#[test]
fn an_invite_card_leaves_the_panes_own_cards_alone() {
    let d = Daemon::child();
    let pane = d.get("/api/panes")[0]["id"].as_u64().unwrap();
    // Claude Code in that terminal, asking a question through its hook.
    let question = json!({ "questions": [{ "question": "Which colour?", "header": "Colour",
        "options": [{ "label": "Blue" }, { "label": "Red" }], "multiSelect": false }] });
    std::thread::scope(|s| {
        let asked = s.spawn(|| d.raw("POST", &format!("/api/panes/{pane}/ask"), Some(question)));
        d.wait_for("its question", || card_on(&d, pane)["kind"] == "questions");
        let q = card_on(&d, pane)["id"].clone();
        let m = Mcp::bridge(&d, Some(pane));
        let r = m.call("invite_person", json!({ "who": "tailnet:sam@example.com", "note": "x" })).unwrap();
        let block = r["block"].as_u64().unwrap();
        assert_eq!(card_on(&d, pane)["id"], q, "its own card stays");
        // Settling the invite doesn't answer it...
        d.post(&format!("/api/blocks/{block}/call/decline"), json!({}));
        settled(&m, r["draft"].as_str().unwrap(), None);
        assert_eq!(card_on(&d, pane)["id"], q);
        // ...nor answering it the invite.
        let r = m.call("invite_person", json!({ "who": "tailnet:sam@example.com", "note": "y" })).unwrap();
        let draft = r["draft"].as_str().unwrap().to_owned();
        d.wait_for("the invite card", || card_on(&d, block)["id"] == draft.as_str());
        let r = d.post(
            "/api/attention/act",
            json!({ "action": "answer", "pane": pane, "content": { "question_0": "Blue" } }),
        );
        assert_eq!(r["results"][0]["ok"], true, "{r}");
        let (status, text) = asked.join().unwrap();
        assert_eq!(status, 200, "{text}");
        assert!(text.contains("Blue"), "{text}");
        assert_eq!(card_on(&d, block)["id"], draft.as_str(), "the invite still waits");

        // A permission card, the same.
        let permit = json!({ "tool_name": "Bash", "tool_input": { "command": "ls" }, "session_id": "s" });
        let permitted = s.spawn(|| d.raw("POST", &format!("/api/panes/{pane}/permit"), Some(permit)));
        d.wait_for("its permission card", || card_on(&d, pane)["kind"] == "permission");
        assert_eq!(card_on(&d, block)["id"], draft.as_str());
        d.post(&format!("/api/blocks/{block}/call/answer"), json!({ "content": {} }));
        assert_eq!(settled(&m, &draft, None)["status"], "sent");
        assert_eq!(card_on(&d, pane)["kind"], "permission", "sending the invite left it");
        d.post("/api/attention/act", json!({ "action": "allow", "pane": pane }));
        let (status, text) = permitted.join().unwrap();
        assert_eq!(status, 200, "{text}");
        assert!(text.contains("allow"), "{text}");
        // Nothing raises anything on the invite block but itself (a card
        // that got there would wait for an answer: give it a few seconds).
        let mut c = std::os::unix::net::UnixStream::connect(d.sock()).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let body = json!({ "questions": [{ "question": "?" }] }).to_string();
        use std::io::{Read, Write};
        write!(
            c,
            "POST /api/panes/{block}/ask HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut head = [0u8; 12];
        c.read_exact(&mut head).expect("a question went up on the invite block");
        assert_eq!(&head[9..12], b"403");
    });
}

#[test]
fn an_agent_block_drafts_beside_itself() {
    let d = Daemon::child();
    let a = d.open("hello");
    d.wait(a, "idle");
    let r =
        agent_mcp(&d, a, "invite_person", json!({ "who": "tailnet:sam@example.com", "note": "pair on it" })).unwrap();
    assert_eq!((r["status"].as_str(), r["pane"].as_u64()), (Some("waiting"), Some(a)), "{r}");
    let block = r["block"].as_u64().unwrap();
    assert_eq!(tab_of(&d, block), tab_of(&d, a));
    assert!(grant_of(&d, "tailnet:sam@example.com").is_none());
    let card = card_on(&d, block);
    // Whose agent it is, and whom exactly, by their principal.
    let want =
        format!("fake-agent (pane %{a}, you started it) wants to bring sam@example.com [tailnet:sam@example.com]");
    assert!(card["message"].as_str().unwrap().starts_with(&want), "{card}");
    let draft = r["draft"].as_str().unwrap().to_owned();
    let r = agent_mcp(&d, a, "read_invite", json!({ "draft": draft })).unwrap();
    assert_eq!(r["status"], "waiting", "{r}");
    // It may not close or answer its own card.
    let e = agent_mcp(&d, a, "close", json!({ "pane": block })).unwrap_err();
    assert!(e.contains("wasn't started by this agent"), "{e}");
    d.post(&format!("/api/blocks/{block}/call/answer"), json!({ "content": {} }));
    d.wait_for("sent", || {
        agent_mcp(&d, a, "read_invite", json!({ "draft": draft })).is_ok_and(|r| r["status"] == "sent")
    });
    assert!(grant_of(&d, "tailnet:sam@example.com").is_some());
}

/// An intent over the WebSocket as FRIEND: the error it got, if any.
fn intent_as_friend(d: &Daemon, intent: Value) -> Option<String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let mut req = format!("ws://127.0.0.1:{}/ws", d.port).into_client_request().unwrap();
        req.headers_mut().insert("Tailscale-User-Login", FRIEND.parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.expect("FRIEND connects");
        let msg = json!({ "type": "intent", "id": 7, "intent": intent });
        ws.send(Message::Text(msg.to_string().into())).await.unwrap();
        let wait = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(Ok(m)) = ws.next().await {
                if let Message::Text(t) = m
                    && let Ok(v) = serde_json::from_str::<Value>(&t)
                    && v["type"] == "error"
                    && v["id"] == 7
                {
                    return v["message"].as_str().map(str::to_owned);
                }
            }
            None
        });
        wait.await.unwrap_or(None)
    })
}

#[test]
fn closing_an_invite_block_is_the_owners_and_loses_nothing() {
    let (d, pane, _) = shared_daemon(&[]);
    let m = Mcp::bridge(&d, Some(pane));
    let invite = |who: &str| m.call("invite_person", json!({ "who": who, "note": "x" })).unwrap();
    let sent = invite("tailnet:sam@example.com");
    let block = sent["block"].as_u64().unwrap();
    let sent = sent["draft"].as_str().unwrap().to_owned();
    d.post(&format!("/api/blocks/{block}/call/answer"), json!({ "content": {} }));
    assert_eq!(settled(&m, &sent, None)["status"], "sent");
    let waiting = invite("tailnet:kim@example.com")["draft"].as_str().unwrap().to_owned();
    d.wait_for("its card", || card_on(&d, block)["id"] == waiting.as_str());

    // An editor can't close it: by the API, the pane or its tab over the
    // WebSocket. Nor an agent, through MCP's close.
    let (status, text) = d.raw_as(FRIEND, "POST", &format!("/api/panes/{block}/close"), Some(json!({})));
    assert_eq!(status, 403, "{text}");
    let why = intent_as_friend(&d, json!({ "op": "close_pane", "pane": block }));
    assert!(why.as_deref().is_some_and(|w| w.contains("only the session's owner closes")), "{why:?}");
    let why = intent_as_friend(&d, json!({ "op": "close_tab", "tab": tab_of(&d, block) }));
    assert!(why.as_deref().is_some_and(|w| w.contains("only the session's owner closes")), "{why:?}");
    let e = m.call("close", json!({ "pane": block })).unwrap_err();
    assert!(e.contains("only the session's owner closes"), "{e}");
    assert_eq!(as_agent(&d, &format!("/api/panes/{block}/close"), json!({})), 403);
    assert!(invite_blocks(&d).contains(&block), "still open");
    assert_eq!(m.call("read_invite", json!({ "draft": waiting })).unwrap()["status"], "waiting");

    // The owner closes it: what waited is dropped, and said so; what was
    // sent still says so.
    d.post(&format!("/api/panes/{block}/close"), json!({}));
    d.wait_for("it to close", || !invite_blocks(&d).contains(&block));
    let r = m.call("read_invite", json!({ "draft": waiting })).unwrap();
    assert_eq!(r["status"], "dropped", "{r}");
    assert!(r["reason"].as_str().unwrap().contains("closed"), "{r}");
    assert_eq!(m.call("read_invite", json!({ "draft": sent })).unwrap()["status"], "sent");
    assert!(grant_of(&d, "tailnet:kim@example.com").is_none());
}
