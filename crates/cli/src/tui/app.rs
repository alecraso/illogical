//! The TUI's state and everything that changes it: what the daemon sends,
//! keys, the mouse, and the commands behind Ctrl-] and the menus.

use std::{
    collections::{HashMap, HashSet},
    sync::mpsc,
    time::{Duration, Instant},
};

use arugula_proto::{
    Action, AttachPane, Attention, BlockType, ClientId, ClientMsg, Dir, Edge, Intent, NodeId, PaneId, PaneInfo, PaneOp,
    Policy, ServerMsg, SessionId, State, TabId, TabView,
};
use arugula_vt::{VtEngine, key, mouse};
use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
};
use serde_json::{Value, json};

use super::{
    agent,
    conn::{Conn, In},
    copy::{Click, Copy, Fetched},
    keys,
    pane::{SCROLLBACK, TermPane, Took},
};

/// `GET /api/conversations`, as it came back (M33).
type Listed = anyhow::Result<Vec<u8>>;

/// What keys go to right now.
pub enum Mode {
    Normal,
    /// After Ctrl-]: the next key is a command.
    Prefix,
    /// Moving through the sidebar's tabs and what needs you.
    Sidebar {
        sel: usize,
    },
    Menu(Menu),
    Prompt(Prompt),
    /// Moving through a pane's history with the keyboard (M32).
    Copy,
}

pub struct Menu {
    pub x: u16,
    pub y: u16,
    pub items: Vec<Item>,
    pub sel: usize,
}

pub struct Item {
    pub label: String,
    pub checked: bool,
    pub act: Act,
}

/// Something a menu item, a key or a click does.
#[derive(Clone)]
pub enum Act {
    Intent(Intent),
    Pane(PaneId, PaneOp),
    /// `POST` this to the API; the words say what failed, if it does.
    Api(String, Value, &'static str),
    Ask(Ask),
    Zoom(PaneId),
    /// Pick where to move the pane: the next click on a pane's edge.
    Move(PaneId),
    /// Show this tab, focused on this pane.
    Go(TabId, Option<PaneId>),
    Sidebar,
    /// Copy mode in the focused pane.
    Copy,
    /// The Claude Code conversations menu (M33).
    Conversations,
    /// Show a conversation as an agent block beside the focused pane.
    OpenConversation(String),
    ToggleSidebar,
    Quit,
    /// A heading, not an item.
    None,
}

/// What a prompt's text is for.
#[derive(Clone)]
pub enum Ask {
    RenameTab(TabId),
    RenameSession(SessionId),
    Hook(PaneId),
    AgentSend(PaneId),
    /// Copy mode's `/` and `?`.
    Search {
        back: bool,
    },
}

pub struct Prompt {
    pub title: String,
    pub text: String,
    pub ask: Ask,
}

/// A row of the sidebar that does something.
#[derive(Clone, Copy)]
pub enum Hit {
    Session(SessionId),
    Tab(TabId),
    /// A pane that needs you, and its tab.
    Wants(PaneId, TabId),
}

pub struct DividerDrag {
    split: NodeId,
    index: usize,
    dir: Dir,
    start: u16,
    extents: Vec<u16>,
}

/// Moving a pane: where it would go if let go now.
pub struct Moving {
    pub pane: PaneId,
    pub target: Option<(PaneId, Edge)>,
    /// Picked from a menu: the next click decides.
    pub by_click: bool,
}

#[derive(Default)]
pub struct Stats {
    pub build: Vec<Duration>,
    pub draw: Vec<Duration>,
    pub bytes: u64,
}

pub struct App {
    pub conn: Conn,
    pub me: ClientId,
    pub state: Option<State>,
    pub tab: Option<TabId>,
    pub panes: HashMap<PaneId, TermPane>,
    /// Agent and browser blocks' state, whole, as the daemon sends it.
    pub blocks: HashMap<PaneId, Value>,
    /// How far up each agent block's transcript is scrolled.
    pub scroll: HashMap<PaneId, usize>,
    pub focus: Option<PaneId>,
    /// Where the tab is drawn.
    pub area: Rect,
    /// The whole screen, as last drawn.
    pub full: Rect,
    /// Where it was last told to the daemon.
    viewed: Option<(TabId, u16, u16, Option<PaneId>)>,
    pub zoom: Option<PaneId>,
    pub sidebar: bool,
    pub mode: Mode,
    pub hits: Vec<(u16, Hit)>,
    pub moving: Option<Moving>,
    drag: Option<DividerDrag>,
    /// Mouse buttons down, for programs that report motion with a button.
    buttons: u8,
    /// Focus whatever pane or tab appears next (our own split or new tab).
    follow_new: bool,
    seen_panes: HashSet<PaneId>,
    seen_tabs: HashSet<TabId>,
    pub toast: Option<(String, Instant)>,
    pub errors: mpsc::Receiver<String>,
    pub dirty: bool,
    pub quit: bool,
    pub stats: Stats,
    /// Copy mode's cursor and search (M32).
    pub copy: Option<Copy>,
    /// The last click, for double and triple clicks.
    pub click: Option<Click>,
    /// A mouse selection being made, in this pane, there.
    pub selecting: Option<(PaneId, Rect)>,
    /// Text for the outer terminal's clipboard, written after the next draw.
    pub clip: Option<String>,
    /// Pane logs read for copy mode, as they come back.
    pub fetched: (mpsc::Sender<Fetched>, mpsc::Receiver<Fetched>),
    /// `--session`: where to start.
    start: Option<String>,
    /// The conversations list, as it comes back (M33).
    pub conversations: (mpsc::Sender<Listed>, mpsc::Receiver<Listed>),
}

impl App {
    pub fn new(conn: Conn, hello: ServerMsg, errors: mpsc::Receiver<String>, start: Option<String>) -> Self {
        let ServerMsg::Hello { client, state, .. } = hello else { unreachable!("Conn::open returns the hello") };
        let mut app = Self {
            conn,
            me: client,
            state: None,
            tab: None,
            panes: HashMap::new(),
            blocks: HashMap::new(),
            scroll: HashMap::new(),
            focus: None,
            area: Rect::default(),
            full: Rect::default(),
            viewed: None,
            zoom: None,
            sidebar: true,
            mode: Mode::Normal,
            hits: Vec::new(),
            moving: None,
            drag: None,
            buttons: 0,
            follow_new: false,
            seen_panes: HashSet::new(),
            seen_tabs: HashSet::new(),
            toast: None,
            errors,
            dirty: true,
            quit: false,
            stats: Stats::default(),
            copy: None,
            click: None,
            selecting: None,
            clip: None,
            fetched: mpsc::channel(),
            start,
            conversations: mpsc::channel(),
        };
        app.seen_tabs = state.tabs.iter().map(|t| t.id).collect();
        app.seen_panes = state.panes.iter().map(|p| p.id).collect();
        app.state = Some(state);
        app.choose_start();
        app
    }

    /// `--session NAME|ID`: its first tab, making it if there's no such
    /// session. Otherwise the first tab there is.
    fn choose_start(&mut self) {
        let Some(state) = &self.state else { return };
        let want = self.start.take();
        let found = want.as_deref().and_then(|w| {
            state.sessions.iter().find(|s| s.name == w || s.id.to_string() == w).and_then(|s| s.tabs.first().copied())
        });
        match (want, found) {
            (_, Some(t)) => self.select(t, None),
            (Some(name), None) => {
                self.follow_new = true;
                self.intent(Intent::NewSession { name: Some(name), from_pane: None });
                self.sync();
            }
            (None, None) => self.sync(),
        }
    }

    // ---- talking to the daemon ----

    pub fn intent(&mut self, intent: Intent) {
        self.conn.send(&ClientMsg::Intent { id: None, intent });
    }

    pub(super) fn attach(&mut self, panes: Vec<AttachPane>) {
        if !panes.is_empty() {
            self.conn.send(&ClientMsg::Attach { panes, zstd: true, acks: true, kitty_keys: true });
        }
    }

    pub fn act(&mut self, act: Act) {
        match act {
            Act::Intent(i) => {
                if matches!(i, Intent::Split { .. } | Intent::NewTab { .. } | Intent::NewSession { .. }) {
                    self.follow_new = true;
                }
                self.intent(i);
            }
            Act::Pane(pane, op) => self.conn.send(&ClientMsg::Pane { pane, op }),
            Act::Api(path, body, what) => self.conn.api(path, body, what),
            Act::Ask(ask) => self.prompt(ask),
            Act::Zoom(p) => {
                self.zoom = if self.zoom == Some(p) { None } else { Some(p) };
                self.view(true);
            }
            Act::Move(pane) => self.moving = Some(Moving { pane, target: None, by_click: true }),
            Act::Go(t, p) => self.select(t, p),
            Act::Sidebar => {
                self.sidebar = true;
                self.mode = Mode::Sidebar { sel: 0 };
            }
            Act::Copy => self.enter_copy(),
            Act::Conversations => self.list_conversations(),
            Act::OpenConversation(id) => {
                self.follow_new = true;
                let body = json!({ "id": id, "split": self.focus, "from_pane": self.focus });
                self.conn.api("/api/conversations/open".into(), body, "open that conversation");
            }
            Act::ToggleSidebar => self.sidebar = !self.sidebar,
            Act::Quit => self.quit = true,
            Act::None => {}
        }
        self.dirty = true;
    }

    fn prompt(&mut self, ask: Ask) {
        let (title, text) = match &ask {
            Ask::RenameTab(t) => ("Rename tab", self.tab_view_of(*t).and_then(|t| t.name.clone()).unwrap_or_default()),
            Ask::RenameSession(s) => (
                "Rename session",
                self.state
                    .as_ref()
                    .and_then(|st| st.sessions.iter().find(|x| x.id == *s))
                    .map(|s| s.name.clone())
                    .unwrap_or_default(),
            ),
            Ask::Hook(p) => (
                "Run when restored",
                match self.info(*p).map(|i| &i.policy) {
                    Some(Policy::Hook { command }) => command.clone(),
                    _ => "claude --continue".into(),
                },
            ),
            Ask::AgentSend(_) => ("Message the agent", String::new()),
            Ask::Search { back } => (if *back { "Search up" } else { "Search down" }, String::new()),
        };
        self.mode = Mode::Prompt(Prompt { title: title.into(), text, ask });
    }

    fn submit(&mut self, p: Prompt) {
        let text = p.text.trim().to_owned();
        match p.ask {
            Ask::RenameTab(tab) => self.intent(Intent::RenameTab { tab, name: (!text.is_empty()).then_some(text) }),
            Ask::RenameSession(session) if !text.is_empty() => {
                self.intent(Intent::RenameSession { session, name: text })
            }
            Ask::Hook(pane) if !text.is_empty() => {
                self.act(Act::Pane(pane, PaneOp::SetPolicy { policy: Policy::Hook { command: text } }))
            }
            Ask::AgentSend(id) if !text.is_empty() => {
                self.conn.api(format!("/api/blocks/{id}/call/send"), json!({ "text": text }), "send that")
            }
            Ask::Search { back } => {
                self.mode = Mode::Copy;
                if !p.text.is_empty() {
                    self.search(&p.text, back);
                }
            }
            _ => {}
        }
    }

    /// Tell the daemon what we show, at what size. `claim`: make it the
    /// tab's size (opened, switched to, resized).
    pub fn view(&mut self, claim: bool) {
        self.send_view(claim, false);
    }

    /// `typed`: the claim is for typing, which the daemon holds off while
    /// the size's owner is still typing (#333).
    fn send_view(&mut self, claim: bool, typed: bool) {
        let Some(tab) = self.tab else { return };
        let (cols, rows) = (self.area.width, self.area.height);
        if cols == 0 || rows == 0 {
            return;
        }
        let zoom = self.zoom.filter(|z| {
            self.tab_view().is_some_and(|t| t.layout.panes.iter().any(|(p, _)| p == z) || t.zoom == Some(*z))
        });
        let now = Some((tab, cols, rows, zoom));
        if !claim && self.viewed == now {
            return;
        }
        self.viewed = now;
        self.conn.send(&ClientMsg::View { tab, cols, rows, zoom, claim, typed });
    }

    /// Typed in: take the tab's size back if another client has it (once
    /// they've stopped typing there for a moment).
    fn claim(&mut self) {
        if self.tab_view().is_some_and(|t| t.owner != Some(self.me)) {
            self.send_view(true, true);
        }
    }

    pub fn set_focus(&mut self, p: PaneId) {
        if self.in_copy() && self.copy.as_ref().is_some_and(|c| c.pane != p) {
            self.leave_copy();
        }
        if self.focus != Some(p) {
            self.focus = Some(p);
            self.conn.send(&ClientMsg::Focus { pane: Some(p) });
            self.dirty = true;
        }
    }

    // ---- what the daemon says ----

    pub fn take(&mut self, m: In) {
        self.dirty = true;
        match m {
            In::Closed => self.quit = true,
            In::Frame(f) => {
                self.stats.bytes += f.data.len() as u64;
                let pane = f.pane;
                let Some(p) = self.panes.get_mut(&pane) else { return };
                match p.take(f) {
                    Took::Nothing => {}
                    Took::Fresh => self.snapshot_taken(pane),
                    Took::Ack(offset) => self.conn.send(&ClientMsg::Ack { pane, offset }),
                    Took::Gap => self.attach(vec![AttachPane { pane, offset: None, history: Some(SCROLLBACK) }]),
                }
            }
            In::Msg(m) => match *m {
                ServerMsg::Hello { state, .. } | ServerMsg::State { state } => {
                    self.state = Some(state);
                    self.sync();
                }
                ServerMsg::Delta { delta } => {
                    if let Some(s) = &mut self.state {
                        s.apply(&delta);
                    }
                    self.sync();
                }
                ServerMsg::Size { pane, cols, rows } => {
                    if let Some(p) = self.panes.get_mut(&pane) {
                        p.resize(cols, rows);
                    }
                }
                ServerMsg::Resync { pane } => {
                    // From where we are: the daemon replays a small gap, or
                    // sends the screen alone (#49).
                    if let Some(p) = self.panes.get_mut(&pane) {
                        let offset = p.offset;
                        p.resync = offset.is_some();
                        let history = if offset.is_some() { Some(0) } else { Some(SCROLLBACK) };
                        self.attach(vec![AttachPane { pane, offset, history }]);
                    }
                }
                ServerMsg::Block { block, state } => {
                    self.blocks.insert(block, state);
                }
                ServerMsg::Error { message, .. } | ServerMsg::Notice { message } => self.say(message),
                ServerMsg::ControlRequest { name, pane, .. } => self.say(format!("{name} asks to drive %{pane}")),
                ServerMsg::TrustRequest { name, pane, .. } => {
                    self.say(format!("{name} asks to be trusted with %{pane}"))
                }
                ServerMsg::Pong { .. }
                | ServerMsg::Follow { .. }
                | ServerMsg::Thread { .. }
                | ServerMsg::CallSignal { .. }
                | ServerMsg::HandCall { .. } => {}
            },
        }
    }

    pub fn say(&mut self, s: impl Into<String>) {
        self.toast = Some((s.into(), Instant::now()));
        self.dirty = true;
    }

    /// Bring the shown tab and its terminals in line with the state.
    fn sync(&mut self) {
        let Some(state) = &self.state else { return };
        let tabs: HashSet<TabId> = state.tabs.iter().map(|t| t.id).collect();
        let new_tab = tabs.difference(&self.seen_tabs).next().copied();
        self.seen_tabs = tabs;
        let panes_now: HashSet<PaneId> = state.panes.iter().map(|p| p.id).collect();
        let fresh: Vec<PaneId> = panes_now.difference(&self.seen_panes).copied().collect();
        self.seen_panes = panes_now;
        self.blocks.retain(|id, _| self.seen_panes.contains(id));

        if self.follow_new
            && let Some(t) = new_tab
        {
            self.follow_new = false;
            return self.select(t, None);
        }
        if self.tab_view().is_none() {
            let first = state.sessions.iter().flat_map(|s| s.tabs.first()).next().copied();
            match first {
                Some(t) => {
                    self.tab = None;
                    return self.select(t, None);
                }
                None => {
                    self.quit = true;
                    return;
                }
            }
        }
        let tab = self.tab_view().expect("checked above").clone();
        let in_tab: Vec<PaneId> = tab.root.panes();
        let terminals: Vec<PaneId> =
            in_tab.iter().copied().filter(|p| self.info(*p).is_some_and(|i| i.kind == BlockType::Terminal)).collect();
        let gone: Vec<PaneId> = self.panes.keys().filter(|p| !terminals.contains(p)).copied().collect();
        if !gone.is_empty() {
            for p in &gone {
                self.panes.remove(p);
            }
            self.conn.send(&ClientMsg::Detach { panes: gone });
        }
        let mut attach = Vec::new();
        for p in terminals {
            if self.panes.contains_key(&p) {
                continue;
            }
            let (cols, rows) =
                tab.layout.panes.iter().find(|(q, _)| *q == p).map_or((80, 24), |(_, r)| (r.cols, r.rows));
            self.panes.insert(p, TermPane::new(cols, rows));
            attach.push(AttachPane { pane: p, offset: None, history: Some(SCROLLBACK) });
        }
        self.attach(attach);
        if self.in_copy() && !self.copy.as_ref().is_some_and(|c| self.panes.contains_key(&c.pane)) {
            self.mode = Mode::Normal;
        }
        if self.zoom.is_some_and(|z| !in_tab.contains(&z)) {
            self.zoom = None;
        }
        if self.follow_new
            && let Some(p) = fresh.iter().find(|p| in_tab.contains(p))
        {
            self.follow_new = false;
            self.set_focus(*p);
        }
        if !self.focus.is_some_and(|f| in_tab.contains(&f))
            && let Some(&p) = in_tab.first()
        {
            self.focus = None;
            self.set_focus(p);
        }
    }

    pub fn select(&mut self, tab: TabId, pane: Option<PaneId>) {
        if self.tab != Some(tab) {
            let old: Vec<PaneId> = self.panes.drain().map(|(p, _)| p).collect();
            if !old.is_empty() {
                self.conn.send(&ClientMsg::Detach { panes: old });
            }
            self.tab = Some(tab);
            self.zoom = None;
            self.view(true);
        }
        self.sync();
        if let Some(p) = pane {
            self.set_focus(p);
        }
        self.dirty = true;
    }

    // ---- reading the state ----

    pub fn tab_view(&self) -> Option<&TabView> {
        self.tab.and_then(|t| self.tab_view_of(t))
    }

    fn tab_view_of(&self, t: TabId) -> Option<&TabView> {
        self.state.as_ref()?.tabs.iter().find(|v| v.id == t)
    }

    pub fn info(&self, p: PaneId) -> Option<&PaneInfo> {
        self.state.as_ref()?.panes.iter().find(|i| i.id == p)
    }

    pub fn tab_of(&self, p: PaneId) -> Option<TabId> {
        self.state.as_ref()?.tabs.iter().find(|t| t.root.contains(p)).map(|t| t.id)
    }

    fn session_of_tab(&self, t: TabId) -> Option<SessionId> {
        self.state.as_ref()?.sessions.iter().find(|s| s.tabs.contains(&t)).map(|s| s.id)
    }

    fn tab_order(&self) -> Vec<TabId> {
        self.state
            .as_ref()
            .map(|s| s.sessions.iter().flat_map(|s| s.tabs.iter().copied()).collect())
            .unwrap_or_default()
    }

    /// Panes that need you, in the order the sidebar lists them.
    pub fn wanting(&self) -> Vec<&PaneInfo> {
        let Some(s) = &self.state else { return vec![] };
        let mut v: Vec<&PaneInfo> =
            s.panes.iter().filter(|p| matches!(p.attention, Attention::NeedsInput | Attention::Done)).collect();
        v.sort_by_key(|p| (p.attention != Attention::NeedsInput, p.reason.as_ref().map(|r| r.since_ms)));
        v
    }

    /// Where each pane is on screen.
    pub fn rects(&self) -> Vec<(PaneId, Rect)> {
        let Some(tab) = self.tab_view() else { return vec![] };
        tab.layout
            .panes
            .iter()
            .map(|(p, r)| (*p, Rect::new(self.area.x + r.x, self.area.y + r.y, r.cols, r.rows).intersection(self.area)))
            .collect()
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<(PaneId, Rect)> {
        self.rects().into_iter().find(|(_, r)| r.contains((x, y).into()))
    }

    // ---- input ----

    pub fn event(&mut self, ev: Event) {
        self.dirty = true;
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.key(k),
            Event::Key(_) => {}
            Event::Mouse(m) => self.mouse(m),
            Event::Paste(text) => match &mut self.mode {
                Mode::Prompt(p) => p.text.push_str(&text.replace(['\r', '\n'], " ")),
                _ => {
                    if let Some((pane, t)) = self.focused_terminal() {
                        t.engine.scroll_to_bottom();
                        let data = t.engine.encode_paste(&text);
                        self.conn.input(pane, data);
                    }
                }
            },
            Event::FocusGained | Event::FocusLost => {
                let gained = matches!(ev, Event::FocusGained);
                // Attention skips panes someone is looking at.
                let pane = if gained { self.focus } else { None };
                self.conn.send(&ClientMsg::Focus { pane });
                // Focus alone doesn't take the size from another window
                // (#333): typing here does.
                if gained {
                    self.view(false);
                }
                if let Some((pane, t)) = self.focused_terminal() {
                    let data = t.engine.encode_focus(gained);
                    self.conn.input(pane, data);
                }
            }
            Event::Resize(..) => {}
        }
    }

    fn focused_terminal(&mut self) -> Option<(PaneId, &mut TermPane)> {
        let f = self.focus?;
        self.panes.get_mut(&f).map(|t| (f, t))
    }

    fn key(&mut self, k: KeyEvent) {
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Prompt(mut p) => match k.code {
                KeyCode::Enter => self.submit(p),
                KeyCode::Esc => {
                    if matches!(p.ask, Ask::Search { .. }) {
                        self.mode = Mode::Copy;
                    }
                }
                KeyCode::Backspace => {
                    p.text.pop();
                    self.mode = Mode::Prompt(p);
                }
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    p.text.clear();
                    self.mode = Mode::Prompt(p);
                }
                KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                    p.text.push(c);
                    self.mode = Mode::Prompt(p);
                }
                _ => self.mode = Mode::Prompt(p),
            },
            Mode::Menu(mut m) => match k.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    m.sel = prev_item(&m.items, m.sel);
                    self.mode = Mode::Menu(m);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    m.sel = next_item(&m.items, m.sel);
                    self.mode = Mode::Menu(m);
                }
                KeyCode::Enter => {
                    let act = m.items.get(m.sel).map(|i| i.act.clone());
                    if let Some(act) = act {
                        self.act(act);
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') => {}
                _ if keys::is_menu_key(&k) => {}
                _ => self.mode = Mode::Menu(m),
            },
            Mode::Sidebar { sel } => self.sidebar_key(k, sel),
            Mode::Prefix => self.command(k),
            Mode::Copy => self.copy_key(k),
            Mode::Normal => {
                if keys::is_menu_key(&k) {
                    self.mode = Mode::Prefix;
                    return;
                }
                if self.moving.take().is_some() && k.code == KeyCode::Esc {
                    return;
                }
                self.key_for_pane(k);
            }
        }
    }

    /// A key for the focused pane.
    fn key_for_pane(&mut self, k: KeyEvent) {
        let Some(pane) = self.focus else { return };
        if let Some(t) = self.panes.get_mut(&pane) {
            // Shift+PgUp/PgDn scroll the pane's history, as terminals do.
            if k.modifiers == KeyModifiers::SHIFT && matches!(k.code, KeyCode::PageUp | KeyCode::PageDown) {
                let page = t.engine.size().1 as isize - 1;
                t.engine.scroll(if k.code == KeyCode::PageUp { -page } else { page });
                return;
            }
            t.engine.scroll_to_bottom();
            t.engine.select_none();
            let Some(ev) = keys::event(&k) else { return };
            let data = t.engine.encode_key(&ev);
            self.conn.input(pane, data);
            self.claim();
            return;
        }
        if self.info(pane).is_some_and(|i| i.kind == BlockType::Agent) {
            self.agent_key(pane, k);
        }
    }

    fn agent_key(&mut self, id: PaneId, k: KeyEvent) {
        let state = self.blocks.get(&id).cloned().unwrap_or_default();
        let first = agent::pending(&state).into_iter().next();
        let call = |method: &str| format!("/api/blocks/{id}/call/{method}");
        let scroll = self.scroll.entry(id).or_default();
        match k.code {
            KeyCode::Char('a') if first.is_some() => {
                let p = first.unwrap();
                self.conn.api(call("approve"), json!({ "id": p.id }), "approve that");
            }
            KeyCode::Char('A') if first.is_some() => {
                let p = first.unwrap();
                self.conn.api(call("approve"), json!({ "id": p.id, "option": "always" }), "approve that");
            }
            KeyCode::Char('d') if first.is_some() => {
                let p = first.unwrap();
                self.conn.api(call("deny"), json!({ "id": p.id }), "deny that");
            }
            KeyCode::Char('i') | KeyCode::Enter => self.prompt(Ask::AgentSend(id)),
            // M33: an opened conversation.
            KeyCode::Char('C') if state["import"].is_object() => {
                self.conn.api(call("continue"), json!({}), "continue it")
            }
            KeyCode::Char('F') if state["import"].is_object() => self.conn.api(call("fork"), json!({}), "fork it"),
            KeyCode::PageUp => *scroll += 10,
            KeyCode::Up => *scroll += 1,
            KeyCode::PageDown => *scroll = scroll.saturating_sub(10),
            KeyCode::Down => *scroll = scroll.saturating_sub(1),
            KeyCode::End => *scroll = 0,
            _ => {}
        }
    }

    /// A key after Ctrl-].
    fn command(&mut self, k: KeyEvent) {
        if keys::is_menu_key(&k) {
            // Twice: the key itself, for the program.
            if let Some(p) = self.focus {
                self.conn.input(p, vec![0x1d]);
            }
            return;
        }
        let focus = self.focus;
        match k.code {
            KeyCode::Char('q') | KeyCode::Char('d') => self.act(Act::Quit),
            KeyCode::Char('v') | KeyCode::Char('|') | KeyCode::Char('%') => {
                if let Some(pane) = focus {
                    self.act(Act::Intent(Intent::Split { pane, edge: Edge::Right, local: false, cwd: None }));
                }
            }
            KeyCode::Char('s') | KeyCode::Char('-') | KeyCode::Char('"') => {
                if let Some(pane) = focus {
                    self.act(Act::Intent(Intent::Split { pane, edge: Edge::Bottom, local: false, cwd: None }));
                }
            }
            KeyCode::Char('c') => {
                if let Some(session) = self.tab.and_then(|t| self.session_of_tab(t)) {
                    self.act(Act::Intent(Intent::NewTab { session, from_pane: focus, cwd: None }));
                }
            }
            KeyCode::Char('x') => {
                if let Some(pane) = focus {
                    self.act(Act::Intent(Intent::ClosePane { pane }));
                }
            }
            KeyCode::Char('o') => self.cycle_focus(),
            KeyCode::Char('[') => self.enter_copy(),
            KeyCode::Char('C') => self.list_conversations(),
            KeyCode::Left => self.focus_toward(Edge::Left),
            KeyCode::Right => self.focus_toward(Edge::Right),
            KeyCode::Up => self.focus_toward(Edge::Top),
            KeyCode::Down => self.focus_toward(Edge::Bottom),
            KeyCode::Char('n') | KeyCode::Char('p') => self.step_tab(k.code == KeyCode::Char('n')),
            KeyCode::Char(c @ '1'..='9') => {
                if let Some(&t) = self.tab_order().get((c as u8 - b'1') as usize) {
                    self.select(t, None);
                }
            }
            KeyCode::Char('z') => {
                if let Some(p) = focus {
                    self.act(Act::Zoom(p));
                }
            }
            KeyCode::Char('w') | KeyCode::Tab => self.act(Act::Sidebar),
            KeyCode::Char('b') => self.act(Act::ToggleSidebar),
            KeyCode::Char('m') => {
                if let Some(p) = focus
                    && let Some((_, r)) = self.rects().into_iter().find(|(q, _)| *q == p)
                {
                    self.open_menu(r.x + 1, r.y + 1, self.pane_menu(p));
                }
            }
            KeyCode::Char('t') => {
                if let Some(t) = self.tab {
                    self.open_menu(self.area.x + 1, self.area.y, self.tab_menu(t));
                }
            }
            KeyCode::Char('r') => {
                if let Some(t) = self.tab {
                    self.act(Act::Ask(Ask::RenameTab(t)));
                }
            }
            KeyCode::Char('?') | KeyCode::Char(' ') => {
                self.open_menu(self.area.x + 1, self.area.y, self.command_menu())
            }
            _ => {}
        }
    }

    fn cycle_focus(&mut self) {
        let ids: Vec<PaneId> = self.rects().into_iter().map(|(p, _)| p).collect();
        let i = ids.iter().position(|p| Some(*p) == self.focus).map_or(0, |i| (i + 1) % ids.len().max(1));
        if let Some(&p) = ids.get(i) {
            self.set_focus(p);
        }
    }

    /// The nearest pane on that side of the focused one.
    fn focus_toward(&mut self, edge: Edge) {
        let rects = self.rects();
        let Some(&(_, from)) = rects.iter().find(|(p, _)| Some(*p) == self.focus) else { return };
        let (cx, cy) = (from.x as i32 + from.width as i32 / 2, from.y as i32 + from.height as i32 / 2);
        let best = rects
            .iter()
            .filter(|(_, r)| match edge {
                Edge::Left => r.right() <= from.x,
                Edge::Right => r.x >= from.right(),
                Edge::Top => r.bottom() <= from.y,
                Edge::Bottom => r.y >= from.bottom(),
                Edge::Center => false,
            })
            .min_by_key(|(_, r)| {
                let (x, y) = (r.x as i32 + r.width as i32 / 2, r.y as i32 + r.height as i32 / 2);
                (x - cx).abs() + (y - cy).abs()
            });
        if let Some(&(p, _)) = best {
            self.set_focus(p);
        }
    }

    fn step_tab(&mut self, forward: bool) {
        let order = self.tab_order();
        if let Some(i) = order.iter().position(|t| Some(*t) == self.tab) {
            let j = if forward { (i + 1) % order.len() } else { (i + order.len() - 1) % order.len() };
            self.select(order[j], None);
        }
    }

    /// Entries of the sidebar a key can land on.
    fn nav_items(&self) -> Vec<Hit> {
        self.hits.iter().map(|(_, h)| *h).filter(|h| !matches!(h, Hit::Session(_))).collect()
    }

    fn sidebar_key(&mut self, k: KeyEvent, sel: usize) {
        let items = self.nav_items();
        let n = items.len();
        let here = items.get(sel.min(n.saturating_sub(1))).copied();
        let mut sel = sel;
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => sel = sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => sel = (sel + 1).min(n.saturating_sub(1)),
            KeyCode::Enter => {
                match here {
                    Some(Hit::Tab(t)) => self.select(t, None),
                    Some(Hit::Wants(p, t)) => self.select(t, Some(p)),
                    _ => {}
                }
                return;
            }
            KeyCode::Char(c @ ('a' | 'A' | 'd' | 'x')) => {
                if let Some(Hit::Wants(p, _)) = here {
                    let (action, option) = match c {
                        'a' => (Action::Allow, None),
                        'A' => (Action::Allow, Some("always")),
                        'd' => (Action::Deny, None),
                        _ => (Action::Dismiss, None),
                    };
                    self.act(answer(p, action, option));
                }
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('w') => return,
            _ if keys::is_menu_key(&k) => return,
            _ => {}
        }
        self.mode = Mode::Sidebar { sel };
    }

    // ---- Claude Code conversations (M33) ----

    fn list_conversations(&mut self) {
        let tx = self.conversations.0.clone();
        self.conn.get("/api/conversations?limit=40".into(), move |r| {
            let _ = tx.send(r);
        });
        self.say("Reading Claude Code conversations…");
    }

    /// The list came back: a menu of them. One a block has (or a pane runs)
    /// goes there; another opens beside the focused pane.
    pub fn take_conversations(&mut self) {
        while let Ok(r) = self.conversations.1.try_recv() {
            let v: Value = match r.and_then(|b| Ok(serde_json::from_slice(&b)?)) {
                Ok(v) => v,
                Err(e) => {
                    self.say(format!("Couldn't list the conversations: {e:#}"));
                    continue;
                }
            };
            self.toast = None;
            let home = std::env::var("HOME").unwrap_or_default();
            let mut m = vec![heading("Claude Code conversations")];
            for c in v["conversations"].as_array().into_iter().flatten() {
                let s = |k: &str| c[k].as_str().unwrap_or("");
                let title: String = s("title").chars().take(48).collect();
                let cwd = s("cwd");
                let cwd = match cwd.strip_prefix(home.as_str()) {
                    Some(rest) if !home.is_empty() => format!("~{rest}"),
                    _ => cwd.to_owned(),
                };
                let tag = match (c["block"].as_u64(), c["live"]["pane"].as_u64(), c["live"].is_object()) {
                    (Some(b), _, _) => format!(" [%{b}]"),
                    (_, Some(p), _) => format!(" [● %{p}]"),
                    (_, _, true) => " [● open]".into(),
                    _ => String::new(),
                };
                let src = if s("source") == "desktop" { "desk" } else { "term" };
                let label = format!("{src}  {title}{tag}  {cwd}");
                let go = |p: u64| {
                    let p = p as PaneId;
                    self.tab_of(p).map(|t| Act::Go(t, Some(p)))
                };
                let act = c["block"]
                    .as_u64()
                    .and_then(go)
                    .or_else(|| c["live"]["pane"].as_u64().and_then(go))
                    .unwrap_or_else(|| Act::OpenConversation(s("id").to_owned()));
                m.push(item(&label, act));
            }
            if m.len() == 1 {
                m.push(heading("None here"));
            }
            self.open_menu(self.area.x + 1, self.area.y, m);
        }
    }

    // ---- menus ----

    pub fn open_menu(&mut self, x: u16, y: u16, items: Vec<Item>) {
        let sel = next_item(&items, usize::MAX);
        self.mode = Mode::Menu(Menu { x, y, items, sel });
    }

    pub fn pane_menu(&self, id: PaneId) -> Vec<Item> {
        let info = self.info(id);
        let mut m = vec![
            item("Split right", Act::Intent(Intent::Split { pane: id, edge: Edge::Right, local: false, cwd: None })),
            item("Split down", Act::Intent(Intent::Split { pane: id, edge: Edge::Bottom, local: false, cwd: None })),
            item(if self.zoom == Some(id) { "Unzoom" } else { "Zoom" }, Act::Zoom(id)),
            item("Move pane…", Act::Move(id)),
        ];
        if self.tab_view().is_some_and(|t| t.root.panes().len() > 1)
            && let Some(session) = self.tab.and_then(|t| self.session_of_tab(t))
        {
            m.push(item("Move to new tab", Act::Intent(Intent::BreakPane { pane: id, session, index: None })));
        }
        if info.is_some_and(|i| i.kind == BlockType::Terminal) {
            m.push(heading("After a restart"));
            let p = info.map(|i| i.policy.clone()).unwrap_or_default();
            let cmd = info.and_then(|i| i.command.clone());
            let short = |s: &str| {
                if s.chars().count() > 28 {
                    format!("{}…", s.chars().take(27).collect::<String>())
                } else {
                    s.to_owned()
                }
            };
            let set = |policy: Policy| Act::Pane(id, PaneOp::SetPolicy { policy });
            m.push(check("Start a shell here", matches!(p, Policy::Shell), set(Policy::Shell)));
            m.push(check(
                &cmd.as_deref().map_or("Re-run the command, asking first".into(), |c| {
                    format!("Re-run {}, asking first", short(c))
                }),
                matches!(p, Policy::Rerun { confirm: true }),
                set(Policy::Rerun { confirm: true }),
            ));
            m.push(check(
                &cmd.as_deref().map_or("Re-run the command".into(), |c| format!("Re-run {}", short(c))),
                matches!(p, Policy::Rerun { confirm: false }),
                set(Policy::Rerun { confirm: false }),
            ));
            m.push(check(
                &match &p {
                    Policy::Hook { command } => format!("Run {}", short(command)),
                    _ => "Run a command…".into(),
                },
                matches!(p, Policy::Hook { .. }),
                Act::Ask(Ask::Hook(id)),
            ));
            let resumes = info.and_then(|i| i.resumes.clone());
            m.push(check(
                &resumes.map_or("Resume the agent's conversation".into(), |r| format!("Resume {}", short(&r))),
                matches!(p, Policy::Resume),
                set(Policy::Resume),
            ));
            m.push(check("Nothing (wait for Enter)", matches!(p, Policy::None), set(Policy::None)));
            m.push(heading(""));
            let on = info.is_none_or(|i| i.integration);
            m.push(check("Shell integration (new shells)", on, Act::Pane(id, PaneOp::SetIntegration { on: !on })));
            m.push(item("Forget history", Act::Pane(id, PaneOp::Purge)));
        }
        if info.is_some_and(|i| matches!(i.attention, Attention::NeedsInput | Attention::Done)) {
            m.push(item("Dismiss", answer(id, Action::Dismiss, None)));
        }
        m.push(item("Close pane", Act::Intent(Intent::ClosePane { pane: id })));
        m
    }

    pub fn tab_menu(&self, tab: TabId) -> Vec<Item> {
        let session = self.session_of_tab(tab);
        let from = self.tab_view_of(tab).and_then(|t| t.root.panes().first().copied());
        let mut m = vec![item("Rename tab", Act::Ask(Ask::RenameTab(tab)))];
        if let Some(session) = session {
            m.push(item("New tab", Act::Intent(Intent::NewTab { session, from_pane: from, cwd: None })));
        }
        m.push(item("Close tab", Act::Intent(Intent::CloseTab { tab })));
        m
    }

    pub fn session_menu(&self, session: SessionId) -> Vec<Item> {
        vec![
            item("New tab", Act::Intent(Intent::NewTab { session, from_pane: self.focus, cwd: None })),
            item("New session", Act::Intent(Intent::NewSession { name: None, from_pane: self.focus })),
            item("Rename session", Act::Ask(Ask::RenameSession(session))),
            item("Close session", Act::Intent(Intent::CloseSession { session })),
        ]
    }

    pub fn wants_menu(&self, pane: PaneId, tab: TabId) -> Vec<Item> {
        let mut m = vec![item("Go there", Act::Go(tab, Some(pane)))];
        let asks = self.info(pane).and_then(|i| i.reason.as_ref()).map(|r| r.actions.clone()).unwrap_or_default();
        for a in asks {
            match a {
                Action::Allow => {
                    m.push(item("Allow", answer(pane, Action::Allow, None)));
                    m.push(item("Allow always", answer(pane, Action::Allow, Some("always"))));
                }
                Action::Deny => m.push(item("Deny", answer(pane, Action::Deny, None))),
                Action::Dismiss => m.push(item("Dismiss", answer(pane, Action::Dismiss, None))),
                Action::Continue => m.push(item("Continue", answer(pane, Action::Continue, None))),
                Action::Accept => m.push(item("Accept the edit", answer(pane, Action::Accept, None))),
                Action::Reject => m.push(item("Reject the edit", answer(pane, Action::Reject, None))),
                Action::Rerun => m.push(item("Rerun", answer(pane, Action::Rerun, None))),
                Action::Answer => {}
            }
        }
        m
    }

    fn command_menu(&self) -> Vec<Item> {
        let mut m = vec![heading("Ctrl-] then")];
        if let Some(pane) = self.focus {
            m.push(item(
                "v  Split right",
                Act::Intent(Intent::Split { pane, edge: Edge::Right, local: false, cwd: None }),
            ));
            m.push(item(
                "s  Split down",
                Act::Intent(Intent::Split { pane, edge: Edge::Bottom, local: false, cwd: None }),
            ));
            m.push(item("z  Zoom", Act::Zoom(pane)));
            m.push(item("x  Close pane", Act::Intent(Intent::ClosePane { pane })));
            if self.panes.contains_key(&pane) {
                m.push(item("[  Copy mode: select, search, copy", Act::Copy));
            }
        }
        if let Some(session) = self.tab.and_then(|t| self.session_of_tab(t)) {
            m.push(item("c  New tab", Act::Intent(Intent::NewTab { session, from_pane: self.focus, cwd: None })));
        }
        if let Some(t) = self.tab {
            m.push(item("r  Rename tab", Act::Ask(Ask::RenameTab(t))));
        }
        m.push(item("C  Claude Code conversations", Act::Conversations));
        m.push(item("w  Sidebar: tabs and what needs you", Act::Sidebar));
        m.push(item("b  Show or hide the sidebar", Act::ToggleSidebar));
        m.push(heading("o next pane · ←→↑↓ focus · n/p tab · 1-9 tab · m pane menu · t tab menu"));
        m.push(item("q  Detach", Act::Quit));
        m
    }

    // ---- the mouse ----

    fn mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(_) => self.buttons = self.buttons.saturating_add(1),
            MouseEventKind::Up(_) => self.buttons = self.buttons.saturating_sub(1),
            _ => {}
        }

        let full = self.area_full();
        if let Mode::Menu(menu) = &mut self.mode {
            let hit = super::draw::menu_item_at(menu, full, x, y);
            match m.kind {
                MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                    if let Some(i) = hit.filter(|i| !matches!(menu.items[*i].act, Act::None)) {
                        menu.sel = i;
                    }
                }
                MouseEventKind::Down(_) => {
                    let act = hit.map(|i| menu.items[i].act.clone());
                    self.mode = Mode::Normal;
                    if let Some(act) = act {
                        self.act(act);
                    }
                }
                _ => {}
            }
            return;
        }
        if matches!(self.mode, Mode::Prompt(_)) {
            return;
        }

        if self.selecting.is_some() {
            self.select_mouse(&m);
            return;
        }

        if let Some(mut d) = self.drag.take() {
            if let MouseEventKind::Drag(_) = m.kind {
                self.drag_divider(&mut d, x, y);
                self.drag = Some(d);
            }
            return;
        }

        let rects = self.rects();
        if let Some(mv) = &mut self.moving {
            let target = rects
                .into_iter()
                .find(|(_, r)| r.contains((x, y).into()))
                .and_then(|(p, r)| (p != mv.pane).then(|| (p, edge_at(r, x, y))));
            mv.target = target;
            let done = match m.kind {
                MouseEventKind::Up(MouseButton::Left) => !mv.by_click,
                MouseEventKind::Down(MouseButton::Left) => mv.by_click,
                MouseEventKind::Down(_) => {
                    self.moving = None;
                    return;
                }
                _ => false,
            };
            if done {
                let mv = self.moving.take().expect("moving");
                if let Some((target, edge)) = mv.target {
                    self.act(Act::Intent(Intent::MovePane { pane: mv.pane, target, edge }));
                }
            }
            return;
        }

        if x < self.area.x {
            self.sidebar_mouse(m);
            return;
        }

        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && !m.modifiers.contains(KeyModifiers::ALT)
            && let Some(d) = self.divider_at(x, y)
        {
            self.drag = Some(d);
            return;
        }

        let Some((pane, r)) = self.pane_at(x, y) else { return };
        let (lx, ly) = (x - r.x, y - r.y);
        if let MouseEventKind::Down(btn) = m.kind {
            self.set_focus(pane);
            if btn == MouseButton::Left && m.modifiers.contains(KeyModifiers::ALT) {
                self.moving = Some(Moving { pane, target: None, by_click: false });
                return;
            }
        }
        let reporting = self.panes.get(&pane).is_some_and(|t| t.engine.mouse_reporting());
        // Drag selects: with Shift when the program takes the mouse, as
        // terminals do.
        if (!reporting || m.modifiers.contains(KeyModifiers::SHIFT)) && self.select_press(pane, r, &m) {
            return;
        }
        let wheel = matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown);
        if !reporting {
            if let MouseEventKind::Down(MouseButton::Right) = m.kind {
                let items = self.pane_menu(pane);
                self.open_menu(x, y, items);
                return;
            }
            if wheel {
                self.wheel(pane, m.kind == MouseEventKind::ScrollUp);
            }
            return;
        }
        let Some(t) = self.panes.get_mut(&pane) else { return };
        let (action, button) = match m.kind {
            MouseEventKind::Down(b) => (mouse::Action::Press, Some(button(b))),
            MouseEventKind::Up(b) => (mouse::Action::Release, Some(button(b))),
            MouseEventKind::Drag(b) => (mouse::Action::Motion, Some(button(b))),
            MouseEventKind::Moved => (mouse::Action::Motion, None),
            MouseEventKind::ScrollUp => (mouse::Action::Press, Some(mouse::Button::Four)),
            MouseEventKind::ScrollDown => (mouse::Action::Press, Some(mouse::Button::Five)),
            MouseEventKind::ScrollLeft => (mouse::Action::Press, Some(mouse::Button::Six)),
            MouseEventKind::ScrollRight => (mouse::Action::Press, Some(mouse::Button::Seven)),
        };
        let Ok(mut ev) = mouse::Event::new() else { return };
        ev.set_action(action).set_button(button).set_mods(mods(m.modifiers));
        let data = t.engine.encode_mouse(&mut ev, lx, ly, self.buttons > 0);
        self.conn.input(pane, data);
    }

    /// The wheel over a pane whose program doesn't take the mouse.
    fn wheel(&mut self, pane: PaneId, up: bool) {
        if let Some(t) = self.panes.get_mut(&pane) {
            if t.engine.alt_screen() {
                // A pager or editor: arrow keys, as its own keys would be.
                let arrow = if up { KeyCode::Up } else { KeyCode::Down };
                let k = KeyEvent::new(arrow, KeyModifiers::NONE);
                let data = keys::event(&k).map(|e| t.engine.encode_key(&e)).unwrap_or_default().repeat(3);
                self.conn.input(pane, data);
            } else {
                t.view_mut().scroll(if up { -3 } else { 3 });
            }
            return;
        }
        let s = self.scroll.entry(pane).or_default();
        *s = if up { *s + 3 } else { s.saturating_sub(3) };
    }

    fn sidebar_mouse(&mut self, m: MouseEvent) {
        let MouseEventKind::Down(btn) = m.kind else { return };
        let Some(&(_, hit)) = self.hits.iter().find(|(row, _)| *row == m.row) else { return };
        let (x, y) = (m.column, m.row);
        match (btn, hit) {
            (MouseButton::Left, Hit::Tab(t)) => self.select(t, None),
            (MouseButton::Left, Hit::Wants(p, t)) => self.select(t, Some(p)),
            (MouseButton::Right, Hit::Tab(t)) => self.open_menu(x, y, self.tab_menu(t)),
            (MouseButton::Right, Hit::Session(s)) => self.open_menu(x, y, self.session_menu(s)),
            (MouseButton::Right, Hit::Wants(p, t)) => self.open_menu(x, y, self.wants_menu(p, t)),
            _ => {}
        }
    }

    fn divider_at(&self, x: u16, y: u16) -> Option<DividerDrag> {
        let tab = self.tab_view()?;
        let (lx, ly) = (x.checked_sub(self.area.x)?, y.checked_sub(self.area.y)?);
        for s in &tab.layout.splits {
            let r = s.rect;
            let (along, across, start, lo, hi) = match s.dir {
                Dir::Row => (lx, ly, r.x, r.y, r.y + r.rows),
                Dir::Column => (ly, lx, r.y, r.x, r.x + r.cols),
            };
            if across < lo || across >= hi {
                continue;
            }
            let mut at = start;
            for i in 0..s.extents.len().saturating_sub(1) {
                at += s.extents[i];
                if along == at {
                    return Some(DividerDrag { split: s.id, index: i, dir: s.dir, start, extents: s.extents.clone() });
                }
                at += 1;
            }
        }
        None
    }

    fn drag_divider(&mut self, d: &mut DividerDrag, x: u16, y: u16) {
        let along = match d.dir {
            Dir::Row => x.saturating_sub(self.area.x),
            Dir::Column => y.saturating_sub(self.area.y),
        };
        let i = d.index;
        let a0 = d.start + d.extents[..i].iter().sum::<u16>() + i as u16;
        let both = d.extents[i] + d.extents[i + 1];
        let a = along.saturating_sub(a0).clamp(1, both.saturating_sub(1).max(1));
        if a != d.extents[i] {
            d.extents[i] = a;
            d.extents[i + 1] = both - a;
            let weights = d.extents.iter().map(|e| *e as f64).collect();
            self.intent(Intent::ResizeSplit { split: d.split, weights });
        }
    }

    /// The whole screen (for menus placed anywhere).
    pub fn area_full(&self) -> Rect {
        self.full
    }

    /// Errors from API calls made in the background.
    pub fn take_errors(&mut self) {
        while let Ok(e) = self.errors.try_recv() {
            self.say(e);
        }
    }
}

fn button(b: MouseButton) -> mouse::Button {
    match b {
        MouseButton::Left => mouse::Button::Left,
        MouseButton::Right => mouse::Button::Right,
        MouseButton::Middle => mouse::Button::Middle,
    }
}

fn mods(m: KeyModifiers) -> key::Mods {
    let mut out = key::Mods::empty();
    if m.contains(KeyModifiers::SHIFT) {
        out |= key::Mods::SHIFT;
    }
    if m.contains(KeyModifiers::CONTROL) {
        out |= key::Mods::CTRL;
    }
    if m.contains(KeyModifiers::ALT) {
        out |= key::Mods::ALT;
    }
    out
}

/// Which edge of `r` a point is nearest: the middle third swaps.
pub fn edge_at(r: Rect, x: u16, y: u16) -> Edge {
    let fx = (x - r.x) as f32 / r.width.max(1) as f32;
    let fy = (y - r.y) as f32 / r.height.max(1) as f32;
    if (0.33..0.67).contains(&fx) && (0.33..0.67).contains(&fy) {
        return Edge::Center;
    }
    let d = [(fx, Edge::Left), (1.0 - fx, Edge::Right), (fy, Edge::Top), (1.0 - fy, Edge::Bottom)];
    d.into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, e)| e).unwrap_or(Edge::Center)
}

fn answer(pane: PaneId, action: Action, option: Option<&str>) -> Act {
    let mut body = json!({ "action": action, "pane": pane });
    if let Some(o) = option {
        body["option"] = json!(o);
    }
    let what = match action {
        Action::Allow => "allow that",
        Action::Deny => "deny that",
        Action::Dismiss => "dismiss that",
        Action::Answer => "answer that",
        Action::Continue => "continue",
        Action::Accept => "accept that",
        Action::Reject => "reject that",
        Action::Rerun => "run that again",
    };
    Act::Api("/api/attention/act".into(), body, what)
}

fn item(label: &str, act: Act) -> Item {
    Item { label: label.into(), checked: false, act }
}

fn check(label: &str, checked: bool, act: Act) -> Item {
    Item { label: label.into(), checked, act }
}

fn heading(label: &str) -> Item {
    Item { label: label.into(), checked: false, act: Act::None }
}

fn next_item(items: &[Item], from: usize) -> usize {
    let n = items.len();
    (1..=n).map(|k| from.wrapping_add(k) % n.max(1)).find(|i| !matches!(items[*i].act, Act::None)).unwrap_or(0)
}

fn prev_item(items: &[Item], from: usize) -> usize {
    let n = items.len();
    (1..=n).map(|k| (from + n * 2 - k) % n.max(1)).find(|i| !matches!(items[*i].act, Act::None)).unwrap_or(0)
}
