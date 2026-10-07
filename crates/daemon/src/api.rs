//! The HTTP API (see `arugula_proto::api` for the routes and shapes). The
//! `arugula` CLI uses it over the Unix socket; remote agents can use it
//! over the tailnet, where the same access checks as the web client apply.

use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use arugula_proto::{
    Driver, EventKind, Frame, FrameKind, PaneId, SessionId,
    api::{
        AttentionRequest, Empty, HistoryKind, Invitable, KeysRequest, MouseRequest, NotifyPref, NotifyRequest,
        OpenConversationRequest, OpenConversationResponse, OpenResponse, Process, PromptRequest, PromptResult,
        RunRequest, RunResponse, SendRequest, ThreadAgent, ThreadMessages, ThreadPostRequest, ThreadPosted,
        ThreadReadRequest, Unreached, UnreachedWhy, WaitResult,
    },
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream::{self, StreamExt};
use regex::Regex;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{
    history::{self, Filter},
    keys,
    mux::{Api, AskReply, Cmd, InboxReply, MuxHandle},
    osc::strip,
    pane::{CaptureFormat, CaptureScope, PaneHandle, Subscriber, ToClient},
    push::Subscription,
    server::App,
    store::{PaneLog, now_ms},
};

type AppState = State<Arc<App>>;

pub fn routes() -> Router<Arc<App>> {
    let r = Router::new()
        .route("/api/panes", get(panes))
        .route("/api/run", post(run))
        .route("/api/panes/{id}/send", post(send))
        .route("/api/panes/{id}/prompt", post(prompt_))
        .route("/api/panes/{id}/keys", post(keys_))
        .route("/api/panes/{id}/mouse", post(mouse))
        .route("/api/panes/{id}/attention", post(attention))
        .route("/api/attention", get(attention_list))
        .route("/api/attention/act", post(act))
        .route("/api/panes/{id}/ask", post(ask))
        .route("/api/panes/{id}/ask/withdraw", post(ask_withdraw))
        .route("/api/panes/{id}/permit", post(permit))
        .route("/api/panes/{id}/hook", post(hook))
        .route("/api/panes/{id}/inbox", post(inbox))
        .route("/api/panes/{id}/followup", post(followup))
        .route("/api/turn", get(turn))
        .route("/api/threads/{target}", get(thread_get).post(thread_post))
        .route("/api/threads/{target}/read", post(thread_read))
        .route("/api/panes/{id}/close", post(close))
        .route("/api/panes/{id}/capture", get(capture))
        .route("/api/panes/{id}/process", get(process))
        .route("/api/panes/{id}/detection", get(detection))
        .route("/api/panes/{id}/tail", get(tail))
        .route("/api/panes/{id}/wait", get(wait))
        .route("/api/panes/{id}/export.cast", get(export))
        .route("/api/panes/{id}/drivers", get(drivers))
        .route("/api/panes/{id}/diff", get(diff_of))
        .route("/api/ide", get(ide_get).put(ide_set))
        .route("/api/rules", get(rules_get).delete(rules_forget_all))
        .route("/api/rules/{index}", axum::routing::delete(rules_forget))
        .route("/api/hosts/self/shell-env", get(shell_env_get))
        .route("/api/hosts/self/shell-env/refresh", post(shell_env_refresh))
        .route("/api/hosts/self/agents", get(agents_get))
        .route("/api/hosts/self/agents/refresh", post(agents_refresh))
        .route("/api/editors", get(editors))
        .route("/api/editors/vsix", get(vsix))
        .route("/api/ide/mention", post(ide_mention))
        .route("/api/sessions/{id}/secrets", get(secrets))
        .route("/api/agents/adapters", get(adapters))
        .route("/api/agents/adapters/{kind}/install", post(install_adapter))
        .route("/api/conversations", get(conversations))
        .route("/api/conversations/open", post(open_conversation))
        .route("/api/blocks", post(open_block))
        .route("/api/blocks/{id}", get(describe))
        .route("/api/blocks/{id}/call/{method}", post(call))
        .route("/api/studio", get(studio_status).post(studio_login).delete(studio_logout))
        .route("/api/studio/apps", get(studio_apps))
        .route("/api/fountain/agents", get(fountain_agents))
        .route("/api/studio/followers/{app}", axum::routing::put(studio_follower).delete(studio_unfollow))
        .route("/api/machines", get(machines))
        .route("/api/machines/{id}/reset", post(reset_machine))
        .route("/api/panes/{id}/share-machine", post(share_machine))
        .route("/api/events", get(events))
        .route("/api/history", get(history_))
        .route("/api/search", get(search))
        .route("/api/push/key", get(push_key))
        .route("/api/push/subscribe", post(push_subscribe))
        .route("/api/push/test", post(push_test))
        .route("/api/notify", get(notify_get).post(notify_set));
    // M70: a file onto the pane's host, and its path pasted.
    #[cfg(unix)]
    let r = r
        .route(
            "/api/panes/{id}/upload",
            post(crate::upload::upload).layer(axum::extract::DefaultBodyLimit::max(crate::upload::CHUNK_MAX)),
        )
        .route("/api/panes/{id}/paste", post(crate::upload::paste));
    r
}

pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

pub(crate) type Res<T> = Result<T, ApiError>;

pub(crate) async fn pane(app: &App, id: PaneId) -> Res<PaneHandle> {
    app.mux.api(|r| Api::Pane(id, r)).await.flatten().ok_or(ApiError(StatusCode::NOT_FOUND, format!("no pane %{id}")))
}

/// A subscriber of our own, for streaming a pane's output; detaches when
/// dropped (the HTTP client went away).
struct Tap {
    pane: PaneHandle,
    client: u64,
    rx: crate::pane::ClientRx,
    _ctrl: mpsc::UnboundedReceiver<ToClient>,
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.pane.detach(self.client);
    }
}

static NEXT_TAP: AtomicU64 = AtomicU64::new(1 << 62);

fn tap(pane: PaneHandle, from: u64) -> Tap {
    let client = NEXT_TAP.fetch_add(1, Ordering::Relaxed);
    let (data, rx) = crate::pane::client_queue();
    let (ctrl, _ctrl) = mpsc::unbounded_channel();
    pane.attach(
        Subscriber { client, data, ctrl, principal: crate::acl::Principal::Owner, name: None, device: None },
        Some(from),
    );
    Tap { pane, client, rx, _ctrl }
}

impl Tap {
    /// The next chunk of output (skipping sizes; a snapshot means the tap
    /// fell behind, which only costs a gap for these readers).
    async fn next(&mut self) -> Option<(u64, Vec<u8>)> {
        loop {
            match self.rx.recv().await? {
                ToClient::Frame(bytes) => {
                    let f = Frame::decode(&bytes).ok()?;
                    if f.kind == FrameKind::Output {
                        return Some((f.offset, f.data));
                    }
                }
                ToClient::Msg(_) | ToClient::Json(_) => {}
                ToClient::Close => return None,
            }
        }
    }
}

fn read_log(app: &App, id: PaneId, from: u64) -> (u64, Vec<u8>) {
    PaneLog::open(app.mux.store.pane_dir(id)).and_then(|l| l.read_from(from)).unwrap_or((from, vec![]))
}

// ---------------------------------------------------------------- handlers

async fn panes(State(app): AppState) -> Res<Response> {
    let list = app.mux.api(Api::Panes).await.unwrap_or_default();
    Ok(Json(list).into_response())
}

async fn run(State(app): AppState, Json(req): Json<RunRequest>) -> Res<Json<RunResponse>> {
    if req.command.as_deref().is_some_and(|c| c.trim().is_empty()) {
        return Err(bad("empty command"));
    }
    match app.mux.api(|r| Api::Run(req, r)).await {
        Some(Ok(pane)) => Ok(Json(RunResponse { pane })),
        Some(Err(e)) => Err(bad(e)),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }
}

async fn send(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(req): Json<SendRequest>,
) -> Res<Json<serde_json::Value>> {
    pane(&app, id).await?.mark_input();
    let mut data = req.text.into_bytes();
    if req.enter {
        data.push(b'\r');
    }
    app.mux.send(Cmd::Input { client: None, pane: id, data });
    Ok(Json(serde_json::json!({})))
}

/// `arugula send %N --wait`: prompt the agent there and wait for its
/// turn (#147).
async fn prompt_(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(req): Json<PromptRequest>,
) -> Res<Json<PromptResult>> {
    let stall = secs(req.stall, STALL);
    let limit = secs(req.timeout, Duration::from_secs(100));
    match tokio::time::timeout(limit, prompt(&app, id, req.text, req.answering, stall, None)).await {
        Ok(Ok(r)) => Ok(Json(r)),
        Ok(Err(e)) => Err(bad(e)),
        Err(_) => Ok(Json(PromptResult::StillRunning)),
    }
}

/// How long a prompted agent has to show any sign of work (#147).
pub(crate) const STALL: Duration = Duration::from_secs(5);

/// Seconds from a request, or a default.
pub(crate) fn secs(s: Option<f64>, default: Duration) -> Duration {
    s.filter(|s| s.is_finite() && *s >= 0.0).map(Duration::from_secs_f64).unwrap_or(default)
}

/// Prompt the agent in a pane (a terminal running one, or an agent block)
/// and wait for its turn: `Done` when it ends, `NeedsInput` when it asks
/// for someone, `Stalled` when nothing shows it working within `stall` (no
/// agent there, the prompt not submitted, the agent gone). The wait starts
/// before anything is typed, so a quick turn can't slip past it. An agent
/// already waiting on someone isn't typed at, unless `answering`: typing
/// into its approval dialog would answer it.
pub(crate) async fn prompt(
    app: &App,
    id: PaneId,
    text: String,
    answering: bool,
    stall: Duration,
    by: Option<String>,
) -> Result<PromptResult, String> {
    use arugula_proto::{Attention, BlockType, WorkKind};
    let info = pane_info(app, id).await.ok_or_else(|| format!("no pane %{id}"))?;
    let block = match info.kind {
        BlockType::Terminal => None,
        BlockType::Agent => Some(app.mux.api(|r| Api::Block(id, r)).await.flatten().ok_or(format!("no block %{id}"))?),
        k => return Err(format!("%{id} is a {k:?} block, not an agent")),
    };
    if block.is_none() && info.work != Some(WorkKind::Agent) {
        let why = match &info.command {
            Some(c) => format!("%{id} runs `{c}`, not an agent; nothing was typed"),
            None => format!("%{id} is at its shell, with no agent running; nothing was typed"),
        };
        return Ok(PromptResult::Stalled { why, screen: screen(app, id).await });
    }
    // What it waits on: a terminal's question card, or an agent block's
    // first question not yet opened (as `wait` finds it).
    let question = |info: &arugula_proto::PaneInfo| {
        let ask = info.ask.clone().or_else(|| {
            let s = block.as_ref()?.state();
            let a = s["asks"].as_array()?.iter().find(|a| a["accepted"] != true)?.clone();
            serde_json::from_value::<arugula_proto::ask::Ask>(a).ok()
        });
        (info.reason.as_ref().map(|r| r.headline.clone()), ask.map(Box::new))
    };
    if info.attention == Attention::NeedsInput && !answering {
        let (question, ask) = question(&info);
        return Ok(PromptResult::Blocked { question, ask });
    }
    // Listen first, then type.
    let mut events = app.mux.events();
    // Typing alone makes a terminal pane "working" (someone's busy in
    // it), so where its agent's screen can be read, that says when the
    // agent starts instead.
    let reads_screen = block.is_none() && screen_state(app, id).await.is_some();
    let started_now = async |attention: Attention| {
        if reads_screen {
            matches!(screen_state(app, id).await.flatten(), Some("working" | "blocked"))
        } else {
            attention == Attention::Working
        }
    };
    let mut started = started_now(info.attention).await;
    match &block {
        Some(b) => {
            b.call_by("send", serde_json::json!({ "text": text }), by.as_deref()).await?;
        }
        None => {
            let p = pane(app, id).await.map_err(|e| e.1)?;
            p.mark_input();
            let input = |data: Vec<u8>| match &by {
                Some(by) => Cmd::Api(Api::InputBy(id, data, by.clone())),
                None => Cmd::Input { client: None, pane: id, data },
            };
            app.mux.send(input(text.into_bytes()));
            // Enter on its own: in the same read as the text, an agent can
            // take it for part of a paste and not submit.
            tokio::time::sleep(Duration::from_millis(150)).await;
            app.mux.send(input(b"\r".to_vec()));
        }
    }
    let stall_at = tokio::time::Instant::now() + stall;
    let mut attention = info.attention;
    loop {
        let next = if started {
            Some(events.recv().await)
        } else {
            if tokio::time::Instant::now() >= stall_at {
                let why = format!("no sign of work within {}s of the prompt", stall.as_secs_f64());
                return Ok(PromptResult::Stalled { why, screen: screen(app, id).await });
            }
            // Look at its screen again now and then: it can start working
            // with its attention already "working" from the typing.
            tokio::time::timeout(Duration::from_millis(100), events.recv()).await.ok()
        };
        let state = match next {
            None => attention,
            Some(Ok(e)) if e.pane != Some(id) => continue,
            Some(Ok(e)) => match e.kind {
                EventKind::Attention { state, .. } => state,
                EventKind::Closed => return Err(format!("%{id} closed")),
                EventKind::Exit { .. } if !started => {
                    let why = "its program exited".to_owned();
                    return Ok(PromptResult::Stalled { why, screen: screen(app, id).await });
                }
                _ => continue,
            },
            // Missed some: where it is now.
            Some(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                pane_info(app, id).await.ok_or_else(|| format!("%{id} closed"))?.attention
            }
            Some(Err(_)) => return Err("the daemon is shutting down".into()),
        };
        attention = state;
        if state == Attention::NeedsInput {
            let info = pane_info(app, id).await.ok_or_else(|| format!("%{id} closed"))?;
            let (question, ask) = question(&info);
            return Ok(PromptResult::NeedsInput { question, ask });
        }
        if !started {
            started = started_now(state).await;
        } else if matches!(state, Attention::Idle | Attention::Done) {
            return Ok(PromptResult::Done);
        }
    }
}

/// What a terminal pane's agent screen was last read as (`working`,
/// `blocked`, `idle`; `Some(None)` before its first reading), or `None`
/// when no agent's screen is read there.
async fn screen_state(app: &App, id: PaneId) -> Option<Option<&'static str>> {
    let p = pane(app, id).await.ok()?;
    tokio::task::spawn_blocking(move || p.detection()).await.ok().flatten().filter(|d| !d.unread).map(|d| d.shown)
}

async fn pane_info(app: &App, id: PaneId) -> Option<arugula_proto::PaneInfo> {
    app.mux.api(Api::Panes).await.unwrap_or_default().into_iter().find(|p| p.info.id == id).map(|p| p.info)
}

/// The last lines of a pane's screen, for a caller to see why.
async fn screen(app: &App, id: PaneId) -> String {
    let Ok(p) = pane(app, id).await else {
        return match app.mux.api(|r| Api::Block(id, r)).await.flatten() {
            Some(b) => {
                b.text().lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
            }
            None => String::new(),
        };
    };
    let text = tokio::task::spawn_blocking(move || p.capture(CaptureFormat::Text, CaptureScope::Screen))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}

async fn keys_(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(req): Json<KeysRequest>,
) -> Res<Json<serde_json::Value>> {
    let p = pane(&app, id).await?;
    let modes = p.status().modes;
    let data: Vec<u8> = req.keys.iter().flat_map(|k| keys::key(k, modes)).collect();
    p.mark_input();
    app.mux.send(Cmd::Input { client: None, pane: id, data });
    Ok(Json(serde_json::json!({})))
}

async fn mouse(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(req): Json<MouseRequest>,
) -> Res<Json<serde_json::Value>> {
    let p = pane(&app, id).await?;
    let data = keys::mouse(req.x, req.y, req.button, req.action, p.status().modes)
        .ok_or_else(|| bad("the program in that pane isn't listening to the mouse"))?;
    p.mark_input();
    app.mux.send(Cmd::Input { client: None, pane: id, data });
    Ok(Json(serde_json::json!({})))
}

async fn attention(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(req): Json<AttentionRequest>,
) -> Res<Json<serde_json::Value>> {
    match app.mux.api(|r| Api::Attention(id, req.state, req.why, r)).await {
        Some(true) => Ok(Json(serde_json::json!({}))),
        _ => Err(ApiError(StatusCode::NOT_FOUND, format!("no pane %{id}"))),
    }
}

/// Every pane that wants you, and why (M24): what `arugula attention`
/// lists.
async fn attention_list(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<Vec<arugula_proto::api::AttentionItem>>> {
    let who = who.map(|axum::Extension(w)| w).filter(|w| !w.is_owner());
    Ok(Json(app.mux.api(|r| Api::AttentionList(who, r)).await.unwrap_or_default()))
}

/// Do something about one pane's reason or several (M24): allow or deny an
/// approval, answer or skip a question, or dismiss it. Each pane is checked
/// on its own (editor on its session) and answered on its own.
async fn act(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
    headers: HeaderMap,
    Json(req): Json<arugula_proto::api::ActRequest>,
) -> Res<Response> {
    use arugula_proto::api::{ActResponse, ActResult};
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    let panes = req.targets();
    if panes.is_empty() {
        return Err(bad("name a pane (pane) or several (panes)"));
    }
    // An agent's invite (#234): the owner's alone, and not an agent's on
    // the owner's CLI (a courtesy, as for `call`).
    if !who.is_owner() || crate::invite::agent(&headers) {
        for p in &panes {
            if is_invite(&app, *p).await {
                return Err(ApiError(StatusCode::FORBIDDEN, crate::invite::OWNER_ONLY.into()));
            }
        }
    }
    // All or nothing on access: a list with one pane you can't change is
    // refused whole, so a bundle never half-happens for that reason.
    if !who.is_owner() {
        for p in &panes {
            match app.mux.api(|r| Api::RoleOn(who.clone(), *p, r)).await.flatten() {
                Some((role, _)) if role >= arugula_core::Role::Editor => {}
                Some(_) => {
                    return Err(ApiError(
                        StatusCode::FORBIDDEN,
                        format!("you're watching %{p}'s session; you can't answer for it"),
                    ));
                }
                None => return Err(ApiError(StatusCode::NOT_FOUND, format!("no pane %{p}"))),
            }
        }
    }
    let mut results = Vec::new();
    let by = who_is(&app, who).await;
    for pane in panes {
        let r = act_one(&app, pane, &req, by.clone()).await;
        results.push(ActResult { pane, ok: r.is_ok(), error: r.err() });
    }
    let status = if results.iter().any(|r| r.ok) { StatusCode::OK } else { StatusCode::CONFLICT };
    Ok((status, Json(ActResponse { results })).into_response())
}

async fn act_one(
    app: &App,
    pane: PaneId,
    req: &arugula_proto::api::ActRequest,
    by: Option<Driver>,
) -> Result<(), String> {
    use arugula_proto::{Action, AskWhat, Attention};
    match req.action {
        // An editor's debugger (M28).
        Action::Continue => {
            let b = app
                .mux
                .api(|r| Api::Block(pane, r))
                .await
                .flatten()
                .ok_or_else(|| format!("%{pane} isn't an editor"))?;
            return block_call(app, pane, &b, "continue", serde_json::json!({}), by).await.map(|_| ());
        }
        // An edit waiting as a diff (M28).
        Action::Accept | Action::Reject => {
            let by = by.unwrap_or(Driver { who: "owner".into(), name: "owner".into() });
            let accept = req.action == Action::Accept;
            return app
                .mux
                .api(|r| Api::DiffAnswer(pane, req.id.clone(), accept, req.text.clone(), by, r))
                .await
                .unwrap_or_else(|| Err("the daemon is stopping".into()));
        }
        _ => {}
    }
    // A failed command typed again (M11), once its shell is idle.
    if req.action == Action::Rerun {
        let (reason, block) = app
            .mux
            .api(|r| Api::Reason(pane, r))
            .await
            .flatten()
            .ok_or_else(|| format!("%{pane} has nothing to run again (it was dismissed, or ran since)"))?;
        // M39: a forge block's failed checks run again through its forge.
        if block && reason.actions.contains(&Action::Rerun) {
            let b = app.mux.api(|r| Api::Block(pane, r)).await.flatten().ok_or_else(|| format!("no block %{pane}"))?;
            return block_call(app, pane, &b, "rerun_checks", serde_json::json!({}), by).await.map(|_| ());
        }
        let command = reason
            .command
            .filter(|_| reason.actions.contains(&Action::Rerun))
            .ok_or_else(|| format!("%{pane} has no command to run again"))?;
        let line = crate::fs::rerun_line(&command).ok_or_else(|| format!("can't type {command:?} again"))?;
        return crate::fs::type_line(app, pane, line, "rerun").await.map_err(|e| e.to_string());
    }
    if req.action == Action::Dismiss {
        return match app.mux.api(|r| Api::Attention(pane, Attention::Idle, None, r)).await {
            Some(true) => Ok(()),
            _ => Err(format!("no pane %{pane}")),
        };
    }
    let (reason, block) = app
        .mux
        .api(|r| Api::Reason(pane, r))
        .await
        .flatten()
        .ok_or_else(|| format!("%{pane} doesn't want anything (it was answered, or dismissed)"))?;
    // A gate (M34): approve it, through the block that read it.
    if let Some(g) = reason.gate.filter(|_| reason.kind == arugula_proto::ReasonKind::Gate) {
        if req.action != Action::Allow {
            return Err(format!("%{pane} waits at a gate: approve it (allow) or dismiss it"));
        }
        if req.id.as_ref().is_some_and(|id| *id != g.key()) {
            return Err(format!("%{pane} now waits at another gate (that one was approved)"));
        }
        let b = app.mux.api(|r| Api::Block(pane, r)).await.flatten().ok_or_else(|| format!("no block %{pane}"))?;
        // M36: a review asked of you is approved as a review, sent with the
        // owner's forge login and naming who approved.
        if matches!(g.source, arugula_proto::GateSource::Forge { .. }) {
            let args = serde_json::json!({ "event": "approve", "key": g.key() });
            return block_call(app, pane, &b, "review", args, by).await.map(|_| ());
        }
        let args = serde_json::json!({ "key": g.key() });
        return block_call(app, pane, &b, "approve", args, by).await.map(|_| ());
    }
    let ask = reason.ask.ok_or_else(|| format!("%{pane} isn't asking anything: dismiss it"))?;
    let id = req.id.clone().unwrap_or(ask.id.clone());
    if id != ask.id {
        return Err(format!("%{pane} now asks something else (it was answered)"));
    }
    let (method, args) = match (req.action, ask.what) {
        (Action::Allow, AskWhat::Approve) => (
            "approve",
            serde_json::json!({ "id": id, "option": req.option.as_deref().unwrap_or("once"), "suggestion": req.suggestion }),
        ),
        (Action::Deny, AskWhat::Approve) => (
            "deny",
            serde_json::json!({ "id": id, "reason": req.message.as_deref().unwrap_or(""), "message": req.message }),
        ),
        (Action::Deny, AskWhat::Question) => ("decline", serde_json::json!({ "id": id })),
        (Action::Answer, AskWhat::Question) => {
            let content = req.content.clone().filter(|c| c.is_object()).ok_or("answer needs content")?;
            ("answer", serde_json::json!({ "id": id, "content": content }))
        }
        (Action::Allow, AskWhat::Question) => return Err(format!("%{pane} asks a question: answer it")),
        (Action::Answer, AskWhat::Approve) => return Err(format!("%{pane} asks for approval: allow or deny it")),
        (Action::Dismiss | Action::Continue | Action::Accept | Action::Reject | Action::Rerun, _) => {
            unreachable!("handled above")
        }
    };
    if block {
        let b = app.mux.api(|r| Api::Block(pane, r)).await.flatten().ok_or_else(|| format!("no block %{pane}"))?;
        block_call(app, pane, &b, method, args, by).await.map(|_| ())
    } else {
        answer_terminal(app, pane, method, args, by).await.map(|_| ()).map_err(|e| e.1)
    }
}

#[derive(Deserialize)]
struct AskRequest {
    /// AskUserQuestion's `questions`, as the hook got them.
    questions: serde_json::Value,
    /// The tool use's id, so asking again (after a daemon restart) is the
    /// same question.
    #[serde(default)]
    id: Option<String>,
    /// What raised it, when that isn't Claude Code's hook (M35: `hud`, for
    /// a studio box's agent asking on a browser or app block).
    #[serde(default)]
    source: Option<String>,
    /// Who asks, as the card names it ("hud asks").
    #[serde(default)]
    agent: Option<String>,
}

/// Withdraws a terminal's question if whoever asked it goes away first.
struct AskGuard {
    mux: MuxHandle,
    pane: PaneId,
    token: u64,
    armed: bool,
}

impl Drop for AskGuard {
    fn drop(&mut self) {
        if self.armed {
            self.mux.send(Cmd::Api(Api::AskWithdraw(self.pane, None, Some(self.token))));
        }
    }
}

/// `arugula ask`: show AskUserQuestion's questions beside a terminal and
/// wait for the answer. Answers `{action: accept, content, output, by}`
/// (the hook's output for Claude Code, and who answered), `{action:
/// decline, output, by}`, `{action: terminal}` (answer in the terminal) or
/// `{action: withdrawn}`. A browser or app block takes questions too
/// (M35), from whatever follows a page's agent: `source` and `agent` say
/// who asks.
async fn ask(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(req): Json<AskRequest>,
) -> Res<Json<serde_json::Value>> {
    use arugula_proto::ask::{self, Ask, AskKind};
    if is_invite(&app, id).await {
        return Err(not_on_invites());
    }
    let questions = req.questions.as_array().filter(|q| !q.is_empty()).ok_or_else(|| bad("no questions"))?;
    let message = match questions.as_slice() {
        [q] => q["question"].as_str().unwrap_or_default().to_owned(),
        _ => "Please answer the following questions.".to_owned(),
    };
    let a = Ask {
        id: req.id.clone().unwrap_or_else(|| format!("q{}", now_ms())),
        kind: AskKind::Questions,
        message,
        questions: Some(req.questions.clone()),
        schema: None,
        url: None,
        accepted: false,
        tool_call_id: req.id,
        source: req.source.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "hook".into()),
        agent: req.agent.filter(|s| !s.trim().is_empty()),
        at_ms: now_ms(),
        tool: None,
        input: None,
        suggestions: None,
        session: None,
    };
    let (token, rx) = match app.mux.api(|r| Api::Ask(id, Box::new(a), r)).await {
        Some(Ok(r)) => r,
        Some(Err(e)) => return Err(ApiError(StatusCode::NOT_FOUND, e)),
        None => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    };
    let mut guard = AskGuard { mux: app.mux.clone(), pane: id, token, armed: true };
    let reply = rx.await;
    guard.armed = false;
    Ok(Json(match reply {
        Ok((AskReply::Answer(content), by)) => {
            let output = ask::hook_output(&req.questions, &content);
            serde_json::json!({ "action": "accept", "content": content, "output": output, "by": by })
        }
        Ok((AskReply::Decline, by)) => {
            serde_json::json!({ "action": "decline", "output": ask::hook_declined(), "by": by })
        }
        Ok((AskReply::Terminal, _)) => serde_json::json!({ "action": "terminal" }),
        // A question is never allowed or denied (the mux refuses that).
        Ok((AskReply::Withdrawn | AskReply::Allow { .. } | AskReply::Deny { .. }, _)) => {
            serde_json::json!({ "action": "withdrawn" })
        }
        // The daemon is going away; the asker asks the next one.
        Err(_) => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }))
}

/// `arugula hook` on Claude Code's `PermissionRequest` (M29): a card
/// with the tool, its input and Claude's suggestions, beside the terminal,
/// until someone answers it or the terminal does. Answers `{action: allow
/// | deny, output}` (the hook's output) or `{action: withdrawn}`.
async fn permit(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(hook): Json<serde_json::Value>,
) -> Res<Json<serde_json::Value>> {
    use arugula_proto::ask::{self, Ask, AskKind};
    if is_invite(&app, id).await {
        return Err(not_on_invites());
    }
    let tool = hook["tool_name"].as_str().ok_or_else(|| bad("no tool_name"))?.to_owned();
    let input = hook["tool_input"].clone();
    let session = format!("{}/{}", hook["session_id"].as_str().unwrap_or(""), hook["agent_id"].as_str().unwrap_or(""));
    let a = Ask {
        id: format!("p{}", now_ms()),
        kind: AskKind::Permission,
        message: ask::permission_message(&tool, &input),
        questions: None,
        schema: None,
        url: None,
        accepted: false,
        tool_call_id: None,
        source: "hook".into(),
        agent: None,
        at_ms: now_ms(),
        tool: Some(tool),
        input: Some(input),
        suggestions: hook.get("permission_suggestions").filter(|s| s.is_array()).cloned(),
        session: Some(session),
    };
    let (token, rx) = match app.mux.api(|r| Api::Ask(id, Box::new(a), r)).await {
        Some(Ok(r)) => r,
        Some(Err(e)) => return Err(ApiError(StatusCode::NOT_FOUND, e)),
        None => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    };
    let mut guard = AskGuard { mux: app.mux.clone(), pane: id, token, armed: true };
    let reply = rx.await;
    guard.armed = false;
    Ok(Json(match reply {
        Ok((AskReply::Allow { always }, _)) => {
            serde_json::json!({ "action": "allow", "output": ask::permit_allow(always.as_ref()) })
        }
        Ok((AskReply::Deny { message }, _)) => {
            serde_json::json!({ "action": "deny", "output": ask::permit_deny(&message) })
        }
        Ok(_) => serde_json::json!({ "action": "withdrawn" }),
        Err(_) => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }))
}

/// `arugula hook`: one of Claude Code's hook events (M29).
async fn hook(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(hook): Json<serde_json::Value>,
) -> Res<Json<serde_json::Value>> {
    app.mux.send(Cmd::Api(Api::Hook(id, hook)));
    Ok(Json(serde_json::json!({})))
}

/// Drops a follow-up waiter if whoever waits goes away first.
struct InboxGuard {
    mux: MuxHandle,
    pane: PaneId,
    token: u64,
}

impl Drop for InboxGuard {
    fn drop(&mut self) {
        self.mux.send(Cmd::Api(Api::InboxGone(self.pane, self.token)));
    }
}

/// `arugula inbox` (Claude Code's background `Stop` and `SessionStart`
/// hook, M29): wait for a follow-up. `{action: follow_up, text, by}`, or
/// `{action: replaced}` when a newer waiter took over.
async fn inbox(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(hook): Json<serde_json::Value>,
) -> Res<Json<serde_json::Value>> {
    let (token, rx) = match app.mux.api(|r| Api::Inbox(id, hook, r)).await {
        Some(Ok(r)) => r,
        Some(Err(e)) => return Err(ApiError(StatusCode::NOT_FOUND, e)),
        None => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    };
    let _guard = InboxGuard { mux: app.mux.clone(), pane: id, token };
    Ok(Json(match rx.await {
        Ok(InboxReply::FollowUp { text, by }) => serde_json::json!({ "action": "follow_up", "text": text, "by": by }),
        Ok(InboxReply::Replaced) => serde_json::json!({ "action": "replaced" }),
        Err(_) => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }))
}

#[derive(Deserialize)]
struct FollowUpRequest {
    text: String,
}

/// A follow-up for the agent in a pane (M29), from whoever may drive it:
/// an agent block's next prompt, or Claude Code's in a terminal (through
/// its inbox hook, never typed). Recorded as theirs. `{delivered}`: it went
/// straight in (else it waits for the agent).
async fn followup(
    State(app): AppState,
    Path(id): Path<PaneId>,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Json(req): Json<FollowUpRequest>,
) -> Res<Json<serde_json::Value>> {
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    let by = who_is(&app, who)
        .await
        .ok_or_else(|| ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into()))?;
    if let Some(b) = app.mux.api(|r| Api::Block(id, r)).await.flatten() {
        let name = (by.who != "owner").then_some(by.name.as_str());
        b.call_by("send", serde_json::json!({ "text": req.text }), name).await.map_err(bad)?;
        return Ok(Json(serde_json::json!({ "delivered": true })));
    }
    match app.mux.api(|r| Api::FollowUp(id, req.text, by, r)).await {
        Some(Ok(now)) => Ok(Json(serde_json::json!({ "delivered": now }))),
        Some(Err(e)) => Err(bad(e)),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }
}

#[derive(Deserialize)]
struct WithdrawRequest {
    #[serde(default)]
    id: Option<String>,
}

async fn ask_withdraw(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(req): Json<WithdrawRequest>,
) -> Res<Json<serde_json::Value>> {
    if is_invite(&app, id).await {
        return Err(not_on_invites());
    }
    app.mux.send(Cmd::Api(Api::AskWithdraw(id, req.id, None)));
    Ok(Json(serde_json::json!({})))
}

// ---- threads (M61)

fn thread_target(key: &str) -> Res<arugula_proto::ThreadTarget> {
    arugula_proto::ThreadTarget::parse(key)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "a thread is pane-N or session-N".into()))
}

fn thread_err(e: crate::mux::ThreadError) -> ApiError {
    ApiError(StatusCode::from_u16(e.0).unwrap_or(StatusCode::BAD_REQUEST), e.1)
}

fn gone() -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())
}

/// A thread's messages, as the caller may read them.
async fn thread_get(
    State(app): AppState,
    Path(key): Path<String>,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<ThreadMessages>> {
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    let target = thread_target(&key)?;
    let messages = app.mux.api(|r| Api::ThreadGet(target, who, r)).await.ok_or_else(gone)?.map_err(thread_err)?;
    Ok(Json(ThreadMessages { target, messages }))
}

/// Post in a thread. An `@agent` in a pane's thread, from someone who may
/// drive the pane, also goes to its agent as a follow-up.
async fn thread_post(
    State(app): AppState,
    Path(key): Path<String>,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Json(req): Json<ThreadPostRequest>,
) -> Res<Json<ThreadPosted>> {
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    let owner = who.is_owner();
    let target = thread_target(&key)?;
    let post = crate::mux::ThreadPost { target, who, as_agent: None, text: req.text, quote: req.quote };
    let (msg, to_agent, mut unreached) =
        app.mux.api(|r| Api::ThreadPost(post, r)).await.ok_or_else(gone)?.map_err(thread_err)?;
    let mut agent = None;
    if to_agent && let arugula_proto::ThreadTarget::Pane(pane) = target {
        agent = Some(match tell_agent(&app, pane, &msg).await {
            Ok(now) => ThreadAgent { delivered: Some(now), error: None },
            Err(e) => ThreadAgent { delivered: None, error: Some(e) },
        });
    }
    // The owner, who sees every grant and roster already, is offered to
    // invite whom an @ named but who can't read the thread (#297). Nobody
    // else's post does any of this, so theirs says nothing about who exists.
    let invitable = if owner { Some(invitable(&app, target, &mut unreached).await) } else { None };
    Ok(Json(ThreadPosted { message: msg, agent, unreached, invitable }))
}

/// Whom an owner's `@`s that reached nobody name, of those an invite may
/// name, who can't read the thread and could once invited (not on a
/// private pane's). Their tokens leave `unreached`: the offer says it.
async fn invitable(app: &App, target: arugula_proto::ThreadTarget, unreached: &mut Vec<Unreached>) -> Vec<Invitable> {
    let tokens: Vec<String> =
        unreached.iter().filter(|u| u.why == UnreachedWhy::Nobody).map(|u| u.token.clone()).collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    let named: Vec<(String, crate::invite::Person)> = crate::invite::nameable(app)
        .into_iter()
        .filter_map(|p| {
            let t = tokens.iter().find(|t| crate::threads::names(t, &p.id, &p.name))?;
            Some((t.clone(), p))
        })
        .collect();
    if named.is_empty() {
        return Vec::new();
    }
    let ids = named.iter().map(|(_, p)| p.id.clone()).collect();
    let Some(reads) = app.mux.api(|r| Api::CanRead(target, ids, r)).await.flatten() else { return Vec::new() };
    let out: Vec<(String, crate::invite::Person)> =
        named.into_iter().zip(reads).filter(|(_, reads)| !reads).map(|(n, _)| n).collect();
    unreached.retain(|u| !out.iter().any(|(t, _)| *t == u.token));
    // One taken for another (a login by an account's name) says whom.
    out.into_iter().map(|(token, p)| Invitable { token, who: p.id, name: p.name, merged: p.merged }).collect()
}

/// Hand a thread message to the pane's agent, as a follow-up from its
/// author (M61). `Ok(true)`: it went straight in; `Ok(false)`: queued.
async fn tell_agent(app: &App, pane: PaneId, msg: &arugula_proto::ThreadMsg) -> Result<bool, String> {
    let mut text = format!("{} wrote in this pane's thread: {}", msg.name, msg.text);
    if let Some(q) = &msg.quote {
        text.push_str(&format!("\n\nQuoting %{}:\n{}", q.pane, q.text));
    }
    text.push_str("\n\n(Answer in the thread with Arugula's post_thread tool.)");
    let by = Driver { who: msg.who.clone(), name: msg.name.clone() };
    if let Some(b) = app.mux.api(|r| Api::Block(pane, r)).await.flatten() {
        let name = (by.who != "owner").then_some(by.name.as_str());
        return b.call_by("send", serde_json::json!({ "text": text }), name).await.map(|_| true);
    }
    app.mux.api(|r| Api::FollowUp(pane, text, by, r)).await.unwrap_or_else(|| Err("daemon is shutting down".into()))
}

async fn thread_read(
    State(app): AppState,
    Path(key): Path<String>,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Json(req): Json<ThreadReadRequest>,
) -> Res<Json<Empty>> {
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    let target = thread_target(&key)?;
    app.mux.send(Cmd::Api(Api::ThreadRead(target, who, req.upto)));
    Ok(Json(Empty {}))
}

/// Whether a block is an agent's invites (#234): the owner's to answer,
/// and it shows only its own cards.
async fn is_invite(app: &App, id: PaneId) -> bool {
    app.mux.api(|r| Api::Block(id, r)).await.flatten().is_some_and(|b| b.kind() == arugula_proto::BlockType::Invite)
}

fn not_on_invites() -> ApiError {
    ApiError(StatusCode::FORBIDDEN, "an invite block shows only its own cards".into())
}

/// What to call whoever made a request (M13), to attribute what they did.
async fn who_is(app: &App, who: crate::acl::Principal) -> Option<Driver> {
    app.mux.api(|r| Api::Who(who, r)).await
}

/// A block's method, on someone's behalf (M29): an approval or answer is
/// recorded as theirs, for its card, the pane's history and the audit log.
async fn block_call(
    app: &App,
    pane: PaneId,
    b: &Arc<dyn crate::block::Block>,
    method: &str,
    args: serde_json::Value,
    by: Option<Driver>,
) -> Result<serde_json::Value, String> {
    let waiting = b.waiting();
    let id = args["id"].as_str().map(str::to_owned);
    // The transcript names whoever isn't its owner (the owner's own
    // answers go unremarked, as before M29). A gate's ledger names whoever
    // approved it, the owner too, by their Arugula name (#75).
    let gate = matches!(b.kind(), arugula_proto::BlockType::Workspace | arugula_proto::BlockType::App);
    // A forge block (M36) names everyone who writes through it, the owner
    // too: its log and its drafts say who sent what.
    let forge = b.kind() == arugula_proto::BlockType::Forge;
    let name = by.as_ref().filter(|d| gate || forge || d.who != "owner").map(|d| d.name.as_str());
    let out = b.call_by(method, args.clone(), name).await?;
    if (gate && method == "approve") || (forge && method == "review" && out.get("gate").is_some()) {
        // Its card closes saying who, and the audit log says so.
        if let (Some(by), Ok(g)) = (by, serde_json::from_value::<arugula_proto::Gate>(out["gate"].clone())) {
            app.mux.send(Cmd::Api(Api::Answered(pane, by, g.key(), "approved".into(), g.headline())));
        }
        return Ok(out);
    }
    let how = match method {
        "approve" if args["option"].as_str().is_some_and(|o| o.starts_with("always")) => "allowed always",
        "approve" => "allowed",
        "deny" => "denied",
        "answer" => "answered",
        "decline" => "skipped",
        _ => return Ok(out),
    };
    if let (Some(by), Some(w)) = (by, waiting.filter(|w| id.as_ref().is_none_or(|i| *i == w.id))) {
        app.mux.send(Cmd::Api(Api::Answered(pane, by, w.id, how.into(), w.headline)));
    }
    Ok(out)
}

/// A terminal's question answered by `call %N answer|decline|terminal`, or
/// a permission card by `approve|deny` (M29).
async fn answer_terminal(
    app: &App,
    id: PaneId,
    method: &str,
    args: serde_json::Value,
    by: Option<Driver>,
) -> Res<Json<serde_json::Value>> {
    let ask_id = args["id"].as_str().map(str::to_owned);
    let reply = match method {
        "answer" => {
            let content = match args.get("content") {
                Some(c) if c.is_object() => c.clone(),
                _ => {
                    let mut c = args.as_object().cloned().unwrap_or_default();
                    c.remove("id");
                    serde_json::Value::Object(c)
                }
            };
            AskReply::Answer(content)
        }
        "decline" => AskReply::Decline,
        // A permission card (M29): `option` once or always (with one of
        // Claude Code's suggestions, by index: `suggestion`, default 0).
        "approve" => AskReply::Allow {
            always: (args["option"].as_str() == Some("always"))
                .then(|| serde_json::json!(args["suggestion"].as_u64().unwrap_or(0))),
        },
        "deny" => {
            let said = args["message"].as_str().or(args["reason"].as_str()).map(str::trim).filter(|m| !m.is_empty());
            let name = by.as_ref().map_or("someone", |b| b.name.as_str());
            let message = match said {
                Some(m) => format!("{name} said no (through Arugula): {m}"),
                None => format!("{name} said no (through Arugula)."),
            };
            AskReply::Deny { message }
        }
        _ => AskReply::Terminal,
    };
    match app.mux.api(|r| Api::AskReply(id, ask_id, reply, by, r)).await {
        Some(Ok(a)) => Ok(Json(serde_json::json!({ "answered": a.id }))),
        Some(Err(e)) if e == crate::invite::OWNER_ONLY => Err(ApiError(StatusCode::FORBIDDEN, e)),
        Some(Err(e)) => Err(bad(e)),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }
}

async fn close(
    State(app): AppState,
    Path(id): Path<PaneId>,
    who: Option<axum::Extension<crate::acl::Principal>>,
    headers: HeaderMap,
) -> Res<Json<Empty>> {
    // An agent's invites (#234) are the owner's to close, not an agent's.
    let owner = who.is_none_or(|axum::Extension(w)| w.is_owner());
    if (!owner || crate::invite::agent(&headers)) && is_invite(&app, id).await {
        return Err(ApiError(StatusCode::FORBIDDEN, crate::invite::CLOSE_OWNER_ONLY.into()));
    }
    match app.mux.api(|r| Api::Close(id, r)).await {
        Some(true) => Ok(Json(Empty {})),
        _ => Err(ApiError(StatusCode::NOT_FOUND, format!("no pane %{id}"))),
    }
}

#[derive(Deserialize)]
struct CaptureQuery {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

async fn capture(State(app): AppState, Path(id): Path<PaneId>, Query(q): Query<CaptureQuery>) -> Res<Response> {
    // Any block has a text rendering; terminals have more.
    if let Some(b) = app.mux.api(|r| Api::Block(id, r)).await.flatten() {
        return Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], b.text()).into_response());
    }
    let p = pane(&app, id).await?;
    let format = match q.format.as_deref().unwrap_or("text") {
        "text" => CaptureFormat::Text,
        "ansi" => CaptureFormat::Ansi,
        "html" => CaptureFormat::Html,
        f => return Err(bad(format!("format {f}: text, ansi or html"))),
    };
    let scope = match q.scope.as_deref().unwrap_or("screen") {
        "screen" => CaptureScope::Screen,
        "scrollback" => CaptureScope::Scrollback,
        "last-command" => CaptureScope::LastCommand,
        s => return Err(bad(format!("scope {s}: screen, scrollback or last-command"))),
    };
    let text = tokio::task::spawn_blocking(move || p.capture(format, scope))
        .await
        .ok()
        .flatten()
        .ok_or_else(|| ApiError(StatusCode::GATEWAY_TIMEOUT, "the pane didn't answer".into()))?;
    let ctype = if format == CaptureFormat::Html { "text/html; charset=utf-8" } else { "text/plain; charset=utf-8" };
    Ok(([(header::CONTENT_TYPE, ctype)], text).into_response())
}

async fn open_block(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
    headers: HeaderMap,
    Json(mut req): Json<arugula_proto::api::OpenRequest>,
) -> Res<Json<OpenResponse>> {
    let who = who.map(|axum::Extension(w)| w);
    // An invite block (#234) is MCP's invite_person's to make, for what it
    // checked: never anyone's from here.
    if req.kind == arugula_proto::BlockType::Invite {
        return Err(ApiError(StatusCode::FORBIDDEN, "invite blocks are made by MCP's invite_person".into()));
    }
    // M44: a worn Fountain agent runs on this host with the owner's
    // secrets: the owner's alone.
    if req.kind == arugula_proto::BlockType::Agent
        && !req.config["as_fountain"].is_null()
        && who.as_ref().is_some_and(|w| !w.is_owner())
    {
        return Err(ApiError(StatusCode::FORBIDDEN, "only the owner can wear a Fountain agent here".into()));
    }
    // A studio box (M35), by its app's name: where it is, from studio.
    if req.kind == arugula_proto::BlockType::App {
        req.config = app_config(&req.config).await.map_err(bad)?;
    }
    // A pull request (M36), by its link, OWNER/REPO#N, or N in a clone.
    if req.kind == arugula_proto::BlockType::Forge {
        // M37: a new issue says who asked for it; under an agent (the CLI's
        // header) it's a draft a person sends.
        if req.config["issue"] == "new" && req.config.is_object() {
            let by = who_is(&app, who.clone().unwrap_or(crate::acl::Principal::Owner)).await;
            req.config["by"] = serde_json::json!(by.map(|d| d.name));
            if crate::invite::agent(&headers) {
                req.config["agent"] = true.into();
            }
        }
        req.config = crate::forge::open_config(&req.config).await.map_err(bad)?;
    }
    // A pane on another daemon (#17) names a host in our list: that's
    // where clients look it up.
    if req.kind == arugula_proto::BlockType::Remote {
        let at = crate::remote::parse(&req.config).map_err(bad)?;
        let list = app.hosts.list();
        if at.host == list.this {
            return Err(bad(format!("{} is this daemon: its panes go in the layout as they are", at.host)));
        }
        if !list.hosts.iter().any(|h| h.name == at.host) {
            return Err(bad(format!("no host {} in this daemon's list", at.host)));
        }
    }
    match app.mux.api(|r| Api::Open(req, who, r)).await {
        Some(Ok(block)) => Ok(Json(OpenResponse { block })),
        Some(Err(e)) => Err(bad(e)),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ConversationsQuery {
    /// Everything: other sources, archived ones, ones whose folder is gone.
    #[serde(default, deserialize_with = "flag")]
    pub all: bool,
    /// Words in the title, prompts or folder.
    pub q: Option<String>,
    /// Under this folder.
    pub cwd: Option<String>,
    /// Only ones a process holds now.
    #[serde(default, deserialize_with = "flag")]
    pub live: bool,
    pub limit: Option<usize>,
}

/// A query flag: `1`, `true`, `yes` or empty (`?all`) are on.
fn flag<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let s = String::deserialize(d)?;
    Ok(matches!(s.as_str(), "" | "1" | "true" | "yes" | "on"))
}

/// Agent blocks by the Claude Code session they have.
async fn blocks_by_session(app: &App) -> HashMap<String, PaneId> {
    let mut out = HashMap::new();
    for p in app.mux.api(Api::Panes).await.unwrap_or_default() {
        if p.info.kind != arugula_proto::BlockType::Agent {
            continue;
        }
        if let Some(b) = app.mux.api(|r| Api::Block(p.info.id, r)).await.flatten()
            && let Some(sid) = b.config()["session_id"].as_str()
        {
            out.insert(sid.to_owned(), p.info.id);
        }
    }
    out
}

/// `GET /api/conversations` (M33): Claude Code conversations on this
/// machine, newest first, with the block that has each one open.
async fn conversations(State(app): AppState, Query(q): Query<ConversationsQuery>) -> Res<Json<serde_json::Value>> {
    list_conversations(&app, q).await.map(Json).map_err(bad)
}

/// Our terminals' and agent blocks' processes on this host (#81): a
/// Claude Code under one of them runs there.
async fn our_pids(app: &App) -> crate::conversations::Ours {
    let mut ours = crate::conversations::Ours::default();
    for p in app.mux.api(Api::Panes).await.unwrap_or_default() {
        let id = p.info.id;
        match p.info.kind {
            arugula_proto::BlockType::Terminal => {
                if let Some(pid) = app.mux.api(|r| Api::Pane(id, r)).await.flatten().and_then(|h| h.pid_now()) {
                    ours.panes.insert(pid, id);
                }
            }
            _ => {
                if let Some(pid) = app.mux.api(|r| Api::Block(id, r)).await.flatten().and_then(|b| b.pid()) {
                    ours.blocks.insert(pid, id);
                }
            }
        }
    }
    ours
}

pub async fn list_conversations(app: &App, q: ConversationsQuery) -> Result<serde_json::Value, String> {
    let blocks = blocks_by_session(app).await;
    let ours: std::collections::HashSet<PaneId> =
        app.mux.api(Api::Panes).await.unwrap_or_default().into_iter().map(|p| p.info.id).collect();
    let pids = our_pids(app).await;
    let list = tokio::task::spawn_blocking(move || {
        let mut ix = crate::conversations::Index::global().lock().unwrap();
        ix.set_ours(pids);
        ix.scan()
    })
    .await
    .map_err(|e| e.to_string())?;
    let words: Vec<String> = q.q.as_deref().unwrap_or("").split_whitespace().map(str::to_lowercase).collect();
    let total = list.len();
    let out: Vec<serde_json::Value> = list
        .into_iter()
        .filter(|c| crate::conversations::shown(c, q.all))
        .filter(|c| !q.live || c.live.is_some())
        .filter(|c| q.cwd.as_deref().is_none_or(|d| crate::paths::is_under(&c.cwd, d)))
        .filter(|c| {
            let hay = format!(
                "{} {} {} {}",
                c.title,
                c.first_prompt.as_deref().unwrap_or(""),
                c.last_prompt.as_deref().unwrap_or(""),
                c.cwd
            )
            .to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .take(q.limit.unwrap_or(500))
        .map(|mut c| {
            let block = blocks.get(&c.id).copied();
            c.live = c.live.take().map(|l| l.ours(|p| ours.contains(&p)));
            // The adapter of the block that has it, when its scope didn't
            // say (no systemd scopes).
            if let (Some(b), Some(l)) = (block, c.live.as_mut())
                && l.block.is_none()
                && l.entrypoint == "sdk-ts"
            {
                l.block = Some(b);
                l.pane = None;
                l.place = l.place();
            }
            let mut v = serde_json::to_value(&c).unwrap_or_default();
            v["block"] = serde_json::json!(block);
            v
        })
        .collect();
    Ok(serde_json::json!({ "conversations": out, "total": total }))
}

/// `POST /api/conversations/open` (M33): a conversation as an agent block,
/// stopped, showing its transcript; the block that already has it, if one
/// does. `then` continues or forks it.
async fn open_conversation(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Json(req): Json<OpenConversationRequest>,
) -> Res<Json<OpenConversationResponse>> {
    open_conversation_as(&app, who.map(|axum::Extension(w)| w), req).await.map(Json)
}

pub async fn open_conversation_as(
    app: &App,
    who: Option<crate::acl::Principal>,
    req: OpenConversationRequest,
) -> Result<OpenConversationResponse, ApiError> {
    let id = req.id.clone();
    let c = tokio::task::spawn_blocking(move || crate::conversations::Index::global().lock().unwrap().find(&id))
        .await
        .map_err(|e| bad(e.to_string()))?
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, e))?;
    let existing = blocks_by_session(app).await.get(&c.id).copied();
    let (block, opened) = match existing {
        Some(b) => (b, false),
        None => {
            let source = serde_json::to_value(c.source).unwrap_or_default();
            let config = serde_json::json!({
                "agent": "claude",
                "cwd": c.cwd,
                "session_id": c.id,
                "import": { "path": c.path, "source": source, "title": c.title, "model": c.model },
            });
            let open = arugula_proto::api::OpenRequest {
                kind: arugula_proto::BlockType::Agent,
                config,
                session: req.session,
                split: req.split,
                from_pane: req.from_pane,
                vm: false,
                image: None,
                host: None,
                local: true,
            };
            match app.mux.api(|r| Api::Open(open, who, r)).await {
                Some(Ok(b)) => (b, true),
                Some(Err(e)) => return Err(bad(e)),
                None => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
            }
        }
    };
    let mut out = OpenConversationResponse { block, opened, conversation: c.id, error: None };
    if let Some(then) = req.then.as_deref() {
        let method = match then {
            "continue" | "fork" => then,
            t => return Err(bad(format!("then: {t}? (continue or fork)"))),
        };
        let b = app.mux.api(|r| Api::Block(block, r)).await.flatten().ok_or_else(|| bad("the block went away"))?;
        if let Err(e) = b.call(method, serde_json::json!({})).await {
            out.error = Some(e);
        }
    }
    Ok(out)
}

/// `describe %N`: where a block is and what it's doing, for any type.
async fn describe(State(app): AppState, Path(id): Path<PaneId>) -> Res<Json<serde_json::Value>> {
    let info = app
        .mux
        .api(Api::Panes)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.info.id == id)
        .ok_or(ApiError(StatusCode::NOT_FOUND, format!("no block %{id}")))?;
    let state = match app.mux.api(|r| Api::Block(id, r)).await.flatten() {
        Some(b) => b.state(),
        None => {
            let st = pane(&app, id).await?.status();
            serde_json::json!({
                "cwd": st.cwd,
                "busy": st.busy,
                "end": st.end,
                "exited": st.exited,
                "current": st.current,
                "last": st.last,
            })
        }
    };
    Ok(Json(serde_json::json!({ "info": info, "state": state })))
}

/// `call %N METHOD [json]`: a block's own methods. Terminals answer `send`,
/// `keys` and `capture` the same way as their own routes.
async fn call(
    State(app): AppState,
    Path((id, method)): Path<(PaneId, String)>,
    who: Option<axum::Extension<crate::acl::Principal>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Res<Json<serde_json::Value>> {
    let mut args: serde_json::Value = if body.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice(&body).map_err(|e| bad(e.to_string()))?
    };
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    let owner = who.is_owner();
    let by = match method.as_str() {
        // M11: a file block's `open` is the owner's only. M36: a forge
        // block's writes say who sent them.
        "approve" | "deny" | "answer" | "decline" | "send" | "terminal" | "open" | "comment" | "review" | "merge"
        | "rerun_checks" => who_is(&app, who).await,
        _ => None,
    };
    if let Some(b) = app.mux.api(|r| Api::Block(id, r)).await.flatten() {
        // An agent's invites (#234) are the owner's, and not for an agent
        // on the owner's CLI either (as a forge's drafts, a courtesy).
        if b.kind() == arugula_proto::BlockType::Invite && (!owner || crate::invite::agent(&headers)) {
            return Err(ApiError(StatusCode::FORBIDDEN, crate::invite::OWNER_ONLY.into()));
        }
        // The CLI says when an agent runs it (CLAUDECODE, AI_AGENT): a forge
        // block makes its writes drafts then (M36). A courtesy, not a
        // boundary.
        if b.kind() == arugula_proto::BlockType::Forge
            && crate::invite::agent(&headers)
            && let Some(o) = args.as_object_mut()
        {
            o.insert("agent".into(), true.into());
        }
        // A question raised on the block (M35) is answered where it waits.
        // (A studio box's gate is approved by the block: `{key}`.)
        let gate = method == "approve" && ["key", "member"].iter().any(|k| args.get(*k).is_some());
        let answering = !gate && matches!(method.as_str(), "answer" | "decline" | "terminal" | "approve" | "deny");
        if !(answering && app.mux.api(|r| Api::Holds(id, r)).await.unwrap_or(false)) {
            return block_call(&app, id, &b, &method, args, by).await.map(Json).map_err(bad);
        }
        return answer_terminal(&app, id, &method, args, by).await;
    }
    let p = pane(&app, id).await?;
    match method.as_str() {
        "send" => {
            let req: SendRequest = serde_json::from_value(args).map_err(|e| bad(e.to_string()))?;
            p.mark_input();
            let mut data = req.text.into_bytes();
            if req.enter {
                data.push(b'\r');
            }
            app.mux.send(Cmd::Input { client: None, pane: id, data });
            Ok(Json(serde_json::json!({})))
        }
        "keys" => {
            let req: KeysRequest = serde_json::from_value(args).map_err(|e| bad(e.to_string()))?;
            let modes = p.status().modes;
            let data: Vec<u8> = req.keys.iter().flat_map(|k| keys::key(k, modes)).collect();
            p.mark_input();
            app.mux.send(Cmd::Input { client: None, pane: id, data });
            Ok(Json(serde_json::json!({})))
        }
        "capture" => {
            let text = tokio::task::spawn_blocking(move || p.capture(CaptureFormat::Text, CaptureScope::Screen))
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            Ok(Json(serde_json::json!({ "text": text })))
        }
        "answer" | "decline" | "terminal" | "approve" | "deny" => answer_terminal(&app, id, &method, args, by).await,
        m => Err(bad(crate::block::no_method(arugula_proto::BlockType::Terminal, m))),
    }
}

async fn machines(State(app): AppState) -> Res<Json<Vec<arugula_proto::Machine>>> {
    Ok(Json(app.mux.api(Api::Machines).await.unwrap_or_default()))
}

/// Finds a VM pane's shell by the tag in its environment (a session leader
/// carrying `ARUGULA_EXEC=$1`, or `ILLOGICAL_EXEC=$1` from an older daemon,
/// #505) and prints: its pid, the foreground
/// process's pid, comm, exe, cwd, and argv separated by \x1f.
const GUEST_PROCESS: &str = r#"
for d in /proc/[0-9]*; do
  p=${d#/proc/}
  tr '\0' '\n' <"$d/environ" 2>/dev/null | grep -qx -e "ARUGULA_EXEC=$1" -e "ILLOGICAL_EXEC=$1" || continue
  st=$(sed 's/^.*) //' "$d/stat" 2>/dev/null) || continue
  set -- "$1" $st
  [ "$5" = "$p" ] || continue
  f=$7; [ "$f" -gt 0 ] 2>/dev/null || f=$p
  printf '%s\n%s\n' "$p" "$f"
  cat "/proc/$f/comm"
  readlink "/proc/$f/exe" || echo
  readlink "/proc/$f/cwd" || echo
  tr '\0' '\037' <"/proc/$f/cmdline"; echo
  exit 0
done
exit 1
"#;

async fn reset_machine(State(app): AppState, Path(id): Path<u32>) -> Res<Json<serde_json::Value>> {
    match app.mux.api(|r| Api::ResetMachine(id, r)).await {
        Some(Ok(())) => Ok(Json(serde_json::json!({}))),
        Some(Err(e)) => Err(ApiError(StatusCode::NOT_FOUND, e)),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }
}

async fn share_machine(State(app): AppState, Path(id): Path<PaneId>) -> Res<Json<serde_json::Value>> {
    match app.mux.api(|r| Api::ShareMachine(id, r)).await {
        Some(Ok(())) => Ok(Json(serde_json::json!({}))),
        Some(Err(e)) => Err(ApiError(StatusCode::CONFLICT, e)),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }
}

async fn guest_process(app: &App, pane: PaneId, machine: &arugula_proto::Machine) -> Res<Json<Process>> {
    let unavailable = |why: String| ApiError(StatusCode::SERVICE_UNAVAILABLE, why);
    let provider = app.mux.provider.clone().ok_or_else(|| unavailable("VM panes aren't set up".into()))?;
    let tag = crate::mux::exec_tag(&app.mux.daemon_id, pane);
    let argv = ["bash", "-c", GUEST_PROCESS, "arugula-process", &tag];
    let (out, code) =
        provider.run(&machine.sprite, &argv).await.map_err(|e| unavailable(format!("unavailable: {e}")))?;
    let text = String::from_utf8_lossy(&out);
    let lines: Vec<&str> = text.lines().collect();
    if code != Some(0) || lines.len() < 6 {
        return Err(ApiError(StatusCode::CONFLICT, "nothing is running in that pane".into()));
    }
    let some = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    Ok(Json(Process {
        pid: lines[0].parse().unwrap_or(0),
        foreground: lines[1].parse().unwrap_or(0),
        comm: lines[2].to_owned(),
        exe: some(lines[3]),
        cwd: some(lines[4]),
        argv: lines[5].split('\x1f').filter(|a| !a.is_empty()).map(str::to_owned).collect(),
    }))
}

async fn process(State(app): AppState, Path(id): Path<PaneId>) -> Res<Json<Process>> {
    let p = pane(&app, id).await?;
    // On a machine: ask it (its processes aren't ours to read).
    if let Some(Some(m)) = app.mux.api(|r| Api::MachineOf(id, r)).await {
        return guest_process(&app, id, &m).await;
    }
    let pid = p.pid_now().ok_or_else(|| ApiError(StatusCode::CONFLICT, "nothing is running in that pane".into()))?;
    use crate::procinfo;
    let tpgid = procinfo::foreground(pid).unwrap_or(pid);
    Ok(Json(Process {
        pid,
        foreground: tpgid,
        comm: procinfo::comm(tpgid).unwrap_or_default(),
        argv: procinfo::argv(tpgid).unwrap_or_default(),
        exe: procinfo::exe(tpgid).map(|p| p.display().to_string()),
        cwd: procinfo::cwd(tpgid).map(|p| p.display().to_string()),
    }))
}

/// How the screen of the agent in a pane reads, rule by rule (#145,
/// `arugula describe %N --detection`): `{agent: null, command}` when no
/// agent's screen is read there.
async fn detection(State(app): AppState, Path(id): Path<PaneId>) -> Res<Json<serde_json::Value>> {
    let p = pane(&app, id).await?;
    let (found, command) = tokio::task::spawn_blocking(move || (p.detection(), p.command()))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(match found {
        // Not read here: what chant found configured instead.
        Some(d) if d.unread => {
            let mut v = serde_json::to_value(d).unwrap_or_default();
            v["configured"] = serde_json::json!(app.mux.inventory.snapshot().runtimes());
            v
        }
        Some(d) => serde_json::to_value(d).unwrap_or_default(),
        None => serde_json::json!({ "agent": null, "command": command }),
    }))
}

#[derive(Deserialize)]
struct TailQuery {
    /// An offset, or `last-command`. Default: the last 64 KB.
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    follow: Option<u8>,
    /// Stop at this offset (not with `follow`): the TUI's copy mode reads
    /// the history before what it has (M32).
    #[serde(default)]
    until: Option<u64>,
    /// Strip escape sequences.
    #[serde(default)]
    text: Option<u8>,
    /// A pane of another host, from its synced history.
    #[serde(default)]
    host: Option<String>,
}

/// A closed pane's output, from its retired log: no following, and offsets
/// only (no command marks).
fn tail_closed(app: &App, id: PaneId, q: &TailQuery) -> Res<Response> {
    let dir = app
        .mux
        .store
        .pane_dirs()
        .into_iter()
        // Just closed, it may not have been moved to `closed/` yet.
        .find(|(p, _, _)| *p == id)
        .map(|(_, _, d)| d)
        .ok_or(ApiError(StatusCode::NOT_FOUND, format!("no pane %{id}")))?;
    let log = PaneLog::open(dir).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let from = match q.from.as_deref() {
        None => log.end().saturating_sub(64 * 1024),
        Some(n) => n.parse().map_err(|_| bad(format!("pane %{id} is closed: from takes an offset")))?,
    };
    let (_, bytes) = log.read_from(from).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(if q.text == Some(1) { strip(&bytes).into_bytes() } else { bytes }.into_response())
}

/// A block that isn't a terminal: its text, then (following) what it adds.
/// Text that changes in place (a tool call finishing) is printed again from
/// the first line that changed.
fn tail_block(app: Arc<App>, id: PaneId, b: Arc<dyn crate::block::Block>, follow: bool) -> Response {
    let first = b.text();
    if !follow {
        return first.into_response();
    }
    drop(b);
    let live = stream::unfold((app, first.clone()), move |(app, mut seen)| async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let b = app.mux.api(|r| Api::Block(id, r)).await.flatten()?;
            let now = b.text();
            if now == seen {
                continue;
            }
            // From the start of the first line that differs.
            let same = seen.bytes().zip(now.bytes()).take_while(|(a, b)| a == b).count();
            let from = now[..same].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let out = now[from..].to_owned();
            seen = now;
            return Some((Ok::<_, Infallible>(Bytes::from(out)), (app, seen)));
        }
    });
    Body::from_stream(stream::once(async move { Ok::<_, Infallible>(Bytes::from(first)) }).chain(live)).into_response()
}

/// `until=idle` (whatever it's doing, it's not working any more) or
/// `until=needs-input`, for any block. An agent's own state says this as
/// soon as a call returns; others go by the daemon's attention.
async fn wait_attention(app: &App, id: PaneId, needs_input: bool) -> Res<WaitResult> {
    use arugula_proto::Attention;
    use arugula_proto::ask::Ask;
    loop {
        let block = app.mux.api(|r| Api::Block(id, r)).await.flatten().map(|b| b.state());
        let found = block.and_then(|s| {
            let a = serde_json::from_value::<Attention>(s["attention"].clone()).ok()?;
            // The question it waits on: the first one not already opened.
            let ask = s["asks"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|a| a["accepted"] != true)
                .and_then(|a| serde_json::from_value::<Ask>(a.clone()).ok());
            Some((a, ask))
        });
        let (state, ask) = match found {
            Some(f) => f,
            None => {
                let summaries = app.mux.api(Api::Panes).await.unwrap_or_default();
                match summaries.into_iter().find(|p| p.info.id == id) {
                    Some(p) => (p.info.attention, p.info.ask),
                    None => return Err(ApiError(StatusCode::GONE, format!("%{id} closed"))),
                }
            }
        };
        let done = if needs_input { state == Attention::NeedsInput } else { state != Attention::Working };
        if done {
            let ask = ask.filter(|_| state == Attention::NeedsInput);
            return Ok(WaitResult::Attention { state, ask: ask.map(Box::new) });
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn tail(State(app): AppState, Path(id): Path<PaneId>, Query(q): Query<TailQuery>) -> Res<Response> {
    if let Some(host) = q.host.clone() {
        return tail_synced(&app, id, host, &q).await;
    }
    if let Some(b) = app.mux.api(|r| Api::Block(id, r)).await.flatten() {
        return Ok(tail_block(app.clone(), id, b, q.follow == Some(1)));
    }
    let p = match pane(&app, id).await {
        Ok(p) => p,
        // Closed: what it left behind.
        Err(ApiError(StatusCode::NOT_FOUND, _)) => return tail_closed(&app, id, &q),
        Err(e) => return Err(e),
    };
    let status = p.status();
    let from = match q.from.as_deref() {
        None => status.end.saturating_sub(64 * 1024),
        Some("last-command") => status
            .current
            .as_ref()
            .or(status.last.as_ref())
            .map(|c| c.start)
            .ok_or_else(|| bad("no command recorded in that pane (is shell integration on?)"))?,
        Some(n) => n.parse().map_err(|_| bad("from: an offset or last-command"))?,
    };
    // Up to the last command's end, unless following.
    let follow = q.follow == Some(1);
    let text = q.text == Some(1);
    let until = match (q.from.as_deref(), follow) {
        (_, false) if q.until.is_some() => q.until,
        (Some("last-command"), false) => status.current.is_none().then(|| status.last.and_then(|l| l.end)).flatten(),
        _ => None,
    };
    let (start, mut bytes) = read_log(&app, id, from);
    if let Some(end) = until {
        bytes.truncate(end.saturating_sub(start) as usize);
    }
    let resume = start + bytes.len() as u64;
    let first = if text { strip(&bytes).into_bytes() } else { bytes };
    if !follow {
        return Ok(first.into_response());
    }
    let tap = tap(p, resume);
    let live = stream::unfold(tap, move |mut tap| async move {
        let (_, data) = tap.next().await?;
        let out = if text { strip(&data).into_bytes() } else { data };
        Some((Ok::<_, Infallible>(Bytes::from(out)), tap))
    });
    let body = stream::once(async move { Ok::<_, Infallible>(Bytes::from(first)) }).chain(live);
    Ok(Body::from_stream(body).into_response())
}

#[derive(Deserialize)]
struct WaitQuery {
    until: String,
    #[serde(default)]
    re: Option<String>,
    /// Seconds; default forever.
    #[serde(default)]
    timeout: Option<f64>,
}

async fn wait(State(app): AppState, Path(id): Path<PaneId>, Query(q): Query<WaitQuery>) -> Res<Json<WaitResult>> {
    let limit = q.timeout.map(Duration::from_secs_f64).unwrap_or(Duration::from_secs(365 * 24 * 3600));
    if q.until == "idle" || q.until == "needs-input" {
        let result = tokio::time::timeout(limit, wait_attention(&app, id, q.until == "needs-input")).await;
        return Ok(Json(match result {
            Ok(r) => r?,
            Err(_) => WaitResult::Timeout,
        }));
    }
    let p = pane(&app, id).await?;
    let result = tokio::time::timeout(limit, wait_for(&app, id, p, &q)).await;
    Ok(Json(match result {
        Ok(r) => r?,
        Err(_) => WaitResult::Timeout,
    }))
}

/// What happened after the last input sent to the pane (so `send` then
/// `wait` never misses a command that finished in between).
async fn wait_for(app: &App, id: PaneId, p: PaneHandle, q: &WaitQuery) -> Res<WaitResult> {
    let mut events = app.mux.events();
    let status = p.status();
    let since = status.input_at;
    match q.until.as_str() {
        "command-end" => {
            if status.current.is_none()
                && let Some(l) = status.last.filter(|l| l.start >= since)
            {
                return Ok(WaitResult::CommandEnd { text: l.text, exit: l.exit, start: l.start, end: l.end });
            }
            loop {
                let Ok(e) = events.recv().await else { continue };
                if e.pane == Some(id) && matches!(e.kind, EventKind::CommandEnd { .. }) {
                    let l = p.status().last.unwrap_or_default();
                    return Ok(WaitResult::CommandEnd { text: l.text, exit: l.exit, start: l.start, end: l.end });
                }
                if e.pane == Some(id) && matches!(e.kind, EventKind::Closed) {
                    return Err(ApiError(StatusCode::GONE, format!("pane %{id} closed")));
                }
            }
        }
        "exit" => {
            if let Some(code) = status.exited {
                return Ok(WaitResult::Exit { code });
            }
            loop {
                let Ok(e) = events.recv().await else { continue };
                if e.pane == Some(id)
                    && let EventKind::Exit { code, .. } = e.kind
                {
                    return Ok(WaitResult::Exit { code });
                }
            }
        }
        "match" => {
            let re = Regex::new(q.re.as_deref().ok_or_else(|| bad("match needs re="))?)
                .map_err(|e| bad(format!("re: {e}")))?;
            let mut tap = tap(p, status.end);
            let (start, bytes) = read_log(app, id, since);
            let mut seen = strip(&bytes);
            let mut base = start;
            loop {
                if let Some(m) = re.find(&seen) {
                    return Ok(WaitResult::Match { text: m.as_str().to_owned(), offset: base + m.start() as u64 });
                }
                // Keep a tail so matches across chunk boundaries are found.
                if seen.len() > 1 << 20 {
                    let cut = seen.len() - (1 << 16);
                    let cut = (cut..seen.len()).find(|i| seen.is_char_boundary(*i)).unwrap_or(cut);
                    base += cut as u64;
                    seen.drain(..cut);
                }
                let Some((_, data)) = tap.next().await else {
                    return Err(ApiError(StatusCode::GONE, format!("pane %{id} closed")));
                };
                seen.push_str(&strip(&data));
            }
        }
        u => Err(bad(format!("until {u}: command-end, exit, match, idle or needs-input"))),
    }
}

/// `wait` for MCP (M16): the same waits, with no limit of their own (the
/// caller has one) and the error as a sentence.
pub(crate) async fn wait_until(app: &App, id: PaneId, until: &str, re: Option<String>) -> Result<WaitResult, String> {
    if until == "idle" || until == "needs-input" {
        return wait_attention(app, id, until == "needs-input").await.map_err(|e| e.1);
    }
    let p = pane(app, id).await.map_err(|e| e.1)?;
    let q = WaitQuery { until: until.to_owned(), re, timeout: None };
    wait_for(app, id, p, &q).await.map_err(|e| e.1)
}

/// Allow, deny, answer or skip what a pane asks, as `by` (MCP's
/// `agent_respond`, M16).
pub(crate) async fn act_as(
    app: &App,
    pane: PaneId,
    req: &arugula_proto::api::ActRequest,
    by: Driver,
) -> Result<(), String> {
    act_one(app, pane, req, Some(by)).await
}

async fn export(State(app): AppState, Path(id): Path<PaneId>) -> Res<Response> {
    let dir = app
        .mux
        .store
        .pane_dirs()
        .into_iter()
        .find(|(p, _, _)| *p == id)
        .map(|(_, _, d)| d)
        .ok_or(ApiError(StatusCode::NOT_FOUND, format!("no history for pane %{id}")))?;
    let cast = tokio::task::spawn_blocking(move || history::export_cast(&dir, &format!("Arugula pane %{id}")))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(([(header::CONTENT_TYPE, "application/x-asciicast")], cast).into_response())
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(default)]
    pane: Option<PaneId>,
    /// Comma-separated event types.
    #[serde(rename = "type", default)]
    types: Option<String>,
    #[serde(default)]
    follow: Option<u8>,
    /// Without follow: how far back, in seconds (default an hour).
    #[serde(default)]
    since: Option<u64>,
}

fn event_type(kind: &EventKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v["type"].as_str().map(str::to_owned)).unwrap_or_default()
}

async fn events(State(app): AppState, Query(q): Query<EventsQuery>) -> Res<Response> {
    let types: Option<Vec<String>> = q.types.map(|t| t.split(',').map(|s| s.trim().to_owned()).collect());
    let keep = move |e: &arugula_proto::Event| {
        q.pane.is_none_or(|p| e.pane == Some(p))
            && types.as_ref().is_none_or(|t| t.iter().any(|t| *t == event_type(&e.kind)))
    };
    let line = |e: &arugula_proto::Event| {
        let mut s = serde_json::to_string(e).unwrap_or_default();
        s.push('\n');
        Bytes::from(s)
    };
    if q.follow != Some(1) {
        let since = now_ms().saturating_sub(q.since.unwrap_or(3600) * 1000);
        let store = app.mux.store.clone();
        let mut all: Vec<arugula_proto::Event> = tokio::task::spawn_blocking(move || {
            store
                .pane_dirs()
                .into_iter()
                .filter(|(_, open, _)| *open)
                .flat_map(|(id, _, dir)| history::stored_events(&dir, id, since))
                .collect()
        })
        .await
        .unwrap_or_default();
        all.retain(&keep);
        all.sort_by_key(|e| e.at_ms);
        let body: Vec<u8> = all.iter().flat_map(|e| line(e).to_vec()).collect();
        return Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], body).into_response());
    }
    let rx = app.mux.events();
    let s = stream::unfold(rx, move |mut rx| {
        let keep = keep.clone();
        async move {
            loop {
                match rx.recv().await {
                    Ok(e) if keep(&e) => return Some((Ok::<_, Infallible>(line(&e)), rx)),
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return None,
                }
            }
        }
    });
    Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], Body::from_stream(s)).into_response())
}

#[derive(Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    pane: Option<PaneId>,
    #[serde(default)]
    failed: Option<u8>,
    /// Seconds back.
    #[serde(default)]
    since: Option<u64>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(rename = "match", default)]
    matching: Option<String>,
    /// `command`, `answer` or `agent`; none is all.
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Another host's synced history (`*`: every host's).
    #[serde(default)]
    host: Option<String>,
}

/// Things that look like credentials, for warning before sharing (M14):
/// common token shapes. A heuristic; it says so where it's shown.
fn secret_kinds() -> &'static [(&'static str, Regex)] {
    static KINDS: std::sync::OnceLock<Vec<(&'static str, Regex)>> = std::sync::OnceLock::new();
    KINDS.get_or_init(|| {
        [
            ("a GitHub token", r"\b(gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{40,})"),
            ("an Anthropic key", r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
            ("an API key", r"\bsk-[A-Za-z0-9]{32,}"),
            ("an AWS key", r"\b(AKIA|ASIA)[0-9A-Z]{16}\b"),
            ("a Slack token", r"\bxox[abprs]-[A-Za-z0-9-]{10,}"),
            ("a private key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
            ("a JWT", r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"),
            ("a password", r"(?i)\b(password|passwd|secret)\s*[:=]\s*\S{6,}"),
        ]
        .into_iter()
        .map(|(k, re)| (k, Regex::new(re).expect("secret pattern")))
        .collect()
    })
}

pub fn find_secrets(text: &str) -> Vec<&'static str> {
    secret_kinds().iter().filter(|(_, re)| re.is_match(text)).map(|(k, _)| *k).collect()
}

/// Panes of a session whose recent output looks like it holds a secret.
async fn secrets(State(app): AppState, Path(id): Path<SessionId>) -> Res<Response> {
    let panes = app
        .mux
        .api(|r| Api::SessionEnds(id, r))
        .await
        .flatten()
        .ok_or(ApiError(StatusCode::NOT_FOUND, format!("no session ${id}")))?;
    let mut found = Vec::new();
    for p in panes.keys() {
        let Ok(h) = pane(&app, *p).await else { continue };
        let text = tokio::task::spawn_blocking(move || h.capture(CaptureFormat::Text, CaptureScope::Scrollback))
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        // Recent output: the last 300 lines.
        let recent: Vec<&str> = text.lines().rev().take(300).collect();
        let kinds = find_secrets(&recent.join("\n"));
        if !kinds.is_empty() {
            found.push(serde_json::json!({ "pane": p, "kinds": kinds }));
        }
    }
    Ok(Json(found).into_response())
}

/// Who typed in a pane, by handoff (M13).
async fn drivers(State(app): AppState, Path(id): Path<PaneId>) -> Res<Response> {
    let dir = app.mux.store.pane_dir(id);
    let list = tokio::task::spawn_blocking(move || history::drivers(&dir)).await.unwrap_or_default();
    Ok(Json(list).into_response())
}

async fn history_(State(app): AppState, Query(q): Query<HistoryQuery>) -> Res<Response> {
    let matching = q.matching.as_deref().map(Regex::new).transpose().map_err(|e| bad(format!("match: {e}")))?;
    let kind = q
        .kind
        .as_deref()
        .map(|k| HistoryKind::parse(k).ok_or_else(|| bad(format!("kind: {k:?} isn't command, answer or agent"))))
        .transpose()?;
    let filter = Filter {
        pane: q.pane,
        failed: q.failed == Some(1),
        kind,
        since_ms: q.since.map(|s| now_ms().saturating_sub(s * 1000)),
        cwd: q.cwd,
        matching,
    };
    let store = app.mux.store.clone();
    let limit = q.limit.unwrap_or(100);
    let synced = app.synced.clone();
    let list = tokio::task::spawn_blocking(move || match &q.host {
        Some(host) => synced.history(host, &filter, limit),
        None => history::history(&store, &filter, limit),
    })
    .await
    .unwrap_or_default();
    Ok(Json(list).into_response())
}

#[derive(Deserialize)]
struct SearchQuery {
    re: String,
    #[serde(default)]
    since: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
    /// Another host's synced history (`*`: every host's).
    #[serde(default)]
    host: Option<String>,
}

async fn search(State(app): AppState, Query(q): Query<SearchQuery>) -> Res<Response> {
    let re = Regex::new(&q.re).map_err(|e| bad(format!("re: {e}")))?;
    let since = q.since.map(|s| now_ms().saturating_sub(s * 1000));
    let store = app.mux.store.clone();
    let limit = q.limit.unwrap_or(100);
    let synced = app.synced.clone();
    let hits = tokio::task::spawn_blocking(move || match &q.host {
        Some(host) => synced.search(host, &re, since, limit),
        None => history::search(&store, &re, since, limit),
    })
    .await
    .unwrap_or_default();
    Ok(Json(hits).into_response())
}

/// A pane synced from another host: its output from an offset (default the
/// last 64 KB), no following.
async fn tail_synced(app: &App, id: PaneId, host: String, q: &TailQuery) -> Res<Response> {
    let synced = app.synced.clone();
    let from = match q.from.as_deref() {
        None => None,
        Some(n) => Some(n.parse::<u64>().map_err(|_| bad("a synced pane's from takes an offset"))?),
    };
    let read = tokio::task::spawn_blocking(move || {
        let from = from.unwrap_or_else(|| synced.pane(&host, id).log_end.saturating_sub(64 * 1024));
        synced.read_from(&host, id, from)
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let (_, bytes) = read.map_err(|e| ApiError(StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(if q.text == Some(1) { strip(&bytes).into_bytes() } else { bytes }.into_response())
}

async fn push_key(State(app): AppState) -> Res<Json<HashMap<&'static str, String>>> {
    let push = app.push.as_ref().ok_or(ApiError(StatusCode::NOT_FOUND, "push is off".into()))?;
    Ok(Json(HashMap::from([("key", push.public_key())])))
}

async fn push_subscribe(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Json(mut sub): Json<Subscription>,
) -> Res<Json<serde_json::Value>> {
    let push = app.push.as_ref().ok_or(ApiError(StatusCode::NOT_FOUND, "push is off".into()))?;
    // Anyone with access here may subscribe (M29); a subscription is its
    // subscriber's, never someone else's.
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    if !who.is_owner() && !app.acl.knows(&who) {
        return Err(ApiError(StatusCode::FORBIDDEN, "you have no access here".into()));
    }
    sub.who = (!who.is_owner()).then(|| who.id().to_owned());
    push.subscribe(sub).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "subscriptions": push.subscriptions() })))
}

/// What "needs you" notifications you get here (M29): `GET` yours, `POST`
/// to opt in or out of a session's agents, or all of them.
async fn notify_get(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<NotifyPref>> {
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    if who.is_owner() {
        return Ok(Json(NotifyPref { all: true, ..Default::default() }));
    }
    Ok(Json(app.acl.notify_pref(who.id())))
}

async fn notify_set(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Json(req): Json<NotifyRequest>,
) -> Res<Json<NotifyPref>> {
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    if who.is_owner() {
        return Err(bad("the owner is always told"));
    }
    match req.session {
        Some(s) if app.acl.role(&who, s).is_none_or(|r| r < arugula_core::Role::Editor) => {
            return Err(ApiError(StatusCode::FORBIDDEN, "only people who may answer are told".into()));
        }
        None if !app.acl.knows(&who) => return Err(ApiError(StatusCode::FORBIDDEN, "you have no access here".into())),
        _ => {}
    }
    let pref = app
        .acl
        .set_notify(who.id(), req.session, req.on)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(pref))
}

async fn push_test(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<serde_json::Value>> {
    let push = app.push.as_ref().ok_or(ApiError(StatusCode::NOT_FOUND, "push is off".into()))?;
    let me = who.map(|axum::Extension(w)| w.id().to_owned()).unwrap_or_else(|| "owner".into());
    push.send_to(0, "Arugula", "Notifications work.", None, |w| w == me);
    Ok(Json(serde_json::json!({ "subscriptions": push.subscriptions() })))
}

/// #505: a machine's shell is found by its tag under either name (a pane
/// an older daemon started has `ILLOGICAL_EXEC`).
#[cfg(all(test, target_os = "linux"))]
mod guest_process_tests {
    #[test]
    fn a_shell_is_found_by_its_tag_under_either_name() {
        for name in ["ARUGULA_EXEC", "ILLOGICAL_EXEC"] {
            let tag = format!("t505-{name}-{}", std::process::id());
            let mut leader =
                std::process::Command::new("setsid").args(["sleep", "30"]).env(name, &tag).spawn().unwrap();
            let pid = leader.id().to_string();
            let found = (0..50).find_map(|_| {
                let out =
                    std::process::Command::new("bash").args(["-c", super::GUEST_PROCESS, "p", &tag]).output().unwrap();
                let first = String::from_utf8_lossy(&out.stdout).lines().next().map(str::to_owned);
                (first.as_deref() == Some(pid.as_str())).then_some(()).or_else(|| {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    None
                })
            });
            let _ = leader.kill();
            let _ = leader.wait();
            assert!(found.is_some(), "{name}");
        }
    }
}

#[cfg(test)]
mod secret_tests {
    #[test]
    fn token_shapes() {
        assert_eq!(super::find_secrets("export GH=ghp_0123456789abcdefghijABCDEFGHIJ012345"), ["a GitHub token"]); // gitleaks:allow (a made-up token)
        assert_eq!(super::find_secrets("key: sk-ant-api03-abcdefghijklmnopqrstuv"), ["an Anthropic key"]);
        assert!(super::find_secrets("AKIAIOSFODNN7EXAMPLE").contains(&"an AWS key"));
        assert!(super::find_secrets("-----BEGIN OPENSSH PRIVATE KEY-----").contains(&"a private key"));
        assert!(super::find_secrets("PASSWORD=hunter22").contains(&"a password"));
        assert!(super::find_secrets("cargo build --release\n   Compiling foo").is_empty());
    }
}

/// `GET /api/panes/{id}/diff` (M28): the edit a pane's diff card shows,
/// before and after, for changing it before accepting.
async fn diff_of(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Path(id): Path<PaneId>,
) -> Res<Json<serde_json::Value>> {
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    if !who.is_owner() && app.mux.api(|r| Api::RoleOn(who, id, r)).await.flatten().is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, format!("no pane %{id}")));
    }
    let (info, old, new) = app
        .mux
        .api(|r| Api::DiffOf(id, r))
        .await
        .flatten()
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("%{id} has no edit waiting")))?;
    Ok(Json(serde_json::json!({ "diff": info, "old": old, "new": new })))
}

/// Home and the environment an agent block gets.
pub(crate) async fn agent_env(app: &App) -> Res<(std::path::PathBuf, Vec<(String, String)>)> {
    let (home, env) = app
        .mux
        .api(Api::AgentEnv)
        .await
        .ok_or_else(|| ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into()))?;
    // #161: as an agent block gets it, with the user's shell environment.
    let shell = app.mux.shell_env.local().await;
    Ok((home, crate::shellenv::merge(&env, &shell, None)))
}

/// `GET /api/agents/adapters` (#111): whether Claude Code's and Codex's
/// adapters can start here, and the command that installs each.
async fn adapters(State(app): AppState) -> Res<Json<serde_json::Value>> {
    let (home, env) = agent_env(&app).await?;
    let list = tokio::task::spawn_blocking(move || crate::agent::adapters::all(&home, &env))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "adapters": list })))
}

#[derive(serde::Deserialize, Default)]
struct InstallAdapter {
    #[serde(default)]
    split: Option<PaneId>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    from_pane: Option<PaneId>,
}

/// `POST /api/agents/adapters/{kind}/install` (#111): its `npm install`, in
/// a new pane (beside `split`, else a tab) to watch.
async fn install_adapter(
    State(app): AppState,
    Path(kind): Path<String>,
    body: Option<Json<InstallAdapter>>,
) -> Res<Json<RunResponse>> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let kind: crate::agent::defs::Kind =
        serde_json::from_value(serde_json::json!(kind)).map_err(|_| bad(format!("no agent {kind}")))?;
    let a = crate::agent::adapters::of(kind).ok_or_else(|| bad("that agent has no adapter to install"))?;
    let (home, env) = agent_env(&app).await?;
    let (st, home) = tokio::task::spawn_blocking(move || (crate::agent::adapters::status(a, &home, &env), home))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if let crate::agent::adapters::State::NoNode { .. } = st.state {
        return Err(bad(st.why()));
    }
    let run = RunRequest {
        command: Some(crate::agent::adapters::install_command(&st, &home)),
        session: req.session,
        split: req.split,
        from_pane: req.from_pane.or(req.split),
        ..Default::default()
    };
    match app.mux.api(|r| Api::Run(run, r)).await {
        Some(Ok(pane)) => Ok(Json(RunResponse { pane })),
        Some(Err(e)) => Err(bad(e)),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }
}

/// `GET /api/ide` (M28): arugulad as Claude Code's IDE, and the other
/// IDEs registered beside it.
async fn ide_get(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<serde_json::Value>> {
    owner_only(&who)?;
    let Some(ide) = &app.mux.ide else {
        return Ok(Json(serde_json::json!({ "on": false })));
    };
    Ok(Json(serde_json::json!({
        "on": true,
        "name": crate::ide::NAME,
        "port": ide.port,
        "lock_dir": ide.lock_dir,
        "diffs": ide.diffs_to().unwrap_or_else(|| crate::ide::NAME.into()),
        "others": ide.others(),
    })))
}

#[derive(Deserialize)]
struct IdeSet {
    /// Which IDE gets diffs: `arugula`, or another's name.
    diffs: String,
}

/// `PUT /api/ide {"diffs": NAME}`: which IDE gets Claude Code's diffs.
async fn ide_set(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Json(req): Json<IdeSet>,
) -> Res<Json<serde_json::Value>> {
    owner_only(&who)?;
    let ide = app.mux.ide.as_ref().ok_or_else(|| bad("arugulad isn't Claude Code's IDE here (--no-claude-ide)"))?;
    ide.set_diffs_to(Some(req.diffs)).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "diffs": ide.diffs_to().unwrap_or_else(|| crate::ide::NAME.into()) })))
}

/// `GET /api/rules` (#166): the standing permission rules agent blocks on
/// this daemon answer from, in order (`DELETE /api/rules/{index}` forgets
/// one; `DELETE /api/rules`, all).
async fn rules_get(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<serde_json::Value>> {
    owner_only(&who)?;
    let list: Vec<serde_json::Value> = app
        .mux
        .rules
        .list()
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let text = r.describe();
            let mut v = serde_json::to_value(r).unwrap_or_default();
            v["index"] = i.into();
            v["text"] = text.into();
            v
        })
        .collect();
    Ok(Json(serde_json::json!({ "rules": list })))
}

async fn rules_forget(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Path(index): Path<usize>,
) -> Res<Json<serde_json::Value>> {
    owner_only(&who)?;
    app.mux.rules.forget(Some(index)).map_err(|e| ApiError(StatusCode::NOT_FOUND, e))?;
    Ok(Json(serde_json::json!({})))
}

async fn rules_forget_all(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<serde_json::Value>> {
    owner_only(&who)?;
    app.mux.rules.forget(None).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(serde_json::json!({})))
}

/// `GET /api/hosts/self/shell-env` (#74): the user's shell environment
/// blocks that run the user's tools get here, waiting for it if it's still
/// being resolved. Its `PATH` and the names of the rest.
async fn shell_env_get(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<serde_json::Value>> {
    owner_only(&who)?;
    let s = &app.mux.shell_env;
    let r = s.local().await;
    Ok(Json(serde_json::json!({
        "shell": s.shell(),
        "ok": r.error.is_none(),
        "error": r.error,
        "ms": r.took.as_millis() as u64,
        "path": r.get("PATH"),
        "vars": r.vars.iter().map(|(k, _)| k).collect::<Vec<_>>(),
    })))
}

/// `POST /api/hosts/self/shell-env/refresh` (#74): resolve it again (here,
/// and on each machine when next needed), after changing an rc file.
async fn shell_env_refresh(
    state: AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<serde_json::Value>> {
    owner_only(&who)?;
    state.0.mux.shell_env.refresh();
    shell_env_get(state, who).await
}

/// `GET /api/hosts/self/agents` (#145): the agents configured on this
/// machine, as `chant audit --agents` last found them, and which screen
/// rule sets run here because of it.
async fn agents_get(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<serde_json::Value>> {
    owner_only(&who)?;
    Ok(Json(agents_json(&app.mux.inventory.snapshot())))
}

/// `POST /api/hosts/self/agents/refresh`: ask chant again, and wait.
async fn agents_refresh(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
) -> Res<Json<serde_json::Value>> {
    owner_only(&who)?;
    Ok(Json(agents_json(&app.mux.inventory.refresh_now().await)))
}

fn agents_json(snap: &crate::inventory::Snapshot) -> serde_json::Value {
    let mut v = serde_json::to_value(snap).unwrap_or_default();
    let (runs, off): (Vec<_>, Vec<_>) = arugula_vt::detect::AGENTS.iter().map(|a| a.id).partition(|id| snap.runs(id));
    v["rules"] = serde_json::json!({ "run": runs, "off": off });
    v
}

fn owner_only(who: &Option<axum::Extension<crate::acl::Principal>>) -> Res<()> {
    match who {
        Some(axum::Extension(w)) if !w.is_owner() => Err(ApiError(StatusCode::FORBIDDEN, "only the owner can".into())),
        _ => Ok(()),
    }
}

/// `GET /api/editors` (M28): every editor in the swarm, joined or a block.
async fn editors(State(app): AppState, who: Option<axum::Extension<crate::acl::Principal>>) -> Json<serde_json::Value> {
    let who = who.map(|axum::Extension(w)| w).filter(|w| !w.is_owner());
    Json(app.mux.api(|r| Api::Editors(who, r)).await.unwrap_or_default().into())
}

#[derive(Deserialize)]
struct Mention {
    /// The terminal Claude Code runs in.
    pane: PaneId,
    file: String,
    /// Lines, from 1.
    start: u32,
    end: u32,
}

/// `POST /api/ide/mention` (M28): put `@file#Lstart-end` in Claude Code's
/// prompt in a pane, as an IDE does ("ask Claude about these lines" from a
/// followed editor). Typing there needs editor access.
async fn ide_mention(
    State(app): AppState,
    who: Option<axum::Extension<crate::acl::Principal>>,
    Json(m): Json<Mention>,
) -> Res<Json<serde_json::Value>> {
    let who = who.map(|axum::Extension(w)| w).unwrap_or(crate::acl::Principal::Owner);
    if !who.is_owner() {
        match app.mux.api(|r| Api::RoleOn(who, m.pane, r)).await.flatten() {
            Some((role, _)) if role >= arugula_core::Role::Editor => {}
            Some(_) => return Err(ApiError(StatusCode::FORBIDDEN, "you're watching that session".into())),
            None => return Err(ApiError(StatusCode::NOT_FOUND, format!("no pane %{}", m.pane))),
        }
    }
    let ide = app.mux.ide.as_ref().ok_or_else(|| bad("arugulad isn't Claude Code's IDE here"))?;
    let conns = app.mux.api(|r| Api::IdeConns(m.pane, r)).await.unwrap_or_default();
    if conns.is_empty() {
        return Err(ApiError(StatusCode::CONFLICT, format!("Claude Code in %{} isn't connected to Arugula", m.pane)));
    }
    // From 0, as VS Code's extension sends them.
    let params = serde_json::json!({
        "filePath": m.file, "lineStart": m.start.saturating_sub(1), "lineEnd": m.end.max(m.start).saturating_sub(1),
    });
    for c in &conns {
        ide.notify(Some(*c), "at_mentioned", params.clone());
    }
    Ok(Json(serde_json::json!({ "sent": conns.len() })))
}

/// `GET /api/editors/vsix` (M28): Arugula's VS Code extension.
async fn vsix() -> Response {
    let name = crate::editor::vsix::file_name();
    (
        [
            (header::CONTENT_TYPE, "application/vsix".to_owned()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\"")),
        ],
        crate::editor::vsix::build(),
    )
        .into_response()
}

/// A studio app block's config (M35) from `{app}` (and optionally `to`,
/// dropped: a deep link is the frame's, never kept): the box and studio
/// filled in from studio's list when not given.
pub async fn app_config(c: &serde_json::Value) -> Result<serde_json::Value, String> {
    let name = c["app"].as_str().filter(|n| !n.is_empty()).ok_or("an app block needs {\"app\": NAME}")?;
    let studio = crate::apps::studio::get().ok_or("no studio here")?;
    // With a follower link kept for the app, hud is told who answered,
    // wherever the block was opened from (the picker passes nothing).
    let follower = c["follower"].as_bool().unwrap_or_else(|| studio.follower(name).is_some());
    let mut out = serde_json::json!({ "app": name, "follower": follower });
    match (c["box_url"].as_str(), c["studio"].as_str()) {
        (Some(b), Some(s)) => {
            out["box_url"] = b.into();
            out["studio"] = s.into();
            if let Some(t) = c["title"].as_str() {
                out["title"] = t.into();
            }
        }
        _ => {
            let a = studio.app(name).await?;
            out["box_url"] = a.url.into();
            out["studio"] = studio.url().ok_or("not logged in to a studio")?.into();
            if let Some(t) = a.title {
                out["title"] = t.into();
            }
        }
    }
    Ok(out)
}

fn studio() -> Res<Arc<crate::apps::studio::Studio>> {
    crate::apps::studio::get().ok_or_else(|| ApiError(StatusCode::SERVICE_UNAVAILABLE, "no studio here".into()))
}

/// `GET /api/studio` (M35): which studio, and whether there's a token.
/// Never the token.
async fn studio_status() -> Res<Json<serde_json::Value>> {
    Ok(Json(studio()?.status()))
}

#[derive(Deserialize)]
struct StudioLogin {
    url: String,
    token: String,
}

/// `arugula studio login`: keep a studio token, once studio takes it.
async fn studio_login(Json(req): Json<StudioLogin>) -> Res<Json<serde_json::Value>> {
    let apps = studio()?.login(&req.url, &req.token).await.map_err(bad)?;
    Ok(Json(serde_json::json!({ "apps": apps })))
}

async fn studio_logout() -> Res<Json<serde_json::Value>> {
    studio()?.logout().map_err(bad)?;
    Ok(Json(serde_json::json!({})))
}

/// The person's apps, from studio, with the app blocks that show them.
#[derive(Debug, Default, Deserialize)]
pub struct FountainQuery {
    pub query: Option<String>,
    pub source: Option<String>,
    pub profile: Option<String>,
}

/// M43: the person's Fountain agents, read with their own login on this
/// host (`arugula fountain agents`): compact cards, filtered.
async fn fountain_agents(State(app): AppState, Query(q): Query<FountainQuery>) -> Res<Json<serde_json::Value>> {
    let mut f = crate::fountain::catalog::Filter::default();
    f.apply(&serde_json::json!({ "query": q.query, "source": q.source })).map_err(bad)?;
    let runner = crate::fountain::local_runner(&app.mux.shell_env).await;
    let got = crate::fountain::agents_for(&runner, q.profile.as_deref()).await.map_err(bad)?;
    let rows = crate::fountain::rows(&got.agents, &f);
    Ok(Json(serde_json::json!({
        "base_url": got.login.base_url, "profile": got.login.profile, "total": got.agents.len(), "filter": f,
        "agents": rows, "unreadable": got.unreadable,
    })))
}

async fn studio_apps(State(app): AppState) -> Res<Json<serde_json::Value>> {
    let s = studio()?;
    let apps = s.apps().await.map_err(bad)?;
    let mut blocks: HashMap<String, Vec<PaneId>> = HashMap::new();
    for p in app.mux.api(Api::Panes).await.unwrap_or_default() {
        if p.info.kind == arugula_proto::BlockType::App
            && let Some(b) = app.mux.api(|r| Api::Block(p.info.id, r)).await.flatten()
            && let Some(name) = b.config()["app"].as_str()
        {
            blocks.entry(name.to_owned()).or_default().push(p.info.id);
        }
    }
    let list: Vec<serde_json::Value> = apps
        .into_iter()
        .map(|a| {
            let mut v = serde_json::to_value(&a).unwrap_or_default();
            v["blocks"] = serde_json::json!(blocks.get(&a.name).cloned().unwrap_or_default());
            v
        })
        .collect();
    Ok(Json(serde_json::json!({ "studio": s.url(), "apps": list })))
}

#[derive(Deserialize)]
struct FollowerLink {
    link: String,
}

/// Keep a hud follower link for an app (`hud share --role follower` in
/// its box): app blocks with `follower` enter with it and name who
/// answered.
async fn studio_follower(Path(name): Path<String>, Json(req): Json<FollowerLink>) -> Res<Json<serde_json::Value>> {
    studio()?.set_follower(&name, Some(&req.link)).map_err(bad)?;
    Ok(Json(serde_json::json!({})))
}

async fn studio_unfollow(Path(name): Path<String>) -> Res<Json<serde_json::Value>> {
    studio()?.set_follower(&name, None).map_err(bad)?;
    Ok(Json(serde_json::json!({})))
}

/// ICE servers for a huddle (M63): TURN credentials from control, or STUN.
async fn turn(State(app): AppState) -> Json<serde_json::Value> {
    Json(app.control.ice_servers().await)
}
