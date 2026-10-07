//! arugulad as Claude Code's IDE (M28, S17: go, as a complement to M29's
//! hook).
//!
//! Claude Code in a pane finds us by `CLAUDE_CODE_SSE_PORT`, which every
//! pane gets, and sends each Edit and Write it wants to make as `openDiff`
//! (only those: Bash, MCP tools and questions stay with M29's hook). The
//! edit becomes a diff card on the pane and on the swarm's rail; accepting
//! it (as proposed, or changed first) or rejecting it answers Claude Code,
//! which then writes the file or doesn't. When the terminal answers first,
//! Claude Code closes the diff (`close_tab`) and the card goes.
//!
//! The connections themselves are held by a relay (`relay.rs`), a process
//! of its own that outlives a daemon restart; this side is the daemon's
//! link to it. "Which IDE gets diffs" is a setting here: Arugula's cards,
//! or another IDE that registered with Claude Code (VS Code with Claude
//! Code's extension, say), to which the daemon passes each `openDiff` on.

pub mod diff;
// Over Unix sockets, which Claude Code's IDE support uses.
#[cfg(unix)]
pub mod relay;

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};
/// The relay's socket. Windows: its named pipe comes with the daemon's (M56,
/// #219); a stream type stands in until then.
#[cfg(not(unix))]
type UnixStream = tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tracing::{info, warn};

use crate::{
    mux::{Api, Cmd, MuxHandle},
    pane::Launcher,
};

/// What we're called in Claude Code's `/ide` list.
pub const NAME: &str = "arugula";
/// What we were called before the rename (#505), in lockfiles a relay
/// started by an older daemon wrote.
const OLD_NAME: &str = arugula_proto::rename::OLD;

/// Where Claude Code looks for IDEs: `$CLAUDE_CONFIG_DIR/ide`, else
/// `~/.claude/ide`.
pub fn default_lock_dir(home: &Path) -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) => PathBuf::from(d).join("ide"),
        None => home.join(".claude/ide"),
    }
}

/// What the relay tells the daemon.
#[derive(Debug, Clone)]
pub enum Event {
    /// The relay (again): what follows is everything open.
    Hello,
    /// A Claude Code connected; the process it is.
    Conn {
        conn: u64,
        pid: Option<u32>,
    },
    Gone {
        conn: u64,
    },
    /// A tool call that waits for an answer.
    Call {
        conn: u64,
        id: Value,
        tool: String,
        args: Value,
    },
    /// Claude Code closed these diffs (its terminal answered).
    Closed {
        conn: u64,
        ids: Vec<Value>,
    },
}

/// Which IDE gets diffs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Prefs {
    /// Another IDE's name as it registers (`Visual Studio Code`); none:
    /// Arugula's own cards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diffs: Option<String>,
}

/// Another IDE registered with Claude Code.
#[derive(Debug, Clone, Serialize)]
pub struct Other {
    pub name: String,
    pub port: u16,
    pub pid: Option<u32>,
    pub folders: Vec<String>,
    pub alive: bool,
    #[serde(skip)]
    token: String,
}

impl std::fmt::Debug for Ide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ide").field("port", &self.port).field("lock_dir", &self.lock_dir).finish_non_exhaustive()
    }
}

pub struct Ide {
    /// The port Claude Code connects to (`CLAUDE_CODE_SSE_PORT`).
    pub port: u16,
    dir: PathBuf,
    pub lock_dir: PathBuf,
    launch: Launcher,
    tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
    /// The first link, made at startup, until `run` takes it.
    first: Mutex<Option<UnixStream>>,
    prefs: Mutex<Prefs>,
    /// Diffs passed to another IDE, by (connection, call id): which, and
    /// the tab Claude Code named.
    forwarded: Mutex<HashMap<(u64, String), (Other, String)>>,
}

impl Ide {
    /// Reach the relay, starting one if none runs, and learn its port.
    pub async fn start(dir: PathBuf, lock_dir: PathBuf, launch: Launcher) -> io::Result<Arc<Self>> {
        std::fs::create_dir_all(&dir)?;
        let (stream, port) = connect(&dir, &lock_dir, &launch).await?;
        let prefs = std::fs::read(dir.join("prefs.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        info!(port, lock_dir = %lock_dir.display(), "Claude Code's IDE");
        Ok(Arc::new(Self {
            port,
            dir,
            lock_dir,
            launch,
            tx: Mutex::new(None),
            first: Mutex::new(Some(stream)),
            prefs: Mutex::new(prefs),
            forwarded: Mutex::new(HashMap::new()),
        }))
    }

    /// Pass what the relay says to the multiplexer, for as long as the
    /// daemon runs; reach the relay again (or start one) if it goes.
    pub fn run(self: &Arc<Self>, mux: MuxHandle) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut stream = me.first.lock().unwrap().take();
            loop {
                let s = match stream.take() {
                    Some(s) => s,
                    None => match connect(&me.dir, &me.lock_dir, &me.launch).await {
                        Ok((s, port)) => {
                            if port != me.port {
                                warn!(
                                    was = me.port,
                                    now = port,
                                    "the IDE relay's port moved: new panes get the new one"
                                );
                            }
                            s
                        }
                        Err(e) => {
                            warn!(error = %e, "can't reach the IDE relay");
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            continue;
                        }
                    },
                };
                me.pump(s, &mux).await;
                *me.tx.lock().unwrap() = None;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    }

    async fn pump(&self, s: UnixStream, mux: &MuxHandle) {
        let (r, mut w) = s.into_split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *self.tx.lock().unwrap() = Some(tx);
        let writer = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if w.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let conn = v["conn"].as_u64().unwrap_or(0);
            let ev = match v["t"].as_str().unwrap_or("") {
                "hello" => Event::Hello,
                "conn" => Event::Conn { conn, pid: v["pid"].as_u64().map(|p| p as u32) },
                "gone" => Event::Gone { conn },
                "call" => Event::Call {
                    conn,
                    id: v["id"].clone(),
                    tool: v["tool"].as_str().unwrap_or("").to_owned(),
                    args: v["args"].clone(),
                },
                "closed" => Event::Closed { conn, ids: v["ids"].as_array().cloned().unwrap_or_default() },
                _ => continue,
            };
            mux.send(Cmd::Api(Api::Ide(ev)));
        }
        writer.abort();
    }

    fn line(&self, v: Value) {
        if let Some(tx) = &*self.tx.lock().unwrap() {
            let _ = tx.send(v.to_string());
        }
    }

    /// Answer a call.
    pub fn reply(&self, conn: u64, id: &Value, result: Value) {
        self.line(json!({ "t": "result", "conn": conn, "id": id, "result": result }));
    }

    /// Tell Claude Code something (`selection_changed`, `at_mentioned`):
    /// one connection, or every one (`None`).
    pub fn notify(&self, conn: Option<u64>, method: &str, params: Value) {
        self.line(json!({ "t": "notify", "conn": conn.unwrap_or(0), "method": method, "params": params }));
    }

    /// Where diffs go: `None` for Arugula's cards.
    pub fn diffs_to(&self) -> Option<String> {
        self.prefs.lock().unwrap().diffs.clone()
    }

    pub fn set_diffs_to(&self, name: Option<String>) -> io::Result<()> {
        let name = name.filter(|n| !n.is_empty() && n != NAME && n != OLD_NAME);
        let mut p = self.prefs.lock().unwrap();
        p.diffs = name;
        crate::store::write_atomic(&self.dir.join("prefs.json"), &serde_json::to_vec(&*p)?)
    }

    /// The other IDEs registered with Claude Code (newest first).
    pub fn others(&self) -> Vec<Other> {
        let mut out = vec![];
        let Ok(dir) = std::fs::read_dir(&self.lock_dir) else { return out };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = dir
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .filter(|(_, p)| p.extension().is_some_and(|x| x == "lock"))
            .collect();
        files.sort_by_key(|f| std::cmp::Reverse(f.0));
        for (_, path) in files {
            let Some(port) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u16>().ok()) else {
                continue;
            };
            if port == self.port {
                continue;
            }
            let Some(v) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
                continue;
            };
            // Ours under the old name (#505), not another IDE.
            if v["ideName"] == OLD_NAME {
                continue;
            }
            let pid = v["pid"].as_u64().map(|p| p as u32);
            out.push(Other {
                name: v["ideName"].as_str().unwrap_or("?").to_owned(),
                port,
                alive: pid.is_some_and(crate::procinfo::alive),
                pid,
                folders: v["workspaceFolders"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default(),
                token: v["authToken"].as_str().unwrap_or("").to_owned(),
            });
        }
        out
    }

    /// The IDE diffs go to now, if it isn't us and it's there.
    pub fn target(&self) -> Option<Other> {
        let name = self.diffs_to()?;
        self.others().into_iter().find(|o| o.alive && o.name == name)
    }

    /// Pass a diff to `to`, and its answer back to Claude Code.
    pub fn forward(self: &Arc<Self>, to: Other, conn: u64, id: Value, args: Value, back: mpsc::UnboundedSender<Cmd>) {
        let tab = args["tab_name"].as_str().unwrap_or("").to_owned();
        self.forwarded.lock().unwrap().insert((conn, id.to_string()), (to.clone(), tab));
        let me = self.clone();
        tokio::spawn(async move {
            info!(ide = to.name, port = to.port, "passing a diff on");
            let result = match call(&to, "openDiff", args.clone()).await {
                Ok(r) => r,
                Err(e) => {
                    // Shown here instead.
                    warn!(ide = to.name, error = %e, "couldn't pass a diff on; it waits here");
                    me.forwarded.lock().unwrap().remove(&(conn, id.to_string()));
                    let tool = "openDiff/here".to_owned();
                    let _ = back.send(Cmd::Api(Api::Ide(Event::Call { conn, id, tool, args })));
                    return;
                }
            };
            if me.forwarded.lock().unwrap().remove(&(conn, id.to_string())).is_some() {
                me.reply(conn, &id, result);
            }
        });
    }

    /// Claude Code closed diffs: close any we passed on, there too.
    pub fn closed(&self, conn: u64, ids: &[Value]) {
        for id in ids {
            let Some((to, tab)) = self.forwarded.lock().unwrap().remove(&(conn, id.to_string())) else { continue };
            tokio::spawn(async move {
                let _ = call(&to, "close_tab", json!({ "tab_name": tab })).await;
            });
        }
    }
}

#[cfg(not(unix))]
async fn connect(_dir: &Path, _lock_dir: &Path, _launch: &Launcher) -> io::Result<(UnixStream, u16)> {
    Err(io::Error::other("the IDE relay isn't on Windows yet (M56, #219)"))
}

/// Connect to the relay (starting it if it isn't there), and read its
/// hello for the port.
#[cfg(unix)]
async fn connect(dir: &Path, lock_dir: &Path, launch: &Launcher) -> io::Result<(UnixStream, u16)> {
    let sock = dir.join("relay.sock");
    let mut started = false;
    let t0 = std::time::Instant::now();
    let mut s = loop {
        match UnixStream::connect(&sock).await {
            Ok(s) => break s,
            Err(e) if t0.elapsed() > Duration::from_secs(5) => return Err(e),
            Err(_) => {
                if !started {
                    spawn_relay(dir, lock_dir, launch)?;
                    started = true;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }
    };
    // Its hello: `{"t":"hello","port":N}`, one byte at a time so nothing
    // after it is read here.
    let mut line = Vec::new();
    loop {
        let mut b = [0u8; 1];
        let n = tokio::time::timeout(Duration::from_secs(5), tokio::io::AsyncReadExt::read(&mut s, &mut b))
            .await
            .map_err(|_| io::Error::other("the IDE relay said nothing"))??;
        if n == 0 {
            return Err(io::Error::other("the IDE relay hung up"));
        }
        if b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
    }
    let hello: Value = serde_json::from_slice(&line).map_err(io::Error::other)?;
    let port = hello["port"].as_u64().ok_or_else(|| io::Error::other("no port from the IDE relay"))? as u16;
    rename_locks(lock_dir, port);
    Ok((s, port))
}

/// Lockfiles under the old name (#505). The relay outlives a daemon
/// update, so one an older daemon started still says `illogical`, and
/// that's what Claude Code's `/ide` shows: it says `arugula` from here.
/// One whose relay is gone (killed, so it never removed it) goes.
#[cfg(unix)]
fn rename_locks(lock_dir: &Path, port: u16) {
    let Ok(dir) = std::fs::read_dir(lock_dir) else { return };
    for path in dir.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "lock")) {
        let Some(mut v) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
            continue;
        };
        if v["ideName"] != OLD_NAME {
            continue;
        }
        let this = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u16>().ok()) == Some(port);
        let r = if this {
            v["ideName"] = json!(NAME);
            crate::store::write_atomic(&path, v.to_string().as_bytes())
        } else if !v["pid"].as_u64().is_some_and(|p| crate::procinfo::alive(p as u32)) {
            std::fs::remove_file(&path)
        } else {
            continue;
        };
        if let Err(e) = r {
            warn!(lock = %path.display(), error = %e, "an IDE lockfile under the old name");
        }
    }
}

#[cfg(unix)]
/// The relay, in a scope (or process group) of its own so it outlives us.
fn spawn_relay(dir: &Path, lock_dir: &Path, launch: &Launcher) -> io::Result<()> {
    let unit = format!("arugula-ide-relay-{}", std::process::id());
    let mut c = launch.command(&unit);
    c.arg("_ide_relay").arg(dir).arg(lock_dir);
    #[cfg(unix)]
    if !launch.scopes {
        std::os::unix::process::CommandExt::process_group(&mut c, 0);
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("relay.log"))?;
    c.stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    let child = c.spawn()?;
    info!(pid = child.id(), "started the IDE relay");
    // It's on its own from here; reap it when it ends.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

/// One MCP tool call to another IDE: connect, initialize, call, wait.
async fn call(to: &Other, tool: &str, args: Value) -> anyhow::Result<Value> {
    let mut req = format!("ws://127.0.0.1:{}", to.port).into_client_request()?;
    let h = req.headers_mut();
    h.insert("x-claude-code-ide-authorization", to.token.parse()?);
    h.insert("sec-websocket-protocol", "mcp".parse()?);
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await?;
    let send = |v: Value| Message::Text(v.to_string().into());
    ws.send(send(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": NAME, "version": env!("CARGO_PKG_VERSION") },
    }})))
    .await?;
    wait(&mut ws, 1).await?;
    ws.send(send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))).await?;
    ws.send(send(
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": tool, "arguments": args } }),
    ))
    .await?;
    let r = wait(&mut ws, 2).await?;
    let _ = ws.close(None).await;
    Ok(r)
}

async fn wait<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>, id: u64) -> anyhow::Result<Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(m) = ws.next().await {
        let Message::Text(t) = m? else { continue };
        let v: Value = serde_json::from_str(&t)?;
        if v["id"] == id {
            if let Some(e) = v.get("error") {
                anyhow::bail!("{e}");
            }
            return Ok(v["result"].clone());
        }
    }
    anyhow::bail!("it hung up")
}

/// `openDiff`'s answers, as Claude Code reads them.
pub fn saved(contents: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": "FILE_SAVED" }, { "type": "text", "text": contents }] })
}

pub fn rejected(tab: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": "DIFF_REJECTED" }, { "type": "text", "text": tab }] })
}

/// `getDiagnostics` with nothing to say.
pub fn no_diagnostics() -> Value {
    json!({ "content": [{ "type": "text", "text": "[]" }] })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn lock(dir: &Path, port: u16, name: &str, pid: u32) -> PathBuf {
        let path = dir.join(format!("{port}.lock"));
        let v = json!({ "pid": pid, "workspaceFolders": [], "ideName": name, "transport": "ws", "authToken": "t" });
        std::fs::write(&path, v.to_string()).unwrap();
        path
    }

    fn name(path: &Path) -> Value {
        serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap()["ideName"].clone()
    }

    #[test]
    fn old_named_locks_are_renamed_or_cleared() {
        let dir = std::env::temp_dir().join(format!("arugula-ide-locks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let me = std::process::id();
        let gone = {
            let mut c = std::process::Command::new("true").spawn().unwrap();
            let pid = c.id();
            c.wait().unwrap();
            pid
        };
        // Our relay, started by an older daemon: renamed, the rest kept.
        let ours = lock(&dir, 4100, "illogical", me);
        // A relay of ours that died without removing its lockfile: gone.
        let stale = lock(&dir, 4101, "illogical", gone);
        // Another old one still running (a second daemon): left.
        let other = lock(&dir, 4102, "illogical", me);
        // Other IDEs, alive or not: left.
        let vscode = lock(&dir, 4103, "Visual Studio Code", gone);
        rename_locks(&dir, 4100);
        assert_eq!(name(&ours), "arugula");
        let v: Value = serde_json::from_slice(&std::fs::read(&ours).unwrap()).unwrap();
        assert_eq!(v["authToken"], "t");
        assert_eq!(v["pid"], me);
        assert!(!stale.exists());
        assert_eq!(name(&other), "illogical");
        assert_eq!(name(&vscode), "Visual Studio Code");
        // Again: nothing more.
        rename_locks(&dir, 4100);
        assert_eq!(name(&ours), "arugula");
        assert!(vscode.exists() && other.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
