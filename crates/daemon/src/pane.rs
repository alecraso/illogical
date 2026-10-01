//! A pane: a process on a PTY, the server-side terminal state it draws, its
//! history on disk, and the clients watching it.
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
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::process::{CommandExt, ExitStatusExt},
    },
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

use crate::store::{Event, PaneLog, now_ms};

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

/// Told to the multiplexer when a pane's process ends. `close` is false when
/// the pane stays (the process was killed by a signal, so it didn't mean to
/// go away: a reboot, an OOM kill), true for an ordinary exit.
#[derive(Debug, Clone, Copy)]
pub struct Exit {
    pub pane: PaneId,
    pub code: Option<i32>,
    pub close: bool,
}

pub type ExitSink = mpsc::UnboundedSender<Exit>;

enum Cmd {
    Output(Vec<u8>),
    Exited {
        pid: u32,
        code: Option<i32>,
        signal: Option<i32>,
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
    Close,
}

#[derive(Clone)]
pub struct PaneHandle {
    pub id: PaneId,
    pub epoch: u64,
    pid: Arc<AtomicU32>,
    running: Arc<AtomicBool>,
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
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
    fn pid(&self) -> Option<u32> {
        Some(self.pid.load(Ordering::Relaxed)).filter(|p| *p != 0)
    }
    /// The process's working directory, from /proc.
    pub fn cwd(&self) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{}/cwd", self.pid()?)).ok()
    }
    /// The command in the foreground, if it isn't the shell itself: what
    /// "re-run" would run again. It is the foreground process's command line
    /// as /proc shows it now, so `bash -c 'a; b'` that exec'd into `b` reads
    /// as `b`; the typed command line needs shell integration (M3).
    pub fn command(&self) -> Option<String> {
        let shell = self.pid()?;
        // The shell is a session leader; its foreground job is the
        // terminal's foreground process group.
        let fg = std::fs::read_to_string(format!("/proc/{shell}/stat")).ok()?;
        let tpgid: i32 = fg
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(5)?
            .parse()
            .ok()?;
        if tpgid <= 0 || tpgid as u32 == shell {
            return None;
        }
        let raw = std::fs::read(format!("/proc/{tpgid}/cmdline")).ok()?;
        let args: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| shell_quote(&String::from_utf8_lossy(a)))
            .collect();
        (!args.is_empty()).then(|| args.join(" "))
    }
}

fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b))
    {
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
    pub on_exit: ExitSink,
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
        cols: u16,
        rows: u16,
        pane: PaneId,
        events: Sender<Cmd>,
    ) -> std::io::Result<Self> {
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&ws), None)?;
        // openpty leaves the master inheritable; the child must not hold its
        // own master or it never sees a hangup (spike S3).
        fcntl(&pty.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        let stdio = |fd: &OwnedFd| fd.try_clone().map(Stdio::from);

        let cwd = if spawn.cwd.is_dir() {
            spawn.cwd.as_path()
        } else {
            Path::new("/")
        };
        let mut cmd = Command::new(&spawn.program);
        cmd.args(&spawn.args)
            .current_dir(cwd)
            .envs(spawn.env.iter().map(|(k, v)| (k, v)))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("ILLOGICAL_PANE", pane.to_string())
            .stdin(stdio(&pty.slave)?)
            .stdout(stdio(&pty.slave)?)
            .stderr(stdio(&pty.slave)?);
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child: Child = cmd.spawn()?;
        drop(pty.slave);
        let pid = child.id();
        info!(pane, pid, program = %spawn.program, cwd = %cwd.display(), "started process");

        let master = File::from(pty.master);
        let mut reader = master.try_clone()?;
        let out = events.clone();
        thread::Builder::new()
            .name(format!("pane{pane}-read"))
            .spawn(move || {
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
        thread::Builder::new()
            .name(format!("pane{pane}-write"))
            .spawn(move || {
                for data in inputs {
                    if w.write_all(&data).is_err() {
                        break;
                    }
                }
            })?;

        thread::Builder::new()
            .name(format!("pane{pane}-wait"))
            .spawn(move || {
                let mut child = child;
                let status = child.wait().ok();
                let _ = events.send(Cmd::Exited {
                    pid,
                    code: status.and_then(|s| s.code()),
                    signal: status.and_then(|s| s.signal()),
                });
            })?;

        Ok(Self {
            pid,
            master,
            writer,
        })
    }

    fn resize(&self, cols: u16, rows: u16) {
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
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
        Some(
            self.buf
                .range((offset - self.start) as usize..)
                .copied()
                .collect(),
        )
    }
}

struct Waiting {
    enter: Spawn,
    escape: Option<Spawn>,
}

struct State {
    id: PaneId,
    engine: GhosttyEngine,
    process: Option<Process>,
    waiting: Option<Waiting>,
    ring: Ring,
    log: Option<PaneLog>,
    subs: HashMap<ClientId, Subscriber>,
    closing: bool,
    shell: Spawn,
    on_exit: ExitSink,
    events: Sender<Cmd>,
    pid: Arc<AtomicU32>,
    running: Arc<AtomicBool>,
    unsaved: u64,
    last_output: Instant,
}

pub fn spawn_pane(setup: Setup) -> std::io::Result<PaneHandle> {
    let Setup {
        id,
        cols,
        rows,
        log,
        restore,
        start,
        shell,
        on_exit,
    } = setup;
    let (tx, rx) = unbounded();
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let pid = Arc::new(AtomicU32::new(0));
    let running = Arc::new(AtomicBool::new(false));
    let handle = PaneHandle {
        id,
        epoch,
        pid: pid.clone(),
        running: running.clone(),
        tx: tx.clone(),
    };
    thread::Builder::new()
        .name(format!("pane{id}-vt"))
        .spawn(move || {
            let mut log = log;
            let engine = if restore {
                restore_engine(&log, id, cols, rows)
            } else {
                GhosttyEngine::new(cols, rows)
            };
            if let Err(e) = log.record(log.end(), Event::Resize { cols, rows }) {
                warn!(pane = id, error = %e, "can't write pane index");
            }
            let mut st = State {
                id,
                engine,
                process: None,
                waiting: None,
                ring: Ring {
                    buf: VecDeque::new(),
                    start: log.end(),
                },
                log: Some(log),
                subs: HashMap::new(),
                closing: false,
                shell,
                on_exit,
                events: tx,
                pid,
                running,
                unsaved: 0,
                last_output: Instant::now(),
            };
            if restore {
                st.restored_banner();
            }
            match start {
                Start::Now(spawn) => st.start(&spawn),
                Start::Wait {
                    banner,
                    enter,
                    escape,
                } => {
                    st.output(banner.as_bytes());
                    st.waiting = Some(Waiting { enter, escape });
                }
            }
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
            let from = log
                .end()
                .saturating_sub(RESTORE_REPLAY_BYTES)
                .max(log.start());
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
            Cmd::Close => {
                st.closing = true;
                st.subs.clear();
                match &st.process {
                    Some(p) => p.hang_up(),
                    None => return st.finish(),
                }
            }
            Cmd::Exited { pid, code, signal } => {
                if st.process.as_ref().map(|p| p.pid) != Some(pid) {
                    continue;
                }
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
    fn start(&mut self, spawn: &Spawn) {
        let (cols, rows) = self.engine.size();
        match Process::start(spawn, cols, rows, self.id, self.events.clone()) {
            Ok(p) => {
                self.pid.store(p.pid, Ordering::Relaxed);
                self.running.store(true, Ordering::Relaxed);
                self.process = Some(p);
            }
            Err(e) => {
                warn!(pane = self.id, error = %e, "can't start process");
                self.output(
                    format!(
                        "\x1b[31m[could not start {}: {e}]\x1b[0m\r\n",
                        spawn.program
                    )
                    .as_bytes(),
                );
                // Leave the pane to the mux: it closes like an exit.
                let _ = self.on_exit.send(Exit {
                    pane: self.id,
                    code: None,
                    close: true,
                });
            }
        }
    }

    /// An ordinary exit closes the pane. A process killed by a signal (a
    /// reboot, the OOM killer) didn't mean to go: keep the pane, its
    /// scrollback, and offer a new shell.
    fn exited(&mut self, code: Option<i32>, signal: Option<i32>) {
        match signal {
            None => {
                let _ = self.on_exit.send(Exit {
                    pane: self.id,
                    code,
                    close: true,
                });
            }
            Some(sig) => {
                let note = format!(
                    "\r\n\x1b[0m\x1b[2m[process ended by signal {sig} · press Enter for a shell]\x1b[0m\r\n"
                );
                self.output(note.as_bytes());
                self.waiting = Some(Waiting {
                    enter: self.shell.clone(),
                    escape: None,
                });
                let _ = self.on_exit.send(Exit {
                    pane: self.id,
                    code: Some(128 + sig),
                    close: false,
                });
            }
        }
    }

    fn input(&mut self, data: Vec<u8>) {
        if let Some(p) = &self.process {
            let _ = p.writer.send(data);
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
            let _ = self.on_exit.send(Exit {
                pane: self.id,
                code: None,
                close: false,
            });
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
        let leave_alt = if self.engine.alt_screen() {
            "\x1b[?1049l"
        } else {
            ""
        };
        let banner = format!(
            "{leave_alt}\x1b[!p\x1b[0m\r\n\x1b[2m── restored {} ──\x1b[0m\r\n",
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
            let _ = p.writer.send(replies);
        }
        self.ring.push(data);
        if let Some(log) = &mut self.log
            && let Err(e) = log.append(data)
        {
            warn!(pane = self.id, error = %e, "can't write pane log");
        }
        self.unsaved += data.len() as u64;
        self.last_output = Instant::now();
        let frame = Frame {
            kind: FrameKind::Output,
            pane: self.id,
            offset,
            data: data.to_vec(),
        }
        .encode();
        self.broadcast(|| ToClient::Frame(frame.clone()));
    }

    fn checkpoint(&mut self) {
        let Some(log) = &mut self.log else { return };
        let started = Instant::now();
        let bytes = self.engine.checkpoint();
        match log.save_checkpoint(log.end(), &bytes) {
            Ok(()) => {
                debug!(
                    pane = self.id,
                    bytes = bytes.len(),
                    ms = started.elapsed().as_millis() as u64,
                    "checkpoint"
                );
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
            let _ = p.writer.send(vec![0x0c]);
        }
        self.checkpoint();
    }

    fn finish(mut self) {
        if let Some(log) = self.log.take() {
            log.remove();
        }
    }

    /// Queue an item for every subscriber, resyncing any that are full.
    fn broadcast(&mut self, item: impl Fn() -> ToClient) {
        let lagged: Vec<ClientId> = self
            .subs
            .iter()
            .filter(|(_, sub)| sub.data.try_send(item()).is_err())
            .map(|(id, _)| *id)
            .collect();
        for id in lagged {
            if let Some(sub) = self.subs.remove(&id) {
                debug!(pane = self.id, client = id, "client fell behind; resync");
                let _ = sub
                    .ctrl
                    .send(ToClient::Msg(ServerMsg::Resync { pane: self.id }));
            }
        }
    }

    fn attach(&mut self, sub: Subscriber, offset: Option<u64>) {
        if self.closing {
            return;
        }
        let end = self.ring.end();
        let (cols, rows) = self.engine.size();
        let replay = offset
            .filter(|o| end.saturating_sub(*o) <= MAX_REPLAY_BYTES)
            .and_then(|o| self.ring.since(o));
        let frame = match replay {
            Some(bytes) if bytes.is_empty() => None,
            Some(bytes) => Some(Frame {
                kind: FrameKind::Output,
                pane: self.id,
                offset: offset.unwrap(),
                data: bytes,
            }),
            None => Some(Frame {
                kind: FrameKind::Snapshot,
                pane: self.id,
                offset: end,
                data: self.engine.snapshot(),
            }),
        };
        debug!(pane = self.id, client = sub.client, ?offset, end, kind = ?frame.as_ref().map(|f| f.kind), "attach");
        // The size goes first so the client resizes before drawing.
        let size = ServerMsg::Size {
            pane: self.id,
            cols,
            rows,
        };
        let queued = sub.data.try_send(ToClient::Msg(size)).is_ok()
            && frame.is_none_or(|f| sub.data.try_send(ToClient::Frame(f.encode())).is_ok());
        if !queued {
            let _ = sub
                .ctrl
                .send(ToClient::Msg(ServerMsg::Resync { pane: self.id }));
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
        self.broadcast(|| {
            ToClient::Msg(ServerMsg::Size {
                pane: id,
                cols,
                rows,
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_tail_and_addresses_by_offset() {
        let mut r = Ring {
            buf: VecDeque::new(),
            start: 0,
        };
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
