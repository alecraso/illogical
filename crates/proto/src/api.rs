//! The HTTP API (`/api/...`), which the `illogical` CLI uses over the
//! daemon's Unix socket and remote agents can use over the tailnet.
//!
//! | method | path | body / query | answer |
//! |---|---|---|---|
//! | GET | `/api/panes` | | `[PaneSummary]` |
//! | POST | `/api/run` | `RunRequest` | `{"pane": N}` |
//! | POST | `/api/panes/N/send` | `SendRequest` | `{}` |
//! | POST | `/api/panes/N/keys` | `KeysRequest` | `{}` |
//! | POST | `/api/panes/N/mouse` | `MouseRequest` | `{}` |
//! | POST | `/api/panes/N/attention` | `AttentionRequest` | `{}` |
//! | POST | `/api/panes/N/close` | | `{}` (its output stays in history) |
//! | POST | `/api/blocks` | `OpenRequest` | `{"block": N}` |
//! | GET | `/api/blocks/N` | | `{info, state}`: `describe` |
//! | POST | `/api/blocks/N/call/METHOD` | JSON args | the method's answer |
//! | GET | `/api/machines` | | `[Machine]` |
//! | POST | `/api/machines/N/reset` | | `{}`: delete and recreate it; its panes restart by policy |
//! | POST | `/api/panes/N/share-machine` | | `{}`: the pane's machine now belongs to its tab |
//! | GET | `/api/panes/N/capture` | `format=text\|ansi\|html`, `scope=screen\|scrollback\|last-command` | text |
//! | GET | `/api/panes/N/process` | | `Process` |
//! | GET | `/api/panes/N/tail` | `from=OFFSET\|last-command`, `follow=1`, `text=1` | bytes (streamed with follow); other blocks: their text |
//! | GET | `/api/panes/N/wait` | `until=command-end\|exit\|match\|idle\|needs-input`, `re=`, `timeout=` secs | `WaitResult` |
//! | GET | `/api/panes/N/export.cast` | | asciicast v3 |
//! | GET | `/api/events` | `pane=`, `type=a,b`, `follow=1` | NDJSON `Event`s |
//! | GET | `/api/history` | `pane=`, `failed=1`, `since=` secs, `cwd=`, `match=` | `[HistoryEntry]` |
//! | GET | `/api/search` | `re=`, `since=` secs | `[SearchHit]` |
//! | GET | `/api/host` | | `HostInfo`: this daemon's name and version |
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
pub struct OpenRequest {
    #[serde(rename = "type")]
    pub kind: crate::BlockType,
    /// What the type needs to make it (a URL, an agent command).
    #[serde(default)]
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
    pub local: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunRequest {
    /// Run with the pane's shell (`$SHELL -l -c COMMAND`); none for just a
    /// shell.
    #[serde(default)]
    pub command: Option<String>,
    /// Run it on a new throwaway machine owned by the pane.
    #[serde(default)]
    pub vm: bool,
    /// In a new tab whose panes all share a new throwaway machine.
    #[serde(default)]
    pub vm_tab: bool,
    /// The machine's image (the provider's default if none).
    #[serde(default)]
    pub image: Option<String>,
    /// Session name or id; created if no session has that name. Default: the
    /// session of `from_pane`, else the first.
    #[serde(default)]
    pub session: Option<String>,
    /// Split this pane instead of opening a tab.
    #[serde(default)]
    pub split: Option<PaneId>,
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
    /// `until=idle` (no longer working) or `until=needs-input`: where it got.
    Attention {
        state: Attention,
    },
    Timeout,
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
}

/// `POST /api/shares`: a read-only link to one terminal pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareRequest {
    pub pane: PaneId,
    /// Seconds until it expires [default: an hour; at most a week].
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

/// A read-only share of one pane. `token`, `path` and `url` are only in the
/// answer that minted it; the daemon keeps a hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
