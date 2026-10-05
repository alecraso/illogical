//! Wire protocol shared by illogicald and its clients.
//!
//! Two kinds of WebSocket message:
//! - Text frames carry JSON control messages ([`ClientMsg`], [`ServerMsg`]).
//! - Binary frames carry terminal bytes with a fixed header ([`Frame`]).
//!
//! The web client mirrors these types by hand in `web/src/proto.ts`; keep
//! them in step.

use serde::{Deserialize, Serialize};

pub use illogical_core::{
    ClientId, Dir, Edge, Intent, Layout, Node, NodeId, OptionMap, OptionScope, Options, PaneId, Rect, Session,
    SessionId, SplitRect, TabId,
};

pub mod api;
pub mod ask;
pub mod fs;
pub mod hosts;
pub mod keys;

/// Control messages from a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Start (or resume) receiving panes. The server replays from each
    /// pane's offset when it still has the bytes, otherwise it sends a
    /// snapshot. With `zstd`, snapshots may come compressed
    /// ([`FrameKind::SnapshotZstd`]). With `acks`, the client sends
    /// [`ClientMsg::Ack`] as it draws, and the server holds back output more
    /// than a window past the last one (#52).
    Attach {
        panes: Vec<AttachPane>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        zstd: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        acks: bool,
        /// The client encodes keys for the kitty keyboard protocol, so
        /// programs asking may be told it's there (M31).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        kitty_keys: bool,
    },
    /// Everything of `pane` before `offset` has been drawn (for an attach
    /// with `acks`): from a snapshot's offset on, about every 64 KB.
    Ack { pane: PaneId, offset: u64 },
    /// Stop receiving panes.
    Detach { panes: Vec<PaneId> },
    /// The client is showing `tab` in a `cols`x`rows` cell area, optionally
    /// with one pane zoomed to fill it. With `claim` (the client was opened,
    /// focused or typed in), that becomes the tab's size; otherwise it only
    /// does if the client already owns the tab's size or nobody does.
    View { tab: TabId, cols: u16, rows: u16, zoom: Option<PaneId>, claim: bool },
    /// Change sessions, tabs or splits. Errors come back as
    /// [`ServerMsg::Error`] with the same id.
    Intent { id: Option<u64>, intent: Intent },
    /// Something about one pane rather than the layout.
    Pane { pane: PaneId, op: PaneOp },
    /// The pane this client is looking at, if its window has focus (`None`
    /// when it doesn't). Attention notifications skip panes someone sees.
    Focus { pane: Option<PaneId> },
    /// Answered with [`ServerMsg::Pong`] once everything sent before it has
    /// been handled (and its `State` sent), so a client can wait for its
    /// own intents to land: an intent that failed answers with an `Error`
    /// before the `Pong`.
    Ping { id: u64 },
    /// What this client wants from now on (M23). `summary`: pane summaries
    /// only (the swarm, the fleet): it attaches to nothing, and pane
    /// objects leave out `epoch`, `policy` and `integration`. Answered
    /// with a fresh `State` in that shape.
    Subscribe { summary: bool },
    /// Follow an editor (M28): its cursor, selection and the file it shows
    /// come as [`ServerMsg::Follow`] while `on`. Viewer access is enough.
    Follow { pane: PaneId, on: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PaneOp {
    /// What happens to the pane when the daemon starts again (after a
    /// reboot).
    SetPolicy {
        policy: Policy,
    },
    /// Delete the pane's saved history and clear its scrollback.
    Purge,
    /// Shell integration (command marks, exit codes, cwd) for shells started
    /// in this pane from now on.
    SetIntegration {
        on: bool,
    },
    /// Set the pane's attention state (a client dismissing a badge).
    Attention {
        state: Attention,
    },
    /// Drive it now (M13); whoever drove it is told.
    TakeControl,
    /// Ask whoever drives it to hand over.
    RequestControl,
    /// Hand over to `to` (a principal id).
    GiveControl {
        to: String,
    },
    /// Stop driving it: the next to type will.
    ReleaseControl,
    /// Pair mode: everyone who may edit types at once.
    SetPair {
        on: bool,
    },
    /// A guest asks the owner to let them drive this pane, which runs on
    /// the owner's machine (M14).
    RequestTrust,
    /// The owner lets `to` drive it for `minutes`.
    GrantTrust {
        to: String,
        minutes: u32,
    },
    RevokeTrust {
        to: String,
    },
    /// Keep it from everyone but the owner (M14).
    SetPrivate {
        on: bool,
    },
}

/// Whether a pane wants you: the cheap version of an "agent block".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    /// At a prompt, or nothing to report.
    #[default]
    Idle,
    /// A command is running and producing output.
    Working,
    /// It asked for you (a notification, a bell, an agent's hook, or a
    /// prompt on an agent's screen).
    NeedsInput,
    /// A long command finished while nobody was looking.
    Done,
}

/// Why a pane wants you (M24): what happened, not just "needs you", so a
/// client can explain it, bundle it with others and act on it. Every
/// `needs_input` and `done` pane has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    pub kind: ReasonKind,
    /// When it started wanting you.
    pub since_ms: u64,
    /// One line: the question, the command that failed, what finished.
    pub headline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// `done` and `failed`: how long the command ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Reasons with the same key are one card on a "needs you" rail ("11
    /// failed on build-03"): `failed:<machine>`, `exited:<machine>`,
    /// `ask:<project>:<agent>`. `None` never bundles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    /// `ask`: what is asked, and how to answer it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<AskRef>,
    /// `gate`: the gate that waits (the first, if several do), which
    /// `allow` approves (M34).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<Box<Gate>>,
    /// What [`api::ActRequest`] can do about it here.
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonKind {
    /// An agent asks: a question, or a permission to approve.
    Ask,
    /// It waits on you some other way (a bell, a notification, a prompt on
    /// an agent's screen).
    Input,
    /// A command that ran a while ended with a non-zero exit.
    Failed,
    /// The pane's program ended (with a non-zero code, or its machine went).
    Exited,
    /// A long command finished while nobody was looking.
    Done,
    /// An editor's debugger stopped, at a breakpoint or an exception (M28).
    Paused,
    /// A save left errors in an editor's workspace that wasn't there before.
    Errors,
    /// An editor has a file with merge conflicts open.
    Conflict,
    /// An agent's edit waits for approval as a diff (Claude Code's
    /// `openDiff`, with illogicald as its IDE).
    Diff,
    /// A release or an op waits at a gate for a person to approve it (M34:
    /// a chant gate in a workspace's member).
    Gate,
}

/// A gate that waits for someone (M34): an op stopped before a step until
/// a person approves it. The `gate` reason, its bundle on the swarm's rail,
/// the card and the phone's sheet are all made from this, whichever reader
/// found it (`source`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    /// The member (of a workspace) whose op waits.
    pub member: String,
    pub op: String,
    pub gate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// When it started waiting, and when it stops (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    /// Approvals so far, of how many it needs.
    pub approvals: u64,
    pub needed: u64,
    /// The source's own command for approving it, to show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub source: GateSource,
}

/// Where a gate was read, which is how it's approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GateSource {
    /// chant on the block's host: approved by `chant approve <op> <gate>`
    /// in the member's directory, `--approver` the person who approves.
    Chant {
        /// The workspace's root, and the member's directory, on that host.
        root: String,
        dir: String,
        /// The machine (sprite) it's on; none for this host.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
    },
    /// hud in a studio box (M35), from its work board: approved through
    /// hud's `POST /__hud/api/work/gates/approve`, naming the person who
    /// approves when the daemon holds a follower credential.
    Hud {
        /// The box's origin, and the app's name in studio.
        box_url: String,
        app: String,
    },
    /// A review asked of you on a forge's pull request (M36): approved as
    /// a review (`review {event: approve}`) through the forge block, with
    /// the person's own CLI login. `member` is the repository, `op` the
    /// PR (`#84`), `gate` is `review`.
    Forge {
        /// The forge's API base (`https://git.example/api/v1`) and its web
        /// address for the PR.
        api: String,
        url: String,
        number: u64,
    },
}

impl Gate {
    /// What names it among a block's gates: `member/op/gate`.
    pub fn key(&self) -> String {
        format!("{}/{}/{}", self.member, self.op, self.gate)
    }

    /// "delivery: ship waits at gate approve-ship".
    pub fn headline(&self) -> String {
        match &self.source {
            GateSource::Forge { .. } => format!("{}{}: review requested from you", self.member, self.op),
            _ => format!("{}: {} waits at gate {}", self.member, self.op, self.gate),
        }
    }

    /// Gates of one workspace are one card on the rail: `gate:<root>`
    /// (with its machine, when it's on one).
    pub fn bundle(&self) -> String {
        match &self.source {
            GateSource::Chant { root, machine: None, .. } => format!("gate:{root}"),
            GateSource::Chant { root, machine: Some(m), .. } => format!("gate:{m}:{root}"),
            GateSource::Hud { box_url, .. } => format!("gate:{box_url}"),
            // M36: bundled by repository, with the PR's other reasons.
            GateSource::Forge { api, .. } => format!("forge:{}/{}", host_of(api), self.member),
        }
    }
}

/// The host part of a URL (`https://git.example:3000/api/v1` →
/// `git.example:3000`), for bundle keys.
pub fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest)
}

/// The open question or approval behind an `ask` reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRef {
    /// What `allow`, `deny` and `answer` name (a permission request's id, or
    /// a question's).
    pub id: String,
    /// `approve` (allow or deny it) or `question` (answer or skip it).
    pub what: AskWhat,
    /// Who asks: the agent (`claude`, `fountain`, …).
    pub agent: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskWhat {
    Approve,
    Question,
}

/// Something done about a reason (`POST /api/attention/act`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Approve a permission request.
    Allow,
    /// Refuse a permission request, or skip a question.
    Deny,
    /// Answer a question (with content).
    Answer,
    /// Clear it: seen, nothing to do.
    Dismiss,
    /// A debugger stopped in an editor (M28): let it run on.
    Continue,
    /// Take an agent's proposed edit (M28: Claude Code's `openDiff`),
    /// optionally changed first (`content`).
    Accept,
    /// Turn an agent's proposed edit down.
    Reject,
    /// Type a failed command into its pane again, once its shell is idle
    /// (M11, M10's remainder).
    Rerun,
}

/// A command the shell integration reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    pub text: Option<String>,
    pub cwd: Option<String>,
    pub exit: Option<i32>,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// Stream offsets of its output: `tail --from start`.
    pub start: u64,
    pub end: Option<u64>,
    /// Who started it (M13), when someone other than the owner might have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// Something that happened, as streamed by `illogical events` and the event
/// API (one JSON object per line).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    Prompt,
    CommandStart {
        text: Option<String>,
    },
    CommandEnd {
        text: Option<String>,
        exit: Option<i32>,
    },
    Cwd {
        path: String,
    },
    Notify {
        title: String,
        body: String,
    },
    Bell,
    Attention {
        state: Attention,
        /// Why (M24); none when it went idle or working.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<Reason>,
    },
    Exit {
        code: Option<i32>,
        /// The machine it ran on went away (not the program ending).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        machine_gone: bool,
    },
    /// A machine was created, reached, or lost.
    Machine {
        machine: MachineId,
        state: MachineState,
    },
    /// A browser block shows a new page.
    Navigated {
        url: String,
        title: Option<String>,
    },
    /// A browser block's page couldn't be loaded (its server died, say).
    LoadError {
        url: String,
        error: String,
    },
    Opened,
    Closed,
    Layout {
        rev: u64,
    },
}

/// What a pane does when the daemon restores it. Its scrollback always comes
/// back; this decides what runs in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Policy {
    /// Nothing; press Enter for a shell.
    None,
    /// A login shell in the pane's last working directory.
    #[default]
    Shell,
    /// The command that was in the foreground, in its directory. With
    /// `confirm`, the pane asks first (press Enter).
    Rerun { confirm: bool },
    /// A fixed command, such as `claude --continue`.
    Hook { command: String },
}

/// Control messages from the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// First message on every connection.
    Hello { version: String, client: ClientId, state: State },
    /// The whole layout, after every change.
    State { state: State },
    /// A pane's size changed. Sent in order with its output, so the client
    /// resizes before drawing what follows.
    Size { pane: PaneId, cols: u16, rows: u16 },
    /// The client fell too far behind and was unsubscribed from this pane;
    /// attach again to get a fresh snapshot.
    Resync { pane: PaneId },
    /// An intent failed.
    Error { id: Option<u64>, message: String },
    /// A non-terminal block's state, whole: on connecting, and whenever it
    /// changes. Its type's renderer draws it.
    Block { block: PaneId, state: serde_json::Value },
    /// The answer to [`ClientMsg::Ping`].
    Pong { id: u64 },
    /// Something to show briefly that isn't an error (M13: someone took
    /// control of a pane you drove).
    Notice { message: String },
    /// `who` asks to drive `pane`, which this client's person drives.
    ControlRequest { pane: PaneId, who: String, name: String },
    /// A guest asks the owner to trust them with a pane on the owner's
    /// machine (M14).
    TrustRequest { pane: PaneId, who: String, name: String },
    /// What changed since the last `State` or `Delta` (M23), field by
    /// field. Layout changes (sessions, tabs, splits, panes opening and
    /// closing) still come as a whole `State`, in order with these.
    Delta { delta: Delta },
    /// What a followed editor sent (M28, S17's follow stream): `{file,
    /// line, col, sel, view, mode}`, `{open: {file, version, text}}`,
    /// `{edit: {file, version, changes}}`, `{diagnostics: {file, items}}`,
    /// or `{gone: true}` when it left.
    Follow { pane: PaneId, msg: serde_json::Value },
}

/// Changes to the last [`State`]: each pane in `panes` is `{id, ...}` with
/// only the fields that changed (a field set to `null` went back to its
/// default, absent); `gone` panes left this client's view. `machines` and
/// `presence` are whole when present. Anything else (sessions, tabs,
/// options, roles) changes with a new `State`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<serde_json::Map<String, serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gone: Vec<PaneId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machines: Option<Vec<Machine>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<Vec<Presence>>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty() && self.gone.is_empty() && self.machines.is_none() && self.presence.is_none()
    }
}

impl State {
    /// Bring this up to date with a [`ServerMsg::Delta`].
    pub fn apply(&mut self, delta: &Delta) {
        for patch in &delta.panes {
            let Some(id) = patch.get("id").and_then(|v| v.as_u64()).map(|v| v as PaneId) else { continue };
            let at = self.panes.iter().position(|p| p.id == id);
            let mut v = match at.map(|i| serde_json::to_value(&self.panes[i])) {
                Some(Ok(serde_json::Value::Object(m))) => m,
                _ => serde_json::Map::new(),
            };
            for (k, val) in patch {
                if val.is_null() {
                    v.remove(k);
                } else {
                    v.insert(k.clone(), val.clone());
                }
            }
            let Ok(info) = serde_json::from_value::<PaneInfo>(serde_json::Value::Object(v)) else { continue };
            match at {
                Some(i) => self.panes[i] = info,
                None => {
                    self.panes.push(info);
                    self.panes.sort_by_key(|p| p.id);
                }
            }
        }
        self.panes.retain(|p| !delta.gone.contains(&p.id));
        if let Some(m) = &delta.machines {
            self.machines = m.clone();
        }
        if let Some(p) = &delta.presence {
            self.presence = p.clone();
        }
    }
}

/// What a pane is busy with (M23), for drawing and grouping it without
/// attaching: from the foreground process's command line, else the
/// command the shell integration reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Shell,
    Build,
    Test,
    Agent,
    Server,
    Logs,
    Editor,
    /// A studio box (M35).
    App,
    /// A pull request on a forge (M36).
    Pr,
    /// An issue on a forge (M37).
    Issue,
    /// A Fountain block (M43): the account's agents.
    Fountain,
}

/// The git repository a pane's working directory is in (M23).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// The repository's top directory.
    pub root: String,
    /// Its last path component.
    pub name: String,
}

/// How much a pane prints (M23).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    /// Bytes of output a second, over the last second or so.
    pub bps: u32,
    /// When it last printed anything (ms since the epoch); 0: not since
    /// the daemon started.
    pub last_ms: u64,
}

/// Everything a client needs to draw: sessions in order, each tab's tree
/// and the cell rectangles the server computed for it, and pane details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub rev: u64,
    pub sessions: Vec<Session>,
    pub tabs: Vec<TabView>,
    pub panes: Vec<PaneInfo>,
    /// Machines that blocks run on, other than this host.
    #[serde(default)]
    pub machines: Vec<Machine>,
    /// Clients' named options (tmux `@` options), per scope.
    #[serde(default)]
    pub options: Box<Options>,
    /// For someone who isn't the daemon's owner (M12): their role in each
    /// session they see, as `[[session, role], ...]` (JSON object keys
    /// can't come back as numbers inside a tagged message). Absent for the
    /// owner, who owns everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<(SessionId, illogical_core::Role)>>,
    /// Who else is here and where they're looking (M13), within what this
    /// client sees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presence: Vec<Presence>,
}

pub type MachineId = u32;

/// A machine that blocks can run on instead of this host: today a
/// throwaway wisp sprite (a Firecracker microVM) owned by one pane, and
/// deleted when that pane closes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub id: MachineId,
    /// Who runs it: `wisp`.
    pub provider: String,
    /// The provider's name for it.
    pub sprite: String,
    /// What to call it ("drifting cedar", M7): a display name for the
    /// machines we make. The sprite keeps its own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    /// What it belongs to; the machine goes when that closes.
    pub owner: Owner,
    #[serde(default)]
    pub state: MachineState,
    /// Someone else's sandbox, borrowed for a shell (M4b): never created or
    /// deleted by us; closing its owner only ends our sessions on it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub borrowed: bool,
    /// Made for a guest (M14): their principal id, for their quota.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// A machine's owner: one pane (M3b), or a tab whose panes share it (M3c).
/// JSON `{"pane": 3}` or `{"tab": 2}`; a bare number (M3b's layout.json) is
/// a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", from = "OwnerRepr")]
pub enum Owner {
    Pane(PaneId),
    Tab(TabId),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OwnerRepr {
    Tagged(OwnerTagged),
    Bare(PaneId),
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum OwnerTagged {
    Pane(PaneId),
    Tab(TabId),
}

impl From<OwnerRepr> for Owner {
    fn from(r: OwnerRepr) -> Self {
        match r {
            OwnerRepr::Tagged(OwnerTagged::Pane(p)) | OwnerRepr::Bare(p) => Owner::Pane(p),
            OwnerRepr::Tagged(OwnerTagged::Tab(t)) => Owner::Tab(t),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineState {
    /// Created or being reached; the first program boots it.
    #[default]
    Starting,
    Running,
    /// Deleted from under us (or lost in a reboot of its host).
    Gone,
}

/// What a block is; where one runs is its `host`, not its type. All types
/// share one id space (`%N`) and one place in the layout tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockType {
    #[default]
    Terminal,
    /// A web page: a dev server on a machine, or another site (M6a).
    Browser,
    /// An agent run driven over ACP (M6b).
    Agent,
    /// VS Code (code-server) in a folder on the block's machine (M27).
    Editor,
    /// What changed in a git repository on the block's machine, read-only
    /// (M11).
    Diff,
    /// A file on the block's machine, read-only, followed live (M11).
    File,
    /// A pane that lives on another daemon in the host list (#17, M4's
    /// option (a)): this layout holds its place, and clients reach its
    /// terminal on that daemon directly. Its config is [`RemoteRef`].
    Remote,
    /// A chant workspace (M34): its members, records and the gates waiting
    /// in it, read through chant's read contract. Config `{root, env}`.
    Workspace,
    /// A studio box (M35): a web page that knows it's a hud box. It frames
    /// the box, and its agent's questions are asks on it.
    App,
    /// A pull request on a git forge (M36: Forgejo): its reviews, checks
    /// and timeline, and what it waits on you for. Config `{provider, api?,
    /// login?, repo, kind: pr | issue, number, host?, dir?}` (M37: an
    /// issue, and the agent working on it).
    Forge,
    /// The person's Fountain account (M43): its agents as a catalog, read
    /// with their own `fountain` login. Config `{profile?, view: catalog,
    /// filter?, specs?}`.
    Fountain,
}

/// Where a remote block's pane lives (#17): a host in the home daemon's
/// list, and the pane's id there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRef {
    pub host: String,
    pub pane: PaneId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabView {
    pub id: TabId,
    pub name: Option<String>,
    pub root: Node,
    pub cols: u16,
    pub rows: u16,
    pub owner: Option<ClientId>,
    pub zoom: Option<PaneId>,
    pub layout: Layout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachPane {
    pub pane: PaneId,
    /// Offset just past the last byte the client has, or `None` for a fresh
    /// view.
    pub offset: Option<u64>,
    /// At most this many rows of scrollback in a snapshot: what the client
    /// keeps (`None`: all of it). After a [`ServerMsg::Resync`], `0`: the
    /// client keeps what it has and needs only the screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<u32>,
}

impl AttachPane {
    /// From `offset`, with the whole history if a snapshot is needed.
    pub fn new(pane: PaneId, offset: Option<u64>) -> Self {
        Self { pane, offset, history: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub id: PaneId,
    /// Identifies this pane's output stream. Offsets are only meaningful
    /// within one epoch; a client holding an offset from another epoch (an
    /// earlier daemon) must attach with `None`.
    pub epoch: u64,
    /// The pane process's working directory, when known.
    pub cwd: Option<String>,
    /// The foreground command, when it isn't the shell itself.
    pub command: Option<String>,
    /// Whether a process is running (false while a restored pane waits for
    /// Enter).
    pub running: bool,
    pub policy: Policy,
    /// Running now, per the shell integration.
    #[serde(default)]
    pub current: Option<CommandInfo>,
    /// The last command that finished.
    #[serde(default)]
    pub last: Option<CommandInfo>,
    #[serde(default)]
    pub attention: Attention,
    /// Why it wants you (M24), when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    /// Shell integration for shells started in this pane.
    #[serde(default = "yes")]
    pub integration: bool,
    #[serde(default, rename = "type")]
    pub kind: BlockType,
    /// The machine it runs on; `None` is this host.
    #[serde(default)]
    pub host: Option<MachineId>,
    /// A question open in a terminal (Claude Code's AskUserQuestion, through
    /// its hook), drawn as a card beside it (M6c).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<ask::Ask>,
    /// Who answered its last question or approval, and how (M29), until
    /// it asks again. For agent blocks too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered: Option<ask::Answered>,
    /// An edit Claude Code in this terminal proposes, waiting as a diff
    /// (M28: illogicald as its IDE).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffInfo>,
    /// Claude Code in this terminal is connected to illogicald as its IDE
    /// (M28): lines can be mentioned to it from a followed editor.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claude_ide: bool,
    /// Claude Code in this terminal waits for a follow-up (its `illogical
    /// inbox` hook, M29): one sent now goes straight in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inbox: bool,
    /// Who is driving it (M13): only their typing reaches it, unless it's
    /// in pair mode. `None`: nobody yet (the next to type drives).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<Driver>,
    /// Its driver typed in it in the last few seconds (#118).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub typing: bool,
    /// Pair mode: every editor types at once.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pair: bool,
    /// Never shown to anyone but the owner (M14).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub private: bool,
    /// Guests trusted to drive this pane, though it runs on the owner's
    /// machine (M14): principal id and until when (ms).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted: Vec<(String, u64)>,
    /// What it's busy with (M23); `None` for blocks other than terminals
    /// and agents.
    #[serde(default, rename = "kind", skip_serializing_if = "Option::is_none")]
    pub work: Option<WorkKind>,
    /// The git repository it works in, if any (M23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Project>,
    /// Output rate (M23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
    /// The title its program set (OSC 0/2), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The file an editor block shows (M27), relative to its folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// What started it, when that wasn't you: an MCP client (M16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<StartedBy>,
    /// An editor's own report (M28): an editor block's, or someone's
    /// editor elsewhere (VS Code, Cursor, nvim) that joined the swarm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<EditorInfo>,
}

/// What an editor says about itself in summaries (M28, S17's schema):
/// what changes about once in ten seconds. The cursor and the file's text
/// are content and go only to followers ([`ServerMsg::Follow`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorInfo {
    /// `vscode`, `cursor`, `code-server`, `nvim`, ...
    pub app: String,
    /// VS Code's remote: `ssh-remote`, `dev-container`, ... (`None`: local).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// The remote's authority (`ssh-remote+geek`), to open the same file
    /// from a desktop editor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    /// The machine it runs on, as it names itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Diagnostics across the workspace.
    #[serde(default)]
    pub diag: Diag,
    /// Files with unsaved changes.
    #[serde(default)]
    pub dirty: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<DebugState>,
    /// A file with merge conflict markers that's open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
    /// How many people follow it now: the editor says so.
    #[serde(default)]
    pub followers: u32,
}

/// Diagnostic counts: errors, warnings, information.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diag {
    #[serde(default)]
    pub e: u32,
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub i: u32,
}

/// An editor's debug session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugState {
    /// `running` or `paused`.
    pub state: String,
    /// Why it stopped: `breakpoint`, `exception`, `step`, ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// An edit an agent proposes, waiting as a diff (M28).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffInfo {
    /// What `accept` and `reject` name.
    pub id: String,
    /// The file it changes (whole path), and relative to the pane's
    /// directory when it's inside.
    pub file: String,
    /// Lines added and removed.
    pub added: u32,
    pub removed: u32,
    /// The change as a unified diff, cut short when it's long.
    pub text: String,
    /// It makes a new file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
    pub at_ms: u64,
    /// Which IDE shows it: `illogical`, or the one diffs go to.
    pub ide: String,
}

/// Who started a pane or block through MCP (M16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartedBy {
    /// `mcp:<client>`, as the pane and history show it.
    pub by: String,
    /// The agent block whose token it came with, if any: that block may
    /// drive and close it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<PaneId>,
}

/// A pane's driver (M13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Driver {
    /// Principal id (`owner`, `tailnet:<login>`, `account:<id>`).
    pub who: String,
    pub name: String,
}

/// Someone looking at the daemon (M13): one per connected client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub client: ClientId,
    /// Principal id: one person's clients share it.
    pub who: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pic: Option<String>,
    /// The tab it shows, and the pane it's focused on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<TabId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
}

fn yes() -> bool {
    true
}

/// Binary frame kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    /// Server -> client: PTY output starting at `offset`.
    Output = 1,
    /// Server -> client: VT bytes that reproduce the pane as of `offset`.
    /// The client resets its terminal before writing them, unless it asked
    /// for the screen only after a resync ([`AttachPane::history`] `0`): then
    /// it keeps its scrollback and clears the screen and modes.
    Snapshot = 2,
    /// Client -> server: input for the pane (`offset` is unused).
    Input = 3,
    /// Server -> client: a [`FrameKind::Snapshot`] compressed with zstd, for
    /// a client that attached with `zstd`.
    SnapshotZstd = 4,
}

impl TryFrom<u8> for FrameKind {
    type Error = DecodeError;
    fn try_from(v: u8) -> Result<Self, DecodeError> {
        match v {
            1 => Ok(Self::Output),
            2 => Ok(Self::Snapshot),
            3 => Ok(Self::Input),
            4 => Ok(Self::SnapshotZstd),
            k => Err(DecodeError::UnknownKind(k)),
        }
    }
}

/// `[u8 kind][u32 pane][u64 offset][payload]`, integers big-endian.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub pane: PaneId,
    pub offset: u64,
    pub data: Vec<u8>,
}

pub const HEADER_LEN: usize = 1 + 4 + 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    TooShort(usize),
    UnknownKind(u8),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort(n) => write!(f, "frame too short: {n} bytes"),
            Self::UnknownKind(k) => write!(f, "unknown frame kind {k}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len());
        out.push(self.kind as u8);
        out.extend_from_slice(&self.pane.to_be_bytes());
        out.extend_from_slice(&self.offset.to_be_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        if buf.len() < HEADER_LEN {
            return Err(DecodeError::TooShort(buf.len()));
        }
        Ok(Self {
            kind: buf[0].try_into()?,
            pane: u32::from_be_bytes(buf[1..5].try_into().unwrap()),
            offset: u64::from_be_bytes(buf[5..13].try_into().unwrap()),
            data: buf[HEADER_LEN..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let f = Frame { kind: FrameKind::Output, pane: 7, offset: 1 << 40, data: b"hi\x1b[0m".to_vec() };
        assert_eq!(Frame::decode(&f.encode()).unwrap(), f);
    }

    #[test]
    fn frame_rejects_garbage() {
        assert_eq!(Frame::decode(&[1, 2]), Err(DecodeError::TooShort(2)));
        assert_eq!(Frame::decode(&[9; 13]), Err(DecodeError::UnknownKind(9)));
    }

    #[test]
    fn json_shape() {
        let m: ClientMsg =
            serde_json::from_str(r#"{"type":"attach","panes":[{"pane":1,"offset":null},{"pane":2,"offset":42}]}"#)
                .unwrap();
        let panes = vec![AttachPane::new(1, None), AttachPane::new(2, Some(42))];
        assert_eq!(m, ClientMsg::Attach { panes, zstd: false, acks: false, kitty_keys: false });
        // A client that keeps 10k rows and reads compressed snapshots.
        let m: ClientMsg =
            serde_json::from_str(r#"{"type":"attach","panes":[{"pane":1,"offset":7,"history":10000}],"zstd":true}"#)
                .unwrap();
        let panes = vec![AttachPane { pane: 1, offset: Some(7), history: Some(10_000) }];
        assert_eq!(m, ClientMsg::Attach { panes, zstd: true, acks: false, kitty_keys: false });
        let m: ClientMsg = serde_json::from_str(r#"{"type":"ack","pane":1,"offset":65536}"#).unwrap();
        assert_eq!(m, ClientMsg::Ack { pane: 1, offset: 65536 });
        let f = Frame { kind: FrameKind::SnapshotZstd, pane: 1, offset: 9, data: vec![1, 2] };
        assert_eq!(Frame::decode(&f.encode()), Ok(f));
        let s = serde_json::to_string(&ServerMsg::Resync { pane: 3 }).unwrap();
        assert_eq!(s, r#"{"type":"resync","pane":3}"#);
        let m: ClientMsg =
            serde_json::from_str(r#"{"type":"intent","id":4,"intent":{"op":"split","pane":1,"edge":"bottom"}}"#)
                .unwrap();
        assert_eq!(
            m,
            ClientMsg::Intent {
                id: Some(4),
                intent: Intent::Split { pane: 1, edge: Edge::Bottom, local: false, cwd: None }
            }
        );
        let m: ClientMsg = serde_json::from_str(
            r#"{"type":"intent","id":5,"intent":{"op":"set_option","scope":{"kind":"session","id":1},"name":"@a","value":"b"}}"#,
        )
        .unwrap();
        assert_eq!(
            m,
            ClientMsg::Intent {
                id: Some(5),
                intent: Intent::SetOption {
                    scope: OptionScope::Session(1),
                    name: "@a".into(),
                    value: Some("b".into())
                }
            }
        );
        assert_eq!(serde_json::to_string(&ServerMsg::Pong { id: 2 }).unwrap(), r#"{"type":"pong","id":2}"#);
        // Options keyed by id survive the trip inside a tagged message.
        let mut options = Options::default();
        options.sessions.insert(1, [("@a".to_owned(), "b".to_owned())].into());
        options.panes.insert(7, [("@uservars".to_owned(), "x".to_owned())].into());
        let state = State {
            rev: 1,
            sessions: vec![],
            tabs: vec![],
            panes: vec![],
            machines: vec![],
            options: Box::new(options),
            roles: Some(vec![(1, illogical_core::Role::Viewer), (4, illogical_core::Role::Editor)]),
            presence: vec![],
        };
        let msg = ServerMsg::State { state };
        let back: ServerMsg = serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        assert_eq!(back, msg);
    }
}
