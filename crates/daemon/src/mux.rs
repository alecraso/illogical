//! The multiplexer task: owns the layout (`illogical_core::Mux`), the panes,
//! the connected clients and what's saved to disk. Every client message, API
//! call and pane notice goes through here, so layout changes, pane starts and
//! stops, resizes, attention and saves happen in one order.

use std::{
    collections::{BTreeMap, HashMap},
    os::fd::OwnedFd,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use illogical_core::{Effect, Intent, Mux, Role};
use illogical_proto::{
    Action, Activity, AskRef, AskWhat, Attention, BlockType, ClientId, ClientMsg, CommandInfo, Delta, Driver, Event,
    EventKind, Machine, MachineId, MachineState, Owner, PaneId, PaneInfo, PaneOp, Policy, Presence, Reason, ReasonKind,
    ServerMsg, SessionId, State, TabId, TabView, WorkKind,
    api::{OpenRequest, PaneSummary, RunRequest},
    ask::{Ask, AskKind},
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    time::{Instant, sleep_until},
};
use tracing::{info, warn};

use crate::{
    acl::Principal,
    block::{Block, BlockCtx},
    osc::Signal,
    pane::{
        self, CommandRec, ExecRecord, Notice, NoticeSink, PaneHandle, Setup, Spawn, Start, Subscriber, ToClient, Want,
        What,
    },
    provider::Provider,
    push::Push,
    shellint::Integration,
    store::{LAYOUT_VERSION, PaneLog, PaneMeta, Saved, StateDir, now_ms},
    sys,
};

/// Layout changes are saved this long after the last one.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(250);
/// Working directories and foreground commands change without layout
/// changes; look at them this often.
const REFRESH: Duration = Duration::from_secs(5);
/// A command that ran at least this long, finishing unwatched, is "done".
const DONE_AFTER_MS: u64 = 5_000;
/// A command that ran at least this long and failed is "failed" (M24);
/// quicker ones you were typing at anyway.
const FAILED_AFTER_MS: u64 = 3_000;
/// Programs that wait for you quietly: one of these going quiet mid-command
/// means it probably needs input.
const AGENTS: &[&str] = &["claude", "codex", "aider", "gemini", "opencode", "goose", "amp"];
/// Changes a card depends on (attention, a question, who drives) reach
/// clients within this (M23); several in a row go together.
const URGENT: Duration = Duration::from_millis(40);
/// Everything else (activity, directories, commands) at most this often.
const TICK: Duration = Duration::from_secs(1);
/// A driver shows as typing this long after their last keystroke (#118).
const TYPING: Duration = Duration::from_secs(5);
/// A driver who hasn't typed in a pane this long stops driving it, so the
/// next to type drives (#118).
const DRIVER_LAPSE: Duration = Duration::from_secs(10 * 60);

/// [`DRIVER_LAPSE`], or `ILLOGICAL_DRIVER_LAPSE_MS` (for tests).
fn driver_lapse() -> Duration {
    std::env::var("ILLOGICAL_DRIVER_LAPSE_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(DRIVER_LAPSE)
}
/// What the OS says a pane runs is read again after this.
const PROC_FRESH: Duration = Duration::from_secs(1);
/// Pane fields a summary leaves out (M23): a client that needs them reads
/// the pane.
const NOT_IN_SUMMARIES: &[&str] = &["epoch", "policy", "integration"];

pub enum Cmd {
    Connect {
        sub: Subscriber,
    },
    Disconnect {
        client: ClientId,
    },
    Msg {
        client: ClientId,
        msg: ClientMsg,
    },
    /// Typing. `client`: whose, to check (M12); `None` when the caller
    /// already did (the API).
    Input {
        client: Option<ClientId>,
        pane: PaneId,
        data: Vec<u8>,
    },
    Api(Api),
    /// Grants changed (M12): show each client what it may see now, and hang
    /// up on anyone left with nothing.
    AclChanged,
    /// A reset machine's sprite is gone: start its panes again on a new one.
    MachineReset(MachineId),
    /// Checkpoint every pane and save the layout, then reply.
    Shutdown(oneshot::Sender<()>),
}

/// Requests from the HTTP API and the CLI.
pub enum Api {
    Panes(oneshot::Sender<Vec<PaneSummary>>),
    Run(RunRequest, oneshot::Sender<Result<PaneId, String>>),
    Pane(PaneId, oneshot::Sender<Option<PaneHandle>>),
    Attention(PaneId, Attention, Option<String>, oneshot::Sender<bool>),
    /// Every pane that wants you and why (M24), within what someone may
    /// read (`None`: the owner).
    AttentionList(Option<crate::acl::Principal>, oneshot::Sender<Vec<illogical_proto::api::AttentionItem>>),
    /// Why one pane wants you now, and whether it's a block (else a
    /// terminal).
    Reason(PaneId, oneshot::Sender<Option<(Reason, bool)>>),
    Close(PaneId, oneshot::Sender<bool>),
    /// Open a block of any type; for a guest (M14), their principal: then
    /// it must be an agent beside a pane they edit, and runs on a VM of
    /// theirs.
    Open(OpenRequest, Option<crate::acl::Principal>, oneshot::Sender<Result<PaneId, String>>),
    /// A non-terminal block, to describe or call.
    Block(PaneId, oneshot::Sender<Option<Arc<dyn Block>>>),
    Machines(oneshot::Sender<Vec<Machine>>),
    /// The machine a pane runs on.
    MachineOf(PaneId, oneshot::Sender<Option<Machine>>),
    /// Give a pane's machine to its tab.
    ShareMachine(PaneId, oneshot::Sender<Result<(), String>>),
    /// Delete and recreate a machine; its panes restart by policy.
    ResetMachine(MachineId, oneshot::Sender<Result<(), String>>),
    /// A question asked in a terminal (`illogical ask`, from Claude Code's
    /// hook), or on a block (M35: a studio box's agent, through its
    /// follower): shown beside it until answered. The reply carries a
    /// token (for withdrawing exactly this one) and where the answer will
    /// come, with who gave it.
    Ask(PaneId, Box<Ask>, oneshot::Sender<Result<(u64, oneshot::Receiver<Replied>), String>>),
    /// Whether the daemon holds a question open on a pane or block (one
    /// raised through `Ask`), so its answer goes there and not to the
    /// block's own methods.
    Holds(PaneId, oneshot::Sender<bool>),
    /// A client answered a terminal's question (`id`: which; `None`: the
    /// one open). Replies with the question, or why not.
    AskReply(PaneId, Option<String>, AskReply, Option<Driver>, oneshot::Sender<Result<Ask, String>>),
    /// Claude Code's hooks in a terminal (M29: `illogical hook`): a
    /// `PreToolUse` names the tool call a permission card is for; it and
    /// `PostToolUse`, `Stop` and `UserPromptSubmit` close a card the
    /// terminal answered first.
    Hook(PaneId, serde_json::Value),
    /// `illogical inbox` (Claude Code's background `Stop` hook): wait for a
    /// follow-up. One waiter per pane; a newer one replaces it. The reply
    /// carries a token (to drop exactly this one) and where it will come.
    Inbox(PaneId, serde_json::Value, oneshot::Sender<Result<(u64, oneshot::Receiver<InboxReply>), String>>),
    /// The waiter went away (Claude Code exited).
    InboxGone(PaneId, u64),
    /// A follow-up for Claude Code in a terminal, from `by`: delivered at
    /// once if it waits, else queued for when it does. `Ok(true)`: it
    /// went straight in.
    FollowUp(PaneId, String, Driver, oneshot::Sender<Result<bool, String>>),
    /// Someone answered a block's approval or question (M29), for its
    /// card, the pane's history and the audit log: `(id, how, headline)`.
    Answered(PaneId, Driver, String, String, String),
    /// What to call someone (M13), for attribution.
    Who(crate::acl::Principal, oneshot::Sender<Driver>),
    /// The asker gave up (Claude Code interrupted it): close the card.
    /// With a token, only if it's still that registration's.
    AskWithdraw(PaneId, Option<String>, Option<u64>),
    /// Someone's role on the session a pane or block is in (M12), and for
    /// a "from now" share where its output may start for them (M13).
    RoleOn(crate::acl::Principal, PaneId, oneshot::Sender<Option<(Role, Option<u64>)>>),
    /// Whether a guest may type in a pane on this machine (M14).
    MayDrive(crate::acl::Principal, PaneId, oneshot::Sender<Result<(), String>>),
    /// Where each pane of a session's output ends now (a "from now" share
    /// starts there).
    SessionEnds(SessionId, oneshot::Sender<Option<BTreeMap<PaneId, u64>>>),
    /// An MCP client started this pane or block (M16): shown on it, and
    /// what lets an agent block's token drive it.
    StartedBy(PaneId, illogical_proto::StartedBy),
    /// Typing by an MCP client (M16): as theirs, in the pane's history.
    InputBy(PaneId, Vec<u8>, String),
    /// An editor joined the swarm (M28): it gets an id of its own.
    EditorJoin(Arc<crate::editor::link::Link>, oneshot::Sender<PaneId>),
    /// ...and left.
    EditorLeave(PaneId),
    /// What Claude Code's IDE connections did (M28).
    Ide(crate::ide::Event),
    /// Someone accepted (true) or rejected an edit waiting as a diff, by
    /// its id or whichever waits; accepting may change it first.
    DiffAnswer(PaneId, Option<String>, bool, Option<String>, Driver, oneshot::Sender<Result<(), String>>),
    /// Every editor in the swarm (M28), as `who` may see them.
    Editors(Option<crate::acl::Principal>, oneshot::Sender<Vec<serde_json::Value>>),
    /// The IDE connections of Claude Code in a pane (M28).
    IdeConns(PaneId, oneshot::Sender<Vec<u64>>),
    /// The edit a pane's diff card shows: before and after.
    DiffOf(PaneId, oneshot::Sender<Option<(illogical_proto::DiffInfo, String, String)>>),
    /// A pane in a tab of its own (M37: an issue, before its agent joins
    /// it): taken out of the tab it shares, next to it, and named if the
    /// tab has no name.
    OwnTab(PaneId, Option<String>, oneshot::Sender<Result<(), String>>),
    /// Home and the environment an agent block gets (#111: whether its
    /// adapter can start).
    AgentEnv(oneshot::Sender<(PathBuf, Vec<(String, String)>)>),
}

/// An edit Claude Code proposes through its IDE connection (M28).
struct PendingDiff {
    /// Its pane, once its Claude Code's process is found in one.
    pane: Option<PaneId>,
    /// The relay's connection, and the call's id there.
    conn: u64,
    call: serde_json::Value,
    tab: String,
    /// The file as it is, and as it would be.
    old: String,
    new: String,
    info: illogical_proto::DiffInfo,
}

/// How much of a file a diff card keeps.
const MAX_DIFF_FILE: u64 = 4 << 20;
/// ...and of its diff.
const MAX_DIFF_TEXT: usize = 16 << 10;

/// What a terminal's question got.
#[derive(Debug, Clone, PartialEq)]
pub enum AskReply {
    /// The card's fields (as AskUserQuestion's form names them).
    Answer(serde_json::Value),
    /// Skipped.
    Decline,
    /// "Answer in terminal": let the program show its own picker.
    Terminal,
    /// It went away (its pane closed, or a newer one replaced it).
    Withdrawn,
    /// A permission card (M29) allowed it; with one of Claude Code's
    /// suggestions to keep as a rule.
    Allow { always: Option<serde_json::Value> },
    /// ...or denied it, saying why.
    Deny { message: String },
}

/// What a question's asker gets: the answer, and who gave it (`None`:
/// nobody did; it was withdrawn).
pub type Replied = (AskReply, Option<Driver>);

/// What a follow-up waiter (`illogical inbox`) gets.
#[derive(Debug, Clone, PartialEq)]
pub enum InboxReply {
    FollowUp {
        text: String,
        by: Driver,
    },
    /// A newer waiter took its place.
    Replaced,
}

/// A follow-up waiter.
struct Waiter {
    token: u64,
    reply: oneshot::Sender<InboxReply>,
}

/// How many follow-ups wait for an agent that isn't listening yet.
const MAX_QUEUED: usize = 8;

/// A question open in a terminal, or raised on a block (M35).
struct TermAsk {
    ask: Ask,
    token: u64,
    reply: oneshot::Sender<Replied>,
}

#[derive(Clone)]
pub struct MuxHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    events: broadcast::Sender<Event>,
    pub store: StateDir,
    pub provider: Option<Arc<dyn Provider>>,
    /// Tags execs on machines (`ILLOGICAL_EXEC`; see `mux::exec_tag`).
    pub daemon_id: String,
    /// This host's files, as the `fs` methods may read them.
    pub fs: Arc<crate::fs::Scope>,
    /// Claude Code's IDE (M28), if on.
    pub ide: Option<Arc<crate::ide::Ide>>,
    /// The user's shell environment, here and on machines (#74).
    pub shell_env: Arc<crate::shellenv::ShellEnv>,
}

impl MuxHandle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Shutdown(tx));
        let _ = rx.await;
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub async fn api<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Api) -> Option<T> {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Api(make(tx)));
        rx.await.ok()
    }
}

/// How panes run their shell.
#[derive(Clone, Debug)]
pub struct Config {
    /// Who else may reach which sessions (M12).
    pub acl: Arc<crate::acl::Acl>,
    /// Illogical control: notifications through it go to people's
    /// devices (M21).
    pub control: Arc<crate::control::Control>,
    /// A hosted sandbox (M20): when its last session closes, it's done.
    pub sandbox_of_control: bool,
    /// What to call the owner to others (M13): their login, else "owner".
    pub owner_name: String,
    /// The owner's picture, if the tailnet gave one.
    pub owner_pic: Option<String>,
    /// How many VMs each guest may have at once (M14).
    pub guest_machines: usize,
    pub shell: String,
    /// Arguments for an interactive shell, e.g. `["-l"]`.
    pub shell_args: Vec<String>,
    pub home: PathBuf,
    /// Merge the systemd user manager's environment into new panes.
    pub manager_env: bool,
    pub launch: pane::Launcher,
    pub integration: Option<Integration>,
    /// The CLI's socket, for `ILLOGICAL_SOCK` in panes.
    pub socket: PathBuf,
    /// Where VM panes get their machines; `None` if not set up.
    pub provider: Option<Arc<dyn Provider>>,
    /// Names this daemon's sprites, so a crash sweep only touches ours.
    pub daemon_id: String,
    /// Where agents in VMs get their credentials from.
    pub secrets: crate::block::Secrets,
    /// Secrets the `fs` methods never serve (the provider's token, agents'
    /// credentials); the state directory is added to these.
    pub private: Vec<PathBuf>,
    /// Where agent blocks reach MCP (M16); `None`: they don't.
    pub mcp: Option<crate::mcp::Link>,
    /// illogicald as Claude Code's IDE (M28); `None`: off.
    pub ide: Option<Arc<crate::ide::Ide>>,
}

impl Config {
    fn env(&self, pane: PaneId) -> Vec<(String, String)> {
        let mut env = if self.manager_env { sys::manager_env() } else { vec![] };
        // The CLI is installed next to the daemon; panes find it on PATH and
        // know which pane (and which daemon) they are.
        let base = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        if let Some(bin) = self.launch.exe.parent() {
            let bin = bin.display().to_string();
            let path = if base.split(':').any(|p| p == bin) { base } else { format!("{bin}:{base}") };
            env.retain(|(k, _)| k != "PATH");
            env.push(("PATH".into(), path));
        }
        env.push(("ILLOGICAL_PANE".into(), pane.to_string()));
        env.push(("ILLOGICAL_SOCK".into(), self.socket.display().to_string()));
        // Claude Code in a pane finds us as its IDE (M28), and only us.
        if let Some(ide) = &self.ide {
            env.retain(|(k, _)| k != "CLAUDE_CODE_SSE_PORT");
            env.push(("CLAUDE_CODE_SSE_PORT".into(), ide.port.to_string()));
        }
        env
    }

    /// An interactive shell, with integration unless it's off for the pane.
    fn shell(&self, pane: PaneId, cwd: PathBuf, integrate: bool) -> Spawn {
        let mut s = Spawn { program: self.shell.clone(), args: self.shell_args.clone(), cwd, env: self.env(pane) };
        if integrate && let Some(i) = &self.integration {
            i.apply(&mut s);
        }
        s
    }

    /// Run `command`, then carry on with an interactive shell in the pane.
    fn run_then_shell(&self, pane: PaneId, cwd: PathBuf, command: &str, integrate: bool) -> Spawn {
        let shell = self.shell(pane, cwd, integrate);
        let then = std::iter::once(shell.program.as_str()).chain(shell.args.iter().map(String::as_str));
        let mut args: Vec<String> = self.shell_args.iter().filter(|a| *a != "--posix").cloned().collect();
        args.extend(["-c".into(), format!("{command}; exec {}", then.collect::<Vec<_>>().join(" "))]);
        Spawn { args, ..shell }
    }

    /// Run `command` by itself (`illogical run`): the pane holds when it
    /// ends, so its output and exit code can still be read.
    fn run_only(&self, pane: PaneId, cwd: PathBuf, command: &str) -> Spawn {
        let mut args = self.shell_args.clone();
        args.extend(["-c".into(), command.into()]);
        Spawn { program: self.shell.clone(), args, cwd, env: self.env(pane) }
    }

    /// What a restored pane does, by its policy.
    fn restore(&self, pane: PaneId, meta: &PaneMeta) -> Start {
        let cwd = meta.cwd.as_ref().map(PathBuf::from).unwrap_or_else(|| self.home.clone());
        let on = meta.integration.unwrap_or(true);
        let shell = match meta.host {
            Some(_) => self.guest_shell(pane, on, None),
            None => self.shell(pane, cwd.clone(), on),
        };
        let then = |command: &str| match meta.host {
            Some(_) => self.guest_run_then_shell(pane, command, on),
            None => self.run_then_shell(pane, cwd.clone(), command, on),
        };
        let note = |s: &str| format!("\x1b[2m[{s}]\x1b[0m\r\n");
        match (&meta.policy, &meta.command) {
            (Policy::None, _) => Start::Wait { banner: note("press Enter for a shell"), enter: shell, escape: None },
            (Policy::Rerun { confirm: true }, Some(cmd)) => Start::Wait {
                banner: note(&format!("press Enter to re-run: {cmd}  ·  Esc for a shell")),
                enter: then(cmd),
                escape: Some(shell),
            },
            (Policy::Rerun { confirm: false }, Some(cmd)) => Start::Now(then(cmd)),
            (Policy::Hook { command }, _) => Start::Now(then(command)),
            (Policy::Shell | Policy::Rerun { .. }, _) => Start::Now(shell),
        }
    }

    /// The environment of a pane's program on a machine: what the terminal
    /// is, and a tag `process` finds its shell by (machines are shared, so
    /// it names the pane). None of this host's.
    fn guest_env(&self, pane: PaneId) -> Vec<(String, String)> {
        vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("ILLOGICAL_EXEC".into(), exec_tag(&self.daemon_id, pane)),
        ]
    }

    /// The sprite names this daemon's machines get.
    fn sprite_prefix(&self) -> String {
        format!("illogical-eph-{}-", self.daemon_id)
    }

    /// A login shell on a machine, in its home directory unless given one
    /// of its own directories.
    fn guest_shell(&self, pane: PaneId, integrate: bool, cwd: Option<PathBuf>) -> Spawn {
        let (program, args, env) = ("bash".into(), vec!["-l".into()], self.guest_env(pane));
        let mut s = Spawn { program, args, cwd: cwd.unwrap_or_default(), env };
        if integrate && self.integration.is_some() {
            crate::shellint::apply_guest(&mut s);
        }
        s
    }

    fn guest_run(&self, pane: PaneId, cwd: Option<PathBuf>, command: &str) -> Spawn {
        Spawn {
            program: "bash".into(),
            args: vec!["-lc".into(), command.into()],
            cwd: cwd.unwrap_or_default(),
            env: self.guest_env(pane),
        }
    }

    fn guest_run_then_shell(&self, pane: PaneId, command: &str, integrate: bool) -> Spawn {
        let shell = self.guest_shell(pane, integrate, None);
        let then = std::iter::once(shell.program.as_str()).chain(shell.args.iter().map(String::as_str));
        let args = vec!["-lc".into(), format!("{command}; exec {}", then.collect::<Vec<_>>().join(" "))];
        Spawn { args, ..shell }
    }
}

/// What was last written to layout.json, to skip writing it unchanged.
type SavedParts = (Mux, BTreeMap<PaneId, PaneMeta>, BTreeMap<MachineId, Machine>);

/// Where `run --split %N --join` puts the new pane.
enum Join {
    /// This host.
    Here,
    /// The split pane's tab's machine.
    TabMachine,
    /// A sandbox the split pane has a shell on: borrowed again.
    Borrow(String),
}

/// A random number for generated names.
fn seed() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(now_ms())
}

/// Tags a pane's execs on machines (`ILLOGICAL_EXEC`).
pub fn exec_tag(daemon_id: &str, pane: PaneId) -> String {
    format!("{daemon_id}-p{pane}")
}

struct Daemon {
    mux: Mux,
    panes: HashMap<PaneId, PaneHandle>,
    meta: HashMap<PaneId, PaneMeta>,
    attention: HashMap<PaneId, Attention>,
    /// Why each pane wants you (M24), as recorded when it started to.
    reasons: HashMap<PaneId, Reason>,
    /// Panes whose agent may be said to have gone quiet: once per turn,
    /// armed by input or a new command, so an idle agent redrawing itself
    /// (a resize, its status line) doesn't want you again and again.
    quiet_armed: std::collections::HashSet<PaneId>,
    clients: HashMap<ClientId, Subscriber>,
    /// The pane each client's focused window is looking at.
    focus: HashMap<ClientId, PaneId>,
    /// When each client was last told it can't type somewhere (once is
    /// enough while it keeps trying).
    refused: HashMap<ClientId, Instant>,
    /// The tab each client shows (M13 presence).
    viewing: HashMap<ClientId, TabId>,
    /// ...and the one pane in it, on a phone.
    zoomed: HashMap<ClientId, PaneId>,
    /// Blocks some client draws now (M11), as they were last told.
    drawn: std::collections::HashSet<PaneId>,
    /// This host's files, as `/api/fs` serves them.
    fs: Arc<crate::fs::Scope>,
    shell_env: Arc<crate::shellenv::ShellEnv>,
    /// Who drives each pane (M13), and panes in pair mode.
    drivers: HashMap<PaneId, Driver>,
    pair: std::collections::HashSet<PaneId>,
    /// When each driver last typed in their pane, or took it (#118).
    drove: HashMap<PaneId, Instant>,
    /// Panes whose driver typed in the last `TYPING`.
    typing: std::collections::HashSet<PaneId>,
    /// How long an idle driver keeps a pane.
    lapse: Duration,
    /// Guests trusted to drive a pane on this machine (M14), until when.
    trust: HashMap<(PaneId, String), u64>,
    /// Size each pane was last given.
    sizes: BTreeMap<PaneId, (u16, u16)>,
    config: Config,
    store: StateDir,
    notices: NoticeSink,
    events: broadcast::Sender<Event>,
    push: Option<Push>,
    /// Non-terminal blocks (terminals are in `panes`).
    blocks: HashMap<PaneId, Arc<dyn Block>>,
    /// The ids of both, for blocks to tell our panes from another daemon's
    /// (#77).
    ids: crate::block::PaneIds,
    /// Questions open in terminals, one per pane (M6c).
    asks: HashMap<PaneId, TermAsk>,
    /// Who answered each pane's last card (M29).
    answered: HashMap<PaneId, illogical_proto::ask::Answered>,
    /// Claude Code's recent `PreToolUse` hook inputs per pane, to match a
    /// permission card to its tool call.
    pre: HashMap<PaneId, std::collections::VecDeque<serde_json::Value>>,
    /// Follow-up waiters (`illogical inbox`), and follow-ups waiting for one.
    inbox: HashMap<PaneId, Waiter>,
    queued: HashMap<PaneId, std::collections::VecDeque<(String, Driver)>>,
    next_ask: u64,
    /// The next block an intent spawns is this type, with this config,
    /// instead of a terminal.
    next_block: Option<(BlockType, serde_json::Value)>,
    /// Why the last block failed to start, for `open_block` to report.
    last_block_error: Option<String>,
    /// The next pane an intent spawns runs this instead of a shell.
    next_spawn: Option<(Spawn, Option<String>)>,
    /// ...and runs it on this machine.
    next_host: Option<MachineId>,
    /// ...which belongs to the new pane's tab, not the pane.
    next_owner_tab: bool,
    /// ...and starts its shell here (a directory on its host).
    next_cwd: Option<PathBuf>,
    /// To ourselves, for work finished in the background.
    tx: mpsc::UnboundedSender<Cmd>,
    machines: BTreeMap<MachineId, Machine>,
    next_machine: MachineId,
    save_due: Option<Instant>,
    last_saved: Option<SavedParts>,
    shutting_down: bool,
    // ---- what clients were last sent (M23)
    /// Panes whose details changed since the last flush.
    dirty: Dirty,
    /// Send everyone a whole `State` at the next flush (grants changed).
    full: bool,
    /// When the next flush is due, if a change is waiting.
    flush_due: Option<Instant>,
    /// Per client: what it has, to send it only what changed.
    sent: HashMap<ClientId, Sent>,
    /// Clients that only want summaries (the swarm, the fleet).
    summary: std::collections::HashSet<ClientId>,
    /// Each pane's byte count at the last tick, and its activity.
    activity: HashMap<PaneId, (u64, Activity)>,
    last_tick: Instant,
    /// What the OS said each pane runs, and when it was read.
    procs: std::cell::RefCell<HashMap<PaneId, ProcSeen>>,
    /// Who follows each editor (M28).
    follows: HashMap<PaneId, std::collections::HashSet<ClientId>>,
    /// Edits waiting as diffs (M28), and the pane each of Claude Code's
    /// IDE connections runs in.
    diffs: Vec<PendingDiff>,
    ide_conns: HashMap<u64, (Option<u32>, Option<PaneId>)>,
}

/// Which panes changed.
#[derive(Default)]
enum Dirty {
    #[default]
    Clean,
    Panes(std::collections::HashSet<PaneId>),
    All,
}

/// A pane as one client has it.
type PaneJson = serde_json::Map<String, serde_json::Value>;
/// The changed panes as one person sees them (`None`: not any more).
type PaneView = Vec<(PaneId, Option<PaneJson>)>;

/// What one client was last sent.
#[derive(Default)]
struct Sent {
    rev: u64,
    panes: HashMap<PaneId, PaneJson>,
    machines: Vec<Machine>,
    presence: Vec<Presence>,
}

/// A pane's working directory and foreground command, as the OS showed
/// them.
#[derive(Clone)]
struct ProcSeen {
    at: Instant,
    cwd: Option<String>,
    command: Option<String>,
    /// What it's busy with, from `command`, else the pane's own process
    /// (what `illogical run` started has no shell above it).
    work: WorkKind,
}

/// `kept`: pane terminals systemd kept for us across a restart, by FD name.
pub fn start(config: Config, store: StateDir, kept: HashMap<String, OwnedFd>, push: Option<Push>) -> MuxHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (notices, notices_rx) = mpsc::unbounded_channel();
    let (events, _) = broadcast::channel(1024);
    let mut private = config.private.clone();
    private.push(store.root().to_path_buf());
    let fs = Arc::new(crate::fs::Scope::new(config.home.clone(), private));
    // The user's shell environment (#74), resolved in the background.
    let shell_env = crate::shellenv::ShellEnv::new(
        config.shell.clone(),
        config.shell_args.iter().filter(|a| a.starts_with("--") && *a != "--login").cloned().collect(),
        config.home.clone(),
        config
            .env(0)
            .into_iter()
            .filter(|(k, _)| !k.starts_with("ILLOGICAL_") && k != "CLAUDE_CODE_SSE_PORT")
            .collect(),
        crate::shellenv::TIMEOUT,
    );
    shell_env.start();
    let mut d = Daemon {
        mux: Mux::new(),
        panes: HashMap::new(),
        meta: HashMap::new(),
        attention: HashMap::new(),
        reasons: HashMap::new(),
        quiet_armed: Default::default(),
        clients: HashMap::new(),
        focus: HashMap::new(),
        refused: HashMap::new(),
        viewing: HashMap::new(),
        zoomed: HashMap::new(),
        drawn: Default::default(),
        fs: fs.clone(),
        shell_env: shell_env.clone(),
        drivers: HashMap::new(),
        pair: Default::default(),
        drove: HashMap::new(),
        typing: Default::default(),
        lapse: driver_lapse(),
        trust: HashMap::new(),
        sizes: BTreeMap::new(),
        config,
        store: store.clone(),
        notices,
        events: events.clone(),
        push,
        blocks: HashMap::new(),
        ids: Default::default(),
        asks: HashMap::new(),
        answered: HashMap::new(),
        pre: HashMap::new(),
        inbox: HashMap::new(),
        queued: HashMap::new(),
        next_ask: 1,
        next_block: None,
        last_block_error: None,
        next_spawn: None,
        next_host: None,
        next_owner_tab: false,
        next_cwd: None,
        tx: tx.clone(),
        machines: BTreeMap::new(),
        next_machine: 1,
        save_due: None,
        last_saved: None,
        shutting_down: false,
        dirty: Dirty::All,
        full: false,
        flush_due: None,
        sent: HashMap::new(),
        summary: Default::default(),
        activity: HashMap::new(),
        last_tick: Instant::now(),
        procs: Default::default(),
        follows: HashMap::new(),
        diffs: Vec::new(),
        ide_conns: HashMap::new(),
    };
    if !d.restore(kept) {
        // Something to attach to on first start.
        if let Err(e) = d.intent(None, Intent::NewSession { name: None, from_pane: None }) {
            warn!(error = %e, "could not create the first session");
        }
    }
    d.sweep_machines();
    let (provider, daemon_id, ide) = (d.config.provider.clone(), d.config.daemon_id.clone(), d.config.ide.clone());
    tokio::spawn(d.run(rx, notices_rx));
    MuxHandle { tx, events, store, provider, daemon_id, fs, ide, shell_env }
}

/// A reason with nothing but its headline.
fn plain_reason(kind: ReasonKind, headline: &str) -> Reason {
    Reason {
        kind,
        since_ms: now_ms(),
        headline: headline.to_owned(),
        command: None,
        exit: None,
        duration_ms: None,
        bundle: None,
        ask: None,
        gate: None,
        actions: vec![Action::Dismiss],
    }
}

/// Claude Code's session and subagent, from a hook's input: `session/agent`.
fn session_key(hook: &serde_json::Value) -> String {
    format!("{}/{}", hook["session_id"].as_str().unwrap_or(""), hook["agent_id"].as_str().unwrap_or(""))
}

/// A hook input about the same tool call as a permission card: same
/// session and subagent, tool and input.
fn same_call(hook: &serde_json::Value, ask: &Ask) -> bool {
    ask.session.as_deref() == Some(session_key(hook).as_str())
        && ask.tool.as_deref() == hook["tool_name"].as_str()
        && ask.input.as_ref() == Some(&hook["tool_input"])
}

/// What a notification about a reason is titled.
fn push_title(state: Attention, reason: Option<&Reason>) -> &'static str {
    match reason.map(|r| r.kind) {
        Some(ReasonKind::Failed) => "Failed",
        Some(ReasonKind::Exited) => "Exited",
        Some(ReasonKind::Done) => "Done",
        Some(ReasonKind::Ask | ReasonKind::Input) => "Needs you",
        Some(ReasonKind::Paused) => "Paused",
        Some(ReasonKind::Errors) => "Errors",
        Some(ReasonKind::Conflict) => "Merge conflict",
        Some(ReasonKind::Diff) => "Wants to edit",
        Some(ReasonKind::Gate) => "Waits at a gate",
        None if state == Attention::Done => "Done",
        None => "Needs you",
    }
}

/// "Claude Code wants to edit src/main.rs (+3 −1)".
fn diff_headline(d: &illogical_proto::DiffInfo) -> String {
    let verb = if d.new { "create" } else { "edit" };
    format!("Claude Code wants to {verb} {} (+{} −{})", d.file, d.added, d.removed)
}

/// An `openDiff` call as a diff card: the file as it is now, and as it
/// would be.
fn new_diff(
    pane: Option<PaneId>,
    conn: u64,
    call: serde_json::Value,
    args: &serde_json::Value,
    cwd: Option<&str>,
) -> PendingDiff {
    use std::io::Read;
    let path = args["new_file_path"].as_str().or(args["old_file_path"].as_str()).unwrap_or("").to_owned();
    let old_path = args["old_file_path"].as_str().unwrap_or(&path).to_owned();
    let mut old = String::new();
    let exists = std::fs::File::open(&old_path).and_then(|f| f.take(MAX_DIFF_FILE).read_to_string(&mut old)).is_ok();
    let new: String = args["new_file_contents"].as_str().unwrap_or("").to_owned();
    let (added, removed, text) = crate::ide::diff::unified(&old, &new, MAX_DIFF_TEXT);
    let file = match cwd {
        Some(c) => crate::paths::relative(c, &path),
        None => path.clone(),
    };
    let id = format!("d{conn}-{}", call.to_string().trim_matches('"'));
    PendingDiff {
        pane,
        conn,
        call,
        tab: args["tab_name"].as_str().unwrap_or("").to_owned(),
        info: illogical_proto::DiffInfo {
            id,
            file,
            added,
            removed,
            text,
            new: !exists,
            at_ms: now_ms(),
            ide: crate::ide::NAME.into(),
        },
        old,
        new,
    }
}

/// What asks bundle by: the project a directory is in (its git root, from
/// M23's cached lookup), else the directory. A directory on a machine (not
/// this host) is taken as is.
pub fn project_key(cwd: Option<&str>, local: bool) -> String {
    let Some(cwd) = cwd else { return String::new() };
    match local.then(|| crate::classify::project(cwd)).flatten() {
        Some(p) => p.root,
        None => cwd.to_owned(),
    }
}

/// A duration as people say it: 42s, 3m 5s, 1h 2m.
pub fn human_took(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

fn info_of(rec: CommandRec) -> CommandInfo {
    CommandInfo {
        text: rec.text,
        cwd: rec.cwd,
        exit: rec.exit,
        started_ms: rec.started_ms,
        ended_ms: rec.ended_ms,
        start: rec.start,
        end: rec.end,
        by: rec.by,
    }
}

impl Daemon {
    /// Bring back the saved layout and every pane in it. False if there was
    /// nothing (usable) to restore.
    fn restore(&mut self, mut kept: HashMap<String, OwnedFd>) -> bool {
        let saved = match self.store.load_layout() {
            Ok(Some(saved)) => saved,
            Ok(None) => return false,
            Err(e) => {
                let aside = self.store.root().join(format!("layout.json.unreadable-{}", now_ms()));
                warn!(error = %e, aside = %aside.display(), "can't read the saved layout; starting fresh");
                let _ = std::fs::rename(self.store.root().join("layout.json"), aside);
                return false;
            }
        };
        let Saved { mux, panes: meta, machines, next_machine, .. } = saved;
        self.mux = mux;
        self.next_machine = next_machine.max(1);
        // A machine is only kept with what owns it.
        let panes: Vec<PaneId> = self.mux.panes();
        for (id, m) in machines {
            let owned = match m.owner {
                Owner::Pane(p) => panes.contains(&p),
                Owner::Tab(t) => self.mux.tab(t).is_ok(),
            };
            if owned {
                self.machines.insert(id, Machine { state: MachineState::Starting, ..m });
            }
        }
        // Nobody is connected yet; whoever views a tab next sizes it.
        let owners: Vec<ClientId> = self.mux.tabs.values().filter_map(|t| t.owner).collect();
        for o in owners {
            self.mux.release(o);
        }
        let ids = self.mux.panes();
        self.store.remove_strays(&ids);
        let rects = self.mux.pane_rects();
        for id in ids {
            let meta = meta.get(&id).cloned().unwrap_or_default();
            let (cols, rows) = rects.get(&id).map(|r| (r.cols, r.rows)).unwrap_or((80, 24));
            let cwd = meta.cwd.as_ref().map(PathBuf::from).unwrap_or_else(|| self.config.home.clone());
            // Still running on a terminal systemd kept for us: carry on with
            // it. Otherwise restore by policy.
            let record = crate::shim::read_record(&self.store.pane_dir(id).join("process"));
            let mut meta = meta;
            meta.host = meta.host.filter(|m| self.machines.contains_key(m));
            let start = match (kept.remove(&format!("pane-{id}")), meta.host) {
                (Some(master), None) if crate::shim::alive(&record) => Start::Adopt(master),
                // On a machine: reattach to the session if there is one.
                (_, Some(_)) => match ExecRecord::read(&self.store.pane_dir(id)) {
                    Some(r) => Start::Resume {
                        session: r.session,
                        received: r.received,
                        otherwise: Box::new(self.config.restore(id, &meta)),
                    },
                    None => self.config.restore(id, &meta),
                },
                _ => self.config.restore(id, &meta),
            };
            if meta.kind != BlockType::Terminal {
                let config = meta.config.clone().unwrap_or_default();
                // What systemd kept for it (an agent server's pipes).
                let prefix = format!("agent-{id}-");
                let names: Vec<String> = kept.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
                let mine = names.into_iter().filter_map(|k| kept.remove_entry(&k)).collect();
                match self.make_block(id, meta.kind, config, meta.host, Some((meta.policy.clone(), mine))) {
                    Ok(()) => {
                        self.meta.insert(id, meta);
                    }
                    Err(e) => {
                        warn!(block = id, error = %e, "can't restore block; dropping it");
                        let _ = self.mux.apply(Intent::ClosePane { pane: id });
                    }
                }
                continue;
            }
            let integrate = meta.integration.unwrap_or(true);
            // Still running what `run` started: keep holding it.
            let hold = meta.hold && matches!(start, Start::Adopt(_) | Start::Resume { .. });
            match self.open_pane(id, cols, rows, true, start, cwd, integrate, hold, meta.host) {
                Ok(()) => {
                    self.meta.insert(id, meta);
                }
                Err(e) => {
                    warn!(pane = id, error = %e, "can't restore pane; dropping it");
                    let _ = self.mux.apply(Intent::ClosePane { pane: id });
                }
            }
        }
        info!(sessions = self.mux.sessions.len(), panes = self.panes.len(), "restored");
        if self.mux.sessions.is_empty() {
            return false;
        }
        self.machines.retain(|_, m| match m.owner {
            Owner::Pane(p) => self.meta.get(&p).is_some_and(|p| p.host == Some(m.id)),
            Owner::Tab(_) => true,
        });
        self.last_saved = Some((self.mux.clone(), self.meta.clone().into_iter().collect(), self.machines.clone()));
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn open_pane(
        &mut self,
        id: PaneId,
        cols: u16,
        rows: u16,
        restore: bool,
        start: Start,
        cwd: PathBuf,
        integrate: bool,
        hold: bool,
        machine: Option<MachineId>,
    ) -> std::io::Result<()> {
        let host = match machine {
            None => None,
            Some(m) => {
                let provider =
                    self.config.provider.clone().ok_or_else(|| std::io::Error::other("VM panes aren't set up"))?;
                let machine = self.machines.get(&m).ok_or_else(|| std::io::Error::other("no such machine"))?;
                Some(pane::Host {
                    provider,
                    borrowed: machine.borrowed,
                    sprite: machine.sprite.clone(),
                    image: machine.image.clone(),
                    rt: tokio::runtime::Handle::current(),
                })
            }
        };
        let shell = match machine {
            Some(m) => self.config.guest_shell(m, integrate, None),
            None => self.config.shell(id, cwd, integrate),
        };
        let log = PaneLog::open(self.store.pane_dir(id))?;
        let h = pane::spawn_pane(Setup {
            id,
            cols,
            rows,
            log,
            restore,
            start,
            shell,
            launch: self.config.launch.clone(),
            hold,
            notices: self.notices.clone(),
            host,
        })?;
        self.panes.insert(id, h);
        self.ids.lock().unwrap().insert(id);
        self.sizes.insert(id, (cols, rows));
        Ok(())
    }

    /// Make a non-terminal block for `id` and keep it. `restoring`: its
    /// restart policy, and what systemd kept for it.
    fn make_block(
        &mut self,
        id: PaneId,
        kind: BlockType,
        config: serde_json::Value,
        host: Option<MachineId>,
        restoring: Option<(Policy, HashMap<String, OwnedFd>)>,
    ) -> Result<(), String> {
        let sprite = host.and_then(|m| self.machines.get(&m)).map(|m| m.sprite.clone());
        let dir = self.store.pane_dir(id);
        let base = crate::block::BlockEnv {
            notices: self.notices.clone(),
            provider: self.config.provider.clone(),
            launch: self.config.launch.clone(),
            // Claude Code's IDE is for terminals: not an agent block's, nor
            // VS Code's own terminals (M28).
            env: self.config.env(id).into_iter().filter(|(k, _)| k != "CLAUDE_CODE_SSE_PORT").collect(),
            home: self.config.home.clone(),
            secrets: self.config.secrets.clone(),
            mcp: self.config.mcp.clone(),
            fs: self.fs.clone(),
            shell_env: self.shell_env.clone(),
            cmds: Some(self.tx.clone()),
            ids: self.ids.clone(),
        };
        let is_restore = restoring.is_some();
        let (policy, kept) = restoring.unwrap_or_default();
        let ctx = BlockCtx::new(id, dir, base, sprite, is_restore, policy, kept);
        let b = crate::block::create(kind, ctx, config)?;
        self.blocks.insert(id, b);
        self.ids.lock().unwrap().insert(id);
        Ok(())
    }

    fn block_msg(&self, id: PaneId) -> Option<ServerMsg> {
        Some(ServerMsg::Block { block: id, state: self.blocks.get(&id)?.state() })
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Cmd>, mut notices: mpsc::UnboundedReceiver<Notice>) {
        let mut refresh = tokio::time::interval(REFRESH);
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let due = self.save_due.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            let flush = self.flush_due.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Shutdown(done)) => {
                        self.shutdown();
                        let _ = done.send(());
                    }
                    Some(cmd) => self.handle(cmd),
                    None => return,
                },
                Some(n) = notices.recv() => self.notice(n),
                _ = sleep_until(due), if self.save_due.is_some() => {
                    self.save_due = None;
                    self.save();
                }
                _ = refresh.tick() => self.save(),
                _ = sleep_until(flush), if self.flush_due.is_some() => self.flush(),
                _ = tick.tick() => {
                    self.tick_activity();
                    self.tick_drivers();
                    self.find_ide_panes();
                    self.flush();
                }
            }
            self.sync_drawn();
            // A new layout goes out at once, in order with what follows.
            if self.full
                || self.clients.values().any(|c| self.sent.get(&c.client).is_none_or(|s| s.rev != self.mux.rev))
            {
                self.flush();
            }
        }
    }

    fn emit(&self, pane: Option<PaneId>, kind: EventKind) {
        let _ = self.events.send(Event { at_ms: now_ms(), pane, kind });
    }

    fn focused(&self, pane: PaneId) -> bool {
        self.focus.values().any(|p| *p == pane)
    }

    fn set_attention(&mut self, pane: PaneId, state: Attention, why: &str) {
        let reason = match state {
            Attention::NeedsInput => Some(plain_reason(ReasonKind::Input, why)),
            Attention::Done => Some(plain_reason(ReasonKind::Done, why)),
            _ => None,
        };
        self.set_attention_with(pane, state, why, reason);
    }

    /// Set a pane's attention, and why (M24). An open ask overrides the
    /// stored reason while it's open (see [`Self::live_reason`]).
    fn set_attention_with(&mut self, pane: PaneId, state: Attention, why: &str, reason: Option<Reason>) {
        let old = self.attention.get(&pane).copied().unwrap_or_default();
        if !(self.panes.contains_key(&pane) || self.blocks.contains_key(&pane)) {
            return;
        }
        if old == state {
            // Another reason for the same state (a second command done, or a
            // failure after a bell): keep the newer one if it says more.
            let newer = match (&reason, self.reasons.get(&pane)) {
                (Some(r), Some(o)) => r.kind != o.kind && r.kind != ReasonKind::Input,
                (Some(_), None) => true,
                _ => false,
            };
            if newer && let Some(r) = reason {
                self.reasons.insert(pane, r);
                self.emit(Some(pane), EventKind::Attention { state, reason: self.live_reason(pane) });
                self.broadcast();
            }
            return;
        }
        info!(pane, ?state, why, "attention");
        self.attention.insert(pane, state);
        match reason {
            Some(r) => self.reasons.insert(pane, r),
            None => self.reasons.remove(&pane),
        };
        let reason = self.live_reason(pane);
        self.emit(Some(pane), EventKind::Attention { state, reason: reason.clone() });
        if matches!(state, Attention::NeedsInput | Attention::Done) && !self.focused(pane) {
            let title = push_title(state, reason.as_ref());
            let body = reason.as_ref().map_or(why, |r| r.headline.as_str()).to_owned();
            // What lets a notification answer it from its buttons: an
            // approval (an agent block's, or a terminal's permission card,
            // M29), or a question with one or two answers.
            let mut extra = self.blocks.get(&pane).and_then(|b| b.push_extra()).or_else(|| {
                let a = &self.asks.get(&pane)?.ask;
                if a.kind == AskKind::Permission {
                    return Some(serde_json::json!({ "approve": { "id": a.id, "title": a.headline() } }));
                }
                Some(serde_json::json!({ "ask": a.push_choice()? }))
            });
            if let Some(r) = &reason {
                let x = extra.get_or_insert_with(|| serde_json::json!({}));
                x["reason"] = serde_json::json!({ "kind": r.kind, "actions": r.actions, "bundle": r.bundle });
            }
            // The owner, and whoever may edit the session and opted in
            // (M29): on this daemon's own push, and through control (M21),
            // where the payload is encrypted to each device, so it can carry
            // what to approve too.
            let session = self.session_of(pane);
            let acl = self.config.acl.clone();
            if let Some(push) = &self.push {
                let acl = acl.clone();
                push.send_to(pane, title, &body, extra.clone(), move |who| acl.notifies(who, session));
            }
            self.config.control.push(pane, title, &body, extra, move |who| acl.notifies(who.id(), session));
        }
        self.touch(pane);
    }

    /// Why a pane wants you now (M24): an open question or approval while it
    /// needs input, else what was recorded when its state changed.
    fn live_reason(&self, pane: PaneId) -> Option<Reason> {
        let state = self.attention.get(&pane).copied().unwrap_or_default();
        if !matches!(state, Attention::NeedsInput | Attention::Done) {
            return None;
        }
        if state == Attention::NeedsInput
            && let Some(r) = self.diff_reason(pane).or_else(|| self.ask_reason(pane))
        {
            return Some(r);
        }
        self.reasons.get(&pane).cloned()
    }

    /// An edit waiting as a diff in a pane (M28): it comes before the
    /// hook's permission card for the same edit.
    fn diff_reason(&self, pane: PaneId) -> Option<Reason> {
        let d = &self.diffs.iter().find(|d| d.pane == Some(pane))?.info;
        Some(Reason {
            kind: ReasonKind::Diff,
            since_ms: d.at_ms,
            headline: diff_headline(d),
            command: None,
            exit: None,
            duration_ms: None,
            bundle: None,
            ask: None,
            gate: None,
            actions: vec![Action::Accept, Action::Reject, Action::Dismiss],
        })
    }

    /// The process `pid` runs in one of our terminals: which (M28: the
    /// Claude Code behind an IDE connection).
    fn pane_of_pid(&self, pid: u32) -> Option<PaneId> {
        let shells: HashMap<u32, PaneId> = self.panes.iter().filter_map(|(id, h)| Some((h.pid_now()?, *id))).collect();
        let mut p = pid;
        for _ in 0..64 {
            if let Some(id) = shells.get(&p) {
                return Some(*id);
            }
            p = crate::procinfo::ppid(p).filter(|p| *p > 1)?;
        }
        None
    }

    /// What Claude Code's IDE connections did (M28).
    fn ide_event(&mut self, ev: crate::ide::Event) {
        use crate::ide::Event;
        let Some(ide) = self.config.ide.clone() else { return };
        match ev {
            Event::Hello => {
                // The relay says again what's open; start from nothing.
                let panes: Vec<PaneId> = self.diffs.drain(..).filter_map(|d| d.pane).collect();
                self.ide_conns.clear();
                for p in panes {
                    self.diff_changed(p);
                }
            }
            Event::Conn { conn, pid } => {
                let pane = pid.and_then(|p| self.pane_of_pid(p));
                info!(conn, ?pid, ?pane, "Claude Code connected to its IDE");
                self.ide_conns.insert(conn, (pid, pane));
                self.rehome(conn, pane);
                if let Some(p) = pane {
                    self.touch(p);
                }
            }
            Event::Gone { conn } => {
                if let Some((_, Some(p))) = self.ide_conns.remove(&conn) {
                    self.touch(p);
                }
                let gone: Vec<PaneId> = self.diffs.iter().filter(|d| d.conn == conn).filter_map(|d| d.pane).collect();
                self.diffs.retain(|d| d.conn != conn);
                for p in gone {
                    self.diff_changed(p);
                }
            }
            Event::Call { conn, id, tool, args } => match tool.as_str() {
                "openDiff" | "openDiff/here" => {
                    // Another IDE gets diffs, unless passing it on failed.
                    if tool == "openDiff"
                        && let Some(to) = ide.target()
                    {
                        return ide.forward(to, conn, id, args, self.tx.clone());
                    }
                    let pane = self.ide_conns.get(&conn).and_then(|c| c.1);
                    let cwd = pane
                        .and_then(|p| self.panes.get(&p))
                        .and_then(|h| h.status().cwd.or_else(|| h.cwd().map(|c| c.display().to_string())));
                    let d = new_diff(pane, conn, id, &args, cwd.as_deref());
                    info!(conn, ?pane, file = d.info.file, "an edit waits as a diff");
                    self.diffs.retain(|x| !(x.conn == d.conn && x.call == d.call));
                    self.diffs.push(d);
                    if let Some(p) = pane {
                        self.diff_changed(p);
                    }
                }
                "getDiagnostics" => ide.reply(conn, &id, crate::ide::no_diagnostics()),
                _ => ide.reply(conn, &id, serde_json::json!({ "content": [] })),
            },
            Event::Closed { conn, ids, .. } => {
                ide.closed(conn, &ids);
                let (gone, keep): (Vec<PendingDiff>, Vec<PendingDiff>) =
                    self.diffs.drain(..).partition(|d| d.conn == conn && ids.contains(&d.call));
                self.diffs = keep;
                for d in gone {
                    let Some(p) = d.pane else { continue };
                    // The terminal answered it (M29's "Allowed, 14:02").
                    self.answered.insert(
                        p,
                        illogical_proto::ask::Answered {
                            id: d.info.id.clone(),
                            how: "answered".into(),
                            who: "terminal".into(),
                            name: "the terminal".into(),
                            at_ms: now_ms(),
                            headline: diff_headline(&d.info),
                        },
                    );
                    self.diff_changed(p);
                }
            }
        }
    }

    /// The pane an IDE connection's diffs belong to is known now.
    fn rehome(&mut self, conn: u64, pane: Option<PaneId>) {
        let Some(p) = pane else { return };
        let mut found = false;
        for d in self.diffs.iter_mut().filter(|d| d.conn == conn && d.pane.is_none()) {
            d.pane = Some(p);
            found = true;
        }
        if found {
            self.diff_changed(p);
        }
    }

    /// IDE connections whose Claude Code wasn't found in a pane yet (the
    /// panes come back after a restart a moment after the relay speaks):
    /// look again.
    fn find_ide_panes(&mut self) {
        let lost: Vec<(u64, u32)> = self
            .ide_conns
            .iter()
            .filter(|(_, (_, p))| p.is_none())
            .filter_map(|(c, (pid, _))| Some((*c, (*pid)?)))
            .collect();
        for (conn, pid) in lost {
            if let Some(pane) = self.pane_of_pid(pid) {
                info!(conn, pid, pane, "found Claude Code's pane");
                self.ide_conns.insert(conn, (Some(pid), Some(pane)));
                self.rehome(conn, Some(pane));
                self.touch(pane);
            }
        }
    }

    /// A pane's diffs changed: its card and its attention follow.
    fn diff_changed(&mut self, pane: PaneId) {
        match self.diff_reason(pane) {
            Some(r) => {
                let state = self.attention.get(&pane).copied().unwrap_or_default();
                if state == Attention::NeedsInput {
                    // Already asking: the card shows the diff now.
                    self.reasons.insert(pane, r);
                    self.emit(Some(pane), EventKind::Attention { state, reason: self.live_reason(pane) });
                } else {
                    let why = r.headline.clone();
                    self.set_attention_with(pane, Attention::NeedsInput, &why, Some(r));
                }
            }
            None => {
                if self.reasons.get(&pane).is_some_and(|r| r.kind == ReasonKind::Diff) {
                    if self.asks.contains_key(&pane) {
                        self.reasons.remove(&pane);
                    } else {
                        self.set_attention(pane, Attention::Idle, "diff closed");
                    }
                }
            }
        }
        self.touch(pane);
    }

    /// Someone answered a diff card (M28).
    fn diff_answer(
        &mut self,
        pane: PaneId,
        id: Option<String>,
        accept: bool,
        text: Option<String>,
        by: Driver,
    ) -> Result<(), String> {
        let ide = self.config.ide.clone().ok_or("illogical isn't Claude Code's IDE here")?;
        let at = self
            .diffs
            .iter()
            .position(|d| d.pane == Some(pane) && id.as_ref().is_none_or(|i| *i == d.info.id))
            .ok_or_else(|| format!("%{pane} has no edit waiting (the terminal answered it, or it was closed)"))?;
        let d = self.diffs.remove(at);
        let changed = text.as_ref().is_some_and(|t| *t != d.new);
        let result =
            if accept { crate::ide::saved(text.as_deref().unwrap_or(&d.new)) } else { crate::ide::rejected(&d.tab) };
        ide.reply(d.conn, &d.call, result);
        let how = match (accept, changed) {
            (true, false) => "accepted",
            (true, true) => "accepted with changes",
            (false, _) => "rejected",
        };
        self.record_answer(pane, &by, &d.info.id, how, &diff_headline(&d.info));
        self.diff_changed(pane);
        Ok(())
    }

    /// An open question in a terminal (Claude Code's hook), or a block's
    /// open permission request or question.
    fn ask_reason(&self, pane: PaneId) -> Option<Reason> {
        let local = self.meta.get(&pane).and_then(|m| m.host).is_none();
        let (id, what, headline, agent, project, at_ms) = if let Some(a) = self.asks.get(&pane) {
            let what = if a.ask.kind == AskKind::Permission { AskWhat::Approve } else { AskWhat::Question };
            let (agent, project) = match self.blocks.get(&pane) {
                // Raised on a block (M35): who asks is the ask's, and the
                // project the block's (a studio box's app), not a cwd.
                Some(b) => {
                    let s = b.summary();
                    let project = match s.project {
                        Some(p) => p.root,
                        None => project_key(s.cwd.as_deref(), local),
                    };
                    (a.ask.agent.clone().unwrap_or_else(|| a.ask.source.clone()), project)
                }
                None => {
                    let agent =
                        a.ask.agent.clone().or_else(|| self.agent_name(pane)).unwrap_or_else(|| "claude".into());
                    let cwd = self
                        .panes
                        .get(&pane)
                        .and_then(|h| h.status().cwd.or_else(|| h.cwd().map(|c| c.display().to_string())));
                    (agent, project_key(cwd.as_deref(), local))
                }
            };
            (a.ask.id.clone(), what, a.ask.headline(), agent, project, a.ask.at_ms)
        } else {
            let w = self.blocks.get(&pane)?.waiting()?;
            (w.id, w.what, w.headline, w.agent, project_key(w.cwd.as_deref(), local), w.at_ms)
        };
        let actions = match what {
            AskWhat::Approve => vec![Action::Allow, Action::Deny, Action::Dismiss],
            AskWhat::Question => vec![Action::Answer, Action::Deny, Action::Dismiss],
        };
        Some(Reason {
            kind: ReasonKind::Ask,
            since_ms: at_ms,
            headline,
            command: None,
            exit: None,
            duration_ms: None,
            bundle: Some(format!("ask:{project}:{agent}")),
            ask: Some(AskRef { id, what, agent }),
            gate: None,
            actions,
        })
    }

    /// The agent running in a terminal, by its command line.
    fn agent_name(&self, pane: PaneId) -> Option<String> {
        let h = self.panes.get(&pane)?;
        let text = h.status().current.and_then(|c| c.text).or_else(|| h.command()).unwrap_or_default();
        text.split_whitespace().take(3).find_map(|w| {
            let name = w.rsplit('/').next().unwrap_or(w);
            AGENTS.iter().find(|a| name == **a || name.starts_with(&format!("{a}-"))).map(|a| (*a).to_owned())
        })
    }

    /// What a pane's failures bundle by: the machine it runs on.
    fn machine_key(&self, pane: PaneId) -> String {
        match self.machine_of(pane) {
            Some(m) => m.name.clone().unwrap_or_else(|| m.sprite.clone()),
            None => "here".into(),
        }
    }

    fn looks_like_agent(&self, pane: PaneId) -> bool {
        let Some(h) = self.panes.get(&pane) else { return false };
        let text = h.status().current.and_then(|c| c.text).or_else(|| h.command()).unwrap_or_default();
        text.split_whitespace().take(3).any(|w| {
            let name = w.rsplit('/').next().unwrap_or(w);
            AGENTS.iter().any(|a| name == *a || name.starts_with(&format!("{a}-")))
        })
    }

    fn notice(&mut self, n: Notice) {
        if self.shutting_down {
            return;
        }
        let pane = n.pane;
        let running_command = || self.panes.get(&pane).is_some_and(|h| h.status().current.is_some());
        match n.what {
            What::Exited { code, close } => {
                let machine_gone = self.machine_of(pane).is_some_and(|m| m.state == MachineState::Gone);
                self.emit(Some(pane), EventKind::Exit { code, machine_gone });
                if close {
                    info!(pane, ?code, "pane exited; closing it");
                    let _ = self.intent(None, Intent::ClosePane { pane });
                    return;
                }
                if !self.focused(pane) {
                    let failed = machine_gone || code.is_some_and(|c| c != 0);
                    let headline = if machine_gone {
                        "its machine went away".to_owned()
                    } else {
                        format!("exited with code {}", code.unwrap_or(-1))
                    };
                    let reason = if failed {
                        Reason {
                            exit: code,
                            bundle: Some(format!("exited:{}", self.machine_key(pane))),
                            command: self.meta.get(&pane).and_then(|m| m.command.clone()),
                            ..plain_reason(ReasonKind::Exited, &headline)
                        }
                    } else {
                        Reason { exit: code, ..plain_reason(ReasonKind::Done, &headline) }
                    };
                    self.set_attention_with(pane, Attention::Done, &headline, Some(reason));
                }
                self.changed();
            }
            What::Machine(up) => {
                let state = if up { MachineState::Running } else { MachineState::Gone };
                let id = self.meta.get(&pane).and_then(|m| m.host);
                if let Some(m) = id.and_then(|id| self.machines.get_mut(&id))
                    && m.state != state
                {
                    m.state = state;
                    let machine = m.id;
                    info!(pane, machine, ?state, "machine");
                    self.emit(Some(pane), EventKind::Machine { machine, state });
                    self.broadcast();
                }
            }
            What::BlockChanged => {
                // Its summary (an editor's file) goes at the next tick.
                self.mark(pane);
                // An editor that joined has no renderer: its summary is all.
                if self.blocks.get(&pane).is_some_and(|b| b.detached()) {
                    return;
                }
                if let Some(msg) = self.block_msg(pane) {
                    for sub in self.clients.values().filter(|c| self.sees(&c.principal, pane)) {
                        let _ = sub.ctrl.send(ToClient::Msg(msg.clone()));
                    }
                }
                // Its config may have changed with it.
                self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
            }
            // A question raised on a block (M35) still wants an answer,
            // whatever the block says about itself meanwhile (a page that
            // finished loading).
            What::Attention(state, _) if state != Attention::NeedsInput && self.asks.contains_key(&pane) => {}
            What::Attention(state, why) => self.set_attention(pane, state, &why),
            What::Reason(state, reason) => {
                // The same reason again, saying more (the debugger's line,
                // once it's known): the card says that now.
                let same = self.attention.get(&pane) == Some(&state)
                    && self.reasons.get(&pane).is_some_and(|r| r.kind == reason.kind);
                if same {
                    self.reasons.insert(pane, reason);
                    self.emit(Some(pane), EventKind::Attention { state, reason: self.live_reason(pane) });
                    self.touch(pane);
                } else {
                    let why = reason.headline.clone();
                    self.set_attention_with(pane, state, &why, Some(reason));
                }
            }
            What::Clear(kind) => {
                // A question held on the block still wants you.
                if self.asks.contains_key(&pane) && self.reasons.get(&pane).is_some_and(|r| r.kind == kind) {
                    self.reasons.remove(&pane);
                    self.touch(pane);
                } else if self.reasons.get(&pane).is_some_and(|r| r.kind == kind) {
                    self.set_attention(pane, Attention::Idle, "cleared");
                }
            }
            What::Follow(msg) => {
                let msg = ServerMsg::Follow { pane, msg };
                for c in self.follows.get(&pane).into_iter().flatten() {
                    if let Some(sub) = self.clients.get(c) {
                        let _ = sub.ctrl.send(ToClient::Msg(msg.clone()));
                    }
                }
            }
            What::Event(kind) => self.emit(Some(pane), kind),
            What::Started => {
                // What `run` started is gone; the new program isn't held.
                if let Some(m) = self.meta.get_mut(&pane) {
                    m.hold = false;
                }
                self.set_attention(pane, Attention::Idle, "started");
                self.quiet_armed.insert(pane);
                self.changed();
            }
            What::Busy(true) => {
                if running_command()
                    && matches!(
                        self.attention.get(&pane).copied().unwrap_or_default(),
                        Attention::Idle | Attention::Done
                    )
                {
                    self.set_attention(pane, Attention::Working, "output");
                }
            }
            What::Busy(false) => {
                if running_command()
                    && self.looks_like_agent(pane)
                    && self.attention.get(&pane) == Some(&Attention::Working)
                {
                    // Once a turn: after that it's an idle agent redrawing.
                    if self.quiet_armed.remove(&pane) {
                        self.set_attention(pane, Attention::NeedsInput, "an agent went quiet");
                    } else {
                        self.set_attention(pane, Attention::Idle, "quiet");
                    }
                }
            }
            What::Signal(signal) => match signal {
                Signal::Prompt => {
                    self.emit(Some(pane), EventKind::Prompt);
                    if self.attention.get(&pane) == Some(&Attention::Working) {
                        self.set_attention(pane, Attention::Idle, "prompt");
                    }
                }
                Signal::CommandLine { .. } => {}
                Signal::CommandStart => {
                    let text = self.panes.get(&pane).and_then(|h| h.status().current.and_then(|c| c.text));
                    self.emit(Some(pane), EventKind::CommandStart { text });
                    self.quiet_armed.insert(pane);
                    self.set_attention(pane, Attention::Working, "command started");
                    self.touch(pane);
                }
                Signal::CommandEnd { exit } => {
                    let last = self.panes.get(&pane).and_then(|h| h.status().last);
                    let took_ms = last.as_ref().map(|l| l.ended_ms.unwrap_or(l.started_ms) - l.started_ms).unwrap_or(0);
                    let text = last.and_then(|l| l.text);
                    self.emit(Some(pane), EventKind::CommandEnd { text: text.clone(), exit });
                    // Something that asked for you still wants you after its
                    // command ends; only input (or a client) clears that.
                    if self.attention.get(&pane) == Some(&Attention::NeedsInput) {
                        self.touch(pane);
                        return;
                    }
                    let failed = exit.is_some_and(|e| e != 0 && e != 130) && took_ms >= FAILED_AFTER_MS;
                    if (failed || took_ms >= DONE_AFTER_MS) && !self.focused(pane) {
                        let name = text.clone().unwrap_or_else(|| "command".into());
                        let (kind, headline, bundle) = if failed {
                            let h =
                                format!("{name} failed (exit {}) after {}", exit.unwrap_or(-1), human_took(took_ms));
                            (ReasonKind::Failed, h, Some(format!("failed:{}", self.machine_key(pane))))
                        } else {
                            let h = match exit {
                                Some(0) | None => format!("{name} finished after {}", human_took(took_ms)),
                                Some(e) => format!("{name} exited {e} after {}", human_took(took_ms)),
                            };
                            (ReasonKind::Done, h, None)
                        };
                        // A failed command can be typed again (M11), if it
                        // was typed (shell integration saw its line).
                        let mut actions = vec![Action::Dismiss];
                        if failed && text.as_deref().is_some_and(|t| crate::fs::rerun_line(t).is_some()) {
                            actions.insert(0, Action::Rerun);
                        }
                        let reason = Reason {
                            command: text.clone(),
                            exit,
                            duration_ms: Some(took_ms),
                            bundle,
                            actions,
                            ..plain_reason(kind, &headline)
                        };
                        self.set_attention_with(pane, Attention::Done, &headline, Some(reason));
                    } else {
                        self.set_attention(pane, Attention::Idle, "command ended");
                    }
                    self.touch(pane);
                }
                Signal::Cwd { path } => {
                    self.emit(Some(pane), EventKind::Cwd { path });
                    self.mark(pane);
                }
                Signal::Notify { title, body } => {
                    self.emit(Some(pane), EventKind::Notify { title: title.clone(), body: body.clone() });
                    let why = if body.is_empty() { title } else { body };
                    self.set_attention(pane, Attention::NeedsInput, &why);
                }
                Signal::Bell => {
                    self.emit(Some(pane), EventKind::Bell);
                    if !self.focused(pane) {
                        self.set_attention(pane, Attention::NeedsInput, "bell");
                    }
                }
            },
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Connect { sub } => {
                let state = self.state_for(&sub.principal);
                let (client, who) = (sub.client, sub.principal.clone());
                self.clients.insert(client, sub.clone());
                self.send_state(client, state, false, true);
                for id in self.blocks.keys().filter(|id| self.sees(&who, **id)) {
                    if let Some(msg) = self.block_msg(*id) {
                        let _ = sub.ctrl.send(ToClient::Msg(msg));
                    }
                }
                // Everyone else sees them arrive.
                self.soon();
            }
            Cmd::Disconnect { client } => {
                let gone = self.clients.remove(&client).map(|c| c.principal);
                self.unfollow(client, None);
                self.sent.remove(&client);
                self.summary.remove(&client);
                self.focus.remove(&client);
                self.refused.remove(&client);
                self.viewing.remove(&client);
                self.zoomed.remove(&client);
                // Their last client left: they no longer drive anything.
                if let Some(who) = gone
                    && !self.clients.values().any(|c| c.principal.id() == who.id() && !self.summary.contains(&c.client))
                {
                    self.drivers.retain(|_, d| d.who != who.id());
                }
                if !self.clients.is_empty() {
                    self.broadcast();
                }
                for p in self.panes.values() {
                    p.detach(client);
                }
                if self.mux.release(client) {
                    self.changed();
                }
            }
            Cmd::Input { client, pane, data } => {
                let Some(c) = client else { return self.input(pane, data, None) };
                if let Some(why) = self.cant(c, &[pane], Role::Editor) {
                    return self.tell_once(c, why);
                }
                let Some(who) = self.clients.get(&c).map(|x| x.principal.clone()) else { return };
                if let Err(why) = self.may_drive_here(&who, pane) {
                    return self.tell_once(c, why);
                }
                // One driver per pane, unless it's in pair mode: the first to
                // type drives; anyone else is told how to take over.
                if !self.pair.contains(&pane) {
                    match self.drivers.get(&pane) {
                        Some(d) if d.who != who.id() => {
                            let why = format!("{} is driving this pane: take control (pane menu) or ask them", d.name);
                            return self.tell_once(c, why);
                        }
                        Some(_) => self.typed(pane),
                        None if self.panes.contains_key(&pane) => {
                            self.drive(pane, self.driver_for(c));
                            self.typed(pane);
                            self.broadcast();
                        }
                        None => {}
                    }
                }
                let name = self.driver_for(c).name;
                self.input(pane, data, Some(name))
            }
            Cmd::AclChanged => self.acl_changed(),
            Cmd::Msg { client, msg } => self.message(client, msg),
            Cmd::Api(api) => self.api(api),
            Cmd::MachineReset(id) => self.machine_reset(id),
            Cmd::Shutdown(_) => unreachable!("handled in run"),
        }
    }

    /// Someone typed in a pane: whatever it wanted, it has their attention.
    fn input(&mut self, pane: PaneId, data: Vec<u8>, by: Option<String>) {
        let Some(p) = self.panes.get(&pane) else { return };
        // A window gaining or losing focus (focus reporting) isn't a turn.
        let turn = !matches!(&data[..], b"\x1b[I" | b"\x1b[O");
        match by {
            Some(by) => p.input_by(data, by),
            None => p.input(data),
        }
        if turn {
            self.quiet_armed.insert(pane);
        }
        // A question open beside it still wants an answer (typing in Claude
        // Code's prompt box doesn't answer it).
        if self.asks.contains_key(&pane) {
            return;
        }
        if matches!(self.attention.get(&pane), Some(Attention::NeedsInput | Attention::Done)) {
            let next = if p.status().current.is_some() { Attention::Working } else { Attention::Idle };
            self.set_attention(pane, next, "input");
        }
    }

    fn api(&mut self, api: Api) {
        match api {
            Api::RoleOn(who, pane, reply) => {
                let r = if who.is_owner() {
                    Some((Role::Owner, None))
                } else if self.is_presence(pane) {
                    self.presence_role(&who).map(|r| (r, None))
                } else {
                    self.session_of(pane).filter(|_| self.readable(&who, pane)).and_then(|s| {
                        let role = self.config.acl.role(&who, s)?;
                        Some((role, self.config.acl.floor(&who, s, pane)))
                    })
                };
                let _ = reply.send(r);
            }
            Api::MayDrive(who, pane, reply) => {
                let _ = reply.send(self.may_drive_here(&who, pane));
            }
            Api::SessionEnds(session, reply) => {
                let ends = self.mux.session(session).ok().map(|s| {
                    s.tabs
                        .iter()
                        .filter_map(|t| self.mux.tab(*t).ok())
                        .flat_map(|t| t.root.panes())
                        .filter_map(|p| self.panes.get(&p).map(|h| (p, h.status().end)))
                        .collect()
                });
                let _ = reply.send(ends);
            }
            Api::Panes(reply) => {
                let _ = reply.send(self.summaries());
            }
            Api::Pane(pane, reply) => {
                let _ = reply.send(self.panes.get(&pane).cloned());
            }
            Api::Attention(pane, state, why, reply) => {
                let known = self.panes.contains_key(&pane) || self.blocks.contains_key(&pane);
                self.set_attention(pane, state, why.as_deref().unwrap_or("set by the API"));
                let _ = reply.send(known);
            }
            Api::AttentionList(who, reply) => {
                let mut out = Vec::new();
                for (pane, state) in &self.attention {
                    if who.as_ref().is_some_and(|w| !self.readable(w, *pane)) {
                        continue;
                    }
                    let Some(reason) = self.live_reason(*pane) else { continue };
                    let session = self.session_of(*pane);
                    if session.is_none() && !self.is_presence(*pane) {
                        continue;
                    }
                    out.push(illogical_proto::api::AttentionItem { pane: *pane, session, state: *state, reason });
                }
                out.sort_by_key(|i| (i.reason.since_ms, i.pane));
                let _ = reply.send(out);
            }
            Api::Reason(pane, reply) => {
                // A block answers through its own methods, unless the daemon
                // holds the question (M35: raised on it through `Ask`).
                let block = self.blocks.contains_key(&pane) && !self.asks.contains_key(&pane);
                let _ = reply.send(self.live_reason(pane).map(|r| (r, block)));
            }
            Api::Machines(reply) => {
                let _ = reply.send(self.machines.values().cloned().collect());
            }
            Api::MachineOf(pane, reply) => {
                let _ = reply.send(self.machine_of(pane).cloned());
            }
            Api::ShareMachine(pane, reply) => {
                let _ = reply.send(self.share_machine(pane));
            }
            Api::ResetMachine(id, reply) => {
                let _ = reply.send(self.reset_machine(id));
            }
            Api::Open(req, who, reply) => {
                let r = match who.filter(|w| !w.is_owner()) {
                    None => self.open_block(req),
                    Some(who) => self.guest_block(req, &who),
                };
                let _ = reply.send(r);
            }
            Api::Block(id, reply) => {
                let _ = reply.send(self.blocks.get(&id).cloned());
            }
            Api::OwnTab(pane, name, reply) => {
                let _ = reply.send(self.own_tab(pane, name));
            }
            Api::Close(pane, reply) => {
                let known = self.panes.contains_key(&pane) || self.blocks.contains_key(&pane);
                if known {
                    let _ = self.intent(None, Intent::ClosePane { pane });
                }
                let _ = reply.send(known);
            }
            Api::Run(req, reply) => {
                let _ = reply.send(self.run_command(req));
            }
            Api::AgentEnv(reply) => {
                let _ = reply.send((self.config.home.clone(), self.config.env(0)));
            }
            Api::InputBy(pane, data, by) => self.input(pane, data, Some(by)),
            Api::Ide(ev) => self.ide_event(ev),
            Api::IdeConns(pane, reply) => {
                let mut v: Vec<u64> =
                    self.ide_conns.iter().filter(|(_, c)| c.1 == Some(pane)).map(|(n, _)| *n).collect();
                v.sort();
                let _ = reply.send(v);
            }
            Api::Editors(who, reply) => {
                let mut out: Vec<(PaneId, serde_json::Value)> = self
                    .blocks
                    .iter()
                    .filter(|(id, b)| b.link().is_some() && who.as_ref().is_none_or(|w| self.readable(w, **id)))
                    .map(|(id, b)| {
                        let i = self.block_info(*id, b);
                        let v = serde_json::json!({
                            "pane": id, "block": !b.detached(), "editor": i.editor, "folder": i.cwd,
                            "project": i.project, "file": i.file, "title": i.title, "attention": i.attention,
                            "reason": i.reason,
                        });
                        (*id, v)
                    })
                    .collect();
                out.sort_by_key(|(id, _)| *id);
                let _ = reply.send(out.into_iter().map(|(_, v)| v).collect());
            }
            Api::DiffAnswer(pane, id, accept, text, by, reply) => {
                let _ = reply.send(self.diff_answer(pane, id, accept, text, by));
            }
            Api::DiffOf(pane, reply) => {
                let d = self.diffs.iter().find(|d| d.pane == Some(pane));
                let _ = reply.send(d.map(|d| (d.info.clone(), d.old.clone(), d.new.clone())));
            }
            Api::EditorJoin(link, reply) => {
                let id = self.mux.reserve_pane();
                link.bind(id, self.notices.clone());
                self.blocks.insert(id, crate::editor::presence::Presence::make(link));
                // A new entry: everyone gets a whole State with it.
                self.full = true;
                self.broadcast();
                self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
                let _ = reply.send(id);
            }
            Api::EditorLeave(id) => {
                if self.blocks.get(&id).is_some_and(|b| b.detached()) {
                    self.blocks.remove(&id);
                    self.attention.remove(&id);
                    self.reasons.remove(&id);
                    self.follows.remove(&id);
                    self.full = true;
                    self.broadcast();
                }
            }
            Api::StartedBy(pane, by) => {
                if self.panes.contains_key(&pane) || self.blocks.contains_key(&pane) {
                    self.meta.entry(pane).or_default().started_by = Some(by);
                    self.touch(pane);
                }
            }
            Api::Ask(pane, ask, reply) => {
                let _ = reply.send(self.ask(pane, *ask));
            }
            Api::Holds(pane, reply) => {
                let _ = reply.send(self.asks.contains_key(&pane));
            }
            Api::AskReply(pane, id, answer, by, reply) => {
                let _ = reply.send(self.ask_reply(pane, id, answer, by));
            }
            Api::Hook(pane, hook) => self.hook(pane, &hook),
            Api::Inbox(pane, hook, reply) => {
                self.hook(pane, &hook);
                let _ = reply.send(self.wait_inbox(pane));
            }
            Api::InboxGone(pane, token) => {
                if self.inbox.get(&pane).is_some_and(|w| w.token == token) {
                    self.inbox.remove(&pane);
                    self.touch(pane);
                }
            }
            Api::FollowUp(pane, text, by, reply) => {
                let _ = reply.send(self.follow_up(pane, text, by));
            }
            Api::Answered(pane, by, id, how, headline) => self.record_answer(pane, &by, &id, &how, &headline),
            Api::Who(who, reply) => {
                let _ = reply.send(self.driver_of(&who));
            }
            Api::AskWithdraw(pane, id, token) => {
                let open = self.asks.get(&pane).is_some_and(|a| {
                    id.as_ref().is_none_or(|id| *id == a.ask.id) && token.is_none_or(|t| t == a.token)
                });
                if open && let Some(a) = self.asks.remove(&pane) {
                    info!(pane, id = a.ask.id, "question withdrawn");
                    let _ = a.reply.send((AskReply::Withdrawn, None));
                    if a.ask.kind == AskKind::Permission && token.is_none() {
                        // Its hook was stopped: "No" or Esc in the terminal.
                        self.terminal_answered(pane, &a.ask, "denied in the terminal");
                    }
                    self.after_ask(pane);
                }
            }
        }
    }

    /// Show a terminal's question on every client, and ask for you.
    fn ask(&mut self, pane: PaneId, ask: Ask) -> Result<(u64, oneshot::Receiver<Replied>), String> {
        // A terminal, or a block that doesn't ask through its own methods
        // (a web page, a studio box: M35). An agent block's questions are
        // its own.
        let block = self.blocks.get(&pane).map(|b| b.kind());
        match block {
            None if !self.panes.contains_key(&pane) => return Err(format!("no pane %{pane}")),
            Some(BlockType::Agent | BlockType::Remote) => {
                return Err(format!("%{pane} is an agent or remote block: it asks through its own methods"));
            }
            _ => {}
        }
        let mut ask = ask;
        if ask.kind == AskKind::Permission && ask.tool_call_id.is_none() {
            // The tool call it's for: the `PreToolUse` just before it.
            if let Some(id) = self.pre.get(&pane).and_then(|pre| pre.iter().rev().find(|p| same_call(p, &ask))) {
                let id = id["tool_use_id"].as_str().map(str::to_owned);
                ask.id = id.clone().unwrap_or(ask.id);
                ask.tool_call_id = id;
            }
        }
        self.answered.remove(&pane);
        let (tx, rx) = oneshot::channel();
        let token = self.next_ask;
        self.next_ask += 1;
        let why = ask.headline();
        info!(pane, id = ask.id, "question asked");
        // The same question again (its asker reconnected) or a newer one:
        // either way the older registration is over.
        if let Some(old) = self.asks.insert(pane, TermAsk { ask, token, reply: tx }) {
            let _ = old.reply.send((AskReply::Withdrawn, None));
        }
        if self.attention.get(&pane) == Some(&Attention::NeedsInput) {
            // Already asking for you (Claude Code's own hook, say): this is
            // what it wants, and the card changed.
            self.touch(pane);
        } else {
            self.set_attention(pane, Attention::NeedsInput, &why);
        }
        Ok((token, rx))
    }

    fn ask_reply(
        &mut self,
        pane: PaneId,
        id: Option<String>,
        answer: AskReply,
        by: Option<Driver>,
    ) -> Result<Ask, String> {
        let a = self
            .asks
            .get(&pane)
            .filter(|a| id.as_ref().is_none_or(|id| *id == a.ask.id))
            .ok_or_else(|| format!("no open question in %{pane} (it was answered, or withdrawn)"))?;
        let ask = a.ask.clone();
        let permission = ask.kind == AskKind::Permission;
        let answer = match answer {
            AskReply::Allow { .. } | AskReply::Deny { .. } if !permission => {
                return Err(format!("%{pane} asks a question: answer it"));
            }
            AskReply::Answer(_) if permission => return Err(format!("%{pane} asks for approval: allow or deny it")),
            // "Always": one of Claude Code's own suggestions, by index.
            AskReply::Allow { always: Some(i) } => {
                let pick = ask.suggestions.as_ref().and_then(|s| s.get(i.as_u64().unwrap_or(0) as usize)).cloned();
                if pick.is_none() {
                    return Err("Claude Code suggested no rule to keep: allow it once".into());
                }
                AskReply::Allow { always: pick }
            }
            a => a,
        };
        let a = self.asks.remove(&pane).expect("just found");
        info!(pane, id = ask.id, ?answer, "question answered");
        let terminal = answer == AskReply::Terminal;
        let how = match &answer {
            AskReply::Answer(_) => Some("answered"),
            AskReply::Decline => Some("skipped"),
            AskReply::Allow { always: None } => Some("allowed"),
            AskReply::Allow { always: Some(_) } => Some("allowed always"),
            AskReply::Deny { .. } => Some("denied"),
            AskReply::Terminal | AskReply::Withdrawn => None,
        };
        let by = by.unwrap_or_else(|| self.driver_of(&Principal::Owner));
        let _ = a.reply.send((answer, Some(by.clone())));
        if let Some(how) = how {
            self.record_answer(pane, &by, &ask.id, how, &ask.headline());
        }
        if terminal {
            // It asks again in the terminal: still wants you.
            self.touch(pane);
        } else {
            self.after_ask(pane);
        }
        Ok(ask)
    }

    /// A terminal's question went: back to work, and its card off every
    /// client.
    fn after_ask(&mut self, pane: PaneId) {
        if self.attention.get(&pane) == Some(&Attention::NeedsInput) {
            // A block still waiting for something else (a studio box's
            // gate) keeps wanting you, for that.
            if self.blocks.contains_key(&pane)
                && self.reasons.get(&pane).is_some_and(|r| !matches!(r.kind, ReasonKind::Ask | ReasonKind::Input))
            {
                self.emit(
                    Some(pane),
                    EventKind::Attention { state: Attention::NeedsInput, reason: self.live_reason(pane) },
                );
                self.touch(pane);
                return;
            }
            // A terminal's program goes on; a block that was asked for
            // someone else (a studio box) is just a page again.
            let next = if self.blocks.contains_key(&pane) { Attention::Idle } else { Attention::Working };
            self.set_attention(pane, next, "answered");
        } else {
            self.touch(pane);
        }
    }

    /// Someone answered a card (M29): every client's card closes saying who,
    /// and the pane's history and the audit log say so too.
    fn record_answer(&mut self, pane: PaneId, by: &Driver, id: &str, how: &str, headline: &str) {
        info!(pane, who = by.who, how, "answered");
        let at_ms = now_ms();
        self.answered.insert(
            pane,
            illogical_proto::ask::Answered {
                id: id.to_owned(),
                how: how.to_owned(),
                who: by.who.clone(),
                name: by.name.clone(),
                at_ms,
                headline: headline.to_owned(),
            },
        );
        if let Some(p) = self.panes.get(&pane) {
            p.note(format!("{how}: {headline}"), by.name.clone());
        }
        self.config.acl.record(serde_json::json!({
            "at": at_ms, "by": by.who, "name": by.name, "action": "answer", "pane": pane, "how": how,
            "headline": headline,
        }));
        self.touch(pane);
    }

    /// The terminal answered a permission card first (M29): Claude Code
    /// never tells its hook about a "Yes", so its next step closes the card.
    fn terminal_answered(&mut self, pane: PaneId, ask: &Ask, how: &str) {
        self.answered.insert(
            pane,
            illogical_proto::ask::Answered {
                id: ask.id.clone(),
                how: how.to_owned(),
                who: "terminal".into(),
                name: "the terminal".into(),
                at_ms: now_ms(),
                headline: ask.headline(),
            },
        );
    }

    /// One of Claude Code's hook events in a terminal (M29).
    fn hook(&mut self, pane: PaneId, hook: &serde_json::Value) {
        let event = hook["hook_event_name"].as_str().unwrap_or_default();
        let session = session_key(hook);
        let open = self
            .asks
            .get(&pane)
            .filter(|a| a.ask.kind == AskKind::Permission)
            .map(|a| a.ask.clone())
            .filter(|a| a.session.as_deref().is_some_and(|s| s.split('/').next() == session.split('/').next()));
        let close = match (event, &open) {
            ("PreToolUse", Some(a)) if a.session.as_deref() == Some(session.as_str()) => {
                if a.tool_call_id.is_none() && same_call(hook, a) {
                    // The card came first: this is its tool call.
                    if let Some(t) = self.asks.get_mut(&pane) {
                        t.ask.tool_call_id = hook["tool_use_id"].as_str().map(str::to_owned);
                    }
                    None
                } else if a.tool_call_id.as_deref() != hook["tool_use_id"].as_str() {
                    Some("closed")
                } else {
                    None
                }
            }
            ("PostToolUse" | "PostToolUseFailure", Some(a))
                if a.tool_call_id.is_some() && a.tool_call_id.as_deref() == hook["tool_use_id"].as_str()
                    || a.tool_call_id.is_none() && same_call(hook, a) =>
            {
                Some("allowed in the terminal")
            }
            ("Stop" | "UserPromptSubmit" | "SessionStart", Some(_)) => Some("closed"),
            _ => None,
        };
        if event == "PreToolUse" {
            let pre = self.pre.entry(pane).or_default();
            pre.push_back(hook.clone());
            while pre.len() > 16 {
                pre.pop_front();
            }
        }
        if let (Some(how), Some(a)) = (close, open)
            && let Some(t) = self.asks.remove(&pane)
        {
            info!(pane, id = a.id, event, "permission card closed: the terminal answered");
            let _ = t.reply.send((AskReply::Withdrawn, None));
            self.terminal_answered(pane, &a, how);
            self.after_ask(pane);
        }
    }

    /// `illogical inbox` waits for a follow-up: one queued goes now.
    fn wait_inbox(&mut self, pane: PaneId) -> Result<(u64, oneshot::Receiver<InboxReply>), String> {
        if !self.panes.contains_key(&pane) {
            return Err(format!("no terminal %{pane}"));
        }
        let (tx, rx) = oneshot::channel();
        let token = self.next_ask;
        self.next_ask += 1;
        if let Some((text, by)) = self.queued.get_mut(&pane).and_then(|q| q.pop_front()) {
            let _ = tx.send(InboxReply::FollowUp { text, by });
            return Ok((token, rx));
        }
        if let Some(old) = self.inbox.insert(pane, Waiter { token, reply: tx }) {
            let _ = old.reply.send(InboxReply::Replaced);
        } else {
            self.touch(pane);
        }
        Ok((token, rx))
    }

    /// A follow-up for Claude Code in a terminal (M29), recorded as input
    /// from whoever sent it.
    fn follow_up(&mut self, pane: PaneId, text: String, by: Driver) -> Result<bool, String> {
        let Some(p) = self.panes.get(&pane) else { return Err(format!("no terminal %{pane}")) };
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err("an empty follow-up".into());
        }
        info!(pane, who = by.who, "follow-up");
        p.note(format!("follow-up: {text}"), by.name.clone());
        self.config.acl.record(serde_json::json!({
            "at": now_ms(), "by": by.who, "name": by.name, "action": "follow_up", "pane": pane, "text": text,
        }));
        let now = match self.inbox.remove(&pane) {
            Some(w) => match w.reply.send(InboxReply::FollowUp { text: text.clone(), by: by.clone() }) {
                Ok(()) => true,
                Err(InboxReply::FollowUp { text, by }) => {
                    self.queued.entry(pane).or_default().push_back((text, by));
                    false
                }
                Err(_) => false,
            },
            None => {
                let q = self.queued.entry(pane).or_default();
                if q.len() >= MAX_QUEUED {
                    return Err("too many follow-ups waiting: is Claude Code's inbox hook set up?".into());
                }
                q.push_back((text, by));
                false
            }
        };
        self.touch(pane);
        Ok(now)
    }

    /// `illogical run`: a new tab (or a split) running a command.
    fn run_command(&mut self, req: RunRequest) -> Result<PaneId, String> {
        let from = req.from_pane.filter(|p| self.panes.contains_key(p));
        let cwd = req
            .cwd
            .clone()
            .map(PathBuf::from)
            .or_else(|| from.and_then(|p| self.panes.get(&p)?.cwd()))
            .unwrap_or_else(|| self.config.home.clone());
        // A session that doesn't exist yet starts with this pane, not a
        // shell beside it (#17: a home daemon's remote panes go in a
        // session of its name here).
        let fresh = req.session.clone().filter(|n| {
            req.split.is_none()
                && !req.vm_tab
                && !self.mux.sessions.iter().any(|s| s.name == *n || s.id.to_string() == *n)
        });
        let session = match fresh {
            Some(_) => None,
            None => self.resolve_session(req.session.as_deref(), from)?,
        };
        let before: Vec<PaneId> = self.mux.panes();
        // Joining a split pane's host: its tab's machine (which a split
        // takes anyway), or a sandbox it has a shell on (borrowed again).
        let join = match req.split.filter(|_| req.join) {
            Some(pane) => self.join_host(pane)?,
            None => Join::Here,
        };
        let host = match (&req.sandbox, &join) {
            (Some(sandbox), _) => Some(self.borrow_machine(sandbox)?),
            (None, Join::Borrow(sprite)) => Some(self.borrow_machine(&sprite.clone())?),
            (None, _) if req.vm || req.vm_tab => Some(self.new_machine(req.image.clone())?),
            (None, _) => None,
        };
        let on_machine = host.is_some() || matches!(join, Join::TabMachine);
        // A shell started in a directory: this host's (the default is the
        // pane it came from), or the machine's when one is given.
        self.next_cwd = match (&req.command, on_machine) {
            (Some(_), _) => None,
            (None, true) => req.cwd.clone().map(PathBuf::from),
            (None, false) => req.cwd.is_some().then(|| cwd.clone()),
        };
        self.next_spawn = req.command.as_ref().map(|command| {
            let spawn = match host {
                // Not this host's directory: the guest's, if one was asked for.
                Some(_) => self.config.guest_run(0, req.cwd.clone().map(PathBuf::from), command),
                None => self.config.run_only(0, cwd, command),
            };
            (spawn, Some(command.clone()))
        });
        self.next_host = host;
        self.next_owner_tab = req.vm_tab;
        let split = req.split.filter(|_| !req.vm_tab);
        let intent = match (split, session) {
            // Here: a script's command is for this host, even in a VM tab
            // (unless it asked to join the pane's machine).
            (Some(pane), _) => {
                let local = !matches!(join, Join::TabMachine);
                Intent::Split { pane, edge: illogical_proto::Edge::Right, local, cwd: None }
            }
            (None, Some(session)) => Intent::NewTab { session, from_pane: from, cwd: None },
            (None, None) => Intent::NewSession { name: fresh, from_pane: from },
        };
        let result = self.intent(None, intent);
        self.next_spawn = None;
        self.next_owner_tab = false;
        self.next_cwd = None;
        if let Some(m) = self.next_host.take() {
            // Nothing took it.
            self.machines.remove(&m);
        }
        result?;
        let pane = self.mux.panes().into_iter().find(|p| !before.contains(p)).ok_or("no pane was created")?;
        if let Some(policy) = req.policy {
            self.meta.entry(pane).or_default().policy = policy;
        }
        Ok(pane)
    }

    /// Where a pane joining `pane`'s host runs (`run --split --join`).
    fn join_host(&self, pane: PaneId) -> Result<Join, String> {
        let Some(m) = self.machine_of(pane) else { return Ok(Join::Here) };
        if m.borrowed {
            return Ok(Join::Borrow(m.sprite.clone()));
        }
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        if self.tab_machine(tab) == Some(m.id) {
            return Ok(Join::TabMachine);
        }
        Err(format!("%{pane}'s machine is its own: share it with the tab first (Share machine with tab)"))
    }

    /// Which session a new tab goes in: one named (made if missing), else
    /// `from`'s, else the first.
    fn resolve_session(&mut self, name: Option<&str>, from: Option<PaneId>) -> Result<Option<u32>, String> {
        Ok(match name {
            Some(name) => match self.mux.sessions.iter().find(|s| s.name == name || s.id.to_string() == name) {
                Some(s) => Some(s.id),
                None => {
                    // A new session starts with a shell; the block gets a
                    // tab of its own next to it.
                    self.intent(None, Intent::NewSession { name: Some(name.to_owned()), from_pane: None })?;
                    self.mux.sessions.last().map(|s| s.id)
                }
            },
            None => from
                .and_then(|p| self.mux.tab_of(p).ok())
                .and_then(|t| self.mux.session_of_tab(t).ok())
                .or_else(|| self.mux.sessions.first().map(|s| s.id)),
        })
    }

    /// `POST /api/blocks`: a new block of any type, in a tab of its own or
    /// split beside another. In a VM tab it runs on the tab's machine.
    /// A guest's block (M14): an agent, beside a pane in a session they
    /// edit, on a VM of their own (within their quota).
    fn guest_block(&mut self, mut req: OpenRequest, who: &Principal) -> Result<PaneId, String> {
        if req.kind != BlockType::Agent {
            return Err("guests can start agents; other blocks are the owner's".into());
        }
        if !req.config["as_fountain"].is_null() {
            return Err("only the owner can wear a Fountain agent here".into());
        }
        let pane = req.split.or(req.from_pane).ok_or("start it beside a pane")?;
        let session = self.session_of(pane).ok_or("no such pane")?;
        if self.config.acl.role(who, session).is_none_or(|r| r < Role::Editor) {
            return Err("you can't start agents in this session".into());
        }
        let mine = self.machines.values().filter(|m| m.by.as_deref() == Some(who.id())).count();
        if mine >= self.config.guest_machines {
            return Err(format!("you have {mine} VMs here, the most a guest may have: close one first"));
        }
        (req.vm, req.local, req.host, req.session) = (true, false, None, None);
        let before: Vec<MachineId> = self.machines.keys().copied().collect();
        let block = self.open_block(req)?;
        for m in self.machines.values_mut().filter(|m| !before.contains(&m.id)) {
            m.by = Some(who.id().to_owned());
        }
        Ok(block)
    }

    fn open_block(&mut self, mut req: OpenRequest) -> Result<PaneId, String> {
        if req.kind == BlockType::Terminal {
            return Err("terminals are opened with run".into());
        }
        let from = req.from_pane.filter(|p| self.panes.contains_key(p) || self.blocks.contains_key(p));
        match req.kind {
            BlockType::Editor => self.editor_defaults(&mut req, from),
            BlockType::Diff | BlockType::File => self.view_defaults(&mut req, from),
            _ => {}
        }
        // A pane on another daemon (#17): only its place is here, never on
        // a machine of ours.
        if req.kind == BlockType::Remote {
            crate::remote::parse(&req.config)?;
            (req.vm, req.host, req.local) = (false, None, true);
        }
        // A studio box (M35) is its own site: nothing of it runs here or on
        // a machine of ours.
        if req.kind == BlockType::App {
            (req.vm, req.host, req.local) = (false, None, true);
        }
        let session = self.resolve_session(req.session.as_deref(), from)?;
        let before: Vec<PaneId> = self.mux.panes();
        self.last_block_error = None;
        // On a new machine of its own, or one that exists (a tab's).
        if let Some(m) = req.host
            && !self.machines.contains_key(&m)
        {
            return Err(format!("no machine m{m}"));
        }
        self.next_host = match (req.vm, req.host) {
            (true, _) => Some(self.new_machine(req.image.clone())?),
            (false, host) => host,
        };
        let made = req.vm.then_some(self.next_host).flatten();
        // Here if asked; an agent also runs here unless asked for a machine;
        // a page in a VM tab is the tab's machine's.
        let local = req.local || (req.kind == BlockType::Agent && self.next_host.is_none());
        self.next_block = Some((req.kind, req.config));
        let intent = match (req.split, session) {
            (Some(pane), _) => Intent::Split { pane, edge: illogical_proto::Edge::Right, local, cwd: None },
            (None, Some(session)) => Intent::NewTab { session, from_pane: from, cwd: None },
            (None, None) => Intent::NewSession { name: None, from_pane: from },
        };
        let result = self.intent(None, intent);
        let unused = self.next_block.take();
        if self.next_host.take().is_some()
            && let Some(m) = made
        {
            // Nothing took it.
            self.machines.remove(&m);
        }
        result?;
        if let Some((kind, _)) = unused {
            return Err(format!("no {kind:?} block was made").to_lowercase());
        }
        let made = self.mux.panes().into_iter().find(|p| !before.contains(p));
        let Some(id) = made else {
            return Err(self.last_block_error.take().unwrap_or_else(|| "no block was made".into()));
        };
        if !self.blocks.contains_key(&id) {
            return Err(self.last_block_error.take().unwrap_or_else(|| "the block couldn't start".into()));
        }
        Ok(id)
    }

    /// `pane` alone in a tab, right after the one it was in; that tab
    /// named `name` unless it has a name.
    fn own_tab(&mut self, pane: PaneId, name: Option<String>) -> Result<(), String> {
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        let session = self.mux.session_of_tab(tab).map_err(|e| e.to_string())?;
        if self.mux.tab(tab).map_err(|e| e.to_string())?.root != illogical_core::Node::pane(pane) {
            let index =
                self.mux.session(session).ok().and_then(|s| s.tabs.iter().position(|t| *t == tab)).map(|i| i + 1);
            self.intent(None, Intent::BreakPane { pane, session, index })?;
        }
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        if let Some(name) = name
            && self.mux.tab(tab).is_ok_and(|t| t.name.is_none())
        {
            self.intent(None, Intent::RenameTab { tab, name: Some(name) })?;
        }
        Ok(())
    }

    /// An editor opens on the machine of the pane it's opened from, in
    /// that pane's directory, unless told otherwise.
    fn editor_defaults(&self, req: &mut OpenRequest, from: Option<PaneId>) {
        let beside = req.split.or(from);
        if req.host.is_none() && !req.local && !req.vm {
            req.host = beside.and_then(|p| self.meta.get(&p)).and_then(|m| m.host);
        }
        let has_path = ["path", "folder"].iter().any(|k| req.config.get(*k).is_some_and(|v| !v.is_null()));
        if !has_path {
            let here = req.host.is_none();
            let cwd = beside.and_then(|p| self.info_of_any(p)).and_then(|i| i.cwd);
            let path = match cwd {
                Some(c) => c,
                None if here => self.config.home.display().to_string(),
                None => "~".into(),
            };
            if !req.config.is_object() {
                req.config = serde_json::json!({});
            }
            req.config["path"] = path.into();
        }
    }

    /// A diff or file block (M11) is on the machine of the pane it's opened
    /// from (a VM tab's, say), unless told otherwise; a diff's repository
    /// is that pane's directory's, and a file's relative path is from it.
    fn view_defaults(&self, req: &mut OpenRequest, from: Option<PaneId>) {
        let beside = req.split.or(from);
        if req.host.is_none() && !req.local && !req.vm {
            req.host = beside.and_then(|p| self.meta.get(&p)).and_then(|m| m.host);
        }
        if !req.config.is_object() {
            req.config = serde_json::json!({});
        }
        let cwd = beside.and_then(|p| self.info_of_any(p)).and_then(|i| i.cwd);
        let key = if req.kind == BlockType::Diff { "repo" } else { "path" };
        let given = req.config[key].as_str().filter(|p| !p.is_empty()).map(str::to_owned);
        let here = req.host.is_none();
        let home = || if here { self.config.home.display().to_string() } else { "~".into() };
        let whole = match (given, cwd) {
            (Some(p), _) if p.starts_with('/') || p.starts_with('~') => p,
            (Some(p), Some(c)) => format!("{}/{p}", c.trim_end_matches('/')),
            (Some(p), None) => p,
            (None, Some(c)) => c,
            (None, None) => home(),
        };
        req.config[key] = whole.into();
    }

    /// Tell blocks whether anyone draws them (M11): a client that isn't
    /// summaries-only shows their tab, and on a phone, them.
    fn sync_drawn(&mut self) {
        if self.blocks.is_empty() && self.drawn.is_empty() {
            return;
        }
        let mut now = std::collections::HashSet::new();
        for (client, tab) in &self.viewing {
            if self.summary.contains(client) || !self.clients.contains_key(client) {
                continue;
            }
            let Ok(t) = self.mux.tab(*tab) else { continue };
            let panes = t.root.panes();
            let zoom = self.zoomed.get(client).filter(|z| panes.contains(z));
            now.extend(panes.into_iter().filter(|p| self.blocks.contains_key(p) && zoom.is_none_or(|z| z == p)));
        }
        if now == self.drawn {
            return;
        }
        for id in now.symmetric_difference(&self.drawn) {
            if let Some(b) = self.blocks.get(id) {
                b.drawn(now.contains(id));
            }
        }
        self.drawn = now;
    }

    /// A new machine for the next pane; it's created when its first
    /// program starts.
    fn new_machine(&mut self, image: Option<String>) -> Result<MachineId, String> {
        if self.config.provider.is_none() {
            return Err("VM panes aren't set up: illogicald found no wisp token (see --wisp-token-file)".into());
        }
        let id = self.next_machine;
        let sprite = format!("{}{id}", self.config.sprite_prefix());
        Ok(self.add_machine(sprite, image, false))
    }

    /// Someone else's sandbox, borrowed for the next pane's shell ("open
    /// shell", M4b): never created, reset or deleted by us.
    fn borrow_machine(&mut self, sprite: &str) -> Result<MachineId, String> {
        if self.config.provider.is_none() {
            return Err("no sandbox provider: illogicald found no wisp token (see --wisp-token-file)".into());
        }
        if sprite.starts_with(&self.config.sprite_prefix()) {
            return Err(format!("{sprite} is one of this daemon's own machines"));
        }
        Ok(self.add_machine(sprite.to_owned(), None, true))
    }

    fn add_machine(&mut self, sprite: String, image: Option<String>, borrowed: bool) -> MachineId {
        let id = self.next_machine;
        self.next_machine += 1;
        let provider = self.config.provider.as_ref().map_or("wisp", |p| p.name()).to_owned();
        let owner = Owner::Pane(0);
        // Ours get a name to show; a borrowed sandbox has its own.
        let name = (!borrowed).then(|| {
            let taken = |n: &str| self.machines.values().any(|m| m.name.as_deref() == Some(n));
            illogical_core::names::generate(seed(), taken)
        });
        let state = MachineState::Starting;
        let m = Machine { id, provider, sprite, name, image, owner, state, borrowed, by: None };
        self.machines.insert(id, m);
        id
    }

    fn machine_of(&self, pane: PaneId) -> Option<&Machine> {
        self.machines.get(&self.meta.get(&pane)?.host?)
    }

    /// The machine a tab owns.
    fn tab_machine(&self, tab: TabId) -> Option<MachineId> {
        self.machines.values().find(|m| m.owner == Owner::Tab(tab)).map(|m| m.id)
    }

    /// Whether a pane runs on its own tab's machine (and so can't leave it).
    fn on_tab_machine(&self, pane: PaneId) -> bool {
        let tab = self.mux.tab_of(pane).ok();
        let host = self.meta.get(&pane).and_then(|m| m.host);
        host.is_some() && tab.and_then(|t| self.tab_machine(t)) == host
    }

    /// Refuse moves that would take a pane away from its tab's machine.
    fn check_move(&self, intent: &Intent) -> Result<(), String> {
        let stuck = |pane: PaneId| format!("%{pane} runs on this tab's machine, so it stays in the tab");
        match *intent {
            Intent::MovePane { pane, target, .. }
                if self.on_tab_machine(pane) && self.mux.tab_of(pane).ok() != self.mux.tab_of(target).ok() =>
            {
                Err(stuck(pane))
            }
            Intent::BreakPane { pane, .. } if self.on_tab_machine(pane) => Err(stuck(pane)),
            Intent::DockTab { tab, .. } if self.tab_machine(tab).is_some() => {
                Err("this tab has a machine: move the whole tab instead".into())
            }
            _ => Ok(()),
        }
    }

    /// Delete machines whose tab has closed.
    fn reap_machines(&mut self) {
        let gone: Vec<MachineId> = self
            .machines
            .values()
            .filter(|m| matches!(m.owner, Owner::Tab(t) if self.mux.tab(t).is_err()))
            .map(|m| m.id)
            .collect();
        for m in gone {
            self.delete_machine(m);
        }
    }

    /// "Share machine with tab": the pane's own machine becomes its tab's,
    /// and new splits in the tab join it.
    fn share_machine(&mut self, pane: PaneId) -> Result<(), String> {
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        if self.tab_machine(tab).is_some() {
            return Err("this tab already has a machine".into());
        }
        let id = self.meta.get(&pane).and_then(|m| m.host).ok_or("that pane runs on this host")?;
        let m = self.machines.get_mut(&id).ok_or("no such machine")?;
        if m.owner != Owner::Pane(pane) {
            return Err("that pane's machine isn't its own".into());
        }
        m.owner = Owner::Tab(tab);
        info!(pane, tab, machine = id, "machine shared with tab");
        self.changed();
        Ok(())
    }

    /// "Reset machine": delete its sprite, then start every pane on it again
    /// by its policy, on a new one.
    fn reset_machine(&mut self, id: MachineId) -> Result<(), String> {
        let m = self.machines.get_mut(&id).ok_or("no such machine")?;
        if m.borrowed {
            return Err(format!("{} isn't ours to reset", m.sprite));
        }
        m.state = MachineState::Starting;
        let sprite = m.sprite.clone();
        let provider = self.config.provider.clone().ok_or("VM panes aren't set up")?;
        info!(machine = id, sprite, "resetting machine");
        self.emit(None, EventKind::Machine { machine: id, state: MachineState::Starting });
        self.broadcast();
        // Let go first, so its panes don't report the machine gone.
        for (pane, meta) in &self.meta {
            if meta.host == Some(id)
                && let Some(h) = self.panes.get(pane)
            {
                h.release();
            }
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Err(e) = provider.delete(&sprite).await {
                warn!(sprite, error = %e, "can't delete machine to reset it");
            }
            let _ = tx.send(Cmd::MachineReset(id));
        });
        Ok(())
    }

    fn machine_reset(&mut self, id: MachineId) {
        let panes: Vec<PaneId> = self.meta.iter().filter(|(_, m)| m.host == Some(id)).map(|(p, _)| *p).collect();
        for pane in panes {
            let (Some(h), Some(meta)) = (self.panes.get(&pane), self.meta.get(&pane)) else { continue };
            h.restart(self.config.restore(pane, meta), "machine reset");
        }
    }

    /// Delete a machine and everything on it, in the background.
    fn delete_machine(&mut self, id: MachineId) {
        let Some(m) = self.machines.remove(&id) else { return };
        self.emit(None, EventKind::Machine { machine: id, state: MachineState::Gone });
        if m.borrowed {
            // Its shells were hung up as their panes closed.
            return info!(machine = m.id, sprite = m.sprite, "let go of a borrowed machine");
        }
        let Some(provider) = self.config.provider.clone() else { return };
        tokio::spawn(async move {
            match provider.delete(&m.sprite).await {
                Ok(()) => info!(machine = m.id, sprite = m.sprite, "deleted machine"),
                Err(e) => warn!(machine = m.id, sprite = m.sprite, error = %e, "can't delete machine"),
            }
        });
    }

    /// Delete our sprites that no machine owns: left by a crash, or by a
    /// pane closed while the daemon was down.
    fn sweep_machines(&self) {
        let Some(provider) = self.config.provider.clone() else { return };
        let prefix = self.config.sprite_prefix();
        let keep: Vec<String> = self.machines.values().map(|m| m.sprite.clone()).collect();
        tokio::spawn(async move {
            let names = match provider.list(&prefix).await {
                Ok(n) => n.into_iter().map(|s| s.name).collect::<Vec<_>>(),
                Err(e) => return warn!(error = %e, "can't list machines to sweep"),
            };
            for name in names.into_iter().filter(|n| !keep.contains(n)) {
                match provider.delete(&name).await {
                    Ok(()) => info!(sprite = name, "deleted a machine nothing owns"),
                    Err(e) => warn!(sprite = name, error = %e, "can't delete stray machine"),
                }
            }
        });
    }

    fn message(&mut self, client: ClientId, msg: ClientMsg) {
        let Some(sub) = self.clients.get(&client).cloned() else {
            return;
        };
        let who = sub.principal.clone();
        match msg {
            ClientMsg::Attach { panes, zstd, acks, kitty_keys } => {
                for a in panes {
                    if !self.readable(&who, a.pane) {
                        continue;
                    }
                    let floor = self.session_of(a.pane).and_then(|s| self.config.acl.floor(&who, s, a.pane));
                    if let Some(p) = self.panes.get(&a.pane) {
                        p.attach_with(
                            sub.clone(),
                            Want { offset: a.offset, history: a.history, zstd, floor, acks, kitty_keys },
                        );
                    }
                }
            }
            ClientMsg::Ack { pane, offset } => {
                if let Some(p) = self.panes.get(&pane) {
                    p.ack(client, offset);
                }
            }
            ClientMsg::Detach { panes } => {
                for id in panes {
                    if let Some(p) = self.panes.get(&id) {
                        p.detach(client);
                    }
                }
            }
            ClientMsg::View { tab, cols, rows, zoom, claim } => {
                match zoom {
                    Some(z) => self.zoomed.insert(client, z),
                    None => self.zoomed.remove(&client),
                };
                if self.viewing.insert(client, tab) != Some(tab) {
                    // Presence only: no pane changed.
                    self.soon();
                }
                // Only editors size a tab; viewers letterbox.
                let editor = who.is_owner()
                    || self
                        .mux
                        .session_of_tab(tab)
                        .ok()
                        .and_then(|s| self.config.acl.role(&who, s))
                        .is_some_and(|r| r >= Role::Editor);
                if !editor {
                    return;
                }
                if let Ok(true) = self.mux.view(client, tab, cols, rows, zoom, claim) {
                    self.changed();
                }
            }
            ClientMsg::Intent { id, intent } => {
                if !who.is_owner() {
                    let allowed = illogical_core::access::need(&self.mux, &intent)
                        .map(|n| n.allowed(|s| self.config.acl.role(&who, s)))
                        .unwrap_or(false);
                    if !allowed {
                        let message = "you can't do that here (ask the owner for more access)".to_owned();
                        let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id, message }));
                        return;
                    }
                }
                let done = if who.is_owner() {
                    self.intent(Some(client), intent)
                } else {
                    self.guest_intent(client, &who, intent)
                };
                if let Err(message) = done {
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id, message }));
                }
            }
            ClientMsg::Ping { id } => {
                // Whatever it did has been sent before the answer.
                self.flush();
                let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Pong { id }));
            }
            ClientMsg::Subscribe { summary } => {
                if summary {
                    self.summary.insert(client);
                    for p in self.panes.values() {
                        p.detach(client);
                    }
                } else {
                    self.summary.remove(&client);
                }
                let state = self.state_for(&who);
                self.send_state(client, state, summary, false);
                // It leaves (or joins) everyone's presence.
                self.soon();
            }
            ClientMsg::Follow { pane, on } => self.follow(client, pane, on),
            ClientMsg::Focus { pane } => {
                let before = self.focus.get(&client).copied();
                match pane.filter(|p| self.sees(&who, *p)) {
                    Some(p) => {
                        self.focus.insert(client, p);
                        // Seeing a finished command is enough.
                        if self.attention.get(&p) == Some(&Attention::Done) {
                            self.set_attention(p, Attention::Idle, "seen");
                        }
                    }
                    None => {
                        self.focus.remove(&client);
                    }
                }
                if self.focus.get(&client).copied() != before {
                    self.soon();
                }
            }
            ClientMsg::Pane { pane, op } => {
                if let Some(why) = self.cant(client, &[pane], Role::Editor) {
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message: why }));
                    return;
                }
                if self.control_op(client, &who, pane, &op) {
                    return;
                }
                // Blocks of other types take what applies to them.
                if self.blocks.contains_key(&pane) {
                    match op {
                        PaneOp::Attention { state } => self.set_attention(pane, state, "set by a client"),
                        PaneOp::SetPolicy { policy } => {
                            info!(pane, ?policy, "restart policy");
                            self.meta.entry(pane).or_default().policy = policy;
                        }
                        _ => {}
                    }
                    self.changed();
                    return;
                }
                let Some(handle) = self.panes.get(&pane) else {
                    let message = format!("no pane %{pane}");
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
                    return;
                };
                match op {
                    PaneOp::SetPolicy { policy } => {
                        info!(pane, ?policy, "restart policy");
                        self.meta.entry(pane).or_default().policy = policy;
                    }
                    PaneOp::Purge => {
                        info!(pane, "purging history");
                        handle.purge();
                    }
                    PaneOp::SetIntegration { on } => {
                        info!(pane, on, "shell integration");
                        self.meta.entry(pane).or_default().integration = Some(on);
                    }
                    PaneOp::Attention { state } => self.set_attention(pane, state, "set by a client"),
                    // Driving: handled before this.
                    PaneOp::TakeControl
                    | PaneOp::RequestControl
                    | PaneOp::GiveControl { .. }
                    | PaneOp::ReleaseControl
                    | PaneOp::SetPair { .. }
                    | PaneOp::RequestTrust
                    | PaneOp::GrantTrust { .. }
                    | PaneOp::RevokeTrust { .. }
                    | PaneOp::SetPrivate { .. } => {}
                }
                self.changed();
            }
        }
    }

    fn intent(&mut self, client: Option<ClientId>, intent: Intent) -> Result<(), String> {
        // A new session without a name gets one ("drifting cedar").
        let intent = match intent {
            Intent::NewSession { name: None, from_pane } => {
                let taken = |n: &str| self.mux.sessions.iter().any(|s| s.name == n);
                Intent::NewSession { name: Some(illogical_core::names::generate(seed(), taken)), from_pane }
            }
            i => i,
        };
        let refused = |why: String| {
            info!(?client, ?intent, why, "intent refused");
            why
        };
        self.check_move(&intent).map_err(refused)?;
        let local = matches!(intent, Intent::Split { local: true, .. });
        let before: Vec<SessionId> = self.mux.sessions.iter().map(|s| s.id).collect();
        let effects = self.mux.apply(intent.clone()).map_err(|e| refused(e.to_string()))?;
        for gone in before.into_iter().filter(|s| self.mux.session(*s).is_err()) {
            self.config.acl.forget_session(gone);
        }
        if self.config.sandbox_of_control && self.mux.sessions.is_empty() {
            self.config.control.sandbox_done();
        }
        info!(?client, ?intent, "intent");
        let rects = self.mux.pane_rects();
        for e in effects {
            match e {
                Effect::Spawn { pane, cwd_from, cwd } => {
                    let from_meta = cwd_from.and_then(|p| self.meta.get(&p)).and_then(|m| m.integration);
                    let integrate = from_meta.unwrap_or(true);
                    let asked = self.next_cwd.take();
                    // A directory asked for (if it exists), else the source
                    // pane's, else home.
                    let cwd = cwd
                        .map(PathBuf::from)
                        .filter(|d| d.is_dir())
                        .or_else(|| cwd_from.and_then(|p| self.panes.get(&p)?.cwd()))
                        .unwrap_or_else(|| self.config.home.clone());
                    let (cols, rows) = rects.get(&pane).map(|r| (r.cols, r.rows)).unwrap_or((80, 24));
                    // A machine made for it, else its tab's (unless asked
                    // for this host).
                    let tab = self.mux.tab_of(pane).ok();
                    let made = self.next_host.take();
                    let host = made.or_else(|| tab.filter(|_| !local).and_then(|t| self.tab_machine(t)));
                    if let Some((kind, config)) = self.next_block.take() {
                        // A machine made for it is its own.
                        if let Some(m) = made.and_then(|m| self.machines.get_mut(&m))
                            && m.owner == Owner::Pane(0)
                        {
                            m.owner = Owner::Pane(pane);
                        }
                        match self.make_block(pane, kind, config.clone(), host, None) {
                            Ok(()) => {
                                let meta = PaneMeta { host, kind, config: Some(config), ..Default::default() };
                                self.meta.insert(pane, meta);
                                self.emit(Some(pane), EventKind::Opened);
                            }
                            Err(e) => {
                                warn!(block = pane, ?kind, error = %e, "could not start block");
                                self.last_block_error = Some(e);
                                if let Some(m) = made
                                    && self.machines.get(&m).is_some_and(|x| x.owner == Owner::Pane(pane))
                                {
                                    self.machines.remove(&m);
                                }
                                let _ = self.mux.apply(Intent::ClosePane { pane });
                            }
                        }
                        continue;
                    }
                    let (start, hold, cwd) = match self.next_spawn.take() {
                        // A command from `run`: fill in the pane id it gets.
                        Some((mut spawn, Some(text))) => {
                            spawn.env.retain(|(k, _)| k != "ILLOGICAL_PANE" && k != "ILLOGICAL_EXEC");
                            spawn.env.push(("ILLOGICAL_PANE".into(), pane.to_string()));
                            if host.is_some() {
                                spawn.env.push(("ILLOGICAL_EXEC".into(), exec_tag(&self.config.daemon_id, pane)));
                            }
                            let cwd = spawn.cwd.clone();
                            (Start::Run { spawn, text }, true, cwd)
                        }
                        _ => {
                            let cwd = match (host, asked.clone()) {
                                (None, Some(c)) => c,
                                _ => cwd,
                            };
                            let shell = match host {
                                Some(_) => self.config.guest_shell(pane, integrate, asked),
                                None => self.config.shell(pane, cwd.clone(), integrate),
                            };
                            (Start::Now(shell), false, cwd)
                        }
                    };
                    if let Some(m) = made.and_then(|m| self.machines.get_mut(&m)) {
                        m.owner = match (self.next_owner_tab, tab) {
                            (true, Some(t)) => Owner::Tab(t),
                            _ => Owner::Pane(pane),
                        };
                    }
                    match self.open_pane(pane, cols, rows, false, start, cwd, integrate, hold, host) {
                        Ok(()) => {
                            let meta = PaneMeta { integration: from_meta, host, hold, ..Default::default() };
                            self.meta.insert(pane, meta);
                            self.emit(Some(pane), EventKind::Opened);
                        }
                        Err(e) => {
                            warn!(pane, error = %e, "could not start pane");
                            if let Some(m) = made {
                                self.machines.remove(&m);
                            }
                            let _ = self.mux.apply(Intent::ClosePane { pane });
                        }
                    }
                }
                Effect::Kill { pane } => {
                    self.ids.lock().unwrap().remove(&pane);
                    if let Some(p) = self.panes.remove(&pane) {
                        p.close();
                    }
                    if let Some(b) = self.blocks.remove(&pane) {
                        b.close();
                        // Its history is kept like a closed pane's.
                        if let Ok(log) = PaneLog::open(self.store.pane_dir(pane)) {
                            log.retire(pane);
                        }
                    }
                    // A pane's own machine goes with it (a tab's, with the tab).
                    if let Some(m) = self.machine_of(pane).filter(|m| m.owner == Owner::Pane(pane)) {
                        self.delete_machine(m.id);
                    }
                    self.sizes.remove(&pane);
                    self.meta.remove(&pane);
                    self.attention.remove(&pane);
                    self.quiet_armed.remove(&pane);
                    if let Some(a) = self.asks.remove(&pane) {
                        let _ = a.reply.send((AskReply::Withdrawn, None));
                    }
                    self.emit(Some(pane), EventKind::Closed);
                }
            }
        }
        self.reap_machines();
        self.changed();
        self.emit(None, EventKind::Layout { rev: self.mux.rev });
        Ok(())
    }

    /// Resize panes whose cells changed, tell every client, and save soon.
    fn changed(&mut self) {
        for (pane, r) in self.mux.pane_rects() {
            let size = (r.cols, r.rows);
            if self.sizes.get(&pane) != Some(&size)
                && let Some(b) = self.blocks.get(&pane)
            {
                b.resize(r.cols, r.rows);
                self.sizes.insert(pane, size);
            }
            if self.sizes.get(&pane) != Some(&size)
                && let Some(p) = self.panes.get(&pane)
            {
                p.resize(r.cols, r.rows);
                self.sizes.insert(pane, size);
            }
        }
        self.broadcast();
        self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
    }

    /// Something changed: every client hears what, shortly (M23). Layout
    /// changes go as a whole `State`; anything else as a `Delta` of the
    /// fields that changed. Where it's one pane, [`Self::touch`] is cheaper.
    fn broadcast(&mut self) {
        self.dirty = Dirty::All;
        self.soon();
    }

    /// One pane's details changed (attention, a question, its driver).
    fn touch(&mut self, pane: PaneId) {
        self.procs.borrow_mut().remove(&pane);
        self.mark(pane);
        self.soon();
    }

    /// One pane's details changed, but nobody needs it before the next
    /// tick (its directory).
    fn mark(&mut self, pane: PaneId) {
        match &mut self.dirty {
            Dirty::All => {}
            Dirty::Panes(s) => {
                s.insert(pane);
            }
            Dirty::Clean => self.dirty = Dirty::Panes([pane].into()),
        }
    }

    fn soon(&mut self) {
        let at = Instant::now() + URGENT;
        if self.flush_due.is_none_or(|d| d > at) {
            self.flush_due = Some(at);
        }
    }

    /// Work out each pane's output rate from its byte count, without
    /// touching the pane's thread (a parked pane stays parked).
    fn tick_activity(&mut self) {
        let now = Instant::now();
        let secs = now.duration_since(self.last_tick).as_secs_f64().max(0.001);
        self.last_tick = now;
        let mut changed = vec![];
        for (id, h) in &self.panes {
            let (end, last_ms) = h.output_seen();
            let (prev, old) = self.activity.get(id).copied().unwrap_or((end, Activity::default()));
            let bps = (end.saturating_sub(prev) as f64 / secs).round().min(u32::MAX as f64) as u32;
            let next = Activity { bps, last_ms };
            self.activity.insert(*id, (end, next));
            if next != old {
                changed.push(*id);
            }
        }
        self.activity.retain(|id, _| self.panes.contains_key(id));
        for id in changed {
            self.mark(id);
        }
    }

    /// `who` drives `pane` from now (M13); they haven't typed in it yet.
    fn drive(&mut self, pane: PaneId, who: Driver) -> Option<Driver> {
        self.drove.insert(pane, Instant::now());
        self.typing.remove(&pane);
        self.drivers.insert(pane, who)
    }

    /// Its driver typed in `pane` (#118).
    fn typed(&mut self, pane: PaneId) {
        self.drove.insert(pane, Instant::now());
        if self.typing.insert(pane) {
            self.touch(pane);
        }
    }

    /// Typing stops showing a few seconds after the last keystroke, and a
    /// driver who has stopped typing for long lets go (#118).
    fn tick_drivers(&mut self) {
        let now = Instant::now();
        self.drove.retain(|p, _| self.drivers.contains_key(p));
        self.typing.retain(|p| self.drivers.contains_key(p));
        let panes: Vec<PaneId> = self.drivers.keys().copied().collect();
        for pane in panes {
            let idle = now.duration_since(*self.drove.entry(pane).or_insert(now));
            if idle >= self.lapse {
                if let Some(d) = self.drivers.remove(&pane) {
                    info!(pane, who = d.who, "stopped driving: no typing for {}s", idle.as_secs());
                }
                self.drove.remove(&pane);
                self.typing.remove(&pane);
                self.touch(pane);
            } else if idle >= TYPING && self.typing.remove(&pane) {
                self.mark(pane);
            }
        }
    }

    /// Send every client what changed since it was last sent anything.
    fn flush(&mut self) {
        self.flush_due = None;
        let dirty = std::mem::take(&mut self.dirty);
        let full = std::mem::take(&mut self.full);
        if self.clients.is_empty() {
            return;
        }
        let ids: Vec<PaneId> = match dirty {
            Dirty::Clean => vec![],
            Dirty::Panes(s) => s.into_iter().collect(),
            Dirty::All => self.panes.keys().chain(self.blocks.keys()).copied().collect(),
        };
        let infos: HashMap<PaneId, PaneInfo> =
            ids.iter().filter_map(|id| Some((*id, self.info_of_any(*id)?))).collect();
        let clients: Vec<(ClientId, Principal)> =
            self.clients.values().map(|c| (c.client, c.principal.clone())).collect();
        // Most clients are one person's: work each view out once.
        let mut states: HashMap<Principal, State> = HashMap::new();
        let mut views: HashMap<(Principal, bool), PaneView> = HashMap::new();
        let mut people: HashMap<Principal, (Vec<Machine>, Vec<Presence>)> = HashMap::new();
        for (client, who) in clients {
            let summary = self.summary.contains(&client);
            let stale = full || self.sent.get(&client).is_none_or(|s| s.rev != self.mux.rev);
            if stale {
                let state = states.entry(who.clone()).or_insert_with(|| self.state_for(&who)).clone();
                self.send_state(client, state, summary, false);
                continue;
            }
            let view = views.entry((who.clone(), summary)).or_insert_with(|| {
                ids.iter()
                    .map(|id| {
                        let v = infos
                            .get(id)
                            .filter(|_| self.sees(&who, *id))
                            .map(|i| self.pane_value(&who, i.clone(), summary));
                        (*id, v)
                    })
                    .collect()
            });
            let (machines, presence) =
                people.entry(who.clone()).or_insert_with(|| (self.machines_for(&who), self.presence(&who)));
            let Some(sent) = self.sent.get_mut(&client) else { continue };
            let mut delta = Delta::default();
            for (id, v) in view.iter() {
                match v {
                    Some(new) => {
                        let old = sent.panes.get(id);
                        let mut patch = serde_json::Map::new();
                        for (k, val) in new {
                            if old.and_then(|o| o.get(k)) != Some(val) {
                                patch.insert(k.clone(), val.clone());
                            }
                        }
                        for k in old.into_iter().flat_map(|o| o.keys()) {
                            if !new.contains_key(k) {
                                patch.insert(k.clone(), serde_json::Value::Null);
                            }
                        }
                        if !patch.is_empty() {
                            patch.insert("id".into(), (*id).into());
                            delta.panes.push(patch);
                            sent.panes.insert(*id, new.clone());
                        }
                    }
                    None => {
                        if sent.panes.remove(id).is_some() {
                            delta.gone.push(*id);
                        }
                    }
                }
            }
            if sent.machines != *machines {
                sent.machines = machines.clone();
                delta.machines = Some(machines.clone());
            }
            if sent.presence != *presence {
                sent.presence = presence.clone();
                delta.presence = Some(presence.clone());
            }
            if !delta.is_empty()
                && let Some(c) = self.clients.get(&client)
            {
                let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Delta { delta }));
            }
        }
    }

    /// A whole `State` for one client (and what it now has), or its
    /// `Hello`.
    fn send_state(&mut self, client: ClientId, state: State, summary: bool, hello: bool) {
        let Some(c) = self.clients.get(&client) else { return };
        let who = c.principal.clone();
        let mut sent = Sent {
            rev: state.rev,
            panes: HashMap::new(),
            machines: state.machines.clone(),
            presence: state.presence.clone(),
        };
        let mut panes = Vec::with_capacity(state.panes.len());
        for p in &state.panes {
            let v = self.pane_value(&who, p.clone(), summary);
            sent.panes.insert(p.id, v.clone());
            panes.push(serde_json::Value::Object(v));
        }
        let msg = if hello {
            ServerMsg::Hello { version: env!("CARGO_PKG_VERSION").into(), client, state }
        } else {
            ServerMsg::State { state }
        };
        // Panes as this client has them (private ones blanked, summaries
        // trimmed): the same objects deltas will be worked out against.
        let mut json = serde_json::to_value(&msg).unwrap_or_default();
        json["state"]["panes"] = serde_json::Value::Array(panes);
        let _ = c.ctrl.send(ToClient::Json(json.to_string()));
        self.sent.insert(client, sent);
    }

    /// A pane as `who` gets it: someone else's private pane shows only that
    /// it's there and private (M14); a summary leaves out what the swarm
    /// doesn't need.
    fn pane_value(
        &self,
        who: &Principal,
        mut info: PaneInfo,
        summary: bool,
    ) -> serde_json::Map<String, serde_json::Value> {
        if info.private && !who.is_owner() {
            info = PaneInfo {
                cwd: None,
                command: None,
                current: None,
                last: None,
                ask: None,
                reason: None,
                answered: None,
                work: None,
                project: None,
                activity: None,
                title: None,
                file: None,
                editor: None,
                diff: None,
                ..info
            };
        }
        let serde_json::Value::Object(mut m) = serde_json::to_value(&info).unwrap_or_default() else {
            return Default::default();
        };
        if summary {
            for k in NOT_IN_SUMMARIES {
                m.remove(*k);
            }
        }
        m
    }

    /// The machines `who` sees: those their panes run on.
    fn machines_for(&self, who: &Principal) -> Vec<Machine> {
        if who.is_owner() {
            return self.machines.values().cloned().collect();
        }
        let hosts: std::collections::HashSet<MachineId> = self
            .mux
            .sessions
            .iter()
            .filter(|s| self.config.acl.role(who, s.id).is_some())
            .flat_map(|s| &s.tabs)
            .filter_map(|t| self.mux.tab(*t).ok())
            .flat_map(|t| t.root.panes())
            .filter_map(|p| self.meta.get(&p).and_then(|m| m.host))
            .collect();
        self.machines.values().filter(|m| hosts.contains(&m.id)).cloned().collect()
    }

    // ---- who may see and do what (M12)

    /// The session a pane or block is in.
    fn session_of(&self, pane: PaneId) -> Option<SessionId> {
        self.mux.tab_of(pane).and_then(|t| self.mux.session_of_tab(t)).ok()
    }

    fn sees(&self, who: &Principal, pane: PaneId) -> bool {
        who.is_owner()
            || match self.session_of(pane) {
                Some(s) => self.config.acl.role(who, s).is_some(),
                None => self.is_presence(pane) && self.presence_role(who).is_some(),
            }
    }

    /// An editor that joined the swarm (M28): in no session.
    fn is_presence(&self, pane: PaneId) -> bool {
        self.blocks.get(&pane).is_some_and(|b| b.detached())
    }

    /// Someone's role on the editors that joined this daemon: the owner's
    /// editors are theirs; a team's members have their team role (M19).
    fn presence_role(&self, who: &Principal) -> Option<Role> {
        if who.is_owner() { Some(Role::Owner) } else { self.config.acl.team_role(who) }
    }

    /// Follow an editor (M28), or stop (`pane: None`: every one).
    fn follow(&mut self, client: ClientId, pane: PaneId, on: bool) {
        if !on {
            return self.unfollow(client, Some(pane));
        }
        let Some(link) = self.blocks.get(&pane).and_then(|b| b.link()) else {
            return self.tell_once(client, format!("%{pane} isn't an editor that can be followed"));
        };
        let Some(sub) = self.clients.get(&client) else { return };
        if !self.readable(&sub.principal, pane) {
            return self.tell_once(client, format!("no pane %{pane}"));
        }
        // What it shows now, then what it sends from here.
        for msg in link.snapshot() {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Follow { pane, msg }));
        }
        let set = self.follows.entry(pane).or_default();
        if set.insert(client) {
            info!(pane, client, "following");
            link.followers(set.len() as u32, true);
        }
    }

    fn unfollow(&mut self, client: ClientId, pane: Option<PaneId>) {
        let ids: Vec<PaneId> = match pane {
            Some(p) => vec![p],
            None => self.follows.keys().copied().collect(),
        };
        for id in ids {
            let Some(set) = self.follows.get_mut(&id) else { continue };
            if !set.remove(&client) {
                continue;
            }
            let n = set.len() as u32;
            if n == 0 {
                self.follows.remove(&id);
            }
            if let Some(link) = self.blocks.get(&id).and_then(|b| b.link()) {
                link.followers(n, false);
            }
        }
    }

    /// Why `client` can't act on `panes` with `role` (`None`: it can).
    fn cant(&self, client: ClientId, panes: &[PaneId], role: Role) -> Option<String> {
        let who = &self.clients.get(&client)?.principal;
        if who.is_owner() {
            return None;
        }
        for p in panes {
            let got = match self.session_of(*p) {
                Some(s) => self.config.acl.role(who, s),
                None if self.is_presence(*p) => self.presence_role(who),
                None => None,
            };
            match got {
                Some(r) if r >= role => {}
                Some(_) => return Some("you're watching this session; you can't type or change it".into()),
                None => return Some(format!("no pane %{p}")),
            }
        }
        None
    }

    fn name_of(&self, who: &Principal) -> String {
        match who {
            Principal::Owner => self.config.owner_name.clone(),
            Principal::User { name, .. } => name.clone(),
        }
    }

    fn driver_of(&self, who: &Principal) -> Driver {
        Driver { who: who.id().to_owned(), name: self.name_of(who) }
    }

    /// A connected client's person, by the name it came with if it has one
    /// (an owner through control, M30).
    fn driver_for(&self, client: ClientId) -> Driver {
        match self.clients.get(&client) {
            Some(c) => Driver {
                who: c.principal.id().to_owned(),
                name: c.name.clone().unwrap_or_else(|| self.name_of(&c.principal)),
            },
            None => self.driver_of(&Principal::Owner),
        }
    }

    fn tell(&self, who: &str, msg: ServerMsg) {
        for c in self.clients.values().filter(|c| c.principal.id() == who) {
            let _ = c.ctrl.send(ToClient::Msg(msg.clone()));
        }
    }

    /// Driving a pane (M13). True if `op` was one of these.
    fn control_op(&mut self, client: ClientId, who: &Principal, pane: PaneId, op: &PaneOp) -> bool {
        let me = self.driver_for(client);
        let title = format!("%{pane}");
        match op {
            PaneOp::TakeControl => {
                if let Some(prev) = self.drive(pane, me.clone())
                    && prev.who != me.who
                {
                    let message = format!("{} took control of {title}", me.name);
                    self.tell(&prev.who, ServerMsg::Notice { message });
                }
                info!(pane, who = me.who, "took control");
            }
            PaneOp::RequestControl => match self.drivers.get(&pane).cloned() {
                Some(d) if d.who != me.who && self.clients.values().any(|c| c.principal.id() == d.who) => {
                    self.tell(&d.who, ServerMsg::ControlRequest { pane, who: me.who.clone(), name: me.name.clone() });
                    if let Some(c) = self.clients.get(&client) {
                        let message = format!("asked {} for control of {title}", d.name);
                        let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Notice { message }));
                    }
                    return true;
                }
                // Nobody (here) drives it: just take it.
                _ => {
                    self.drive(pane, me);
                }
            },
            PaneOp::GiveControl { to } => {
                let current = self.drivers.get(&pane).map(|d| d.who.clone());
                if current.as_deref().is_some_and(|c| c != me.who) && !who.is_owner() {
                    if let Some(c) = self.clients.get(&client) {
                        let message = "only whoever drives it can hand it over".to_owned();
                        let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
                    }
                    return true;
                }
                let Some(to_client) = self.clients.values().find(|c| c.principal.id() == to).map(|c| c.client) else {
                    return true;
                };
                let next = self.driver_for(to_client);
                let message = format!("{} handed you control of {title}", me.name);
                self.drive(pane, next);
                self.tell(to, ServerMsg::Notice { message });
            }
            PaneOp::ReleaseControl => {
                if self.drivers.get(&pane).is_some_and(|d| d.who == me.who) {
                    self.drivers.remove(&pane);
                }
            }
            PaneOp::SetPair { on } => {
                if *on {
                    self.pair.insert(pane);
                } else {
                    self.pair.remove(&pane);
                }
            }
            PaneOp::RequestTrust => {
                let msg = ServerMsg::TrustRequest { pane, who: me.who.clone(), name: me.name.clone() };
                self.tell("owner", msg);
                // The owner may only have a phone in their pocket.
                if let Some(push) = &self.push {
                    let body = format!("{} asks to drive %{pane}, which runs on this machine", me.name);
                    push.send(
                        pane,
                        "Someone asks to drive a pane",
                        &body,
                        Some(serde_json::json!({ "trust": me.who })),
                    );
                }
                let body = format!("{} asks to drive %{pane}, which runs on this machine", me.name);
                self.config.control.push(pane, "Someone asks to drive a pane", &body, None, |who| who.is_owner());
                if let Some(c) = self.clients.get(&client) {
                    let message = "asked the owner to trust you with it".to_owned();
                    let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Notice { message }));
                }
                return true;
            }
            PaneOp::GrantTrust { .. } | PaneOp::RevokeTrust { .. } if !who.is_owner() => {
                self.refuse_to(client, "only the owner trusts people with panes on this machine");
                return true;
            }
            PaneOp::GrantTrust { to, minutes } => {
                let minutes = (*minutes).clamp(1, 24 * 60);
                let until = now_ms() + u64::from(minutes) * 60_000;
                self.trust.insert((pane, to.clone()), until);
                info!(pane, to, minutes, "trusted with a local pane");
                let message = format!("you may drive %{pane} for {minutes} minutes");
                self.tell(to, ServerMsg::Notice { message });
                let expire = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(u64::from(minutes) * 60_000 + 50)).await;
                    // Show everyone it ended.
                    let _ = expire.send(Cmd::AclChanged);
                });
            }
            PaneOp::RevokeTrust { to } => {
                self.trust.remove(&(pane, to.clone()));
                if self.drivers.get(&pane).is_some_and(|d| &d.who == to) {
                    self.drivers.remove(&pane);
                }
            }
            PaneOp::SetPrivate { on } => {
                if !who.is_owner() {
                    self.refuse_to(client, "only the owner makes a pane private");
                    return true;
                }
                self.meta.entry(pane).or_default().private = *on;
                if *on {
                    // Whoever else watches it stops getting it.
                    let others: Vec<ClientId> =
                        self.clients.values().filter(|c| !c.principal.is_owner()).map(|c| c.client).collect();
                    if let Some(p) = self.panes.get(&pane) {
                        for c in others {
                            p.detach(c);
                        }
                    }
                }
                self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
            }
            _ => return false,
        }
        self.broadcast();
        true
    }

    fn refuse_to(&self, client: ClientId, why: &str) {
        if let Some(c) = self.clients.get(&client) {
            let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message: why.to_owned() }));
        }
    }

    /// A guest may drive a pane on a machine of its own; one on this
    /// machine only while the owner trusts them with it (M14).
    fn may_drive_here(&self, who: &Principal, pane: PaneId) -> Result<(), String> {
        if who.is_owner() || self.machine_of(pane).is_some() || self.blocks.contains_key(&pane) {
            return Ok(());
        }
        // A team's machine is the team's: its members drive it by their
        // team role, no one person's trust needed (M19).
        if self.config.acl.team_role(who).is_some_and(|r| r >= Role::Editor) {
            return Ok(());
        }
        let until = self.trust.get(&(pane, who.id().to_owned())).copied().unwrap_or(0);
        if until > now_ms() {
            return Ok(());
        }
        Err(format!(
            "%{pane} runs on {}'s own machine: ask them to trust you with it (pane menu), or work in a VM tab",
            self.config.owner_name
        ))
    }

    /// Someone other than the owner may read this pane (not private).
    fn readable(&self, who: &Principal, pane: PaneId) -> bool {
        who.is_owner() || (self.sees(who, pane) && !self.meta.get(&pane).is_some_and(|m| m.private))
    }

    /// A guest's intent that makes a pane: on a VM, never this machine
    /// (M14). A new tab is a VM tab; a split joins its tab's machine, or
    /// gets one of its own. Their VMs count against their quota.
    fn guest_intent(&mut self, client: ClientId, who: &Principal, intent: Intent) -> Result<(), String> {
        let vm = match &intent {
            Intent::NewTab { .. } => Some(true),
            Intent::Split { pane, .. } => {
                let tab = self.mux.tab_of(*pane).map_err(|e| e.to_string())?;
                if self.tab_machine(tab).is_some() { None } else { Some(false) }
            }
            _ => None,
        };
        let intent = match intent {
            Intent::Split { pane, edge, cwd, .. } => Intent::Split { pane, edge, local: false, cwd },
            i => i,
        };
        let Some(tab) = vm else { return self.intent(Some(client), intent) };
        let mine = self.machines.values().filter(|m| m.by.as_deref() == Some(who.id())).count();
        if mine >= self.config.guest_machines {
            return Err(format!("you have {mine} VMs here, the most a guest may have: close one first",));
        }
        let m = self.new_machine(None)?;
        if let Some(machine) = self.machines.get_mut(&m) {
            machine.by = Some(who.id().to_owned());
        }
        self.next_host = Some(m);
        self.next_owner_tab = tab;
        let r = self.intent(Some(client), intent);
        self.next_owner_tab = false;
        if let Some(m) = self.next_host.take() {
            self.machines.remove(&m);
        }
        r
    }

    /// Who is connected and where they look, within what `viewer` sees.
    fn presence(&self, viewer: &Principal) -> Vec<Presence> {
        let mut out: Vec<Presence> = self
            .clients
            .values()
            // A summaries-only connection (the fleet, M25) looks at nothing.
            .filter(|c| !self.summary.contains(&c.client))
            .map(|c| Presence {
                client: c.client,
                who: c.principal.id().to_owned(),
                name: c.name.clone().unwrap_or_else(|| self.name_of(&c.principal)),
                pic: match &c.principal {
                    Principal::User { pic, .. } => pic.clone(),
                    Principal::Owner => self.config.owner_pic.clone(),
                },
                tab: self.viewing.get(&c.client).copied(),
                pane: self.focus.get(&c.client).copied(),
            })
            .filter(|p| {
                viewer.is_owner()
                    || p.who == viewer.id()
                    || p.tab
                        .and_then(|t| self.mux.session_of_tab(t).ok())
                        .and_then(|s| self.config.acl.role(viewer, s))
                        .is_some()
            })
            .map(|mut p| {
                // Only where the viewer can see.
                if !viewer.is_owner() {
                    p.pane = p.pane.filter(|x| self.sees(viewer, *x));
                    p.tab = p.tab.filter(|t| {
                        self.mux.session_of_tab(*t).ok().and_then(|s| self.config.acl.role(viewer, s)).is_some()
                    });
                }
                p
            })
            .collect();
        out.sort_by_key(|p| p.client);
        out
    }

    fn tell_once(&mut self, client: ClientId, message: String) {
        let now = Instant::now();
        if self.refused.get(&client).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(5)) {
            return;
        }
        self.refused.insert(client, now);
        if let Some(sub) = self.clients.get(&client) {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
        }
    }

    /// Grants changed: hang up on whoever has nothing left, let go of panes
    /// they no longer see, and show everyone their state.
    fn acl_changed(&mut self) {
        let acl = self.config.acl.clone();
        let gone: Vec<ClientId> =
            self.clients.iter().filter(|(_, c)| !acl.knows(&c.principal)).map(|(id, _)| *id).collect();
        for id in gone {
            if let Some(sub) = self.clients.remove(&id) {
                info!(client = id, who = sub.principal.id(), "access revoked: disconnecting");
                let message = "your access was removed".to_owned();
                let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
                let _ = sub.ctrl.send(ToClient::Close);
            }
            self.focus.remove(&id);
            for p in self.panes.values() {
                p.detach(id);
            }
            self.mux.release(id);
        }
        let hidden: Vec<(ClientId, PaneId)> = self
            .clients
            .values()
            .flat_map(|c| self.panes.keys().filter(|p| !self.sees(&c.principal, **p)).map(|p| (c.client, *p)))
            .collect();
        for (client, pane) in hidden {
            if let Some(p) = self.panes.get(&pane) {
                p.detach(client);
            }
        }
        // Sessions and roles changed with them: everyone starts over.
        self.full = true;
        self.broadcast();
    }

    /// What `who` sees: everything for the owner; for anyone else only the
    /// sessions granted to them, their tabs, panes and machines, and their
    /// role in each.
    fn state_for(&self, who: &Principal) -> State {
        let mut st = self.state();
        st.presence = self.presence(who);
        if who.is_owner() {
            return st;
        }
        // A grant per session, or a team role on all of them (M19).
        let roles: BTreeMap<SessionId, Role> =
            st.sessions.iter().filter_map(|s| Some((s.id, self.config.acl.role(who, s.id)?))).collect();
        st.sessions.retain(|s| roles.contains_key(&s.id));
        let tabs: std::collections::HashSet<TabId> = st.sessions.iter().flat_map(|s| s.tabs.iter().copied()).collect();
        st.tabs.retain(|t| tabs.contains(&t.id));
        let panes: std::collections::HashSet<PaneId> = st.tabs.iter().flat_map(|t| t.root.panes()).collect();
        // Editors that joined (M28) are in no tab: by team role.
        let editors = self.presence_role(who).is_some();
        st.panes.retain(|p| panes.contains(&p.id) || (editors && self.is_presence(p.id)));
        let machines: std::collections::HashSet<MachineId> = st.panes.iter().filter_map(|p| p.host).collect();
        st.machines.retain(|m| machines.contains(&m.id));
        st.roles = Some(roles.into_iter().collect());
        st
    }

    /// Note each running pane's directory and the command a re-run would
    /// run, and mark the panes where either changed.
    fn refresh_meta(&mut self) {
        let mut changed = vec![];
        for (id, h) in &self.panes {
            if !h.running() {
                continue;
            }
            let status = h.status();
            let m = self.meta.entry(*id).or_default();
            let cwd = status.cwd.clone().or_else(|| h.cwd().map(|c| c.display().to_string())).or(m.cwd.clone());
            // The command line as typed, when the shell integration reported
            // it; otherwise what /proc says is in the foreground.
            let fg = h.command();
            let command = match status.current {
                Some(c) => c.text.or_else(|| fg.clone()),
                None => fg.clone(),
            };
            // What clients were told runs there (and so its kind), too.
            let told = self.procs.borrow().get(id).map(|s| s.command.clone());
            if m.cwd != cwd || m.command != command || told.is_some_and(|t| t != fg) {
                changed.push(*id);
            }
            (m.cwd, m.command) = (cwd, command);
        }
        for id in changed {
            self.procs.borrow_mut().remove(&id);
            self.mark(id);
        }
    }

    /// Write the layout and pane details if anything changed since the last
    /// write.
    fn save(&mut self) {
        // Whichever notices a new directory or command tells the clients
        // (at the next tick).
        self.refresh_meta();
        for (id, b) in &self.blocks {
            if let Some(m) = self.meta.get_mut(id) {
                m.config = Some(b.config());
            }
        }
        let panes: BTreeMap<PaneId, PaneMeta> = self.meta.iter().map(|(k, v)| (*k, v.clone())).collect();
        if self.last_saved.as_ref().is_some_and(|(m, p, ms)| *m == self.mux && *p == panes && *ms == self.machines) {
            return;
        }
        let saved = Saved {
            version: LAYOUT_VERSION,
            saved_at_ms: now_ms(),
            mux: self.mux.clone(),
            panes,
            machines: self.machines.clone(),
            next_machine: self.next_machine,
        };
        match self.store.save_layout(&saved) {
            Ok(()) => self.last_saved = Some((saved.mux, saved.panes, saved.machines)),
            Err(e) => warn!(error = %e, "can't save layout"),
        }
    }

    /// Before the daemon exits: every pane's terminal and the layout to disk.
    /// Exits from here on (panes hung up by our own exit) change nothing.
    fn shutdown(&mut self) {
        self.shutting_down = true;
        for p in self.panes.values() {
            p.checkpoint(Duration::from_secs(3));
        }
        self.last_saved = None;
        self.save();
        info!(panes = self.panes.len(), "saved for shutdown");
    }

    /// A pane's directory and foreground command from the OS, read at most
    /// once a second (or again after something happened in it).
    fn proc_seen(&self, p: &PaneHandle) -> ProcSeen {
        if let Some(seen) = self.procs.borrow().get(&p.id).filter(|s| s.at.elapsed() < PROC_FRESH) {
            return seen.clone();
        }
        let command = p.command();
        let work = match &command {
            Some(c) => crate::classify::kind(c),
            None => p.own_command().map_or(WorkKind::Shell, |c| crate::classify::kind(&c)),
        };
        let seen = ProcSeen { at: Instant::now(), cwd: p.cwd().map(|c| c.display().to_string()), command, work };
        self.procs.borrow_mut().insert(p.id, seen.clone());
        seen
    }

    fn pane_info(&self, p: &PaneHandle) -> PaneInfo {
        let meta = self.meta.get(&p.id).cloned().unwrap_or_default();
        let running = p.running();
        let status = p.status();
        let seen = if running { Some(self.proc_seen(p)) } else { None };
        let cwd = status.cwd.clone().or_else(|| seen.as_ref().and_then(|s| s.cwd.clone())).or(meta.cwd);
        // The process's own command line sees through aliases; the typed
        // text is next best (a command that hasn't started its process yet).
        let typed = status.current.as_ref().and_then(|c| c.text.as_deref()).map(crate::classify::kind);
        let work = match &seen {
            Some(s) if s.command.is_some() => s.work,
            _ => typed.or(seen.as_ref().map(|s| s.work)).unwrap_or(WorkKind::Shell),
        };
        let command = if running { seen.and_then(|s| s.command) } else { meta.command };
        PaneInfo {
            id: p.id,
            epoch: p.epoch,
            project: cwd.as_deref().and_then(crate::classify::project),
            cwd,
            command,
            work: Some(work),
            activity: self.activity.get(&p.id).map(|(_, a)| *a).filter(|a| a.last_ms > 0),
            title: status.title.clone(),
            file: None,
            editor: None,
            diff: self.diffs.iter().find(|d| d.pane == Some(p.id)).map(|d| d.info.clone()),
            claude_ide: self.ide_conns.values().any(|c| c.1 == Some(p.id)),
            started_by: meta.started_by.clone(),
            running,
            policy: meta.policy,
            current: status.current.map(info_of),
            last: status.last.map(info_of),
            attention: self.attention.get(&p.id).copied().unwrap_or_default(),
            reason: self.live_reason(p.id),
            integration: meta.integration.unwrap_or(true),
            kind: BlockType::Terminal,
            host: meta.host,
            ask: self.asks.get(&p.id).map(|a| a.ask.clone()),
            answered: self.answered.get(&p.id).cloned(),
            inbox: self.inbox.contains_key(&p.id),
            driver: self.drivers.get(&p.id).cloned(),
            typing: self.typing.contains(&p.id),
            pair: self.pair.contains(&p.id),
            private: meta.private,
            trusted: {
                let now = now_ms();
                let mut t: Vec<(String, u64)> = self
                    .trust
                    .iter()
                    .filter(|((pane, _), until)| *pane == p.id && **until > now)
                    .map(|((_, w), u)| (w.clone(), *u))
                    .collect();
                t.sort();
                t
            },
        }
    }

    /// A non-terminal block's entry in the state.
    fn block_info(&self, id: PaneId, b: &Arc<dyn Block>) -> PaneInfo {
        let meta = self.meta.get(&id).cloned().unwrap_or_default();
        let s = b.summary();
        PaneInfo {
            id,
            epoch: 0,
            project: s.project,
            cwd: s.cwd,
            file: s.file,
            command: None,
            running: true,
            policy: meta.policy,
            current: None,
            last: None,
            attention: self.attention.get(&id).copied().unwrap_or_default(),
            reason: self.live_reason(id),
            integration: false,
            kind: b.kind(),
            host: meta.host,
            // A question raised on it (M35), as a terminal's is drawn.
            ask: self.asks.get(&id).map(|a| a.ask.clone()),
            answered: self.answered.get(&id).cloned(),
            inbox: false,
            driver: None,
            typing: false,
            pair: false,
            private: meta.private,
            trusted: Vec::new(),
            work: s.work.or(match b.kind() {
                BlockType::Agent => Some(WorkKind::Agent),
                BlockType::App => Some(WorkKind::App),
                BlockType::Forge => Some(WorkKind::Pr),
                BlockType::Fountain => Some(WorkKind::Fountain),
                _ => None,
            }),
            activity: None,
            title: s.title,
            started_by: meta.started_by.clone(),
            editor: s.editor,
            diff: None,
            claude_ide: false,
        }
    }

    fn info_of_any(&self, id: PaneId) -> Option<PaneInfo> {
        match (self.panes.get(&id), self.blocks.get(&id)) {
            (Some(h), _) => Some(self.pane_info(h)),
            (_, Some(b)) => Some(self.block_info(id, b)),
            _ => None,
        }
    }

    fn summaries(&self) -> Vec<PaneSummary> {
        let mut out = Vec::new();
        for s in &self.mux.sessions {
            for tab in &s.tabs {
                let Ok(t) = self.mux.tab(*tab) else { continue };
                for pane in t.root.panes() {
                    let Some(info) = self.info_of_any(pane) else { continue };
                    out.push(PaneSummary {
                        session: s.id,
                        session_name: s.name.clone(),
                        tab: t.id,
                        tab_name: t.name.clone(),
                        info,
                    });
                }
            }
        }
        out
    }

    fn state(&self) -> State {
        let tabs = self
            .mux
            .sessions
            .iter()
            .flat_map(|s| &s.tabs)
            .filter_map(|id| {
                let t = self.mux.tab(*id).ok()?;
                Some(TabView {
                    id: t.id,
                    name: t.name.clone(),
                    root: t.root.clone(),
                    cols: t.cols,
                    rows: t.rows,
                    owner: t.owner,
                    zoom: t.zoom,
                    layout: self.mux.layout(t.id).ok()?,
                })
            })
            .collect();
        let mut panes: Vec<PaneInfo> = self.panes.values().map(|p| self.pane_info(p)).collect();
        panes.extend(self.blocks.iter().map(|(id, b)| self.block_info(*id, b)));
        panes.sort_by_key(|p| p.id);
        let machines = self.machines.values().cloned().collect();
        let options = Box::new(self.mux.options.clone());
        State {
            rev: self.mux.rev,
            sessions: self.mux.sessions.clone(),
            tabs,
            panes,
            machines,
            options,
            roles: None,
            presence: Vec::new(),
        }
    }
}
