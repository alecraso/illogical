//! The IDE relay (M28): what Claude Code connects to when arugulad is its
//! IDE. A small process of its own, so a daemon restart keeps every Claude
//! Code connected: Claude Code doesn't reconnect by itself (S17), and a
//! dropped connection would cost each running Claude its IDE until someone
//! typed `/ide`.
//!
//! - **Towards Claude Code** it's an IDE as S17 found them: a lockfile in
//!   `~/.claude/ide/<port>.lock` (mode 0600, **no workspace folders**, so
//!   only Claude Code started with our `CLAUDE_CODE_SSE_PORT`, in a pane,
//!   picks it), and MCP over a WebSocket on loopback that checks the token
//!   and refuses any upgrade with an `Origin` (a browser).
//! - **It answers MCP itself:** `initialize`, `tools/list`, and the tools
//!   that need nothing from the daemon. `close_tab` and `closeAllDiffTabs`
//!   close the diffs they name here (Claude Code calls them when the
//!   terminal answered first) and tell the daemon.
//! - **`openDiff` and `getDiagnostics` go to the daemon** over a Unix socket
//!   (`relay.sock`, lines of JSON) and wait there for its answer. Calls wait
//!   while no daemon is connected, and a daemon that connects (after a
//!   restart) is told every connection and every call still open, so the
//!   cards come back.
//! - **It stops** when no daemon has connected for a while: soon if no
//!   Claude Code is connected either, after [`GRACE`] if one is. Then it
//!   removes its lockfile.

use std::{
    collections::{BTreeMap, HashMap},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, UnixListener, UnixStream},
    sync::mpsc,
};
use tokio_tungstenite::tungstenite::{
    Message,
    handshake::server::{ErrorResponse, Request, Response},
    http::{HeaderValue, StatusCode},
};
use tracing::{debug, info, warn};

/// How long it keeps Claude Code connected with no daemon (a restart, an
/// upgrade).
pub const GRACE: Duration = Duration::from_secs(60);
/// ...and how long with nothing to keep.
const IDLE: Duration = Duration::from_secs(5);

/// What the tools Claude Code may call are.
const TOOLS: &[(&str, &str, &[&str])] = &[
    (
        "openDiff",
        "Show a proposed edit as a diff and wait for it to be accepted or rejected",
        &["old_file_path", "new_file_path", "new_file_contents", "tab_name"],
    ),
    ("close_tab", "Close a diff", &["tab_name"]),
    ("closeAllDiffTabs", "Close every diff", &[]),
    ("getDiagnostics", "Diagnostics for a file, or every file", &["uri"]),
    ("openFile", "Open a file", &["filePath"]),
    ("getCurrentSelection", "The selection", &[]),
    ("getLatestSelection", "The last selection", &[]),
    ("getOpenEditors", "Open editors", &[]),
    ("getWorkspaceFolders", "Workspace folders", &[]),
    ("checkDocumentDirty", "Whether a file has unsaved changes", &["filePath"]),
    ("saveDocument", "Save a file", &["filePath"]),
];

/// The tools the daemon answers.
const TO_DAEMON: &[&str] = &["openDiff", "getDiagnostics"];

pub struct Args {
    /// `<state>/ide`: the token, the port, `relay.sock`.
    pub dir: PathBuf,
    /// Where Claude Code looks for IDEs (`~/.claude/ide`).
    pub lock_dir: PathBuf,
}

/// One Claude Code.
struct Conn {
    pid: Option<u32>,
    tx: mpsc::UnboundedSender<Message>,
}

/// A call waiting for the daemon.
#[derive(Clone)]
struct Call {
    conn: u64,
    id: Value,
    tool: String,
    args: Value,
}

#[derive(Default)]
struct St {
    conns: HashMap<u64, Conn>,
    /// By (connection, the call's id as JSON).
    calls: BTreeMap<(u64, String), Call>,
    daemon: Option<(u64, mpsc::UnboundedSender<String>)>,
    next_conn: u64,
    next_daemon: u64,
    /// When the daemon (or the last one) went.
    alone_since: Option<Instant>,
}

type Shared = Arc<Mutex<St>>;

/// Run the relay until it's no longer needed.
pub async fn run(args: Args) -> io::Result<()> {
    std::fs::create_dir_all(&args.dir)?;
    let sock = args.dir.join("relay.sock");
    if UnixStream::connect(&sock).await.is_ok() {
        info!("an IDE relay is already running");
        return Ok(());
    }
    let token = token(&args.dir)?;
    let listener = bind(&args.dir).await?;
    let port = listener.local_addr()?.port();
    let _ = std::fs::remove_file(&sock);
    let local = UnixListener::bind(&sock)?;
    std::fs::set_permissions(&sock, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    let lock = lockfile(&args.lock_dir, port, &token)?;
    info!(port, lock = %lock.display(), "IDE relay listening");
    let st: Shared = Arc::new(Mutex::new(St { next_conn: 1, alone_since: Some(Instant::now()), ..St::default() }));
    let grace = std::env::var("ARUGULA_IDE_GRACE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(GRACE);
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            a = listener.accept() => if let Ok((tcp, _)) = a {
                let _ = tcp.set_nodelay(true);
                tokio::spawn(claude(st.clone(), tcp, token.clone()));
            },
            a = local.accept() => if let Ok((unix, _)) = a {
                tokio::spawn(daemon(st.clone(), unix, port));
            },
            _ = tick.tick() => {
                let s = st.lock().unwrap();
                let wait = if s.conns.is_empty() { IDLE } else { grace };
                if s.alone_since.is_some_and(|t| t.elapsed() > wait) {
                    info!(claudes = s.conns.len(), "no daemon: the IDE relay stops");
                    break;
                }
            }
            _ = term.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    let _ = std::fs::remove_file(&lock);
    let _ = std::fs::remove_file(&sock);
    Ok(())
}

/// The token Claude Code must bring: kept, so a new relay takes the same.
fn token(dir: &Path) -> io::Result<String> {
    let path = dir.join("token");
    if let Ok(t) = std::fs::read_to_string(&path)
        && t.trim().len() >= 32
    {
        return Ok(t.trim().to_owned());
    }
    let t: String = crate::push::random::<18>().iter().map(|b| format!("{b:02x}")).collect();
    crate::store::write_atomic(&path, t.as_bytes())?;
    Ok(t)
}

/// Loopback, on the port the last relay had (panes keep it in their
/// environment), else any.
async fn bind(dir: &Path) -> io::Result<TcpListener> {
    let path = dir.join("port");
    let last: Option<u16> = std::fs::read_to_string(&path).ok().and_then(|p| p.trim().parse().ok());
    let l = match last {
        Some(p) => match TcpListener::bind(("127.0.0.1", p)).await {
            Ok(l) => l,
            Err(e) => {
                warn!(port = p, error = %e, "the IDE port moved");
                TcpListener::bind(("127.0.0.1", 0)).await?
            }
        },
        None => TcpListener::bind(("127.0.0.1", 0)).await?,
    };
    crate::store::write_atomic(&path, l.local_addr()?.port().to_string().as_bytes())?;
    Ok(l)
}

/// What Claude Code reads to find us.
fn lockfile(dir: &Path, port: u16, token: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{port}.lock"));
    let body = json!({
        "pid": std::process::id(),
        "workspaceFolders": [],
        "ideName": super::NAME,
        "transport": "ws",
        "runningInWindows": false,
        "authToken": token,
    });
    crate::store::write_atomic(&path, body.to_string().as_bytes())?;
    Ok(path)
}

fn text(parts: &[&str]) -> Value {
    json!({ "content": parts.iter().map(|p| json!({ "type": "text", "text": p })).collect::<Vec<_>>() })
}

/// A Claude Code connection.
#[allow(clippy::result_large_err)] // tungstenite's handshake callback
async fn claude(st: Shared, tcp: tokio::net::TcpStream, token: String) {
    let check = |req: &Request, mut res: Response| -> Result<Response, ErrorResponse> {
        let refuse = |code: StatusCode, why: &str| {
            let mut r = ErrorResponse::new(Some(why.to_owned()));
            *r.status_mut() = code;
            Err(r)
        };
        // A browser always sends an Origin; Claude Code never does (S17).
        if req.headers().contains_key("origin") {
            return refuse(StatusCode::FORBIDDEN, "no browsers");
        }
        let got = req.headers().get("x-claude-code-ide-authorization").and_then(|v| v.to_str().ok());
        if got != Some(token.as_str()) {
            return refuse(StatusCode::UNAUTHORIZED, "bad token");
        }
        let mcp = req
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|p| p.trim() == "mcp"));
        if mcp {
            res.headers_mut().insert("sec-websocket-protocol", HeaderValue::from_static("mcp"));
        }
        Ok(res)
    };
    let ws = match tokio_tungstenite::accept_hdr_async(tcp, check).await {
        Ok(ws) => ws,
        Err(e) => return debug!(error = %e, "refused an IDE connection"),
    };
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let conn = {
        let mut s = st.lock().unwrap();
        let n = s.next_conn;
        s.next_conn += 1;
        s.conns.insert(n, Conn { pid: None, tx: tx.clone() });
        n
    };
    info!(conn, "Claude Code connected");
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(t) = msg else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
        handle(&st, conn, &tx, v);
    }
    writer.abort();
    let daemon = {
        let mut s = st.lock().unwrap();
        s.conns.remove(&conn);
        s.calls.retain(|(c, _), _| *c != conn);
        s.daemon.as_ref().map(|d| d.1.clone())
    };
    if let Some(d) = daemon {
        let _ = d.send(json!({ "t": "gone", "conn": conn }).to_string());
    }
    info!(conn, "Claude Code went");
}

fn reply(tx: &mpsc::UnboundedSender<Message>, id: &Value, result: Value) {
    let _ = tx.send(Message::Text(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string().into()));
}

/// One JSON-RPC message from Claude Code.
fn handle(st: &Shared, conn: u64, tx: &mpsc::UnboundedSender<Message>, v: Value) {
    let method = v["method"].as_str().unwrap_or("");
    let Some(id) = v.get("id").filter(|i| !i.is_null()).cloned() else {
        // A notification.
        if method == "ide_connected" {
            let pid = v["params"]["pid"].as_u64().map(|p| p as u32);
            let daemon = {
                let mut s = st.lock().unwrap();
                if let Some(c) = s.conns.get_mut(&conn) {
                    c.pid = pid;
                }
                s.daemon.as_ref().map(|d| d.1.clone())
            };
            if let Some(d) = daemon {
                let _ = d.send(json!({ "t": "conn", "conn": conn, "pid": pid }).to_string());
            }
        }
        return;
    };
    match method {
        "initialize" => reply(
            tx,
            &id,
            json!({
                "protocolVersion": v["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": super::NAME, "version": env!("CARGO_PKG_VERSION") },
            }),
        ),
        "tools/list" => {
            let tools: Vec<Value> = TOOLS
                .iter()
                .map(|(name, about, props)| {
                    let props: serde_json::Map<String, Value> =
                        props.iter().map(|p| ((*p).to_owned(), json!({ "type": "string" }))).collect();
                    json!({ "name": name, "description": about, "inputSchema": { "type": "object", "properties": props } })
                })
                .collect();
            reply(tx, &id, json!({ "tools": tools }));
        }
        "tools/call" => {
            let tool = v["params"]["name"].as_str().unwrap_or("").to_owned();
            let args = v["params"]["arguments"].clone();
            call(st, conn, tx, id, tool, args);
        }
        "ping" => reply(tx, &id, json!({})),
        "prompts/list" => reply(tx, &id, json!({ "prompts": [] })),
        "resources/list" => reply(tx, &id, json!({ "resources": [] })),
        _ => {
            let err =
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("no {method}") } });
            let _ = tx.send(Message::Text(err.to_string().into()));
        }
    }
}

fn call(st: &Shared, conn: u64, tx: &mpsc::UnboundedSender<Message>, id: Value, tool: String, args: Value) {
    match tool.as_str() {
        t if TO_DAEMON.contains(&t) => {
            let c = Call { conn, id: id.clone(), tool, args };
            let daemon = {
                let mut s = st.lock().unwrap();
                s.calls.insert((conn, id.to_string()), c.clone());
                s.daemon.as_ref().map(|d| d.1.clone())
            };
            match daemon {
                Some(d) => {
                    let _ = d.send(call_line(&c));
                }
                // No daemon: diagnostics can't wait; a diff can.
                None if c.tool == "getDiagnostics" => {
                    st.lock().unwrap().calls.remove(&(conn, id.to_string()));
                    reply(tx, &id, text(&["[]"]));
                }
                None => {}
            }
        }
        "close_tab" | "closeAllDiffTabs" => {
            // The terminal answered first (or a new turn began): the diffs
            // close, here and on every card.
            let tab = args["tab_name"].as_str().map(str::to_owned);
            let (closed, daemon) = {
                let mut s = st.lock().unwrap();
                let keys: Vec<(u64, String)> = s
                    .calls
                    .iter()
                    .filter(|((c, _), call)| {
                        *c == conn
                            && call.tool == "openDiff"
                            && tab.as_deref().is_none_or(|t| call.args["tab_name"].as_str() == Some(t))
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                let closed: Vec<Call> = keys.iter().filter_map(|k| s.calls.remove(k)).collect();
                (closed, s.daemon.as_ref().map(|d| d.1.clone()))
            };
            for c in &closed {
                reply(tx, &c.id, text(&["TAB_CLOSED"]));
            }
            if let Some(d) = daemon {
                let ids: Vec<&Value> = closed.iter().map(|c| &c.id).collect();
                let _ = d.send(json!({ "t": "closed", "conn": conn, "ids": ids, "tab": tab }).to_string());
            }
            reply(tx, &id, text(&["TAB_CLOSED"]));
        }
        "getCurrentSelection" | "getLatestSelection" => {
            reply(tx, &id, text(&[&json!({ "success": false, "message": "No active editor found" }).to_string()]))
        }
        "getOpenEditors" => reply(tx, &id, text(&[&json!({ "tabs": [] }).to_string()])),
        "getWorkspaceFolders" => {
            reply(tx, &id, text(&[&json!({ "success": true, "folders": [], "rootPath": null }).to_string()]))
        }
        "checkDocumentDirty" => reply(
            tx,
            &id,
            text(&[&json!({ "success": true, "filePath": args["filePath"], "isDirty": false, "isUntitled": false })
                .to_string()]),
        ),
        "saveDocument" => {
            reply(tx, &id, text(&[&json!({ "success": true, "saved": false, "message": "not open here" }).to_string()]))
        }
        _ => reply(tx, &id, text(&[&json!({ "success": true }).to_string()])),
    }
}

fn call_line(c: &Call) -> String {
    json!({ "t": "call", "conn": c.conn, "id": c.id, "tool": c.tool, "args": c.args }).to_string()
}

/// The daemon's link: a newer one replaces the last.
async fn daemon(st: Shared, unix: UnixStream, port: u16) {
    let (r, mut w) = unix.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let me = {
        let mut s = st.lock().unwrap();
        let me = s.next_daemon;
        s.next_daemon += 1;
        s.daemon = Some((me, tx.clone()));
        s.alone_since = None;
        // Everything open, so it can show it again.
        let _ = tx.send(json!({ "t": "hello", "port": port, "pid": std::process::id() }).to_string());
        for (n, c) in &s.conns {
            let _ = tx.send(json!({ "t": "conn", "conn": n, "pid": c.pid }).to_string());
        }
        for c in s.calls.values() {
            let _ = tx.send(call_line(c));
        }
        me
    };
    info!("the daemon connected");
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
        from_daemon(&st, v);
    }
    writer.abort();
    let mut s = st.lock().unwrap();
    if s.daemon.as_ref().is_some_and(|d| d.0 == me) {
        s.daemon = None;
        s.alone_since = Some(Instant::now());
        info!("the daemon went");
    }
}

fn from_daemon(st: &Shared, v: Value) {
    let conn = v["conn"].as_u64().unwrap_or(0);
    match v["t"].as_str().unwrap_or("") {
        "result" => {
            let mut s = st.lock().unwrap();
            if s.calls.remove(&(conn, v["id"].to_string())).is_some()
                && let Some(c) = s.conns.get(&conn)
            {
                reply(&c.tx, &v["id"], v["result"].clone());
            }
        }
        "notify" => {
            let note = json!({ "jsonrpc": "2.0", "method": v["method"], "params": v["params"] });
            let s = st.lock().unwrap();
            for (n, c) in &s.conns {
                if conn == 0 || *n == conn {
                    let _ = c.tx.send(Message::Text(note.to_string().into()));
                }
            }
        }
        other => debug!(kind = other, "unknown line from the daemon"),
    }
}
