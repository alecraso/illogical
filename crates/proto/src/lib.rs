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
    ClientId, Dir, Edge, Intent, Layout, Node, NodeId, PaneId, Rect, Session, SessionId, SplitRect, TabId,
};

pub mod api;

/// Control messages from a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Start (or resume) receiving panes. The server replays from each
    /// pane's offset when it still has the bytes, otherwise it sends a
    /// snapshot.
    Attach { panes: Vec<AttachPane> },
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PaneOp {
    /// What happens to the pane when the daemon starts again (after a
    /// reboot).
    SetPolicy { policy: Policy },
    /// Delete the pane's saved history and clear its scrollback.
    Purge,
    /// Shell integration (command marks, exit codes, cwd) for shells started
    /// in this pane from now on.
    SetIntegration { on: bool },
    /// Set the pane's attention state (a client dismissing a badge).
    Attention { state: Attention },
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
    /// It asked for you (a notification, a bell, an agent's hook) or an agent
    /// went quiet mid-command.
    NeedsInput,
    /// A long command finished while nobody was looking.
    Done,
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
    #[serde(default)]
    pub image: Option<String>,
    /// What it belongs to; the machine goes when that closes.
    pub owner: Owner,
    #[serde(default)]
    pub state: MachineState,
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

/// What a block is. Only terminals exist so far; where one runs is its
/// `host`, not its type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockType {
    #[default]
    Terminal,
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
    /// Shell integration for shells started in this pane.
    #[serde(default = "yes")]
    pub integration: bool,
    #[serde(default, rename = "type")]
    pub kind: BlockType,
    /// The machine it runs on; `None` is this host.
    #[serde(default)]
    pub host: Option<MachineId>,
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
    /// The client resets its terminal before writing them.
    Snapshot = 2,
    /// Client -> server: input for the pane (`offset` is unused).
    Input = 3,
}

impl TryFrom<u8> for FrameKind {
    type Error = DecodeError;
    fn try_from(v: u8) -> Result<Self, DecodeError> {
        match v {
            1 => Ok(Self::Output),
            2 => Ok(Self::Snapshot),
            3 => Ok(Self::Input),
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
        let panes = vec![AttachPane { pane: 1, offset: None }, AttachPane { pane: 2, offset: Some(42) }];
        assert_eq!(m, ClientMsg::Attach { panes });
        let s = serde_json::to_string(&ServerMsg::Resync { pane: 3 }).unwrap();
        assert_eq!(s, r#"{"type":"resync","pane":3}"#);
        let m: ClientMsg =
            serde_json::from_str(r#"{"type":"intent","id":4,"intent":{"op":"split","pane":1,"edge":"bottom"}}"#)
                .unwrap();
        assert_eq!(
            m,
            ClientMsg::Intent { id: Some(4), intent: Intent::Split { pane: 1, edge: Edge::Bottom, local: false } }
        );
    }
}
