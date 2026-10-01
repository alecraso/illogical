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
    mux::{Api, Cmd},
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
        .route("/api/panes/{id}/close", post(close))
        .route("/api/panes/{id}/capture", get(capture))
        .route("/api/panes/{id}/process", get(process))
        .route("/api/panes/{id}/tail", get(tail))
        .route("/api/panes/{id}/wait", get(wait))
        .route("/api/panes/{id}/export.cast", get(export))
        .route("/api/machines", get(machines))
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

async fn guest_process(app: &App, machine: &illogical_proto::Machine) -> Res<Json<Process>> {
    let unavailable = |why: String| ApiError(StatusCode::SERVICE_UNAVAILABLE, why);
    let wisp = app.mux.wisp.clone().ok_or_else(|| unavailable("VM panes aren't set up".into()))?;
    let tag = format!("{}-{}", app.mux.daemon_id, machine.id);
    let argv = ["bash", "-c", GUEST_PROCESS, "illogical-process", &tag];
    let (out, code) = wisp.run(&machine.sprite, &argv).await.map_err(|e| unavailable(format!("unavailable: {e}")))?;
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
        return guest_process(&app, &m).await;
    }
    let pid = p.pid_now().ok_or_else(|| ApiError(StatusCode::CONFLICT, "nothing is running in that pane".into()))?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let tpgid: u32 = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(5)?.parse::<i32>().ok())
        .filter(|t| *t > 0)
        .map(|t| t as u32)
        .unwrap_or(pid);
    let argv: Vec<String> = std::fs::read(format!("/proc/{tpgid}/cmdline"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    let link = |what: &str| std::fs::read_link(format!("/proc/{tpgid}/{what}")).ok().map(|p| p.display().to_string());
    Ok(Json(Process {
        pid,
        foreground: tpgid,
        comm: std::fs::read_to_string(format!("/proc/{tpgid}/comm")).unwrap_or_default().trim().to_owned(),
        argv,
        exe: link("exe"),
        cwd: link("cwd"),
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

async fn tail(State(app): AppState, Path(id): Path<PaneId>, Query(q): Query<TailQuery>) -> Res<Response> {
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
    let p = pane(&app, id).await?;
    let limit = q.timeout.map(Duration::from_secs_f64).unwrap_or(Duration::from_secs(365 * 24 * 3600));
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
        u => Err(bad(format!("until {u}: command-end, exit or match"))),
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
    let list = tokio::task::spawn_blocking(move || history::history(&store, &filter, limit)).await.unwrap_or_default();
    Ok(Json(list).into_response())
}

#[derive(Deserialize)]
struct SearchQuery {
    re: String,
    #[serde(default)]
    since: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

async fn search(State(app): AppState, Query(q): Query<SearchQuery>) -> Res<Response> {
    let re = Regex::new(&q.re).map_err(|e| bad(format!("re: {e}")))?;
    let since = q.since.map(|s| now_ms().saturating_sub(s * 1000));
    let store = app.mux.store.clone();
    let limit = q.limit.unwrap_or(100);
    let hits =
        tokio::task::spawn_blocking(move || history::search(&store, &re, since, limit)).await.unwrap_or_default();
    Ok(Json(hits).into_response())
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
    push.send(0, "illogical", "Notifications work.");
    Ok(Json(serde_json::json!({ "subscriptions": push.subscriptions() })))
}
