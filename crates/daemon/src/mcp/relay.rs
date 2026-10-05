//! MCP for agent blocks in a VM (#59). A wisp guest can't reach any host
//! address (S14), so the daemon reaches in instead: it opens a non-TTY exec
//! in the VM running `guest_relay.py`, which listens on a Unix socket there
//! and carries each connection over the exec's stdin and stdout. Each
//! connection is served here as its own MCP session (stdio framing) under
//! the block's scope, the same as its token would get over HTTP. The
//! agent's `mcpServers` gets `guest_client.py` on stdio, which connects to
//! that socket, and reconnects (replaying `initialize`) when the relay is
//! replaced.
//!
//! The relay lives as long as the block: an agent that restarts just
//! connects again. A restarted daemon opens a new relay, which replaces the
//! old one (by its pid file) in the guest.

use std::{collections::HashMap, sync::Arc, time::Duration};

use illogical_proto::PaneId;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream},
    sync::mpsc,
};
use tracing::{info, warn};

use crate::provider::{Pipe, PipeBegin, PipeEvent, Provider};

pub const RELAY: &str = include_str!("guest_relay.py");
pub const CLIENT: &str = include_str!("guest_client.py");

/// Serves one connection as an MCP session for an agent block.
pub type Serve = Arc<dyn Fn(PaneId, DuplexStream) + Send + Sync>;

/// The relay's socket in the guest, per block (a VM tab can hold several).
pub fn socket(id: PaneId) -> String {
    format!("/tmp/illogical-mcp-{id}.sock")
}

/// The `mcpServers` entry a VM agent gets: the client, on stdio.
pub fn server_entry(id: PaneId) -> Value {
    json!({
        "name": super::SERVER_NAME,
        "command": "python3",
        "args": ["-c", CLIENT, "illogical-mcp", socket(id)],
        "env": [],
    })
}

/// A running relay; stopping it ends it in the guest too.
pub struct Relay {
    stop: mpsc::UnboundedSender<()>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

impl Relay {
    pub fn stop(&self) {
        let _ = self.stop.send(());
    }

    /// It gave up (its machine is gone, or it can't start there).
    pub fn finished(&self) -> bool {
        self.done.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Start the relay for block `id` on `sprite`.
pub fn start(
    rt: &tokio::runtime::Handle,
    provider: Arc<dyn Provider>,
    sprite: String,
    id: PaneId,
    serve: Arc<std::sync::OnceLock<Serve>>,
) -> Relay {
    let (stop, stop_rx) = mpsc::unbounded_channel();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d = done.clone();
    rt.spawn(async move {
        drive(provider, sprite, id, serve, stop_rx).await;
        d.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    Relay { stop, done }
}

async fn drive(
    provider: Arc<dyn Provider>,
    sprite: String,
    id: PaneId,
    serve: Arc<std::sync::OnceLock<Serve>>,
    mut stop: mpsc::UnboundedReceiver<()>,
) {
    let argv: Vec<String> =
        ["python3", "-c", RELAY, "illogical-mcp-relay", &socket(id)].iter().map(|s| s.to_string()).collect();
    // The agent's own start creates its machine; the relay starts beside
    // it, so wait for the machine rather than taking "no such machine" as
    // its end.
    let deadline = std::time::Instant::now() + Duration::from_secs(600);
    loop {
        match crate::provider::exists(&*provider, &sprite).await {
            Ok(true) => break,
            _ if std::time::Instant::now() > deadline => {
                warn!(block = id, sprite, "its machine never came up; no MCP relay");
                return;
            }
            _ => {}
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            _ = stop.recv() => return,
        }
    }
    let mut failures = 0u32;
    loop {
        let pipe = tokio::select! {
            p = provider.pipe(&sprite, PipeBegin::New { argv: argv.clone() }) => p,
            _ = stop.recv() => return,
        };
        match pipe {
            Ok(pipe) => {
                let serve = serve.clone();
                let serve = move |io| serve.get().map(|s| s(id, io)).is_some();
                let began = std::time::Instant::now();
                let mut session = None;
                let why = tokio::select! {
                    w = carry(pipe, serve, &mut session) => Some(w),
                    _ = stop.recv() => None,
                };
                // Stopped, or it ended: either way it's gone in the guest.
                if let Some(s) = &session {
                    let _ = provider.kill(&sprite, s, "TERM").await;
                }
                let Some(why) = why else { return };
                info!(block = id, sprite, why, "MCP relay ended");
                if began.elapsed() > Duration::from_secs(60) {
                    failures = 0;
                }
            }
            Err(e) => warn!(block = id, sprite, error = %e, "couldn't start the MCP relay"),
        }
        failures += 1;
        match crate::provider::exists(&*provider, &sprite).await {
            Ok(false) => return,
            _ if failures > 5 => {
                warn!(block = id, sprite, "giving up on the MCP relay");
                return;
            }
            _ => {}
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(500 * failures as u64)) => {}
            _ = stop.recv() => return,
        }
    }
}

/// Carry one relay exec until it ends: each connection it reports is
/// served by `serve` over a duplex stream. Notes its exec session in
/// `session`, and returns why it ended.
pub async fn carry(mut pipe: Pipe, serve: impl Fn(DuplexStream) -> bool, session: &mut Option<String>) -> String {
    let mut conns: HashMap<u64, mpsc::UnboundedSender<Vec<u8>>> = HashMap::new();
    let (back_tx, mut back) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut partial: Vec<u8> = Vec::new();
    let mut errs: Vec<u8> = Vec::new();
    loop {
        tokio::select! {
            ev = pipe.events.recv() => match ev {
                Some(PipeEvent::Session(s)) => *session = Some(s),
                Some(PipeEvent::Stdout(data)) => {
                    partial.extend_from_slice(&data);
                    while let Some(nl) = partial.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = partial.drain(..=nl).collect();
                        on_line(&line[..line.len() - 1], &mut conns, &back_tx, &serve);
                    }
                }
                Some(PipeEvent::Stderr(data)) => {
                    errs.extend_from_slice(&data);
                    while let Some(nl) = errs.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = errs.drain(..=nl).collect();
                        info!(said = %String::from_utf8_lossy(&line).trim_end(), "MCP relay");
                    }
                }
                Some(PipeEvent::Exited(code)) => {
                    return format!("exited with code {}", code.unwrap_or(-1));
                }
                None => return "its exec dropped".into(),
            },
            Some(line) = back.recv() => {
                let _ = pipe.stdin.send(line);
            }
        }
    }
}

/// One line from the relay: `N+`, `N:LINE` or `N-`.
fn on_line(
    line: &[u8],
    conns: &mut HashMap<u64, mpsc::UnboundedSender<Vec<u8>>>,
    back: &mpsc::UnboundedSender<Vec<u8>>,
    serve: &impl Fn(DuplexStream) -> bool,
) {
    let digits = line.iter().take_while(|b| b.is_ascii_digit()).count();
    let Some(n) = std::str::from_utf8(&line[..digits]).ok().and_then(|s| s.parse::<u64>().ok()) else { return };
    match line.get(digits) {
        Some(b'+') => {
            let (ours, theirs) = tokio::io::duplex(256 * 1024);
            if !serve(theirs) {
                // Not serving yet (the daemon is starting): it connects again.
                let _ = back.send(format!("{n}-\n").into_bytes());
                return;
            }
            let (r, mut w) = tokio::io::split(ours);
            let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
            conns.insert(n, tx);
            tokio::spawn(async move {
                while let Some(mut line) = rx.recv().await {
                    line.push(b'\n');
                    if w.write_all(&line).await.is_err() {
                        break;
                    }
                }
                let _ = w.shutdown().await;
            });
            let back = back.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(r).split(b'\n');
                while let Ok(Some(line)) = lines.next_segment().await {
                    if line.iter().all(u8::is_ascii_whitespace) {
                        continue;
                    }
                    let mut f = format!("{n}:").into_bytes();
                    f.extend_from_slice(&line);
                    f.push(b'\n');
                    if back.send(f).is_err() {
                        return;
                    }
                }
                let _ = back.send(format!("{n}-\n").into_bytes());
            });
        }
        Some(b':') => {
            if let Some(tx) = conns.get(&n) {
                let _ = tx.send(line[digits + 1..].to_vec());
            }
        }
        Some(b'-') => {
            conns.remove(&n);
        }
        _ => {}
    }
}

// Unix: the relay runs in Linux guests, on a Unix socket.
#[cfg(all(test, unix))]
mod tests {
    use std::{
        io::{BufRead, Write},
        process::{Child, Command, Stdio},
    };

    use super::*;

    #[cfg(unix)]
    /// A stand-in MCP session: answers every request with its method and
    /// which connection (session) it is.
    fn toy(count: Arc<std::sync::atomic::AtomicU64>) -> impl Fn(DuplexStream) -> bool {
        move |io| {
            let k = count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            tokio::spawn(async move {
                let (r, mut w) = tokio::io::split(io);
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let m: Value = serde_json::from_str(&l).unwrap();
                    if m.get("id").is_none() {
                        continue;
                    }
                    if m["method"] == "slow" {
                        // Never answered: the connection drops first.
                        continue;
                    }
                    let a =
                        json!({ "jsonrpc": "2.0", "id": m["id"], "result": { "method": m["method"], "session": k } });
                    w.write_all(format!("{a}\n").as_bytes()).await.unwrap();
                }
            });
            true
        }
    }

    #[cfg(unix)]
    /// The relay, run on this host as the exec would run it in a guest.
    fn local_relay(sock: &str) -> (Child, Pipe) {
        let mut child = Command::new("python3")
            .args(["-c", RELAY, "illogical-mcp-relay", sock])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (ev_tx, events) = mpsc::unbounded_channel();
        let (stdin, mut in_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let mut input = child.stdin.take().unwrap();
        std::thread::spawn(move || {
            while let Some(d) = in_rx.blocking_recv() {
                if input.write_all(&d).and_then(|_| input.flush()).is_err() {
                    break;
                }
            }
        });
        for (mut from, err) in [
            (Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>, false),
            (Box::new(child.stderr.take().unwrap()), true),
        ] {
            let tx = ev_tx.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = from.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let d = buf[..n].to_vec();
                    let _ = tx.send(if err { PipeEvent::Stderr(d) } else { PipeEvent::Stdout(d) });
                }
                if !err {
                    let _ = tx.send(PipeEvent::Exited(None));
                }
            });
        }
        (child, Pipe { events, stdin })
    }

    #[cfg(unix)]
    struct Agent {
        child: Child,
        out: std::io::BufReader<std::process::ChildStdout>,
    }

    #[cfg(unix)]
    impl Agent {
        fn send(&mut self, m: Value) {
            writeln!(self.child.stdin.as_mut().unwrap(), "{m}").unwrap();
        }
        fn recv(&mut self) -> Value {
            let mut l = String::new();
            self.out.read_line(&mut l).unwrap();
            serde_json::from_str(&l).unwrap_or_else(|e| panic!("{e}: {l:?}"))
        }
    }

    // Unix: the relay runs in Linux guests, on a Unix socket.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn sessions_per_connection_and_reconnects() {
        // Windows' python3 can be the Store's stand-in, which only says to install it.
        if !Command::new("python3").arg("-V").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no python3; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("illogical-relay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("mcp.sock").display().to_string();
        let count = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let (mut relay, pipe) = local_relay(&sock);
        let c = count.clone();
        let first = tokio::spawn(async move { carry(pipe, toy(c), &mut None).await });

        let client = |sock: &str| {
            let mut child = Command::new("python3")
                .args(["-c", CLIENT, "illogical-mcp", sock])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let out = std::io::BufReader::new(child.stdout.take().unwrap());
            Agent { child, out }
        };
        let init = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {} });
        let mut a = client(&sock);
        a.send(init.clone());
        assert_eq!(a.recv()["result"]["session"], 1);
        a.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        a.send(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }));
        let r = a.recv();
        assert_eq!((r["id"].as_u64(), r["result"]["session"].as_u64()), (Some(1), Some(1)), "{r}");

        // A second client (an agent restarting its MCP client) is a session
        // of its own.
        let mut b = client(&sock);
        b.send(init.clone());
        assert_eq!(b.recv()["result"]["session"], 2);
        drop(b);

        // The relay is replaced (a restarted daemon): the call in flight
        // gets an error, and the client is back with its initialize
        // replayed (and its answer kept from the agent).
        a.send(json!({ "jsonrpc": "2.0", "id": 2, "method": "slow" }));
        std::thread::sleep(Duration::from_millis(300));
        // (It finds its predecessor in /proc, which guests have.)
        if !cfg!(target_os = "linux") {
            let _ = relay.kill();
        }
        let (mut relay2, pipe) = local_relay(&sock);
        let c = count.clone();
        tokio::spawn(async move { carry(pipe, toy(c), &mut None).await });
        let e = a.recv();
        assert_eq!(e["id"], 2, "{e}");
        assert!(e["error"]["message"].as_str().unwrap().contains("call again"), "{e}");
        first.await.unwrap();
        let _ = relay.wait();
        a.send(json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call" }));
        let r = a.recv();
        assert_eq!((r["id"].as_u64(), r["result"]["method"].as_str()), (Some(3), Some("tools/call")), "{r}");
        assert_eq!(r["result"]["session"], 3, "a new session, opened by the replayed initialize");

        let _ = a.child.kill();
        let _ = relay2.kill();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
