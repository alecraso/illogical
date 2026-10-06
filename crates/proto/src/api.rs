//! The HTTP API (`/api/...`), which the `illogical` CLI uses over the
//! daemon's Unix socket and remote agents can use over the tailnet.
//!
//! | method | path | body / query | answer |
//! |---|---|---|---|
//! | GET | `/api/panes` | | `[PaneSummary]` |
//! | POST | `/api/run` | `RunRequest` | `RunResponse`: `{"pane": N}` |
//! | POST | `/api/panes/N/send` | `SendRequest` | `{}` |
//! | POST | `/api/panes/N/prompt` | `PromptRequest` | `PromptResult`: the agent's turn, waited through (#147) |
//! | POST | `/api/panes/N/keys` | `KeysRequest` | `{}` |
//! | POST | `/api/panes/N/mouse` | `MouseRequest` | `{}` |
//! | POST | `/api/panes/N/attention` | `AttentionRequest` | `{}` |
//! | POST | `/api/panes/N/ask` | `{questions, id, source?, agent?}` (AskUserQuestion's, from `illogical ask`; on a browser or app block, whatever follows its page's agent, M35) | when answered: `{action: accept\|decline\|terminal\|withdrawn, content?, output?, by?}` |
//! | POST | `/api/panes/N/ask/withdraw` | `{id}` | `{}`: the asker gave up |
//! | GET | `/api/attention` | | `[AttentionItem]`: every pane that wants you, and why (M24) |
//! | POST | `/api/attention/act` | `ActRequest` | `ActResponse`: one result per pane |
//! | POST | `/api/panes/N/permit` | Claude Code's `PermissionRequest` hook input (`illogical hook`) | when answered: `{action: allow\|deny\|withdrawn, output?}` |
//! | POST | `/api/panes/N/hook` | any other Claude Code hook input | `{}`: closes a permission card the terminal answered |
//! | POST | `/api/panes/N/inbox` | `Stop`/`SessionStart` hook input (`illogical inbox`) | a follow-up: `{action: follow_up\|replaced, text?, by?}` |
//! | POST | `/api/panes/N/followup` | `{text}` | `{delivered}`: the agent's next instruction, from whoever may drive it |
//! | GET, POST | `/api/notify` | POST `NotifyRequest` | `NotifyPref`: which agents' "needs you" notifications reach you (M29) |
//! | POST | `/api/invite` | `InviteRequest` | `Invited`: share a session and push that person alone (#233; the owner's) |
//! | GET, POST | `/api/team-pins` | POST `{pins: {team: "<founder>.<founder's root>"}}` | `{pins, checked}`: teams the owner's browser pinned, whose rosters this machine checked (#233; the owner's) |
//! | POST | `/api/panes/N/close` | | `Empty` (its output stays in history) |
//! | POST | `/api/blocks` | `OpenRequest` | `OpenResponse`: `{"block": N}` |
//! | POST | `/api/conversations/open` | `OpenConversationRequest` | `OpenConversationResponse`: a Claude Code conversation as an agent block (M33) |
//! | GET | `/api/blocks/N` | | `{info, state}`: `describe` |
//! | POST | `/api/blocks/N/call/METHOD` | JSON args | the method's answer |
//! | GET, POST, DELETE | `/api/studio` | POST `{url, token}` | the studio and whether there's a token (never the token); POST logs in (`{apps}`), DELETE forgets it (M35) |
//! | GET | `/api/studio/apps` | | `{studio, apps: [{name, title, url, status, blocks}]}` |
//! | PUT, DELETE | `/api/studio/followers/APP` | PUT `{link}` | `{}`: a hud follower link for the app's box |
//! | GET | `/api/machines` | | `[Machine]` |
//! | POST | `/api/machines/N/reset` | | `{}`: delete and recreate it; its panes restart by policy |
//! | POST | `/api/panes/N/share-machine` | | `{}`: the pane's machine now belongs to its tab |
//! | GET | `/api/panes/N/capture` | `format=text\|ansi\|html`, `scope=screen\|scrollback\|last-command` | text |
//! | GET | `/api/panes/N/process` | | `Process` |
//! | GET | `/api/panes/N/detection` | | how its agent's screen reads, rule by rule (#145) |
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
//! | GET | `/api/hosts/self/shell-env` | | `{shell, ok, error, ms, path, vars}`: the shell environment blocks that run your tools get (#74) |
//! | POST | `/api/hosts/self/shell-env/refresh` | | the same, resolved again |
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
    /// Where the request comes from (`$ILLOGICAL_PANE`): the default session
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

/// A handoff in a pane: from here on, `who` typed (`illogical log --who`).
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
    pub role: Option<illogical_core::Role>,
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
    pub role: illogical_core::Role,
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
                    role: illogical_core::Role::Editor,
                    granted: true,
                },
                pane: 4,
                delivery,
                reason: reason.map(str::to_owned),
                drive,
            };
            let (id, session, principal, name, granted, pane) = ("a1b2c3d4", 1, "tailnet:sam@x", "sam@x", true, 4);
            let role = illogical_core::Role::Editor;
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
}
