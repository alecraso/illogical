//! The pipe to an agent server: newline-delimited JSON-RPC on its stdin and
//! stdout, wherever it runs.
//!
//! - **On this host** the server runs through the pane shim, in its own
//!   systemd scope, like a pane's shell. Its stdin is a pipe and its stdout
//!   a socket; the daemon's ends of both go in systemd's FD store
//!   (`agent-N-in`, `agent-N-out`), so a daemon restart never closes them:
//!   `claude-agent-acp` aborts its turn on stdin EOF (S7). The reader peeks
//!   at the socket and only takes whole lines, after they're logged, so a
//!   restart neither loses nor splits a frame (one longer than the socket's
//!   1 MiB buffer is the exception: see `read_lines`). Its stderr goes to
//!   `agent.err` in the block's directory.
//! - **In a VM** it runs as a non-TTY exec session on the block's sprite.
//!   The session outlives the WebSocket (`max_run_after_disconnect`), so a
//!   restarted daemon reattaches at the output offset it had processed, kept
//!   in `agent-exec.json`. Credentials go in on stdin before the server
//!   starts (see `GUEST_BOOT`), never in the URL, the argv or the VM's disk.

#[cfg(unix)]
use std::{
    fs::File,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::net::UnixStream,
    },
};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
#[cfg(unix)]
use std::{
    process::Stdio,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use arugula_proto::PaneId;
#[cfg(unix)]
use nix::sys::socket::{MsgFlags, recv, setsockopt, sockopt};
use tokio::sync::mpsc;
use tracing::info;
#[cfg(unix)]
use tracing::warn;

use crate::{
    pane::Launcher,
    provider::{PipeBegin, PipeEvent, Provider},
};

/// What comes from the agent server.
#[derive(Debug)]
pub enum FromAgent {
    /// One line of its stdout (a JSON-RPC frame), already logged.
    Line(Vec<u8>),
    /// It's gone (exited, or its machine went away).
    Closed(String),
}

/// Called for every whole line before it's taken off the pipe: it logs the
/// line and queues it for the block.
pub type Sink = Arc<dyn Fn(FromAgent) + Send + Sync>;

/// Our end of a running agent server.
pub struct Link {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    stop: Box<dyn Fn() + Send + Sync>,
}

impl Link {
    /// Send one frame (a newline is added).
    pub fn send(&self, mut line: Vec<u8>) {
        line.push(b'\n');
        let _ = self.tx.send(line);
    }

    /// End it: the agent server is stopped.
    pub fn stop(&self) {
        (self.stop)();
    }
}

fn fd_names(id: PaneId) -> (String, String) {
    (format!("agent-{id}-in"), format!("agent-{id}-out"))
}

pub fn record_path(dir: &Path) -> PathBuf {
    dir.join("process")
}

/// The process the shim started for this block, if it's still running.
pub fn alive_pid(dir: &Path) -> Option<u32> {
    let r = crate::shim::read_record(&record_path(dir));
    r.pid.filter(|_| crate::shim::alive(&r)).map(|(pid, _)| pid)
}

/// Kill a process group: TERM now, KILL in a few seconds if it's still there.
/// (Windows: the process now; its tree goes with the job in M58, #221.)
#[cfg(not(unix))]
pub fn kill_group(pid: u32, _dir: PathBuf) {
    crate::procinfo::kill(pid);
}

/// Kill a process group: TERM now, KILL in a few seconds if it's still there.
#[cfg(unix)]
pub fn kill_group(pid: u32, dir: PathBuf) {
    use nix::{sys::signal, unistd::Pid};
    let _ = signal::killpg(Pid::from_raw(pid as i32), signal::SIGTERM);
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if alive_pid(&dir) != Some(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = signal::killpg(Pid::from_raw(pid as i32), signal::SIGKILL);
    });
}

#[cfg_attr(not(unix), allow(dead_code))]
/// How a local agent server is started.
pub struct LocalSpawn<'a> {
    pub id: PaneId,
    pub dir: &'a Path,
    pub argv: &'a [String],
    pub cwd: &'a Path,
    pub env: &'a [(String, String)],
    pub remove: &'a [String],
    pub launch: &'a Launcher,
}

/// A pipe whose ends aren't inherited (`pipe2` doesn't exist on macOS).
#[cfg(unix)]
fn pipe_cloexec() -> nix::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use nix::fcntl::{FcntlArg, FdFlag, fcntl};
    let (r, w) = nix::unistd::pipe()?;
    for fd in [&r, &w] {
        fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    }
    Ok((r, w))
}

/// Start an agent server on this host. Windows: in M56 (#219).
#[cfg(not(unix))]
pub fn spawn_local(_s: LocalSpawn, _sink: Sink) -> std::io::Result<(Link, u32)> {
    Err(std::io::Error::other("agents don't run on Windows yet (M56, #219)"))
}

/// Start an agent server on this host.
#[cfg(unix)]
pub fn spawn_local(s: LocalSpawn, sink: Sink) -> std::io::Result<(Link, u32)> {
    let (in_r, in_w) = pipe_cloexec()?;
    let (ours, theirs) = UnixStream::pair()?;
    // Best effort: the kernel may cap it, and read_lines copes with less.
    let _ = setsockopt(&theirs, sockopt::SndBuf, &OUT_BUF);
    let _ = setsockopt(&ours, sockopt::RcvBuf, &OUT_BUF);
    let record = record_path(s.dir);
    let _ = std::fs::remove_file(&record);
    let err =
        crate::perm::open_mode(OpenOptions::new().create(true).append(true), 0o600).open(s.dir.join("agent.err"))?;
    let err_from = err.metadata().map_or(0, |m| m.len());
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    // Frozen (#504): scopes are read back from /proc/*/cgroup (`scope_of`).
    let mut cmd = s.launch.command(&format!("illogical-agent-{}-{nanos}", s.id));
    let cwd = if s.cwd.is_dir() { s.cwd } else { Path::new("/") };
    for k in s.remove {
        cmd.env_remove(k);
    }
    cmd.arg("_shim")
        .arg("--record")
        .arg(&record)
        .arg("--")
        .args(s.argv)
        .current_dir(cwd)
        .envs(s.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::from(in_r))
        .stdout(Stdio::from(OwnedFd::from(theirs)))
        .stderr(Stdio::from(err));
    let mut child = cmd.spawn()?;
    // The shim reports the program's pid once it has forked it.
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Some((pid, _)) = crate::shim::read_record(&record).pid {
            break pid;
        }
        let exited = child.try_wait().ok().flatten();
        // It may have recorded the pid just before it ended.
        if let Some((pid, _)) = crate::shim::read_record(&record).pid {
            break pid;
        }
        if Instant::now() > deadline || exited.is_some() {
            let _ = child.kill();
            let why = match exited {
                Some(status) => format!("ended at once ({status})"),
                None => "took too long to start".into(),
            };
            let said = said_since(&s.dir.join("agent.err"), err_from);
            let said = if said.is_empty() { String::new() } else { format!(": {said}") };
            return Err(std::io::Error::other(format!("{} {why}{said}", s.argv.join(" "))));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    info!(block = s.id, pid, argv = ?s.argv, cwd = %cwd.display(), scope = s.launch.scopes, "started agent server");
    let out = OwnedFd::from(ours);
    if s.launch.fd_store {
        let (a, b) = fd_names(s.id);
        crate::sys::remove_fd(&a);
        crate::sys::remove_fd(&b);
        if !crate::sys::store_fd(&a, in_w.as_raw_fd()) || !crate::sys::store_fd(&b, out.as_raw_fd()) {
            warn!(block = s.id, "couldn't keep the agent's pipes in the FD store");
        }
    }
    Ok((run_local(s.id, s.dir.to_owned(), in_w, out, pid, s.launch.fd_store, sink), pid))
}

/// Take over an agent server that outlived the previous daemon, by the pipe
/// ends systemd kept for us.
pub fn adopt_local(
    id: PaneId,
    dir: &Path,
    kept: &mut std::collections::HashMap<String, crate::pane::Kept>,
    fd_store: bool,
    sink: Sink,
) -> Option<(Link, u32)> {
    let (a, b) = fd_names(id);
    let (in_w, out) = (kept.remove(&a)?, kept.remove(&b)?);
    // Windows keeps no agent servers across a restart (yet).
    #[cfg(not(unix))]
    {
        let _ = (dir, in_w, out, fd_store, sink);
        None
    }
    #[cfg(unix)]
    {
        let pid = alive_pid(dir)?;
        info!(block = id, pid, "adopted agent server");
        Some((run_local(id, dir.to_owned(), in_w, out, pid, fd_store, sink), pid))
    }
}

/// Forget the pipes in the FD store (the block is closing).
pub fn forget_fds(id: PaneId) {
    let (a, b) = fd_names(id);
    crate::sys::remove_fd(&a);
    crate::sys::remove_fd(&b);
}

#[cfg(unix)]
fn run_local(id: PaneId, dir: PathBuf, in_w: OwnedFd, out: OwnedFd, pid: u32, fd_store: bool, sink: Sink) -> Link {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut writer = File::from(in_w);
    let _ = std::thread::Builder::new().name(format!("agent{id}-write")).spawn(move || {
        while let Some(line) = rx.blocking_recv() {
            if writer.write_all(&line).is_err() {
                break;
            }
        }
    });
    let reader_dir = dir.clone();
    let _ = std::thread::Builder::new().name(format!("agent{id}-read")).spawn(move || {
        let why = read_lines(&out, &sink);
        // The shim notes how it ended a moment after the pipe closes.
        let deadline = Instant::now() + Duration::from_secs(1);
        let ended = loop {
            let r = crate::shim::read_record(&record_path(&reader_dir));
            if r.exit.is_some() || Instant::now() > deadline {
                break r.exit;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let what = match ended {
            // Why, as it said on its way out.
            Some(crate::shim::Ended::Code(c)) if c != 0 => match last_words(&reader_dir) {
                Some(w) => format!("exited with code {c}: {w}"),
                None => format!("exited with code {c}"),
            },
            Some(crate::shim::Ended::Code(c)) => format!("exited with code {c}"),
            Some(crate::shim::Ended::Signal(s)) => format!("killed by signal {s}"),
            None => why,
        };
        if fd_store {
            forget_fds(id);
        }
        sink(FromAgent::Closed(what));
    });
    Link {
        tx,
        stop: Box::new(move || {
            if alive_pid(&dir) == Some(pid) {
                kill_group(pid, dir.clone());
            }
        }),
    }
}

#[cfg(unix)]
/// The agent's stdout socket's buffer: room for a whole line, so lines are
/// peeked whole (macOS's default is 8 KiB, Linux's about 200).
const OUT_BUF: usize = 1 << 20;

/// Whole lines from the socket, each handed to `sink` before it's taken
/// off. Returns why it stopped.
///
/// A line longer than the socket's buffer can't be peeked whole: the
/// buffer fills and the agent's write waits for us. So when a partial line
/// stops growing, it's taken off and kept here until its end comes (a
/// restart in the middle of such a line loses it).
#[cfg(unix)]
fn read_lines(sock: &OwnedFd, sink: &Sink) -> String {
    let mut buf = vec![0u8; 64 * 1024];
    let mut carry: Vec<u8> = vec![];
    let mut stalled = 0;
    loop {
        let n = match recv(sock.as_raw_fd(), &mut buf, MsgFlags::MSG_PEEK) {
            Ok(0) => return "closed its output".into(),
            Ok(n) => n,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return format!("read failed: {e}"),
        };
        let Some(last) = buf[..n].iter().rposition(|b| *b == b'\n') else {
            if n == buf.len() {
                // A line longer than the buffer: look further.
                buf.resize(buf.len() * 2, 0);
            } else if n == stalled {
                // Part of a line, and no more is coming until we take it.
                carry.extend_from_slice(&buf[..n]);
                if let Err(why) = take(sock, n) {
                    return why;
                }
                stalled = 0;
            } else {
                // Part of a line: wait for the rest.
                stalled = n;
                std::thread::sleep(Duration::from_millis(5));
            }
            continue;
        };
        stalled = 0;
        for (i, line) in buf[..last].split(|b| *b == b'\n').enumerate() {
            let line = if i == 0 && !carry.is_empty() {
                carry.extend_from_slice(line);
                std::mem::take(&mut carry)
            } else {
                line.to_vec()
            };
            if !line.iter().all(u8::is_ascii_whitespace) {
                sink(FromAgent::Line(line));
            }
        }
        // Now take them.
        if let Err(why) = take(sock, last + 1) {
            return why;
        }
    }
}

/// Take `n` bytes off the socket that a peek has already seen.
#[cfg(unix)]
fn take(sock: &OwnedFd, n: usize) -> Result<(), String> {
    let mut left = n;
    let mut scratch = vec![0u8; left];
    while left > 0 {
        match recv(sock.as_raw_fd(), &mut scratch[..left], MsgFlags::empty()) {
            Ok(0) => return Err("closed its output".into()),
            Ok(k) => left -= k,
            Err(nix::errno::Errno::EINTR) => {}
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
    Ok(())
}

/// Runs first in the VM, under `bash -c`: installs Node and the adapter
/// (once per machine, into `~/.arugula/agents`, or the `~/.illogical/agents`
/// an older daemon made; its noise goes to
/// stderr), reads `KEY=value` lines from stdin up to a blank one into the
/// environment (credentials: never on disk, never in argv), then becomes
/// the agent server.
///
/// `$1` is the npm package (or empty), `$2` the directory, then the command.
pub const GUEST_BOOT: &str = r#"
set -e
pkg=$1; dir=$2; shift 2
A=$HOME/.arugula/agents
# What an illogical daemon installed, where it is (#505).
if ! [ -d "$A" ] && [ -d "$HOME/.illogical/agents" ]; then A=$HOME/.illogical/agents; fi
exec 3>&1 1>&2
if [ -n "$pkg" ]; then
  if ! [ -x "$A/node/bin/node" ]; then
    arch=$(uname -m); case $arch in x86_64) arch=x64;; aarch64) arch=arm64;; esac
    v=v22.23.2
    echo "arugula: installing Node $v in this machine"
    mkdir -p "$A/node"
    curl -fsSL "https://nodejs.org/dist/$v/node-$v-linux-$arch.tar.gz" | tar -xz -C "$A/node" --strip-components=1
  fi
  export PATH="$A/node/bin:$A/npm/node_modules/.bin:$PATH"
  name=${pkg%@*}
  if ! [ -d "$A/npm/node_modules/$name" ] || ! grep -q "\"version\": \"${pkg##*@}\"" "$A/npm/node_modules/$name/package.json"; then
    echo "arugula: installing $pkg in this machine"
    npm install --no-fund --no-audit --prefix "$A/npm" "$pkg"
  fi
fi
while IFS= read -r line && [ -n "$line" ]; do export "$line"; done
exec 1>&3 3>&-
mkdir -p "$dir" 2>/dev/null || true
cd "$dir"
exec "$@"
"#;

/// Where a VM agent's exec session is, for a restarted daemon.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ExecRecord {
    pub session: String,
    /// Output bytes of it processed (whole lines only).
    pub received: u64,
}

impl ExecRecord {
    fn path(dir: &Path) -> PathBuf {
        dir.join("agent-exec.json")
    }
    pub fn read(dir: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(Self::path(dir)).ok()?).ok()
    }
    fn write(&self, dir: &Path) {
        if let Ok(b) = serde_json::to_vec(self) {
            let _ = crate::store::write_atomic(&Self::path(dir), &b);
        }
    }
    pub fn clear(dir: &Path) {
        let _ = std::fs::remove_file(Self::path(dir));
    }
}

/// How a VM agent begins.
pub enum VmBegin {
    /// A new session: the npm package to install first (if any), the
    /// directory, the command, and the environment sent on stdin.
    New { npm: Option<String>, cwd: String, argv: Vec<String>, secret_env: Vec<(String, String)> },
    /// Reattach to the session the last daemon followed.
    Resume(ExecRecord),
}

/// Start (or reattach to) an agent server in a VM.
pub fn spawn_vm(
    rt: &tokio::runtime::Handle,
    provider: Arc<dyn Provider>,
    sprite: String,
    dir: PathBuf,
    begin: VmBegin,
    sink: Sink,
) -> Link {
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (stop_tx, stop_rx) = mpsc::unbounded_channel::<()>();
    rt.spawn(drive_vm(provider, sprite, dir, begin, rx, stop_rx, sink));
    Link {
        tx,
        stop: Box::new(move || {
            let _ = stop_tx.send(());
        }),
    }
}

async fn drive_vm(
    provider: Arc<dyn Provider>,
    sprite: String,
    dir: PathBuf,
    begin: VmBegin,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut stop: mpsc::UnboundedReceiver<()>,
    sink: Sink,
) {
    let closed = |why: String| sink(FromAgent::Closed(why));
    let (mut rec, mut fresh) = match begin {
        VmBegin::New { npm, cwd, argv, secret_env } => {
            if let Err(e) = crate::provider::ensure(&*provider, &sprite, None).await {
                closed(format!("couldn't start its machine: {e}"));
                return;
            }
            let mut cmd = vec!["bash".to_owned(), "-c".into(), GUEST_BOOT.into(), "arugula-agent".into()];
            cmd.push(npm.unwrap_or_default());
            cmd.push(cwd);
            cmd.extend(argv);
            (Err(cmd), Some(secret_env))
        }
        VmBegin::Resume(r) => (Ok(r), None),
    };
    let mut err =
        crate::perm::open_mode(OpenOptions::new().create(true).append(true), 0o600).open(dir.join("agent.err")).ok();
    let mut failures = 0u32;
    loop {
        let begin = match &rec {
            Ok(r) => PipeBegin::Resume { session: r.session.clone(), received: r.received },
            Err(cmd) => PipeBegin::New { argv: cmd.clone() },
        };
        let mut pipe = match provider.pipe(&sprite, begin).await {
            Ok(p) => p,
            Err(e) => {
                failures += 1;
                match crate::provider::exists(&*provider, &sprite).await {
                    Ok(false) => return closed("its machine is gone".into()),
                    _ if failures > 5 => return closed(format!("can't reach its machine: {e}")),
                    _ => {}
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        failures = 0;
        // Output past the last whole line we handled.
        let mut partial: Vec<u8> = Vec::new();
        let mut received = rec.as_ref().map(|r| r.received).unwrap_or(0);
        let mut ended: Option<String> = None;
        loop {
            tokio::select! {
                ev = pipe.events.recv() => match ev {
                    Some(PipeEvent::Stdout(data)) => {
                        received += data.len() as u64;
                        partial.extend_from_slice(&data);
                        if let Some(last) = partial.iter().rposition(|c| *c == b'\n') {
                            for line in partial[..last].split(|c| *c == b'\n') {
                                if !line.iter().all(u8::is_ascii_whitespace) {
                                    sink(FromAgent::Line(line.to_vec()));
                                }
                            }
                            partial.drain(..=last);
                        }
                        // Only whole lines count as handled.
                        if partial.is_empty()
                            && let Ok(r) = rec.as_mut()
                        {
                            r.received = received;
                            r.write(&dir);
                        }
                    }
                    Some(PipeEvent::Stderr(data)) => {
                        received += data.len() as u64;
                        if let Some(f) = err.as_mut() {
                            let _ = f.write_all(&data);
                        }
                        if partial.is_empty()
                            && let Ok(r) = rec.as_mut()
                        {
                            r.received = received;
                            r.write(&dir);
                        }
                    }
                    Some(PipeEvent::Exited(code)) => {
                        ended = Some(format!("exited with code {}", code.unwrap_or(-1)));
                    }
                    Some(PipeEvent::Session(session)) if rec.is_err() => {
                        let r = ExecRecord { session, received };
                        r.write(&dir);
                        rec = Ok(r);
                        // Credentials and settings, then a blank line.
                        if let Some(env) = fresh.take() {
                            let mut pre = Vec::new();
                            for (k, v) in env {
                                pre.extend_from_slice(format!("{k}={v}\n").as_bytes());
                            }
                            pre.push(b'\n');
                            let _ = pipe.stdin.send(pre);
                        }
                    }
                    Some(PipeEvent::Session(_)) => {}
                    None => break,
                },
                // Not before the environment's preamble: the guest's boot
                // script reads stdin up to a blank line first.
                line = rx.recv(), if fresh.is_none() => match line {
                    Some(line) => {
                        let _ = pipe.stdin.send(line);
                    }
                    None => {
                        // The block let go (daemon shutting down): detach,
                        // leaving the agent running for the next daemon.
                        return;
                    }
                },
                _ = stop.recv() => {
                    if let Ok(r) = &rec {
                        let _ = provider.kill(&sprite, &r.session, "TERM").await;
                    }
                    ExecRecord::clear(&dir);
                    return;
                }
            }
        }
        if let Some(why) = ended {
            ExecRecord::clear(&dir);
            return closed(why);
        }
        match (&rec, crate::provider::exists(&*provider, &sprite).await) {
            (_, Ok(false)) => return closed("its machine is gone".into()),
            (Err(_), _) => return closed("the agent server didn't start".into()),
            _ => {
                info!(sprite, "agent exec dropped; reattaching");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

#[cfg(unix)]
/// The last thing an agent server wrote to stderr (`agent.err`), for the
/// note when it exits with an error.
/// What was written to `path` after its first `from` bytes: the last few
/// lines, short.
fn said_since(path: &Path, from: u64) -> String {
    let Ok(bytes) = std::fs::read(path) else { return String::new() };
    let text = String::from_utf8_lossy(bytes.get(from as usize..).unwrap_or_default());
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" / ").chars().take(400).collect()
}

#[cfg(unix)]
fn last_words(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("agent.err")).ok()?;
    let tail = &text[text.len().saturating_sub(4096)..];
    let line = tail.lines().map(str::trim).filter(|l| !l.is_empty()).find(|l| {
        let l = l.to_ascii_lowercase();
        l.contains("error") || l.contains("not found") || l.contains("cannot")
    });
    let line = line.or_else(|| tail.lines().map(str::trim).rfind(|l| !l.is_empty()))?;
    Some(line.chars().take(240).collect())
}

#[cfg(all(test, unix))]
mod tests {
    use std::{io::Write, os::unix::net::UnixStream, sync::Mutex};

    use super::*;

    #[test]
    fn a_line_longer_than_the_socket_buffer_comes_through_whole() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let _ = setsockopt(&theirs, sockopt::SndBuf, &4096);
        let _ = setsockopt(&ours, sockopt::RcvBuf, &4096);
        let big = "x".repeat(300_000);
        let lines = Arc::new(Mutex::new(vec![]));
        let got = lines.clone();
        let sink: Sink = Arc::new(move |f| {
            if let FromAgent::Line(l) = f {
                got.lock().unwrap().push(String::from_utf8(l).unwrap());
            }
        });
        let text = format!("first\n{big}\nlast\n");
        let writer = std::thread::spawn(move || theirs.write_all(text.as_bytes()).unwrap());
        let reader = std::thread::spawn(move || read_lines(&OwnedFd::from(ours), &sink));
        writer.join().unwrap();
        assert_eq!(reader.join().unwrap(), "closed its output");
        assert_eq!(*lines.lock().unwrap(), ["first".to_owned(), big, "last".to_owned()]);
    }
}
