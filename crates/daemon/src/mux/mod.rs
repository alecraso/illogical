//! The multiplexer task: owns the layout (`illogical_core::Mux`), the panes,
//! the connected clients and what's saved to disk. Every client message, API
//! call and pane notice goes through here, so layout changes, pane starts and
//! stops, resizes, attention and saves happen in one order.

mod api_calls;
mod attention;
mod blocks;
mod call_ops;
mod clients;
mod config;
mod info;
mod machines;
mod thread_ops;
mod who_may;

pub use config::Config;
pub use thread_ops::{Posted, ThreadError, ThreadPost};

use self::attention::{PendingDiff, TermAsk, Waiter};
use crate::{
    block::{Block, BlockCtx},
    osc::Signal,
    pane::{self, ExecRecord, Kept, Notice, NoticeSink, PaneHandle, Setup, Spawn, Start, Subscriber, ToClient, What},
    provider::Provider,
    push::Push,
    store::{PaneLog, PaneMeta, Saved, StateDir, now_ms},
};
use illogical_core::{Intent, Mux, Role};
use illogical_proto::{
    Action, Activity, Attention, BlockType, Call, ClientId, ClientMsg, Driver, Event, EventKind, Machine, MachineId,
    MachineState, Owner, PaneId, Policy, Presence, Reason, ReasonKind, ServerMsg, SessionId, TabId, ThreadMsg,
    ThreadSummary, ThreadTarget, WorkKind,
    api::{OpenRequest, PaneSummary, RunRequest},
    ask::Ask,
};
use illogical_vt::detect::AgentState;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    time::{Instant, sleep_until},
};
use tracing::{info, warn};

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
    /// A thread's messages (M61), as `who` may read them.
    ThreadGet(ThreadTarget, crate::acl::Principal, oneshot::Sender<Result<Vec<ThreadMsg>, ThreadError>>),
    /// Post in a thread as `who`. `as_agent`: an agent posts through MCP
    /// under that name. Answers with the message, and whether it goes to
    /// the pane's agent as a follow-up.
    ThreadPost(ThreadPost, oneshot::Sender<Result<Posted, ThreadError>>),
    /// `who` has read a thread up to a message.
    ThreadRead(ThreadTarget, crate::acl::Principal, u64),
    /// Which of these (principal ids) read a thread now (#297); `None` if
    /// no grant could open it (gone, or a private pane's).
    CanRead(ThreadTarget, Vec<String>, oneshot::Sender<Option<Vec<bool>>>),
    /// A thread an invite opens at (#297): its session, and when a message
    /// of it was posted (`None`: no such message there). `Err`: no grant
    /// could open the thread.
    ThreadPlace(ThreadTarget, Option<u64>, oneshot::Sender<Result<(SessionId, Option<u64>), String>>),
    /// Where an invite to a session opens (#233): the pane given, if it's
    /// in the session, else the session's first; and the session's name.
    InviteTo(SessionId, Option<PaneId>, oneshot::Sender<Result<(PaneId, String), String>>),
    /// Trust someone (by principal id) with a pane on this machine for so
    /// many minutes (#233: an invite's `drive_minutes`), as the owner's
    /// pane menu does (M14). Nothing on a VM, a block or a team's machine
    /// (false).
    Trust(PaneId, String, u32, oneshot::Sender<bool>),
    /// A notice for whoever (by principal id) is connected.
    Tell(String, String),
    /// Where each pane of a session's output ends now (a "from now" share
    /// starts there).
    SessionEnds(SessionId, oneshot::Sender<Option<BTreeMap<PaneId, u64>>>),
    /// An MCP client started this pane or block (M16): shown on it, and
    /// what lets an agent block's token drive it.
    StartedBy(PaneId, illogical_proto::StartedBy),
    /// The guest (principal id) behind a pane or block, if any: who
    /// started it, or the agent that did, or whose VM it runs on.
    GuestBehind(PaneId, oneshot::Sender<Option<String>>),
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
    /// An ssh guest with a read-write invite typed (M65). Refused while
    /// someone else drives; the first keys take the pane, and its size, as
    /// `illogical attach` does.
    GuestInput {
        pane: PaneId,
        client: ClientId,
        by: Driver,
        data: Vec<u8>,
        size: (u16, u16),
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Their window changed: the pane follows if they drive it.
    GuestSize {
        pane: PaneId,
        client: ClientId,
        who: String,
        size: (u16, u16),
    },
    /// They left: they drive nothing and size nothing.
    GuestLeft {
        client: ClientId,
        who: String,
    },
}

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
    /// Standing permission rules for agent blocks (#166).
    pub rules: Arc<crate::rules::Rules>,
    /// The agents configured here, from `chant audit --agents` (#145).
    pub inventory: Arc<crate::inventory::Inventory>,
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

/// What was last written to layout.json, to skip writing it unchanged.
type SavedParts = (Mux, BTreeMap<PaneId, PaneMeta>, BTreeMap<MachineId, Machine>);

/// A random number for generated names.
fn seed() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(now_ms())
}

/// The agent a command line runs, seen through wrappers (#145).
fn agent_in(text: &str) -> Option<String> {
    crate::classify::agent(text).map(str::to_owned)
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
    /// The agent each pane's screen is read for (#145), and what its
    /// screen last said.
    watching: HashMap<PaneId, &'static str>,
    /// Agents with rules whose screens aren't read: not configured on this
    /// machine, says the inventory.
    unread: HashMap<PaneId, &'static str>,
    screen: HashMap<PaneId, AgentState>,
    /// Panes typed in since their agent was last idle: its next idle ends
    /// a turn someone started (a spinner at startup doesn't).
    turn_typed: std::collections::HashSet<PaneId>,
    /// Who's been typing in the tabs whose size they own: another client's
    /// typing waits for them to stop before taking the size (#333).
    hold: illogical_core::SizeHold,
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
    rules: Arc<crate::rules::Rules>,
    inventory: Arc<crate::inventory::Inventory>,
    /// The inventory's generation the panes were last watched by.
    inventory_seen: u64,
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
    /// Threads on panes and sessions (M61).
    threads: crate::threads::Threads,
    /// Huddles on sessions (M63).
    calls: crate::calls::Calls,
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

/// What one person sees besides panes: machines, who's here, threads,
/// huddles.
type People = (Vec<Machine>, Vec<Presence>, Vec<ThreadSummary>, Vec<Call>);

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
    threads: Vec<ThreadSummary>,
    calls: Vec<Call>,
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
pub fn start(config: Config, store: StateDir, kept: HashMap<String, Kept>, push: Option<Push>) -> MuxHandle {
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
    let rules = crate::rules::Rules::open(store.root().join("rules.json"));
    // Which agents are configured here (#145), read in the background.
    let inventory = crate::inventory::Inventory::new(shell_env.clone(), config.home.clone());
    inventory.refresh();
    let mut d = Daemon {
        mux: Mux::new(),
        panes: HashMap::new(),
        meta: HashMap::new(),
        attention: HashMap::new(),
        reasons: HashMap::new(),
        watching: Default::default(),
        unread: Default::default(),
        screen: Default::default(),
        turn_typed: Default::default(),
        hold: Default::default(),
        clients: HashMap::new(),
        focus: HashMap::new(),
        refused: HashMap::new(),
        viewing: HashMap::new(),
        zoomed: HashMap::new(),
        drawn: Default::default(),
        fs: fs.clone(),
        shell_env: shell_env.clone(),
        rules: rules.clone(),
        inventory: inventory.clone(),
        inventory_seen: 0,
        drivers: HashMap::new(),
        pair: Default::default(),
        drove: HashMap::new(),
        typing: Default::default(),
        lapse: driver_lapse(),
        trust: HashMap::new(),
        sizes: BTreeMap::new(),
        config,
        threads: crate::threads::Threads::open(store.root()),
        calls: Default::default(),
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
    MuxHandle { tx, events, store, provider, daemon_id, fs, ide, shell_env, rules, inventory }
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

/// A duration as people say it: 42s, 3m 5s, 1h 2m.
pub fn human_took(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

impl Daemon {
    /// Bring back the saved layout and every pane in it. False if there was
    /// nothing (usable) to restore.
    fn restore(&mut self, mut kept: HashMap<String, Kept>) -> bool {
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
        restoring: Option<(Policy, HashMap<String, Kept>)>,
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
            rules: self.rules.clone(),
            invite: self.config.invite.clone(),
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
                    self.tick_inventory();
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
                self.watch_agent(pane);
                self.changed();
            }
            What::Busy(true) => {
                // An idle agent redrawing itself (a resize, its status line)
                // isn't working; its screen says when it is.
                if running_command()
                    && self.screen.get(&pane) != Some(&AgentState::Idle)
                    && matches!(
                        self.attention.get(&pane).copied().unwrap_or_default(),
                        Attention::Idle | Attention::Done
                    )
                {
                    self.set_attention(pane, Attention::Working, "output");
                }
                self.watch_agent(pane);
            }
            What::Busy(false) => {
                // An agent gone quiet may be thinking, or done, or asking:
                // only its screen (or a hook, or a notification) says it
                // wants you. Quiet alone is idle, unless the screen says
                // it's still working (a long think).
                if running_command()
                    && self.looks_like_agent(pane)
                    && self.attention.get(&pane) == Some(&Attention::Working)
                    && self.screen.get(&pane) != Some(&AgentState::Working)
                {
                    self.set_attention(pane, Attention::Idle, "quiet");
                }
            }
            What::Screen(state, headline) => self.agent_screen(pane, state, headline),
            What::Signal(signal) => match signal {
                Signal::Prompt => {
                    self.emit(Some(pane), EventKind::Prompt);
                    self.watch_agent(pane);
                    if self.attention.get(&pane) == Some(&Attention::Working) {
                        self.set_attention(pane, Attention::Idle, "prompt");
                    }
                }
                Signal::CommandLine { .. } => {}
                Signal::CommandStart => {
                    let text = self.panes.get(&pane).and_then(|h| h.status().current.and_then(|c| c.text));
                    self.emit(Some(pane), EventKind::CommandStart { text });
                    self.watch_agent(pane);
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
                self.calls.leave_all(client);
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
                if !matches!(&data[..], b"\x1b[I" | b"\x1b[O") {
                    self.hold.typed(&self.mux, c, pane, std::time::Instant::now());
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
            self.turn_typed.insert(pane);
        }
        // A question open beside it still wants an answer (typing in Claude
        // Code's prompt box doesn't answer it), and so does a prompt on the
        // agent's screen that was only looked at.
        if self.asks.contains_key(&pane) || (!turn && self.screen.get(&pane) == Some(&AgentState::Blocked)) {
            return;
        }
        if matches!(self.attention.get(&pane), Some(Attention::NeedsInput | Attention::Done)) {
            let next = if p.status().current.is_some() { Attention::Working } else { Attention::Idle };
            self.set_attention(pane, next, "input");
        }
    }
}
