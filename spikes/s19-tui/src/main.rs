//! S19: what illogical feels like as a TUI (herdr's shape).
//!
//! One tab at a time, every pane attached over the daemon's socket. Each
//! pane has a local libghostty terminal fed with the same snapshot and
//! output frames the web client gets; a frame copies their cells into a
//! ratatui buffer beside a sidebar of sessions, tabs and what needs you.
//!
//! Mouse: click a tab or a "needs you" line, click a pane to focus it, drag
//! a divider, wheel to scroll back. Ctrl-] then: v split right, s split
//! down, c new tab, x close pane, o next pane, n/p next/previous tab, q quit,
//! Ctrl-] a literal Ctrl-].
//!
//! `S19_STATS=path` writes frame timings there on exit.

use std::{
    collections::{HashMap, HashSet},
    io::{ErrorKind, Read, Write},
    os::{fd::AsFd, unix::net::UnixStream},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Context;
use illogical_proto::{
    AttachPane, Attention, BlockType, ClientMsg, Dir, Edge, Frame, FrameKind, Intent, NodeId, PaneId, ServerMsg,
    State, TabId, TabView,
};
use libghostty_vt::{
    RenderState, Terminal,
    render::{CellIterator, RowIterator},
    screen::{CellContentTag, CellWide, Screen},
    style::{StyleColor, Underline},
    terminal::{Mode, ModeKind, ScrollViewport},
};
use nix::{
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::termios::{self, SetArg},
};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    prelude::CrosstermBackend,
    style::{Color, Modifier, Style},
};
use tungstenite::{Message, WebSocket};

const SIDEBAR: u16 = 26;
const PREFIX: u8 = 0x1d; // Ctrl-]
const FRAME: Duration = Duration::from_millis(16);
/// Rows of history to ask for in a snapshot.
const SCROLLBACK: u32 = 10_000;

/// `S19_LEGACY=1`: attach as before #49 (all history, no zstd, a fresh
/// snapshot on resync), to compare.
fn legacy() -> bool {
    std::env::var_os("S19_LEGACY").is_some()
}

/// Ack what the engines take in (#52), unless `S19_NO_ACKS` (or legacy).
fn acks() -> bool {
    !legacy() && std::env::var_os("S19_NO_ACKS").is_none()
}

struct Pane {
    term: Terminal<'static, 'static>,
    rs: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    sym: String,
    /// Just past the last byte we have, to resume from after a resync.
    offset: Option<u64>,
    resyncs: u32,
    /// Asked for the screen alone after a resync: keep the scrollback.
    resync: bool,
    /// The offset last acked (#52).
    acked: u64,
}

impl Pane {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            term: Terminal::new(cols.max(1), rows.max(1)).expect("terminal"),
            rs: RenderState::new().expect("render state"),
            rows: RowIterator::new().expect("rows"),
            cells: CellIterator::new().expect("cells"),
            sym: String::new(),
            offset: None,
            resyncs: 0,
            resync: false,
            acked: 0,
        }
    }

    fn dec(&self, m: u16) -> bool {
        self.term.mode(Mode::new(m, ModeKind::Dec)).unwrap_or(false)
    }

    fn mouse_tracking(&self) -> bool {
        self.dec(1000) || self.dec(1002) || self.dec(1003)
    }

    fn alt(&self) -> bool {
        self.term.active_screen().ok() == Some(Screen::Alternate)
    }

    /// Copy the visible cells into `buf` at `area`; the cursor, if shown.
    fn draw(&mut self, buf: &mut Buffer, area: Rect) -> Option<Position> {
        let Pane { term, rs, rows, cells, sym, .. } = self;
        let snap = rs.update(term).ok()?;
        {
            let mut ri = rows.update(&snap).ok()?;
            let mut y = 0;
            while let Some(row) = ri.next() {
                if y >= area.height {
                    break;
                }
                if let Ok(mut ci) = cells.update(row) {
                    let mut x = 0;
                    while let Some(cell) = ci.next() {
                        if x >= area.width {
                            break;
                        }
                        let raw = cell.raw_cell().ok();
                        let st = cell.style().unwrap_or_default();
                        let mut bg = color(st.bg_color);
                        if bg == Color::Reset
                            && let Some(raw) = raw
                        {
                            bg = match raw.content_tag() {
                                Ok(CellContentTag::BgColorPalette) => {
                                    raw.bg_color_palette().map(|p| Color::Indexed(p.0)).unwrap_or(Color::Reset)
                                }
                                Ok(CellContentTag::BgColorRgb) => {
                                    raw.bg_color_rgb().map(|c| Color::Rgb(c.r, c.g, c.b)).unwrap_or(Color::Reset)
                                }
                                _ => Color::Reset,
                            };
                        }
                        let mut m = Modifier::empty();
                        for (on, f) in [
                            (st.bold, Modifier::BOLD),
                            (st.italic, Modifier::ITALIC),
                            (st.faint, Modifier::DIM),
                            (st.blink, Modifier::SLOW_BLINK),
                            (st.inverse, Modifier::REVERSED),
                            (st.invisible, Modifier::HIDDEN),
                            (st.strikethrough, Modifier::CROSSED_OUT),
                            (st.underline != Underline::None, Modifier::UNDERLINED),
                        ] {
                            if on {
                                m |= f;
                            }
                        }
                        sym.clear();
                        let tail = raw.and_then(|r| r.wide().ok()) == Some(CellWide::SpacerTail);
                        if !tail {
                            let _ = cell.graphemes_utf8(sym);
                        }
                        if sym.is_empty() {
                            sym.push(' ');
                        }
                        if let Some(out) = buf.cell_mut((area.x + x, area.y + y)) {
                            out.set_symbol(sym)
                                .set_style(Style::default().fg(color(st.fg_color)).bg(bg).add_modifier(m));
                        }
                        x += 1;
                    }
                }
                let _ = row.set_dirty(false);
                y += 1;
            }
        }
        if !snap.cursor_visible().unwrap_or(false) {
            return None;
        }
        let c = snap.cursor_viewport().ok()??;
        (c.x < area.width && c.y < area.height).then(|| Position::new(area.x + c.x, area.y + c.y))
    }
}

fn color(c: StyleColor) -> Color {
    match c {
        StyleColor::None => Color::Reset,
        StyleColor::Palette(p) => Color::Indexed(p.0),
        StyleColor::Rgb(c) => Color::Rgb(c.r, c.g, c.b),
    }
}

struct Drag {
    split: NodeId,
    index: usize,
    dir: Dir,
    /// Where the split starts along its direction, in tab cells.
    start: u16,
    extents: Vec<u16>,
}

#[derive(Default)]
struct Stats {
    build: Vec<Duration>,
    draw: Vec<Duration>,
    bytes: u64,
    snapshot_bytes: u64,
    /// Snapshots before decompression.
    snapshot_raw: u64,
    resyncs: u32,
    started: Option<Instant>,
}

struct App {
    ws: WebSocket<UnixStream>,
    state: Option<State>,
    tab: Option<TabId>,
    panes: HashMap<PaneId, Pane>,
    focus: Option<PaneId>,
    /// Where the tab is drawn on screen.
    area: Rect,
    prefix: bool,
    drag: Option<Drag>,
    /// Sidebar rows that go somewhere.
    hits: Vec<(u16, TabId, Option<PaneId>)>,
    /// Focus whatever appears next (after our own split or new tab).
    follow_new: bool,
    seen_panes: HashSet<PaneId>,
    seen_tabs: HashSet<TabId>,
    status: String,
    stats: Stats,
    dirty: bool,
    quit: bool,
}

impl App {
    fn send(&mut self, msg: &ClientMsg) {
        let r = self.ws.send(Message::Text(serde_json::to_string(msg).expect("json").into()));
        match r {
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => self.status = format!("send: {e}"),
            Ok(()) => {}
        }
    }

    fn intent(&mut self, intent: Intent) {
        self.send(&ClientMsg::Intent { id: None, intent });
    }

    fn input(&mut self, pane: PaneId, data: Vec<u8>) {
        let f = Frame { kind: FrameKind::Input, pane, offset: 0, data };
        match self.ws.send(Message::Binary(f.encode().into())) {
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => self.status = format!("send: {e}"),
            Ok(()) => {}
        }
    }

    fn tab_view(&self) -> Option<&TabView> {
        let s = self.state.as_ref()?;
        s.tabs.iter().find(|t| Some(t.id) == self.tab)
    }

    fn tab_order(&self) -> Vec<TabId> {
        self.state.as_ref().map(|s| s.sessions.iter().flat_map(|s| s.tabs.iter().copied()).collect()).unwrap_or_default()
    }

    fn select(&mut self, tab: TabId, pane: Option<PaneId>) {
        if self.tab != Some(tab) {
            let old: Vec<PaneId> = self.panes.keys().copied().collect();
            if !old.is_empty() {
                self.send(&ClientMsg::Detach { panes: old });
            }
            self.panes.clear();
            self.tab = Some(tab);
            self.view();
        }
        self.sync();
        if let Some(p) = pane {
            self.set_focus(p);
        }
    }

    fn view(&mut self) {
        if let Some(tab) = self.tab {
            let (cols, rows) = (self.area.width, self.area.height);
            self.send(&ClientMsg::View { tab, cols, rows, zoom: None, claim: true });
        }
    }

    fn set_focus(&mut self, p: PaneId) {
        if self.focus != Some(p) {
            self.focus = Some(p);
            self.send(&ClientMsg::Focus { pane: Some(p) });
        }
    }

    /// Attach the current tab's terminals we don't have; drop the rest.
    fn sync(&mut self) {
        let Some(state) = self.state.as_ref() else { return };
        let tabs: HashSet<TabId> = state.tabs.iter().map(|t| t.id).collect();
        let new_tab = tabs.difference(&self.seen_tabs).next().copied();
        self.seen_tabs = tabs;
        if self.follow_new && new_tab.is_some() {
            self.follow_new = false;
            self.select(new_tab.unwrap(), None);
            return;
        }
        if self.tab_view().is_none() {
            match self.tab_order().first() {
                Some(&t) => {
                    self.tab = None;
                    return self.select(t, None);
                }
                None => {
                    self.quit = true;
                    return;
                }
            }
        }
        let state = self.state.as_ref().unwrap();
        let tab = self.tab_view().unwrap();
        let kinds: HashMap<PaneId, BlockType> = state.panes.iter().map(|p| (p.id, p.kind)).collect();
        let here: Vec<(PaneId, u16, u16)> = tab
            .layout
            .panes
            .iter()
            .filter(|(p, _)| kinds.get(p) == Some(&BlockType::Terminal))
            .map(|(p, r)| (*p, r.cols, r.rows))
            .collect();
        let ids: HashSet<PaneId> = tab.layout.panes.iter().map(|(p, _)| *p).collect();
        let first = tab.layout.panes.first().map(|(p, _)| *p);
        let fresh: Vec<PaneId> = ids.difference(&self.seen_panes).copied().collect();
        self.seen_panes.extend(ids.iter().copied());

        let gone: Vec<PaneId> = self.panes.keys().filter(|p| !ids.contains(p)).copied().collect();
        if !gone.is_empty() {
            for p in &gone {
                self.panes.remove(p);
            }
            self.send(&ClientMsg::Detach { panes: gone });
        }
        let mut attach = vec![];
        for (p, cols, rows) in here {
            self.panes.entry(p).or_insert_with(|| {
                attach.push(AttachPane { pane: p, offset: None, history: (!legacy()).then_some(SCROLLBACK) });
                Pane::new(cols, rows)
            });
        }
        if !attach.is_empty() {
            self.send(&ClientMsg::Attach { panes: attach, zstd: !legacy(), acks: acks() });
        }
        if self.follow_new
            && let Some(&p) = fresh.first()
        {
            self.follow_new = false;
            self.set_focus(p);
        }
        if !self.focus.is_some_and(|f| ids.contains(&f))
            && let Some(p) = first
        {
            self.focus = None;
            self.set_focus(p);
        }
    }

    fn on_text(&mut self, t: &str) {
        let Ok(msg) = serde_json::from_str::<ServerMsg>(t) else { return };
        match msg {
            ServerMsg::Hello { state, .. } | ServerMsg::State { state } => {
                self.state = Some(state);
                self.sync();
            }
            // Pane changes other than layout come as deltas (M23).
            ServerMsg::Delta { delta } => {
                if let Some(state) = self.state.as_mut() {
                    state.apply(&delta);
                }
            }
            ServerMsg::Size { pane, cols, rows } => {
                if let Some(p) = self.panes.get_mut(&pane) {
                    let _ = p.term.resize(cols, rows, 8, 16);
                }
            }
            ServerMsg::Resync { pane } => {
                // Resume from what we have: the daemon replays up to 1MB
                // from its log rather than sending the whole scrollback.
                if let Some(p) = self.panes.get_mut(&pane) {
                    p.resyncs += 1;
                    self.stats.resyncs += 1;
                    // #49: from our offset, and the screen alone if the gap is
                    // too big to replay. S19_LEGACY: as before #49.
                    let (offset, history) = if legacy() { (None, None) } else { (p.offset, Some(0)) };
                    p.resync = history == Some(0) && offset.is_some();
                    self.send(&ClientMsg::Attach { panes: vec![AttachPane { pane, offset, history }], zstd: !legacy(), acks: acks() });
                }
            }
            ServerMsg::Error { message, .. } => self.status = message,
            ServerMsg::Notice { message } => self.status = message,
            _ => {}
        }
        self.dirty = true;
    }

    fn on_frame(&mut self, b: &[u8]) {
        let Ok(f) = Frame::decode(b) else { return };
        let Some(p) = self.panes.get_mut(&f.pane) else { return };
        self.stats.bytes += f.data.len() as u64;
        let snapshot = matches!(f.kind, FrameKind::Snapshot | FrameKind::SnapshotZstd);
        let data = if f.kind == FrameKind::SnapshotZstd { zstd::decode_all(&f.data[..]).unwrap_or_default() } else { f.data };
        if snapshot {
            self.stats.snapshot_bytes += b.len() as u64 - 13;
            self.stats.snapshot_raw += data.len() as u64;
            if p.resync {
                // Keep the scrollback: push the screen into it under a rule,
                // then start the screen and modes over (as the web client).
                let rows = p.term.rows().unwrap_or(24);
                let gap = format!(
                    "\x1b[?1049l\x1b[0m\x1b[{rows};1H\r\n\x1b[2m── output skipped here ──\x1b[0m{}\x1b[!p\x1b[?7h\x1b[?1l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b]104\x1b\\\x1b[H\x1b[2J",
                    "\r\n".repeat(rows as usize)
                );
                p.term.vt_write(gap.as_bytes());
            } else {
                p.term.reset();
            }
            p.resync = false;
        }
        p.term.vt_write(&data);
        let end = f.offset + if snapshot { 0 } else { data.len() as u64 };
        p.offset = Some(end);
        // #52: the engine has taken it in; say so about every 64 KB.
        if snapshot {
            p.acked = end;
        } else if acks() && end - p.acked >= 64 * 1024 {
            p.acked = end;
            self.send(&ClientMsg::Ack { pane: f.pane, offset: end });
        }
        self.dirty = true;
    }

    fn on_stdin(&mut self, data: &[u8]) {
        let mut fwd = Vec::new();
        let mut i = 0;
        while i < data.len() {
            if self.prefix {
                self.prefix = false;
                self.flush_input(&mut fwd);
                if data[i] == PREFIX {
                    fwd.push(PREFIX);
                } else {
                    self.command(data[i]);
                }
                i += 1;
                self.dirty = true;
                continue;
            }
            if data[i] == PREFIX {
                self.prefix = true;
                self.dirty = true;
                i += 1;
                continue;
            }
            if let Some((ev, n)) = parse_sgr_mouse(&data[i..]) {
                self.flush_input(&mut fwd);
                self.mouse(ev);
                i += n;
                continue;
            }
            fwd.push(data[i]);
            i += 1;
        }
        self.flush_input(&mut fwd);
    }

    fn flush_input(&mut self, fwd: &mut Vec<u8>) {
        if fwd.is_empty() {
            return;
        }
        let data = std::mem::take(fwd);
        let Some(f) = self.focus else { return };
        let Some(p) = self.panes.get_mut(&f) else { return };
        p.term.scroll_viewport(ScrollViewport::Bottom);
        // The outer terminal encodes keys for its own modes, not the pane's:
        // cursor keys in application mode (DECCKM) are SS3, not CSI.
        let data = if p.dec(1) { ckm(&data) } else { data };
        self.input(f, data);
    }

    fn command(&mut self, c: u8) {
        let focus = self.focus;
        match c {
            b'q' => self.quit = true,
            b'v' | b'|' | b's' | b'-' => {
                if let Some(pane) = focus {
                    let edge = if matches!(c, b'v' | b'|') { Edge::Right } else { Edge::Bottom };
                    self.follow_new = true;
                    self.intent(Intent::Split { pane, edge, local: false, cwd: None });
                }
            }
            b'x' => {
                if let Some(pane) = focus {
                    self.intent(Intent::ClosePane { pane });
                }
            }
            b'c' => {
                let session = self.state.as_ref().and_then(|s| {
                    s.sessions.iter().find(|s| self.tab.is_some_and(|t| s.tabs.contains(&t))).map(|s| s.id)
                });
                if let Some(session) = session {
                    self.follow_new = true;
                    self.intent(Intent::NewTab { session, from_pane: focus, cwd: None });
                }
            }
            b'o' => {
                if let Some(tab) = self.tab_view() {
                    let ids: Vec<PaneId> = tab.layout.panes.iter().map(|(p, _)| *p).collect();
                    let i = ids.iter().position(|p| Some(*p) == focus).map_or(0, |i| (i + 1) % ids.len());
                    if let Some(&p) = ids.get(i) {
                        self.set_focus(p);
                    }
                }
            }
            b'n' | b'p' => {
                let order = self.tab_order();
                if let Some(i) = order.iter().position(|t| Some(*t) == self.tab) {
                    let j = if c == b'n' { (i + 1) % order.len() } else { (i + order.len() - 1) % order.len() };
                    self.select(order[j], None);
                }
            }
            _ => {}
        }
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<(PaneId, u16, u16)> {
        let tab = self.tab_view()?;
        let (lx, ly) = (x.checked_sub(self.area.x)?, y.checked_sub(self.area.y)?);
        tab.layout.panes.iter().find_map(|(p, r)| {
            (lx >= r.x && lx < r.x + r.cols && ly >= r.y && ly < r.y + r.rows).then_some((*p, lx - r.x, ly - r.y))
        })
    }

    fn divider_at(&self, x: u16, y: u16) -> Option<Drag> {
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
                    return Some(Drag { split: s.id, index: i, dir: s.dir, start, extents: s.extents.clone() });
                }
                at += 1;
            }
        }
        None
    }

    fn mouse(&mut self, ev: MouseEv) {
        self.dirty = true;
        let wheel = ev.b & 64 != 0;
        let motion = ev.b & 32 != 0;
        let button = ev.b & 3;

        if let Some(mut d) = self.drag.take() {
            if !ev.press {
                return;
            }
            let along = match d.dir {
                Dir::Row => ev.x.saturating_sub(self.area.x),
                Dir::Column => ev.y.saturating_sub(self.area.y),
            };
            let i = d.index;
            let a0 = d.start + d.extents[..i].iter().sum::<u16>() + i as u16;
            let both = d.extents[i] + d.extents[i + 1];
            let a = along.saturating_sub(a0).clamp(1, both - 1);
            if a != d.extents[i] {
                d.extents[i] = a;
                d.extents[i + 1] = both - a;
                let weights = d.extents.iter().map(|e| *e as f64).collect();
                self.intent(Intent::ResizeSplit { split: d.split, weights });
            }
            self.drag = Some(d);
            return;
        }

        if ev.x < SIDEBAR {
            if ev.press && !motion && !wheel && button == 0
                && let Some(&(_, tab, pane)) = self.hits.iter().find(|h| h.0 == ev.y)
            {
                self.select(tab, pane);
            }
            return;
        }

        if ev.press && !motion && !wheel && button == 0
            && let Some(d) = self.divider_at(ev.x, ev.y)
        {
            self.drag = Some(d);
            return;
        }

        let Some((pid, lx, ly)) = self.pane_at(ev.x, ev.y) else { return };
        if ev.press && !motion && !wheel {
            self.set_focus(pid);
        }
        let Some(p) = self.panes.get_mut(&pid) else { return };
        if p.mouse_tracking() && p.dec(1006) {
            if motion && !p.dec(1002) && !p.dec(1003) {
                return;
            }
            let s = format!("\x1b[<{};{};{}{}", ev.b, lx + 1, ly + 1, if ev.press { 'M' } else { 'm' });
            self.input(pid, s.into_bytes());
        } else if wheel {
            let up = ev.b & 1 == 0;
            if p.alt() {
                let key: &[u8] = match (up, p.dec(1)) {
                    (true, true) => b"\x1bOA",
                    (true, false) => b"\x1b[A",
                    (false, true) => b"\x1bOB",
                    (false, false) => b"\x1b[B",
                };
                self.input(pid, key.repeat(3));
            } else {
                p.term.scroll_viewport(ScrollViewport::Delta(if up { -3 } else { 3 }));
            }
        }
    }

    fn draw(&mut self, f: &mut ratatui::Frame) {
        let t0 = Instant::now();
        let full = f.area();
        let area = Rect {
            x: SIDEBAR + 1,
            y: 0,
            width: full.width.saturating_sub(SIDEBAR + 1),
            height: full.height.saturating_sub(1),
        };
        if area != self.area {
            self.area = area;
            self.view();
        }
        let buf = f.buffer_mut();
        self.draw_sidebar(buf, full.height.saturating_sub(1));
        for y in 0..full.height.saturating_sub(1) {
            if let Some(c) = buf.cell_mut((SIDEBAR, y)) {
                c.set_symbol("│").set_fg(Color::DarkGray);
            }
        }

        let mut cursor = None;
        if let Some(tab) = self.state.as_ref().and_then(|s| s.tabs.iter().find(|t| Some(t.id) == self.tab)).cloned() {
            for (pid, r) in &tab.layout.panes {
                let rect = Rect { x: area.x + r.x, y: area.y + r.y, width: r.cols, height: r.rows }.intersection(area);
                match self.panes.get_mut(pid) {
                    Some(p) => {
                        let c = p.draw(buf, rect);
                        if self.focus == Some(*pid) {
                            cursor = c;
                        }
                    }
                    None => {
                        let what = self
                            .state
                            .as_ref()
                            .and_then(|s| s.panes.iter().find(|p| p.id == *pid))
                            .map_or("block", |p| match p.kind {
                                BlockType::Agent => "agent block",
                                BlockType::Browser => "browser block",
                                BlockType::Terminal => "terminal",
                            });
                        buf.set_stringn(
                            rect.x + 1,
                            rect.y,
                            format!("%{pid} {what}: open it in the browser"),
                            rect.width.saturating_sub(1) as usize,
                            Style::default().fg(Color::DarkGray),
                        );
                    }
                }
            }
            let focus_rect = tab.layout.panes.iter().find(|(p, _)| Some(*p) == self.focus).map(|(_, r)| *r);
            for s in &tab.layout.splits {
                let r = s.rect;
                let mut at = 0;
                for i in 0..s.extents.len().saturating_sub(1) {
                    at += s.extents[i];
                    let len = if s.dir == Dir::Row { r.rows } else { r.cols };
                    for k in 0..len {
                        let (x, y, sym) = match s.dir {
                            Dir::Row => (r.x + at, r.y + k, "│"),
                            Dir::Column => (r.x + k, r.y + at, "─"),
                        };
                        // The divider is lit where it borders the focused pane.
                        let lit = focus_rect.is_some_and(|f| {
                            let near_x = x + 1 >= f.x && x <= f.x + f.cols;
                            let near_y = y + 1 >= f.y && y <= f.y + f.rows;
                            near_x && near_y
                        });
                        if let Some(c) = buf.cell_mut((area.x + x, area.y + y)) {
                            c.set_symbol(sym).set_style(
                                Style::default().fg(if lit { Color::Cyan } else { Color::DarkGray }).bg(Color::Reset),
                            );
                        }
                    }
                    at += 1;
                }
            }
        }

        self.draw_status(buf, full);
        if let Some(c) = cursor {
            f.set_cursor_position(c);
        }
        self.stats.build.push(t0.elapsed());
    }

    fn draw_sidebar(&mut self, buf: &mut Buffer, height: u16) {
        self.hits.clear();
        let Some(state) = self.state.as_ref() else { return };
        let mut y = 0;
        let line = |buf: &mut Buffer, y: &mut u16, text: &str, style: Style| {
            if *y < height {
                buf.set_stringn(0, *y, format!("{text:<w$}", w = SIDEBAR as usize), SIDEBAR as usize, style);
            }
            *y += 1;
        };
        line(buf, &mut y, " illogical", Style::default().add_modifier(Modifier::BOLD));
        y += 1;
        let info = |p: PaneId| state.panes.iter().find(|i| i.id == p);
        for s in &state.sessions {
            line(buf, &mut y, &format!(" {}", s.name), Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
            for t in &s.tabs {
                let Some(tv) = state.tabs.iter().find(|v| v.id == *t) else { continue };
                let panes: Vec<_> = tv.layout.panes.iter().filter_map(|(p, _)| info(*p)).collect();
                let label = tv.name.clone().unwrap_or_else(|| {
                    panes
                        .first()
                        .and_then(|p| {
                            p.command.clone().or_else(|| {
                                p.cwd.as_deref().map(|c| c.rsplit('/').next().unwrap_or(c).to_owned())
                            })
                        })
                        .unwrap_or_else(|| "shell".into())
                });
                let worst = panes.iter().map(|p| rank(p.attention)).max().unwrap_or(0);
                let (glyph, gc) = glyph(worst);
                let sel = Some(*t) == self.tab;
                let mut text: String = format!("  {} {label}", if sel { "▸" } else { " " }).chars().take(22).collect();
                while text.chars().count() < 23 {
                    text.push(' ');
                }
                let style = if sel { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
                if y < height {
                    buf.set_stringn(0, y, format!("{text}{glyph}  "), SIDEBAR as usize, style);
                    if let Some(c) = buf.cell_mut((23, y)) {
                        c.set_fg(gc);
                    }
                    self.hits.push((y, *t, None));
                }
                y += 1;
            }
        }
        let wanting: Vec<_> =
            state.panes.iter().filter(|p| matches!(p.attention, Attention::NeedsInput | Attention::Done)).collect();
        if !wanting.is_empty() {
            y += 1;
            line(buf, &mut y, " needs you", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
            for p in wanting {
                let Some(tab) = state.tabs.iter().find(|t| t.layout.panes.iter().any(|(q, _)| *q == p.id)) else {
                    continue;
                };
                let (glyph, gc) = glyph(rank(p.attention));
                let what = p
                    .reason
                    .as_ref()
                    .map(|r| r.headline.clone())
                    .or_else(|| p.command.clone())
                    .unwrap_or_else(|| "wants you".into());
                if y < height {
                    buf.set_stringn(0, y, format!("  {glyph} %{} {what}", p.id), SIDEBAR as usize, Style::default());
                    if let Some(c) = buf.cell_mut((2, y)) {
                        c.set_fg(gc);
                    }
                    self.hits.push((y, tab.id, Some(p.id)));
                }
                y += 1;
            }
        }
    }

    fn draw_status(&self, buf: &mut Buffer, full: Rect) {
        let y = full.height.saturating_sub(1);
        let (left, style) = if self.prefix {
            (
                " v split right · s split down · c new tab · x close · o next pane · n/p tab · q quit".to_owned(),
                Style::default().bg(Color::Cyan).fg(Color::Black),
            )
        } else if !self.status.is_empty() {
            (format!(" {}", self.status), Style::default().fg(Color::Red))
        } else {
            let what = self
                .focus
                .and_then(|f| self.state.as_ref()?.panes.iter().find(|p| p.id == f))
                .map(|p| format!(" %{} {}", p.id, p.command.as_deref().or(p.cwd.as_deref()).unwrap_or("")))
                .unwrap_or_default();
            (format!("{what}  ·  ^] menu · click to focus · drag dividers · wheel scrolls"), Style::default().fg(Color::DarkGray))
        };
        buf.set_stringn(0, y, format!("{left:<w$}", w = full.width as usize), full.width as usize, style);
        if let Some(last) = self.stats.build.last() {
            let r = format!(" frame {:.2}ms ", last.as_secs_f64() * 1e3);
            let x = full.width.saturating_sub(r.len() as u16);
            buf.set_string(x, y, r, style);
        }
    }
}

fn rank(a: Attention) -> u8 {
    match a {
        Attention::Idle => 0,
        Attention::Working => 1,
        Attention::Done => 2,
        Attention::NeedsInput => 3,
    }
}

fn glyph(rank: u8) -> (&'static str, Color) {
    match rank {
        3 => ("●", Color::Yellow),
        2 => ("✓", Color::Green),
        1 => ("◌", Color::Cyan),
        _ => (" ", Color::Reset),
    }
}

/// `CSI A`..`CSI D`, `CSI H`, `CSI F` without parameters become SS3.
fn ckm(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(b"\x1b[") && i + 2 < data.len() && b"ABCDHF".contains(&data[i + 2]) {
            out.extend_from_slice(&[0x1b, b'O', data[i + 2]]);
            i += 3;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

struct MouseEv {
    b: u16,
    x: u16,
    y: u16,
    press: bool,
}

/// `ESC [ < b ; x ; y M|m`, 1-based coordinates.
fn parse_sgr_mouse(d: &[u8]) -> Option<(MouseEv, usize)> {
    let rest = d.strip_prefix(b"\x1b[<")?;
    let end = rest.iter().position(|c| *c == b'M' || *c == b'm')?;
    let body = std::str::from_utf8(&rest[..end]).ok()?;
    let mut it = body.split(';').map(|n| n.parse::<u16>().ok());
    let (b, x, y) = (it.next()??, it.next()??, it.next()??);
    Some((MouseEv { b, x: x.saturating_sub(1), y: y.saturating_sub(1), press: rest[end] == b'M' }, 3 + end + 1))
}

fn default_socket() -> PathBuf {
    if let Some(s) = std::env::var_os("ILLOGICAL_SOCK") {
        return PathBuf::from(s);
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"))
        .join("illogical");
    match std::fs::read_to_string(state.join("sock.path")) {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p.trim()),
        _ => state.join("sock"),
    }
}

fn pct(v: &mut [Duration], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort();
    v[((v.len() - 1) as f64 * p) as usize].as_secs_f64() * 1e3
}

fn main() -> anyhow::Result<()> {
    let sock = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(default_socket);
    let stream = UnixStream::connect(&sock).with_context(|| format!("connect {}", sock.display()))?;
    let (ws, _) = tungstenite::client("ws://localhost/ws", stream).map_err(|e| anyhow::anyhow!("handshake: {e}"))?;

    let stdin = std::io::stdin();
    let saved = termios::tcgetattr(stdin.as_fd()).context("not a terminal")?;
    let mut raw = saved.clone();
    termios::cfmakeraw(&mut raw);
    termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &raw)?;
    struct Restore(termios::Termios);
    impl Drop for Restore {
        fn drop(&mut self) {
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[0 q\x1b[?25h\x1b[?1049l");
            let _ = out.flush();
            let _ = termios::tcsetattr(std::io::stdin().as_fd(), SetArg::TCSANOW, &self.0);
        }
    }
    let _restore = Restore(saved);
    {
        let mut out = std::io::stdout();
        out.write_all(b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1006h")?;
        out.flush()?;
    }
    let mut term = ratatui::Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    term.clear()?;

    let mut app = App {
        ws,
        state: None,
        tab: None,
        panes: HashMap::new(),
        focus: None,
        area: Rect::default(),
        prefix: false,
        drag: None,
        hits: vec![],
        follow_new: false,
        seen_panes: HashSet::new(),
        seen_tabs: HashSet::new(),
        status: String::new(),
        stats: Stats { started: Some(Instant::now()), ..Default::default() },
        dirty: true,
        quit: false,
    };
    // The pane area before the first frame, so the first View is right.
    let size = term.size()?;
    app.area = Rect { x: SIDEBAR + 1, y: 0, width: size.width.saturating_sub(SIDEBAR + 1), height: size.height - 1 };
    app.ws.get_mut().set_nonblocking(true)?;

    let mut last_draw = Instant::now() - FRAME;
    let mut buf = vec![0u8; 64 * 1024];
    while !app.quit {
        let wait = if app.dirty { FRAME.saturating_sub(last_draw.elapsed()) } else { Duration::from_millis(250) };
        let (sock_ready, stdin_ready) = {
            let mut fds = [
                PollFd::new(app.ws.get_ref().as_fd(), PollFlags::POLLIN),
                PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
            ];
            match poll(&mut fds, PollTimeout::try_from(wait.as_millis() as u64).unwrap_or(PollTimeout::MAX)) {
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => {}
                Err(e) => return Err(e.into()),
            }
            let r = |i: usize| fds[i].revents().is_some_and(|e| !e.is_empty());
            (r(0), r(1))
        };
        if sock_ready {
            loop {
                match app.ws.read() {
                    Ok(Message::Binary(b)) => app.on_frame(&b),
                    Ok(Message::Text(t)) => app.on_text(&t),
                    Ok(Message::Close(_)) => app.quit = true,
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
                if app.quit {
                    break;
                }
            }
        }
        if stdin_ready {
            let n = std::io::stdin().read(&mut buf)?;
            if n == 0 {
                break;
            }
            app.on_stdin(&buf[..n]);
        }
        if app.dirty && last_draw.elapsed() >= FRAME {
            let t0 = Instant::now();
            term.draw(|f| app.draw(f))?;
            app.stats.draw.push(t0.elapsed());
            last_draw = Instant::now();
            app.dirty = false;
        }
        match app.ws.flush() {
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
            r => r?,
        }
    }

    if let Some(path) = std::env::var_os("S19_STATS") {
        let s = &mut app.stats;
        let secs = s.started.map_or(0.0, |t| t.elapsed().as_secs_f64());
        let report = format!(
            "frames {}\nseconds {secs:.1}\nbytes_in {}\nsnapshot_bytes {}\nsnapshot_raw {}\nresyncs {}\nbuild_ms p50 {:.3} p90 {:.3} p99 {:.3} max {:.3}\ndraw_ms p50 {:.3} p90 {:.3} p99 {:.3} max {:.3}\n",
            s.build.len(),
            s.bytes,
            s.snapshot_bytes,
            s.snapshot_raw,
            s.resyncs,
            pct(&mut s.build, 0.5),
            pct(&mut s.build, 0.9),
            pct(&mut s.build, 0.99),
            pct(&mut s.build, 1.0),
            pct(&mut s.draw, 0.5),
            pct(&mut s.draw, 0.9),
            pct(&mut s.draw, 0.99),
            pct(&mut s.draw, 1.0),
        );
        std::fs::write(path, report)?;
    }
    Ok(())
}
