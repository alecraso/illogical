//! History that outlives a sandbox (M4c): a host pushes its panes' log
//! segments and indexes to the home daemon, which keeps them encrypted at
//! rest (`seal.rs`) and answers `history`, `search` and `tail` from them
//! with `host=NAME`, after the host is gone.
//!
//! **Opt-in per host:** a host pushes only when started with `--sync`
//! (closed panes) or `--sync-live` (open ones too), and only with its
//! per-host token, which scopes everything it sends to its own name.
//! **Resumable:** it first asks what the home daemon has (`GET
//! /api/sync/state`), then sends from there by stream offset (output) and
//! byte offset (index), a megabyte at a time; the home daemon drops any
//! overlap and starts a new segment at a gap (output the host already
//! dropped). **Bounded:** each pane keeps at most [`RETAIN_BYTES`] of
//! output, and a pane not pushed to for [`RETAIN_MS`] is deleted.
//!
//! ```text
//! <state>/synced/                       0700
//!   key                                 0600  the key ring (seal.rs)
//!   <host>/<pane>/seg-<offset>.enc      sealed output from that offset
//!   <host>/<pane>/index.enc             the pane's index, sealed
//!   <host>/<pane>/meta.json             {closed_ms, last_push_ms}: no content
//! ```

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path as UrlPath, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use illogical_proto::{
    PaneId,
    api::{HistoryEntry, SearchHit, SyncState, SyncedHost, SyncedPane},
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{
    history::{self, Filter},
    seal::{self, KeyRing},
    server::App,
    store::{Event, PaneLog, StateDir, now_ms, parse_events, private_dir, write_atomic},
};

/// Output kept per synced pane.
pub const RETAIN_BYTES: u64 = 256 * 1024 * 1024;
/// A synced pane not pushed to for this long is deleted.
pub const RETAIN_MS: u64 = 30 * 24 * 3600 * 1000;
/// Plaintext per sealed segment file.
const SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
/// Largest push.
pub const CHUNK: usize = 1024 * 1024;

#[derive(Default, Serialize, Deserialize)]
struct Meta {
    #[serde(default)]
    closed_ms: Option<u64>,
    #[serde(default)]
    last_push_ms: u64,
}

/// The home daemon's store of other hosts' history.
pub struct Synced {
    root: PathBuf,
    key_file: PathBuf,
    /// Loaded (or made) on first use: daemons nobody syncs to have no key.
    ring: Mutex<Option<KeyRing>>,
}

fn seg_name(start: u64) -> String {
    format!("seg-{start:020}.enc")
}

fn seg_start(name: &str) -> Option<u64> {
    name.strip_prefix("seg-")?.strip_suffix(".enc")?.parse().ok()
}

fn io_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl Synced {
    pub fn new(state_dir: &Path, key_file: Option<PathBuf>) -> Arc<Self> {
        let root = state_dir.join("synced");
        let key_file = key_file.unwrap_or_else(|| root.join("key"));
        Arc::new(Self { root, key_file, ring: Mutex::new(None) })
    }

    fn with_ring<T>(&self, f: impl FnOnce(&mut KeyRing) -> io::Result<T>) -> io::Result<T> {
        let mut ring = self.ring.lock().unwrap();
        if ring.is_none() {
            *ring = Some(KeyRing::open(&self.key_file)?);
        }
        f(ring.as_mut().expect("loaded"))
    }

    fn pane_dir(&self, host: &str, pane: PaneId) -> PathBuf {
        self.root.join(host).join(pane.to_string())
    }

    fn context(host: &str, pane: PaneId, file: &str) -> String {
        format!("{host}/{pane}/{file}")
    }

    /// Segment starts of a pane, oldest first.
    fn segments(dir: &Path) -> Vec<u64> {
        let mut v: Vec<u64> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| seg_start(e.file_name().to_str()?))
            .collect();
        v.sort_unstable();
        v
    }

    fn meta(dir: &Path) -> Meta {
        fs::read(dir.join("meta.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn save_meta(dir: &Path, m: &Meta) -> io::Result<()> {
        write_atomic(&dir.join("meta.json"), &serde_json::to_vec(m).map_err(io_err)?)
    }

    /// What is held of one pane. The sealed files are the truth: a crash
    /// mid-push leaves them consistent, and the next push resumes from here.
    pub fn pane(&self, host: &str, pane: PaneId) -> SyncedPane {
        let dir = self.pane_dir(host, pane);
        let segs = Self::segments(&dir);
        let mut bytes = 0;
        let mut log_end = 0;
        for s in &segs {
            let n = seal::plaintext_len(&dir.join(seg_name(*s))).unwrap_or(0);
            bytes += n;
            log_end = log_end.max(s + n);
        }
        let index_len = seal::plaintext_len(&dir.join("index.enc")).unwrap_or(0);
        let m = Self::meta(&dir);
        SyncedPane { log_end, index_len, closed_ms: m.closed_ms, last_push_ms: m.last_push_ms, bytes }
    }

    fn pane_ids(&self, host: &str) -> Vec<PaneId> {
        let mut v: Vec<PaneId> = fs::read_dir(self.root.join(host))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .collect();
        v.sort_unstable();
        v
    }

    pub fn state(&self, host: &str) -> SyncState {
        SyncState { panes: self.pane_ids(host).into_iter().map(|p| (p, self.pane(host, p))).collect() }
    }

    pub fn hosts(&self) -> Vec<SyncedHost> {
        let mut names: Vec<String> = fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        names.into_iter().map(|name| SyncedHost { panes: self.state(&name).panes, name }).collect()
    }

    pub fn remove_host(&self, host: &str) -> bool {
        valid_host(host) && fs::remove_dir_all(self.root.join(host)).is_ok()
    }

    fn touch(dir: &Path, f: impl FnOnce(&mut Meta)) -> io::Result<()> {
        let mut m = Self::meta(dir);
        f(&mut m);
        m.last_push_ms = now_ms();
        Self::save_meta(dir, &m)
    }

    /// Output from stream offset `from`. Overlap with what is held is
    /// dropped; a gap starts a new segment.
    pub fn push_log(&self, host: &str, pane: PaneId, from: u64, data: &[u8]) -> io::Result<SyncedPane> {
        let dir = self.pane_dir(host, pane);
        private_dir(&dir)?;
        let held = self.pane(host, pane);
        let end = held.log_end;
        if from > end || held.bytes == 0 {
            // A gap (or the first push): start a segment where this begins.
            self.append_log(host, pane, &dir, from, data, true)?;
        } else {
            let skip = (end - from) as usize;
            if skip < data.len() {
                self.append_log(host, pane, &dir, end, &data[skip..], false)?;
            }
        }
        self.enforce_retention(&dir);
        Self::touch(&dir, |_| {})?;
        Ok(self.pane(host, pane))
    }

    fn append_log(
        &self,
        host: &str,
        pane: PaneId,
        dir: &Path,
        mut at: u64,
        mut data: &[u8],
        fresh: bool,
    ) -> io::Result<()> {
        let mut fresh = fresh;
        while !data.is_empty() {
            let segs = Self::segments(dir);
            let (start, used) = match segs.last() {
                Some(s) if !fresh => (*s, at - s),
                _ => (at, 0),
            };
            let (start, used) = if used >= SEGMENT_BYTES { (at, 0) } else { (start, used) };
            let room = (SEGMENT_BYTES - used) as usize;
            let (now, rest) = data.split_at(room.min(data.len()));
            let name = seg_name(start);
            self.with_ring(|ring| seal::append(ring, &dir.join(&name), &Self::context(host, pane, &name), now))?;
            at += now.len() as u64;
            data = rest;
            fresh = false;
        }
        Ok(())
    }

    fn enforce_retention(&self, dir: &Path) {
        let mut segs = Self::segments(dir);
        let len = |s: &u64| seal::plaintext_len(&dir.join(seg_name(*s))).unwrap_or(0);
        let mut total: u64 = segs.iter().map(len).sum();
        while segs.len() > 1 && total > RETAIN_BYTES {
            let s = segs.remove(0);
            total -= len(&s);
            let _ = fs::remove_file(dir.join(seg_name(s)));
        }
    }

    /// The index from byte `from`. It must continue what is held.
    pub fn push_index(
        &self,
        host: &str,
        pane: PaneId,
        from: u64,
        data: &[u8],
    ) -> Result<SyncedPane, (StatusCode, String)> {
        let dir = self.pane_dir(host, pane);
        private_dir(&dir).map_err(internal)?;
        let held = self.pane(host, pane);
        if from > held.index_len {
            return Err((StatusCode::CONFLICT, format!("index: have {} bytes, got from {from}", held.index_len)));
        }
        let skip = (held.index_len - from) as usize;
        if skip < data.len() {
            self.with_ring(|ring| {
                seal::append(ring, &dir.join("index.enc"), &Self::context(host, pane, "index.enc"), &data[skip..])
            })
            .map_err(internal)?;
        }
        Self::touch(&dir, |_| {}).map_err(internal)?;
        Ok(self.pane(host, pane))
    }

    pub fn push_closed(&self, host: &str, pane: PaneId, at_ms: u64) -> io::Result<SyncedPane> {
        let dir = self.pane_dir(host, pane);
        private_dir(&dir)?;
        Self::touch(&dir, |m| m.closed_ms = Some(at_ms))?;
        Ok(self.pane(host, pane))
    }

    /// Delete panes not pushed to within `max_age_ms`.
    pub fn prune(&self, max_age_ms: u64) {
        let cutoff = now_ms().saturating_sub(max_age_ms);
        for h in self.hosts() {
            for (pane, p) in &h.panes {
                if p.last_push_ms < cutoff {
                    let _ = fs::remove_dir_all(self.pane_dir(&h.name, *pane));
                    info!(host = h.name, pane, "synced history expired");
                }
            }
            if self.pane_ids(&h.name).is_empty() {
                let _ = fs::remove_dir(self.root.join(&h.name));
            }
        }
    }

    /// A new key; every file re-sealed under it; the old keys dropped.
    pub fn rotate(&self) -> io::Result<u32> {
        self.with_ring(|ring| {
            let id = ring.add()?;
            for h in self.hosts() {
                for pane in h.panes.keys() {
                    let dir = self.pane_dir(&h.name, *pane);
                    for e in fs::read_dir(&dir)?.flatten() {
                        let name = e.file_name().to_string_lossy().into_owned();
                        if name.ends_with(".enc") {
                            seal::reseal(ring, &e.path(), &Self::context(&h.name, *pane, &name))?;
                        }
                    }
                }
            }
            ring.drop_old()?;
            info!(key = id, "sync key rotated");
            Ok(id)
        })
    }

    // ------------------------------------------------------------ reading

    fn events(&self, host: &str, pane: PaneId) -> Vec<(u64, Event)> {
        let path = self.pane_dir(host, pane).join("index.enc");
        if !path.exists() {
            return vec![];
        }
        match self.with_ring(|ring| seal::read(ring, &path, &Self::context(host, pane, "index.enc"))) {
            Ok(b) => parse_events(&String::from_utf8_lossy(&b)),
            Err(e) => {
                warn!(host, pane, error = %e, "can't open a synced index");
                vec![]
            }
        }
    }

    /// Output from `from` (clamped to what is held) to the end.
    pub fn read_from(&self, host: &str, pane: PaneId, from: u64) -> io::Result<(u64, Vec<u8>)> {
        let dir = self.pane_dir(host, pane);
        let segs = Self::segments(&dir);
        let Some(first) = segs.first() else {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("no synced pane %{pane} from {host}")));
        };
        let from = from.max(*first);
        let mut out = Vec::new();
        let mut start = None;
        for s in &segs {
            let name = seg_name(*s);
            let len = seal::plaintext_len(&dir.join(&name))?;
            if s + len <= from {
                continue;
            }
            let bytes = self.with_ring(|ring| seal::read(ring, &dir.join(&name), &Self::context(host, pane, &name)))?;
            let skip = from.saturating_sub(*s) as usize;
            start.get_or_insert(s + skip as u64);
            out.extend_from_slice(&bytes[skip.min(bytes.len())..]);
        }
        Ok((start.unwrap_or(from), out))
    }

    fn hosts_matching(&self, host: &str) -> Vec<String> {
        if host == "*" {
            self.hosts().into_iter().map(|h| h.name).collect()
        } else if valid_host(host) {
            vec![host.to_owned()]
        } else {
            vec![]
        }
    }

    pub fn history(&self, host: &str, f: &Filter, limit: usize) -> Vec<HistoryEntry> {
        let mut all = Vec::new();
        for h in self.hosts_matching(host) {
            for pane in self.pane_ids(&h).into_iter().filter(|p| f.pane.is_none_or(|x| x == *p)) {
                let open = self.pane(&h, pane).closed_ms.is_none();
                all.extend(history::commands_in(self.events(&h, pane), pane, open).into_iter().map(|mut c| {
                    c.host = Some(h.clone());
                    c
                }));
            }
        }
        history::filtered(all.into_iter(), f, limit)
    }

    pub fn search(&self, host: &str, re: &Regex, since_ms: Option<u64>, limit: usize) -> Vec<SearchHit> {
        let mut hits = Vec::new();
        for h in self.hosts_matching(host) {
            for pane in self.pane_ids(&h) {
                let open = self.pane(&h, pane).closed_ms.is_none();
                let before = hits.len();
                let read = |from| self.read_from(&h, pane, from).ok();
                let full = history::search_log(pane, open, self.events(&h, pane), read, re, since_ms, limit, &mut hits);
                for hit in &mut hits[before..] {
                    hit.host = Some(h.clone());
                }
                if full {
                    return hits;
                }
            }
        }
        hits
    }
}

/// Host names as directory names: what `hosts::validate` lets through.
fn valid_host(h: &str) -> bool {
    !h.is_empty() && !h.starts_with('.') && h.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn internal(e: io::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

// ---------------------------------------------------------------- routes

type AppState = State<Arc<App>>;

/// Hosts pushing (a host token is the credential: see `server::guard`).
pub const PUSH_PREFIX: &str = "/api/sync/";

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/sync/state", get(state))
        .route("/api/sync/{pane}/log", post(push_log))
        .route("/api/sync/{pane}/index", post(push_index))
        .route("/api/sync/{pane}/closed", post(push_closed))
        .route("/api/synced", get(list))
        .route("/api/synced/rotate-key", post(rotate))
        .route("/api/synced/{host}", delete(forget))
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// The host whose token this is.
fn caller(app: &App, headers: &HeaderMap) -> Result<String, (StatusCode, &'static str)> {
    let token = crate::dial::bearer(headers)
        .ok_or((StatusCode::UNAUTHORIZED, "a host token is needed (Authorization: Bearer)"))?;
    app.hosts.host_for_token(token).ok_or_else(|| {
        warn!("refused a sync push: unknown or revoked token");
        (StatusCode::FORBIDDEN, "invalid or revoked host token")
    })
}

async fn state(State(app): AppState, headers: HeaderMap) -> Response {
    match caller(&app, &headers) {
        Ok(host) => Json(app.synced.state(&host)).into_response(),
        Err((s, e)) => error(s, e),
    }
}

#[derive(Deserialize)]
struct From {
    #[serde(default)]
    from: u64,
}

#[derive(Deserialize)]
struct At {
    at: u64,
}

async fn push_log(
    State(app): AppState,
    UrlPath(pane): UrlPath<PaneId>,
    Query(q): Query<From>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let host = match caller(&app, &headers) {
        Ok(h) => h,
        Err((s, e)) => return error(s, e),
    };
    if body.len() > CHUNK {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "at most 1 MB a push");
    }
    let synced = app.synced.clone();
    match tokio::task::spawn_blocking(move || synced.push_log(&host, pane, q.from, &body)).await {
        Ok(Ok(p)) => Json(p).into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn push_index(
    State(app): AppState,
    UrlPath(pane): UrlPath<PaneId>,
    Query(q): Query<From>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let host = match caller(&app, &headers) {
        Ok(h) => h,
        Err((s, e)) => return error(s, e),
    };
    if body.len() > CHUNK {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "at most 1 MB a push");
    }
    let synced = app.synced.clone();
    match tokio::task::spawn_blocking(move || synced.push_index(&host, pane, q.from, &body)).await {
        Ok(Ok(p)) => Json(p).into_response(),
        Ok(Err((s, e))) => error(s, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn push_closed(
    State(app): AppState,
    UrlPath(pane): UrlPath<PaneId>,
    Query(q): Query<At>,
    headers: HeaderMap,
) -> Response {
    let host = match caller(&app, &headers) {
        Ok(h) => h,
        Err((s, e)) => return error(s, e),
    };
    match app.synced.push_closed(&host, pane, q.at) {
        Ok(p) => Json(p).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn list(State(app): AppState) -> Json<Vec<SyncedHost>> {
    Json(app.synced.hosts())
}

async fn forget(State(app): AppState, UrlPath(host): UrlPath<String>) -> Response {
    if app.synced.remove_host(&host) {
        Json(serde_json::json!({})).into_response()
    } else {
        error(StatusCode::NOT_FOUND, format!("no synced history from {host}"))
    }
}

async fn rotate(State(app): AppState) -> Response {
    let synced = app.synced.clone();
    match tokio::task::spawn_blocking(move || synced.rotate()).await {
        Ok(Ok(id)) => Json(serde_json::json!({ "key": id })).into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ---------------------------------------------------------------- host end

/// `--sync`: where to push and what.
#[derive(Debug, Clone)]
pub struct PushOpts {
    /// The home daemon's origin (`https://geek.….ts.net`).
    pub origin: String,
    pub token_file: PathBuf,
    /// Open panes too, not only closed ones.
    pub live: bool,
    pub every: Duration,
}

/// Push for good, every `every`.
pub async fn keep_pushing(opts: PushOpts, store: StateDir) {
    let http = crate::roots::http().timeout(Duration::from_secs(60)).build().expect("an HTTP client");
    let mut tick = tokio::time::interval(opts.every);
    let mut failing = false;
    loop {
        tick.tick().await;
        match push_once(&http, &opts, &store).await {
            Ok(n) => {
                if n > 0 || failing {
                    info!(bytes = n, "history synced to the home daemon");
                }
                failing = false;
            }
            Err(e) => {
                if !failing {
                    warn!(error = %e, "can't sync history to the home daemon");
                }
                failing = true;
            }
        }
    }
}

/// One pass over every pane; bytes sent.
pub async fn push_once(http: &reqwest::Client, opts: &PushOpts, store: &StateDir) -> anyhow::Result<u64> {
    let token = fs::read_to_string(&opts.token_file)?.trim().to_owned();
    let auth = format!("Bearer {token}");
    let state: SyncState = checked(http.get(format!("{}/api/sync/state", opts.origin)).header("authorization", &auth))
        .await?
        .json()
        .await?;
    let mut sent = 0u64;
    for (pane, open, dir) in store.pane_dirs() {
        if open && !opts.live {
            continue;
        }
        let held = state.panes.get(&pane).cloned().unwrap_or_default();
        let log = PaneLog::open(dir.clone())?;
        let mut from = held.log_end.max(log.start());
        while from < log.end() {
            let (start, chunk) = log.read_range(from, CHUNK)?;
            if chunk.is_empty() {
                break;
            }
            let url = format!("{}/api/sync/{pane}/log?from={start}", opts.origin);
            checked(http.post(url).header("authorization", &auth).body(chunk.clone())).await?;
            sent += chunk.len() as u64;
            from = start + chunk.len() as u64;
        }
        let index = fs::read(dir.join("index")).unwrap_or_default();
        let mut at = held.index_len as usize;
        while at < index.len() {
            let end = (at + CHUNK).min(index.len());
            let url = format!("{}/api/sync/{pane}/index?from={at}", opts.origin);
            checked(http.post(url).header("authorization", &auth).body(index[at..end].to_vec())).await?;
            sent += (end - at) as u64;
            at = end;
        }
        // Closed panes' directories are `<id>-<when>`.
        let closed = (!open).then(|| dir.file_name()?.to_str()?.rsplit_once('-')?.1.parse::<u64>().ok()).flatten();
        if let Some(at) = closed.filter(|_| held.closed_ms.is_none()) {
            let url = format!("{}/api/sync/{pane}/closed?at={at}", opts.origin);
            checked(http.post(url).header("authorization", &auth)).await?;
        }
    }
    Ok(sent)
}

async fn checked(req: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
    let res = req.send().await?;
    if !res.status().is_success() {
        let status = res.status();
        anyhow::bail!("HTTP {status}: {}", res.text().await.unwrap_or_default());
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ilg-sync-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn pushes_resume_skip_overlap_and_mark_gaps() {
        let d = dir("push");
        let s = Synced::new(&d, None);
        assert_eq!(s.pane("box", 3), SyncedPane::default());
        s.push_log("box", 3, 0, b"hello ").unwrap();
        // A retry of what was already sent, plus more: only the new part.
        let p = s.push_log("box", 3, 3, b"lo world\n").unwrap();
        assert_eq!(p.log_end, 12);
        assert_eq!(s.read_from("box", 3, 0).unwrap(), (0, b"hello world\n".to_vec()));
        assert_eq!(s.read_from("box", 3, 6).unwrap(), (6, b"world\n".to_vec()));
        // A gap (the host dropped output before it was sent): a new segment.
        let p = s.push_log("box", 3, 100, b"later\n").unwrap();
        assert_eq!((p.log_end, p.bytes), (106, 18));
        assert_eq!(s.read_from("box", 3, 0).unwrap().1, b"hello world\nlater\n");
        // The index must continue what is held.
        s.push_index("box", 3, 0, b"{\"o\":6,\"e\":\"cwd\",\"path\":\"/w\"}\n").unwrap();
        assert_eq!(s.push_index("box", 3, 999, b"x").unwrap_err().0, StatusCode::CONFLICT);
        assert_eq!(s.events("box", 3), vec![(6, Event::Cwd { path: "/w".into() })]);
        s.push_closed("box", 3, 1234).unwrap();
        let state = s.state("box");
        assert_eq!(state.panes[&3].closed_ms, Some(1234));
        assert!(state.panes[&3].last_push_ms > 0);
        // On disk it's ciphertext, and the key is private.
        for e in fs::read_dir(d.join("synced/box/3")).unwrap().flatten() {
            let raw = fs::read(e.path()).unwrap();
            assert!(!raw.windows(5).any(|w| w == b"hello" || w == b"later"), "{:?}", e.path());
        }
        // Modes are Unix's; Windows has the profile's ACL.
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(d.join("synced/key")).unwrap().permissions()) & 0o777,
            0o600
        );
        // Rotation keeps it readable.
        assert_eq!(s.rotate().unwrap(), 2);
        assert_eq!(Synced::new(&d, None).read_from("box", 3, 0).unwrap().1, b"hello world\nlater\n");
        // Searchable, with the host named.
        let hits = s.search("*", &Regex::new("wor").unwrap(), None, 10);
        assert_eq!((hits.len(), hits[0].host.as_deref(), hits[0].open), (1, Some("box"), false));
        assert!(s.search("nope", &Regex::new("wor").unwrap(), None, 10).is_empty());
        assert!(s.search("..", &Regex::new("wor").unwrap(), None, 10).is_empty());
        // Expiry and forgetting.
        s.prune(RETAIN_MS);
        assert_eq!(s.hosts().len(), 1);
        assert!(s.remove_host("box"));
        assert!(s.hosts().is_empty());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn long_output_rolls_over_segments() {
        let d = dir("roll");
        let s = Synced::new(&d, None);
        let chunk = vec![b'x'; CHUNK];
        for i in 0..5 {
            s.push_log("box", 1, (i * CHUNK) as u64, &chunk).unwrap();
        }
        assert_eq!(Synced::segments(&d.join("synced/box/1")), vec![0, SEGMENT_BYTES]);
        let p = s.pane("box", 1);
        assert_eq!((p.log_end, p.bytes), (5 * CHUNK as u64, 5 * CHUNK as u64));
        let (from, tail) = s.read_from("box", 1, SEGMENT_BYTES - 1).unwrap();
        assert_eq!((from, tail.len()), (SEGMENT_BYTES - 1, CHUNK + 1));
        fs::remove_dir_all(d).unwrap();
    }
}
