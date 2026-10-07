//! The front end's state: what the daemon says (its `State`, and each
//! pane's output in a mirror terminal), what the tmux client has been told,
//! and turning one into the other as `%` notifications.
//!
//! **Capture and stream line up** as they do in tmux: each pane's mirror
//! holds exactly the bytes that have gone out as `%output` (or that a
//! `capture-pane` returned), so a capture answered from the mirror is
//! followed by the output after it, nothing lost and nothing twice. While a
//! command line runs, frames from the daemon wait in an inbox; they are fed
//! to the mirrors and sent after the line's last `%end`, with the other
//! notifications.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    io::ErrorKind,
    time::{Duration, Instant},
};

use anyhow::Context;
use arugula_proto::{
    AttachPane, BlockType, ClientId, ClientMsg, Frame, FrameKind, PaneId, PaneInfo, ServerMsg, SessionId, State, TabId,
    TabView,
};
use arugula_vt::{GhosttyEngine, VtEngine};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use tungstenite::{Message, WebSocket};

use super::layout;
use crate::http::{Stream, Target};

/// Scrollback each mirror keeps (tmux's default `history-limit`).
pub const HISTORY: usize = 2000;
/// The version clients are told. HTM reports it, and the Ghostty and WezTerm
/// branches were tested against it; iTerm2 turns on pause mode, per-window
/// sizes and `-N` captures for it.
pub const VERSION: &str = "3.5a";
/// `%output` lines carry at most this much raw output (Ghostty drops lines
/// over 1 MiB; escaping can quadruple it).
const OUTPUT_CHUNK: usize = 64 * 1024;
/// A non-terminal block is redrawn at most this often.
const BLOCK_REDRAW: Duration = Duration::from_millis(500);

pub struct Conn {
    ws: WebSocket<Box<dyn Stream>>,
}

/// What the daemon sent.
pub enum In {
    Msg(ServerMsg),
    Frame(Frame),
}

impl Conn {
    pub fn open(target: &Target) -> anyhow::Result<(Self, ClientId, State)> {
        let stream = target.connect()?;
        let (mut ws, _) = tungstenite::client(target.ws_url(), stream).map_err(|e| match e {
            tungstenite::HandshakeError::Failure(e) => anyhow::Error::from(e).context("websocket handshake"),
            tungstenite::HandshakeError::Interrupted(_) => anyhow::anyhow!("websocket handshake interrupted"),
        })?;
        let (client, state) = loop {
            if let Message::Text(t) = ws.read()?
                && let Ok(ServerMsg::Hello { client, state, .. }) = serde_json::from_str(&t)
            {
                break (client, state);
            }
        };
        ws.get_mut().set_nonblocking(true)?;
        Ok((Self { ws }, client, state))
    }

    pub fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.ws.get_ref().fd()
    }

    fn queue(&mut self, msg: Message) -> anyhow::Result<()> {
        match self.ws.write(msg) {
            Ok(()) => Ok(()),
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn send(&mut self, msg: &ClientMsg) -> anyhow::Result<()> {
        self.queue(Message::Text(serde_json::to_string(msg)?.into()))?;
        self.flush()
    }

    pub fn input(&mut self, pane: PaneId, data: Vec<u8>) -> anyhow::Result<()> {
        let f = Frame { kind: FrameKind::Input, pane, offset: 0, data };
        self.queue(Message::Binary(f.encode().into()))?;
        self.flush()
    }

    pub fn flush(&mut self) -> anyhow::Result<()> {
        match self.ws.flush() {
            Ok(()) => Ok(()),
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// The next message, or `None` if nothing is waiting.
    pub fn read(&mut self) -> anyhow::Result<Option<In>> {
        loop {
            match self.ws.read() {
                Ok(Message::Text(t)) => {
                    if let Ok(m) = serde_json::from_str(&t) {
                        return Ok(Some(In::Msg(m)));
                    }
                }
                Ok(Message::Binary(b)) => {
                    if let Ok(f) = Frame::decode(&b) {
                        return Ok(Some(In::Frame(f)));
                    }
                }
                Ok(Message::Close(_)) => anyhow::bail!("the daemon closed the connection"),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
                Err(e) => return Err(e).context("reading from the daemon"),
            }
        }
    }

    /// Wait up to `timeout` for the socket to be readable.
    pub fn wait(&self, timeout: Duration) -> anyhow::Result<()> {
        let mut fds = [PollFd::new(self.fd(), PollFlags::POLLIN)];
        let ms = timeout.as_millis().min(u16::MAX as u128) as u16;
        poll(&mut fds, PollTimeout::from(ms))?;
        Ok(())
    }
}

/// A pane's output stream: frames, and size changes in order with them.
#[derive(Debug, Clone)]
enum Data {
    Frame(Frame),
    Size { pane: PaneId, cols: u16, rows: u16 },
}

impl Data {
    fn pane(&self) -> PaneId {
        match self {
            Data::Frame(f) => f.pane,
            Data::Size { pane, .. } => *pane,
        }
    }

    fn is_snapshot(&self) -> bool {
        matches!(self, Data::Frame(f) if f.kind == FrameKind::Snapshot)
    }
}

/// Whether a pane's output goes to the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    On,
    /// Paused (`refresh-client -A %N:pause`, or `%pause` after the daemon
    /// dropped us): the mirror keeps up, nothing is sent until `continue`.
    Paused,
    /// Dropped by the daemon for a client without pause mode: the fresh
    /// snapshot goes out as `%output` when it comes.
    Resync,
}

pub struct PaneView {
    pub kind: BlockType,
    pub mirror: Option<GhosttyEngine>,
    pub size: (u16, u16),
    /// Stream offset just past the last byte in the mirror.
    pub next: u64,
    pub live: Live,
}

impl PaneView {
    fn new(kind: BlockType, size: (u16, u16)) -> Self {
        Self { kind, mirror: None, size, next: 0, live: Live::On }
    }
}

/// What the tmux client has been told about one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToldWindow {
    pub name: String,
    /// `layout visible-layout flags`, as in `%layout-change`.
    pub layout: String,
    pub panes: Vec<PaneId>,
    pub active: Option<PaneId>,
}

#[derive(Debug, Clone, Default)]
pub struct Told {
    pub sessions: Vec<(SessionId, String)>,
    pub windows: BTreeMap<TabId, ToldWindow>,
    pub order: Vec<TabId>,
    pub active: Option<TabId>,
}

/// Flags the client set with `refresh-client -f`.
#[derive(Debug, Clone, Default)]
pub struct ClientFlags {
    /// `pause-after`: `%extended-output`, and `%pause` when we fall behind.
    pub pause_after: Option<u32>,
    pub wait_exit: bool,
    pub no_output: bool,
}

pub struct Front {
    pub conn: Conn,
    pub target: Target,
    /// `-CC` (iTerm2) rather than `-C`.
    pub dcs: bool,
    pub client: ClientId,
    pub state: State,
    pub session: SessionId,
    pub told: Told,
    /// The current window per session, and the one before it.
    pub active_tab: HashMap<SessionId, TabId>,
    pub last_tab: HashMap<SessionId, TabId>,
    /// The active pane per window.
    pub active_pane: HashMap<TabId, PaneId>,
    pub panes: HashMap<PaneId, PaneView>,
    /// Output held while a command line runs.
    inbox: VecDeque<Data>,
    /// Inside a command line: notifications wait for its last `%end`.
    pub busy: bool,
    /// Notifications to send after the current command line.
    pub notes: Vec<u8>,
    /// Everything for the client, written out by the main loop.
    pub out: Vec<u8>,
    pub flags: ClientFlags,
    /// Sizes the client asked for: per window, and for new ones.
    pub sizes: HashMap<TabId, (u16, u16)>,
    pub default_size: Option<(u16, u16)>,
    pub zoom: HashMap<TabId, Option<PaneId>>,
    pub cmd_no: u64,
    next_ping: u64,
    pongs: HashSet<u64>,
    errors: HashMap<u64, String>,
    /// Windows whose layout goes out again even if unchanged (a
    /// `select-layout` that couldn't be applied snaps back).
    pub force_layout: HashSet<TabId>,
    /// `refresh-client -B` subscriptions: name, format, last value sent.
    pub subscriptions: BTreeMap<String, (String, Option<String>)>,
    blocks_dirty: HashSet<PaneId>,
    last_block_draw: Option<Instant>,
    /// Set when the client is to be let go, with the `%exit` reason.
    pub exit: Option<String>,
}

impl Front {
    pub fn new(conn: Conn, target: Target, dcs: bool, client: ClientId, state: State) -> Self {
        Self {
            conn,
            target,
            dcs,
            client,
            state,
            session: 0,
            told: Told::default(),
            active_tab: HashMap::new(),
            last_tab: HashMap::new(),
            active_pane: HashMap::new(),
            panes: HashMap::new(),
            inbox: VecDeque::new(),
            busy: false,
            notes: Vec::new(),
            out: Vec::new(),
            flags: ClientFlags::default(),
            sizes: HashMap::new(),
            default_size: None,
            zoom: HashMap::new(),
            cmd_no: 0,
            next_ping: 1,
            pongs: HashSet::new(),
            errors: HashMap::new(),
            force_layout: HashSet::new(),
            subscriptions: BTreeMap::new(),
            blocks_dirty: HashSet::new(),
            last_block_draw: None,
            exit: None,
        }
    }

    pub fn conn_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.conn.fd()
    }

    // ---- the daemon's state, looked up

    pub fn tab(&self, id: TabId) -> Option<&TabView> {
        self.state.tabs.iter().find(|t| t.id == id)
    }

    pub fn info(&self, pane: PaneId) -> Option<&PaneInfo> {
        self.state.panes.iter().find(|p| p.id == pane)
    }

    pub fn session_tabs(&self, session: SessionId) -> Vec<TabId> {
        self.state.sessions.iter().find(|s| s.id == session).map(|s| s.tabs.clone()).unwrap_or_default()
    }

    pub fn session_name(&self, session: SessionId) -> String {
        self.state.sessions.iter().find(|s| s.id == session).map(|s| s.name.clone()).unwrap_or_default()
    }

    pub fn tab_of(&self, pane: PaneId) -> Option<TabId> {
        self.state.tabs.iter().find(|t| t.root.contains(pane)).map(|t| t.id)
    }

    pub fn session_of(&self, tab: TabId) -> Option<SessionId> {
        self.state.sessions.iter().find(|s| s.tabs.contains(&tab)).map(|s| s.id)
    }

    /// The current window of `session`, mended if it went away.
    pub fn current_tab(&self, session: SessionId) -> Option<TabId> {
        let tabs = self.session_tabs(session);
        self.active_tab.get(&session).copied().filter(|t| tabs.contains(t)).or_else(|| tabs.first().copied())
    }

    pub fn current_pane(&self, tab: TabId) -> Option<PaneId> {
        let t = self.tab(tab)?;
        self.active_pane.get(&tab).copied().filter(|p| t.root.contains(*p)).or_else(|| t.root.panes().first().copied())
    }

    /// A window's name as tmux would show it: its own, or (automatic-rename)
    /// what runs in its active pane. Never with spaces: WezTerm splits
    /// `list-windows` on them.
    pub fn window_name(&self, tab: TabId) -> String {
        let Some(t) = self.tab(tab) else { return String::new() };
        let name = match &t.name {
            Some(n) => n.clone(),
            None => match self.current_pane(tab).and_then(|p| self.info(p)) {
                Some(i) if i.kind != BlockType::Terminal => format!("{:?}", i.kind).to_lowercase(),
                Some(i) => match &i.command {
                    Some(c) => {
                        let first = c.split_whitespace().next().unwrap_or("");
                        first.rsplit('/').next().unwrap_or(first).to_owned()
                    }
                    None => shell_name(),
                },
                None => shell_name(),
            },
        };
        let name: String = name.chars().map(|c| if c.is_whitespace() { '_' } else { c }).collect();
        if name.is_empty() { "shell".into() } else { name }
    }

    pub fn window_flags(&self, tab: TabId) -> String {
        let session = self.session_of(tab).unwrap_or(self.session);
        let mut f = String::new();
        if self.current_tab(session) == Some(tab) {
            f.push('*');
        } else if self.last_tab.get(&session) == Some(&tab) {
            f.push('-');
        }
        if self.tab(tab).is_some_and(|t| t.zoom.is_some()) {
            f.push('Z');
        }
        f
    }

    /// `layout visible-layout`: the whole tree at the window's size, and
    /// what shows (one pane, while zoomed).
    pub fn layouts(&self, tab: TabId) -> (String, String) {
        let Some(t) = self.tab(tab) else { return (String::new(), String::new()) };
        let full = layout::to_tmux(&t.root, t.cols, t.rows);
        let visible = match t.zoom {
            Some(p) => layout::single(p, t.cols, t.rows),
            None => full.clone(),
        };
        (full, visible)
    }

    // ---- talking to the daemon

    /// Send `msgs`, then wait until the daemon has handled them. Errors for
    /// any of them (by intent id) come back.
    pub fn sync(&mut self, msgs: Vec<ClientMsg>) -> Result<(), String> {
        let mut ids = vec![];
        for m in msgs {
            let m = match m {
                ClientMsg::Intent { intent, .. } => {
                    let id = self.ping_id();
                    ids.push(id);
                    ClientMsg::Intent { id: Some(id), intent }
                }
                m => m,
            };
            self.conn.send(&m).map_err(|e| e.to_string())?;
        }
        let ping = self.ping_id();
        self.conn.send(&ClientMsg::Ping { id: ping }).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.pongs.remove(&ping) {
            if Instant::now() > deadline {
                return Err("the daemon didn't answer".into());
            }
            self.pump(Duration::from_millis(100)).map_err(|e| e.to_string())?;
        }
        match ids.iter().find_map(|id| self.errors.remove(id)) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn ping_id(&mut self) -> u64 {
        self.next_ping += 1;
        self.next_ping
    }

    /// Read what the daemon has sent, waiting up to `timeout` for some.
    pub fn pump(&mut self, timeout: Duration) -> anyhow::Result<()> {
        if let Some(m) = self.conn.read()? {
            self.on_daemon(m)?;
        } else {
            self.conn.wait(timeout)?;
        }
        while let Some(m) = self.conn.read()? {
            self.on_daemon(m)?;
        }
        Ok(())
    }

    pub fn on_daemon(&mut self, m: In) -> anyhow::Result<()> {
        match m {
            In::Frame(f) => self.data(Data::Frame(f)),
            // Sizes travel with the output; they stay in order with it.
            In::Msg(ServerMsg::Size { pane, cols, rows }) => self.data(Data::Size { pane, cols, rows }),
            In::Msg(ServerMsg::State { state }) => {
                self.state = state;
                self.sync_panes()?;
                if !self.busy {
                    self.notify();
                }
            }
            In::Msg(ServerMsg::Delta { delta }) => {
                self.state.apply(&delta);
                self.sync_panes()?;
                if !self.busy {
                    self.notify();
                }
            }
            In::Msg(ServerMsg::Resync { pane }) => self.resync(pane)?,
            In::Msg(ServerMsg::Error { id: Some(id), message }) => {
                self.errors.insert(id, message);
            }
            In::Msg(ServerMsg::Pong { id }) => {
                self.pongs.insert(id);
            }
            In::Msg(ServerMsg::Block { block, .. }) => {
                if self.panes.contains_key(&block) {
                    self.blocks_dirty.insert(block);
                }
            }
            In::Msg(_) => {}
        }
        Ok(())
    }

    /// Fell behind on `pane`: the daemon dropped us from it. Attach again
    /// for a fresh snapshot; the client is told as its flags allow.
    fn resync(&mut self, pane: PaneId) -> anyhow::Result<()> {
        let Some(pv) = self.panes.get_mut(&pane) else { return Ok(()) };
        if pv.live == Live::On {
            if self.flags.pause_after.is_some() {
                pv.live = Live::Paused;
                let note = format!("%pause %{pane}\n");
                self.note(note.as_bytes());
            } else {
                pv.live = Live::Resync;
            }
        }
        self.conn.send(&ClientMsg::Attach {
            panes: vec![mirror_attach(pane)],
            zstd: false,
            acks: false,
            kitty_keys: false,
        })
    }

    /// Follow the attached session's panes: attach new terminals, forget
    /// panes that left.
    pub fn sync_panes(&mut self) -> anyhow::Result<()> {
        let mut want: HashMap<PaneId, (BlockType, (u16, u16))> = HashMap::new();
        for tab in self.session_tabs(self.session) {
            let Some(t) = self.tab(tab) else { continue };
            for (p, r) in &t.layout.panes {
                want.insert(*p, (BlockType::Terminal, (r.cols, r.rows)));
            }
            // Panes hidden behind a zoom keep the size they had.
            for p in t.root.panes() {
                want.entry(p).or_insert((BlockType::Terminal, (t.cols, t.rows)));
            }
        }
        for (p, (kind, _)) in want.iter_mut() {
            if let Some(i) = self.info(*p) {
                *kind = i.kind;
            }
        }
        let gone: Vec<PaneId> = self.panes.keys().filter(|p| !want.contains_key(p)).copied().collect();
        let still: Vec<PaneId> =
            gone.iter().filter(|p| self.state.panes.iter().any(|i| i.id == **p)).copied().collect();
        for p in gone {
            self.panes.remove(&p);
            self.blocks_dirty.remove(&p);
        }
        if !still.is_empty() {
            self.conn.send(&ClientMsg::Detach { panes: still })?;
        }
        let mut attach = vec![];
        for (p, (kind, size)) in want {
            if self.panes.contains_key(&p) {
                continue;
            }
            self.panes.insert(p, PaneView::new(kind, size));
            if kind == BlockType::Terminal {
                attach.push(mirror_attach(p));
            } else {
                self.blocks_dirty.insert(p);
            }
        }
        if !attach.is_empty() {
            self.conn.send(&ClientMsg::Attach { panes: attach, zstd: false, acks: false, kitty_keys: false })?;
        }
        // Blocks follow their pane's size.
        let sizes: Vec<(PaneId, (u16, u16))> =
            self.state.tabs.iter().flat_map(|t| t.layout.panes.iter().map(|(p, r)| (*p, (r.cols, r.rows)))).collect();
        for (p, size) in sizes {
            if let Some(pv) = self.panes.get_mut(&p)
                && pv.kind != BlockType::Terminal
                && pv.size != size
            {
                pv.size = size;
                self.blocks_dirty.insert(p);
            }
        }
        Ok(())
    }

    fn data(&mut self, d: Data) {
        if self.busy {
            self.inbox.push_back(d);
        } else {
            self.apply(d, true);
        }
    }

    /// Feed output to its pane's mirror and, if `emit`, send it on.
    fn apply(&mut self, d: Data, emit: bool) {
        let no_output = self.flags.no_output;
        let Some(pv) = self.panes.get_mut(&d.pane()) else { return };
        let f = match d {
            Data::Size { cols, rows, .. } => {
                pv.size = (cols, rows);
                if let Some(m) = &mut pv.mirror {
                    m.resize(cols, rows);
                }
                return;
            }
            Data::Frame(f) => f,
        };
        match f.kind {
            // Never asked for compressed snapshots.
            FrameKind::Input | FrameKind::SnapshotZstd => {}
            FrameKind::Snapshot => {
                let mut m = GhosttyEngine::mirror(pv.size.0.max(1), pv.size.1.max(1), HISTORY);
                m.feed(&f.data);
                let _ = m.take_replies();
                pv.mirror = Some(m);
                pv.next = f.offset;
                if pv.live == Live::Resync {
                    pv.live = Live::On;
                    if emit && !no_output {
                        let mut data = b"\x1bc".to_vec();
                        data.extend_from_slice(&f.data);
                        let pane = f.pane;
                        self.output(pane, &data);
                    }
                }
            }
            FrameKind::Output => {
                let Some(m) = &mut pv.mirror else { return };
                // Only what's past the mirror (a frame can overlap a fresh
                // snapshot).
                let end = f.offset + f.data.len() as u64;
                if end <= pv.next {
                    return;
                }
                let skip = pv.next.saturating_sub(f.offset) as usize;
                let data = &f.data[skip..];
                m.feed(data);
                let _ = m.take_replies();
                pv.next = end;
                if emit && pv.live == Live::On && !no_output {
                    let data = data.to_vec();
                    self.output(f.pane, &data);
                }
            }
        }
    }

    /// `%output` (or `%extended-output` in pause mode), octal-escaped, in
    /// lines of bounded length.
    pub fn output(&mut self, pane: PaneId, data: &[u8]) {
        for chunk in data.chunks(OUTPUT_CHUNK) {
            let mut line = match self.flags.pause_after {
                Some(_) => format!("%extended-output %{pane} 0 : ").into_bytes(),
                None => format!("%output %{pane} ").into_bytes(),
            };
            escape(chunk, &mut line);
            line.push(b'\n');
            self.note(&line);
        }
    }

    /// A notification: now, or after the command line that's running.
    pub fn note(&mut self, line: &[u8]) {
        if self.busy {
            self.notes.extend_from_slice(line);
        } else {
            self.out.extend_from_slice(line);
        }
    }

    /// The mirror for `pane`, waiting for its first snapshot if need be
    /// (what arrived for it so far goes into the mirror unsent: the caller
    /// is about to hand it over as a capture).
    pub fn mirror(&mut self, pane: PaneId) -> Option<&GhosttyEngine> {
        let kind = self.panes.get(&pane)?.kind;
        if kind != BlockType::Terminal {
            if self.panes.get(&pane)?.mirror.is_none() {
                self.draw_block(pane, false);
            }
            return self.panes.get(&pane)?.mirror.as_ref();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.panes.get(&pane).is_some_and(|p| p.mirror.is_none()) {
            if self.inbox.iter().any(|d| d.pane() == pane && d.is_snapshot()) {
                let mine: Vec<Data> = self.inbox.iter().filter(|d| d.pane() == pane).cloned().collect();
                self.inbox.retain(|d| d.pane() != pane);
                for d in mine {
                    self.apply(d, false);
                }
                break;
            }
            if Instant::now() > deadline || self.pump(Duration::from_millis(50)).is_err() {
                break;
            }
        }
        self.panes.get(&pane)?.mirror.as_ref()
    }

    /// After a command line: hand over what was held, then what changed.
    pub fn settle(&mut self) {
        self.busy = false;
        let held = std::mem::take(&mut self.inbox);
        let notes = std::mem::take(&mut self.notes);
        self.out.extend(notes);
        for d in held {
            self.apply(d, true);
        }
        self.notify();
    }

    /// Redraw non-terminal blocks that changed (at most every half second).
    pub fn tick(&mut self) {
        if self.blocks_dirty.is_empty() || self.last_block_draw.is_some_and(|t| t.elapsed() < BLOCK_REDRAW) {
            return;
        }
        self.last_block_draw = Some(Instant::now());
        for p in std::mem::take(&mut self.blocks_dirty) {
            self.draw_block(p, true);
        }
    }

    /// A non-terminal block as a read-only pane: its `capture --text`,
    /// with a line saying where to use it.
    fn draw_block(&mut self, pane: PaneId, emit: bool) {
        let Some(pv) = self.panes.get(&pane) else { return };
        let (cols, rows) = (pv.size.0.max(1), pv.size.1.max(1));
        let text = crate::http::request(&self.target, "GET", &format!("/api/panes/{pane}/capture"), None)
            .and_then(|r| r.text())
            .unwrap_or_default();
        let mut bytes = b"\x1b[H\x1b[2J".to_vec();
        let keep = rows.saturating_sub(1) as usize;
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines[lines.len().saturating_sub(keep)..].iter().enumerate() {
            if i > 0 {
                bytes.extend_from_slice(b"\r\n");
            }
            let l: String = l.chars().filter(|c| !c.is_control()).take(cols as usize).collect();
            bytes.extend_from_slice(l.as_bytes());
        }
        let hint = format!("[%{pane} is a block: open it in the Arugula web app]");
        let hint: String = hint.chars().take(cols as usize).collect();
        bytes.extend_from_slice(format!("\x1b[{rows};1H\x1b[2m{hint}\x1b[0m").as_bytes());
        let mut m = GhosttyEngine::mirror(cols, rows, HISTORY);
        m.feed(&bytes);
        let _ = m.take_replies();
        let pv = self.panes.get_mut(&pane).expect("checked above");
        pv.mirror = Some(m);
        if emit && pv.live == Live::On && !self.flags.no_output {
            let mut data = b"\x1bc".to_vec();
            data.extend(bytes);
            self.output(pane, &data);
        }
    }

    // ---- notifications

    /// What changed since the client was last told, as `%` notifications.
    pub fn notify(&mut self) {
        // The attached session went away: to another, or the end.
        if !self.state.sessions.iter().any(|s| s.id == self.session) {
            match self.state.sessions.first().map(|s| s.id) {
                Some(s) => {
                    let _ = self.switch_session(s);
                }
                None => {
                    self.exit.get_or_insert_with(|| "server exited".into());
                    return;
                }
            }
        }
        let mut lines: Vec<String> = vec![];
        let sessions: Vec<(SessionId, String)> = self.state.sessions.iter().map(|s| (s.id, s.name.clone())).collect();
        if sessions != self.told.sessions {
            let ids = |v: &[(SessionId, String)]| v.iter().map(|s| s.0).collect::<Vec<_>>();
            let renamed = sessions
                .iter()
                .find(|(id, name)| *id == self.session && self.told.sessions.iter().any(|(i, n)| i == id && n != name));
            if let Some((id, name)) = renamed {
                lines.push(format!("%session-renamed ${id} {name}"));
            }
            if ids(&sessions) != ids(&self.told.sessions) || renamed.is_none() {
                lines.push("%sessions-changed".into());
            }
            self.told.sessions = sessions;
        }

        let s = self.session;
        let tabs = self.session_tabs(s);
        let current = self.current_tab(s);
        // The current window, mended if it went away.
        if let Some(cur) = current {
            self.active_tab.insert(s, cur);
        }
        if current != self.told.active
            && let Some(cur) = current
        {
            lines.push(format!("%session-window-changed ${s} @{cur}"));
        }
        self.told.active = current;
        for t in &tabs {
            if !self.told.windows.contains_key(t) {
                lines.push(format!("%window-add @{t}"));
            }
        }
        let closed: Vec<TabId> = self.told.windows.keys().filter(|t| !tabs.contains(t)).copied().collect();
        for t in &closed {
            lines.push(format!("%window-close @{t}"));
            self.told.windows.remove(t);
            self.zoom.remove(t);
            self.sizes.remove(t);
        }
        for t in tabs.clone() {
            let name = self.window_name(t);
            let (full, visible) = self.layouts(t);
            let layout = format!("{full} {visible} {}", self.window_flags(t));
            let panes = self.tab(t).map(|v| v.root.panes()).unwrap_or_default();
            let active = self.current_pane(t);
            let now = ToldWindow { name, layout, panes, active };
            let Some(was) = self.told.windows.get(&t).cloned() else {
                // A new window: the client lists it itself.
                self.told.windows.insert(t, now);
                continue;
            };
            if was.name != now.name {
                lines.push(format!("%window-renamed @{t} {}", now.name));
            }
            let pane_changed = now.active != was.active && now.active.is_some();
            let new_pane = now.active.is_some_and(|p| !was.panes.contains(&p));
            if pane_changed && new_pane {
                lines.push(format!("%window-pane-changed @{t} %{}", now.active.unwrap()));
            }
            if was.layout != now.layout || self.force_layout.remove(&t) {
                lines.push(format!("%layout-change @{t} {}", now.layout));
            }
            if pane_changed && !new_pane {
                lines.push(format!("%window-pane-changed @{t} %{}", now.active.unwrap()));
            }
            self.told.windows.insert(t, now);
        }
        self.told.order = tabs;
        self.force_layout.clear();
        // Subscriptions whose value changed.
        let subs: Vec<(String, String, Option<String>)> =
            self.subscriptions.iter().map(|(n, (f, v))| (n.clone(), f.clone(), v.clone())).collect();
        for (name, fmt, was) in subs {
            let value = self.expand_in(&fmt, Some(s), None, None);
            if was.as_ref() != Some(&value) {
                lines.push(format!("%subscription-changed {name} ${s} - - - : {value}"));
                self.subscriptions.insert(name, (fmt, Some(value)));
            }
        }
        for l in lines {
            let mut b = l.into_bytes();
            b.push(b'\n');
            self.note(&b);
        }
    }

    /// Attach to another session: the client starts over with it.
    pub fn switch_session(&mut self, session: SessionId) -> anyhow::Result<()> {
        let old: Vec<PaneId> = self.panes.keys().copied().collect();
        self.panes.clear();
        self.inbox.clear();
        self.blocks_dirty.clear();
        let still: Vec<PaneId> = old.into_iter().filter(|p| self.state.panes.iter().any(|i| i.id == *p)).collect();
        if !still.is_empty() {
            self.conn.send(&ClientMsg::Detach { panes: still })?;
        }
        self.session = session;
        self.sync_panes()?;
        self.reset_told();
        let line = format!("%session-changed ${session} {}\n", self.session_name(session));
        self.note(line.as_bytes());
        Ok(())
    }

    /// The client lists everything itself after `%session-changed`: take
    /// what there is now as told.
    pub fn reset_told(&mut self) {
        self.told =
            Told { sessions: self.state.sessions.iter().map(|s| (s.id, s.name.clone())).collect(), ..Told::default() };
        let s = self.session;
        self.told.active = self.current_tab(s);
        for t in self.session_tabs(s) {
            let (full, visible) = self.layouts(t);
            let w = ToldWindow {
                name: self.window_name(t),
                layout: format!("{full} {visible} {}", self.window_flags(t)),
                panes: self.tab(t).map(|v| v.root.panes()).unwrap_or_default(),
                active: self.current_pane(t),
            };
            self.told.windows.insert(t, w);
        }
        self.told.order = self.session_tabs(s);
    }
}

/// tmux's `%output` escaping: bytes below space and `\` as `\ooo`; the rest
/// raw, even invalid UTF-8.
pub fn escape(data: &[u8], out: &mut Vec<u8>) {
    for &b in data {
        if b < b' ' || b == b'\\' || b == 0x7f {
            out.extend_from_slice(format!("\\{b:03o}").as_bytes());
        } else {
            out.push(b);
        }
    }
}

/// What a pane running only its shell is called.
pub fn shell_name() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|s| s.rsplit('/').next().map(str::to_owned))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "shell".into())
}

/// A fresh attach, with no more history than a mirror keeps.
fn mirror_attach(pane: PaneId) -> AttachPane {
    AttachPane { pane, offset: None, history: Some(HISTORY as u32) }
}
