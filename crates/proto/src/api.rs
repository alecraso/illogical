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
//! | GET | `/api/panes/N/tail` | `from=OFFSET\|last-command`, `follow=1`, `text=1` | bytes (streamed with follow) |
//! | GET | `/api/panes/N/wait` | `until=command-end\|exit\|match`, `re=`, `timeout=` secs | `WaitResult` |
//! | GET | `/api/panes/N/export.cast` | | asciicast v3 |
//! | GET | `/api/events` | `pane=`, `type=a,b`, `follow=1` | NDJSON `Event`s |
//! | GET | `/api/history` | `pane=`, `failed=1`, `since=` secs, `cwd=`, `match=` | `[HistoryEntry]` |
//! | GET | `/api/search` | `re=`, `since=` secs | `[SearchHit]` |
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
    CommandEnd { text: Option<String>, exit: Option<i32>, start: u64, end: Option<u64> },
    Exit { code: Option<i32> },
    Match { text: String, offset: u64 },
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
}
