//! A pane: a process on a PTY, the server-side terminal state it draws, and
//! the clients watching it.
//!
//! Each pane runs a VT thread that owns everything stateful (libghostty's
//! terminal is `!Send`). PTY output, client attaches, resizes and process
//! exits all arrive on one channel, so a client's snapshot or replay and the
//! live output after it are always in order.

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
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossbeam_channel::{Receiver, Sender, unbounded};
use illogical_proto::{ClientId, Frame, FrameKind, PaneId, ServerMsg};
use illogical_vt::{GhosttyEngine, VtEngine};
use nix::{
    fcntl::{FcntlArg, FdFlag, fcntl},
    libc,
    pty::{Winsize, openpty},
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Output kept in memory for clients that reconnect: anything within this
/// many bytes of the end is replayed instead of snapshotted.
const RING_BYTES: usize = 2 * 1024 * 1024;
const MAX_REPLAY_BYTES: u64 = 1024 * 1024;
/// Frames queued per client before it counts as too slow and is resynced.
pub const CLIENT_QUEUE: usize = 1024;

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

/// Told to the multiplexer when a pane's process ends.
pub type ExitSink = mpsc::UnboundedSender<(PaneId, Option<i32>)>;

enum Cmd {
    Output(Vec<u8>),
    Exited {
        code: Option<i32>,
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
    Close,
}

#[derive(Clone)]
pub struct PaneHandle {
    pub id: PaneId,
    pub epoch: u64,
    pid: Arc<AtomicU32>,
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
    /// Hang up the process; the pane's thread ends when it exits.
    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
    }
    /// The process's working directory, from /proc.
    pub fn cwd(&self) -> Option<PathBuf> {
        match self.pid.load(Ordering::Relaxed) {
            0 => None,
            pid => std::fs::read_link(format!("/proc/{pid}/cwd")).ok(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Spawn {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
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
                let code = child
                    .wait()
                    .ok()
                    .and_then(|s| s.code().or(s.signal().map(|sig| 128 + sig)));
                let _ = events.send(Cmd::Exited { code });
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

struct State {
    id: PaneId,
    engine: GhosttyEngine,
    process: Process,
    ring: Ring,
    subs: HashMap<ClientId, Subscriber>,
    closing: bool,
    on_exit: ExitSink,
}

pub fn spawn_pane(
    id: PaneId,
    spawn: &Spawn,
    cols: u16,
    rows: u16,
    on_exit: ExitSink,
) -> std::io::Result<PaneHandle> {
    let (tx, rx) = unbounded();
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let process = Process::start(spawn, cols, rows, id, tx.clone())?;
    let pid = Arc::new(AtomicU32::new(process.pid));
    let pid_out = pid.clone();
    thread::Builder::new()
        .name(format!("pane{id}-vt"))
        .spawn(move || {
            let state = State {
                id,
                engine: GhosttyEngine::new(cols, rows),
                process,
                ring: Ring {
                    buf: VecDeque::new(),
                    start: 0,
                },
                subs: HashMap::new(),
                closing: false,
                on_exit,
            };
            run(state, rx);
            pid_out.store(0, Ordering::Relaxed);
        })?;
    Ok(PaneHandle { id, epoch, pid, tx })
}

fn run(mut st: State, rx: Receiver<Cmd>) {
    for cmd in rx {
        match cmd {
            Cmd::Output(data) => st.output(&data),
            Cmd::Input(data) => {
                let _ = st.process.writer.send(data);
            }
            Cmd::Attach { sub, offset } => st.attach(sub, offset),
            Cmd::Detach { client } => {
                st.subs.remove(&client);
            }
            Cmd::Resize { cols, rows } => st.resize(cols, rows),
            Cmd::Close => {
                st.closing = true;
                st.subs.clear();
                st.process.hang_up();
            }
            Cmd::Exited { code } => {
                info!(pane = st.id, pid = st.process.pid, ?code, "process exited");
                if !st.closing {
                    let _ = st.on_exit.send((st.id, code));
                }
                return;
            }
        }
    }
}

impl State {
    fn output(&mut self, data: &[u8]) {
        let offset = self.ring.end();
        self.engine.feed(data);
        let replies = self.engine.take_replies();
        if !replies.is_empty() {
            let _ = self.process.writer.send(replies);
        }
        self.ring.push(data);
        let frame = Frame {
            kind: FrameKind::Output,
            pane: self.id,
            offset,
            data: data.to_vec(),
        }
        .encode();
        self.broadcast(|| ToClient::Frame(frame.clone()));
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
        self.process.resize(cols, rows);
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
}
