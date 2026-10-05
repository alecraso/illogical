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
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(unix)]
use std::{
    fs::File,
    io::{Read, Write},
    process::Child,
};

use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded, unbounded};
use illogical_proto::{ClientId, Frame, FrameKind, PaneId, ServerMsg};
use illogical_vt::{
    GhosttyEngine, VtEngine,
    detect::{Agent, AgentState, Debounce},
};
#[cfg(unix)]
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
/// many bytes of the end is replayed instead of snapshotted. The ring holds
/// exactly that much (M9: it used to hold 2 MiB and grow to 4).
const MAX_REPLAY_BYTES: u64 = 1024 * 1024;
const RING_BYTES: usize = MAX_REPLAY_BYTES as usize;
/// Smaller snapshots go uncompressed: not worth a client's decoder.
const MIN_ZSTD_BYTES: usize = 4096;
/// Chunks of a local program's output (up to 64 KB each) waiting for the
/// pane; past this the program waits.
const PROGRAM_QUEUE: usize = 64;
/// How far past its last ack a client that acks may be sent (#52).
const ACK_WINDOW: u64 = 512 * 1024;
/// A held-back client gets output again once it has acked this close.
const ACK_RESUME: u64 = ACK_WINDOW / 2;
/// Frames queued per client before it counts as too slow and is resynced.
const CLIENT_QUEUE: usize = 1024;
/// Live output queued per client before it counts as too slow (M9: a frame
/// is up to one 64 KiB read, so 1024 frames could hold 64 MiB). Snapshots
/// and replays count toward it but are never refused for it: an attach to
/// many panes queues them all at once.
const CLIENT_QUEUE_BYTES: usize = 8 * 1024 * 1024;
/// Checkpoint after this much output, or after this long idle with output
/// since the last one.
const CHECKPOINT_BYTES: u64 = 2 * 1024 * 1024;
const CHECKPOINT_IDLE: Duration = Duration::from_secs(5);
/// Most log a restore replays after a checkpoint, or at all without one.
const RESTORE_REPLAY_BYTES: u64 = 8 * 1024 * 1024;

/// A client's `data` queue: bounded in items, and in live output bytes.
pub fn client_queue() -> (ClientTx, ClientRx) {
    let (tx, rx) = mpsc::channel(CLIENT_QUEUE);
    let bytes = Arc::new(AtomicUsize::new(0));
    (ClientTx { tx, bytes: bytes.clone() }, ClientRx { rx, bytes })
}

#[derive(Clone, Debug)]
pub struct ClientTx {
    tx: mpsc::Sender<ToClient>,
    bytes: Arc<AtomicUsize>,
}

impl ClientTx {
    /// Queue an item, if there's room for one more.
    pub fn try_send(&self, item: ToClient) -> Result<(), Full> {
        let n = item.queued_bytes();
        self.bytes.fetch_add(n, Ordering::Relaxed);
        self.tx.try_send(item).map_err(|_| {
            self.bytes.fetch_sub(n, Ordering::Relaxed);
            Full
        })
    }

    /// Queue live output, if the client isn't already this far behind.
    fn try_send_output(&self, frame: Vec<u8>) -> Result<(), Full> {
        if self.bytes.load(Ordering::Relaxed) + frame.len() > CLIENT_QUEUE_BYTES {
            return Err(Full);
        }
        self.try_send(ToClient::Frame(frame))
    }
}

/// A client's queue had no room (or it's gone).
#[derive(Debug)]
pub struct Full;

pub struct ClientRx {
    rx: mpsc::Receiver<ToClient>,
    bytes: Arc<AtomicUsize>,
}

impl ClientRx {
    /// The next item. Cancel safe, as `mpsc::Receiver::recv` is.
    pub async fn recv(&mut self) -> Option<ToClient> {
        let item = self.rx.recv().await?;
        self.bytes.fetch_sub(item.queued_bytes(), Ordering::Relaxed);
        Some(item)
    }
}

/// What a client connection receives.
// A `ServerMsg` carrying a whole `State` is a few hundred bytes; queues
// hold a handful of these, so boxing every message isn't worth it.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum ToClient {
    Frame(Vec<u8>),
    Msg(ServerMsg),
    /// A control message already serialized (a `State` shaped for one
    /// client, M23).
    Json(String),
    /// Hang up (access revoked).
    Close,
}

impl ToClient {
    /// What it counts for in a client's queue.
    fn queued_bytes(&self) -> usize {
        match self {
            ToClient::Frame(f) => f.len(),
            ToClient::Json(j) => j.len(),
            ToClient::Msg(_) | ToClient::Close => 0,
        }
    }
}

/// A client's subscription. Everything the client must apply in order with
/// a pane's output (sizes, snapshots, output) goes through the bounded
/// `data` queue; `ctrl` is unbounded and carries layout state and the
/// resync notice for a full `data` queue.
#[derive(Clone)]
pub struct Subscriber {
    pub client: ClientId,
    pub data: ClientTx,
    pub ctrl: mpsc::UnboundedSender<ToClient>,
    /// Who this client is (M12): what it sees and may do.
    pub principal: crate::acl::Principal,
    /// What to call them when the principal doesn't say (M30): someone who
    /// is an owner here through control (the account's own login, or a
    /// team box's owner by name).
    pub name: Option<String>,
    /// The device it connected from, when that was through control with a
    /// device key (M63: its huddle signatures are checked against this).
    pub device: Option<illogical_e2e::Cert>,
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
    /// A block asks for attention with a reason of its own (M28: an
    /// editor's debugger paused, say).
    Reason(illogical_proto::Attention, illogical_proto::Reason),
    /// ...or lets go of it, if that's still why it wants you.
    Clear(illogical_proto::ReasonKind),
    /// What an editor sends its followers (M28).
    Follow(serde_json::Value),
    /// What the agent in it is doing, read off its screen (#145), and for
    /// a blocked one what it asks.
    Screen(AgentState, Option<String>),
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
    /// Who typed it (M13).
    pub by: Option<String>,
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
    /// When the last output arrived (ms since the epoch; 0: none yet). With
    /// `end`, a running byte count, the mux works out how busy a pane is
    /// without waking it (M23).
    pub last_output_ms: u64,
    /// The title the program set (OSC 0 or 2), if any.
    pub title: Option<String>,
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
        want: Want,
    },
    Ack {
        client: ClientId,
        offset: u64,
    },
    Detach {
        client: ClientId,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    /// Bytes, and who typed them if someone in particular did.
    Input(Vec<u8>, Option<String>),
    /// Something someone did here that isn't typing (M29: an approval, a
    /// follow-up), for the pane's history.
    Note(String, String),
    Purge,
    Checkpoint(Sender<()>),
    /// Text as a paste into this pane: bracketed if its program asked
    /// (M70).
    #[cfg_attr(windows, allow(dead_code))] // Uploads are Unix only, as serving is.
    EncodePaste(String, Sender<Vec<u8>>),
    Capture {
        format: CaptureFormat,
        scope: CaptureScope,
        reply: Sender<String>,
    },
    /// Read this agent's state off the screen from now on (`None`: stop).
    /// The second: an agent with rules that runs here but whose screen
    /// isn't read (not configured on this machine, #145).
    Agent(Option<&'static Agent>, Option<&'static Agent>),
    /// How the agent's screen reads now, rule by rule (`describe
    /// --detection`); `None` when no agent's screen is read.
    Detection(Sender<Option<Detection>>),
    Close,
}

/// How a pane's agent screen reads now, rule by rule (#145).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Detection {
    pub agent: &'static str,
    pub name: &'static str,
    /// What was last reported (after the debounce).
    pub shown: Option<&'static str>,
    /// The rule that matches now, if any.
    pub fired: Option<&'static str>,
    pub title: String,
    pub rules: Vec<DetectionRule>,
    /// Its screen isn't read: chant's inventory doesn't list it here
    /// (#145). No rules, then.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DetectionRule {
    pub rule: &'static str,
    pub state: &'static str,
    pub priority: u16,
    pub region: String,
    pub text: Vec<String>,
    pub matched: bool,
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

/// What a subscriber asks of an attach.
#[derive(Debug, Clone, Copy, Default)]
pub struct Want {
    /// Just past the last byte it has: replay from there if the log can.
    pub offset: Option<u64>,
    /// At most this many rows of scrollback in a snapshot.
    pub history: Option<u32>,
    /// It reads [`FrameKind::SnapshotZstd`].
    pub zstd: bool,
    /// It may see output only from here on (M13: a "from now" share):
    /// below it, the screen and no history.
    pub floor: Option<u64>,
    /// It acks what it has drawn: hold it to [`ACK_WINDOW`] (#52).
    pub acks: bool,
    /// It speaks the kitty keyboard protocol (M31).
    pub kitty_keys: bool,
}

/// Where a client that acks is in a pane's stream.
#[derive(Debug, Clone, Copy)]
struct Flow {
    /// Just past the last byte queued for it.
    sent: u64,
    /// Just past the last byte it has drawn.
    acked: u64,
    /// Held back from here, until it acks enough.
    paused: Option<u64>,
}

impl PaneHandle {
    pub fn attach(&self, sub: Subscriber, offset: Option<u64>) {
        self.attach_with(sub, Want { offset, ..Want::default() });
    }
    pub fn attach_with(&self, sub: Subscriber, want: Want) {
        let _ = self.tx.send(Cmd::Attach { sub, want });
    }
    pub fn detach(&self, client: ClientId) {
        let _ = self.tx.send(Cmd::Detach { client });
    }
    /// `client` has drawn everything before `offset`.
    pub fn ack(&self, client: ClientId, offset: u64) {
        let _ = self.tx.send(Cmd::Ack { client, offset });
    }
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.tx.send(Cmd::Resize { cols, rows });
    }
    pub fn input(&self, data: Vec<u8>) {
        let _ = self.tx.send(Cmd::Input(data, None));
    }
    /// Typing by someone (M13): when that changes, the pane's history
    /// notes it, and commands carry who started them.
    pub fn input_by(&self, data: Vec<u8>, by: String) {
        let _ = self.tx.send(Cmd::Input(data, Some(by)));
    }
    /// Record what `by` did here that isn't typing (an approval, a
    /// follow-up to its agent): in its history as theirs, and in `log
    /// --who` as them taking a turn.
    pub fn note(&self, text: String, by: String) {
        let _ = self.tx.send(Cmd::Note(text, by));
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
    /// The agent the pane runs, whose screen to read (#145), or `None`;
    /// `unread`, one it runs whose screen isn't read here.
    pub fn watch_agent(&self, agent: Option<&'static Agent>, unread: Option<&'static Agent>) {
        let _ = self.tx.send(Cmd::Agent(agent, unread));
    }
    /// How its agent's screen reads now, rule by rule; `None` when no
    /// agent's screen is read (or the pane didn't answer).
    pub fn detection(&self) -> Option<Detection> {
        let (tx, rx) = bounded(1);
        self.tx.send(Cmd::Detection(tx)).ok()?;
        rx.recv_timeout(Duration::from_secs(5)).ok().flatten()
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
    /// Text encoded as a paste for the program here now: bracketed if it
    /// asked, with anything that could end the bracket made harmless (M70).
    #[cfg_attr(windows, allow(dead_code))] // Uploads are Unix only, as serving is.
    pub fn encode_paste(&self, text: String) -> Option<Vec<u8>> {
        let (tx, rx) = bounded(1);
        self.tx.send(Cmd::EncodePaste(text, tx)).ok()?;
        rx.recv_timeout(Duration::from_secs(5)).ok()
    }
    /// Note that input is about to be sent, before it's queued: a `wait`
    /// that follows a `send` then sees only what happens after it.
    /// The pane's running byte count and when it last printed (M23): one
    /// lock, nothing asked of the pane's thread.
    pub fn output_seen(&self) -> (u64, u64) {
        self.status.lock().map(|s| (s.end, s.last_output_ms)).unwrap_or_default()
    }
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
    /// The pane's own process's command line (a shell, or what `illogical
    /// run` started), quoted.
    pub fn own_command(&self) -> Option<String> {
        let args: Vec<String> = crate::procinfo::argv(self.pid()?)?.iter().map(|a| shell_quote(a)).collect();
        (!args.is_empty()).then(|| args.join(" "))
    }
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

/// The running command, the last one that ended, and the directory, as a
/// pane's index recorded them: what a daemon adopting the pane's program
/// knew before. A restore (a new program) ends whatever was running.
fn status_from(events: &[(u64, Event)]) -> (Option<CommandRec>, Option<CommandRec>, Option<String>) {
    let (mut current, mut last, mut dir) = (None::<CommandRec>, None, None);
    for (at, e) in events {
        match e {
            Event::Command { at_ms, text, cwd, by } => {
                current = Some(CommandRec {
                    text: text.clone(),
                    cwd: cwd.clone(),
                    start: *at,
                    started_ms: *at_ms,
                    by: by.clone(),
                    ..Default::default()
                });
            }
            Event::End { at_ms, exit } => {
                if let Some(mut rec) = current.take() {
                    (rec.end, rec.ended_ms, rec.exit) = (Some(*at), Some(*at_ms), *exit);
                    last = Some(rec);
                }
            }
            Event::Restore { .. } => current = None,
            Event::Cwd { path } => dir = Some(path.clone()),
            _ => {}
        }
    }
    (current, last, dir)
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

/// A terminal (and the program on it) kept while the daemon restarted:
/// its PTY master; on Windows, the pane host's pipes (`crate::host`).
#[cfg(unix)]
pub type Kept = std::os::fd::OwnedFd;
#[cfg(not(unix))]
#[derive(Debug)]
pub struct Kept {
    pub pipe: String,
}

/// SIGKILL's number, for exits that report a signal on every system.
const SIGKILL: i32 = 9;

/// How a pane begins.
pub enum Start {
    Now(Spawn),
    /// Take over a terminal (and the program on it) that outlived the
    /// previous daemon.
    Adopt(Kept),
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
    /// A `rerun` pane's command again after a restart, recorded like `Run`
    /// so the pane goes on knowing its command (and a second restart runs
    /// it again). `spawn` ends in a shell, whose first prompt ends the
    /// record (that prompt reports no exit code).
    Rerun {
        spawn: Spawn,
        text: String,
    },
    /// Show `banner` and wait: Enter runs `enter` (recorded as `Rerun` does
    /// when it has `text`), Escape runs `escape`.
    Wait {
        banner: String,
        enter: Spawn,
        text: Option<String>,
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
    /// systemd-run expands `$VAR` and `$$` in the command it's given
    /// (since systemd 254) unless told not to: the shell is to do that.
    pub no_expand: bool,
    pub fd_store: bool,
    /// No FD store, but keep panes anyway: each shim holds its terminal
    /// for the next daemon (`--keep-panes`, see [`crate::holder`]).
    pub hold: bool,
    /// Windows: what pane hosts run (`crate::host::exe`).
    #[cfg(windows)]
    pub host: PathBuf,
}

impl Launcher {
    /// What works here: scopes and the FD store need a systemd user service.
    /// Without one, `keep_panes` has the shims keep terminals instead.
    pub fn detect(keep_panes: bool) -> Self {
        let systemd = crate::sys::under_systemd();
        // "systemd 259 (259.5-0ubuntu3.4)"
        let version = std::process::Command::new("systemd-run")
            .arg("--version")
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| systemd_version(&String::from_utf8_lossy(&o.stdout)));
        Self {
            exe: std::env::current_exe().unwrap_or_else(|_| "illogicald".into()),
            scopes: systemd && version.is_some(),
            no_expand: version.flatten().is_some_and(|v| v >= 254),
            fd_store: systemd,
            hold: keep_panes && !systemd,
            #[cfg(windows)]
            host: std::env::current_exe().unwrap_or_else(|_| "illogicald.exe".into()),
        }
    }

    #[cfg(unix)]
    /// A command that runs this executable (the shim) for a pane or an
    /// agent: in its own scope `unit` when there are scopes. Without the
    /// daemon's service environment, which isn't the program's.
    pub fn command(&self, unit: &str) -> Command {
        let mut c = if self.scopes {
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--scope", "--quiet", "--collect"]);
            if self.no_expand {
                c.arg("--expand-environment=no");
            }
            c.arg(format!("--unit={unit}")).arg("--").arg(&self.exe);
            c
        } else {
            Command::new(&self.exe)
        };
        for k in crate::sys::SERVICE_ENV {
            c.env_remove(k);
        }
        c
    }
}

fn systemd_version(out: &str) -> Option<u32> {
    out.split_whitespace().nth(1)?.parse().ok()
}

fn fd_name(pane: PaneId) -> String {
    format!("pane-{pane}")
}

/// A running process on its own PTY.
#[cfg(unix)]
struct Process {
    pid: u32,
    master: File,
    writer: Sender<Vec<u8>>,
    /// The shim's record of it.
    record: PathBuf,
}

/// A running process on its own pseudoconsole, which its pane host owns
/// (Windows, M58: `crate::host`).
#[cfg(windows)]
struct Process {
    pid: u32,
    writer: Sender<Vec<u8>>,
    /// Resizes and close, to the host.
    ctl: Sender<HostCtl>,
}

#[cfg(windows)]
enum HostCtl {
    Resize(u16, u16),
    Close,
}

#[cfg(unix)]
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
        // Nor a stray copy of the slave beyond its stdio (the shim would
        // keep the terminal open after the program has gone).
        fcntl(&pty.slave, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        let stdio = |fd: &std::os::fd::OwnedFd| fd.try_clone().map(Stdio::from);

        let cwd = if spawn.cwd.is_dir() { spawn.cwd.as_path() } else { Path::new("/") };
        let _ = std::fs::remove_file(record);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let mut cmd = launch.command(&format!("illogical-pane-{pane}-{nanos}"));
        cmd.arg("_shim").arg("--record").arg(record);
        let hold = launch.hold.then(|| crate::holder::socket_for(record.parent().unwrap_or(Path::new("."))));
        if let Some(socket) = &hold {
            cmd.arg("--hold").arg(socket);
            // The master goes to the shim as fd 3 (without close-on-exec).
            use std::os::{fd::AsRawFd, unix::process::CommandExt};
            let master = pty.master.as_raw_fd();
            // SAFETY: only dup2/fcntl between fork and exec.
            unsafe {
                cmd.pre_exec(move || {
                    let held = crate::shim::HELD_FD;
                    if master == held {
                        let flags = libc::fcntl(held, libc::F_GETFD);
                        libc::fcntl(held, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    } else if libc::dup2(master, held) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        cmd.arg("--")
            .arg(&spawn.program)
            .args(&spawn.args)
            .current_dir(cwd)
            .envs(spawn.env.iter().map(|(k, v)| (k, v)))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("ILLOGICAL_PANE", pane.to_string())
            // The daemon's own setting, not the pane's.
            .env_remove("ILLOGICAL_KEEP_PANES")
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
            let exited = child.try_wait().ok().flatten().is_some();
            // It may have recorded the pid just before it ended.
            if let Some(p) = crate::shim::read_record(record).pid {
                break p;
            }
            if Instant::now() > deadline || exited {
                let _ = child.kill();
                return Err(std::io::Error::other("the pane shim did not start the program"));
            }
            thread::sleep(Duration::from_millis(5));
        };
        info!(pane, pid, program = %spawn.program, cwd = %cwd.display(), scope = launch.scopes, "started process");
        let master = File::from(pty.master);
        if let Some(socket) = &hold {
            // Our lease on the shim's copy: while we hold it, the shim keeps
            // the pane; we have the master already.
            if let Err(e) = crate::holder::borrow(pane, socket) {
                warn!(pane, error = %e, "the shim isn't keeping the terminal; the pane ends with the daemon");
            }
        }
        if launch.fd_store {
            crate::sys::remove_fd(&fd_name(pane));
            if !crate::sys::store_fd(&fd_name(pane), std::os::fd::AsRawFd::as_raw_fd(&master)) {
                warn!(pane, "couldn't keep the terminal in the FD store");
            }
        }
        Self::run(pid, Some(child), master, record.to_owned(), pane, events)
    }

    /// Take over a pane whose terminal and program outlived the previous
    /// daemon.
    fn adopt(master: Kept, record: &Path, pane: PaneId, events: Sender<Cmd>) -> std::io::Result<Self> {
        let r = crate::shim::read_record(record);
        let Some((pid, _)) = r.pid.filter(|_| crate::shim::alive(&r)) else {
            return Err(std::io::Error::other("the pane's program is gone"));
        };
        info!(pane, pid, "adopted process");
        Self::run(pid, None, File::from(master), record.to_owned(), pane, events)
    }

    /// `shim` is the shim (or systemd-run) when we started it: our child, to
    /// reap once the program has gone.
    fn run(
        pid: u32,
        shim: Option<Child>,
        master: File,
        record: PathBuf,
        pane: PaneId,
        events: Sender<Cmd>,
    ) -> std::io::Result<Self> {
        let mut reader = master.try_clone()?;
        let out = events.clone();
        // Dropped when the reader is done: the terminal hung up, and
        // everything the program wrote has gone out before it.
        let (read_done, drained) = bounded::<()>(0);
        thread::Builder::new().name(format!("pane{pane}-read")).spawn(move || {
            let _done = read_done;
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

        let waited = record.clone();
        thread::Builder::new().name(format!("pane{pane}-wait")).spawn(move || {
            let (code, signal) = wait_for_exit(pid, &waited);
            // The program's last output can still be in the terminal, or
            // read but not yet sent: the exit goes after it, or the command
            // it ends would end before its output (#60). The terminal hangs
            // up once the program and whatever it left holding it are gone;
            // don't wait long for those.
            let _ = drained.recv_timeout(DRAIN_AFTER_EXIT);
            let _ = events.send(Cmd::Exited { key: pid as u64, code, signal });
            // The shim ends right after its program (one thread fewer per
            // pane than a reaper of its own, M9).
            if let Some(mut shim) = shim {
                let _ = shim.wait();
            }
        })?;

        Ok(Self { pid, master, writer, record })
    }

    fn resize(&self, cols: u16, rows: u16) {
        let ws = Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ reads one Winsize from the pointer.
        let rc = unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(&self.master), libc::TIOCSWINSZ, &ws) };
        if rc < 0 {
            warn!(error = %std::io::Error::last_os_error(), "TIOCSWINSZ failed");
        }
    }

    /// Hang up the process group (the shell is a session leader), and kill
    /// it if it is still around a few seconds later. The shim does the same,
    /// so it happens even if this daemon doesn't live that long; the timer
    /// here is for shims from before that.
    fn hang_up(&self) {
        let pgid = self.pid as libc::pid_t;
        // SAFETY: plain signal sends.
        unsafe { libc::killpg(pgid, libc::SIGHUP) };
        crate::shim::close(&crate::shim::read_record(&self.record));
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(3));
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        });
    }
}

/// Windows (M58): the program runs under its pane's host, which outlives
/// this daemon; the daemon is the host's client, and a restarted one
/// adopts it (`crate::host::collect`).
#[cfg(windows)]
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
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let cwd = if spawn.cwd.is_dir() { spawn.cwd.clone() } else { crate::home() };
        let pipe = crate::host::pipe_name(record);
        let _ = std::fs::remove_file(record);
        let mut cmd = Command::new(&launch.host);
        cmd.arg("_host")
            .arg("--record")
            .arg(record)
            .arg("--pipe")
            .arg(&pipe)
            .args(["--cols", &cols.to_string(), "--rows", &rows.to_string()])
            .arg("--")
            .arg(&spawn.program)
            .args(&spawn.args)
            .current_dir(&cwd)
            .envs(spawn.env.iter().map(|(k, v)| (k, v)))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("ILLOGICAL_PANE", pane.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Out of this daemon's job, if it's in one (an ssh session, a
        // service), so the pane outlives it.
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        cmd.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB);
        let mut host = match cmd.spawn() {
            Ok(h) => h,
            Err(_) => {
                cmd.creation_flags(base);
                cmd.spawn()?
            }
        };
        // The host records the program's pid once it has started it.
        let deadline = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            if let Some((p, _)) = crate::shim::read_record(record).pid {
                break p;
            }
            if Instant::now() > deadline || host.try_wait().ok().flatten().is_some() {
                let _ = host.kill();
                return Err(std::io::Error::other(format!("the pane host didn't start {}", spawn.program)));
            }
            thread::sleep(Duration::from_millis(5));
        };
        info!(pane, pid, program = %spawn.program, cwd = %cwd.display(), conpty = crate::conpty::which(), "started process");
        Self::connect(&pipe, pid, pane, events)
    }

    /// Take over a pane whose host outlived the previous daemon.
    fn adopt(kept: Kept, record: &Path, pane: PaneId, events: Sender<Cmd>) -> std::io::Result<Self> {
        let r = crate::shim::read_record(record);
        let Some((pid, _)) = r.pid else { return Err(std::io::Error::other("the pane's record has no program")) };
        let p = Self::connect(&kept.pipe, pid, pane, events)?;
        info!(pane, pid, "adopted process");
        Ok(p)
    }

    /// Connect to the host's pipes: its output (and the program's exit) on
    /// one, input, resizes and close on the other.
    fn connect(pipe: &str, pid: u32, pane: PaneId, events: Sender<Cmd>) -> std::io::Result<Self> {
        let open = |suffix: &str, write: bool| -> std::io::Result<std::fs::File> {
            let name = format!("{pipe}-{suffix}");
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match std::fs::OpenOptions::new().read(!write).write(write).open(&name) {
                    Ok(f) => return Ok(f),
                    // Not made yet, or still serving the last daemon.
                    Err(e) if Instant::now() < deadline && matches!(e.raw_os_error(), Some(2 | 231)) => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => return Err(e),
                }
            }
        };
        let mut from_host = open("out", false)?;
        let mut to_host = open("in", true)?;

        let out = events.clone();
        thread::Builder::new().name(format!("pane{pane}-read")).spawn(move || {
            let mut code = None;
            while let Ok((kind, p)) = crate::host::read_frame(&mut from_host) {
                match kind {
                    crate::host::DATA => {
                        if out.send(Cmd::Output(p)).is_err() {
                            return;
                        }
                    }
                    crate::host::EXIT if p.len() == 4 => {
                        code = Some(i32::from_le_bytes([p[0], p[1], p[2], p[3]]));
                        break;
                    }
                    _ => {}
                }
            }
            // Its exit, after all its output; or the host is gone (killed).
            // A program its console closed under it (the session ending,
            // a shutdown) ends with STATUS_CONTROL_C_EXIT: that's Unix's
            // hangup, so the pane stays for its restore policy rather than
            // closing as a shell's own `exit` does.
            const STATUS_CONTROL_C_EXIT: i32 = 0xC000_013Au32 as i32;
            const SIGHUP: i32 = 1;
            let (code, signal) = match code {
                Some(STATUS_CONTROL_C_EXIT) => (Some(128 + SIGHUP), Some(SIGHUP)),
                Some(c) => (Some(c), None),
                None => (Some(128 + SIGKILL), Some(SIGKILL)),
            };
            let _ = out.send(Cmd::Exited { key: pid as u64, code, signal });
        })?;

        let (writer, inputs) = unbounded::<Vec<u8>>();
        let (ctl, ctls) = unbounded::<HostCtl>();
        thread::Builder::new().name(format!("pane{pane}-write")).spawn(move || {
            loop {
                let sent = crossbeam_channel::select! {
                    recv(inputs) -> d => match d {
                        Ok(d) => crate::host::write_frame(&mut to_host, crate::host::DATA, &d),
                        Err(_) => return,
                    },
                    recv(ctls) -> c => match c {
                        Ok(HostCtl::Resize(c, r)) => {
                            let p = [c.to_le_bytes(), r.to_le_bytes()].concat();
                            crate::host::write_frame(&mut to_host, crate::host::RESIZE, &p)
                        }
                        Ok(HostCtl::Close) => crate::host::write_frame(&mut to_host, crate::host::CLOSE, &[]),
                        Err(_) => return,
                    },
                };
                if sent.is_err() {
                    return;
                }
            }
        })?;
        Ok(Self { pid, writer, ctl })
    }

    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.ctl.send(HostCtl::Resize(cols, rows));
    }

    fn hang_up(&self) {
        let _ = self.ctl.send(HostCtl::Close);
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

#[cfg(unix)]
/// How long a program's exit waits for its terminal to hang up (to read the
/// last of its output first).
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(1);

/// Wait for a process that may not be our child (a restarted daemon is no
/// longer its parent), then read how it ended from the shim's record.
#[cfg(unix)]
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
    (None, Some(SIGKILL))
}

/// Recent output, addressed by absolute stream offset.
struct Ring {
    buf: VecDeque<u8>,
    /// Absolute offset of `buf[0]`.
    start: u64,
}

impl Ring {
    /// Empty, at `start`. Its one allocation is never touched past what's
    /// written, and never grows: pushes drain before they extend.
    fn new(start: u64) -> Self {
        Ring { buf: VecDeque::with_capacity(RING_BYTES), start }
    }
    fn end(&self) -> u64 {
        self.start + self.buf.len() as u64
    }
    fn push(&mut self, data: &[u8]) {
        // Only the tail of a chunk bigger than the ring can stay.
        let skip = data.len().saturating_sub(RING_BYTES);
        let data = &data[skip..];
        let excess = (self.buf.len() + data.len()).saturating_sub(RING_BYTES);
        self.buf.drain(..excess);
        self.buf.extend(data);
        self.start += (excess + skip) as u64;
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
    /// What `enter` runs, to record it as a command.
    text: Option<String>,
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
    /// The next prompt ends the command `Start::Rerun` recorded.
    prompt_ends: bool,
    ring: Ring,
    log: Option<PaneLog>,
    subs: HashMap<ClientId, Subscriber>,
    /// The subscribers that ack.
    flows: HashMap<ClientId, Flow>,
    /// The subscribers that speak the kitty keyboard protocol: while there
    /// are any, a program asking is told it's there.
    kitty: HashSet<ClientId>,
    closing: bool,
    shell: Spawn,
    launch: Launcher,
    /// The shim's record of the pane's program.
    record: PathBuf,
    hold: bool,
    notices: NoticeSink,
    events: Sender<Cmd>,
    /// For the local program's output and exit: bounded, so a program that
    /// prints faster than the pane can take it waits, and what clients ask
    /// never queues behind a flood (#52).
    program: Sender<Cmd>,
    pid: Arc<AtomicU32>,
    running: Arc<AtomicBool>,
    unsaved: u64,
    last_output: Instant,
    scanner: crate::osc::Scanner,
    /// The command line reported just before its command starts.
    pending_text: Option<String>,
    /// Who typed here last (M13); the next command is theirs.
    typed_by: Option<String>,
    status: Arc<std::sync::Mutex<Status>>,
    last_time_mark: Instant,
    /// When the once-a-second chores last ran.
    last_tick: Instant,
    /// The agent whose screen is read (#145).
    watch: Option<Watch>,
    /// An agent with rules running here whose screen isn't read.
    unread: Option<&'static Agent>,
}

/// Reading an agent's state off the pane's screen (#145).
struct Watch {
    agent: &'static Agent,
    debounce: Debounce,
    /// When the screen was last read, and whether output came since.
    looked: Instant,
    dirty: bool,
}

/// The screen is read at most this often while output flows, and this
/// often while a change waits to be confirmed.
const LOOK_EVERY: Duration = Duration::from_millis(100);

pub fn spawn_pane(setup: Setup) -> std::io::Result<PaneHandle> {
    let Setup { id, cols, rows, log, restore, start, shell, launch, hold, notices, host } = setup;
    let record = log.dir().join("process");
    let (tx, rx) = unbounded();
    let (program_tx, program_rx) = bounded(PROGRAM_QUEUE);
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
            prompt_ends: false,
            ring: Ring::new(log.end()),
            log: Some(log),
            subs: HashMap::new(),
            flows: HashMap::new(),
            kitty: HashSet::new(),
            closing: false,
            shell,
            launch,
            record,
            hold,
            notices,
            events: tx,
            program: program_tx,
            pid,
            running,
            unsaved: 0,
            last_output: Instant::now(),
            scanner: crate::osc::Scanner::new(),
            pending_text: None,
            typed_by: None,
            status,
            last_time_mark: Instant::now() - Duration::from_secs(60),
            last_tick: Instant::now(),
            watch: None,
            unread: None,
        };
        let adopting = matches!(start, Start::Adopt(_) | Start::Resume { .. });
        if restore && !adopting {
            st.restored_banner();
        }
        st.begin(start);
        run(st, rx, program_rx);
        // The pane's engine, ring and buffers are freed now: give the memory
        // back rather than keep it for panes that may never come (S9).
        crate::heap::trim();
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
#[cfg(unix)]
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

#[cfg(windows)]
fn local_time(ms: u64) -> String {
    use windows_sys::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime},
    };
    // FILETIME counts 100 ns from 1601.
    let t = (ms + 11_644_473_600_000) * 10_000;
    let ft = FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 };
    // SAFETY: plain conversions between valid structs.
    let (mut utc, mut tm): (SYSTEMTIME, SYSTEMTIME) = unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc);
        SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut tm);
    }
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    format!(
        "{} {:04}-{:02}-{:02} {:02}:{:02}",
        DAYS[(tm.wDayOfWeek % 7) as usize],
        tm.wYear,
        tm.wMonth,
        tm.wDay,
        tm.wHour,
        tm.wMinute
    )
}

fn run(mut st: State, rx: Receiver<Cmd>, program: Receiver<Cmd>) {
    // When the chores last ran. They run on time however busy the
    // channels are: commands arriving more often than a tick (a client
    // polling, acks) mustn't keep the agent's screen from being read or the
    // pane from going quiet.
    let mut chores = Instant::now();
    loop {
        let due = chores + st.tick();
        if Instant::now() >= due {
            chores = Instant::now();
            st.look_if_due(true);
            if st.last_tick.elapsed() >= Duration::from_secs(1) {
                st.last_tick = Instant::now();
                if st.unsaved > 0 && st.last_output.elapsed() >= CHECKPOINT_IDLE {
                    st.checkpoint();
                }
                st.check_quiet();
                st.save_exec();
            }
            continue;
        }
        // What clients ask goes first: an attach, an ack or a Ctrl-C must
        // not wait behind a flood of output.
        let cmd = match rx.try_recv() {
            Ok(cmd) => cmd,
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => crossbeam_channel::select! {
                recv(rx) -> cmd => match cmd {
                    Ok(cmd) => cmd,
                    Err(_) => return,
                },
                // `st` holds a sender, so this never disconnects.
                recv(program) -> cmd => match cmd {
                    Ok(cmd) => cmd,
                    Err(_) => continue,
                },
                default(due.saturating_duration_since(Instant::now())) => continue,
            },
        };
        match cmd {
            Cmd::Output(data) => {
                st.output(&data);
                if st.unsaved >= CHECKPOINT_BYTES {
                    st.checkpoint();
                }
            }
            Cmd::Input(data, by) => {
                if let Some(by) = by {
                    st.typed_by(by);
                }
                st.input(data)
            }
            Cmd::Note(text, by) => st.note(text, by),
            Cmd::Attach { sub, want } => st.attach(sub, want),
            Cmd::Ack { client, offset } => st.ack(client, offset),
            Cmd::Detach { client } => {
                st.subs.remove(&client);
                st.flows.remove(&client);
                st.forget_kitty(client);
            }
            Cmd::Resize { cols, rows } => st.resize(cols, rows),
            Cmd::Purge => st.purge(),
            Cmd::Agent(agent, unread) => {
                st.watch_agent(agent);
                st.unread = unread;
            }
            Cmd::Detection(reply) => {
                let _ = reply.send(st.detection());
            }
            Cmd::Checkpoint(done) => {
                st.checkpoint();
                let _ = done.send(());
            }
            Cmd::Capture { format, scope, reply } => {
                let _ = reply.send(st.capture(format, scope));
            }
            Cmd::EncodePaste(text, reply) => {
                let _ = reply.send(st.engine.encode_paste(&text));
            }
            Cmd::Close => {
                st.closing = true;
                st.subs.clear();
                st.flows.clear();
                st.kitty.clear();
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
                st.prompt_ends = false;
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
            Start::Run { spawn, text } => self.run(&spawn, text, false),
            Start::Rerun { spawn, text } => self.run(&spawn, text, true),
            Start::Adopt(master) => match Process::adopt(master, &self.record, id, self.program.clone()) {
                Ok(p) => {
                    self.pid.store(p.pid, Ordering::Relaxed);
                    self.running.store(true, Ordering::Relaxed);
                    self.process = Some(Backend::Local(p));
                    // The same program carries on: so does what the last
                    // daemon knew of it (#208: `illogical ls` lost the
                    // command of a pane `illogical run` started).
                    if let Some(log) = &self.log {
                        let (current, last, cwd) = status_from(&log.events());
                        let mut st = self.status.lock().unwrap();
                        (st.current, st.last) = (current, last);
                        st.cwd = st.cwd.take().or(cwd);
                    }
                }
                Err(e) => {
                    info!(pane = id, error = %e, "can't adopt; treating as ended");
                    self.exited(None, Some(SIGKILL));
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
            Start::Wait { banner, enter, text, escape } => {
                self.output(banner.as_bytes());
                self.waiting = Some(Waiting { enter, text, escape });
            }
        }
    }

    /// Start `spawn` recorded as the command `text`; with `then_shell`, the
    /// shell it ends in ends the record at its first prompt.
    fn run(&mut self, spawn: &Spawn, text: String, then_shell: bool) {
        if self.host.is_none() {
            self.status.lock().unwrap().cwd = Some(spawn.cwd.display().to_string());
        }
        let at = self.ring.end();
        self.signal(at, Signal::CommandLine { text });
        self.signal(at, Signal::CommandStart);
        self.prompt_ends = then_shell;
        self.start(spawn);
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
        self.waiting = Some(Waiting { enter: self.shell.clone(), text: None, escape: None });
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
        // Windows: a pseudoconsole starts from what it takes for a blank
        // screen and draws only what changes from there, so whatever the pane
        // shows already (restored output, a finished command) goes up into
        // scrollback first, and the two agree on the screen.
        #[cfg(windows)]
        if self.engine.content_rows() > 0 {
            let leave_alt = if self.engine.alt_screen() { "\x1b[?1049l" } else { "" };
            let up = format!("{leave_alt}\x1b[{rows};1H{}\x1b[H", "\r\n".repeat(rows as usize));
            self.output(up.as_bytes());
        }
        match Process::start(spawn, &self.launch, &self.record, cols, rows, self.id, self.program.clone()) {
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
                self.waiting = Some(Waiting { enter: self.shell.clone(), text: None, escape: None });
                self.notify(What::Exited { code, close: false });
            }
            Some(sig) => {
                let note =
                    format!("\r\n\x1b[0m\x1b[2m[process ended by signal {sig} · press Enter for a shell]\x1b[0m\r\n");
                self.output(note.as_bytes());
                self.waiting = Some(Waiting { enter: self.shell.clone(), text: None, escape: None });
                self.notify(What::Exited { code: Some(128 + sig), close: false });
            }
        }
    }

    /// Someone typed: when it's someone else than last time, the history
    /// says so at this offset (`illogical log --who`).
    fn typed_by(&mut self, by: String) {
        if self.typed_by.as_deref() == Some(by.as_str()) {
            return;
        }
        let at = self.ring.end();
        self.index(at, Event::Driver { at_ms: now_ms(), who: by.clone() });
        self.typed_by = Some(by);
    }

    /// What someone did here, as a finished entry in its history.
    fn note(&mut self, text: String, by: String) {
        self.typed_by(by.clone());
        let at = self.ring.end();
        let cwd = self.status.lock().unwrap().cwd.clone();
        self.index(at, Event::Command { at_ms: now_ms(), text: Some(text), cwd, by: Some(by) });
        self.index(at, Event::End { at_ms: now_ms(), exit: Some(0) });
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
        let (spawn, text) = if data.iter().any(|b| *b == b'\r' || *b == b'\n') {
            (Some(w.enter.clone()), w.text.clone())
        } else if data == [0x1b] {
            (w.escape.clone(), None)
        } else {
            (None, None)
        };
        if let Some(spawn) = spawn {
            self.waiting = None;
            self.output(b"\x1b[0m\r\n");
            match text {
                Some(text) => self.run(&spawn, text, true),
                None => self.start(&spawn),
            }
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
        let end = offset + data.len() as u64;
        let mut lagged = Vec::new();
        for (id, sub) in &self.subs {
            let flow = self.flows.get_mut(id);
            if flow.as_ref().is_some_and(|f| f.paused.is_some()) {
                continue;
            }
            // Drawing too far behind (or its queue is full of small frames):
            // hold back what's next until it acks, rather than piling it up
            // in the client or starting it over.
            let full = match flow.as_ref() {
                Some(f) if end.saturating_sub(f.acked) > ACK_WINDOW => true,
                _ => sub.data.try_send_output(frame.clone()).is_err(),
            };
            match flow {
                Some(f) if full => {
                    debug!(pane = self.id, client = id, at = offset, acked = f.acked, "client behind; holding back");
                    f.paused = Some(offset);
                }
                Some(f) => f.sent = end,
                None if full => lagged.push(*id),
                None => {}
            }
        }
        self.resync(lagged);

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
        // A title only changes in an OSC 0/1/2, so only look for one then.
        let title = data
            .windows(3)
            .any(|w| w[0] == 0x1b && w[1] == b']' && matches!(w[2], b'0' | b'2'))
            .then(|| self.engine.title())
            .map(|t| Some(t).filter(|t| !t.is_empty()));
        let busy_now = {
            let mut st = self.status.lock().unwrap();
            st.end = end;
            st.last_output_ms = now_ms();
            if let Some(t) = title {
                st.title = t;
            }
            st.modes = modes;
            let flip = !st.busy;
            st.busy = true;
            flip || was_quiet
        };
        if busy_now {
            self.notify(What::Busy(true));
        }
        if let Some(w) = &mut self.watch {
            w.dirty = true;
        }
        self.look_if_due(false);
    }

    /// How long the pane's thread waits for something to do before its
    /// chores: less while an agent's screen wants another look.
    fn tick(&self) -> Duration {
        match &self.watch {
            Some(w) if w.dirty || w.debounce.pending() => LOOK_EVERY,
            _ => Duration::from_secs(1),
        }
    }

    fn watch_agent(&mut self, agent: Option<&'static Agent>) {
        if self.watch.as_ref().map(|w| w.agent.id) == agent.map(|a| a.id) {
            return;
        }
        let now = Instant::now();
        self.watch = agent.map(|agent| Watch { agent, debounce: Debounce::new(now), looked: now, dirty: true });
    }

    fn detection(&self) -> Option<Detection> {
        if let (None, Some(a)) = (&self.watch, self.unread) {
            return Some(Detection {
                agent: a.id,
                name: a.name,
                shown: None,
                fired: None,
                title: self.engine.title(),
                rules: vec![],
                unread: true,
            });
        }
        let w = self.watch.as_ref()?;
        let (title, lines) = (self.engine.title(), self.engine.screen_lines());
        let rules = w.agent.explain(&title, &lines);
        Some(Detection {
            agent: w.agent.id,
            name: w.agent.name,
            shown: w.debounce.shown().map(AgentState::as_str),
            fired: rules.iter().find(|r| r.matched).map(|r| r.rule),
            title,
            rules: rules
                .into_iter()
                .map(|r| DetectionRule {
                    rule: r.rule,
                    state: r.state.as_str(),
                    priority: r.priority,
                    region: r.region,
                    text: r.text,
                    matched: r.matched,
                })
                .collect(),
            unread: false,
        })
    }

    /// Read the agent's state off the screen, if it's been long enough
    /// (`idle`: or there's no output to wait for), and say when it changes.
    fn look_if_due(&mut self, idle: bool) {
        let now = Instant::now();
        let Some(w) = &mut self.watch else { return };
        if !(w.dirty || w.debounce.pending()) || (!idle && now.duration_since(w.looked) < LOOK_EVERY) {
            return;
        }
        w.looked = now;
        w.dirty = false;
        let seen = w.agent.detect(&self.engine.title(), &self.engine.screen_lines());
        let state = seen.as_ref().map(|d| d.state);
        let Some(changed) = w.debounce.see(now, state) else { return };
        debug!(pane = self.id, agent = w.agent.id, ?changed, rule = seen.as_ref().map(|d| d.rule), "agent screen");
        let headline = seen.and_then(|d| d.headline);
        self.notify(What::Screen(changed, headline));
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
                if std::mem::take(&mut self.prompt_ends) && self.status.lock().unwrap().current.is_some() {
                    self.signal(at, Signal::CommandEnd { exit: None });
                }
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
                let by = self.typed_by.clone();
                self.index(at, Event::Command { at_ms: ms, text: text.clone(), cwd: cwd.clone(), by: by.clone() });
                let rec = CommandRec { text, cwd, start: at, started_ms: ms, by, ..Default::default() };
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
        self.resync(lagged);
    }

    fn forget_kitty(&mut self, client: ClientId) {
        if self.kitty.remove(&client) && self.kitty.is_empty() {
            self.engine.set_kitty_keyboard(false);
        }
    }

    /// Drop these subscribers and tell them to attach again.
    fn resync(&mut self, ids: Vec<ClientId>) {
        for id in ids {
            self.flows.remove(&id);
            self.forget_kitty(id);
            if let Some(sub) = self.subs.remove(&id) {
                debug!(pane = self.id, client = id, "client fell behind; resync");
                let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Resync { pane: self.id }));
            }
        }
    }

    /// A client that acks has drawn up to `offset`. If it was held back and
    /// is close enough now, send it what it missed from the log, or have it
    /// resync if the log no longer has that.
    fn ack(&mut self, client: ClientId, offset: u64) {
        let Some(f) = self.flows.get_mut(&client) else { return };
        f.acked = f.acked.max(offset.min(f.sent));
        let Some(from) = f.paused else { return };
        if f.sent.saturating_sub(f.acked) > ACK_RESUME {
            return;
        }
        let end = self.ring.end();
        let missed = (end.saturating_sub(from) <= MAX_REPLAY_BYTES).then(|| self.ring.since(from)).flatten();
        let Some(sub) = self.subs.get(&client) else { return };
        let sent = match missed {
            Some(bytes) if bytes.is_empty() => true,
            Some(bytes) => {
                let frame = Frame { kind: FrameKind::Output, pane: self.id, offset: from, data: bytes };
                sub.data.try_send(ToClient::Frame(frame.encode())).is_ok()
            }
            None => false,
        };
        if sent {
            debug!(pane = self.id, client, from, end, "client caught up; resuming");
            f.paused = None;
            f.sent = end;
        } else {
            self.resync(vec![client]);
        }
    }

    fn attach(&mut self, sub: Subscriber, want: Want) {
        if self.closing {
            return;
        }
        let Want { offset, history, zstd, floor, acks, kitty_keys } = want;
        let end = self.ring.end();
        let (cols, rows) = self.engine.size();
        // Nothing from before the floor: no replay from below it, and the
        // screen without scrollback instead of a full snapshot.
        let offset = offset.filter(|o| floor.is_none_or(|f| *o >= f));
        let replay = offset.filter(|o| end.saturating_sub(*o) <= MAX_REPLAY_BYTES).and_then(|o| self.ring.since(o));
        let frame = match replay {
            Some(bytes) if bytes.is_empty() => None,
            Some(bytes) => Some(Frame { kind: FrameKind::Output, pane: self.id, offset: offset.unwrap(), data: bytes }),
            None => {
                let history = if floor.is_some() { Some(0) } else { history.map(|h| h as usize) };
                let data = self.engine.snapshot_history(history);
                // Snapshots are mostly runs of the same few escape sequences
                // (S10: 3.9 MB to 183 KB gzipped).
                let packed =
                    (zstd && data.len() >= MIN_ZSTD_BYTES).then(|| zstd::encode_all(&data[..], 3).ok()).flatten();
                Some(match packed {
                    Some(data) => Frame { kind: FrameKind::SnapshotZstd, pane: self.id, offset: end, data },
                    None => Frame { kind: FrameKind::Snapshot, pane: self.id, offset: end, data },
                })
            }
        };
        debug!(pane = self.id, client = sub.client, ?offset, end, kind = ?frame.as_ref().map(|f| f.kind), "attach");
        // The size goes first so the client resizes before drawing.
        let size = ServerMsg::Size { pane: self.id, cols, rows };
        // What the client has once it draws this: from the replay's start, or
        // (after a snapshot, which isn't stream bytes) the end.
        let has = match &frame {
            Some(f) if f.kind == FrameKind::Output => f.offset,
            _ => end,
        };
        let queued = sub.data.try_send(ToClient::Msg(size)).is_ok()
            && frame.is_none_or(|f| sub.data.try_send(ToClient::Frame(f.encode())).is_ok());
        if !queued {
            self.flows.remove(&sub.client);
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Resync { pane: self.id }));
            return;
        }
        if acks {
            self.flows.insert(sub.client, Flow { sent: end, acked: has, paused: None });
        } else {
            self.flows.remove(&sub.client);
        }
        if kitty_keys {
            self.kitty.insert(sub.client);
            self.engine.set_kitty_keyboard(true);
        } else {
            self.forget_kitty(sub.client);
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
        // A held-back client would draw what it missed at the new size:
        // start it over instead.
        let held: Vec<ClientId> = self.flows.iter().filter(|(_, f)| f.paused.is_some()).map(|(id, _)| *id).collect();
        self.resync(held);
        let id = self.id;
        self.broadcast(|| ToClient::Msg(ServerMsg::Size { pane: id, cols, rows }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_versions() {
        assert_eq!(systemd_version("systemd 259 (259.5-0ubuntu3.4)\n+PAM +AUDIT"), Some(259));
        assert_eq!(systemd_version("systemd 252 (252.33-1~deb12u1)"), Some(252));
        assert_eq!(systemd_version(""), None);
    }

    #[test]
    fn ring_keeps_the_tail_and_addresses_by_offset() {
        let mut r = Ring::new(0);
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

    #[tokio::test]
    async fn client_queue_caps_live_output_in_bytes() {
        let (tx, mut rx) = client_queue();
        let chunk = vec![b'x'; 64 * 1024];
        let mut queued = 0;
        while tx.try_send_output(chunk.clone()).is_ok() {
            queued += 1;
        }
        // 8 MiB of 64 KiB reads, not 1024 of them.
        assert_eq!(queued, CLIENT_QUEUE_BYTES / chunk.len());
        // A snapshot still goes in (an attach queues them all at once), and
        // live output after it waits for the client to read.
        assert!(tx.try_send(ToClient::Frame(vec![0; 3 * 1024 * 1024])).is_ok());
        assert!(tx.try_send(ToClient::Msg(ServerMsg::Resync { pane: 1 })).is_ok());
        assert!(tx.try_send_output(b"more".to_vec()).is_err());
        for _ in 0..queued + 1 {
            assert!(matches!(rx.recv().await, Some(ToClient::Frame(_))));
        }
        assert!(matches!(rx.recv().await, Some(ToClient::Msg(_))));
        assert!(tx.try_send_output(chunk.clone()).is_ok());
        // Refused items don't count.
        assert_eq!(rx.bytes.load(Ordering::Relaxed), chunk.len());
    }

    #[test]
    fn an_adopted_panes_status_comes_from_its_index() {
        let cmd = |at_ms, text: &str| Event::Command { at_ms, text: Some(text.into()), cwd: None, by: None };
        let events = vec![
            (0, Event::Cwd { path: "/src".into() }),
            (10, cmd(1, "make")),
            (20, Event::End { at_ms: 2, exit: Some(0) }),
            (30, cmd(3, "sleep 300")),
        ];
        let (current, last, cwd) = status_from(&events);
        let current = current.unwrap();
        assert_eq!((current.text.as_deref(), current.start, current.started_ms), (Some("sleep 300"), 30, 3));
        let last = last.unwrap();
        assert_eq!((last.text.as_deref(), last.end, last.exit), (Some("make"), Some(20), Some(0)));
        assert_eq!(cwd.as_deref(), Some("/src"));
        // A restore started a new program: nothing of the old one runs.
        let mut restored = events.clone();
        restored.push((40, Event::Restore { at_ms: 4 }));
        assert!(status_from(&restored).0.is_none());
    }

    #[test]
    fn ring_never_grows_past_its_size() {
        let mut r = Ring::new(100);
        let cap = r.buf.capacity();
        assert!(cap >= RING_BYTES);
        // Pushes of every size, more than the ring holds in all, and one
        // bigger than the ring.
        for n in [1, 4096, 65536, 300_000, 1, RING_BYTES - 1, 7] {
            r.push(&vec![b'a'; n]);
            assert!(r.buf.len() <= RING_BYTES);
        }
        let total = 1 + 4096 + 65536 + 300_000 + 1 + (RING_BYTES - 1) + 7;
        assert_eq!(r.end(), 100 + total as u64);
        assert_eq!(r.buf.len(), RING_BYTES);
        let mut big = vec![b'b'; RING_BYTES + 10];
        big[10] = b'c';
        r.push(&big);
        assert_eq!(r.end(), 100 + (total + RING_BYTES + 10) as u64);
        assert_eq!(r.since(r.start).unwrap()[0], b'c');
        assert_eq!(r.buf.capacity(), cap);
    }

    #[test]
    fn quoting_for_rerun() {
        assert_eq!(shell_quote("make"), "make");
        assert_eq!(shell_quote("--flag=a/b"), "--flag=a/b");
        assert_eq!(shell_quote("two words"), "'two words'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
