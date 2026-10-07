//! The HTTP API (`/api/...`), which the `arugula` CLI uses over the
//! daemon's Unix socket and remote agents can use over the tailnet.
//!
//! | method | path | body / query | answer |
//! |---|---|---|---|
//! | GET | `/api/panes` | | `[PaneSummary]` |
//! | POST | `/api/run` | `RunRequest` | `RunResponse`: `{"pane": N}` |
//! | POST | `/api/panes/N/send` | `SendRequest` | `Empty` |
//! | POST | `/api/panes/N/prompt` | `PromptRequest` | `PromptResult`: the agent's turn, waited through (#147) |
//! | POST | `/api/panes/N/keys` | `KeysRequest` | `Empty` |
//! | POST | `/api/panes/N/mouse` | `MouseRequest` | `Empty` |
//! | POST | `/api/panes/N/attention` | `AttentionRequest` | `Empty` |
//! | POST | `/api/panes/N/ask` | `AskRequest` (AskUserQuestion's, from `arugula ask`; on a browser or app block, whatever follows its page's agent, M35) | when answered, `AskAnswer`: `{action: accept\|decline\|terminal\|withdrawn, content?, output?, by?}` |
//! | POST | `/api/panes/N/ask/withdraw` | `WithdrawRequest` | `Empty`: the asker gave up |
//! | GET | `/api/attention` | | `[AttentionItem]`: every pane that wants you, and why (M24) |
//! | POST | `/api/attention/act` | `ActRequest` | `ActResponse`: one result per pane |
//! | POST | `/api/panes/N/permit` | `PermitRequest`: Claude Code's `PermissionRequest` hook input (`arugula hook`) | when answered, `PermitAnswer`: `{action: allow\|deny\|withdrawn, output?}` |
//! | POST | `/api/panes/N/hook` | any other Claude Code hook input | `Empty`: closes a permission card the terminal answered |
//! | POST | `/api/panes/N/inbox` | `Stop`/`SessionStart` hook input (`arugula inbox`) | `InboxAnswer`, a follow-up: `{action: follow_up\|replaced, text?, by?}` |
//! | POST | `/api/panes/N/followup` | `FollowUpRequest` | `FollowedUp`: the agent's next instruction, from whoever may drive it |
//! | GET, POST | `/api/notify` | POST `NotifyRequest` | `NotifyPref`: which agents' "needs you" notifications reach you (M29) |
//! | POST | `/api/invite` | `InviteRequest` | `Invited`: share a session and push that person alone (#233; the owner's) |
//! | GET, POST | `/api/team-pins` | POST `{pins: {team: "<founder>.<founder's root>"}}` | `{pins, checked}`: teams the owner's browser pinned, whose rosters this machine checked (#233; the owner's) |
//! | POST | `/api/panes/N/close` | | `Empty` (its output stays in history) |
//! | POST | `/api/blocks` | `OpenRequest` | `OpenResponse`: `{"block": N}` |
//! | POST | `/api/conversations/open` | `OpenConversationRequest` | `OpenConversationResponse`: a Claude Code conversation as an agent block (M33) |
//! | GET | `/api/blocks/N` | | `Described`: `{info, state}`, `describe` |
//! | POST | `/api/blocks/N/call/METHOD` | JSON args | the method's answer |
//! | GET, POST, DELETE | `/api/studio` | POST `StudioLoginRequest` | `StudioStatus`: the studio and whether there's a token (never the token); POST logs in (`StudioLoggedIn`), DELETE forgets it (`Empty`, M35) |
//! | GET | `/api/studio/apps` | | `StudioApps`: `{studio, apps: [{name, title, url, status, blocks}]}` |
//! | PUT, DELETE | `/api/studio/followers/APP` | PUT `FollowerLinkRequest` | `Empty`: a hud follower link for the app's box |
//! | GET | `/api/machines` | | `[Machine]` |
//! | POST | `/api/machines/N/reset` | | `Empty`: delete and recreate it; its panes restart by policy |
//! | POST | `/api/panes/N/share-machine` | | `Empty`: the pane's machine now belongs to its tab |
//! | GET | `/api/panes/N/capture` | `format=text\|ansi\|html`, `scope=screen\|scrollback\|last-command` | text |
//! | GET | `/api/panes/N/process` | | `Process` |
//! | GET | `/api/panes/N/detection` | | `DetectionAnswer`: how its agent's screen reads, rule by rule (#145) |
//! | GET | `/api/panes/N/tail` | `from=OFFSET\|last-command`, `until=OFFSET`, `follow=1`, `text=1` | bytes (streamed with follow); other blocks: their text |
//! | GET | `/api/panes/N/wait` | `until=command-end\|exit\|match\|idle\|needs-input`, `re=`, `timeout=` secs | `WaitResult` |
//! | GET | `/api/panes/N/export.cast` | | asciicast v3 |
//! | GET | `/api/events` | `pane=`, `type=a,b`, `follow=1` | NDJSON `Event`s |
//! | GET | `/api/history` | `pane=`, `failed=1`, `since=` secs, `cwd=`, `match=` | `[HistoryEntry]` |
//! | GET | `/api/search` | `re=`, `since=` secs | `[SearchHit]` (output, and thread messages) |
//! | GET | `/api/threads/pane-N`, `/api/threads/session-N` | | `ThreadMessages`: a thread (M61), as the caller may read it |
//! | POST | `/api/threads/pane-N`, `/api/threads/session-N` | `ThreadPostRequest` | `ThreadPosted`: posted (needs drive); `agent` is `{delivered}` (or `{error}`) when an `@agent` went to the pane's agent |
//! | POST | `/api/threads/…/read` | `ThreadReadRequest` | `Empty`: the caller has read up to that message |
//! | GET | `/api/fs/…`, POST `/api/panes/N/cd` | | files on a host: see [`crate::fs`] |
//! | GET | `/api/host` | | `HostInfo`: this daemon's name and version, its tailnet URL, whether the tailnet has reached it, the control it joined |
//! | GET | `/api/hosts/self/shell-env` | | `ShellEnv`: the shell environment blocks that run your tools get (#74) |
//! | POST | `/api/hosts/self/shell-env/refresh` | | the same, resolved again |
//! | GET, POST | `/api/hosts/self/agents`, `.../agents/refresh` | | `AgentsInventory`: the agents chant found configured here, and which screen rule sets run (#145) |
//! | GET | `/api/agents/adapters` | | `Adapters`: whether Claude Code's and Codex's adapters can start here (#111) |
//! | POST | `/api/agents/adapters/KIND/install` | optional `{split?, session?, from_pane?}` | `RunResponse`: the install, in a pane |
//! | GET, PUT | `/api/ide` | PUT `IdeDiffsRequest` | `IdeInfo` (PUT: `IdeDiffs`): arugulad as Claude Code's IDE (M28; the owner's) |
//! | POST | `/api/ide/mention` | `IdeMentionRequest` | `IdeMentioned` |
//! | GET, DELETE | `/api/rules`, `/api/rules/N` | | `Rules` (DELETE: `Empty`): the standing permission rules (#166; the owner's) |
//! | GET | `/api/panes/N/diff` | | `PaneDiff`: the edit its diff card shows (M28) |
//! | GET | `/api/sessions/N/secrets` | | `[SecretFinding]`: panes whose recent output looks like it holds a secret |
//! | GET | `/api/conversations` | `all`, `q=`, `cwd=`, `live`, `limit=` | `ConversationList` (M33) |
//! | GET | `/api/fountain/agents` | `query=`, `source=`, `profile=` | `FountainAgents` (M43) |
//! | POST | `/api/push/subscribe`, `/api/push/test` | a browser's subscription | `PushSubscriptions` |
//! | GET | `/api/hosts` | | `HostList`: the daemons a client can switch between |
//! | POST | `/api/hosts` | `AddHost` | `Host` (replaces one with the same name) |
//! | DELETE | `/api/hosts/NAME` | | `{}` |
//! | POST | `/api/hosts/invite` | | `Invite`: a one-time token for `join` |
//! | POST | `/api/hosts/join` | `JoinRequest` | `Joined`; the token is the credential |
//! | POST | `/api/hosts/NAME/token` | | `HostToken`: a per-host token (replaces the last) |
//! | DELETE | `/api/hosts/NAME/token` | | `{}`: revoked, and its dial-out connection dropped |
//! | GET | `/api/dial` | `Authorization: Bearer <host token>` | WebSocket: a dial-out host's tunnel |
//! | any | `/h/NAME/ws`, `/h/NAME/api/...` | | a dial-out host's own WebSocket and API, through its tunnel |
//! | POST | `/api/shares` | `ShareRequest` | `Share` (with its token, shown once) |
//! | GET | `/api/shares` | | `[Share]` (no tokens) |
//! | DELETE | `/api/shares/N` | | `{}`: revoked; open viewers are cut off |
//! | GET | `/share/TOKEN` | | the read-only viewer page |
//! | GET | `/share/TOKEN/ws` | | WebSocket: the shared pane's snapshot and output, nothing else |
//! | GET | `/api/sync/state` | host token | `SyncState`: how much of each pane the home daemon has |
//! | POST | `/api/sync/N/log?from=OFFSET` | host token; raw bytes | `SyncedPane` |
//! | POST | `/api/sync/N/index?from=BYTE` | host token; raw bytes | `SyncedPane` |
//! | POST | `/api/sync/N/closed?at=MS` | host token | `SyncedPane` |
//! | GET | `/api/synced` | | `[SyncedHost]`: hosts whose history is kept here |
//! | DELETE | `/api/synced/NAME` | | `{}`: forget a host's synced history |
//! | POST | `/api/synced/rotate-key` | | `{"key": id}`: re-encrypt it all under a new key |
//!
//! `history`, `search` and `tail` take `host=NAME` (`*`: every host, for
//! history and search) to read history synced from another host instead.
//!
//! `dial`, `sync/*` and `/share/*` are reached without the owner's
//! identity: a host token or a share token is the credential there, and it
//! grants nothing else.
//!
//! `join` is how a sandbox adds itself to the home daemon's list. Sandboxes
//! are tagged tailnet nodes with no user identity, so the access checks
//! refuse them everything else.
//!
//! Errors are `{"error": "..."}` with a 4xx/5xx status.

use serde::{Deserialize, Serialize};

use crate::{Attention, PaneId, PaneInfo, Policy, SessionId, TabId};

/// `GET /api/attention`: a pane that wants you (M24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionItem {
    pub pane: PaneId,
    /// Its session; `None` for an editor that joined the swarm (M28),
    /// which isn't in one.
    #[serde(default)]
    pub session: Option<SessionId>,
    pub state: Attention,
    pub reason: crate::Reason,
}

/// `POST /api/attention/act`: do something about one pane's reason, or
/// several at once ("allow all 3", "dismiss all 11"). Each pane needs
/// editor on its session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ActRequest {
    pub action: crate::Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub pane: Option<PaneId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<PaneId>,
    /// The ask it answers (`AskRef::id`); without one, whatever the pane
    /// asks now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub id: Option<String>,
    /// `answer`: the card's fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub content: Option<serde_json::Value>,
    /// `allow`: `once` (default) or `always`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub option: Option<String>,
    /// `allow` `always` for Claude Code in a terminal (M29): which of its
    /// suggestions to keep (default the first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub suggestion: Option<u64>,
    /// `deny`: why, for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub message: Option<String>,
    /// `accept` (M28): the file as it should be saved, when someone
    /// changed the proposal first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub text: Option<String>,
}

impl ActRequest {
    /// The panes it names, once each.
    pub fn targets(&self) -> Vec<PaneId> {
        let mut out: Vec<PaneId> = self.pane.into_iter().chain(self.panes.iter().copied()).collect();
        let mut seen = std::collections::HashSet::new();
        out.retain(|p| seen.insert(*p));
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct ActResult {
    pub pane: PaneId,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ActResponse {
    pub results: Vec<ActResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneSummary {
    pub session: SessionId,
    pub session_name: String,
    pub tab: TabId,
    pub tab_name: Option<String>,
    #[serde(flatten)]
    pub info: PaneInfo,
}

/// `POST /api/blocks`: open a block of any type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields = nullable))]
pub struct OpenRequest {
    #[serde(rename = "type")]
    pub kind: crate::BlockType,
    /// What the type needs to make it (a URL, an agent command).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>", optional))]
    pub config: serde_json::Value,
    /// Session name or id, as for `run`.
    #[serde(default)]
    pub session: Option<String>,
    /// Split this block instead of opening a tab.
    #[serde(default)]
    pub split: Option<PaneId>,
    #[serde(default)]
    pub from_pane: Option<PaneId>,
    /// Run it on a new throwaway machine of its own.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub vm: bool,
    /// The new machine's image.
    #[serde(default)]
    pub image: Option<String>,
    /// Run it on this machine [default: the tab's, when splitting in a VM
    /// tab; else this host].
    #[serde(default)]
    pub host: Option<crate::MachineId>,
    /// On this host, even split in a VM tab.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub local: bool,
}

/// What opening a block answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OpenResponse {
    pub block: PaneId,
}

/// `POST /api/run`: a new terminal pane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields = nullable))]
pub struct RunRequest {
    /// Run with the pane's shell (`$SHELL -l -c COMMAND`); none for just a
    /// shell.
    #[serde(default)]
    pub command: Option<String>,
    /// Run it on a new throwaway machine owned by the pane.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub vm: bool,
    /// In a new tab whose panes all share a new throwaway machine.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub vm_tab: bool,
    /// The machine's image (the provider's default if none).
    #[serde(default)]
    pub image: Option<String>,
    /// On a sandbox that already exists (the provider's name for it), over
    /// a plain exec with no daemon there ("open shell", M4b). The sandbox
    /// isn't ours: closing the pane leaves it be.
    #[serde(default)]
    pub sandbox: Option<String>,
    /// Session name or id; created if no session has that name. Default: the
    /// session of `from_pane`, else the first.
    #[serde(default)]
    pub session: Option<String>,
    /// Split this pane instead of opening a tab.
    #[serde(default)]
    pub split: Option<PaneId>,
    /// With `split`: the new pane runs where the split pane does (its tab's
    /// machine, or the sandbox it has a shell on) instead of this host.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub join: bool,
    /// Where it starts. On a machine, a directory there.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub policy: Option<Policy>,
    /// Where the request comes from (`$ARUGULA_PANE`): the default session
    /// and working directory.
    #[serde(default)]
    pub from_pane: Option<PaneId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RunResponse {
    pub pane: PaneId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequest {
    pub text: String,
    /// Press Enter afterwards.
    #[serde(default)]
    pub enter: bool,
}

/// Prompt the agent in a pane (a terminal running one, or an agent block)
/// and wait for its turn, in one call (#147): the wait starts before the
/// prompt is typed, so it can't miss the agent starting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptRequest {
    pub text: String,
    /// It's waiting on an approval or a question, and this answers it.
    /// Without it, an agent that's waiting on someone isn't typed at.
    #[serde(default)]
    pub answering: bool,
    /// Seconds to wait for any sign of work before `stalled` (default 5).
    #[serde(default)]
    pub stall: Option<f64>,
    /// Seconds to wait in all before `still_running` (default 100).
    #[serde(default)]
    pub timeout: Option<f64>,
}

/// What prompting an agent came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PromptResult {
    /// Its turn ended.
    Done,
    /// It asks for someone: an approval or a question.
    NeedsInput {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        question: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ask: Option<Box<crate::ask::Ask>>,
    },
    /// It was already waiting on someone, so nothing was typed (typing
    /// would answer it); `answering` says it's meant to.
    Blocked {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        question: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ask: Option<Box<crate::ask::Ask>>,
    },
    /// No sign of work within the stall window: no agent there, the
    /// prompt wasn't submitted, or the agent died. With the screen's last
    /// lines, to see which.
    Stalled { why: String, screen: String },
    /// Still working at the timeout: wait until idle.
    StillRunning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeysRequest {
    /// tmux-style names: `C-c`, `M-x`, `Up`, `Enter`, `F5`, `Space`, or a
    /// single character.
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MouseAction {
    /// Press and release.
    #[default]
    Click,
    Press,
    Release,
    /// Move with the button held.
    Drag,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseRequest {
    /// Cell column, from 1.
    pub x: u16,
    /// Cell row, from 1.
    pub y: u16,
    #[serde(default)]
    pub button: MouseButton,
    #[serde(default)]
    pub action: MouseAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionRequest {
    pub state: Attention,
    /// What it's about (a hook's message), for the reason's headline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    pub pid: u32,
    /// The foreground process (the shell when nothing else runs).
    pub foreground: u32,
    pub argv: Vec<String>,
    pub comm: String,
    pub exe: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum WaitResult {
    CommandEnd {
        text: Option<String>,
        exit: Option<i32>,
        start: u64,
        end: Option<u64>,
    },
    Exit {
        code: Option<i32>,
    },
    Match {
        text: String,
        offset: u64,
    },
    /// `until=idle` (no longer working) or `until=needs-input`: where it got,
    /// and the question it's waiting on, if that's why.
    Attention {
        state: Attention,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ask: Option<Box<crate::ask::Ask>>,
    },
    Timeout,
}

/// What a history entry is. Only a command ran in a shell and has an exit
/// code worth counting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HistoryKind {
    /// Ran in a shell.
    #[default]
    Command,
    /// An answer or an approval: who said what to a card or a gate.
    Answer,
    /// What an agent block did: a tool call that isn't a shell command,
    /// or a turn.
    Agent,
}

impl HistoryKind {
    pub fn is_command(&self) -> bool {
        *self == HistoryKind::Command
    }

    /// `command`, `answer` or `agent`.
    pub fn parse(s: &str) -> Option<HistoryKind> {
        match s {
            "command" => Some(HistoryKind::Command),
            "answer" => Some(HistoryKind::Answer),
            "agent" => Some(HistoryKind::Agent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub pane: PaneId,
    /// False for a pane that has been closed (its history is kept a while).
    pub open: bool,
    pub text: Option<String>,
    pub cwd: Option<String>,
    pub exit: Option<i32>,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// Stream offsets of the output: `tail --from start`.
    pub start: u64,
    pub end: Option<u64>,
    /// A synced copy of another host's history (`host=NAME`), not ours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Who typed it (M13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Command, answer or agent. Older records have none: commands.
    #[serde(default)]
    pub kind: HistoryKind,
}

/// A handoff in a pane: from here on, `who` typed (`arugula log --who`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverEntry {
    pub at_ms: u64,
    pub offset: u64,
    pub who: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub pane: PaneId,
    pub open: bool,
    /// Stream offset of the line.
    pub offset: u64,
    pub line: String,
    /// The command whose output it is, if known.
    pub command: Option<String>,
    /// A synced copy of another host's history (`host=NAME`), not ours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// A message in a thread (M61), `pane-N` or `session-N` (then `pane`
    /// is 0), not output: `offset` is the message's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

/// `POST /api/shares`: a read-only link to one terminal pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields = nullable))]
pub struct ShareRequest {
    pub pane: PaneId,
    /// Seconds until it expires [default: an hour; at most a week].
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

/// A read-only share of one pane. `token`, `path` and `url` are only in the
/// answer that minted it; the daemon keeps a hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct Share {
    pub id: u32,
    pub pane: PaneId,
    pub created_ms: u64,
    pub expires_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// `/share/<token>`, on this daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The whole link, on this daemon's tailnet name when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `POST /api/guests` (M65): an invite to one terminal pane for someone
/// with only OpenSSH.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields = nullable))]
pub struct GuestInviteRequest {
    pub pane: PaneId,
    /// They may type (one driver per pane still applies).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub rw: bool,
    /// Good for any number of logins until it ends; else the first spends it.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub reusable: bool,
    /// Seconds until it expires [default: an hour; at most a day, or two
    /// hours with `rw`].
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    /// What to call them, on their input [default: `guest`].
    #[serde(default)]
    pub label: Option<String>,
    /// The address to put in the command [default: the daemon's
    /// `--guest-ssh-host`, else its hostname].
    #[serde(default)]
    pub host: Option<String>,
    /// Through control's ssh jump host (`true`), or straight to this
    /// machine (`false`) [default: through control when the daemon is
    /// joined to one that has a jump host and no address is named].
    #[serde(default)]
    pub relay: Option<bool>,
}

/// An ssh invite to a pane (M65). `token`, `command` and the pinning lines
/// are only in the answer that made it; the daemon keeps a hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct GuestInvite {
    pub id: u32,
    pub pane: PaneId,
    pub rw: bool,
    pub reusable: bool,
    pub label: String,
    pub created_ms: u64,
    pub expires_ms: u64,
    /// A single-use invite someone has logged in with.
    #[serde(default)]
    pub used: bool,
    /// Guests connected with it now.
    #[serde(default)]
    pub sessions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// What the guest pastes: `ssh` with the host key pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The pinned key as a known-hosts line, for an OpenSSH older than 8.5
    /// (no `KnownHostsCommand`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_hosts: Option<String>,
    /// The host key's SHA256 fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Through control's ssh jump host (the daemon is behind NAT).
    #[serde(default)]
    pub relay: bool,
    /// The jump host, as `host[:port]` (with `command` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<String>,
}

/// `GET /api/sync/state`: what the home daemon holds of the calling host's
/// panes, so a push resumes where the last one stopped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncState {
    pub panes: std::collections::BTreeMap<PaneId, SyncedPane>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedPane {
    /// Stream offset just past the last output byte held.
    pub log_end: u64,
    /// Bytes of the pane's index held.
    pub index_len: u64,
    /// When the pane closed on its host, once it has.
    #[serde(default)]
    pub closed_ms: Option<u64>,
    #[serde(default)]
    pub last_push_ms: u64,
    /// Output bytes held (after retention).
    #[serde(default)]
    pub bytes: u64,
}

/// `GET /api/synced`: a host whose history the home daemon keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedHost {
    pub name: String,
    pub panes: std::collections::BTreeMap<PaneId, SyncedPane>,
}

/// What a route answers when it has nothing to say: `{}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

/// `GET /api/threads/…`: a thread's messages, as the caller may read them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ThreadMessages {
    pub target: crate::ThreadTarget,
    pub messages: Vec<crate::ThreadMsg>,
}

/// `POST /api/threads/…`: a message, with output it quotes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields = nullable))]
pub struct ThreadPostRequest {
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<String>"))]
    pub text: String,
    #[serde(default)]
    pub quote: Option<crate::Quote>,
}

/// `POST /api/threads/…/read`: the caller has read up to that message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ThreadReadRequest {
    pub upto: u64,
}

/// What a post answers. `agent` is set when an `@agent` went to the pane's
/// agent; `invitable` only in the owner's answer (#297), so nobody else's
/// says who exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ThreadPosted {
    pub message: crate::ThreadMsg,
    pub agent: Option<ThreadAgent>,
    pub unreached: Vec<Unreached>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub invitable: Option<Vec<Invitable>>,
}

/// What handing a post to the pane's agent came to: `delivered` (`false`:
/// queued), or `error`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct ThreadAgent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Why an `@` in a post reached no one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum UnreachedWhy {
    /// `@agent` in a session's thread: an agent is a pane's.
    AgentNeedsPane,
    /// `@agent` from someone who may not drive the pane.
    MayNotDrive,
    /// The name is unknown, or its owner can't read the thread.
    Nobody,
}

/// An `@` in a post that reached no one, for the poster alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Unreached {
    pub token: String,
    pub why: UnreachedWhy,
}

/// Someone the owner's `@token` named who can't read the thread (#297):
/// theirs to invite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct Invitable {
    pub token: String,
    /// `tailnet:<login>` or `account:<id>`.
    pub who: String,
    pub name: String,
    /// Another principal taken to be them (a login by their name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged: Option<String>,
}

/// `POST /api/invite`: share a session with someone and tell them, and only
/// them (#233; the owner's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields = nullable))]
pub struct InviteRequest {
    pub session: SessionId,
    /// `tailnet:<login>`, `account:<id>`, or a name: someone shared with,
    /// or in a checked roster.
    pub who: String,
    #[serde(default)]
    pub role: Option<arugula_core::Role>,
    #[serde(default)]
    pub note: Option<String>,
    /// Where it opens (default: the session's first pane).
    #[serde(default)]
    pub pane: Option<PaneId>,
    /// With history (default: from now on), for a new grant.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub history: bool,
    /// An editor may also type on this machine's pane for so long (M14).
    #[serde(default)]
    pub drive_minutes: Option<u32>,
    /// For an `account:` no grant or pin vouches for: their root device,
    /// whose fingerprint the owner checked with them.
    #[serde(default)]
    pub root: Option<String>,
    /// From a thread's mention (#297): the thread (`pane-N`, `session-N`)
    /// it opens, the one a "from now" share reads from `msg` on (the
    /// message that mentioned them), or all of with `whole_thread`. Other
    /// threads start at the share, as ever.
    #[serde(default)]
    pub thread: Option<String>,
    #[serde(default)]
    pub msg: Option<u64>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(as = "Option<bool>"))]
    pub whole_thread: bool,
}

/// How an invite's push went: `sent` once a subscription took it,
/// `pending` while control can't reach them yet, else `unreachable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum InviteDelivery {
    Sent,
    Pending,
    Unreachable,
}

impl InviteDelivery {
    /// `sent`, `pending` or `unreachable`.
    pub fn as_str(self) -> &'static str {
        match self {
            InviteDelivery::Sent => "sent",
            InviteDelivery::Pending => "pending",
            InviteDelivery::Unreachable => "unreachable",
        }
    }
}

/// What an invite granted: the role they hold now, and whether this invite
/// gave it (`false`: they held it already).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct InviteGrant {
    pub session: SessionId,
    pub principal: String,
    pub name: String,
    pub role: arugula_core::Role,
    pub granted: bool,
}

/// What an invite answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Invited {
    pub invite: String,
    pub grant: InviteGrant,
    pub pane: PaneId,
    pub delivery: InviteDelivery,
    /// Why it isn't `sent`.
    pub reason: Option<String>,
    /// Whether they may drive (`drive_minutes`), when that was asked.
    pub drive: Option<bool>,
}

/// `POST /api/conversations/open` (M33): a Claude Code conversation as an
/// agent block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields = nullable))]
pub struct OpenConversationRequest {
    /// Its id, or a unique prefix.
    pub id: String,
    /// Then `continue` or `fork` it.
    #[serde(default)]
    pub then: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub split: Option<PaneId>,
    #[serde(default)]
    pub from_pane: Option<PaneId>,
}

/// What opening a conversation answers: its block (`opened`: made now, not
/// there already), and why `then` didn't go through, if it didn't.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct OpenConversationResponse {
    pub block: PaneId,
    pub opened: bool,
    pub conversation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `POST /api/team-pins`: the teams the owner's browser pinned, and those
/// it left (#233; the owner's). Their rosters are checked against these.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct TeamPinsRequest {
    /// Team id to `<founder device>.<founder's root>`, as the owner's
    /// browser pinned it.
    #[serde(default)]
    pub pins: std::collections::BTreeMap<String, String>,
    /// Teams pinned here that the owner's account is no longer in: their
    /// members stop being nameable.
    #[serde(default)]
    pub drop: Vec<String>,
}

/// `GET` and `POST /api/team-pins`: the teams pinned here, and those whose
/// rosters this machine checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TeamPins {
    pub pins: std::collections::BTreeMap<String, String>,
    pub checked: Vec<String>,
}

/// What "needs you" notifications someone other than the owner gets (M29):
/// agents in these sessions, or everything they may edit here ("this team's
/// agents" on a team daemon). The owner always is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NotifyPref {
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub sessions: std::collections::BTreeSet<SessionId>,
}

/// `POST /api/notify`: opt in or out of a session's agents, or all of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct NotifyRequest {
    /// One session; none: everything you may edit here.
    #[serde(default)]
    pub session: Option<SessionId>,
    pub on: bool,
}

// ---- the routes the web client doesn't call (#446): the CLI's, agents' and
// hooks'. Where a body or answer holds a shape another crate owns (a Claude
// Code hook's input, a block's config, the daemon's own inventory records),
// that part stays a `serde_json::Value`, and the type around it says the rest.

/// `POST /api/panes/N/ask` (`arugula ask`): AskUserQuestion's questions, to
/// show beside a terminal (or a browser or app block, M35) and wait for the
/// answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct AskRequest {
    /// AskUserQuestion's `questions`, as the hook got them.
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub questions: serde_json::Value,
    /// The tool use's id, so asking again (after a daemon restart) is the
    /// same question.
    #[serde(default)]
    pub id: Option<String>,
    /// What raised it, when that isn't Claude Code's hook (M35: `hud`, for
    /// a studio box's agent asking on a browser or app block).
    #[serde(default)]
    pub source: Option<String>,
    /// Who asks, as the card names it ("hud asks").
    #[serde(default)]
    pub agent: Option<String>,
}

/// How a question was answered: `accept` with the card's fields, the hook's
/// `output` for Claude Code and who answered; `decline` (skipped);
/// `terminal` (answer in the terminal); `withdrawn`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum AskAnswer {
    Accept {
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        content: serde_json::Value,
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        output: serde_json::Value,
        by: Option<crate::Driver>,
    },
    Decline {
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        output: serde_json::Value,
        by: Option<crate::Driver>,
    },
    Terminal,
    Withdrawn,
}

/// `POST /api/panes/N/ask/withdraw`: the asker gave up (the question with
/// this `id`, else whatever the pane asks).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields = nullable))]
pub struct WithdrawRequest {
    #[serde(default)]
    pub id: Option<String>,
}

/// `POST /api/panes/N/permit` (`arugula hook`): Claude Code's
/// `PermissionRequest` hook input, of which the daemon reads these. A field
/// that is missing or isn't what Claude Code sends reads as absent, as it
/// always did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields = nullable))]
pub struct PermitRequest {
    #[serde(default, deserialize_with = "string_or_none")]
    pub tool_name: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub tool_input: serde_json::Value,
    #[serde(default, deserialize_with = "string_or_none")]
    pub session_id: Option<String>,
    #[serde(default, deserialize_with = "string_or_none")]
    pub agent_id: Option<String>,
    /// Claude Code's suggestions for rules to keep (an array, else ignored).
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub permission_suggestions: Option<serde_json::Value>,
}

/// A string where there is one; anything else is none.
fn string_or_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::String(s) => Some(s),
        _ => None,
    })
}

/// How a permission card was answered: `allow` or `deny` with the hook's
/// `output`, or `withdrawn`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum PermitAnswer {
    Allow {
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        output: serde_json::Value,
    },
    Deny {
        #[cfg_attr(feature = "ts", ts(type = "unknown"))]
        output: serde_json::Value,
    },
    Withdrawn,
}

/// `POST /api/panes/N/inbox` (`arugula inbox`): a follow-up for the agent,
/// or `replaced` when a newer waiter took over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum InboxAnswer {
    FollowUp { text: String, by: crate::Driver },
    Replaced,
}

/// `POST /api/panes/N/followup`: the agent's next instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FollowUpRequest {
    pub text: String,
}

/// What a follow-up answers: `delivered` it went straight in (else it waits
/// for the agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FollowedUp {
    pub delivered: bool,
}

/// What answering a question or permission card on a terminal or block
/// says: the id of what was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Answered {
    pub answered: String,
}

/// `GET /api/sessions/N/secrets`: a pane of the session whose recent output
/// looks like it holds a secret, and what kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SecretFinding {
    pub pane: PaneId,
    pub kinds: Vec<String>,
}

/// `GET /api/panes/N/diff` (M28): the edit a pane's diff card shows, before
/// and after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PaneDiff {
    pub diff: crate::DiffInfo,
    pub old: String,
    pub new: String,
}

/// `GET /api/blocks/N`: where a block is and what it's doing. `state` is
/// the block's own (each type has its shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Described {
    #[cfg_attr(feature = "ts", ts(type = "Record<string, unknown>"))]
    pub info: PaneSummary,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub state: serde_json::Value,
}

/// `GET /api/conversations` (M33): Claude Code conversations on this
/// machine, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ConversationList {
    pub conversations: Vec<ConversationRow>,
    /// How many there are, before `limit`.
    pub total: usize,
}

/// A conversation as the daemon's index has it (its own record, kept as
/// JSON here), with the agent block that has it open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Record<string, unknown> & { block: number | null }"))]
pub struct ConversationRow {
    #[serde(flatten)]
    pub conversation: serde_json::Value,
    pub block: Option<PaneId>,
}

/// `GET /api/agents/adapters` (#111): whether Claude Code's and Codex's
/// adapters can start here (each one's own record).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Adapters {
    #[cfg_attr(feature = "ts", ts(type = "Array<unknown>"))]
    pub adapters: Vec<serde_json::Value>,
}

/// `GET /api/ide` (M28): arugulad as Claude Code's IDE, and the other IDEs
/// registered beside it. Off, it's only `on: false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct IdeInfo {
    pub on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub lock_dir: Option<std::path::PathBuf>,
    /// Which IDE gets diffs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub others: Option<Vec<IdeOther>>,
}

/// Another IDE registered with Claude Code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IdeOther {
    pub name: String,
    pub port: u16,
    pub pid: Option<u32>,
    pub folders: Vec<String>,
    pub alive: bool,
}

/// `PUT /api/ide`: which IDE gets Claude Code's diffs (`arugula`, or
/// another's name).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IdeDiffsRequest {
    pub diffs: String,
}

/// What `PUT /api/ide` answers: the IDE that gets diffs now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IdeDiffs {
    pub diffs: String,
}

/// `POST /api/ide/mention` (M28): put `@file#Lstart-end` in Claude Code's
/// prompt in a pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IdeMentionRequest {
    /// The terminal Claude Code runs in.
    pub pane: PaneId,
    pub file: String,
    /// Lines, from 1.
    pub start: u32,
    pub end: u32,
}

/// What a mention answers: how many Claude Code connections it went to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct IdeMentioned {
    pub sent: usize,
}

/// `GET /api/rules` (#166): the standing permission rules agent blocks
/// answer from, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Rules {
    pub rules: Vec<StandingRule>,
}

/// One standing permission rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(optional_fields))]
pub struct StandingRule {
    pub tool: String,
    /// Only titles starting with this (word for word); the whole tool
    /// without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Blocks working in this directory or under it; every block without
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The VM (sprite) the directory is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite: Option<String>,
    /// When it was made (ms since the epoch), and the request that made it.
    #[serde(default)]
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Its place in the list: what `DELETE /api/rules/{index}` names.
    pub index: usize,
    /// How it reads.
    pub text: String,
}

/// `GET /api/hosts/self/shell-env` (#74): the shell environment blocks that
/// run your tools get: its `PATH` and the names of the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ShellEnv {
    pub shell: String,
    pub ok: bool,
    pub error: Option<String>,
    pub ms: u64,
    pub path: Option<String>,
    pub vars: Vec<String>,
}

/// `GET /api/hosts/self/agents` (#145): the agents configured on this
/// machine, as chant last found them (the daemon's inventory record, kept
/// as JSON here), and which screen rule sets run here because of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Record<string, unknown> & { rules: AgentRules }"))]
pub struct AgentsInventory {
    #[serde(flatten)]
    pub inventory: serde_json::Value,
    pub rules: AgentRules,
}

/// Which agents' screens are read here (`run`) and which aren't (`off`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentRules {
    pub run: Vec<String>,
    pub off: Vec<String>,
}

/// `GET /api/panes/N/detection` (#145): how the screen of the agent in a
/// pane reads, rule by rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum DetectionAnswer {
    Read(Detection),
    /// No agent's screen is read there.
    NoAgent(NoDetection),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Detection {
    pub agent: String,
    pub name: String,
    /// What was last reported (after the debounce).
    pub shown: Option<String>,
    /// The rule that matches now, if any.
    pub fired: Option<String>,
    pub title: String,
    pub rules: Vec<DetectionRule>,
    /// Its screen isn't read: chant's inventory doesn't list it here. No
    /// rules, then.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
    /// When unread: the runtimes chant found configured instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub configured: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct DetectionRule {
    pub rule: String,
    pub state: String,
    pub priority: u16,
    pub region: String,
    pub text: Vec<String>,
    pub matched: bool,
}

/// `agent: null`, and the command that runs there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoDetection {
    pub agent: Option<String>,
    pub command: Option<String>,
}

/// `GET /api/studio` (M35): which studio, whether there's a token, and which
/// apps have a follower link. Never the token or a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StudioStatus {
    pub url: Option<String>,
    pub logged_in: bool,
    pub followers: Vec<String>,
}

/// `POST /api/studio` (`arugula studio login`): keep a studio token, once
/// studio takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StudioLoginRequest {
    pub url: String,
    pub token: String,
}

/// What a studio login answers: the person's apps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StudioLoggedIn {
    pub apps: Vec<StudioApp>,
}

/// One of the person's apps, as studio lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StudioApp {
    pub name: String,
    pub title: Option<String>,
    /// The box's origin (`https://name.studio.example`).
    pub url: String,
    pub status: Option<String>,
}

/// `GET /api/studio/apps`: the person's apps, from studio, with the app
/// blocks that show them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StudioApps {
    pub studio: Option<String>,
    pub apps: Vec<StudioAppRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct StudioAppRow {
    #[serde(flatten)]
    pub app: StudioApp,
    pub blocks: Vec<PaneId>,
}

/// `PUT /api/studio/followers/APP`: a hud follower link for the app's box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FollowerLinkRequest {
    pub link: String,
}

/// `GET /api/fountain/agents` (M43): the person's Fountain agents, read with
/// their own login on this host: compact cards (the daemon's own record of
/// each, kept as JSON here), filtered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FountainAgents {
    pub base_url: String,
    pub profile: String,
    /// How many agents Fountain has, before the filter.
    pub total: usize,
    #[cfg_attr(feature = "ts", ts(type = "unknown"))]
    pub filter: serde_json::Value,
    #[cfg_attr(feature = "ts", ts(type = "Array<unknown>"))]
    pub agents: Vec<serde_json::Value>,
    /// Rows Fountain sent that didn't parse.
    pub unreadable: usize,
}

/// What `POST /api/push/subscribe` and `/api/push/test` answer: how many
/// browsers are subscribed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PushSubscriptions {
    pub subscriptions: usize,
}

/// Each typed answer against the `json!` literal the daemon built before
/// the type existed (#445): the bytes on the wire don't change. The
/// literals are copied from those handlers.
#[cfg(test)]
mod wire {
    use serde_json::{Value, json};

    use super::*;
    use crate::{Quote, ThreadMsg, ThreadTarget, hosts::*};

    fn msg() -> ThreadMsg {
        ThreadMsg {
            id: 3,
            at: 1000,
            who: "owner".into(),
            name: "owner".into(),
            pic: None,
            text: "hi @sam".into(),
            quote: Some(Quote { pane: 2, text: "ls".into() }),
            mentions: vec![],
            landed: vec![],
            to_agent: false,
            agent: false,
        }
    }

    /// `thread_get`, `thread_post` and `thread_read`.
    #[test]
    fn threads_answer_as_they_did() {
        let m = msg();
        let target = ThreadTarget::Pane(2);
        let msgs = vec![m.clone()];
        let typed = ThreadMessages { target, messages: msgs.clone() };
        assert_eq!(serde_json::to_value(&typed).unwrap(), json!({ "target": target, "messages": msgs }));
        let none = ThreadMessages { target: ThreadTarget::Session(1), messages: vec![] };
        assert_eq!(
            serde_json::to_value(&none).unwrap(),
            json!({ "target": ThreadTarget::Session(1), "messages": Vec::<ThreadMsg>::new() })
        );

        // What the old `Unreached` and the old `agent` values serialized to.
        let unreached = vec![
            Unreached { token: "sam".into(), why: UnreachedWhy::Nobody },
            Unreached { token: "agent".into(), why: UnreachedWhy::MayNotDrive },
        ];
        let old_unreached = json!([{ "token": "sam", "why": "nobody" }, { "token": "agent", "why": "may_not_drive" }]);
        let session = Unreached { token: "agent".into(), why: UnreachedWhy::AgentNeedsPane };
        assert_eq!(serde_json::to_value(&session).unwrap(), json!({ "token": "agent", "why": "agent_needs_pane" }));
        let agents = [
            (None, Value::Null),
            (Some(ThreadAgent { delivered: Some(true), error: None }), json!({ "delivered": true })),
            (Some(ThreadAgent { delivered: Some(false), error: None }), json!({ "delivered": false })),
            (Some(ThreadAgent { delivered: None, error: Some("gone".into()) }), json!({ "error": "gone" })),
        ];
        for (agent, old_agent) in agents {
            // Anyone's post: no `invitable`.
            let theirs = ThreadPosted {
                message: m.clone(),
                agent: agent.clone(),
                unreached: unreached.clone(),
                invitable: None,
            };
            assert_eq!(
                serde_json::to_value(&theirs).unwrap(),
                json!({ "message": m, "agent": old_agent, "unreached": old_unreached })
            );
            // The owner's, even with no one to offer.
            for (invitable, old) in [
                (vec![], json!([])),
                (
                    vec![
                        Invitable {
                            token: "sam".into(),
                            who: "tailnet:sam@x".into(),
                            name: "sam".into(),
                            merged: None,
                        },
                        Invitable {
                            token: "al".into(),
                            who: "account:1".into(),
                            name: "al".into(),
                            merged: Some("tailnet:al@x".into()),
                        },
                    ],
                    // `merged` is only inserted when there is one.
                    json!([
                        { "token": "sam", "who": "tailnet:sam@x", "name": "sam" },
                        { "token": "al", "who": "account:1", "name": "al", "merged": "tailnet:al@x" },
                    ]),
                ),
            ] {
                let mine = ThreadPosted {
                    message: m.clone(),
                    agent: agent.clone(),
                    unreached: vec![],
                    invitable: Some(invitable),
                };
                assert_eq!(
                    serde_json::to_value(&mine).unwrap(),
                    json!({ "message": m, "agent": old_agent, "unreached": [], "invitable": old })
                );
            }
        }

        assert_eq!(serde_json::to_value(Empty::default()).unwrap(), json!({}));
        // What the handlers read still has its defaults.
        let post: ThreadPostRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!((post.text.as_str(), post.quote), ("", None));
        assert_eq!(serde_json::from_value::<ThreadReadRequest>(json!({ "upto": 4 })).unwrap().upto, 4);
    }

    /// `invite::run`'s answer.
    #[test]
    fn an_invite_answers_as_it_did() {
        for (delivery, old_delivery, reason, drive) in [
            (InviteDelivery::Sent, "sent", None, None),
            (
                InviteDelivery::Pending,
                "pending",
                Some("control hasn't answered yet; it goes out once it does"),
                Some(true),
            ),
            (InviteDelivery::Unreachable, "unreachable", Some("they haven't turned on notifications"), Some(false)),
        ] {
            let typed = Invited {
                invite: "a1b2c3d4".into(),
                grant: InviteGrant {
                    session: 1,
                    principal: "tailnet:sam@x".into(),
                    name: "sam@x".into(),
                    role: arugula_core::Role::Editor,
                    granted: true,
                },
                pane: 4,
                delivery,
                reason: reason.map(str::to_owned),
                drive,
            };
            let (id, session, principal, name, granted, pane) = ("a1b2c3d4", 1, "tailnet:sam@x", "sam@x", true, 4);
            let role = arugula_core::Role::Editor;
            let old = json!({
                "invite": id,
                "grant": { "session": session, "principal": principal, "name": name, "role": role, "granted": granted },
                "pane": pane,
                "delivery": old_delivery,
                "reason": reason.map(str::to_owned),
                "drive": drive,
            });
            assert_eq!(serde_json::to_value(&typed).unwrap(), old);
            assert_eq!(delivery.as_str(), old_delivery);
        }
        let req: InviteRequest = serde_json::from_value(json!({ "session": 1, "who": "sam" })).unwrap();
        assert_eq!((req.role, req.history, req.whole_thread, req.thread), (None, false, false, None));
    }

    /// `run`, `open_block`, `open_conversation` and `close`.
    #[test]
    fn blocks_answer_as_they_did() {
        assert_eq!(serde_json::to_value(RunResponse { pane: 7 }).unwrap(), json!({ "pane": 7 }));
        let block = 9;
        assert_eq!(serde_json::to_value(OpenResponse { block }).unwrap(), json!({ "block": block }));

        // A conversation opened now, or there already; `error` only when
        // `then` failed (`out["error"] = ...`).
        for (opened, error) in [(true, None), (false, None), (true, Some("it won't start"))] {
            let typed = OpenConversationResponse {
                block,
                opened,
                conversation: "abc-123".into(),
                error: error.map(str::to_owned),
            };
            let mut old = json!({ "block": block, "opened": opened, "conversation": "abc-123" });
            if let Some(e) = error {
                old["error"] = json!(e);
            }
            assert_eq!(serde_json::to_value(&typed).unwrap(), old);
        }

        // `close` answered `json!({})`.
        assert_eq!(serde_json::to_value(Empty {}).unwrap(), json!({}));
        let open: OpenRequest = serde_json::from_value(json!({ "type": "browser" })).unwrap();
        assert!(open.config.is_null() && !open.vm && !open.local);
        let conv: OpenConversationRequest = serde_json::from_value(json!({ "id": "abc" })).unwrap();
        assert_eq!((conv.then, conv.split), (None, None));
    }

    /// `notify_get` and `notify_set` answered the pref as it was.
    #[test]
    fn notify_answers_as_it_did() {
        // `NotifyPref { all: true, ..Default::default() }`: the owner's.
        let owner = NotifyPref { all: true, ..Default::default() };
        assert_eq!(serde_json::to_value(&owner).unwrap(), json!({ "all": true, "sessions": [] }));
        let some = NotifyPref { all: false, sessions: [3, 1].into() };
        assert_eq!(serde_json::to_value(&some).unwrap(), json!({ "all": false, "sessions": [1, 3] }));
        let req: NotifyRequest = serde_json::from_value(json!({ "on": true })).unwrap();
        assert_eq!((req.session, req.on), (None, true));
    }

    /// `team-pins` answered `json!({ "pins": ..., "checked": ... })`.
    #[test]
    fn team_pins_answer_as_they_did() {
        let pins: std::collections::BTreeMap<String, String> = [("t1".to_owned(), "dev.root".to_owned())].into();
        let checked = vec!["t1".to_owned(), "t2".to_owned()];
        let typed = TeamPins { pins: pins.clone(), checked: checked.clone() };
        assert_eq!(serde_json::to_value(&typed).unwrap(), json!({ "pins": pins, "checked": checked }));
        let none = TeamPins::default();
        assert_eq!(serde_json::to_value(&none).unwrap(), json!({ "pins": {}, "checked": [] }));
        let req: TeamPinsRequest = serde_json::from_value(json!({})).unwrap();
        assert!(req.pins.is_empty() && req.drop.is_empty());
        // What an act answers: `error` only when a pane refused.
        let act = ActResponse {
            results: vec![
                ActResult { pane: 1, ok: true, error: None },
                ActResult { pane: 2, ok: false, error: Some("no".into()) },
            ],
        };
        assert_eq!(
            serde_json::to_value(&act).unwrap(),
            json!({ "results": [{ "pane": 1, "ok": true }, { "pane": 2, "ok": false, "error": "no" }] })
        );
    }

    /// `GET /api/host`: `None`s are left out, as its derive always did.
    #[test]
    fn host_answers_as_it_did() {
        let bare = HostInfo {
            name: "box".into(),
            version: "1.2.3".into(),
            protocol: None,
            tailnet_url: None,
            tailnet_seen: false,
            control: None,
            team: None,
            fountain_runner: None,
            features: None,
        };
        assert_eq!(serde_json::to_value(&bare).unwrap(), json!({ "name": "box", "version": "1.2.3" }));
        let full = HostInfo {
            protocol: Some(2),
            tailnet_url: Some("https://box.ts.net".into()),
            tailnet_seen: true,
            control: Some("https://control".into()),
            team: Some("acme".into()),
            fountain_runner: Some(FountainRunnerInfo { name: "r".into(), online: Some(true), ..Default::default() }),
            features: Some(HostFeatures { labs: true, blocks: true, ..Default::default() }),
            ..bare
        };
        assert_eq!(
            serde_json::to_value(&full).unwrap(),
            json!({
                "name": "box", "version": "1.2.3", "protocol": 2, "tailnet_url": "https://box.ts.net",
                "tailnet_seen": true, "control": "https://control", "team": "acme",
                "fountain_runner": { "name": "r", "online": true },
                "features": { "labs": true, "blocks": true, "vms": false, "fountain": false, "studio": false,
                              "threads": false, "calls": false },
            })
        );
    }

    /// What each typed value serializes to, against the literal the daemon
    /// built before (#446).
    fn same<T: Serialize>(typed: &T, old: Value) {
        assert_eq!(serde_json::to_value(typed).unwrap(), old);
    }

    fn driver(name: &str) -> crate::Driver {
        crate::Driver { who: format!("tailnet:{name}"), name: name.into() }
    }

    /// `ask`: `by` is `null` when nobody is named, as `json!` wrote it.
    #[test]
    fn ask_answers_as_it_did() {
        let content = json!({ "Which?": "A" });
        let output = json!({ "hookSpecificOutput": { "decision": "x" } });
        let by = Some(driver("sam"));
        same(
            &AskAnswer::Accept { content: content.clone(), output: output.clone(), by: by.clone() },
            json!({ "action": "accept", "content": content, "output": output, "by": by }),
        );
        same(
            &AskAnswer::Accept { content: content.clone(), output: output.clone(), by: None },
            json!({ "action": "accept", "content": content, "output": output, "by": null }),
        );
        same(
            &AskAnswer::Decline { output: output.clone(), by: by.clone() },
            json!({ "action": "decline", "output": output, "by": by }),
        );
        same(
            &AskAnswer::Decline { output: output.clone(), by: None },
            json!({ "action": "decline", "output": output, "by": null }),
        );
        same(&AskAnswer::Terminal, json!({ "action": "terminal" }));
        same(&AskAnswer::Withdrawn, json!({ "action": "withdrawn" }));
        // The CLI reads them back.
        let back: AskAnswer = serde_json::from_value(json!({ "action": "terminal" })).unwrap();
        assert_eq!(back, AskAnswer::Terminal);
    }

    /// `ask`'s body: a recorded one, and an old client's with only the
    /// questions.
    #[test]
    fn ask_requests_parse_as_they_did() {
        let recorded = json!({
            "questions": [{ "question": "Which?", "options": [{ "label": "A" }] }],
            "id": "toolu_1", "source": "hud", "agent": "hud",
        });
        let req: AskRequest = serde_json::from_value(recorded.clone()).unwrap();
        assert_eq!(req.questions, recorded["questions"]);
        assert_eq!(
            (req.id.as_deref(), req.source.as_deref(), req.agent.as_deref()),
            (Some("toolu_1"), Some("hud"), Some("hud"))
        );
        let old: AskRequest = serde_json::from_value(json!({ "questions": [] })).unwrap();
        assert_eq!((old.id, old.source, old.agent), (None, None, None));
        // Without questions it never parsed.
        assert!(serde_json::from_value::<AskRequest>(json!({ "id": "x" })).is_err());
        // Nor withdraw's: its `id` is optional.
        let w: WithdrawRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(w.id, None);
        let w: WithdrawRequest = serde_json::from_value(json!({ "id": "q1" })).unwrap();
        assert_eq!(w.id.as_deref(), Some("q1"));
        // The CLI sends `{"id": null}` for none; that parsed too.
        let w: WithdrawRequest = serde_json::from_value(json!({ "id": null })).unwrap();
        assert_eq!(w.id, None);
    }

    /// `permit`: the answers, and what its body read (the daemon read
    /// `hook["tool_name"].as_str()` and the like, from any JSON).
    #[test]
    fn permit_answers_and_requests_as_they_did() {
        let allow = json!({ "hookSpecificOutput": { "decision": { "behavior": "allow" } } });
        same(&PermitAnswer::Allow { output: allow.clone() }, json!({ "action": "allow", "output": allow }));
        let deny = json!({ "hookSpecificOutput": { "decision": { "behavior": "deny", "message": "no" } } });
        same(&PermitAnswer::Deny { output: deny.clone() }, json!({ "action": "deny", "output": deny }));
        same(&PermitAnswer::Withdrawn, json!({ "action": "withdrawn" }));

        let recorded = json!({
            "session_id": "abc", "transcript_path": "/t.jsonl", "cwd": "/w", "hook_event_name": "PermissionRequest",
            "tool_name": "Bash", "tool_input": { "command": "ls" },
            "permission_suggestions": [{ "type": "addRules" }],
        });
        let req: PermitRequest = serde_json::from_value(recorded).unwrap();
        assert_eq!(req.tool_name.as_deref(), Some("Bash"));
        assert_eq!(req.tool_input, json!({ "command": "ls" }));
        assert_eq!((req.session_id.as_deref(), req.agent_id.as_deref()), (Some("abc"), None));
        assert_eq!(req.permission_suggestions, Some(json!([{ "type": "addRules" }])));
        // Each field was optional but `tool_name`, which the daemon refused
        // with a 400 of its own; a field of another type read as absent.
        let bare: PermitRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(bare, PermitRequest::default());
        assert_eq!(bare.tool_input, Value::Null);
        let odd: PermitRequest = serde_json::from_value(
            json!({ "tool_name": 3, "session_id": null, "agent_id": ["x"], "permission_suggestions": null }),
        )
        .unwrap();
        assert_eq!(odd, PermitRequest::default());
    }

    /// `inbox`, `followup`, `hook` and the answer to a card on a block.
    #[test]
    fn inbox_followup_and_answers_as_they_did() {
        let by = driver("sam");
        same(
            &InboxAnswer::FollowUp { text: "go on".into(), by: by.clone() },
            json!({ "action": "follow_up", "text": "go on", "by": by }),
        );
        same(&InboxAnswer::Replaced, json!({ "action": "replaced" }));
        same(&FollowedUp { delivered: true }, json!({ "delivered": true }));
        same(&FollowedUp { delivered: false }, json!({ "delivered": false }));
        let req: FollowUpRequest = serde_json::from_value(json!({ "text": "hi" })).unwrap();
        assert_eq!(req.text, "hi");
        assert!(serde_json::from_value::<FollowUpRequest>(json!({})).is_err());
        same(&Answered { answered: "q1".into() }, json!({ "answered": "q1" }));
        // `send`, `keys`, `mouse`, `attention`, `hook`, `ask/withdraw`.
        same(&Empty {}, json!({}));
    }

    /// `secrets`, `diff` and `detection`.
    #[test]
    fn pane_reads_answer_as_they_did() {
        same(&Vec::<SecretFinding>::new(), json!([]));
        same(
            &vec![SecretFinding { pane: 4, kinds: vec!["a GitHub token".into(), "a password".into()] }],
            json!([{ "pane": 4, "kinds": ["a GitHub token", "a password"] }]),
        );
        let info = crate::DiffInfo {
            id: "d1".into(),
            file: "/w/a.rs".into(),
            added: 2,
            removed: 1,
            text: "@@".into(),
            new: false,
            at_ms: 5,
            ide: "arugula".into(),
        };
        same(
            &PaneDiff { diff: info.clone(), old: "a".into(), new: "b".into() },
            json!({ "diff": info, "old": "a", "new": "b" }),
        );
        let rule = |matched| DetectionRule {
            rule: "claude.idle".into(),
            state: "idle".into(),
            priority: 10,
            region: "bottom".into(),
            text: vec!["> ".into()],
            matched,
        };
        let old_rule = |matched| json!({ "rule": "claude.idle", "state": "idle", "priority": 10, "region": "bottom", "text": ["> "], "matched": matched });
        let read = Detection {
            agent: "claude".into(),
            name: "Claude Code".into(),
            shown: Some("idle".into()),
            fired: None,
            title: "t".into(),
            rules: vec![rule(true), rule(false)],
            unread: false,
            configured: None,
        };
        same(
            &DetectionAnswer::Read(read.clone()),
            json!({ "agent": "claude", "name": "Claude Code", "shown": "idle", "fired": null, "title": "t",
                    "rules": [old_rule(true), old_rule(false)] }),
        );
        // Not read here: `unread`, and what chant found configured.
        let unread = Detection {
            shown: None,
            fired: Some("r".into()),
            rules: vec![],
            unread: true,
            configured: Some(vec!["codex".into()]),
            ..read
        };
        same(
            &DetectionAnswer::Read(unread),
            json!({ "agent": "claude", "name": "Claude Code", "shown": null, "fired": "r", "title": "t",
                    "rules": [], "unread": true, "configured": ["codex"] }),
        );
        same(
            &DetectionAnswer::NoAgent(NoDetection { agent: None, command: Some("vim".into()) }),
            json!({ "agent": null, "command": "vim" }),
        );
        same(
            &DetectionAnswer::NoAgent(NoDetection { agent: None, command: None }),
            json!({ "agent": null, "command": null }),
        );
        // Both read back as what they were.
        for a in [
            DetectionAnswer::NoAgent(NoDetection { agent: None, command: Some("vim".into()) }),
            DetectionAnswer::Read(Detection {
                agent: "claude".into(),
                name: "n".into(),
                shown: None,
                fired: None,
                title: String::new(),
                rules: vec![],
                unread: true,
                configured: Some(vec![]),
            }),
        ] {
            let back: DetectionAnswer = serde_json::from_value(serde_json::to_value(&a).unwrap()).unwrap();
            assert_eq!(back, a);
        }
    }

    /// `conversations` (a record kept as the daemon's JSON, and the block
    /// that has it open) and `describe`.
    #[test]
    fn conversations_and_blocks_answer_as_they_did() {
        let c = json!({ "id": "abc", "cwd": "/w", "title": "t", "live": null });
        // The old code set `block` on the conversation's own object.
        let with = |block: Option<PaneId>| {
            let mut v = c.clone();
            v["block"] = json!(block);
            v
        };
        let rows = vec![
            ConversationRow { conversation: c.clone(), block: Some(3) },
            ConversationRow { conversation: c.clone(), block: None },
        ];
        same(
            &ConversationList { conversations: rows, total: 7 },
            json!({ "conversations": [with(Some(3)), with(None)], "total": 7 }),
        );
        same(&ConversationList { conversations: vec![], total: 0 }, json!({ "conversations": [], "total": 0 }));
        let back: ConversationRow = serde_json::from_value(with(Some(3))).unwrap();
        assert_eq!((back.block, &back.conversation), (Some(3), &c));

        let info: PaneSummary = serde_json::from_value(json!({
            "session": 1, "session_name": "s", "tab": 2, "tab_name": null,
            "id": 5, "epoch": 1, "cwd": null, "command": null, "running": true, "policy": { "kind": "shell" },
        }))
        .unwrap();
        let state = json!({ "cwd": "/w", "busy": false, "end": 10, "exited": null, "current": null, "last": null });
        same(&Described { info: info.clone(), state: state.clone() }, json!({ "info": info, "state": state }));
    }

    /// `ide`, `rules`, `shell-env`, `agents` and `adapters`.
    #[test]
    fn ide_rules_and_hosts_answer_as_they_did() {
        same(
            &IdeInfo { on: false, name: None, port: None, lock_dir: None, diffs: None, others: None },
            json!({ "on": false }),
        );
        let others = vec![
            IdeOther { name: "VS Code".into(), port: 5000, pid: Some(9), folders: vec!["/w".into()], alive: true },
            IdeOther { name: "nvim".into(), port: 5001, pid: None, folders: vec![], alive: false },
        ];
        same(
            &IdeInfo {
                on: true,
                name: Some("arugula".into()),
                port: Some(4000),
                lock_dir: Some("/home/u/.claude/ide".into()),
                diffs: Some("arugula".into()),
                others: Some(others.clone()),
            },
            json!({
                "on": true, "name": "arugula", "port": 4000, "lock_dir": "/home/u/.claude/ide", "diffs": "arugula",
                "others": [
                    { "name": "VS Code", "port": 5000, "pid": 9, "folders": ["/w"], "alive": true },
                    { "name": "nvim", "port": 5001, "pid": null, "folders": [], "alive": false },
                ],
            }),
        );
        same(&IdeDiffs { diffs: "nvim".into() }, json!({ "diffs": "nvim" }));
        let set: IdeDiffsRequest = serde_json::from_value(json!({ "diffs": "nvim" })).unwrap();
        assert_eq!(set.diffs, "nvim");
        let m: IdeMentionRequest =
            serde_json::from_value(json!({ "pane": 2, "file": "a.rs", "start": 3, "end": 5 })).unwrap();
        assert_eq!((m.pane, m.file.as_str(), m.start, m.end), (2, "a.rs", 3, 5));
        assert!(serde_json::from_value::<IdeMentionRequest>(json!({ "pane": 2, "file": "a.rs", "start": 3 })).is_err());
        same(&IdeMentioned { sent: 2 }, json!({ "sent": 2 }));

        // A rule is its record (what is `None` is left out), then its index
        // and how it reads.
        let rule = |prefix: Option<&str>, cwd: Option<&str>, sprite: Option<&str>, from: Option<&str>| StandingRule {
            tool: "Bash".into(),
            prefix: prefix.map(Into::into),
            cwd: cwd.map(Into::into),
            sprite: sprite.map(Into::into),
            at_ms: 12,
            from: from.map(Into::into),
            index: 1,
            text: "Bash (any)".into(),
        };
        same(
            &Rules {
                rules: vec![rule(None, None, None, None), rule(Some("git"), Some("/w"), Some("sp"), Some("req"))],
            },
            json!({ "rules": [
                { "tool": "Bash", "at_ms": 12, "index": 1, "text": "Bash (any)" },
                { "tool": "Bash", "prefix": "git", "cwd": "/w", "sprite": "sp", "at_ms": 12, "from": "req",
                  "index": 1, "text": "Bash (any)" },
            ] }),
        );
        same(&Rules { rules: vec![] }, json!({ "rules": [] }));

        same(
            &ShellEnv {
                shell: "/bin/zsh".into(),
                ok: true,
                error: None,
                ms: 40,
                path: Some("/bin".into()),
                vars: vec!["A".into(), "B".into()],
            },
            json!({ "shell": "/bin/zsh", "ok": true, "error": null, "ms": 40, "path": "/bin", "vars": ["A", "B"] }),
        );
        same(
            &ShellEnv {
                shell: "sh".into(),
                ok: false,
                error: Some("timed out".into()),
                ms: 0,
                path: None,
                vars: vec![],
            },
            json!({ "shell": "sh", "ok": false, "error": "timed out", "ms": 0, "path": null, "vars": [] }),
        );

        // The snapshot is the daemon's own; `rules` goes beside it.
        let snapshot = json!({ "state": "read", "chant": "chant", "sites": [], "notes": [] });
        let mut old = snapshot.clone();
        old["rules"] = json!({ "run": ["claude"], "off": ["codex"] });
        same(
            &AgentsInventory {
                inventory: snapshot,
                rules: AgentRules { run: vec!["claude".into()], off: vec!["codex".into()] },
            },
            old,
        );
        let list = vec![json!({ "kind": "claude", "state": "ready" })];
        same(&Adapters { adapters: list.clone() }, json!({ "adapters": list }));
    }

    /// `studio`, `studio/apps`, the follower link and `fountain/agents`.
    #[test]
    fn studio_and_fountain_answer_as_they_did() {
        same(
            &StudioStatus {
                url: Some("https://s.example".into()),
                logged_in: true,
                followers: vec!["pinboard".into()],
            },
            json!({ "url": "https://s.example", "logged_in": true, "followers": ["pinboard"] }),
        );
        same(
            &StudioStatus { url: None, logged_in: false, followers: vec![] },
            json!({ "url": null, "logged_in": false, "followers": [] }),
        );
        let login: StudioLoginRequest = serde_json::from_value(json!({ "url": "u", "token": "t" })).unwrap();
        assert_eq!((login.url.as_str(), login.token.as_str()), ("u", "t"));
        assert!(serde_json::from_value::<StudioLoginRequest>(json!({ "url": "u" })).is_err());
        let a = StudioApp {
            name: "pinboard".into(),
            title: Some("Pinboard".into()),
            url: "https://p.example".into(),
            status: None,
        };
        let old_a = json!({ "name": "pinboard", "title": "Pinboard", "url": "https://p.example", "status": null });
        let bare = StudioApp { name: "b".into(), title: None, url: "u".into(), status: Some("running".into()) };
        let old_bare = json!({ "name": "b", "title": null, "url": "u", "status": "running" });
        same(&StudioLoggedIn { apps: vec![a.clone(), bare.clone()] }, json!({ "apps": [old_a, old_bare] }));
        same(&StudioLoggedIn { apps: vec![] }, json!({ "apps": [] }));
        // Each app, as the old code made it: its record and its `blocks`.
        let row = |mut v: Value, blocks: Vec<PaneId>| {
            v["blocks"] = json!(blocks);
            v
        };
        same(
            &StudioApps {
                studio: Some("https://studio.example".into()),
                apps: vec![StudioAppRow { app: a, blocks: vec![3, 4] }, StudioAppRow { app: bare, blocks: vec![] }],
            },
            json!({ "studio": "https://studio.example", "apps": [row(old_a, vec![3, 4]), row(old_bare, vec![])] }),
        );
        same(&StudioApps { studio: None, apps: vec![] }, json!({ "studio": null, "apps": [] }));
        let link: FollowerLinkRequest = serde_json::from_value(json!({ "link": "https://p.example/join" })).unwrap();
        assert_eq!(link.link, "https://p.example/join");

        let agents = vec![json!({ "id": "a1", "name": "Agent" })];
        same(
            &FountainAgents {
                base_url: "https://fountain.example".into(),
                profile: "default".into(),
                total: 3,
                filter: json!({ "query": "a" }),
                agents: agents.clone(),
                unreadable: 1,
            },
            json!({ "base_url": "https://fountain.example", "profile": "default", "total": 3,
                    "filter": { "query": "a" }, "agents": agents, "unreadable": 1 }),
        );
    }

    /// `push/subscribe` and `push/test`.
    #[test]
    fn push_answers_as_it_did() {
        same(&PushSubscriptions { subscriptions: 0 }, json!({ "subscriptions": 0 }));
        same(&PushSubscriptions { subscriptions: 3 }, json!({ "subscriptions": 3 }));
    }

    /// `machines/N/reset` and `panes/N/share-machine` answer `{}`.
    #[test]
    fn machine_routes_answer_empty() {
        same(&Empty {}, json!({}));
        let back: Empty = serde_json::from_value(json!({})).unwrap();
        assert_eq!(back, Empty {});
    }
}
