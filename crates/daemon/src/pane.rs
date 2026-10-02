//! A pane: a process on a PTY (here, or an exec TTY on a machine), the
//! server-side terminal state it draws, its history on disk, and the clients
//! watching it.
//!
//! Each pane runs a VT thread that owns everything stateful (libghostty's
//! terminal is `!Send`). PTY output, client attaches, resizes and process
//! exits all arrive on one channel, so a client's snapshot or replay and the
//! live output after it are always in order, and the log on disk sees the
//! same bytes in the same order.

use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io::{Read, Write},
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use illogical_proto::{ClientId, Frame, FrameKind, PaneId, ServerMsg};
use illogical_vt::{GhosttyEngine, VtEngine};
use nix::{
    fcntl::{FcntlArg, FdFlag, fcntl},
    libc,
    pty::{Winsize, openpty},
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{
    machine::{Begin, Exec, ExecEvent},
    osc::Signal,
    provider::Provider,
    store::{Event, PaneLog, now_ms},
};

/// Output kept in memory for clients that reconnect: anything within this
/// many bytes of the end is replayed instead of snapshotted.
const RING_BYTES: usize = 2 * 1024 * 1024;
const MAX_REPLAY_BYTES: u64 = 1024 * 1024;
/// Frames queued per client before it counts as too slow and is resynced.
pub const CLIENT_QUEUE: usize = 1024;
/// Checkpoint after this much output, or after this long idle with output
/// since the last one.
const CHECKPOINT_BYTES: u64 = 2 * 1024 * 1024;
const CHECKPOINT_IDLE: Duration = Duration::from_secs(5);
/// Most log a restore replays after a checkpoint, or at all without one.
const RESTORE_REPLAY_BYTES: u64 = 8 * 1024 * 1024;

/// What a client connection receives.
#[derive(Debug)]
pub enum ToClient {
    Frame(Vec<u8>),
    Msg(ServerMsg),
}

/// A client's subscription. Everything the client must apply in order with
/// a pane's output (sizes, snapshots, output) goes through the bounded
/// `data` queue; `ctrl` is unbounded and carries layout state and the
/// resync notice for a full `data` queue.
#[derive(Clone)]
pub struct Subscriber {
    pub client: ClientId,
    pub data: mpsc::Sender<ToClient>,
    pub ctrl: mpsc::UnboundedSender<ToClient>,
}

/// What a pane tells the multiplexer.
#[derive(Debug, Clone)]
pub struct Notice {
    pub pane: PaneId,
    pub what: What,
}

#[derive(Debug, Clone)]
pub enum What {
    /// The process ended. `close` is false when the pane stays: the process
    /// was killed by a signal (it didn't mean to go away: a reboot, an OOM
    /// kill), or the pane holds on exit (`illogical run`).
    Exited { code: Option<i32>, close: bool },
    /// The pane's machine is up (true), or gone (false): deleted from under
    /// it, or lost in a reboot of the host it ran on.
    Machine(bool),
    /// A process started in a pane that was waiting.
    Started,
    /// Structure in the output: prompts, commands, cwd, notifications.
    Signal(Signal),
    /// Output started flowing (true) or has been quiet for a while (false).
    Busy(bool),
    /// A non-terminal block's state changed.
    BlockChanged,
    /// A block asks for attention (or lets go of it), and why.
    Attention(illogical_proto::Attention, String),
    /// A block's own event, for the event stream.
    Event(illogical_proto::EventKind),
}

pub type NoticeSink = mpsc::UnboundedSender<Notice>;

/// A command as the shell integration reported it, with where its output
/// sits in the pane's stream.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct CommandRec {
    pub text: Option<String>,
    pub cwd: Option<String>,
    /// Stream offset where its output starts.
    pub start: u64,
    /// Stream offset where it finished, once it has.
    pub end: Option<u64>,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    pub exit: Option<i32>,
}

/// What a pane's thread knows that others want to read without asking it.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// From OSC 7, which (unlike /proc) works over ssh too.
    pub cwd: Option<String>,
    /// Running now.
    pub current: Option<CommandRec>,
    /// The last one that finished.
    pub last: Option<CommandRec>,
    pub busy: bool,
    /// Stream offset just past the last byte of output.
    pub end: u64,
    /// Stream offset when input was last written: `wait` looks for what
    /// happened after it.
    pub input_at: u64,
    /// How the last process ended (`Some(code)`), until another starts.
    pub exited: Option<Option<i32>>,
    pub modes: crate::keys::Modes,
    /// The shell drew its prompt and nothing has started since (shell
    /// integration says so): typing a line there runs it in the shell.
    pub at_prompt: bool,
}

/// Quiet this long and a busy pane counts as quiet.
const QUIET: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFormat {
    Text,
    Ansi,
    Html,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureScope {
    Screen,
    Scrollback,
    LastCommand,
}

enum Cmd {
    Output(Vec<u8>),
    /// The process `key` (a local pid, or a machine exec) ended.
    Exited {
        key: u64,
        code: Option<i32>,
        signal: Option<i32>,
    },
    Exec {
        key: u64,
        event: ExecEvent,
    },
    /// Let go of the machine session quietly: it's about to be deleted.
    Release,
    /// Drop what's running (its machine was replaced) and start again.
    Restart {
        start: Start,
        note: String,
    },
    Attach {
        sub: Subscriber,
        offset: Option<u64>,
    },
    Detach {
        client: ClientId,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Input(Vec<u8>),
    Purge,
    Checkpoint(Sender<()>),
    Capture {
        format: CaptureFormat,
        scope: CaptureScope,
        reply: Sender<String>,
    },
    Close,
}

#[derive(Clone)]
pub struct PaneHandle {
    pub id: PaneId,
    pub epoch: u64,
    pid: Arc<AtomicU32>,
    running: Arc<AtomicBool>,
    status: Arc<std::sync::Mutex<Status>>,
    tx: Sender<Cmd>,
}

impl PaneHandle {
    pub fn attach(&self, sub: Subscriber, offset: Option<u64>) {
        let _ = self.tx.send(Cmd::Attach { sub, offset });
    }
    pub fn detach(&self, client: ClientId) {
        let _ = self.tx.send(Cmd::Detach { client });
    }
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.tx.send(Cmd::Resize { cols, rows });
    }
    pub fn input(&self, data: Vec<u8>) {
        let _ = self.tx.send(Cmd::Input(data));
    }
    /// Let go of the session on the pane's machine without a word, before
    /// the machine is deleted (and `restart` follows).
    pub fn release(&self) {
        let _ = self.tx.send(Cmd::Release);
    }
    /// Start over as `start` says, after a `── note ──` rule: the pane's
    /// machine was reset, so what ran on the old one is gone.
    pub fn restart(&self, start: Start, note: &str) {
        let _ = self.tx.send(Cmd::Restart { start, note: note.into() });
    }
    pub fn purge(&self) {
        let _ = self.tx.send(Cmd::Purge);
    }
    /// Hang up the process and delete the pane's history; the pane's thread
    /// ends when that's done.
    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
    }
    /// Write a checkpoint now and wait for it (on shutdown).
    pub fn checkpoint(&self, timeout: Duration) {
        let (tx, rx) = bounded(1);
        if self.tx.send(Cmd::Checkpoint(tx)).is_ok() {
            let _ = rx.recv_timeout(timeout);
        }
    }
    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }
    /// The pane's screen, scrollback or last command as text, ANSI or HTML.
    pub fn capture(&self, format: CaptureFormat, scope: CaptureScope) -> Option<String> {
        let (tx, rx) = bounded(1);
        self.tx.send(Cmd::Capture { format, scope, reply: tx }).ok()?;
        rx.recv_timeout(Duration::from_secs(5)).ok()
    }
    /// Note that input is about to be sent, before it's queued: a `wait`
    /// that follows a `send` then sees only what happens after it.
    pub fn mark_input(&self) {
        if let Ok(mut st) = self.status.lock() {
            st.input_at = st.end;
        }
    }
    pub fn pid_now(&self) -> Option<u32> {
        self.pid()
    }
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
    fn pid(&self) -> Option<u32> {
        Some(self.pid.load(Ordering::Relaxed)).filter(|p| *p != 0)
    }
    /// The process's working directory, from the OS.
    pub fn cwd(&self) -> Option<PathBuf> {
        crate::procinfo::cwd(self.pid()?)
    }
    /// The command in the foreground, if it isn't the shell itself: what
    /// "re-run" would run again. It is the foreground process's command line
    /// as the OS shows it now, so `bash -c 'a; b'` that exec'd into `b` reads
    /// as `b`; the typed command line needs shell integration (M3).
    pub fn command(&self) -> Option<String> {
        let shell = self.pid()?;
        // The shell is a session leader; its foreground job is the
        // terminal's foreground process group.
        let tpgid = crate::procinfo::foreground(shell)?;
        if tpgid == shell {
            return None;
        }
        let args: Vec<String> = crate::procinfo::argv(tpgid)?.iter().map(|a| shell_quote(a)).collect();
        (!args.is_empty()).then(|| args.join(" "))
    }
}

fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b)) {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[derive(Clone, Debug)]
pub struct Spawn {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

/// How a pane begins.
pub enum Start {
    Now(Spawn),
    /// Take over a terminal (and the program on it) that outlived the
    /// previous daemon.
    Adopt(OwnedFd),
    /// Reattach to a session on the pane's machine that outlived the
    /// previous daemon, having logged `received` bytes of it; if it's gone,
    /// start as `otherwise` says.
    Resume {
        session: String,
        received: u64,
        otherwise: Box<Start>,
    },
    /// Run a command (`illogical run`), recorded as a command with this
    /// text, so it has history, events and an exit code like any other.
    Run {
        spawn: Spawn,
        text: String,
    },
    /// Show `banner` and wait: Enter runs `enter`, Escape runs `escape`.
    Wait {
        banner: String,
        enter: Spawn,
        escape: Option<Spawn>,
    },
}

pub struct Setup {
    pub id: PaneId,
    pub cols: u16,
    pub rows: u16,
    pub log: PaneLog,
    /// Rebuild the terminal from the log (and checkpoint) before starting.
    pub restore: bool,
    pub start: Start,
    /// What a pane whose process was killed offers to run instead.
    pub shell: Spawn,
    pub launch: Launcher,
    /// Keep the pane when its program exits normally (`illogical run`), so
    /// its output and exit code can still be read.
    pub hold: bool,
    pub notices: NoticeSink,
    /// The machine the pane's programs run on; `None` for this host.
    pub host: Option<Host>,
}

/// A machine a pane's programs run on.
#[derive(Clone)]
pub struct Host {
    pub provider: Arc<dyn Provider>,
    /// Someone else's sandbox (an "open shell"): never created by us.
    pub borrowed: bool,
    pub sprite: String,
    pub image: Option<String>,
    pub rt: tokio::runtime::Handle,
}

/// Where a VM pane's session is, so a restarted daemon can reattach:
/// `exec.json` in the pane's directory.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecRecord {
    pub session: String,
    /// Bytes of it in the log.
    pub received: u64,
}

impl ExecRecord {
    pub fn read(dir: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(dir.join("exec.json")).ok()?).ok()
    }
}
/// How pane processes are started: through the shim (so a restarted daemon
/// can still learn how they exit), in their own systemd scope (so restarting
/// the daemon's service doesn't kill them), with their terminal kept in the
/// FD store (so it stays open while the daemon is gone).
#[derive(Clone, Debug)]
pub struct Launcher {
    /// This executable, which also serves as the shim.
    pub exe: PathBuf,
    pub scopes: bool,
    pub fd_store: bool,
}

impl Launcher {
    /// What works here: scopes and the FD store need a systemd user service.
    pub fn detect() -> Self {
        let systemd = crate::sys::under_systemd();
        let have_run = std::process::Command::new("systemd-run")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        Self {
            exe: std::env::current_exe().unwrap_or_else(|_| "illogicald".into()),
            scopes: systemd && have_run,
            fd_store: systemd,
        }
    }
}

fn fd_name(pane: PaneId) -> String {
    format!("pane-{pane}")
}

/// A running process on its own PTY.
struct Process {
    pid: u32,
    master: File,
    writer: Sender<Vec<u8>>,
}

impl Process {
    fn start(
        spawn: &Spawn,
        launch: &Launcher,
        record: &Path,
        cols: u16,
        rows: u16,
        pane: PaneId,
        events: Sender<Cmd>,
    ) -> std::io::Result<Self> {
        let ws = Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        let pty = openpty(Some(&ws), None)?;
        // openpty leaves the master inheritable; the child must not hold its
        // own master or it never sees a hangup (spike S3).
        fcntl(&pty.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        let stdio = |fd: &OwnedFd| fd.try_clone().map(Stdio::from);

        let cwd = if spawn.cwd.is_dir() { spawn.cwd.as_path() } else { Path::new("/") };
        let _ = std::fs::remove_file(record);
        let mut cmd = if launch.scopes {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--scope", "--quiet", "--collect"])
                .arg(format!("--unit=illogical-pane-{pane}-{nanos}"))
                .arg("--")
                .arg(&launch.exe);
            c
        } else {
            Command::new(&launch.exe)
        };
        cmd.arg("_shim")
            .arg("--record")
            .arg(record)
            .arg("--")
            .arg(&spawn.program)
            .args(&spawn.args)
            .current_dir(cwd)
            .envs(spawn.env.iter().map(|(k, v)| (k, v)))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("ILLOGICAL_PANE", pane.to_string())
            .stdin(stdio(&pty.slave)?)
            .stdout(stdio(&pty.slave)?)
            .stderr(stdio(&pty.slave)?);
        let mut child: Child = cmd.spawn()?;
        drop(pty.slave);
        // The shim reports the program's pid once it has forked it.
        let deadline = Instant::now() + Duration::from_secs(5);
        let (pid, _) = loop {
            if let Some(p) = crate::shim::read_record(record).pid {
                break p;
            }
            if Instant::now() > deadline || child.try_wait().ok().flatten().is_some() {
                let _ = child.kill();
                return Err(std::io::Error::other("the pane shim did not start the program"));
            }
            thread::sleep(Duration::from_millis(5));
        };
        // Reap the shim (or systemd-run) if it ends while we're its parent.
        thread::spawn(move || {
            let _ = child.wait();
        });
        info!(pane, pid, program = %spawn.program, cwd = %cwd.display(), scope = launch.scopes, "started process");
        let master = File::from(pty.master);
        if launch.fd_store {
            crate::sys::remove_fd(&fd_name(pane));
            if !crate::sys::store_fd(&fd_name(pane), master.as_raw_fd()) {
                warn!(pane, "couldn't keep the terminal in the FD store");
            }
        }
        Self::run(pid, master, record.to_owned(), pane, events)
    }

    /// Take over a pane whose terminal and program outlived the previous
    /// daemon.
    fn adopt(master: OwnedFd, record: &Path, pane: PaneId, events: Sender<Cmd>) -> std::io::Result<Self> {
        let r = crate::shim::read_record(record);
        let Some((pid, _)) = r.pid.filter(|_| crate::shim::alive(&r)) else {
            return Err(std::io::Error::other("the pane's program is gone"));
        };
        info!(pane, pid, "adopted process");
        Self::run(pid, File::from(master), record.to_owned(), pane, events)
    }

    fn run(pid: u32, master: File, record: PathBuf, pane: PaneId, events: Sender<Cmd>) -> std::io::Result<Self> {
        let mut reader = master.try_clone()?;
        let out = events.clone();
        thread::Builder::new().name(format!("pane{pane}-read")).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    // EIO is how a PTY master reports that the slave closed.
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out.send(Cmd::Output(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        })?;

        let (writer, inputs) = unbounded::<Vec<u8>>();
        let mut w = master.try_clone()?;
        thread::Builder::new().name(format!("pane{pane}-write")).spawn(move || {
            for data in inputs {
                if w.write_all(&data).is_err() {
                    break;
                }
            }
        })?;

        thread::Builder::new().name(format!("pane{pane}-wait")).spawn(move || {
            let (code, signal) = wait_for_exit(pid, &record);
            let _ = events.send(Cmd::Exited { key: pid as u64, code, signal });
        })?;

        Ok(Self { pid, master, writer })
    }

    fn resize(&self, cols: u16, rows: u16) {
        let ws = Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ reads one Winsize from the pointer.
        let rc = unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
        if rc < 0 {
            warn!(error = %std::io::Error::last_os_error(), "TIOCSWINSZ failed");
        }
    }

    /// Hang up the process group (the shell is a session leader), and kill
    /// it if it is still around a few seconds later.
    fn hang_up(&self) {
        let pgid = self.pid as libc::pid_t;
        // SAFETY: plain signal sends.
        unsafe { libc::killpg(pgid, libc::SIGHUP) };
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(3));
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        });
    }
}

/// What a pane's program runs on: a local PTY, or an exec on its machine.
enum Backend {
    Local(Process),
    Vm { exec: Exec, key: u64 },
}

impl Backend {
    fn key(&self) -> u64 {
        match self {
            Backend::Local(p) => p.pid as u64,
            Backend::Vm { key, .. } => *key,
        }
    }
    fn send(&self, data: Vec<u8>) {
        match self {
            Backend::Local(p) => {
                let _ = p.writer.send(data);
            }
            Backend::Vm { exec, .. } => exec.input(data),
        }
    }
    fn resize(&self, cols: u16, rows: u16) {
        match self {
            Backend::Local(p) => p.resize(cols, rows),
            Backend::Vm { exec, .. } => exec.resize(cols, rows),
        }
    }
    fn hang_up(&self) {
        match self {
            Backend::Local(p) => p.hang_up(),
            Backend::Vm { exec, .. } => exec.hang_up(),
        }
    }
}

/// Keys for machine execs, above any pid.
static NEXT_EXEC: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 32);

/// Wait for a process that may not be our child (a restarted daemon is no
/// longer its parent), then read how it ended from the shim's record.
fn wait_for_exit(pid: u32, record: &Path) -> (Option<i32>, Option<i32>) {
    if !crate::procinfo::wait_gone(pid) {
        // Can't be watched: poll until it's gone.
        while crate::procinfo::start_time(pid).is_some() {
            thread::sleep(Duration::from_millis(200));
        }
    }
    // The shim writes the status right after reaping; give it a moment.
    for _ in 0..200 {
        match crate::shim::read_record(record).exit {
            Some(crate::shim::Ended::Code(c)) => return (Some(c), None),
            Some(crate::shim::Ended::Signal(s)) => return (Some(128 + s), Some(s)),
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    (None, Some(libc::SIGKILL))
}

/// Recent output, addressed by absolute stream offset.
struct Ring {
    buf: VecDeque<u8>,
    /// Absolute offset of `buf[0]`.
    start: u64,
}

impl Ring {
    fn end(&self) -> u64 {
        self.start + self.buf.len() as u64
    }
    fn push(&mut self, data: &[u8]) {
        self.buf.extend(data);
        let excess = self.buf.len().saturating_sub(RING_BYTES);
        self.buf.drain(..excess);
        self.start += excess as u64;
    }
    /// Bytes from `offset` to the end, if still held.
    fn since(&self, offset: u64) -> Option<Vec<u8>> {
        if offset < self.start || offset > self.end() {
            return None;
        }
        Some(self.buf.range((offset - self.start) as usize..).copied().collect())
    }
}

struct Waiting {
    enter: Spawn,
    escape: Option<Spawn>,
}

struct State {
    id: PaneId,
    engine: GhosttyEngine,
    process: Option<Backend>,
    host: Option<Host>,
    /// The exec session followed now, and how many of its bytes are logged
    /// (and what `exec.json` says, to write it only when that changes).
    exec: Option<ExecRecord>,
    exec_saved: Option<(String, u64)>,
    /// A resumed session that turns out to be gone starts like this.
    resume_otherwise: Option<Start>,
    waiting: Option<Waiting>,
    ring: Ring,
    log: Option<PaneLog>,
    subs: HashMap<ClientId, Subscriber>,
    closing: bool,
    shell: Spawn,
    launch: Launcher,
    /// The shim's record of the pane's program.
    record: PathBuf,
    hold: bool,
    notices: NoticeSink,
    events: Sender<Cmd>,
    pid: Arc<AtomicU32>,
    running: Arc<AtomicBool>,
    unsaved: u64,
    last_output: Instant,
    scanner: crate::osc::Scanner,
    /// The command line reported just before its command starts.
    pending_text: Option<String>,
    status: Arc<std::sync::Mutex<Status>>,
    last_time_mark: Instant,
}

pub fn spawn_pane(setup: Setup) -> std::io::Result<PaneHandle> {
    let Setup { id, cols, rows, log, restore, start, shell, launch, hold, notices, host } = setup;
    let record = log.dir().join("process");
    let (tx, rx) = unbounded();
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64;
    let pid = Arc::new(AtomicU32::new(0));
    let running = Arc::new(AtomicBool::new(false));
    let status = Arc::new(std::sync::Mutex::new(Status { end: log.end(), ..Default::default() }));
    let handle =
        PaneHandle { id, epoch, pid: pid.clone(), running: running.clone(), status: status.clone(), tx: tx.clone() };
    thread::Builder::new().name(format!("pane{id}-vt")).spawn(move || {
        let mut log = log;
        let engine = if restore { restore_engine(&log, id, cols, rows) } else { GhosttyEngine::new(cols, rows) };
        if let Err(e) = log.record(log.end(), Event::Resize { cols, rows }) {
            warn!(pane = id, error = %e, "can't write pane index");
        }
        let mut st = State {
            id,
            engine,
            process: None,
            host,
            exec: None,
            exec_saved: None,
            resume_otherwise: None,
            waiting: None,
            ring: Ring { buf: VecDeque::new(), start: log.end() },
            log: Some(log),
            subs: HashMap::new(),
            closing: false,
            shell,
            launch,
            record,
            hold,
            notices,
            events: tx,
            pid,
            running,
            unsaved: 0,
            last_output: Instant::now(),
            scanner: crate::osc::Scanner::new(),
            pending_text: None,
            status,
            last_time_mark: Instant::now() - Duration::from_secs(60),
        };
        let adopting = matches!(start, Start::Adopt(_) | Start::Resume { .. });
        if restore && !adopting {
            st.restored_banner();
        }
        st.begin(start);
        run(st, rx);
    })?;
    Ok(handle)
}

/// Rebuild a pane's terminal: the checkpoint plus the log after it, or
/// failing that, the tail of the log. Answers the replayed programs asked
/// for are dropped; they were answered at the time.
fn restore_engine(log: &PaneLog, id: PaneId, cols: u16, rows: u16) -> GhosttyEngine {
    let events = log.events();
    let mut from_checkpoint = None;
    if let Some((offset, bytes)) = log.load_checkpoint() {
        if offset < log.start() || offset > log.end() || log.end() - offset > RESTORE_REPLAY_BYTES {
            info!(pane = id, offset, "checkpoint too old to use");
        } else {
            match GhosttyEngine::from_checkpoint(&bytes) {
                Ok(e) => from_checkpoint = Some((offset, e)),
                Err(e) => info!(pane = id, error = %e, "ignoring checkpoint"),
            }
        }
    }
    let (from, mut engine) = match from_checkpoint {
        Some(x) => x,
        None => {
            let from = log.end().saturating_sub(RESTORE_REPLAY_BYTES).max(log.start());
            let (c, r) = events
                .iter()
                .rev()
                .find_map(|(o, e)| match e {
                    Event::Resize { cols, rows } if *o <= from => Some((*cols, *rows)),
                    _ => None,
                })
                .unwrap_or((cols, rows));
            (from, GhosttyEngine::new(c, r))
        }
    };
    match log.read_from(from) {
        Ok((start, bytes)) => {
            // Replay in pieces, resizing where the pane was resized.
            let mut at = start;
            for (offset, event) in events.iter().filter(|(o, _)| *o > start) {
                if let Event::Resize { cols, rows } = event {
                    let upto = ((*offset - start) as usize).min(bytes.len());
                    engine.feed(&bytes[(at - start) as usize..upto]);
                    engine.resize(*cols, *rows);
                    at = start + upto as u64;
                }
            }
            engine.feed(&bytes[(at - start) as usize..]);
            info!(pane = id, from = start, replayed = bytes.len(), "restored");
        }
        Err(e) => warn!(pane = id, error = %e, "can't read pane log"),
    }
    let _ = engine.take_replies();
    engine.resize(cols, rows);
    engine
}

/// Local time as HH:MM on a weekday, for the restored marker.
fn local_time(ms: u64) -> String {
    let t = (ms / 1000) as libc::time_t;
    // SAFETY: localtime_r writes one tm; both pointers are valid.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    format!(
        "{} {:04}-{:02}-{:02} {:02}:{:02}",
        DAYS[tm.tm_wday.rem_euclid(7) as usize],
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

fn run(mut st: State, rx: Receiver<Cmd>) {
    loop {
        let cmd = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => {
                if st.unsaved > 0 && st.last_output.elapsed() >= CHECKPOINT_IDLE {
                    st.checkpoint();
                }
                st.check_quiet();
                st.save_exec();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match cmd {
            Cmd::Output(data) => {
                st.output(&data);
                if st.unsaved >= CHECKPOINT_BYTES {
                    st.checkpoint();
                }
            }
            Cmd::Input(data) => st.input(data),
            Cmd::Attach { sub, offset } => st.attach(sub, offset),
            Cmd::Detach { client } => {
                st.subs.remove(&client);
            }
            Cmd::Resize { cols, rows } => st.resize(cols, rows),
            Cmd::Purge => st.purge(),
            Cmd::Checkpoint(done) => {
                st.checkpoint();
                let _ = done.send(());
            }
            Cmd::Capture { format, scope, reply } => {
                let _ = reply.send(st.capture(format, scope));
            }
            Cmd::Close => {
                st.closing = true;
                st.subs.clear();
                match &st.process {
                    Some(p) => p.hang_up(),
                    None => return st.finish(),
                }
            }
            Cmd::Release => {
                if matches!(st.process, Some(Backend::Vm { .. })) {
                    st.ended();
                    st.forget_exec();
                }
            }
            Cmd::Restart { start, note } => {
                if st.closing {
                    continue;
                }
                if let Some(Backend::Local(p)) = &st.process {
                    p.hang_up();
                }
                st.ended();
                st.forget_exec();
                st.waiting = None;
                st.resume_otherwise = None;
                let end = {
                    let mut s = st.status.lock().unwrap();
                    s.busy = false;
                    s.current.is_some().then_some(s.end)
                };
                if let Some(end) = end {
                    st.signal(end, Signal::CommandEnd { exit: None });
                }
                st.output(format!("\x1b[0m\r\n\x1b[2m── {note} ──\x1b[0m\r\n").as_bytes());
                st.begin(start);
                st.notify(What::Started);
            }
            Cmd::Exec { key, event } => {
                if st.process.as_ref().map(|p| p.key()) != Some(key) {
                    continue;
                }
                match event {
                    ExecEvent::Output(data) => {
                        if let Some(e) = &mut st.exec {
                            e.received += data.len() as u64;
                        }
                        st.output(&data);
                        if st.unsaved >= CHECKPOINT_BYTES {
                            st.checkpoint();
                        }
                    }
                    ExecEvent::Session(session) => {
                        info!(pane = st.id, session, "attached to machine session");
                        st.resume_otherwise = None;
                        let received = st.exec.as_ref().map_or(0, |e| e.received);
                        st.exec = Some(ExecRecord { session, received });
                        st.save_exec();
                        st.notify(What::Machine(true));
                    }
                    ExecEvent::Exited(code) => {
                        info!(pane = st.id, ?code, "machine process exited");
                        st.forget_exec();
                        if st.ended() {
                            return st.finish();
                        }
                        st.exited(code, None);
                    }
                    ExecEvent::Lost { machine_gone } => {
                        info!(pane = st.id, machine_gone, "lost the machine session");
                        st.forget_exec();
                        if st.ended() {
                            return st.finish();
                        }
                        st.lost(machine_gone);
                    }
                }
            }
            Cmd::Exited { key, code, signal } => {
                if st.process.as_ref().map(|p| p.key()) != Some(key) {
                    continue;
                }
                let pid = key;
                info!(pane = st.id, pid, ?code, ?signal, "process exited");
                st.process = None;
                st.pid.store(0, Ordering::Relaxed);
                st.running.store(false, Ordering::Relaxed);
                if st.closing {
                    return st.finish();
                }
                st.exited(code, signal);
            }
        }
    }
}

impl State {
    fn begin(&mut self, start: Start) {
        let id = self.id;
        match start {
            Start::Now(spawn) => self.start(&spawn),
            Start::Run { spawn, text } => {
                if self.host.is_none() {
                    self.status.lock().unwrap().cwd = Some(spawn.cwd.display().to_string());
                }
                let at = self.ring.end();
                self.signal(at, Signal::CommandLine { text });
                self.signal(at, Signal::CommandStart);
                self.start(&spawn);
            }
            Start::Adopt(master) => match Process::adopt(master, &self.record, id, self.events.clone()) {
                Ok(p) => {
                    self.pid.store(p.pid, Ordering::Relaxed);
                    self.running.store(true, Ordering::Relaxed);
                    self.process = Some(Backend::Local(p));
                }
                Err(e) => {
                    info!(pane = id, error = %e, "can't adopt; treating as ended");
                    self.exited(None, Some(libc::SIGKILL));
                }
            },
            Start::Resume { session, received, otherwise } => {
                let Some(host) = self.host.clone() else { return self.begin(*otherwise) };
                info!(pane = id, sprite = host.sprite, session, received, "reattaching to machine session");
                self.exec = Some(ExecRecord { session: session.clone(), received });
                self.exec_saved = Some((session.clone(), received));
                self.resume_otherwise = Some(*otherwise);
                self.attach_exec(&host, Begin::Resume { session, received });
            }
            Start::Wait { banner, enter, escape } => {
                self.output(banner.as_bytes());
                self.waiting = Some(Waiting { enter, escape });
            }
        }
    }

    fn attach_exec(&mut self, host: &Host, begin: Begin) {
        let key = NEXT_EXEC.fetch_add(1, Ordering::Relaxed);
        let events = self.events.clone();
        let exec =
            crate::machine::start(&host.rt, host.provider.clone(), host.sprite.clone(), begin, self.engine.size(), {
                move |event| events.send(Cmd::Exec { key, event }).is_ok()
            });
        self.running.store(true, Ordering::Relaxed);
        self.process = Some(Backend::Vm { exec, key });
    }

    /// The process is gone; true if the pane is closing and should finish.
    fn ended(&mut self) -> bool {
        self.process = None;
        self.pid.store(0, Ordering::Relaxed);
        self.running.store(false, Ordering::Relaxed);
        self.closing
    }

    /// A VM pane lost its session: the machine is gone, or the session is.
    fn lost(&mut self, machine_gone: bool) {
        if let Some(otherwise) = self.resume_otherwise.take() {
            // Restored after the machine went (a reboot of its host): start
            // as the pane's policy says, on a fresh machine.
            self.restored_banner();
            let note: &[u8] = if machine_gone {
                b"\x1b[2m[the machine was lost; this is a new one]\x1b[0m\r\n"
            } else {
                b"\x1b[2m[the session on the machine was lost]\x1b[0m\r\n"
            };
            self.output(note);
            return self.begin(otherwise);
        }
        let pending = self.status.lock().unwrap().current.is_some();
        if pending {
            let end = self.status.lock().unwrap().end;
            self.signal(end, Signal::CommandEnd { exit: None });
        }
        {
            let mut st = self.status.lock().unwrap();
            st.busy = false;
            st.at_prompt = false;
            st.exited = Some(None);
        }
        let note = if machine_gone {
            self.notify(What::Machine(false));
            "machine gone · press Enter for a new one"
        } else {
            "lost the session on the machine · press Enter for a shell"
        };
        self.output(format!("\r\n\x1b[0m\x1b[2m[{note}]\x1b[0m\r\n").as_bytes());
        self.waiting = Some(Waiting { enter: self.shell.clone(), escape: None });
        self.notify(What::Exited { code: None, close: false });
    }

    /// The session ended: a restart has nothing to reattach to, and restores
    /// the pane by its policy, as for a local pane.
    fn forget_exec(&mut self) {
        self.exec = None;
        self.exec_saved = None;
        if let Some(log) = &self.log {
            let _ = std::fs::remove_file(log.dir().join("exec.json"));
        }
    }

    /// Write `exec.json` if it changed. Not synced: it matters across a
    /// daemon restart, where the page cache survives.
    fn save_exec(&mut self) {
        let Some(e) = &self.exec else { return };
        let now = (e.session.clone(), e.received);
        if self.exec_saved.as_ref() == Some(&now) {
            return;
        }
        let Some(log) = &self.log else { return };
        match serde_json::to_vec(e) {
            Ok(b) => {
                if let Err(err) = std::fs::write(log.dir().join("exec.json"), b) {
                    warn!(pane = self.id, error = %err, "can't write exec.json");
                }
            }
            Err(_) => return,
        }
        self.exec_saved = Some(now);
    }

    fn start(&mut self, spawn: &Spawn) {
        {
            let mut st = self.status.lock().unwrap();
            st.exited = None;
            st.at_prompt = false;
            if self.host.is_none() {
                st.cwd.get_or_insert_with(|| spawn.cwd.display().to_string());
            }
        }
        if let Some(host) = self.host.clone() {
            // A new session: what the old one left in exec.json no longer
            // applies.
            self.exec = Some(ExecRecord::default());
            self.exec_saved = None;
            if let Some(log) = &self.log {
                let _ = std::fs::remove_file(log.dir().join("exec.json"));
            }
            info!(pane = self.id, sprite = host.sprite, program = %spawn.program, "starting on machine");
            let image = host.image.clone();
            return self.attach_exec(&host, Begin::New { spawn: spawn.clone(), image, create: !host.borrowed });
        }
        let (cols, rows) = self.engine.size();
        match Process::start(spawn, &self.launch, &self.record, cols, rows, self.id, self.events.clone()) {
            Ok(p) => {
                self.pid.store(p.pid, Ordering::Relaxed);
                self.running.store(true, Ordering::Relaxed);
                self.process = Some(Backend::Local(p));
            }
            Err(e) => {
                warn!(pane = self.id, error = %e, "can't start process");
                self.output(format!("\x1b[31m[could not start {}: {e}]\x1b[0m\r\n", spawn.program).as_bytes());
                // Leave the pane to the mux: it closes like an exit.
                self.notify(What::Exited { code: None, close: true });
            }
        }
    }

    /// An ordinary exit closes the pane. A process killed by a signal (a
    /// reboot, the OOM killer) didn't mean to go: keep the pane, its
    /// scrollback, and offer a new shell.
    fn exited(&mut self, code: Option<i32>, signal: Option<i32>) {
        // A program that ends mid-command never reports the end itself (a
        // `run` command, a killed shell): close it with the exit code.
        let (pending, end) = {
            let mut st = self.status.lock().unwrap();
            st.busy = false;
            st.at_prompt = false;
            st.exited = Some(code);
            (st.current.is_some(), st.end)
        };
        if pending {
            self.signal(end, Signal::CommandEnd { exit: code });
        }
        match signal {
            None if !self.hold => self.notify(What::Exited { code, close: true }),
            None => {
                let c = code.unwrap_or(-1);
                let note = format!("\r\n\x1b[0m\x1b[2m[exited with code {c} · press Enter for a shell]\x1b[0m\r\n");
                self.output(note.as_bytes());
                self.waiting = Some(Waiting { enter: self.shell.clone(), escape: None });
                self.notify(What::Exited { code, close: false });
            }
            Some(sig) => {
                let note =
                    format!("\r\n\x1b[0m\x1b[2m[process ended by signal {sig} · press Enter for a shell]\x1b[0m\r\n");
                self.output(note.as_bytes());
                self.waiting = Some(Waiting { enter: self.shell.clone(), escape: None });
                self.notify(What::Exited { code: Some(128 + sig), close: false });
            }
        }
    }

    fn input(&mut self, data: Vec<u8>) {
        {
            let mut st = self.status.lock().unwrap();
            st.input_at = st.end;
        }
        if let Some(p) = &self.process {
            p.send(data);
            return;
        }
        let Some(w) = &self.waiting else { return };
        let spawn = if data.iter().any(|b| *b == b'\r' || *b == b'\n') {
            Some(w.enter.clone())
        } else if data == [0x1b] {
            w.escape.clone()
        } else {
            None
        };
        if let Some(spawn) = spawn {
            self.waiting = None;
            self.output(b"\x1b[0m\r\n");
            self.start(&spawn);
            self.hold = false;
            self.notify(What::Started);
        }
    }

    fn restored_banner(&mut self) {
        let at = now_ms();
        if let Some(log) = &mut self.log {
            let _ = log.record(log.end(), Event::Restore { at_ms: at });
        }
        // Leave whatever full-screen program was running (only if one was:
        // 1049l also restores the saved cursor, which would move us), reset
        // modes, and mark where the old output ends.
        let leave_alt = if self.engine.alt_screen() { "\x1b[?1049l" } else { "" };
        // On the main screen, below everything on it: a program that drew in
        // place (Claude Code's TUI) may have left the cursor mid-screen, and
        // the marker would land on top of what it drew.
        let below = match self.engine.content_rows() {
            n if !self.engine.alt_screen() && n > 0 => format!("\x1b[{n};1H"),
            _ => String::new(),
        };
        // DECSTR (`CSI ! p`) leaves input modes alone, so also turn off what
        // a program that died with the old daemon may have left on: mouse
        // reporting, focus reports, application cursor keys and keypad, the
        // kitty keyboard stack; and show the cursor.
        const INPUT_RESET: &str = "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[?1016l\x1b[?1004l\x1b[?1l\x1b>\x1b[<99u\x1b[?25h";
        let banner = format!(
            "{leave_alt}\x1b[!p{INPUT_RESET}{below}\x1b[0m\r\n\x1b[2m── restored {} ──\x1b[0m\r\n",
            local_time(at)
        );
        self.output(banner.as_bytes());
    }

    fn output(&mut self, data: &[u8]) {
        let offset = self.ring.end();
        self.engine.feed(data);
        let replies = self.engine.take_replies();
        if !replies.is_empty()
            && let Some(p) = &self.process
        {
            p.send(replies);
        }
        self.ring.push(data);
        if let Some(log) = &mut self.log
            && let Err(e) = log.append(data)
        {
            warn!(pane = self.id, error = %e, "can't write pane log");
        }
        self.unsaved += data.len() as u64;
        let was_quiet = self.last_output.elapsed() >= QUIET;
        self.last_output = Instant::now();
        let frame = Frame { kind: FrameKind::Output, pane: self.id, offset, data: data.to_vec() }.encode();
        self.broadcast(|| ToClient::Frame(frame.clone()));

        let end = offset + data.len() as u64;
        if self.last_time_mark.elapsed() >= Duration::from_secs(1) {
            self.last_time_mark = Instant::now();
            self.index(offset, Event::Time { at_ms: now_ms() });
        }
        // Move the end first: a command that ends in this chunk becomes
        // `last` below, and input sent once that's visible must be marked
        // after it, or `send` then `wait` would get that command again.
        self.status.lock().unwrap().end = end;
        for (at, signal) in self.scanner.feed(data, offset) {
            self.signal(at, signal);
        }
        let modes = crate::keys::Modes {
            app_cursor: self.engine.dec_mode(1),
            mouse: [1000, 1002, 1003].iter().any(|m| self.engine.dec_mode(*m)),
            sgr_mouse: self.engine.dec_mode(1006),
        };
        let busy_now = {
            let mut st = self.status.lock().unwrap();
            st.end = end;
            st.modes = modes;
            let flip = !st.busy;
            st.busy = true;
            flip || was_quiet
        };
        if busy_now {
            self.notify(What::Busy(true));
        }
    }

    fn check_quiet(&mut self) {
        let mut st = self.status.lock().unwrap();
        if st.busy && self.last_output.elapsed() >= QUIET {
            st.busy = false;
            drop(st);
            self.notify(What::Busy(false));
        }
    }

    fn notify(&self, what: What) {
        let _ = self.notices.send(Notice { pane: self.id, what });
    }

    fn index(&mut self, offset: u64, event: Event) {
        if let Some(log) = &mut self.log
            && let Err(e) = log.record(offset, event)
        {
            warn!(pane = self.id, error = %e, "can't write pane index");
        }
    }

    /// Record what the shell integration (or a program) said, and keep the
    /// pane's command status current.
    fn signal(&mut self, at: u64, signal: Signal) {
        let ms = now_ms();
        match &signal {
            Signal::Prompt => {
                self.status.lock().unwrap().at_prompt = true;
                self.index(at, Event::Prompt { at_ms: ms })
            }
            Signal::CommandLine { text } => {
                self.pending_text = Some(text.clone()).filter(|t| !t.is_empty());
                return;
            }
            Signal::CommandStart => {
                let text = self.pending_text.take();
                let cwd = self.status.lock().unwrap().cwd.clone();
                self.index(at, Event::Command { at_ms: ms, text: text.clone(), cwd: cwd.clone() });
                let rec = CommandRec { text, cwd, start: at, started_ms: ms, ..Default::default() };
                let mut st = self.status.lock().unwrap();
                st.current = Some(rec);
                st.at_prompt = false;
            }
            Signal::CommandEnd { exit } => {
                let mut st = self.status.lock().unwrap();
                // A prompt after an empty line reports an end with no start.
                let Some(mut rec) = st.current.take() else { return };
                rec.end = Some(at);
                rec.ended_ms = Some(ms);
                rec.exit = *exit;
                st.last = Some(rec);
                drop(st);
                self.index(at, Event::End { at_ms: ms, exit: *exit });
            }
            Signal::Cwd { path } => {
                self.status.lock().unwrap().cwd = Some(path.clone());
                self.index(at, Event::Cwd { path: path.clone() });
            }
            Signal::Notify { title, body } => {
                self.index(at, Event::Notify { at_ms: ms, title: title.clone(), body: body.clone() })
            }
            Signal::Bell => self.index(at, Event::Bell { at_ms: ms }),
        }
        self.notify(What::Signal(signal));
    }

    /// The screen, the scrollback, or the last command's output.
    fn capture(&mut self, format: CaptureFormat, scope: CaptureScope) -> String {
        if scope == CaptureScope::LastCommand {
            let st = self.status.lock().unwrap().clone();
            let Some(rec) = st.current.or(st.last) else { return String::new() };
            let to = rec.end.unwrap_or(st.end);
            let bytes = match &self.log {
                Some(log) => log.read_from(rec.start).map(|(from, b)| {
                    let skip = (rec.start.saturating_sub(from)) as usize;
                    let take = (to.saturating_sub(rec.start)) as usize;
                    b.get(skip..(skip + take).min(b.len())).unwrap_or_default().to_vec()
                }),
                None => Ok(vec![]),
            }
            .unwrap_or_default();
            return match format {
                CaptureFormat::Text => crate::osc::strip(&bytes),
                CaptureFormat::Ansi => String::from_utf8_lossy(&bytes).into_owned(),
                CaptureFormat::Html => {
                    let (cols, _) = self.engine.size();
                    let mut e = GhosttyEngine::new(cols, 500);
                    e.feed(&bytes);
                    e.html()
                }
            };
        }
        let full = match format {
            CaptureFormat::Text => self.engine.plain_text(),
            CaptureFormat::Ansi => self.engine.vt_text(),
            CaptureFormat::Html => self.engine.html(),
        };
        if scope == CaptureScope::Scrollback || format != CaptureFormat::Text {
            return full;
        }
        // The visible screen: the last screenful of lines.
        let rows = self.engine.size().1 as usize;
        let lines: Vec<&str> = full.lines().collect();
        lines[lines.len().saturating_sub(rows)..].join("\n")
    }

    fn checkpoint(&mut self) {
        self.save_exec();
        let Some(log) = &mut self.log else { return };
        let started = Instant::now();
        let bytes = self.engine.checkpoint();
        match log.save_checkpoint(log.end(), &bytes) {
            Ok(()) => {
                debug!(pane = self.id, bytes = bytes.len(), ms = started.elapsed().as_millis() as u64, "checkpoint");
                self.unsaved = 0;
            }
            Err(e) => warn!(pane = self.id, error = %e, "can't write checkpoint"),
        }
    }

    fn purge(&mut self) {
        if let Some(log) = &mut self.log {
            let _ = log.purge();
        }
        // Clear the screen and scrollback here and in every client, then
        // ask whatever is running to redraw (Ctrl-L) on the clean screen.
        self.output(b"\x1b[H\x1b[2J\x1b[3J");
        if let Some(p) = &self.process {
            p.send(vec![0x0c]);
        }
        self.checkpoint();
    }

    fn finish(mut self) {
        if self.launch.fd_store {
            crate::sys::remove_fd(&fd_name(self.id));
        }
        if let Some(log) = self.log.take() {
            log.retire(self.id);
        }
    }

    /// Queue an item for every subscriber, resyncing any that are full.
    fn broadcast(&mut self, item: impl Fn() -> ToClient) {
        let lagged: Vec<ClientId> =
            self.subs.iter().filter(|(_, sub)| sub.data.try_send(item()).is_err()).map(|(id, _)| *id).collect();
        for id in lagged {
            if let Some(sub) = self.subs.remove(&id) {
                debug!(pane = self.id, client = id, "client fell behind; resync");
                let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Resync { pane: self.id }));
            }
        }
    }

    fn attach(&mut self, sub: Subscriber, offset: Option<u64>) {
        if self.closing {
            return;
        }
        let end = self.ring.end();
        let (cols, rows) = self.engine.size();
        let replay = offset.filter(|o| end.saturating_sub(*o) <= MAX_REPLAY_BYTES).and_then(|o| self.ring.since(o));
        let frame = match replay {
            Some(bytes) if bytes.is_empty() => None,
            Some(bytes) => Some(Frame { kind: FrameKind::Output, pane: self.id, offset: offset.unwrap(), data: bytes }),
            None => Some(Frame { kind: FrameKind::Snapshot, pane: self.id, offset: end, data: self.engine.snapshot() }),
        };
        debug!(pane = self.id, client = sub.client, ?offset, end, kind = ?frame.as_ref().map(|f| f.kind), "attach");
        // The size goes first so the client resizes before drawing.
        let size = ServerMsg::Size { pane: self.id, cols, rows };
        let queued = sub.data.try_send(ToClient::Msg(size)).is_ok()
            && frame.is_none_or(|f| sub.data.try_send(ToClient::Frame(f.encode())).is_ok());
        if !queued {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Resync { pane: self.id }));
            return;
        }
        self.subs.insert(sub.client, sub);
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 || self.engine.size() == (cols, rows) {
            return;
        }
        self.engine.resize(cols, rows);
        if let Some(p) = &self.process {
            p.resize(cols, rows);
        }
        if let Some(log) = &mut self.log {
            let _ = log.record(log.end(), Event::Resize { cols, rows });
        }
        let id = self.id;
        self.broadcast(|| ToClient::Msg(ServerMsg::Size { pane: id, cols, rows }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_tail_and_addresses_by_offset() {
        let mut r = Ring { buf: VecDeque::new(), start: 0 };
        r.push(b"hello ");
        r.push(b"world");
        assert_eq!(r.end(), 11);
        assert_eq!(r.since(6).unwrap(), b"world");
        assert_eq!(r.since(11).unwrap(), b"");
        assert!(r.since(12).is_none());
        r.push(&vec![b'x'; RING_BYTES]);
        assert_eq!(r.start, 11);
        assert!(r.since(5).is_none());
        assert_eq!(r.since(r.end() - 2).unwrap(), b"xx");
    }

    #[test]
    fn quoting_for_rerun() {
        assert_eq!(shell_quote("make"), "make");
        assert_eq!(shell_quote("--flag=a/b"), "--flag=a/b");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
