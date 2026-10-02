//! The HTTP API (see `illogical_proto::api` for the routes and shapes). The
//! `illogical` CLI uses it over the Unix socket; remote agents can use it
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

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream::{self, StreamExt};
use illogical_proto::{
    EventKind, Frame, FrameKind, PaneId,
    api::{AttentionRequest, KeysRequest, MouseRequest, Process, RunRequest, RunResponse, SendRequest, WaitResult},
};
use regex::Regex;
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{
    history::{self, Filter},
    keys,
    mux::{Api, AskReply, Cmd, MuxHandle},
    osc::strip,
    pane::{CaptureFormat, CaptureScope, PaneHandle, Subscriber, ToClient},
    push::Subscription,
    server::App,
    store::{PaneLog, now_ms},
};

type AppState = State<Arc<App>>;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/panes", get(panes))
        .route("/api/run", post(run))
        .route("/api/panes/{id}/send", post(send))
        .route("/api/panes/{id}/keys", post(keys_))
        .route("/api/panes/{id}/mouse", post(mouse))
        .route("/api/panes/{id}/attention", post(attention))
        .route("/api/panes/{id}/ask", post(ask))
        .route("/api/panes/{id}/ask/withdraw", post(ask_withdraw))
        .route("/api/panes/{id}/close", post(close))
        .route("/api/panes/{id}/capture", get(capture))
        .route("/api/panes/{id}/process", get(process))
        .route("/api/panes/{id}/tail", get(tail))
        .route("/api/panes/{id}/wait", get(wait))
        .route("/api/panes/{id}/export.cast", get(export))
        .route("/api/blocks", post(open_block))
        .route("/api/blocks/{id}", get(describe))
        .route("/api/blocks/{id}/call/{method}", post(call))
        .route("/api/machines", get(machines))
        .route("/api/machines/{id}/reset", post(reset_machine))
        .route("/api/panes/{id}/share-machine", post(share_machine))
        .route("/api/events", get(events))
        .route("/api/history", get(history_))
        .route("/api/search", get(search))
        .route("/api/push/key", get(push_key))
        .route("/api/push/subscribe", post(push_subscribe))
        .route("/api/push/test", post(push_test))
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

type Res<T> = Result<T, ApiError>;

async fn pane(app: &App, id: PaneId) -> Res<PaneHandle> {
    app.mux.api(|r| Api::Pane(id, r)).await.flatten().ok_or(ApiError(StatusCode::NOT_FOUND, format!("no pane %{id}")))
}

/// A subscriber of our own, for streaming a pane's output; detaches when
/// dropped (the HTTP client went away).
struct Tap {
    pane: PaneHandle,
    client: u64,
    rx: mpsc::Receiver<ToClient>,
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
    let (data, rx) = mpsc::channel(crate::pane::CLIENT_QUEUE);
    let (ctrl, _ctrl) = mpsc::unbounded_channel();
    pane.attach(Subscriber { client, data, ctrl }, Some(from));
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
                ToClient::Msg(_) => {}
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
    app.mux.send(Cmd::Input { pane: id, data });
    Ok(Json(serde_json::json!({})))
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
    app.mux.send(Cmd::Input { pane: id, data });
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
    app.mux.send(Cmd::Input { pane: id, data });
    Ok(Json(serde_json::json!({})))
}

async fn attention(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(req): Json<AttentionRequest>,
) -> Res<Json<serde_json::Value>> {
    match app.mux.api(|r| Api::Attention(id, req.state, r)).await {
        Some(true) => Ok(Json(serde_json::json!({}))),
        _ => Err(ApiError(StatusCode::NOT_FOUND, format!("no pane %{id}"))),
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

/// `illogical ask`: show AskUserQuestion's questions beside a terminal and
/// wait for the answer. Answers `{action: accept, content, output}` (the
/// hook's output for Claude Code), `{action: decline, output}`,
/// `{action: terminal}` (answer in the terminal) or `{action: withdrawn}`.
async fn ask(
    State(app): AppState,
    Path(id): Path<PaneId>,
    Json(req): Json<AskRequest>,
) -> Res<Json<serde_json::Value>> {
    use illogical_proto::ask::{self, Ask, AskKind};
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
        source: "hook".into(),
        at_ms: now_ms(),
    };
    let (token, rx) = match app.mux.api(|r| Api::Ask(id, a, r)).await {
        Some(Ok(r)) => r,
        Some(Err(e)) => return Err(ApiError(StatusCode::NOT_FOUND, e)),
        None => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    };
    let mut guard = AskGuard { mux: app.mux.clone(), pane: id, token, armed: true };
    let reply = rx.await;
    guard.armed = false;
    Ok(Json(match reply {
        Ok(AskReply::Answer(content)) => {
            let output = ask::hook_output(&req.questions, &content);
            serde_json::json!({ "action": "accept", "content": content, "output": output })
        }
        Ok(AskReply::Decline) => serde_json::json!({ "action": "decline", "output": ask::hook_declined() }),
        Ok(AskReply::Terminal) => serde_json::json!({ "action": "terminal" }),
        Ok(AskReply::Withdrawn) => serde_json::json!({ "action": "withdrawn" }),
        // The daemon is going away; the asker asks the next one.
        Err(_) => return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }))
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
    app.mux.send(Cmd::Api(Api::AskWithdraw(id, req.id, None)));
    Ok(Json(serde_json::json!({})))
}

/// A terminal's question answered by `call %N answer|decline|terminal`.
async fn answer_terminal(app: &App, id: PaneId, method: &str, args: serde_json::Value) -> Res<Json<serde_json::Value>> {
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
        _ => AskReply::Terminal,
    };
    match app.mux.api(|r| Api::AskReply(id, ask_id, reply, r)).await {
        Some(Ok(a)) => Ok(Json(serde_json::json!({ "answered": a.id }))),
        Some(Err(e)) => Err(bad(e)),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }
}

async fn close(State(app): AppState, Path(id): Path<PaneId>) -> Res<Json<serde_json::Value>> {
    match app.mux.api(|r| Api::Close(id, r)).await {
        Some(true) => Ok(Json(serde_json::json!({}))),
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
    Json(req): Json<illogical_proto::api::OpenRequest>,
) -> Res<Json<serde_json::Value>> {
    match app.mux.api(|r| Api::Open(req, r)).await {
        Some(Ok(block)) => Ok(Json(serde_json::json!({ "block": block }))),
        Some(Err(e)) => Err(bad(e)),
        None => Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "daemon is shutting down".into())),
    }
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
    body: axum::body::Bytes,
) -> Res<Json<serde_json::Value>> {
    let args: serde_json::Value = if body.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_slice(&body).map_err(|e| bad(e.to_string()))?
    };
    if let Some(b) = app.mux.api(|r| Api::Block(id, r)).await.flatten() {
        return b.call(&method, args).await.map(Json).map_err(bad);
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
            app.mux.send(Cmd::Input { pane: id, data });
            Ok(Json(serde_json::json!({})))
        }
        "keys" => {
            let req: KeysRequest = serde_json::from_value(args).map_err(|e| bad(e.to_string()))?;
            let modes = p.status().modes;
            let data: Vec<u8> = req.keys.iter().flat_map(|k| keys::key(k, modes)).collect();
            p.mark_input();
            app.mux.send(Cmd::Input { pane: id, data });
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
        "answer" | "decline" | "terminal" => answer_terminal(&app, id, &method, args).await,
        m => Err(bad(crate::block::no_method(illogical_proto::BlockType::Terminal, m))),
    }
}

async fn machines(State(app): AppState) -> Res<Json<Vec<illogical_proto::Machine>>> {
    Ok(Json(app.mux.api(Api::Machines).await.unwrap_or_default()))
}

/// Finds a VM pane's shell by the tag in its environment (a session leader
/// carrying `ILLOGICAL_EXEC=$1`) and prints: its pid, the foreground
/// process's pid, comm, exe, cwd, and argv separated by \x1f.
const GUEST_PROCESS: &str = r#"
for d in /proc/[0-9]*; do
  p=${d#/proc/}
  tr '\0' '\n' <"$d/environ" 2>/dev/null | grep -qx "ILLOGICAL_EXEC=$1" || continue
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

async fn guest_process(app: &App, pane: PaneId, machine: &illogical_proto::Machine) -> Res<Json<Process>> {
    let unavailable = |why: String| ApiError(StatusCode::SERVICE_UNAVAILABLE, why);
    let provider = app.mux.provider.clone().ok_or_else(|| unavailable("VM panes aren't set up".into()))?;
    let tag = crate::mux::exec_tag(&app.mux.daemon_id, pane);
    let argv = ["bash", "-c", GUEST_PROCESS, "illogical-process", &tag];
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

#[derive(Deserialize)]
struct TailQuery {
    /// An offset, or `last-command`. Default: the last 64 KB.
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    follow: Option<u8>,
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
    use illogical_proto::Attention;
    use illogical_proto::ask::Ask;
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
            return Ok(WaitResult::Attention { state, ask });
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

async fn export(State(app): AppState, Path(id): Path<PaneId>) -> Res<Response> {
    let dir = app
        .mux
        .store
        .pane_dirs()
        .into_iter()
        .find(|(p, _, _)| *p == id)
        .map(|(_, _, d)| d)
        .ok_or(ApiError(StatusCode::NOT_FOUND, format!("no history for pane %{id}")))?;
    let cast = tokio::task::spawn_blocking(move || history::export_cast(&dir, &format!("illogical pane %{id}")))
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
    let keep = move |e: &illogical_proto::Event| {
        q.pane.is_none_or(|p| e.pane == Some(p))
            && types.as_ref().is_none_or(|t| t.iter().any(|t| *t == event_type(&e.kind)))
    };
    let line = |e: &illogical_proto::Event| {
        let mut s = serde_json::to_string(e).unwrap_or_default();
        s.push('\n');
        Bytes::from(s)
    };
    if q.follow != Some(1) {
        let since = now_ms().saturating_sub(q.since.unwrap_or(3600) * 1000);
        let store = app.mux.store.clone();
        let mut all: Vec<illogical_proto::Event> = tokio::task::spawn_blocking(move || {
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
    #[serde(default)]
    limit: Option<usize>,
    /// Another host's synced history (`*`: every host's).
    #[serde(default)]
    host: Option<String>,
}

async fn history_(State(app): AppState, Query(q): Query<HistoryQuery>) -> Res<Response> {
    let matching = q.matching.as_deref().map(Regex::new).transpose().map_err(|e| bad(format!("match: {e}")))?;
    let filter = Filter {
        pane: q.pane,
        failed: q.failed == Some(1),
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

async fn push_subscribe(State(app): AppState, Json(sub): Json<Subscription>) -> Res<Json<serde_json::Value>> {
    let push = app.push.as_ref().ok_or(ApiError(StatusCode::NOT_FOUND, "push is off".into()))?;
    push.subscribe(sub).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "subscriptions": push.subscriptions() })))
}

async fn push_test(State(app): AppState) -> Res<Json<serde_json::Value>> {
    let push = app.push.as_ref().ok_or(ApiError(StatusCode::NOT_FOUND, "push is off".into()))?;
    push.send(0, "illogical", "Notifications work.", None);
    Ok(Json(serde_json::json!({ "subscriptions": push.subscriptions() })))
}
