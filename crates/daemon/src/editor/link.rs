//! Editors in the swarm (M28): VS Code, Cursor or nvim, wherever they run,
//! connected to the arugulad on their machine.
//!
//! An editor connects to the daemon's socket (`/api/editors/connect`, an
//! HTTP upgrade to lines of JSON both ways) once its workspace is opted in.
//! Under Remote-SSH that's the remote machine's daemon, since the extension
//! runs there. What it says follows S17's schema:
//!
//! - **`hello`** first: which editor, its remote, its workspace, and the
//!   editor block it is if it's one (M27).
//! - **`summary`**: what M23's summaries carry (the file, diagnostic counts,
//!   unsaved buffers, the debugger, a conflict), at most once a second and
//!   at once when something wants attention. A debugger stopping, errors
//!   showing up after a save, or a merge conflict opening becomes a reason
//!   on the rail (M24).
//! - **`peek`**: the lines around the cursor, for previews and `capture`.
//!   They stay in the daemon: they're read on request, never broadcast.
//! - **`follow`**, **`open`**, **`edit`**, **`diagnostics`**: the follow
//!   stream, sent only while someone follows (the daemon says how many with
//!   `followers`). It's content, so it goes only to the clients following,
//!   on their own (end-to-end) connections.
//!
//! The daemon sends `welcome` (the id it got), `followers`, `resend` (a new
//! follower needs the whole file again) and `continue` (the debugger).
//!
//! An editor that isn't an editor block becomes a presence of its own
//! (`presence.rs`): an entry beside the panes, with no tab and no PTY, which
//! goes when the editor disconnects.

use std::sync::{Arc, Mutex};

use arugula_proto::{Action, Attention, DebugState, Diag, EditorInfo, PaneId, Reason, ReasonKind};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};
use tracing::{debug, info};

use crate::{
    mux::Api,
    pane::{Notice, NoticeSink, What},
    server::App,
    store::now_ms,
};

/// The upgrade's protocol name.
pub const PROTOCOL: &str = "arugula-editor";
/// Its name before the rename (#505): an extension installed before the
/// update still asks for it until it's replaced, and the daemon answers in
/// the name it was asked for.
const OLD_PROTOCOL: &str = "illogical-editor";
/// The longest line an editor may send (a file opened for followers).
const MAX_LINE: usize = 4 << 20;
/// Edits kept after the last `open`, for a new follower; past this the
/// editor sends the file again.
const MAX_EDITS: usize = 2000;
/// At most this many lines around the cursor.
const MAX_PEEK: usize = 12;

/// What an editor says first.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Hello {
    /// `vscode`, `cursor`, `code-server`, `nvim`, ...
    pub editor: String,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub authority: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    /// Its first workspace folder.
    #[serde(default)]
    pub workspace: Option<String>,
    /// The editor block it is (M27), if it's one.
    #[serde(default)]
    pub block: Option<PaneId>,
}

/// The lines around an editor's cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Peek {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub top: Option<u32>,
    pub lines: Vec<String>,
    pub dirty: u32,
}

/// One connected editor.
pub struct Link {
    pub hello: Hello,
    out: mpsc::UnboundedSender<String>,
    st: Mutex<St>,
    /// An editor block's: told of each peek.
    on_peek: Mutex<Option<OnPeek>>,
}

type OnPeek = Box<dyn Fn(&Peek) + Send + Sync>;

#[derive(Default)]
struct St {
    /// Its pane id, and where notices about it go.
    bound: Option<(PaneId, NoticeSink)>,
    info: EditorInfo,
    /// The active file, whole.
    file: Option<String>,
    peek: Peek,
    // ---- the follow stream, as a new follower needs it
    cursor: Option<Value>,
    open: Option<Value>,
    edits: Vec<Value>,
    diags: Option<Value>,
    /// Edits were dropped: the next follower needs the file again.
    stale: bool,
    /// The reason it raised, while it stands.
    raised: Option<ReasonKind>,
}

impl Link {
    pub fn new(hello: Hello, out: mpsc::UnboundedSender<String>) -> Arc<Self> {
        let info = EditorInfo {
            app: hello.editor.clone(),
            remote: hello.remote.clone(),
            authority: hello.authority.clone(),
            hostname: hello.hostname.clone(),
            ..EditorInfo::default()
        };
        Arc::new(Self { hello, out, st: Mutex::new(St { info, ..St::default() }), on_peek: Mutex::new(None) })
    }

    /// It's pane `id` now: notices about it go to `sink`.
    pub fn bind(&self, id: PaneId, sink: NoticeSink) {
        self.st.lock().unwrap().bound = Some((id, sink));
        self.send(json!({ "t": "welcome", "id": id }));
    }

    pub fn on_peek(&self, f: impl Fn(&Peek) + Send + Sync + 'static) {
        *self.on_peek.lock().unwrap() = Some(Box::new(f));
    }

    pub fn id(&self) -> Option<PaneId> {
        self.st.lock().unwrap().bound.as_ref().map(|(id, _)| *id)
    }

    pub fn info(&self) -> EditorInfo {
        self.st.lock().unwrap().info.clone()
    }

    /// The active file, whole.
    pub fn file(&self) -> Option<String> {
        self.st.lock().unwrap().file.clone()
    }

    pub fn peek(&self) -> Peek {
        self.st.lock().unwrap().peek.clone()
    }

    /// Something for the editor.
    pub fn send(&self, msg: Value) {
        let _ = self.out.send(msg.to_string());
    }

    fn notice(&self, what: What) {
        if let Some((pane, sink)) = &self.st.lock().unwrap().bound {
            let _ = sink.send(Notice { pane: *pane, what });
        }
    }

    /// How many follow it now. It starts or stops its follow stream, and
    /// says so in its status bar.
    pub fn followers(&self, n: u32, new: bool) {
        let resend = {
            let mut st = self.st.lock().unwrap();
            if st.info.followers == n && !new {
                return;
            }
            st.info.followers = n;
            if n == 0 {
                // Nobody follows: it stops sending, so what's kept goes stale.
                (st.cursor, st.open, st.diags) = (None, None, None);
                st.edits.clear();
            }
            new && (st.stale || st.open.is_none())
        };
        self.send(json!({ "t": "followers", "n": n }));
        if resend {
            self.send(json!({ "t": "resend" }));
        }
        self.notice(What::BlockChanged);
    }

    /// What a new follower needs to draw it now: the file, the edits since,
    /// its diagnostics and the cursor.
    pub fn snapshot(&self) -> Vec<Value> {
        let st = self.st.lock().unwrap();
        let mut out = vec![];
        if !st.stale
            && let Some(o) = &st.open
        {
            out.push(o.clone());
            out.extend(st.edits.iter().cloned());
        }
        out.extend(st.diags.iter().cloned());
        out.extend(st.cursor.iter().cloned());
        out
    }

    /// The debugger: run on.
    pub fn resume(&self) -> Result<(), String> {
        if self.st.lock().unwrap().info.debug.as_ref().is_none_or(|d| d.state != "paused") {
            return Err("its debugger isn't paused".into());
        }
        self.send(json!({ "t": "continue" }));
        Ok(())
    }

    /// A line from the editor.
    pub fn handle(&self, msg: Value) {
        match msg["t"].as_str().unwrap_or("") {
            "summary" => self.summary(&msg),
            "peek" => self.peeked(&msg),
            "follow" => {
                let mut m = msg;
                strip(&mut m);
                self.st.lock().unwrap().cursor = Some(m.clone());
                self.notice(What::Follow(m));
            }
            "open" => {
                let mut m = msg;
                strip(&mut m);
                let m = json!({ "open": m });
                {
                    let mut st = self.st.lock().unwrap();
                    st.open = Some(m.clone());
                    st.edits.clear();
                    st.stale = false;
                }
                self.notice(What::Follow(m));
            }
            "edit" => {
                let mut m = msg;
                strip(&mut m);
                let m = json!({ "edit": m });
                {
                    let mut st = self.st.lock().unwrap();
                    if st.edits.len() >= MAX_EDITS {
                        st.edits.clear();
                        st.stale = true;
                    } else if !st.stale {
                        st.edits.push(m.clone());
                    }
                }
                self.notice(What::Follow(m));
            }
            "diagnostics" => {
                let mut m = msg;
                strip(&mut m);
                let m = json!({ "diagnostics": m });
                self.st.lock().unwrap().diags = Some(m.clone());
                self.notice(What::Follow(m));
            }
            other => debug!(kind = other, "editor sent something unknown"),
        }
    }

    /// What summaries carry. Fields left out stay as they were; `null`
    /// clears one.
    fn summary(&self, m: &Value) {
        let has = |k: &str| m.as_object().is_some_and(|o| o.contains_key(k));
        let (raise, clear) = {
            let mut st = self.st.lock().unwrap();
            let before = st.info.clone();
            if has("file") {
                st.file = m["file"].as_str().map(str::to_owned);
            }
            if has("diag") {
                st.info.diag = serde_json::from_value::<Diag>(m["diag"].clone()).unwrap_or_default();
            }
            if has("dirty") {
                st.info.dirty = m["dirty"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32;
            }
            if has("debug") {
                st.info.debug = serde_json::from_value::<Option<DebugState>>(m["debug"].clone()).ok().flatten();
            }
            if has("conflict") {
                st.info.conflict = m["conflict"].as_str().map(str::to_owned);
            }
            let saved = m["saved"].as_bool().unwrap_or(false);
            let (raise, clear) = attention(&before, &st.info, saved, st.raised, st.file.as_deref());
            if let Some((_, r)) = &raise {
                st.raised = Some(r.kind);
            }
            if clear.is_some() {
                st.raised = None;
            }
            (raise, clear)
        };
        if let Some(kind) = clear {
            self.notice(What::Clear(kind));
        }
        if let Some((state, reason)) = raise {
            self.notice(What::Reason(state, reason));
        }
        self.notice(What::BlockChanged);
    }

    fn peeked(&self, m: &Value) {
        let n = |k: &str| m[k].as_u64().map(|v| v.min(u32::MAX as u64) as u32);
        let file = m["file"].as_str().map(str::to_owned);
        let peek = Peek {
            line: n("line").filter(|_| file.is_some()),
            col: n("col").filter(|_| file.is_some()),
            top: n("top").filter(|_| file.is_some()),
            lines: m["lines"]
                .as_array()
                .filter(|_| file.is_some())
                .map(|a| {
                    a.iter().take(MAX_PEEK).filter_map(|l| l.as_str()).map(|l| l.chars().take(240).collect()).collect()
                })
                .unwrap_or_default(),
            dirty: n("dirty").unwrap_or(0),
            file,
        };
        {
            let mut st = self.st.lock().unwrap();
            if st.peek == peek {
                return;
            }
            st.peek = peek.clone();
            if peek.file.is_some() {
                st.file = peek.file.clone();
            }
            st.info.dirty = peek.dirty;
        }
        if let Some(f) = &*self.on_peek.lock().unwrap() {
            f(&peek);
        }
        self.notice(What::BlockChanged);
    }

    /// It went away: anyone following hears so.
    fn gone(&self) {
        self.notice(What::Follow(json!({ "gone": true })));
    }
}

/// Drop the message's type: followers get the rest.
fn strip(m: &mut Value) {
    if let Some(o) = m.as_object_mut() {
        o.remove("t");
    }
}

/// What a summary's change means for attention: a reason to raise, and one
/// to clear (the one raised before, when it no longer holds).
fn attention(
    before: &EditorInfo,
    now: &EditorInfo,
    saved: bool,
    raised: Option<ReasonKind>,
    file: Option<&str>,
) -> (Option<(Attention, Reason)>, Option<ReasonKind>) {
    let paused = |i: &EditorInfo| i.debug.as_ref().is_some_and(|d| d.state == "paused");
    let short = |f: &str| f.rsplit('/').next().unwrap_or(f).to_owned();
    let reason = |kind, headline: String, actions: Vec<Action>| Reason {
        kind,
        since_ms: now_ms(),
        headline,
        command: None,
        exit: None,
        duration_ms: None,
        bundle: None,
        ask: None,
        gate: None,
        actions,
    };
    // What stopped holding.
    let clear = match raised {
        Some(ReasonKind::Paused) if !paused(now) => Some(ReasonKind::Paused),
        Some(ReasonKind::Errors) if now.diag.e == 0 => Some(ReasonKind::Errors),
        Some(ReasonKind::Conflict) if now.conflict.is_none() => Some(ReasonKind::Conflict),
        _ => None,
    };
    // What started.
    let raise = if paused(now) && (!paused(before) || before.debug != now.debug) {
        let d = now.debug.as_ref().unwrap();
        let at = match (&d.file, d.line) {
            (Some(f), Some(l)) => format!(" at {}:{l}", short(f)),
            (Some(f), None) => format!(" in {}", short(f)),
            _ => String::new(),
        };
        let why = d.reason.as_deref().filter(|r| !r.is_empty()).map(|r| format!(" ({r})")).unwrap_or_default();
        Some((
            Attention::NeedsInput,
            reason(ReasonKind::Paused, format!("Paused{at}{why}"), vec![Action::Continue, Action::Dismiss]),
        ))
    } else if now.conflict.is_some() && now.conflict != before.conflict {
        let f = short(now.conflict.as_deref().unwrap());
        Some((
            Attention::NeedsInput,
            reason(ReasonKind::Conflict, format!("Merge conflict in {f}"), vec![Action::Dismiss]),
        ))
    } else if saved && before.diag.e == 0 && now.diag.e > 0 {
        let n = now.diag.e;
        let after = file.map(|f| format!(" after saving {}", short(f))).unwrap_or_default();
        let what = if n == 1 { "1 error".to_owned() } else { format!("{n} errors") };
        Some((Attention::NeedsInput, reason(ReasonKind::Errors, format!("{what}{after}"), vec![Action::Dismiss])))
    } else {
        None
    };
    let clear = clear.filter(|k| raise.as_ref().is_none_or(|(_, r)| r.kind != *k));
    (raise, clear)
}

/// `GET /api/editors/connect` on the daemon's socket, upgraded to lines of
/// JSON. Only local: an editor talks to the daemon on its own machine.
pub async fn connect(State(app): State<Arc<App>>, mut req: Request) -> Response {
    let Some(protocol) = req.headers().get(header::UPGRADE).and_then(|v| v.to_str().ok()).and_then(protocol) else {
        return (StatusCode::BAD_REQUEST, format!("upgrade to {PROTOCOL}")).into_response();
    };
    let upgrade = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        match upgrade.await {
            Ok(io) => serve(app, TokioIo::new(io)).await,
            Err(e) => debug!(error = %e, "editor upgrade failed"),
        }
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, protocol)
        .body(Body::empty())
        .unwrap()
}

/// The protocol an `Upgrade` header asks for (the first we know of a list),
/// under either name.
fn protocol(upgrade: &str) -> Option<&'static str> {
    upgrade
        .split(',')
        .map(str::trim)
        .find_map(|p| [PROTOCOL, OLD_PROTOCOL].into_iter().find(|ours| p.eq_ignore_ascii_case(ours)))
}

async fn serve<S>(app: Arc<App>, io: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (r, mut w) = tokio::io::split(io);
    let mut lines = BufReader::new(r);
    let Some(first) = read_line(&mut lines).await else { return };
    let Ok(hello) = serde_json::from_value::<Hello>(first.clone()).map_err(|e| debug!(error = %e, "bad editor hello"))
    else {
        return;
    };
    if first["t"] != "hello" || hello.editor.is_empty() {
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let link = Link::new(hello.clone(), tx);
    // An editor block's window (M27), or an editor of its own.
    let block = match hello.block {
        Some(id) => app.mux.api(|r| Api::Block(id, r)).await.flatten().filter(|b| b.attach(link.clone())),
        None => None,
    };
    let id = match &block {
        Some(_) => link.id(),
        None => app.mux.api(|r| Api::EditorJoin(link.clone(), r)).await,
    };
    let Some(id) = id else { return };
    info!(pane = id, editor = hello.editor, remote = hello.remote.as_deref().unwrap_or(""), "editor joined");
    loop {
        tokio::select! {
            line = read_line(&mut lines) => match line {
                Some(v) => link.handle(v),
                None => break,
            },
            Some(out) = rx.recv() => {
                if w.write_all(format!("{out}\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    }
    link.gone();
    match block {
        Some(b) => b.detach(&link),
        None => app.mux.send(crate::mux::Cmd::Api(Api::EditorLeave(id))),
    }
    info!(pane = id, "editor left");
}

/// The next JSON line; `None` at the end (or past [`MAX_LINE`]).
async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Option<Value> {
    loop {
        let mut buf = Vec::new();
        let n = (&mut *r).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf).await.ok()?;
        if n == 0 || buf.len() > MAX_LINE {
            return None;
        }
        if buf.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice(&buf) {
            Ok(v) => return Some(v),
            Err(e) => debug!(error = %e, "bad line from an editor"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(f: impl FnOnce(&mut EditorInfo)) -> EditorInfo {
        let mut i = EditorInfo { app: "vscode".into(), ..Default::default() };
        f(&mut i);
        i
    }

    #[test]
    fn a_debugger_stopping_asks_and_running_clears() {
        let idle = info(|_| {});
        let paused = info(|i| {
            i.debug = Some(DebugState {
                state: "paused".into(),
                reason: Some("breakpoint".into()),
                file: Some("/r/src/app.ts".into()),
                line: Some(12),
            })
        });
        let (raise, clear) = attention(&idle, &paused, false, None, None);
        let (state, r) = raise.unwrap();
        assert_eq!(state, Attention::NeedsInput);
        assert_eq!(r.kind, ReasonKind::Paused);
        assert_eq!(r.headline, "Paused at app.ts:12 (breakpoint)");
        assert_eq!(r.actions, vec![Action::Continue, Action::Dismiss]);
        assert_eq!(clear, None);
        let running = info(|i| i.debug = Some(DebugState { state: "running".into(), ..Default::default() }));
        assert_eq!(
            attention(&paused, &running, false, Some(ReasonKind::Paused), None),
            (None, Some(ReasonKind::Paused))
        );
    }

    #[test]
    fn errors_count_only_when_a_save_brings_them() {
        let clean = info(|_| {});
        let broken = info(|i| i.diag = Diag { e: 2, w: 1, i: 0 });
        // Typing: the language server finds errors mid-word. Not yet.
        assert_eq!(attention(&clean, &broken, false, None, Some("/r/a.rs")).0, None);
        let (_, r) = attention(&clean, &broken, true, None, Some("/r/a.rs")).0.unwrap();
        assert_eq!((r.kind, r.headline.as_str()), (ReasonKind::Errors, "2 errors after saving a.rs"));
        // Already broken before: no news.
        assert_eq!(attention(&broken, &broken, true, None, None).0, None);
        assert_eq!(attention(&broken, &clean, true, Some(ReasonKind::Errors), None), (None, Some(ReasonKind::Errors)));
    }

    #[test]
    fn a_conflict_opening_asks() {
        let clean = info(|_| {});
        let conflict = info(|i| i.conflict = Some("/r/src/lib.rs".into()));
        let (_, r) = attention(&clean, &conflict, false, None, None).0.unwrap();
        assert_eq!((r.kind, r.headline.as_str()), (ReasonKind::Conflict, "Merge conflict in lib.rs"));
        assert_eq!(attention(&conflict, &clean, false, Some(ReasonKind::Conflict), None).1, Some(ReasonKind::Conflict));
    }

    #[test]
    fn the_upgrade_takes_either_name() {
        assert_eq!(protocol("arugula-editor"), Some(PROTOCOL));
        assert_eq!(protocol("Arugula-Editor"), Some(PROTOCOL));
        // An extension from before the rename (#505).
        assert_eq!(protocol("illogical-editor"), Some(OLD_PROTOCOL));
        assert_eq!(protocol("websocket, illogical-editor"), Some(OLD_PROTOCOL));
        assert_eq!(protocol("arugula-editor, illogical-editor"), Some(PROTOCOL));
        assert_eq!(protocol("websocket"), None);
        assert_eq!(protocol(""), None);
    }

    #[tokio::test]
    async fn followers_get_the_file_then_its_edits_then_the_cursor() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let link = Link::new(Hello { editor: "nvim".into(), ..Default::default() }, tx);
        link.handle(json!({ "t": "open", "file": "/r/a.rs", "version": 1, "text": "fn main() {}\n" }));
        link.handle(json!({ "t": "edit", "file": "/r/a.rs", "version": 2, "changes": [{ "range": [0, 0, 0, 0], "text": "x" }] }));
        link.handle(json!({ "t": "follow", "file": "/r/a.rs", "line": 1, "col": 2 }));
        let snap = link.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0]["open"]["text"], "fn main() {}\n");
        assert_eq!(snap[1]["edit"]["version"], 2);
        assert_eq!(snap[2], json!({ "file": "/r/a.rs", "line": 1, "col": 2 }));
        // The first follower: the editor starts its stream.
        link.followers(1, true);
        assert_eq!(rx.recv().await.unwrap(), r#"{"n":1,"t":"followers"}"#);
        assert_eq!(link.info().followers, 1);
    }
}
