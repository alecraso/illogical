//! tmux commands, each answered from the front end's state or turned into
//! intents and other messages for the daemon.
//!
//! Commands that iTerm2 sends with errors not tolerated (`list-keys`,
//! `show @iterm2_id`, `resize-pane`, `select-layout`, ...) never fail: an
//! `%error` there makes it disconnect with an alert.

use illogical_core::layout::Layout;
use illogical_proto::{
    BlockType, ClientMsg, Dir, Edge, Intent, Node, NodeId, OptionScope, PaneId, PaneOp, SessionId, TabId,
};
use illogical_vt::{CaptureOpts, Line, VtEngine};

use super::{
    front::{Front, escape},
    layout::{self, Cell},
    parse::{self, Cmd},
};

type Reply = Result<Vec<u8>, String>;

/// What a `-t` names.
#[derive(Debug, Clone, Copy)]
struct Tgt {
    session: SessionId,
    tab: Option<TabId>,
    pane: Option<PaneId>,
}

fn lines(v: impl IntoIterator<Item = String>) -> Vec<u8> {
    let mut out = Vec::new();
    for l in v {
        out.extend_from_slice(l.as_bytes());
        out.push(b'\n');
    }
    out
}

fn intent(intent: Intent) -> ClientMsg {
    ClientMsg::Intent { id: None, intent }
}

/// tmux's quoting for `show-options` values.
fn quoted(v: &str) -> String {
    if v.is_empty() || v.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '#') {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        v.to_owned()
    }
}

/// `N` or `N%` of `total`.
fn cells(spec: &str, total: u16) -> Option<u16> {
    match spec.strip_suffix('%') {
        Some(p) => p.parse::<u32>().ok().map(|p| (total as u32 * p / 100) as u16),
        None => spec.parse().ok(),
    }
}

impl Front {
    /// Run one command: the reply's body, or an error.
    pub fn exec(&mut self, c: &Cmd) -> Reply {
        match c.name {
            "attach-session" | "switch-client" => {
                let t = self.resolve(c.get('t'))?;
                if t.session != self.session {
                    self.switch_session(t.session).map_err(|e| e.to_string())?;
                }
                Ok(vec![])
            }
            "new-session" => self.new_session(c),
            "has-session" => self.resolve(c.get('t')).map(|_| vec![]),
            "kill-session" => {
                let t = self.resolve(c.get('t'))?;
                self.sync(vec![intent(Intent::CloseSession { session: t.session })])?;
                Ok(vec![])
            }
            "kill-server" => Err("kill-server isn't supported: close sessions instead".into()),
            "rename-session" => {
                let t = self.resolve(c.get('t'))?;
                let name = c.args.first().cloned().unwrap_or_default();
                self.sync(vec![intent(Intent::RenameSession { session: t.session, name })])?;
                Ok(vec![])
            }
            "rename-window" => {
                let t = self.resolve(c.get('t'))?;
                let tab = t.tab.ok_or("no window")?;
                let name = c.args.first().cloned().filter(|n| !n.is_empty());
                self.sync(vec![intent(Intent::RenameTab { tab, name })])?;
                Ok(vec![])
            }
            "detach-client" => {
                self.exit.get_or_insert_with(String::new);
                Ok(vec![])
            }
            "list-sessions" => {
                let fmt = c
                    .get('F')
                    .unwrap_or("#{session_name}: #{session_windows} windows#{?session_attached, (attached),}");
                let ids: Vec<SessionId> = self.state.sessions.iter().map(|s| s.id).collect();
                Ok(lines(ids.into_iter().map(|s| self.expand_in(fmt, Some(s), None, None))))
            }
            "list-windows" => {
                let fmt = c.get('F').unwrap_or(
                    "#{window_index}: #{window_name}#{window_flags} (#{window_panes} panes) [#{window_width}x#{window_height}]",
                );
                let sessions: Vec<SessionId> = if c.has('a') {
                    self.state.sessions.iter().map(|s| s.id).collect()
                } else {
                    vec![self.resolve(c.get('t'))?.session]
                };
                let mut out = vec![];
                for s in sessions {
                    for t in self.session_tabs(s) {
                        out.push(self.expand_in(fmt, Some(s), Some(t), None));
                    }
                }
                Ok(lines(out))
            }
            "list-panes" => self.list_panes(c),
            "list-clients" => {
                let fmt = c.get('F').unwrap_or(
                    "#{client_name}: #{session_name} [#{client_width}x#{client_height} #{client_termname}] (utf8)",
                );
                Ok(lines([self.expand_in(fmt, None, None, None)]))
            }
            "list-commands" => {
                Ok(lines(parse::names().map(|(n, a)| if a.is_empty() { n.to_owned() } else { format!("{n} ({a})") })))
            }
            "list-keys" | "list-buffers" | "copy-mode" | "choose-tree" | "display-panes" | "run-shell"
            | "source-file" | "wait-for" | "set-environment" | "show-environment" | "set-hook" | "server-info"
            | "show-messages" | "delete-buffer" | "set-buffer" => Ok(vec![]),
            "show-buffer" => Err("no buffers".into()),
            "link-window" | "swap-window" => Err(format!("{} isn't supported", c.name)),
            "clear-history" => {
                let t = self.resolve(c.get('t'))?;
                let pane = t.pane.ok_or("no pane")?;
                self.sync(vec![ClientMsg::Pane { pane, op: PaneOp::Purge }])?;
                Ok(vec![])
            }
            "display-message" => {
                let t = self.resolve(c.get('t'))?;
                let fmt = c.get('F').map(str::to_owned).unwrap_or_else(|| c.args.join(" "));
                self.ensure_mirrors(&fmt, t.pane.into_iter().collect());
                Ok(lines([self.expand_in(&fmt, Some(t.session), t.tab, t.pane)]))
            }
            "show-options" | "show-window-options" => self.show_options(c),
            "set-option" | "set-window-option" => self.set_option(c),
            "capture-pane" => self.capture(c),
            "refresh-client" => self.refresh_client(c),
            "resize-window" => {
                let t = self.resolve(c.get('t'))?;
                let tab = t.tab.ok_or("no window")?;
                let cur = self.tab(tab).map(|v| (v.cols, v.rows)).unwrap_or((80, 24));
                let cols = c.get('x').and_then(|x| x.parse().ok()).unwrap_or(cur.0);
                let rows = c.get('y').and_then(|y| y.parse().ok()).unwrap_or(cur.1);
                self.sizes.insert(tab, (cols, rows));
                self.view(tab, true)?;
                Ok(vec![])
            }
            "split-window" => self.split_window(c),
            "new-window" => self.new_window(c),
            "kill-pane" => {
                let t = self.resolve(c.get('t'))?;
                let pane = t.pane.ok_or("no pane")?;
                let panes: Vec<PaneId> = if c.has('a') {
                    t.tab.and_then(|tab| self.tab(tab)).map(|v| v.root.panes()).unwrap_or_default()
                } else {
                    vec![pane]
                };
                let msgs = panes
                    .into_iter()
                    .filter(|p| !c.has('a') || *p != pane)
                    .map(|p| intent(Intent::ClosePane { pane: p }))
                    .collect();
                self.sync(msgs)?;
                Ok(vec![])
            }
            "kill-window" | "unlink-window" => {
                let t = self.resolve(c.get('t'))?;
                let tab = t.tab.ok_or("no window")?;
                let tabs: Vec<TabId> = if c.has('a') {
                    self.session_tabs(t.session).into_iter().filter(|x| *x != tab).collect()
                } else {
                    vec![tab]
                };
                self.sync(tabs.into_iter().map(|tab| intent(Intent::CloseTab { tab })).collect())?;
                Ok(vec![])
            }
            "resize-pane" => {
                // Never an error: iTerm2 disconnects on one.
                let _ = self.resize_pane(c);
                Ok(vec![])
            }
            "select-layout" => {
                let _ = self.select_layout(c);
                Ok(vec![])
            }
            "send-keys" => self.send_keys(c),
            "select-pane" | "last-pane" => self.select_pane(c),
            "select-window" | "last-window" | "next-window" | "previous-window" => self.select_window(c),
            "swap-pane" => {
                let t = self.resolve(c.get('t'))?;
                let target = t.pane.ok_or("no pane")?;
                let pane = match c.get('s') {
                    Some(s) => self.resolve(Some(s))?.pane.ok_or("no pane")?,
                    None => {
                        // -U / -D: the previous or next pane in the window.
                        let panes = t.tab.and_then(|x| self.tab(x)).map(|v| v.root.panes()).unwrap_or_default();
                        let i = panes.iter().position(|p| *p == target).unwrap_or(0);
                        let j = if c.has('U') { i + panes.len() - 1 } else { i + 1 } % panes.len().max(1);
                        panes.get(j).copied().unwrap_or(target)
                    }
                };
                if pane != target {
                    self.sync(vec![intent(Intent::MovePane { pane, target, edge: Edge::Center })])?;
                }
                Ok(vec![])
            }
            "move-pane" | "join-pane" => {
                let pane = self.resolve(c.get('s'))?.pane.ok_or("no pane")?;
                let target = self.resolve(c.get('t'))?.pane.ok_or("no pane")?;
                let edge = match (c.has('h'), c.has('b')) {
                    (true, false) => Edge::Right,
                    (true, true) => Edge::Left,
                    (false, false) => Edge::Bottom,
                    (false, true) => Edge::Top,
                };
                self.sync(vec![intent(Intent::MovePane { pane, target, edge })])?;
                Ok(vec![])
            }
            "break-pane" => {
                let src = self.resolve(c.get('s'))?;
                let pane = src.pane.ok_or("no pane")?;
                let session = match c.get('t') {
                    Some(t) => self.resolve(Some(t))?.session,
                    None => src.session,
                };
                let before = self.session_tabs(session);
                self.sync(vec![intent(Intent::BreakPane { pane, session, index: None })])?;
                let tab = self.session_tabs(session).into_iter().find(|t| !before.contains(t));
                if let (Some(tab), false) = (tab, c.has('d')) {
                    self.set_active_tab(session, tab);
                }
                if let Some(n) = c.get('n')
                    && let Some(tab) = tab
                {
                    self.sync(vec![intent(Intent::RenameTab { tab, name: Some(n.to_owned()) })])?;
                }
                if c.has('P') {
                    let fmt = c.get('F').unwrap_or("#{session_name}:#{window_index}.#{pane_index}");
                    return Ok(lines([self.expand_in(fmt, Some(session), tab, Some(pane))]));
                }
                Ok(vec![])
            }
            "move-window" => {
                let src = self.resolve(c.get('s'))?;
                let tab = src.tab.ok_or("no window")?;
                let dst = self.resolve(c.get('t'))?;
                let tabs = self.session_tabs(dst.session);
                let index = dst.tab.and_then(|t| tabs.iter().position(|x| *x == t)).map_or(tabs.len(), |i| i + 1);
                self.sync(vec![intent(Intent::MoveTab { tab, session: dst.session, index })])?;
                Ok(vec![])
            }
            other => Err(format!("unknown command: {other}")),
        }
    }

    // ---- targets

    /// `-t` as tmux reads it: `%pane`, `@window`, `$session`, or
    /// `session:window.pane` with names, indexes, `+` and `-`.
    fn resolve(&self, t: Option<&str>) -> Result<Tgt, String> {
        let here = |session: SessionId| {
            let tab = self.current_tab(session);
            Tgt { session, tab, pane: tab.and_then(|t| self.current_pane(t)) }
        };
        let t = t.unwrap_or("").trim();
        if t.is_empty() {
            return Ok(here(self.session));
        }
        if let Some(p) = t.strip_prefix('%') {
            let pane: PaneId = p.parse().map_err(|_| format!("can't find pane: {t}"))?;
            let tab = self.tab_of(pane).ok_or_else(|| format!("can't find pane: {t}"))?;
            let session = self.session_of(tab).ok_or_else(|| format!("can't find pane: {t}"))?;
            return Ok(Tgt { session, tab: Some(tab), pane: Some(pane) });
        }
        if let Some(w) = t.strip_prefix('@') {
            let tab: TabId = w.parse().map_err(|_| format!("can't find window: {t}"))?;
            let session = self.session_of(tab).ok_or_else(|| format!("can't find window: {t}"))?;
            return Ok(Tgt { session, tab: Some(tab), pane: self.current_pane(tab) });
        }
        let (sess, rest) = match t.split_once(':') {
            Some((s, r)) => (s, Some(r)),
            None => (t, None),
        };
        let session = if sess.is_empty() {
            self.session
        } else if let Some(id) = sess.strip_prefix('$') {
            let id: SessionId = id.parse().map_err(|_| format!("can't find session: {sess}"))?;
            self.state
                .sessions
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.id)
                .ok_or(format!("can't find session: {sess}"))?
        } else if let Some(s) = self.state.sessions.iter().find(|s| s.name == sess) {
            s.id
        } else if let Some(s) = self.state.sessions.iter().find(|s| s.name.starts_with(sess)) {
            s.id
        } else if rest.is_none() {
            // A window name in the current session.
            let tab = self
                .session_tabs(self.session)
                .into_iter()
                .find(|x| self.window_name(*x) == sess)
                .ok_or_else(|| format!("can't find session: {sess}"))?;
            return Ok(Tgt { session: self.session, tab: Some(tab), pane: self.current_pane(tab) });
        } else {
            return Err(format!("can't find session: {sess}"));
        };
        let Some(rest) = rest else { return Ok(here(session)) };
        let (win, pane) = match rest.split_once('.') {
            Some((w, p)) => (w, Some(p)),
            None => (rest, None),
        };
        let tabs = self.session_tabs(session);
        let cur = self.current_tab(session).and_then(|c| tabs.iter().position(|t| *t == c)).unwrap_or(0);
        let tab = match win {
            "" => tabs.get(cur).copied(),
            "+" => tabs.get((cur + 1) % tabs.len().max(1)).copied(),
            "-" => tabs.get((cur + tabs.len().max(1) - 1) % tabs.len().max(1)).copied(),
            w if w.starts_with('@') => w[1..].parse().ok().filter(|t| tabs.contains(t)),
            w => match w.parse::<usize>() {
                Ok(i) => tabs.get(i).copied(),
                Err(_) => tabs.iter().copied().find(|t| self.window_name(*t) == w),
            },
        };
        let tab = tab.ok_or_else(|| format!("can't find window: {win}"))?;
        let pane = match pane {
            None | Some("") => self.current_pane(tab),
            Some(p) if p.starts_with('%') => p[1..].parse().ok(),
            Some(p) => p.parse::<usize>().ok().and_then(|i| self.tab(tab)?.root.panes().get(i).copied()),
        };
        Ok(Tgt { session, tab: Some(tab), pane })
    }

    fn set_active_tab(&mut self, session: SessionId, tab: TabId) {
        if let Some(cur) = self.current_tab(session)
            && cur != tab
        {
            self.last_tab.insert(session, cur);
        }
        self.active_tab.insert(session, tab);
    }

    /// Tell the daemon the size this client wants for `tab` (its own, else
    /// the client's).
    fn view(&mut self, tab: TabId, claim: bool) -> Result<(), String> {
        self.send_view(tab, claim, false)
    }

    /// `typed`: a claim for typing, which the daemon holds off while the
    /// size's owner is still typing (#333).
    fn send_view(&mut self, tab: TabId, claim: bool, typed: bool) -> Result<(), String> {
        let Some((cols, rows)) = self.sizes.get(&tab).copied().or(self.default_size) else { return Ok(()) };
        let zoom = self.zoom.get(&tab).copied().flatten();
        self.sync(vec![ClientMsg::View { tab, cols, rows, zoom, claim, typed }])
    }

    /// Mirrors for the panes a format reads terminal state from.
    fn ensure_mirrors(&mut self, fmt: &str, panes: Vec<PaneId>) {
        let reads = ["cursor_", "alternate_", "scroll_region", "pane_tabs", "_flag", "history_size"];
        if reads.iter().any(|r| fmt.contains(r)) {
            for p in panes {
                self.mirror(p);
            }
        }
    }

    // ---- sessions and windows

    fn new_session(&mut self, c: &Cmd) -> Reply {
        let name = c.get('s').map(str::to_owned);
        let existing = name.as_ref().and_then(|n| self.state.sessions.iter().find(|s| &s.name == n)).map(|s| s.id);
        let session = match (existing, c.has('A')) {
            (Some(s), true) => s,
            (Some(_), false) => return Err(format!("duplicate session: {}", name.unwrap_or_default())),
            (None, _) => {
                let before: Vec<SessionId> = self.state.sessions.iter().map(|s| s.id).collect();
                self.sync(vec![intent(Intent::NewSession { name, from_pane: None })])?;
                let s = self.state.sessions.iter().map(|s| s.id).find(|s| !before.contains(s)).ok_or("no session")?;
                if let Some(n) = c.get('n')
                    && let Some(tab) = self.session_tabs(s).first().copied()
                {
                    self.sync(vec![intent(Intent::RenameTab { tab, name: Some(n.to_owned()) })])?;
                }
                s
            }
        };
        if !c.has('d') && session != self.session {
            self.switch_session(session).map_err(|e| e.to_string())?;
        }
        if c.has('P') {
            let fmt = c.get('F').unwrap_or("#{session_name}:");
            return Ok(lines([self.expand_in(fmt, Some(session), None, None)]));
        }
        Ok(vec![])
    }

    fn new_window(&mut self, c: &Cmd) -> Reply {
        let t = self.resolve(c.get('t'))?;
        let session = t.session;
        let from = self.current_tab(session).and_then(|x| self.current_pane(x));
        let cwd = self.cwd_arg(c, from);
        let before = self.session_tabs(session);
        self.sync(vec![intent(Intent::NewTab { session, from_pane: from, cwd })])?;
        let tab = self.session_tabs(session).into_iter().find(|x| !before.contains(x)).ok_or("no window was made")?;
        let mut more = vec![];
        if c.has('a')
            && let Some(cur) = t.tab
        {
            let index = before.iter().position(|x| *x == cur).map_or(before.len(), |i| i + 1);
            more.push(intent(Intent::MoveTab { tab, session, index }));
        }
        if let Some(n) = c.get('n') {
            more.push(intent(Intent::RenameTab { tab, name: Some(n.to_owned()) }));
        }
        // It starts at the client's size, as a new tmux window would.
        if let Some((cols, rows)) = self.default_size {
            more.push(ClientMsg::View { tab, cols, rows, zoom: None, claim: false, typed: false });
        }
        if !more.is_empty() {
            self.sync(more)?;
        }
        if !c.has('d') {
            self.set_active_tab(session, tab);
        }
        if c.has('P') {
            let fmt = c.get('F').unwrap_or("#{session_name}:#{window_index}");
            return Ok(lines([self.expand_in(fmt, Some(session), Some(tab), None)]));
        }
        Ok(vec![])
    }

    /// `-c DIR` (a format, e.g. `#{pane_current_path}`), if it names one.
    fn cwd_arg(&self, c: &Cmd, pane: Option<PaneId>) -> Option<String> {
        let dir = self.expand_in(c.get('c')?, None, None, pane);
        let same = pane.and_then(|p| self.info(p)).and_then(|i| i.cwd.clone()).as_deref() == Some(dir.as_str());
        (!dir.is_empty() && !same).then_some(dir)
    }

    fn split_window(&mut self, c: &Cmd) -> Reply {
        let t = self.resolve(c.get('t'))?;
        let pane = t.pane.ok_or("no pane")?;
        let tab = t.tab.ok_or("no window")?;
        let edge = match (c.has('h'), c.has('b')) {
            (true, false) => Edge::Right,
            (true, true) => Edge::Left,
            (false, false) => Edge::Bottom,
            (false, true) => Edge::Top,
        };
        let cwd = self.cwd_arg(c, Some(pane));
        let before = self.tab(tab).map(|v| v.root.panes()).unwrap_or_default();
        self.sync(vec![intent(Intent::Split { pane, edge, local: false, cwd })])?;
        let new = self
            .tab(tab)
            .and_then(|v| v.root.panes().into_iter().find(|p| !before.contains(p)))
            .ok_or("no pane was made")?;
        // -l N / -p N: the new pane's size.
        let dir = if c.has('h') { Dir::Row } else { Dir::Column };
        let total = self.tab(tab).map(|v| if dir == Dir::Row { v.cols } else { v.rows }).unwrap_or(0);
        let size = match (c.get('l'), c.get('p')) {
            (Some(l), _) => cells(l, total),
            (None, Some(p)) => cells(&format!("{p}%"), total),
            _ => None,
        };
        if let Some(size) = size {
            let _ = self.set_extent(tab, new, dir, size);
        }
        if !c.has('d') {
            self.active_pane.insert(tab, new);
        }
        if c.has('P') {
            let fmt = c.get('F').unwrap_or("#{session_name}:#{window_index}.#{pane_index}");
            return Ok(lines([self.expand_in(fmt, Some(t.session), Some(tab), Some(new))]));
        }
        Ok(vec![])
    }

    fn list_panes(&mut self, c: &Cmd) -> Reply {
        let fmt = c
            .get('F')
            .unwrap_or("#{pane_index}: [#{pane_width}x#{pane_height}] #{pane_id}#{?pane_active, (active),}")
            .to_owned();
        let mut which: Vec<(SessionId, TabId)> = vec![];
        if c.has('a') {
            for s in &self.state.sessions {
                which.extend(s.tabs.iter().map(|t| (s.id, *t)));
            }
        } else {
            let t = self.resolve(c.get('t'))?;
            if c.has('s') {
                which.extend(self.session_tabs(t.session).into_iter().map(|x| (t.session, x)));
            } else if let Some(tab) = t.tab {
                which.push((t.session, tab));
            }
        }
        let mut targets = vec![];
        for (s, tab) in which {
            for p in self.tab(tab).map(|v| v.root.panes()).unwrap_or_default() {
                targets.push((s, tab, p));
            }
        }
        self.ensure_mirrors(&fmt, targets.iter().map(|t| t.2).collect());
        Ok(lines(targets.into_iter().map(|(s, tab, p)| self.expand_in(&fmt, Some(s), Some(tab), Some(p)))))
    }

    // ---- options

    /// Which store an `@` option goes in, by the command's flags and target.
    fn option_scope(&self, c: &Cmd) -> Result<OptionScope, String> {
        let window = c.name.contains("window") || c.has('w');
        if c.has('g') || c.has('s') {
            return Ok(OptionScope::Global);
        }
        let t = self.resolve(c.get('t'))?;
        Ok(if c.has('p') {
            OptionScope::Pane(t.pane.ok_or("no pane")?)
        } else if window {
            OptionScope::Tab(t.tab.ok_or("no window")?)
        } else {
            OptionScope::Session(t.session)
        })
    }

    fn show_options(&mut self, c: &Cmd) -> Reply {
        let values = c.has('v');
        let show =
            |name: &str, value: &str| if values { value.to_owned() } else { format!("{name} {}", quoted(value)) };
        let Some(name) = c.args.first() else {
            // Everything set in that scope.
            let scope = self.option_scope(c).unwrap_or(OptionScope::Global);
            let map = self.state.options.get(scope).cloned().unwrap_or_default();
            return Ok(lines(map.iter().map(|(k, v)| show(k, v))));
        };
        if name.starts_with('@') {
            let scope = self.option_scope(c).unwrap_or(OptionScope::Global);
            let value = self.state.options.get(scope).and_then(|m| m.get(name)).cloned();
            // Unset: empty, never an error (iTerm2 asks with errors fatal).
            return Ok(value.map(|v| lines([show(name, &v)])).unwrap_or_default());
        }
        let window_level = (c.name == "show-window-options" || c.has('w')) && !c.has('g');
        match Front::builtin_option(name) {
            Some(_) if window_level => Ok(vec![]),
            Some(v) => Ok(lines([show(name, v)])),
            None => Ok(vec![]),
        }
    }

    fn set_option(&mut self, c: &Cmd) -> Reply {
        let Some(name) = c.args.first().cloned() else { return Err("no option name".into()) };
        if !name.starts_with('@') {
            // Built-in options are ours to decide; accept and ignore.
            return Ok(vec![]);
        }
        let scope = self.option_scope(c)?;
        let current = self.state.options.get(scope).and_then(|m| m.get(&name)).cloned();
        if c.has('o') && current.is_some() {
            return Ok(vec![]);
        }
        let value = if c.has('u') || c.has('U') {
            None
        } else {
            let mut v = c.args.get(1).cloned().unwrap_or_default();
            if c.has('F') {
                let t = self.resolve(c.get('t')).ok();
                v = self.expand_in(&v, t.map(|t| t.session), t.and_then(|t| t.tab), t.and_then(|t| t.pane));
            }
            if c.has('a') {
                v = format!("{}{v}", current.unwrap_or_default());
            }
            Some(v)
        };
        self.sync(vec![intent(Intent::SetOption { scope, name, value })])?;
        Ok(vec![])
    }

    // ---- panes' content

    fn capture(&mut self, c: &Cmd) -> Reply {
        let t = self.resolve(c.get('t'))?;
        let pane = t.pane.ok_or("no pane")?;
        if c.has('P') {
            // Nothing is ever left half-sent: the mirror gets whole frames.
            return Ok(b"\n".to_vec());
        }
        let line = |s: Option<&str>| match s {
            None => None,
            Some("-") => Some(Line::Edge),
            Some(n) => n.parse().ok().map(Line::At),
        };
        let opts = CaptureOpts {
            other: c.has('a'),
            start: line(c.get('S')).unwrap_or(Line::At(0)),
            end: line(c.get('E')).unwrap_or(Line::Edge),
            escapes: c.has('e'),
            join: c.has('J'),
            spaces: c.has('N'),
        };
        let Some(mirror) = self.mirror(pane) else { return Ok(vec![]) };
        let Some(rows) = mirror.capture(&opts) else {
            return if c.has('q') { Ok(b"\n".to_vec()) } else { Err("no alternate screen".into()) };
        };
        if !c.has('p') {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for l in rows {
            if c.has('C') {
                escape(&l, &mut out);
            } else {
                out.extend_from_slice(&l);
            }
            out.push(b'\n');
        }
        Ok(out)
    }

    fn send_keys(&mut self, c: &Cmd) -> Reply {
        let t = self.resolve(c.get('t'))?;
        let pane = t.pane.ok_or("no pane")?;
        if c.has('X') || c.has('R') {
            // Copy-mode commands and resets: nothing to do here.
            return Ok(vec![]);
        }
        let modes = match self.panes.get(&pane).and_then(|p| p.mirror.as_ref()) {
            Some(m) => illogical_proto::keys::Modes {
                app_cursor: m.dec_mode(1),
                mouse: [1000, 1002, 1003].iter().any(|x| m.dec_mode(*x)),
                sgr_mouse: m.dec_mode(1006),
            },
            None => Default::default(),
        };
        let mut data = Vec::new();
        let utf8 = |cp: u32, data: &mut Vec<u8>| {
            if let Some(ch) = char::from_u32(cp) {
                let mut b = [0u8; 4];
                data.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
            }
        };
        for a in &c.args {
            if c.has('H') {
                // Bytes to tmux and WezTerm; Ghostty's branch sends code
                // points, so values past a byte are encoded as UTF-8.
                let Ok(v) = u32::from_str_radix(a.trim_start_matches("0x"), 16) else { continue };
                if v <= 0xff { data.push(v as u8) } else { utf8(v, &mut data) }
            } else if c.has('l') {
                data.extend_from_slice(a.as_bytes());
            } else if let Some(hex) = a.strip_prefix("0x")
                && let Ok(v) = u32::from_str_radix(hex, 16)
            {
                if v < 0x80 { data.push(v as u8) } else { utf8(v, &mut data) }
            } else {
                data.extend(illogical_proto::keys::key(a, modes));
            }
        }
        let n = c.get('N').and_then(|n| n.parse::<usize>().ok()).unwrap_or(1).max(1);
        let data = data.repeat(n);
        let is_terminal = self.panes.get(&pane).is_none_or(|p| p.kind == BlockType::Terminal);
        if data.is_empty() || !is_terminal {
            return Ok(vec![]);
        }
        // Typing claims the window's size, as it does in the web client
        // (once whoever has it stops typing, #333).
        if let Some(tab) = t.tab
            && self.tab(tab).is_some_and(|v| v.owner != Some(self.client))
        {
            let _ = self.send_view(tab, true, true);
        }
        self.conn.input(pane, data).map_err(|e| e.to_string())?;
        Ok(vec![])
    }

    fn select_pane(&mut self, c: &Cmd) -> Reply {
        let t = self.resolve(c.get('t'))?;
        let (Some(tab), Some(mut pane)) = (t.tab, t.pane) else { return Err("no pane".into()) };
        let dir = ['L', 'R', 'U', 'D'].into_iter().find(|d| c.has(*d));
        if let (Some(d), Some(v)) = (dir, self.tab(tab)) {
            pane = neighbour(&v.layout, pane, d).unwrap_or(pane);
        }
        if c.name == "select-pane" && (c.has('T') || c.has('P') || c.has('M') || c.has('m')) && dir.is_none() {
            // A title, style or mark: nothing illogical keeps.
            return Ok(vec![]);
        }
        self.set_active_tab(t.session, tab);
        self.active_pane.insert(tab, pane);
        self.sync(vec![ClientMsg::Focus { pane: Some(pane) }])?;
        Ok(vec![])
    }

    fn select_window(&mut self, c: &Cmd) -> Reply {
        let t = self.resolve(c.get('t'))?;
        let s = t.session;
        let tabs = self.session_tabs(s);
        let cur = self.current_tab(s).and_then(|x| tabs.iter().position(|y| *y == x)).unwrap_or(0);
        let n = tabs.len().max(1);
        let tab = if c.name == "last-window" || c.has('l') {
            self.last_tab.get(&s).copied().filter(|x| tabs.contains(x))
        } else if c.name == "next-window" || c.has('n') {
            tabs.get((cur + 1) % n).copied()
        } else if c.name == "previous-window" || c.has('p') {
            tabs.get((cur + n - 1) % n).copied()
        } else {
            t.tab
        };
        let tab = tab.ok_or("no window")?;
        self.set_active_tab(s, tab);
        Ok(vec![])
    }

    // ---- sizes and layouts

    fn refresh_client(&mut self, c: &Cmd) -> Reply {
        if let Some(size) = c.get('C') {
            if size.starts_with('@') {
                for item in size.split(',') {
                    let Some((w, dims)) = item[1..].split_once(':') else { continue };
                    let Ok(tab) = w.parse::<TabId>() else { continue };
                    match parse_size(dims) {
                        Some(sz) => {
                            self.sizes.insert(tab, sz);
                            let _ = self.view(tab, true);
                        }
                        None => _ = self.sizes.remove(&tab),
                    }
                }
            } else if let Some(sz) = parse_size(size) {
                self.default_size = Some(sz);
                let views: Vec<ClientMsg> = self
                    .session_tabs(self.session)
                    .into_iter()
                    .map(|tab| {
                        let (cols, rows) = self.sizes.get(&tab).copied().unwrap_or(sz);
                        let zoom = self.zoom.get(&tab).copied().flatten();
                        ClientMsg::View { tab, cols, rows, zoom, claim: false, typed: false }
                    })
                    .collect();
                self.sync(views)?;
            }
        }
        if let Some(flags) = c.get('f').or(c.get('F')) {
            for f in flags.split(',') {
                let (off, f) = match f.strip_prefix('!') {
                    Some(f) => (true, f),
                    None => (false, f),
                };
                let (name, value) = f.split_once('=').unwrap_or((f, ""));
                match name {
                    "pause-after" => self.flags.pause_after = (!off).then(|| value.parse().unwrap_or(0)),
                    "wait-exit" => self.flags.wait_exit = !off,
                    "no-output" => self.flags.no_output = !off,
                    _ => {}
                }
            }
        }
        if let Some(a) = c.get('A') {
            for item in a.split(',') {
                let Some((p, what)) = item.split_once(':') else { continue };
                let Ok(pane) = p.trim_start_matches('%').parse::<PaneId>() else { continue };
                let Some(pv) = self.panes.get_mut(&pane) else { continue };
                match what {
                    "pause" | "off" if pv.live == super::front::Live::On => {
                        pv.live = super::front::Live::Paused;
                        self.note(format!("%pause %{pane}\n").as_bytes());
                    }
                    "continue" | "on" if pv.live != super::front::Live::On => {
                        pv.live = super::front::Live::On;
                        self.note(format!("%continue %{pane}\n").as_bytes());
                    }
                    _ => {}
                }
            }
        }
        if let Some(b) = c.get('B') {
            let mut it = b.splitn(3, ':');
            let (name, _target, fmt) = (it.next().unwrap_or(""), it.next(), it.next());
            match fmt {
                Some(fmt) if !name.is_empty() => {
                    self.subscriptions.insert(name.to_owned(), (fmt.to_owned(), None));
                }
                _ => _ = self.subscriptions.remove(name),
            }
        }
        Ok(vec![])
    }

    fn resize_pane(&mut self, c: &Cmd) -> Result<(), String> {
        let t = self.resolve(c.get('t'))?;
        let (Some(tab), Some(pane)) = (t.tab, t.pane) else { return Err("no pane".into()) };
        let (cols, rows) = self.tab(tab).map(|v| (v.cols, v.rows)).ok_or("no window")?;
        if c.has('Z') {
            let zoomed = self.tab(tab).is_some_and(|v| v.zoom.is_some());
            self.zoom.insert(tab, (!zoomed).then_some(pane));
            self.sizes.entry(tab).or_insert((cols, rows));
            return self.view(tab, true);
        }
        if let Some(x) = c.get('x').and_then(|x| cells(x, cols)) {
            self.set_extent(tab, pane, Dir::Row, x)?;
        }
        if let Some(y) = c.get('y').and_then(|y| cells(y, rows)) {
            self.set_extent(tab, pane, Dir::Column, y)?;
        }
        let n: i32 = c.args.first().and_then(|a| a.parse().ok()).unwrap_or(1);
        for (flag, dir, sign) in
            [('L', Dir::Row, -1), ('R', Dir::Row, 1), ('U', Dir::Column, -1), ('D', Dir::Column, 1)]
        {
            if c.has(flag) {
                self.move_border(tab, pane, dir, sign * n)?;
            }
        }
        Ok(())
    }

    /// The split along `dir` nearest `pane`, the child holding it, the
    /// children's extents and minimums.
    fn split_for(&self, tab: TabId, pane: PaneId, dir: Dir) -> Option<(NodeId, usize, Vec<i32>, Vec<i32>)> {
        let v = self.tab(tab)?;
        let (id, i, children) = deepest(&v.root, pane, dir)?;
        let ext: Vec<i32> = v.layout.splits.iter().find(|s| s.id == id)?.extents.iter().map(|e| *e as i32).collect();
        let mins = children
            .iter()
            .map(|n| {
                let (c, r) = n.min_size();
                (if dir == Dir::Row { c } else { r }) as i32
            })
            .collect();
        Some((id, i, ext, mins))
    }

    /// Move the border beside `pane` by `delta` cells (right or down when
    /// positive), as a divider drag does.
    fn move_border(&mut self, tab: TabId, pane: PaneId, dir: Dir, delta: i32) -> Result<(), String> {
        let (split, i, mut ext, mins) = self.split_for(tab, pane, dir).ok_or("no split there")?;
        let n = ext.len();
        // The border after the pane's child, or before it for the last one
        // (and before it when moving back, unless it is the first).
        let b = if delta > 0 {
            if i + 1 < n { i + 1 } else { i }
        } else if i > 0 {
            i
        } else {
            1
        };
        let delta = delta.clamp(mins[b - 1] - ext[b - 1], ext[b] - mins[b]);
        ext[b - 1] += delta;
        ext[b] -= delta;
        self.resize_split(split, &ext)
    }

    /// Make `pane`'s child of the nearest split along `dir` `size` cells.
    fn set_extent(&mut self, tab: TabId, pane: PaneId, dir: Dir, size: u16) -> Result<(), String> {
        let (split, i, mut ext, mins) = self.split_for(tab, pane, dir).ok_or("no split there")?;
        let want = size as i32 - ext[i];
        let (b, delta) = if i + 1 < ext.len() { (i + 1, want) } else { (i, -want) };
        let delta = delta.clamp(mins[b - 1] - ext[b - 1], ext[b] - mins[b]);
        ext[b - 1] += delta;
        ext[b] -= delta;
        self.resize_split(split, &ext)
    }

    fn resize_split(&mut self, split: NodeId, ext: &[i32]) -> Result<(), String> {
        let total: i32 = ext.iter().sum();
        if total <= 0 {
            return Ok(());
        }
        let weights = ext.iter().map(|e| *e as f64 / total as f64).collect();
        self.sync(vec![intent(Intent::ResizeSplit { split, weights })])
    }

    /// A layout string with the same shape as the window's tree becomes its
    /// splits' weights; anything else can't be expressed and the window's
    /// layout goes out again so the client snaps back.
    fn select_layout(&mut self, c: &Cmd) -> Result<(), String> {
        let t = self.resolve(c.get('t'))?;
        let tab = t.tab.ok_or("no window")?;
        self.force_layout.insert(tab);
        let root = self.tab(tab).map(|v| v.root.clone()).ok_or("no window")?;
        let arg = c.args.first().map(String::as_str).unwrap_or("");
        let mut msgs = vec![];
        match arg {
            "even-horizontal" | "even-vertical" => {
                let want = if arg == "even-horizontal" { Dir::Row } else { Dir::Column };
                if let Node::Split { id, dir, children } = &root
                    && *dir == want
                    && children.iter().all(|c| matches!(c.node, Node::Pane { .. }))
                {
                    let weights = vec![1.0; children.len()];
                    msgs.push(intent(Intent::ResizeSplit { split: *id, weights }));
                }
            }
            s if s.contains(',') => {
                let cell = layout::parse(s)?;
                same_shape(&root, &cell, &mut msgs).then_some(()).ok_or("layout shape differs")?;
            }
            _ => {}
        }
        if !msgs.is_empty() {
            self.sync(msgs)?;
        }
        Ok(())
    }
}

/// `W,H` or `WxH`.
fn parse_size(s: &str) -> Option<(u16, u16)> {
    let (w, h) = s.split_once(',').or_else(|| s.split_once('x'))?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The deepest split along `dir` that holds `pane`: its id, the index of
/// the child holding the pane, and its children.
fn deepest(node: &Node, pane: PaneId, dir: Dir) -> Option<(NodeId, usize, Vec<Node>)> {
    let Node::Split { id, dir: d, children } = node else { return None };
    let i = children.iter().position(|c| c.node.contains(pane))?;
    if let Some(found) = deepest(&children[i].node, pane, dir) {
        return Some(found);
    }
    (*d == dir).then(|| (*id, i, children.iter().map(|c| c.node.clone()).collect()))
}

/// Whether `cell` has `node`'s shape (directions, child counts, panes); if
/// so, push one `ResizeSplit` per split with the cell extents as weights.
fn same_shape(node: &Node, cell: &Cell, msgs: &mut Vec<ClientMsg>) -> bool {
    match (node, cell) {
        (Node::Pane { pane }, Cell::Pane { pane: p, .. }) => pane == p,
        (Node::Split { id, dir, children }, Cell::Split { dir: d, children: cells, .. }) => {
            if dir != d || children.len() != cells.len() {
                return false;
            }
            let ext: Vec<f64> =
                cells.iter().map(|c| if *dir == Dir::Row { c.size().0 } else { c.size().1 } as f64).collect();
            let total: f64 = ext.iter().sum();
            msgs.push(intent(Intent::ResizeSplit { split: *id, weights: ext.iter().map(|e| e / total).collect() }));
            children.iter().zip(cells).all(|(n, c)| same_shape(&n.node, c, msgs))
        }
        _ => false,
    }
}

/// The pane next to `pane` on side `d` (L, R, U, D), overlapping it most.
fn neighbour(l: &Layout, pane: PaneId, d: char) -> Option<PaneId> {
    let (_, r) = l.panes.iter().find(|(p, _)| *p == pane)?;
    let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| (a1.min(b1) as i32 - a0.max(b0) as i32).max(0);
    l.panes
        .iter()
        .filter(|(p, _)| *p != pane)
        .filter_map(|(p, o)| {
            let touching = match d {
                'L' => o.x + o.cols + 1 == r.x,
                'R' => r.x + r.cols + 1 == o.x,
                'U' => o.y + o.rows + 1 == r.y,
                _ => r.y + r.rows + 1 == o.y,
            };
            let ov = if matches!(d, 'L' | 'R') {
                overlap(r.y, r.y + r.rows, o.y, o.y + o.rows)
            } else {
                overlap(r.x, r.x + r.cols, o.x, o.x + o.cols)
            };
            (touching && ov > 0).then_some((*p, ov))
        })
        .max_by_key(|(_, ov)| *ov)
        .map(|(p, _)| p)
}
