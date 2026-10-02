//! S14: a throwaway rmcp server to see what rmcp 3.5 and real clients do.
//!
//!   s14-mcp stdio                 serve on stdin/stdout
//!   s14-mcp http ADDR             Streamable HTTP at http://ADDR/mcp
//!   s14-mcp unix SOCKET           Streamable HTTP served on a Unix socket
//!   s14-mcp bridge SOCKET         rmcp client over a Unix socket: list tools, call one
//!   s14-mcp listen URL            2026-07-28 client: subscriptions/listen, bump, await the update
//!
//! Hosts allowed for HTTP: $S14_HOSTS (comma-separated), default localhost,
//! 127.0.0.1 and wisp's bridge address. S14_CACHE_HINTS=0 drops ttlMs and
//! cacheScope from resource results.
//!
//! Everything the server sees (initialize params, tool calls, progress tokens,
//! subscribe calls, raw HTTP requests) is appended as JSON lines to $S14_LOG
//! (default ./s14.log).

use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rmcp::{
    ErrorData as McpError, Json, Peer, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CacheScope, CallToolResult, ContentBlock, Implementation, InitializeRequestParams, InitializeResult,
        ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
        ProgressNotificationParam, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, RequestMetaObject, Resource, ResourceContents,
        ResourceTemplate, ResourceUpdatedNotificationParam, ServerCapabilities, ServerConfig,
        SubscribeRequestParams, SubscriptionFilter, UnsubscribeRequestParams,
    },
    service::{RequestContext, SubscriptionContext},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// S14_CACHE_HINTS=0 leaves ttlMs/cacheScope unset (rmcp's default), which
/// Claude Code 2.1.287 rejects on resources/list and resources/read.
fn cache_hints() -> bool {
    std::env::var("S14_CACHE_HINTS").map(|v| v != "0").unwrap_or(true)
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

fn log(event: &str, data: serde_json::Value) {
    let path = std::env::var("S14_LOG").unwrap_or_else(|_| "s14.log".into());
    let line = json!({"t": now(), "pid": std::process::id(), "event": event, "data": data});
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

#[derive(Clone)]
struct S14 {
    tool_router: ToolRouter<S14>,
    counter: Arc<Mutex<u64>>,
    // legacy resources/subscribe subscribers
    subs: Arc<Mutex<Vec<(Peer<RoleServer>, String)>>>,
    // 2026-07-28 subscriptions/listen sinks get a broadcast
    bump_tx: tokio::sync::broadcast::Sender<String>,
}

#[derive(Deserialize, JsonSchema)]
struct SleepArgs {
    /// seconds to sleep
    secs: u64,
    /// send a progress notification every N seconds (0 = never)
    #[serde(default)]
    progress_every: u64,
}

#[derive(Deserialize, JsonSchema)]
struct BigArgs {
    /// how many characters of text to return
    chars: usize,
    /// also return the text in structuredContent
    #[serde(default)]
    structured: bool,
}

#[derive(Serialize, JsonSchema)]
struct PaneSummary {
    pane: String,
    exit_code: Option<i32>,
    last_lines: Vec<String>,
    next_offset: u64,
}

fn big_text(chars: usize) -> String {
    // numbered 100-char lines so truncation points are visible
    let mut s = String::with_capacity(chars + 100);
    let mut n = 0;
    while s.len() < chars {
        let line = format!("line {n:06} ");
        s.push_str(&line);
        s.push_str(&"x".repeat(99 - line.len()));
        s.push('\n');
        n += 1;
    }
    s.truncate(chars);
    s
}

#[tool_router]
impl S14 {
    fn new() -> Self {
        let (bump_tx, _) = tokio::sync::broadcast::channel(16);
        Self {
            tool_router: Self::tool_router(),
            counter: Arc::new(Mutex::new(0)),
            subs: Arc::new(Mutex::new(Vec::new())),
            bump_tx,
        }
    }

    /// Sleep for `secs` seconds, optionally sending progress notifications.
    /// Returns how long it slept.
    #[tool(annotations(read_only_hint = true, open_world_hint = false))]
    async fn sleep(
        &self,
        Parameters(args): Parameters<SleepArgs>,
        meta: RequestMetaObject,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let token = meta.get_progress_token();
        log("sleep.start", json!({"secs": args.secs, "progress_every": args.progress_every, "progress_token": token, "meta": meta}));
        let start = Instant::now();
        let end = start + Duration::from_secs(args.secs);
        let mut step = 0u64;
        while Instant::now() < end {
            let tick = if args.progress_every > 0 { Duration::from_secs(args.progress_every) } else { end - Instant::now() };
            tokio::time::sleep(tick.min(end.saturating_duration_since(Instant::now()))).await;
            if args.progress_every > 0 {
                step += 1;
                if let Some(t) = &token {
                    let r = peer
                        .notify_progress(
                            ProgressNotificationParam::new(t.clone(), (step * args.progress_every) as f64)
                                .with_total(args.secs as f64)
                                .with_message(format!("slept {}s", step * args.progress_every)),
                        )
                        .await;
                    log("sleep.progress", json!({"step": step, "ok": r.is_ok(), "err": r.err().map(|e| e.to_string())}));
                }
            }
        }
        let took = start.elapsed().as_secs_f64();
        log("sleep.end", json!({"took": took}));
        Ok(CallToolResult::success(vec![ContentBlock::text(format!("slept {took:.1}s"))]))
    }

    /// Return `chars` characters of numbered lines (for output-limit tests).
    #[tool(annotations(read_only_hint = true))]
    async fn big(&self, Parameters(args): Parameters<BigArgs>) -> CallToolResult {
        log("big", json!({"chars": args.chars, "structured": args.structured}));
        let text = big_text(args.chars);
        let mut r = CallToolResult::success(vec![ContentBlock::text(text.clone())]);
        if args.structured {
            r.structured_content = Some(json!({"text": text, "chars": args.chars}));
        }
        r
    }

    /// Structured-only result via rmcp's Json wrapper (outputSchema is generated).
    #[tool(annotations(read_only_hint = true, idempotent_hint = true))]
    async fn pane_summary(&self) -> Json<PaneSummary> {
        log("pane_summary", json!({}));
        Json(PaneSummary {
            pane: "%7".into(),
            exit_code: Some(2),
            last_lines: vec!["error[E0425]: cannot find value `x`".into(), "build failed".into()],
            next_offset: 4096,
        })
    }

    /// Short text summary plus a separate structuredContent (the shape M16 wants).
    #[tool(annotations(read_only_hint = true))]
    async fn summary_and_structured(&self) -> CallToolResult {
        log("summary_and_structured", json!({}));
        let mut r = CallToolResult::success(vec![ContentBlock::text(
            "pane %7 exited 2; last line: build failed (SUMMARY-TEXT)",
        )]);
        r.structured_content = Some(json!({"pane": "%7", "exit_code": 2, "secret_marker": "STRUCTURED-ONLY-42"}));
        r
    }

    /// Fail the way M16 wants failures to look: a tool result with isError.
    #[tool(annotations(destructive_hint = true, idempotent_hint = false))]
    async fn close_pane(&self) -> CallToolResult {
        log("close_pane", json!({}));
        CallToolResult::error(vec![ContentBlock::text("pane %7 is gone; it exited 2 at 14:03")])
    }

    /// Bump the s14://counter resource and notify subscribers.
    #[tool]
    async fn bump(&self) -> String {
        let v = {
            let mut c = self.counter.lock().unwrap();
            *c += 1;
            *c
        };
        let subs = self.subs.lock().unwrap().clone();
        let mut sent = 0;
        for (peer, uri) in subs {
            if peer
                .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri))
                .await
                .is_ok()
            {
                sent += 1;
            }
        }
        let listeners = self.bump_tx.send("s14://counter".into()).unwrap_or(0);
        log("bump", json!({"value": v, "legacy_sent": sent, "listen_sinks": listeners}));
        format!("counter={v}; notified {sent} legacy subscriber(s), {listeners} listen stream(s)")
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for S14 {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .enable_resources_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new("s14", "0.0.0"))
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        log("initialize", serde_json::to_value(&request).unwrap());
        context.peer.set_peer_info(request.clone());
        let r = self.negotiate_initialize(&request);
        log("initialize.result", json!({"version": r.as_ref().ok().map(|r| r.protocol_version.to_string())}));
        r
    }

    async fn list_resources(
        &self,
        _r: Option<PaginatedRequestParams>,
        _c: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        log("resources/list", json!({}));
        let r = ListResourcesResult::with_all_items(vec![Resource::new("s14://counter", "counter")]);
        Ok(if cache_hints() { r.with_ttl_ms(0).with_cache_scope(CacheScope::Private) } else { r })
    }

    async fn list_resource_templates(
        &self,
        _r: Option<PaginatedRequestParams>,
        _c: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        log("resources/templates/list", json!({}));
        let r = ListResourceTemplatesResult::with_all_items(vec![ResourceTemplate::new(
            "s14://pane/{id}/output",
            "pane output",
        )]);
        Ok(if cache_hints() { r.with_ttl_ms(0).with_cache_scope(CacheScope::Private) } else { r })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _c: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        log("resources/read", json!({"uri": request.uri}));
        let text = if request.uri == "s14://counter" {
            format!("counter={}", self.counter.lock().unwrap())
        } else {
            format!("output of {}", request.uri)
        };
        let r = ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]);
        Ok(if cache_hints() { r.with_ttl_ms(0).with_cache_scope(CacheScope::Private) } else { r }.into())
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        log("resources/subscribe", json!({"uri": request.uri}));
        self.subs.lock().unwrap().push((context.peer.clone(), request.uri));
        Ok(())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _c: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        log("resources/unsubscribe", json!({"uri": request.uri}));
        self.subs.lock().unwrap().retain(|(_, u)| u != &request.uri);
        Ok(())
    }

    fn accepted_subscription_filter(&self, requested: &SubscriptionFilter) -> Option<SubscriptionFilter> {
        log("subscriptions/listen.filter", serde_json::to_value(requested).unwrap_or_default());
        Some(requested.supported_by(&self.get_info().capabilities))
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), McpError> {
        log("subscriptions/listen", json!({}));
        let mut rx = self.bump_tx.subscribe();
        loop {
            tokio::select! {
                _ = context.cancelled() => return Ok(()),
                Ok(uri) = rx.recv() => {
                    let r = context.sink().notify_resource_updated(uri).await;
                    log("listen.sent", json!({"ok": r.is_ok()}));
                }
            }
        }
    }
}

async fn log_http(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 16 << 20).await.unwrap_or_default();
    let headers: serde_json::Map<String, serde_json::Value> = parts
        .headers
        .iter()
        // the spike only ever sees fake tokens; keep a prefix to show it arrived
        .map(|(k, v)| {
            let v = v.to_str().unwrap_or("?");
            let v = if k.as_str() == "authorization" { format!("{}…", &v[..v.len().min(12)]) } else { v.to_owned() };
            (k.to_string(), json!(v))
        })
        .collect();
    let body: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    log("http", json!({"method": parts.method.as_str(), "uri": parts.uri.to_string(), "headers": headers, "body": body}));
    let resp = next.run(axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes))).await;
    log("http.resp", json!({"status": resp.status().as_u16(), "ct": resp.headers().get("content-type").and_then(|v| v.to_str().ok())}));
    resp
}

fn http_router() -> axum::Router {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    };
    // Any Host: the spike is reached as 127.0.0.1, the bridge IP and a Unix socket.
    let hosts: Vec<String> = std::env::var("S14_HOSTS")
        .map(|s| s.split(',').map(str::to_owned).collect())
        .unwrap_or_else(|_| vec!["localhost".into(), "127.0.0.1".into(), "10.209.0.1".into()]);
    let config = StreamableHttpServerConfig::default().with_allowed_hosts(hosts);
    let server = S14::new();
    let service: StreamableHttpService<S14, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(server.clone()), Default::default(), config);
    axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn(log_http))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("stdio") => {
            log("start", json!({"mode": "stdio"}));
            let svc = S14::new().serve(rmcp::transport::stdio()).await?;
            svc.waiting().await?;
        }
        Some("http") => {
            let addr = args.get(2).cloned().unwrap_or_else(|| "127.0.0.1:7914".into());
            log("start", json!({"mode": "http", "addr": addr}));
            let l = tokio::net::TcpListener::bind(&addr).await?;
            eprintln!("listening on http://{addr}/mcp");
            axum::serve(l, http_router()).await?;
        }
        Some("unix") => {
            let path = args.get(2).cloned().expect("socket path");
            let _ = std::fs::remove_file(&path);
            log("start", json!({"mode": "unix", "path": path}));
            let l = tokio::net::UnixListener::bind(&path)?;
            eprintln!("listening on unix:{path} /mcp");
            axum::serve(l, http_router()).await?;
        }
        Some("bridge") => bridge(args.get(2).cloned().expect("socket path")).await?,
        Some("listen") => listen_modern(args.get(2).cloned().expect("http url")).await?,
        _ => eprintln!("usage: s14-mcp stdio | http ADDR | unix SOCKET | bridge SOCKET | listen URL"),
    }
    Ok(())
}

/// The 2026-07-28 path: no initialize, `subscriptions/listen` for a resource,
/// then `bump` and wait for `notifications/resources/updated`.
async fn listen_modern(url: String) -> anyhow::Result<()> {
    use rmcp::{
        ClientLifecycleMode, ClientServiceExt,
        model::{ClientConfig, ProtocolVersion, ServerNotification},
        transport::{
            StreamableHttpClientTransport,
            streamable_http_client::StreamableHttpClientTransportConfig,
        },
    };
    let transport =
        StreamableHttpClientTransport::from_config(StreamableHttpClientTransportConfig::with_uri(url));
    let client = ClientConfig::default()
        .serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
        )
        .await?;
    let mut sub = client
        .listen(SubscriptionFilter::builder().resource_subscription("s14://counter").build())
        .await?;
    println!("listening");
    let r = client.call_tool(rmcp::model::CallToolRequestParams::new("bump")).await?;
    println!("bump: {}", serde_json::to_string(&r.content)?);
    let n = tokio::time::timeout(Duration::from_secs(5), sub.next()).await??;
    match n {
        Some(ServerNotification::ResourceUpdatedNotification(u)) => println!("got update: {}", u.params.uri),
        other => println!("got {other:?}"),
    }
    sub.cancel().await?;
    client.cancel().await?;
    Ok(())
}

/// `illogical mcp` in miniature: an rmcp client on the Unix socket proxied to
/// an rmcp server on stdio. Generic proxying in rmcp means re-dispatching
/// each request; here we only check the unix-socket client transport works.
async fn bridge(socket: String) -> anyhow::Result<()> {
    use rmcp::transport::{
        StreamableHttpClientTransport,
    };
    let transport = StreamableHttpClientTransport::from_unix_socket(socket.as_str(), "http://localhost/mcp");
    let client = ().serve(transport).await?;
    let tools = client.list_all_tools().await?;
    println!("{} tools over unix socket: {:?}", tools.len(), tools.iter().map(|t| t.name.to_string()).collect::<Vec<_>>());
    let r = client
        .call_tool(rmcp::model::CallToolRequestParams::new("summary_and_structured"))
        .await?;
    println!("{}", serde_json::to_string(&r)?);
    client.cancel().await?;
    Ok(())
}
