//! MCP (M16): illogical as tools for any agent. A curated layer over the
//! same machinery the API uses, served by rmcp at `/mcp` as Streamable HTTP
//! (both the stateless 2026-07-28 protocol Claude Code speaks and the older
//! sessions Codex speaks).
//!
//! **Who.** `/mcp` is part of the owner's API, with the same checks: the
//! owner over the Unix socket (`illogical mcp` bridges a stdio client to
//! it), on loopback, or over the tailnet (serve's headers, or `WhoIs` on a
//! direct listener). Anyone else needs a bearer token (`tokens.rs`): a
//! client token from `illogical mcp token`, for clients without a tailnet
//! identity, or an agent block's own, which only reaches the block's tab
//! (see [`Scope`]). Browsers must come from one of our origins, as for the
//! API. rmcp's own Host check is off: the daemon's (`Access::check_host`)
//! already ran on every TCP request, with the tailnet names in it.
//!
//! **Tools** (`tools.rs`) return their structured result with a `summary`
//! sentence in it, and the same JSON as text: Claude Code shows the model
//! only the structured part, Codex both (S14). Output is paged (about 16 KB
//! a page) so a chatty pane can't flood the agent's context, and waits
//! send progress every 15s (over HTTP, Claude Code drops a call that's
//! silent for 60s) and return "still running" by 100s, before interactive
//! Claude Code moves a call into the background.
//!
//! **Resources** are read-only templates (`illogical://pane/N/output`,
//! `…/screen`, `illogical://block/N`, `illogical://history`). No client
//! subscribes to resources (S14), so they aren't subscribable.
//!
//! Every call is logged with the client's name and token, and what it
//! starts says so: the pane shows "started by mcp:<client>", and what the
//! client typed is in history as theirs.

pub mod relay;
pub mod tokens;
mod tools;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use illogical_proto::{BlockType, PaneId};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CacheScope, CallToolRequestParams, CallToolResponse, Implementation, ListResourceTemplatesResult,
        ListResourcesResult, ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse,
        ReadResourceResult, Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use tracing::info;

pub use self::tokens::Tokens;
use self::tokens::{Bearer, TokenScope};
use crate::{mux::Api, server::App};

pub const PATH: &str = "/mcp";
/// What agent blocks call the server.
pub const SERVER_NAME: &str = "illogical";

/// How an agent block reaches MCP: the daemon's loopback `/mcp`, or the
/// CLI's bridge on the Unix socket, with the block's token; in a VM, a
/// relay the daemon opens into it (`relay.rs`).
#[derive(Clone)]
pub struct Link {
    pub url: String,
    pub cli: std::path::PathBuf,
    pub socket: std::path::PathBuf,
    pub tokens: Arc<Tokens>,
    /// Serves a VM agent's connections; set once the server is up.
    pub serve: Arc<std::sync::OnceLock<relay::Serve>>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link").field("url", &self.url).finish_non_exhaustive()
    }
}

/// What a caller may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Everything, like the CLI: the owner, or a full client token. The MCP
    /// client's own tool permissions are the guard, which is why the tools'
    /// annotations are honest.
    Full,
    /// The read-only tools (a `read` client token).
    Read,
    /// An agent block's token: new panes and blocks in the block's own tab
    /// (on the tab's machine, in a VM tab), driving and closing what it
    /// started, and reading the rest of its tab. Nothing in other tabs.
    Block(PaneId),
}

/// Who is calling, as the request's checks found.
#[derive(Debug, Clone)]
pub struct Caller {
    pub scope: Scope,
    /// The token it came with, for the log (`None`: the owner's own).
    pub token: Option<String>,
    /// The caller's own pane, which a tool uses where it's left out: an
    /// agent block's own id, or for the others the pane `illogical mcp` says
    /// it runs in (`x-illogical-pane`). It only fills defaults, and is no credential:
    /// what a caller may reach is its scope's alone.
    pub pane: Option<PaneId>,
}

impl Caller {
    /// A caller with `scope`. An agent block is always its own pane, whatever
    /// `from_header` says; the others take the header's, if there was one.
    fn new(scope: Scope, token: Option<String>, from_header: Option<PaneId>) -> Self {
        let pane = match scope {
            Scope::Block(id) => Some(id),
            Scope::Full | Scope::Read => from_header,
        };
        Caller { scope, token, pane }
    }
}

/// The header `illogical mcp` sends its pane in (`$ILLOGICAL_PANE`).
pub const PANE_HEADER: &str = "x-illogical-pane";

/// `/mcp`, for one of the daemon's routers.
pub fn routes(app: &Arc<App>) -> Router<Arc<App>> {
    let config = StreamableHttpServerConfig::default().disable_allowed_hosts();
    let server = McpServer { app: app.clone(), fallback: None };
    let service: StreamableHttpService<McpServer, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(server.clone()), Default::default(), config);
    Router::new()
        .nest_service(PATH, service)
        .layer(middleware::from_fn_with_state(app.clone(), authenticate))
        .merge(tokens::api_routes())
}

/// What serves a VM agent's relayed connections: an MCP session each, on
/// stdio framing, scoped as the block's token would be over HTTP.
pub fn pipe_server(app: &Arc<App>) -> relay::Serve {
    let app = Arc::downgrade(app);
    Arc::new(move |id, io| {
        let Some(app) = app.upgrade() else { return };
        let caller = Caller::new(Scope::Block(id), Some(format!("%{id}")), None);
        let server = McpServer { app, fallback: Some(caller) };
        tokio::spawn(async move {
            match rmcp::ServiceExt::serve(server, tokio::io::split(io)).await {
                Ok(running) => {
                    let _ = running.waiting().await;
                }
                Err(e) => tracing::debug!(block = id, error = %e, "relayed MCP session didn't start"),
            }
        });
    })
}

fn refuse(status: StatusCode, why: &str) -> Response {
    (status, Json(serde_json::json!({ "error": why }))).into_response()
}

/// Whose request this is: a bearer token's holder, or the owner (who got
/// past the server's checks, or is on the socket).
async fn authenticate(State(app): State<Arc<App>>, mut req: Request, next: Next) -> Response {
    // Any Authorization at all skipped the identity check (`server.rs`):
    // it must be one of our tokens, never the owner by default.
    let bearer = match req.headers().get(header::AUTHORIZATION) {
        None => None,
        Some(v) => {
            match v.to_str().ok().and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer "))) {
                Some(t) => Some(t.trim().to_owned()),
                None => return refuse(StatusCode::UNAUTHORIZED, "MCP takes a bearer token (Authorization: Bearer …)"),
            }
        }
    };
    let pane = req
        .headers()
        .get(PANE_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().trim_start_matches('%').parse::<PaneId>().ok());
    let caller = match bearer {
        None => Caller::new(Scope::Full, None, pane),
        // The daemon's local token: the owner, as the server already found.
        Some(t) if app.access.is_local_token(&t) => Caller::new(Scope::Full, None, pane),
        Some(t) => match app.mcp.check(&t) {
            Some(Bearer::Client { name, scope }) => {
                let scope = match scope {
                    TokenScope::Full => Scope::Full,
                    TokenScope::Read => Scope::Read,
                };
                Caller::new(scope, Some(name), pane)
            }
            Some(Bearer::Block(id)) => match app.mux.api(|r| Api::Block(id, r)).await.flatten() {
                Some(b) if b.kind() == BlockType::Agent => Caller::new(Scope::Block(id), Some(format!("%{id}")), pane),
                _ => return refuse(StatusCode::UNAUTHORIZED, &format!("agent block %{id} is gone; its token with it")),
            },
            None => return refuse(StatusCode::UNAUTHORIZED, "unknown or revoked MCP token"),
        },
    };
    req.extensions_mut().insert(caller);
    next.run(req).await
}

/// The server one MCP session (or one stateless request) talks to.
#[derive(Clone)]
pub struct McpServer {
    app: Arc<App>,
    /// Who it serves when there's no HTTP request to say (a pipe).
    fallback: Option<Caller>,
}

impl McpServer {
    fn caller(&self, ctx: &RequestContext<RoleServer>) -> Caller {
        ctx.extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<Caller>().cloned())
            .or_else(|| self.fallback.clone())
            // Never reached through `/mcp`; the least, to be safe.
            .unwrap_or(Caller::new(Scope::Read, Some("unknown".into()), None))
    }
}

/// What list and resource results carry: Claude Code 2.1.287 rejects them
/// without (S14 saw it for resources; `tools/list` too, at 2026-07-28).
fn fresh<T: CacheHints>(r: T) -> T {
    r.hints()
}

trait CacheHints {
    fn hints(self) -> Self;
}

impl CacheHints for ListToolsResult {
    fn hints(self) -> Self {
        self.with_ttl_ms(0).with_cache_scope(CacheScope::Private)
    }
}

impl CacheHints for ListResourcesResult {
    fn hints(self) -> Self {
        self.with_ttl_ms(0).with_cache_scope(CacheScope::Private)
    }
}

impl CacheHints for ListResourceTemplatesResult {
    fn hints(self) -> Self {
        self.with_ttl_ms(0).with_cache_scope(CacheScope::Private)
    }
}

impl CacheHints for ReadResourceResult {
    fn hints(self) -> Self {
        self.with_ttl_ms(0).with_cache_scope(CacheScope::Private)
    }
}

const INSTRUCTIONS: &str = "illogical runs commands in durable terminal panes that the user can watch \
(on the web and the phone) and take over. Use run to start a build or a dev server in a pane (wait: true \
to wait for it), wait and read_output to follow it (they return \"still running\" with an offset: call \
again), list to see what's there, open_port to show a dev server in a browser block beside its terminal, \
open_app to show one of the user's studio apps (its agent's questions come to them; send_input prompts it), attach to put a file (a screenshot) into a terminal or an agent block, open_pr to show a pull request (read_pr reads it; pr_comment, pr_review and pr_merge draft writes the user sends), open_issue to show an issue (read_issue reads it; issue_comment and issue_new draft what the user sends), list_agents and read_agent to see the user's Fountain agents (open_fountain shows them as a catalog; start_agent with a Fountain agent hands one a task), invite_person to ask the user to bring someone into the session (read_invite says what became of it), start_agent and agent_respond to supervise another agent, read_thread and post_thread for the people's conversation about a pane or session (an @agent message there reaches you as a follow-up: answer with post_thread), list_conversations and open_conversation to \
pick up a Claude Code conversation from a terminal or the desktop app, and history and search for what \
happened before. Output is paged: pass next_offset back as offset. \
Blocks you can open: a terminal (run), a browser (open_port), a diff (show_changes), a file (show_file), \
a PR or issue (open_pr, open_issue), a studio app (open_app), the Fountain catalog (open_fountain), a \
conversation (open_conversation); show one instead of describing it, and wait (until idle or needs_input) instead of polling output. \
Claude Code hooks put your questions (illogical ask), permission prompts (illogical hook, which anyone allowed can \
answer), follow-ups (illogical inbox) and attention on cards; without them your questions stay in the terminal. \
`illogical hooks install` adds them: ask your person first.";

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_server_info(Implementation::new("illogical", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let scope = self.caller(&ctx).scope;
        Ok(fresh(ListToolsResult::with_all_items(tools::list(scope))))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let caller = self.caller(&ctx);
        let client = ctx
            .client_info()
            .map(|i| i.name)
            .filter(|n| !n.trim().is_empty())
            .or_else(|| caller.token.clone())
            .unwrap_or_else(|| "client".into());
        info!(
            tool = %request.name,
            client,
            token = caller.token.as_deref().unwrap_or("owner"),
            scope = ?caller.scope,
            "mcp call"
        );
        let args = request.arguments.map(serde_json::Value::Object).unwrap_or_else(|| serde_json::json!({}));
        let call = tools::Call::new(&self.app, caller, client, &ctx);
        Ok(call.dispatch(&request.name, args).await.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let mut history = Resource::new("illogical://history", "history");
        history.description = Some("The last commands across panes, with exit codes and who ran them".into());
        history.mime_type = Some("application/json".into());
        Ok(fresh(ListResourcesResult::with_all_items(vec![history])))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let t = |uri: &str, name: &str, what: &str| {
            let mut t = ResourceTemplate::new(uri, name);
            t.description = Some(what.into());
            t
        };
        Ok(fresh(ListResourceTemplatesResult::with_all_items(vec![
            t("illogical://pane/{id}/output", "pane output", "A pane's latest output, escape sequences stripped"),
            t("illogical://pane/{id}/screen", "pane screen", "What a pane shows now, as text"),
            t("illogical://block/{id}", "block", "A pane's or block's state, as JSON"),
        ])))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let caller = self.caller(&ctx);
        let call = tools::Call::new(&self.app, caller, "resource".into(), &ctx);
        let text = call.resource(&request.uri).await.map_err(|e| McpError::resource_not_found(e, None))?;
        Ok(fresh(ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)])).into())
    }
}
