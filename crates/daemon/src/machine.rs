//! Machines other than this host that panes run on: throwaway wisp sprites
//! (Firecracker microVMs on a host running wispd, behind the Sprites API).
//!
//! A VM pane's terminal is an exec TTY session on its sprite: a WebSocket
//! carrying the program's output as binary frames, `session_info` and `exit`
//! as JSON text frames, and our input and resizes. The daemon runs its own
//! terminal engine and log over those bytes, as for a local pane, so
//! scrollback, snapshots, `tail` and history don't depend on wisp.
//!
//! - **Create** is nearly free and doesn't boot anything; the first exec
//!   does (about 0.3s to a prompt; spike M3b).
//! - **Detach and reattach**: the session outlives the WebSocket for
//!   `max_run_after_disconnect`, so a daemon restart reattaches. wisp
//!   resends output past `output_offset`, which is the count of bytes we
//!   already have, so nothing is duplicated or lost.
//! - **Machine gone**: the WebSocket drops without an `exit` frame, and the
//!   sprite is 404. A drop with the sprite still there is a network blip:
//!   reattach.
//! - **Close**: delete the sprite, which ends every session on it at once.
//!
//! This is the exec half of the M4b `Provider`: create, exec, resize, kill,
//! delete.

use std::{path::Path, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tracing::{info, warn};

use crate::pane::Spawn;

/// How long a session survives with nobody attached: longer than any
/// daemon restart or upgrade.
const DETACHED_FOR: &str = "12h";

/// Tries, half a second apart, before giving up on a wispd that isn't
/// answering (it may still be starting, at boot).
const WISP_PATIENCE: u32 = 120;

/// A wispd, by its API URL and bearer token.
pub struct Wisp {
    base: Url,
    token: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct SpriteList {
    #[serde(default)]
    sprites: Vec<SpriteEntry>,
}

#[derive(Deserialize)]
struct SpriteEntry {
    name: String,
}

impl std::fmt::Debug for Wisp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wisp").field("base", &self.base.as_str()).finish_non_exhaustive()
    }
}

impl Wisp {
    pub fn new(base: &str, token: String) -> anyhow::Result<Self> {
        let base = Url::parse(base)?;
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self { base, token, http })
    }

    /// From a URL and a token file; `None` (VM panes unavailable) if there's
    /// no token.
    pub fn open(base: &str, token_file: &Path) -> Option<Self> {
        let token = std::fs::read_to_string(token_file).ok()?.trim().to_owned();
        if token.is_empty() {
            return None;
        }
        match Self::new(base, token) {
            Ok(w) => Some(w),
            Err(e) => {
                warn!(error = %e, base, "bad wisp URL; VM panes are off");
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

    async fn call(&self, method: reqwest::Method, path: &str) -> reqwest::Result<reqwest::Response> {
        self.http.request(method, self.url(path)).bearer_auth(&self.token).send().await
    }

    /// Create a sprite. Already existing counts as created.
    pub async fn create(&self, name: &str, image: Option<&str>) -> anyhow::Result<()> {
        let mut body = serde_json::json!({ "name": name });
        if let Some(i) = image {
            body["from"] = serde_json::json!({ "image": i });
        }
        let r = self.http.post(self.url("")).bearer_auth(&self.token).json(&body).send().await?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::BAD_REQUEST if text.contains("name_taken") => Ok(()),
            s => anyhow::bail!("creating {name}: {s} {}", text.trim()),
        }
    }

    /// Delete a sprite, ending everything on it. Already gone is fine.
    pub async fn delete(&self, name: &str) -> anyhow::Result<()> {
        let r = self.call(reqwest::Method::DELETE, &format!("/{name}")).await?;
        match r.status() {
            s if s.is_success() || s == StatusCode::NOT_FOUND => Ok(()),
            s => anyhow::bail!("deleting {name}: {s}"),
        }
    }

    /// Whether the sprite exists. Doesn't wake it.
    pub async fn exists(&self, name: &str) -> anyhow::Result<bool> {
        let r = self.call(reqwest::Method::GET, &format!("/{name}")).await?;
        match r.status() {
            s if s.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            s => anyhow::bail!("looking up {name}: {s}"),
        }
    }

    /// Names of the sprites starting with `prefix`.
    pub async fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut u = self.url("");
        u.query_pairs_mut().append_pair("prefix", prefix);
        let r = self.http.get(u).bearer_auth(&self.token).send().await?.error_for_status()?;
        let list: SpriteList = r.json().await?;
        Ok(list.sprites.into_iter().map(|s| s.name).filter(|n| n.starts_with(prefix)).collect())
    }

    /// Send a signal to one session's process (HUP ends a shell at once;
    /// wisp's default TERM is ignored by an interactive bash).
    pub async fn kill(&self, name: &str, session: &str, signal: &str) -> anyhow::Result<()> {
        let mut u = self.url(&format!("/{name}/exec/{session}/kill"));
        u.query_pairs_mut().append_pair("signal", signal).append_pair("timeout", "3s");
        self.http.post(u).bearer_auth(&self.token).send().await?.error_for_status()?;
        Ok(())
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

    /// Run a command to completion without a terminal: (stdout, exit code).
    pub async fn run(&self, name: &str, argv: &[&str]) -> anyhow::Result<(Vec<u8>, Option<i32>)> {
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
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            anyhow::Ok(())
        };
        tokio::time::timeout(Duration::from_secs(10), read).await??;
        Ok((out, code))
    }

    /// The Sprites proxy to `name`'s ports: one WebSocket per TCP connection
    /// (the protocol is in `ports.rs`).
    pub fn proxy_request(
        &self,
        name: &str,
    ) -> anyhow::Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
        self.request(self.ws_url(&format!("/{name}/proxy")))
    }

    /// Make sure the sprite exists, creating it if not; waits for a wispd
    /// that isn't answering yet (at boot).
    pub async fn ensure(&self, name: &str, image: Option<&str>) -> anyhow::Result<()> {
        let mut tries = 0;
        loop {
            match self.exists(name).await {
                Ok(true) => return Ok(()),
                Ok(false) => return self.create(name, image).await,
                Err(e) if tries >= WISP_PATIENCE => return Err(e),
                Err(_) => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    /// A non-TTY exec with stdin (an agent server's pipes): stdin frames are
    /// `0` + data, output `1` (stdout) and `2` (stderr) + data, `3` + exit
    /// code.
    pub fn pipe_exec(
        &self,
        name: &str,
        argv: &[String],
    ) -> anyhow::Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
        let mut u = self.ws_url(&format!("/{name}/exec"));
        {
            let mut q = u.query_pairs_mut();
            for a in argv {
                q.append_pair("cmd", a);
            }
            q.append_pair("stdin", "true");
            q.append_pair("max_run_after_disconnect", DETACHED_FOR);
        }
        self.request(u)
    }

    /// Reattach to a non-TTY exec, past the output we already have.
    pub fn pipe_attach(
        &self,
        name: &str,
        session: &str,
        offset: u64,
    ) -> anyhow::Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
        self.request(self.attach_url(name, session, offset))
    }

    fn request(&self, url: Url) -> anyhow::Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
        let mut req = url.as_str().into_client_request()?;
        req.headers_mut().insert("Authorization", format!("Bearer {}", self.token).parse()?);
        Ok(req)
    }
}

/// What an exec reports to its pane.
#[derive(Debug)]
pub enum ExecEvent {
    /// Attached to this session (first time or again).
    Session(String),
    Output(Vec<u8>),
    /// The program ended.
    Exited(Option<i32>),
    /// The machine is gone, or the session can't be reached.
    Lost {
        machine_gone: bool,
    },
}

enum ExecInput {
    Data(Vec<u8>),
    Resize(u16, u16),
    HangUp,
}

/// How to begin: a new session for `spawn`, or reattach to one we were
/// following, having received `received` bytes of it.
pub enum Begin {
    New { spawn: Spawn, image: Option<String> },
    Resume { session: String, received: u64 },
}

/// A pane's terminal on a machine. Dropping it detaches, leaving the session
/// running for the next daemon.
pub struct Exec {
    tx: mpsc::UnboundedSender<ExecInput>,
}

impl Exec {
    pub fn input(&self, data: Vec<u8>) {
        let _ = self.tx.send(ExecInput::Data(data));
    }
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.tx.send(ExecInput::Resize(cols, rows));
    }
    pub fn hang_up(&self) {
        let _ = self.tx.send(ExecInput::HangUp);
    }
}

/// Start (or resume) a session on `sprite`. Events go to `sink` in order,
/// from a task on `rt`.
pub fn start(
    rt: &tokio::runtime::Handle,
    wisp: Arc<Wisp>,
    sprite: String,
    begin: Begin,
    size: (u16, u16),
    sink: impl Fn(ExecEvent) -> bool + Send + 'static,
) -> Exec {
    let (tx, rx) = mpsc::unbounded_channel();
    rt.spawn(drive(wisp, sprite, begin, size, rx, sink));
    Exec { tx }
}

async fn drive(
    wisp: Arc<Wisp>,
    sprite: String,
    begin: Begin,
    mut size: (u16, u16),
    mut rx: mpsc::UnboundedReceiver<ExecInput>,
    sink: impl Fn(ExecEvent) -> bool,
) {
    let (mut session, mut received) = match begin {
        Begin::New { spawn, image } => {
            // New, or lost in a reboot of its host: (re)create it. At boot
            // wispd may not be answering yet; wait for it.
            let mut tries = 0;
            let made = loop {
                match wisp.exists(&sprite).await {
                    Ok(true) => break Ok(()),
                    Ok(false) => break wisp.create(&sprite, image.as_deref()).await,
                    Err(e) if tries >= WISP_PATIENCE => break Err(e),
                    Err(_) => {
                        tries += 1;
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
            };
            if let Err(e) = made {
                warn!(sprite, error = %e, "can't create machine");
                sink(ExecEvent::Lost { machine_gone: true });
                return;
            }
            let url = wisp.exec_url(&sprite, &spawn, size.0, size.1);
            (Err(url), 0)
        }
        Begin::Resume { session, received } => (Ok(session), received),
    };
    let mut failures = 0u32;
    let mut hung_up = false;
    loop {
        let url = match &session {
            Ok(id) => wisp.attach_url(&sprite, id, received),
            Err(url) => url.clone(),
        };
        let ws = match wisp.request(url) {
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
                match wisp.exists(&sprite).await {
                    Ok(false) => {
                        sink(ExecEvent::Lost { machine_gone: true });
                        return;
                    }
                    Ok(true) if session.is_ok() && failures >= 3 => {
                        info!(sprite, error = %e, "session gone");
                        sink(ExecEvent::Lost { machine_gone: false });
                        return;
                    }
                    _ if failures >= WISP_PATIENCE => {
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
                            let (w, s, id) = (wisp.clone(), sprite.clone(), id.clone());
                            tokio::spawn(async move {
                                if let Err(e) = w.kill(&s, &id, "HUP").await {
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
        match wisp.exists(&sprite).await {
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
