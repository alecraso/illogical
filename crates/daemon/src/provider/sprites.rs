//! The Sprites API adapter: wisp (Firecracker microVMs on a host running
//! wispd) and Fly's hosted sprites speak the same API, with the
//! differences in [`Caps`]. Endpoints (all under `/v1/sprites`, bearer
//! token):
//!
//! - `GET /`, `GET /{name}` (doesn't wake it), `POST /`, `DELETE /{name}`;
//! - **exec**, a WebSocket at `/{name}/exec`: with `tty=true` binary frames
//!   are the terminal's bytes both ways and `session_info`, `resize` and
//!   `exit` are JSON text frames; without, frames carry a stream byte (`0`
//!   stdin, `1` stdout, `2` stderr, `3` exit code) then data. A session
//!   outlives its socket for `max_run_after_disconnect`, and
//!   `/{name}/exec/{id}?output_offset=N` reattaches past the N bytes we
//!   already have. `POST /{name}/exec/{id}/kill?signal=` signals it;
//! - **proxy**, a WebSocket at `/{name}/proxy` carrying one TCP connection:
//!   send `{"host": "localhost", "port": N}`, get `{"status": "connected"}`
//!   (or an error and a close), then binary frames both ways. It wakes the
//!   sprite, and holds it awake while open;
//! - **fs**: `PUT /{name}/fs/write?path=…&mode=…`; `GET /{name}/fs/list?path=…`
//!   (a directory's entries, or the one entry of anything else, not
//!   following a symlink) and `GET /{name}/fs/read?path=…` (the whole file:
//!   we stop reading past the range we want). The guest agent serves these
//!   as root, so the daemon adds its own limits (`fs.rs`);
//! - **services**: `PUT /{name}/services/{svc}` defines and starts one
//!   (streaming NDJSON events), `DELETE` removes it. Services start on
//!   every boot and restart when they exit.
//!
//! VM panes' terminals are exec TTY sessions. The daemon runs its own
//! terminal engine and log over those bytes, so scrollback doesn't depend
//! on the provider's replay:
//!
//! - **Create** is nearly free and doesn't boot anything; the first exec
//!   does (about 0.3s to a prompt on wisp; spike M3b).
//! - **Detach and reattach**: a daemon restart reattaches from the count of
//!   bytes it has, so nothing is duplicated or lost.
//! - **Machine gone**: the WebSocket drops without an `exit` frame, and the
//!   sprite is 404. A drop with the sprite still there is a network blip:
//!   reattach.

use std::{io, sync::Arc, time::Duration};

use futures_util::{FutureExt, SinkExt, StreamExt, future::BoxFuture};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, handshake::client::Request};
use tracing::{info, warn};

use super::{
    Begin, Caps, Cold, Conn, Exec, ExecEvent, ExecInput, ExecSink, PATIENCE, Pipe, PipeBegin, PipeEvent, Provider,
    Sandbox, ServiceDef,
};
use crate::pane::Spawn;
use arugula_proto::fs::{FsEntry, FsKind, FsList};

/// How long a session survives with nobody attached: longer than any
/// daemon restart or upgrade.
const DETACHED_FOR: &str = "12h";

/// Opening a proxied connection takes at most this long (a sprite may be
/// waking, or booting cold).
const DIAL_TIMEOUT: Duration = Duration::from_secs(30);

/// A Sprites API, by its URL and bearer token.
pub struct Sprites {
    base: Url,
    token: String,
    http: reqwest::Client,
    /// Without a timeout, for streamed answers (service starts).
    slow: reqwest::Client,
    name: &'static str,
    caps: Caps,
}

#[derive(Deserialize)]
struct SpriteList {
    #[serde(default)]
    sprites: Vec<SpriteEntry>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_continuation_token: Option<String>,
}

/// Pages of a sprite list we follow at most (50 sprites each).
const MAX_PAGES: usize = 40;

#[derive(Deserialize)]
struct SpriteEntry {
    name: String,
    #[serde(default)]
    status: String,
}

impl std::fmt::Debug for Sprites {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sprites").field("name", &self.name).field("base", &self.base.as_str()).finish_non_exhaustive()
    }
}

/// wisp (spike M3b, S4): a 1 MiB replay from any offset, the reattacher is
/// not the owner but may resize, kill takes a signal (TERM waits 10s for an
/// interactive bash), and cold is a reboot after `--warm-ttl`.
fn wisp_caps() -> Caps {
    Caps {
        exec_replay: 1 << 20,
        exec_offset: true,
        reattach_owner: false,
        reattach_resize: true,
        kill_signal: true,
        kill_grace_ms: 10_000,
        cold: Cold::Reboot,
        fs: true,
        fs_browse: true,
        services: true,
    }
}

/// Fly (S4): about 6.5 KB of replay, the reattacher becomes owner, and cold
/// was a memory restore every time we saw it. Offsets and kill signals are
/// untested there, so they're assumed absent.
fn fly_caps() -> Caps {
    Caps {
        exec_replay: 6_656,
        exec_offset: false,
        reattach_owner: true,
        reattach_resize: true,
        kill_signal: false,
        kill_grace_ms: 10_000,
        cold: Cold::Restore,
        fs: true,
        fs_browse: true,
        services: true,
    }
}

impl Sprites {
    pub fn new(base: &str, token: String) -> anyhow::Result<Self> {
        let base = Url::parse(base)?;
        let http = crate::roots::http().timeout(Duration::from_secs(30)).build()?;
        let slow = crate::roots::http().connect_timeout(Duration::from_secs(30)).build()?;
        let fly = base.host_str().is_some_and(|h| h == "sprites.dev" || h.ends_with(".sprites.dev"));
        let (name, caps) = if fly { ("sprites", fly_caps()) } else { ("wisp", wisp_caps()) };
        Ok(Self { base, token, http, slow, name, caps })
    }

    /// From a URL and a token file; `None` (VM panes unavailable) if there's
    /// no token.
    pub fn open(base: &str, token_file: &std::path::Path) -> Option<Self> {
        let token = std::fs::read_to_string(token_file).ok()?.trim().to_owned();
        if token.is_empty() {
            return None;
        }
        match Self::new(base, token) {
            Ok(w) => Some(w),
            Err(e) => {
                warn!(error = %e, base, "bad Sprites API URL; VM panes are off");
                None
            }
        }
    }

    fn url(&self, path: &str) -> Url {
        let mut u = self.base.clone();
        u.set_path(&format!("/v1/sprites{path}"));
        u
    }

    fn ws_url(&self, path: &str) -> Url {
        let mut u = self.url(path);
        let scheme = if u.scheme() == "https" { "wss" } else { "ws" };
        let _ = u.set_scheme(scheme);
        u
    }

    fn request(&self, url: Url) -> anyhow::Result<Request> {
        let mut req = url.as_str().into_client_request()?;
        req.headers_mut().insert("Authorization", format!("Bearer {}", self.token).parse()?);
        Ok(req)
    }

    async fn get_status(&self, name: &str) -> anyhow::Result<Option<Sandbox>> {
        let r = self.http.get(self.url(&format!("/{name}"))).bearer_auth(&self.token).send().await?;
        match r.status() {
            s if s.is_success() => {
                let e: SpriteEntry = r.json().await?;
                Ok(Some(Sandbox { name: e.name, status: e.status }))
            }
            StatusCode::NOT_FOUND => Ok(None),
            s => anyhow::bail!("looking up {name}: {s}"),
        }
    }

    async fn do_create(&self, name: &str, image: Option<&str>) -> anyhow::Result<()> {
        let mut body = serde_json::json!({ "name": name });
        if let Some(i) = image {
            body["from"] = serde_json::json!({ "image": i });
        }
        let r = self.http.post(self.url("")).bearer_auth(&self.token).json(&body).send().await?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::BAD_REQUEST | StatusCode::CONFLICT if text.contains("name_taken") => Ok(()),
            s => anyhow::bail!("creating {name}: {s} {}", text.trim()),
        }
    }

    async fn do_run(&self, name: &str, argv: &[&str]) -> anyhow::Result<(Vec<u8>, Option<i32>)> {
        let mut u = self.ws_url(&format!("/{name}/exec"));
        {
            let mut q = u.query_pairs_mut();
            for a in argv {
                q.append_pair("cmd", a);
            }
            q.append_pair("stdin", "false");
        }
        let (mut ws, _) = tokio_tungstenite::connect_async(self.request(u)?).await?;
        let (mut out, mut code) = (Vec::new(), None);
        let read = async {
            while let Some(m) = ws.next().await {
                match m? {
                    // Non-TTY output: one stream byte, then data.
                    Message::Binary(b) if b.first() == Some(&1) => out.extend_from_slice(&b[1..]),
                    Message::Binary(b) if b.first() == Some(&3) => code = b.get(1).map(|c| *c as i32),
                    Message::Text(t) => {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                        if v["type"] == "exit" {
                            code = v["exit_code"].as_i64().map(|c| c as i32);
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            anyhow::Ok(())
        };
        // A cold sandbox boots first.
        tokio::time::timeout(Duration::from_secs(30), read).await??;
        Ok((out, code))
    }

    fn exec_url(&self, name: &str, spawn: &Spawn, cols: u16, rows: u16) -> Url {
        let mut u = self.ws_url(&format!("/{name}/exec"));
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("tty", "true");
            q.append_pair("cmd", &spawn.program);
            for a in &spawn.args {
                q.append_pair("cmd", a);
            }
            if !spawn.cwd.as_os_str().is_empty() {
                q.append_pair("dir", &spawn.cwd.display().to_string());
            }
            for (k, v) in &spawn.env {
                q.append_pair("env", &format!("{k}={v}"));
            }
            q.append_pair("cols", &cols.to_string());
            q.append_pair("rows", &rows.to_string());
            q.append_pair("max_run_after_disconnect", DETACHED_FOR);
        }
        u
    }

    fn attach_url(&self, name: &str, session: &str, offset: u64) -> Url {
        let mut u = self.ws_url(&format!("/{name}/exec/{session}"));
        u.query_pairs_mut().append_pair("output_offset", &offset.to_string());
        u
    }

    async fn do_kill(&self, name: &str, session: &str, signal: &str) -> anyhow::Result<()> {
        let mut u = self.url(&format!("/{name}/exec/{session}/kill"));
        if self.caps.kill_signal {
            u.query_pairs_mut().append_pair("signal", signal).append_pair("timeout", "3s");
        }
        self.http.post(u).bearer_auth(&self.token).send().await?.error_for_status()?;
        Ok(())
    }

    async fn do_pipe(&self, name: &str, begin: PipeBegin) -> anyhow::Result<Pipe> {
        let url = match begin {
            PipeBegin::New { argv } => {
                let mut u = self.ws_url(&format!("/{name}/exec"));
                {
                    let mut q = u.query_pairs_mut();
                    for a in &argv {
                        q.append_pair("cmd", a);
                    }
                    q.append_pair("stdin", "true");
                    q.append_pair("max_run_after_disconnect", DETACHED_FOR);
                }
                u
            }
            PipeBegin::Resume { session, received } => self.attach_url(name, &session, received),
        };
        let (ws, _) = tokio_tungstenite::connect_async(self.request(url)?).await?;
        let (ev_tx, events) = mpsc::unbounded_channel();
        let (stdin, mut in_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            let mut ws = ws;
            loop {
                tokio::select! {
                    m = ws.next() => {
                        let ev = match m {
                            Some(Ok(Message::Binary(b))) if !b.is_empty() => match b[0] {
                                1 => PipeEvent::Stdout(b[1..].to_vec()),
                                2 => PipeEvent::Stderr(b[1..].to_vec()),
                                3 => PipeEvent::Exited(Some(b.get(1).copied().unwrap_or(0) as i32)),
                                _ => continue,
                            },
                            Some(Ok(Message::Text(t))) => {
                                let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                                match v["type"].as_str() {
                                    Some("session_info") => PipeEvent::Session(
                                        v["session_id"].as_str().map(str::to_owned)
                                            .unwrap_or_else(|| v["session_id"].to_string()),
                                    ),
                                    Some("exit") => PipeEvent::Exited(v["exit_code"].as_i64().map(|c| c as i32)),
                                    _ => continue,
                                }
                            }
                            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                            Some(Ok(_)) => continue,
                        };
                        if ev_tx.send(ev).is_err() {
                            let _ = ws.close(None).await;
                            return;
                        }
                    }
                    data = in_rx.recv() => match data {
                        Some(d) => {
                            let mut f = Vec::with_capacity(d.len() + 1);
                            f.push(0u8);
                            f.extend_from_slice(&d);
                            let _ = ws.send(Message::Binary(f.into())).await;
                        }
                        // Let go: detach, leaving the process running.
                        None => {
                            let _ = ws.close(None).await;
                            return;
                        }
                    },
                }
            }
        });
        Ok(Pipe { events, stdin })
    }

    async fn do_dial(&self, name: &str, port: u16) -> io::Result<Conn> {
        match tokio::time::timeout(DIAL_TIMEOUT, dial_proxy(self, name, port)).await {
            Ok(r) => r,
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, format!("{name}:{port} didn't answer"))),
        }
    }

    async fn do_write(&self, name: &str, path: &str, data: Vec<u8>, mode: u32) -> anyhow::Result<()> {
        let mut u = self.url(&format!("/{name}/fs/write"));
        u.query_pairs_mut()
            .append_pair("path", path)
            .append_pair("mode", &format!("{mode:o}"))
            .append_pair("mkdirParents", "true");
        let r = self.slow.put(u).bearer_auth(&self.token).body(data).send().await?;
        let status = r.status();
        if !status.is_success() {
            anyhow::bail!("writing {path} in {name}: {status} {}", r.text().await.unwrap_or_default().trim());
        }
        Ok(())
    }

    /// `path` as the agent wants it: `~` is the sprite user's home, which
    /// is where relative paths start.
    fn fs_url(&self, name: &str, op: &str, path: &str) -> Url {
        let path = match path.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => rest.trim_start_matches('/'),
            _ => path,
        };
        let path = if path.is_empty() { "." } else { path };
        let mut u = self.url(&format!("/{name}/fs/{op}"));
        u.query_pairs_mut().append_pair("path", path);
        u
    }

    async fn fs_failed(name: &str, path: &str, r: reqwest::Response) -> anyhow::Error {
        use crate::fs::FsError;
        let status = r.status();
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        let why = v["error"].as_str().unwrap_or("failed").to_owned();
        match status {
            StatusCode::NOT_FOUND if v["code"] == "not_found" => FsError::NotFound(format!("{path}: {why}")).into(),
            // The sprite itself.
            StatusCode::NOT_FOUND => FsError::Unavailable(format!("{name}: no such sandbox")).into(),
            StatusCode::FORBIDDEN => FsError::Denied(format!("{path}: {why}")).into(),
            StatusCode::BAD_REQUEST => FsError::Bad(format!("{path}: {why}")).into(),
            s => FsError::Unavailable(format!("{name}: {s} {why}")).into(),
        }
    }

    async fn do_fs_list(&self, name: &str, path: &str) -> anyhow::Result<FsList> {
        let r = self.http.get(self.fs_url(name, "list", path)).bearer_auth(&self.token).send().await?;
        if !r.status().is_success() {
            return Err(Self::fs_failed(name, path, r).await);
        }
        let list: WispList = r.json().await?;
        let mut entries: Vec<FsEntry> = list.entries.into_iter().map(WispEntry::into_entry).collect();
        let truncated = entries.len() > arugula_proto::fs::LIST_MAX;
        entries.truncate(arugula_proto::fs::LIST_MAX);
        let parent = std::path::Path::new(&list.path).parent().map(|p| p.display().to_string());
        Ok(FsList { path: list.path, parent, entries, truncated })
    }

    async fn do_fs_read(&self, name: &str, path: &str, offset: u64, len: u64) -> anyhow::Result<(Vec<u8>, u64)> {
        let mut r = self.slow.get(self.fs_url(name, "read", path)).bearer_auth(&self.token).send().await?;
        if !r.status().is_success() {
            return Err(Self::fs_failed(name, path, r).await);
        }
        let size = r.content_length().unwrap_or(0);
        let (mut skip, mut out) = (offset, Vec::new());
        let read = async {
            // Dropping the response mid-file stops the agent's copy.
            while (out.len() as u64) < len
                && let Some(chunk) = r.chunk().await?
            {
                let from = skip.min(chunk.len() as u64) as usize;
                skip -= from as u64;
                let want = (len - out.len() as u64) as usize;
                let rest = &chunk[from..];
                out.extend_from_slice(&rest[..rest.len().min(want)]);
            }
            anyhow::Ok(())
        };
        tokio::time::timeout(Duration::from_secs(60), read).await??;
        Ok((out, size))
    }

    async fn do_put_service(&self, name: &str, service: &str, def: &ServiceDef) -> anyhow::Result<()> {
        let mut u = self.url(&format!("/{name}/services/{service}"));
        // Long enough to see it crash at once, short enough not to wait.
        u.query_pairs_mut().append_pair("duration", "2s");
        let env: serde_json::Map<String, serde_json::Value> =
            def.env.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
        let mut body = serde_json::json!({ "cmd": def.cmd, "args": def.args, "env": env });
        if let Some(d) = &def.dir {
            body["dir"] = d.clone().into();
        }
        let r = self.slow.put(u).bearer_auth(&self.token).json(&body).send().await?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("starting service {service} in {name}: {status} {}", text.trim());
        }
        // NDJSON: an `error` event means it couldn't start.
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
            if v["type"] == "error" {
                anyhow::bail!("service {service} in {name}: {}", v["data"].as_str().unwrap_or("failed"));
            }
        }
        Ok(())
    }

    async fn do_delete_service(&self, name: &str, service: &str) -> anyhow::Result<()> {
        let r =
            self.slow.delete(self.url(&format!("/{name}/services/{service}"))).bearer_auth(&self.token).send().await?;
        match r.status() {
            s if s.is_success() || s == StatusCode::NOT_FOUND => Ok(()),
            s => anyhow::bail!("removing service {service} from {name}: {s}"),
        }
    }
}

impl Provider for Sprites {
    fn name(&self) -> &str {
        self.name
    }

    fn caps(&self) -> &Caps {
        &self.caps
    }

    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<Sandbox>>> {
        async move {
            let mut all = Vec::new();
            let mut after: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let mut u = self.url("");
                {
                    let mut q = u.query_pairs_mut();
                    if !prefix.is_empty() {
                        q.append_pair("prefix", prefix);
                    }
                    if let Some(a) = &after {
                        q.append_pair("continuation_token", a);
                    }
                }
                let r = self.http.get(u).bearer_auth(&self.token).send().await?.error_for_status()?;
                let page: SpriteList = r.json().await?;
                all.extend(
                    page.sprites
                        .into_iter()
                        .filter(|s| s.name.starts_with(prefix))
                        .map(|s| Sandbox { name: s.name, status: s.status }),
                );
                match page.next_continuation_token {
                    Some(next) if page.has_more => after = Some(next),
                    _ => break,
                }
            }
            Ok(all)
        }
        .boxed()
    }

    fn status<'a>(&'a self, name: &'a str) -> BoxFuture<'a, anyhow::Result<Option<Sandbox>>> {
        self.get_status(name).boxed()
    }

    fn create<'a>(&'a self, name: &'a str, image: Option<&'a str>) -> BoxFuture<'a, anyhow::Result<()>> {
        self.do_create(name, image).boxed()
    }

    fn delete<'a>(&'a self, name: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        async move {
            let r = self.http.delete(self.url(&format!("/{name}"))).bearer_auth(&self.token).send().await?;
            match r.status() {
                s if s.is_success() || s == StatusCode::NOT_FOUND => Ok(()),
                s => anyhow::bail!("deleting {name}: {s}"),
            }
        }
        .boxed()
    }

    fn wake<'a>(&'a self, name: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        // Any exec wakes a sprite; this one returns as soon as it's up.
        async move {
            self.do_run(name, &["true"]).await?;
            Ok(())
        }
        .boxed()
    }

    fn dial<'a>(&'a self, name: &'a str, port: u16) -> BoxFuture<'a, io::Result<Conn>> {
        self.do_dial(name, port).boxed()
    }

    fn exec_tty(
        self: Arc<Self>,
        rt: &tokio::runtime::Handle,
        name: String,
        begin: Begin,
        size: (u16, u16),
        sink: ExecSink,
    ) -> Exec {
        let (tx, rx) = mpsc::unbounded_channel();
        rt.spawn(drive(self, name, begin, size, rx, sink));
        Exec { tx }
    }

    fn kill<'a>(&'a self, name: &'a str, session: &'a str, signal: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        self.do_kill(name, session, signal).boxed()
    }

    fn run<'a>(&'a self, name: &'a str, argv: &'a [&'a str]) -> BoxFuture<'a, anyhow::Result<(Vec<u8>, Option<i32>)>> {
        self.do_run(name, argv).boxed()
    }

    fn pipe<'a>(&'a self, name: &'a str, begin: PipeBegin) -> BoxFuture<'a, anyhow::Result<Pipe>> {
        self.do_pipe(name, begin).boxed()
    }

    fn write_file<'a>(
        &'a self,
        name: &'a str,
        path: &'a str,
        data: Vec<u8>,
        mode: u32,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        self.do_write(name, path, data, mode).boxed()
    }

    fn put_service<'a>(
        &'a self,
        name: &'a str,
        service: &'a str,
        def: &'a ServiceDef,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        self.do_put_service(name, service, def).boxed()
    }

    fn delete_service<'a>(&'a self, name: &'a str, service: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        self.do_delete_service(name, service).boxed()
    }

    fn fs_list<'a>(&'a self, name: &'a str, path: &'a str) -> BoxFuture<'a, anyhow::Result<FsList>> {
        self.do_fs_list(name, path).boxed()
    }

    fn fs_read<'a>(
        &'a self,
        name: &'a str,
        path: &'a str,
        offset: u64,
        len: u64,
    ) -> BoxFuture<'a, anyhow::Result<(Vec<u8>, u64)>> {
        self.do_fs_read(name, path, offset, len).boxed()
    }
}

/// `GET /fs/list`'s answer.
#[derive(Deserialize)]
struct WispList {
    path: String,
    #[serde(default)]
    entries: Vec<WispEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WispEntry {
    name: String,
    path: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: i64,
    /// Octal, `"0644"`.
    #[serde(default)]
    mode: String,
    /// RFC 3339, UTC.
    #[serde(default)]
    mod_time: String,
}

impl WispEntry {
    fn into_entry(self) -> FsEntry {
        let kind = match self.kind.as_str() {
            "directory" => FsKind::Directory,
            "symlink" => FsKind::Symlink,
            "file" => FsKind::File,
            _ => FsKind::Other,
        };
        FsEntry {
            name: self.name,
            path: self.path,
            kind,
            size: self.size.max(0) as u64,
            mode: u32::from_str_radix(&self.mode, 8).unwrap_or(0),
            mtime_ms: rfc3339_ms(&self.mod_time).unwrap_or(0),
            // The agent doesn't say what a link points at.
            target: None,
        }
    }
}

/// `2026-10-01T12:34:56.789Z` as milliseconds since the epoch (UTC only,
/// which is what the agent sends).
fn rfc3339_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || !s.ends_with('Z') {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let frac = s[19..s.len() - 1].strip_prefix('.').unwrap_or("");
    let ms = format!("{frac:0<3}").get(..3)?.parse::<i64>().ok()?;
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(((days * 24 + h) * 60 + mi) * 60 * 1000 + sec * 1000 + ms).ok()
}

async fn drive(
    sp: Arc<Sprites>,
    sprite: String,
    begin: Begin,
    mut size: (u16, u16),
    mut rx: mpsc::UnboundedReceiver<ExecInput>,
    sink: ExecSink,
) {
    let (mut session, mut received) = match begin {
        Begin::New { spawn, image, create } => {
            // New, or lost in a reboot of its host: (re)create it, if it's
            // ours to create. At boot wispd may not be answering yet; wait.
            let mut tries = 0;
            let made = loop {
                match sp.get_status(&sprite).await {
                    Ok(Some(_)) => break Ok(()),
                    Ok(None) if create => break sp.do_create(&sprite, image.as_deref()).await,
                    Ok(None) => break Err(anyhow::anyhow!("{sprite} doesn't exist")),
                    Err(e) if tries >= PATIENCE => break Err(e),
                    Err(_) => {
                        tries += 1;
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            };
            if let Err(e) = made {
                warn!(sprite, error = %e, "can't reach machine");
                sink(ExecEvent::Lost { machine_gone: true });
                return;
            }
            let url = sp.exec_url(&sprite, &spawn, size.0, size.1);
            (Err(url), 0)
        }
        Begin::Resume { session, received } => (Ok(session), received),
    };
    let exists = |sp: Arc<Sprites>, s: String| async move { sp.get_status(&s).await.map(|x| x.is_some()) };
    let mut failures = 0u32;
    let mut hung_up = false;
    loop {
        let url = match &session {
            Ok(id) => sp.attach_url(&sprite, id, received),
            Err(url) => url.clone(),
        };
        let ws = match sp.request(url) {
            Ok(req) => tokio_tungstenite::connect_async(req).await,
            Err(e) => {
                warn!(sprite, error = %e, "bad exec request");
                sink(ExecEvent::Lost { machine_gone: false });
                return;
            }
        };
        let mut ws = match ws {
            Ok((ws, _)) => ws,
            Err(e) => {
                // Refused: the sprite or the session is gone, or wispd is
                // restarting.
                match exists(sp.clone(), sprite.clone()).await {
                    Ok(false) => {
                        sink(ExecEvent::Lost { machine_gone: true });
                        return;
                    }
                    Ok(true) if session.is_ok() && failures >= 3 => {
                        info!(sprite, error = %e, "session gone");
                        sink(ExecEvent::Lost { machine_gone: false });
                        return;
                    }
                    _ if failures >= PATIENCE => {
                        sink(ExecEvent::Lost { machine_gone: false });
                        return;
                    }
                    _ => {}
                }
                failures += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        failures = 0;
        let mut ended = false;
        loop {
            tokio::select! {
                m = ws.next() => match m {
                    Some(Ok(Message::Binary(b))) => {
                        received += b.len() as u64;
                        if !sink(ExecEvent::Output(b.to_vec())) {
                            return;
                        }
                    }
                    Some(Ok(Message::Text(t))) => {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                        match v["type"].as_str() {
                            Some("session_info") => {
                                let id = v["session_id"].as_str().map(str::to_owned)
                                    .unwrap_or_else(|| v["session_id"].to_string());
                                session = Ok(id.clone());
                                sink(ExecEvent::Session(id));
                                // The size may have changed while we were
                                // away (or since the URL was made).
                                let r = serde_json::json!({"type": "resize", "cols": size.0, "rows": size.1});
                                let _ = ws.send(Message::Text(r.to_string().into())).await;
                            }
                            Some("exit") => {
                                ended = true;
                                sink(ExecEvent::Exited(v["exit_code"].as_i64().map(|c| c as i32)));
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
                i = rx.recv() => match i {
                    Some(ExecInput::Data(d)) => {
                        let _ = ws.send(Message::Binary(d.into())).await;
                    }
                    Some(ExecInput::Resize(c, r)) => {
                        size = (c, r);
                        let m = serde_json::json!({"type": "resize", "cols": c, "rows": r});
                        let _ = ws.send(Message::Text(m.to_string().into())).await;
                    }
                    Some(ExecInput::HangUp) if !hung_up => {
                        hung_up = true;
                        if let Ok(id) = &session {
                            let (sp, s, id) = (sp.clone(), sprite.clone(), id.clone());
                            tokio::spawn(async move {
                                if let Err(e) = sp.do_kill(&s, &id, "HUP").await {
                                    info!(sprite = s, error = %e, "hang up");
                                }
                            });
                        }
                    }
                    Some(ExecInput::HangUp) => {}
                    // The pane let go: detach, leaving the session running.
                    None => {
                        let _ = ws.close(None).await;
                        return;
                    }
                },
            }
        }
        if ended {
            return;
        }
        // Dropped without an exit: is the machine still there?
        match exists(sp.clone(), sprite.clone()).await {
            Ok(false) => {
                sink(ExecEvent::Lost { machine_gone: true });
                return;
            }
            Ok(true) if session.is_err() => {
                sink(ExecEvent::Lost { machine_gone: false });
                return;
            }
            _ => {
                info!(sprite, received, "exec dropped; reattaching");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn dial_proxy(sp: &Sprites, sprite: &str, port: u16) -> io::Result<Conn> {
    let req = sp.request(sp.ws_url(&format!("/{sprite}/proxy"))).map_err(io::Error::other)?;
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| match e {
        tokio_tungstenite::tungstenite::Error::Http(r) if r.status() == 404 => {
            io::Error::new(io::ErrorKind::NotFound, format!("machine {sprite} is gone"))
        }
        e => io::Error::other(format!("Sprites proxy: {e}")),
    })?;
    let hello = serde_json::json!({ "host": "localhost", "port": port }).to_string();
    ws.send(Message::Text(hello.into())).await.map_err(io::Error::other)?;
    // The first answer says whether the port took the connection.
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                match v["status"].as_str() {
                    Some("connected") => break,
                    _ => {
                        let why = v["error"].as_str().unwrap_or("refused").to_owned();
                        let kind = if why.contains("refused") {
                            io::ErrorKind::ConnectionRefused
                        } else {
                            io::ErrorKind::Other
                        };
                        return Err(io::Error::new(kind, format!("nothing is answering on port {port}: {why}")));
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None => {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "the Sprites proxy closed"));
            }
            Some(Err(e)) => return Err(io::Error::other(e)),
            Some(Ok(_)) => {}
        }
    }
    // Hand back one end of a pipe; a task moves bytes between the other end
    // and the socket. The pipe's buffer is the backpressure.
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let (mut from_us, mut to_us) = tokio::io::split(theirs);
        let (mut sink, mut stream) = ws.split();
        let up = async {
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                match from_us.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sink.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = sink.close().await;
        };
        let down = async {
            while let Some(m) = stream.next().await {
                match m {
                    Ok(Message::Binary(b)) => {
                        if to_us.write_all(&b).await.is_err() {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            let _ = to_us.shutdown().await;
        };
        tokio::pin!(up, down);
        tokio::select! {
            // We're done sending: let the answer finish, briefly.
            _ = &mut up => { let _ = tokio::time::timeout(Duration::from_secs(5), down).await; }
            // The port closed: so does our end.
            _ = &mut down => {}
        }
    });
    Ok(Box::new(ours))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_times_and_paths() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(rfc3339_ms("2026-10-01T12:00:00.5Z"), Some(1_790_856_000_500));
        assert_eq!(rfc3339_ms("2000-03-01T00:00:00.123456789Z"), Some(951_868_800_123));
        assert_eq!(rfc3339_ms("garbage"), None);
        let wisp = Sprites::new("http://127.0.0.1:7788", "t".into()).unwrap();
        let q = |p: &str| wisp.fs_url("s", "list", p).query().unwrap().to_owned();
        assert_eq!(q("~"), "path=.");
        assert_eq!(q("~/src/app"), "path=src%2Fapp");
        assert_eq!(q("/etc"), "path=%2Fetc");
        assert_eq!(q(""), "path=.");
        assert_eq!(q("~other"), "path=%7Eother");
    }

    #[test]
    fn capabilities_follow_the_api() {
        let wisp = Sprites::new("http://127.0.0.1:7788", "t".into()).unwrap();
        assert_eq!(wisp.name(), "wisp");
        assert_eq!(wisp.caps().exec_replay, 1 << 20);
        assert_eq!(wisp.caps().cold, Cold::Reboot);
        assert!(!wisp.caps().reattach_owner);
        let fly = Sprites::new("https://api.sprites.dev", "t".into()).unwrap();
        assert_eq!(fly.name(), "sprites");
        assert!(fly.caps().exec_replay < 8 * 1024, "Fly keeps about 6.5 KB");
        assert_eq!(fly.caps().cold, Cold::Restore);
        assert!(fly.caps().reattach_owner);
        assert!(wisp.caps().fs_browse && fly.caps().fs_browse);
        // The token never shows in debug output.
        assert!(!format!("{wisp:?}").contains("\"t\""));
    }
}
