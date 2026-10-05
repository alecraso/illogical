//! Read-only share links (M4c): `illogical share %N` (or *Share read-only
//! link…* on a pane) mints a token that shows one terminal pane, live, to
//! whoever opens `/share/<token>`: its snapshot and then its output, and
//! nothing else. No input, no size claims, no layout, no other pane, no
//! API. Links expire (an hour by default, a week at most), can be revoked,
//! and are listed; the daemon keeps only each token's hash, in
//! `shares.json`.
//!
//! **Who can open one.** The tailnet only: any tailnet *user* the ACL lets
//! reach this daemon (the owner, or someone the node is shared with), or a
//! local process. Never a tagged node (sandboxes run untrusted agents),
//! Funnel, or anything off the tailnet (see `Access::check_viewer`). The
//! link is a credential, so the page asks browsers not to send it on as a
//! referrer. A viewer's identity opens the viewer page, its WebSocket and
//! the page's static files; everything else still needs the owner.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::{
        Path, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use illogical_proto::{
    BlockType, EventKind, Frame, FrameKind, PaneId, ServerMsg,
    api::{Share, ShareRequest},
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{
    hosts::digest,
    mux::Api,
    pane::{Subscriber, ToClient, client_queue},
    server::App,
    store::{now_ms, write_atomic},
};

const DEFAULT_TTL_SECS: u64 = 3600;
const MAX_TTL_SECS: u64 = 7 * 24 * 3600;
/// Path prefix of everything a viewer reaches.
pub const PREFIX: &str = "/share/";
/// WebSocket close code for "you may not do that" (RFC 6455's policy
/// violation).
const POLICY: u16 = 1008;

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    id: u32,
    digest: String,
    pane: PaneId,
    created_ms: u64,
    expires_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct SavedShares {
    next: u32,
    shares: Vec<Saved>,
}

pub struct Shares {
    path: PathBuf,
    inner: Mutex<SavedShares>,
}

fn random_token() -> String {
    let b = crate::push::random::<24>();
    format!("ils_{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}

impl Shares {
    pub fn open(state_dir: &std::path::Path) -> Arc<Self> {
        let path = state_dir.join("shares.json");
        let saved = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Arc::new(Self { path, inner: Mutex::new(saved) })
    }

    fn save(&self, s: &SavedShares) {
        let bytes = serde_json::to_vec_pretty(s).expect("serialize shares");
        if let Err(e) = write_atomic(&self.path, &bytes) {
            warn!(error = %e, "can't save shares");
        }
    }

    fn prune(&self, s: &mut SavedShares) {
        let now = now_ms();
        let before = s.shares.len();
        s.shares.retain(|x| x.expires_ms > now);
        if s.shares.len() != before {
            self.save(s);
        }
    }

    /// A new link to `pane`, good for `ttl_secs`.
    pub fn mint(&self, pane: PaneId, ttl_secs: u64) -> Share {
        let token = random_token();
        let created_ms = now_ms();
        let expires_ms = created_ms + ttl_secs.min(MAX_TTL_SECS) * 1000;
        let mut s = self.inner.lock().unwrap();
        self.prune(&mut s);
        s.next += 1;
        let id = s.next;
        s.shares.push(Saved { id, digest: digest(&token), pane, created_ms, expires_ms });
        self.save(&s);
        info!(id, pane, "share minted");
        Share {
            id,
            pane,
            created_ms,
            expires_ms,
            path: Some(format!("{PREFIX}{token}")),
            token: Some(token),
            url: None,
        }
    }

    pub fn list(&self) -> Vec<Share> {
        let mut s = self.inner.lock().unwrap();
        self.prune(&mut s);
        s.shares
            .iter()
            .map(|x| Share {
                id: x.id,
                pane: x.pane,
                created_ms: x.created_ms,
                expires_ms: x.expires_ms,
                token: None,
                path: None,
                url: None,
            })
            .collect()
    }

    pub fn revoke(&self, id: u32) -> bool {
        let mut s = self.inner.lock().unwrap();
        let before = s.shares.len();
        s.shares.retain(|x| x.id != id);
        let gone = s.shares.len() != before;
        if gone {
            self.save(&s);
            info!(id, "share revoked");
        }
        gone
    }

    /// The share a token opens, if it is current: (id, pane, expiry).
    pub fn lookup(&self, token: &str) -> Option<(u32, PaneId, u64)> {
        let want = digest(token);
        let now = now_ms();
        let s = self.inner.lock().unwrap();
        s.shares.iter().find(|x| x.digest == want && x.expires_ms > now).map(|x| (x.id, x.pane, x.expires_ms))
    }

    /// Whether share `id` is still good (not revoked or expired).
    pub fn live(&self, id: u32) -> bool {
        let now = now_ms();
        self.inner.lock().unwrap().shares.iter().any(|x| x.id == id && x.expires_ms > now)
    }
}

// ---------------------------------------------------------------- routes

type AppState = State<Arc<App>>;

/// The owner's: minting, listing, revoking.
pub fn api_routes() -> Router<Arc<App>> {
    Router::new().route("/api/shares", get(list).post(mint)).route("/api/shares/{id}", delete(revoke))
}

/// A viewer's: the page and its WebSocket.
pub fn viewer_routes() -> Router<Arc<App>> {
    Router::new().route("/share/{token}", get(page)).route("/share/{token}/ws", get(ws))
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

async fn mint(State(app): AppState, Json(req): Json<ShareRequest>) -> Response {
    let summaries = app.mux.api(Api::Panes).await.unwrap_or_default();
    match summaries.iter().find(|p| p.info.id == req.pane) {
        None => return error(StatusCode::NOT_FOUND, format!("no pane %{}", req.pane)),
        Some(p) if p.info.kind != BlockType::Terminal => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("%{} isn't a terminal; only terminals can be shared", req.pane),
            );
        }
        Some(_) => {}
    }
    let mut share = app.shares.mint(req.pane, req.ttl_secs.unwrap_or(DEFAULT_TTL_SECS).max(1));
    share.url = share.path.as_ref().map(|p| format!("{}{p}", app.access.page_origin()));
    Json(share).into_response()
}

async fn list(State(app): AppState) -> Json<Vec<Share>> {
    Json(app.shares.list())
}

async fn revoke(State(app): AppState, Path(id): Path<u32>) -> Response {
    if app.shares.revoke(id) {
        Json(serde_json::json!({})).into_response()
    } else {
        error(StatusCode::NOT_FOUND, format!("no share {id}"))
    }
}

fn gone() -> Response {
    let mut r = (StatusCode::NOT_FOUND, "This link has expired or was revoked.").into_response();
    r.headers_mut().insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    r
}

async fn page(State(app): AppState, Path(token): Path<String>) -> Response {
    if app.shares.lookup(&token).is_none() {
        return gone();
    }
    let Some(file) = crate::server::asset_file("share.html") else {
        return (StatusCode::NOT_FOUND, "web client not built: run `just web`").into_response();
    };
    let mut r = Response::new(Body::from(file));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    r
}

async fn ws(
    State(app): AppState,
    Path(token): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Err((status, why)) = app.access.check_origin(&headers) {
        warn!(%why, "rejected a share viewer");
        return (status, why).into_response();
    }
    let Some((id, pane, expires_ms)) = app.shares.lookup(&token) else {
        return gone();
    };
    let Some(handle) = app.mux.api(|r| Api::Pane(pane, r)).await.flatten() else {
        return error(StatusCode::GONE, format!("%{pane} has closed"));
    };
    upgrade.on_upgrade(move |socket| view(app, id, pane, expires_ms, handle, socket))
}

static NEXT_VIEWER: AtomicU64 = AtomicU64::new(1 << 61);

/// One viewer: subscribed to the pane alone (never to the multiplexer, so
/// it gets no layout), passing on output and sizes, and hung up on the
/// Rows of scrollback the viewer page keeps.
const VIEWER_SCROLLBACK: u32 = 10_000;

/// moment it sends anything or its share ends.
async fn view(
    app: Arc<App>,
    id: u32,
    pane: PaneId,
    expires_ms: u64,
    handle: crate::pane::PaneHandle,
    mut socket: WebSocket,
) {
    let client = NEXT_VIEWER.fetch_add(1, Ordering::Relaxed);
    let (data, mut data_rx) = client_queue();
    let (ctrl, mut ctrl_rx) = mpsc::unbounded_channel();
    // Watches one pane only; never sends the mux anything.
    let sub = Subscriber { client, data, ctrl, principal: crate::acl::Principal::Owner, name: None };
    // The viewer page keeps as much scrollback as the app (share.ts).
    let want = crate::pane::Want { history: Some(VIEWER_SCROLLBACK), ..Default::default() };
    handle.attach_with(sub.clone(), want);
    info!(share = id, pane, "share viewer connected");
    // What it's looking at, before any of it.
    let hello = serde_json::json!({ "type": "share", "pane": pane, "expires_ms": expires_ms });
    if socket.send(Message::Text(hello.to_string().into())).await.is_err() {
        handle.detach(client);
        return;
    }
    let mut check = tokio::time::interval(Duration::from_secs(1));
    let mut events = app.mux.events();
    let why = loop {
        tokio::select! {
            m = socket.recv() => match m {
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break None,
                // Input, attach, view, intents: a viewer may send nothing.
                Some(Ok(_)) => break Some("read-only"),
            },
            out = data_rx.recv() => match out {
                Some(ToClient::Frame(bytes)) => {
                    let ok = Frame::decode(&bytes).is_ok_and(|f| f.pane == pane && matches!(f.kind, FrameKind::Output | FrameKind::Snapshot));
                    if ok && socket.send(Message::Binary(bytes.into())).await.is_err() { break None }
                }
                Some(ToClient::Msg(m @ ServerMsg::Size { .. })) => {
                    if socket.send(Message::Text(serde_json::to_string(&m).unwrap_or_default().into())).await.is_err() { break None }
                }
                Some(ToClient::Msg(_) | ToClient::Json(_)) => {}
                Some(ToClient::Close) | None => break Some("the pane closed"),
            },
            Some(out) = ctrl_rx.recv() => {
                // Fell behind and was dropped: start over from a snapshot.
                if let ToClient::Msg(ServerMsg::Resync { .. }) = out {
                    handle.attach_with(sub.clone(), want);
                }
            }
            e = events.recv() => {
                if let Ok(e) = e && e.pane == Some(pane) && matches!(e.kind, EventKind::Closed) {
                    break Some("the pane closed");
                }
            }
            _ = check.tick() => {
                if !app.shares.live(id) { break Some("the link expired or was revoked") }
            }
        }
    };
    handle.detach(client);
    if let Some(reason) = why {
        let _ = socket.send(Message::Close(Some(CloseFrame { code: POLICY, reason: reason.into() }))).await;
    }
    info!(share = id, pane, "share viewer disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_expire_revoke_and_keep_only_hashes() {
        let d = std::env::temp_dir().join(format!("ilg-shares-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let s = Shares::open(&d);
        let a = s.mint(3, 60);
        let token = a.token.clone().unwrap();
        assert!(token.starts_with("ils_"));
        assert_eq!(a.path.as_deref(), Some(format!("/share/{token}").as_str()));
        assert_eq!(s.lookup(&token).map(|(id, p, _)| (id, p)), Some((a.id, 3)));
        assert_eq!(s.lookup("ils_wrong"), None);
        assert_eq!(s.list().len(), 1);
        assert!(s.list()[0].token.is_none(), "listing never shows a token");
        let saved = std::fs::read_to_string(d.join("shares.json")).unwrap();
        assert!(!saved.contains(&token));
        // Survives a restart.
        let s = Shares::open(&d);
        assert!(s.live(a.id));
        assert!(s.revoke(a.id));
        assert!(!s.revoke(a.id));
        assert!(!s.live(a.id));
        assert_eq!(s.lookup(&token), None);
        // TTLs are capped at a week.
        let long = s.mint(3, 10 * MAX_TTL_SECS);
        assert!(long.expires_ms - long.created_ms <= MAX_TTL_SECS * 1000);
        std::fs::remove_dir_all(d).unwrap();
    }
}
