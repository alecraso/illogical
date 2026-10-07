//! Format variables for a session, window and pane, from the daemon's
//! `State`, the pane mirrors and the client's own state (active window and
//! pane), named as tmux names them.

use arugula_proto::{BlockType, OptionScope, PaneId, SessionId, TabId};
use arugula_vt::VtEngine;

use super::{
    format::{self, Vars},
    front::{Front, HISTORY, VERSION},
};

/// tmux's "no position" for the alternate screen's saved cursor.
const NONE: u32 = u32::MAX;

pub struct Ctx<'a> {
    pub f: &'a Front,
    pub session: SessionId,
    pub tab: Option<TabId>,
    pub pane: Option<PaneId>,
}

impl Front {
    /// Expand `fmt` for a session, window and pane (missing ones are the
    /// current ones: the window's active pane, the session's window).
    pub fn expand_in(&self, fmt: &str, session: Option<SessionId>, tab: Option<TabId>, pane: Option<PaneId>) -> String {
        let tab = tab.or_else(|| pane.and_then(|p| self.tab_of(p)));
        let session = session.or_else(|| tab.and_then(|t| self.session_of(t))).unwrap_or(self.session);
        let tab = tab.or_else(|| self.current_tab(session));
        let pane = pane.or_else(|| tab.and_then(|t| self.current_pane(t)));
        format::expand(fmt, &Ctx { f: self, session, tab, pane })
    }

    /// A built-in option's value, as tmux would answer for this front end.
    pub fn builtin_option(name: &str) -> Option<&'static str> {
        Some(match name {
            "aggressive-resize" => "off",
            "status" => "off",
            "set-titles" => "off",
            "set-titles-string" => "#S:#I:#W - \"#T\" #{session_alerts}",
            "default-terminal" => "xterm-256color",
            "pane-border-status" => "off",
            "pane-border-format" => "",
            "message-style" => "bg=yellow,fg=black",
            "history-limit" => "2000",
            "set-clipboard" => "external",
            "escape-time" => "0",
            "base-index" => "0",
            "pane-base-index" => "0",
            "automatic-rename" => "on",
            "allow-rename" => "off",
            "allow-passthrough" => "off",
            "window-size" => "latest",
            "mouse" => "off",
            "focus-events" => "off",
            "extended-keys" => "off",
            "mode-keys" => "emacs",
            "status-keys" => "emacs",
            "prefix" => "C-b",
            _ => return None,
        })
    }
}

impl Ctx<'_> {
    /// A `@user` option, looked up from the pane outwards.
    fn user_option(&self, name: &str) -> Option<String> {
        let o = &self.f.state.options;
        let scopes = [
            self.pane.map(OptionScope::Pane),
            self.tab.map(OptionScope::Tab),
            Some(OptionScope::Session(self.session)),
            Some(OptionScope::Global),
        ];
        scopes.into_iter().flatten().find_map(|s| o.get(s).and_then(|m| m.get(name)).cloned())
    }

    fn session_var(&self, name: &str) -> Option<String> {
        let f = self.f;
        let s = self.session;
        Some(match name {
            "session_id" => format!("${s}"),
            "session_name" => f.session_name(s),
            "session_windows" => f.session_tabs(s).len().to_string(),
            "session_attached" | "session_many_attached" => ((s == f.session) as u8).to_string(),
            "session_created" | "session_activity" | "session_last_attached" => "0".into(),
            "session_group" | "session_alerts" | "session_path" => String::new(),
            "session_grouped" | "session_marked" => "0".into(),
            "session_width" | "client_width" => f.default_size.map(|s| s.0).unwrap_or(80).to_string(),
            "session_height" | "client_height" => f.default_size.map(|s| s.1).unwrap_or(24).to_string(),
            _ => return None,
        })
    }

    fn window_var(&self, name: &str) -> Option<String> {
        let f = self.f;
        let tab = self.tab?;
        let t = f.tab(tab)?;
        let index = f.session_tabs(self.session).iter().position(|x| *x == tab).unwrap_or(0);
        Some(match name {
            "window_id" => format!("@{tab}"),
            "window_index" => index.to_string(),
            "window_name" => f.window_name(tab),
            "window_width" => t.cols.to_string(),
            "window_height" => t.rows.to_string(),
            "window_layout" => f.layouts(tab).0,
            "window_visible_layout" => f.layouts(tab).1,
            "window_flags" | "window_raw_flags" => f.window_flags(tab),
            "window_active" => ((f.current_tab(self.session) == Some(tab)) as u8).to_string(),
            "window_last_flag" => ((f.last_tab.get(&self.session) == Some(&tab)) as u8).to_string(),
            "window_zoomed_flag" => (t.zoom.is_some() as u8).to_string(),
            "window_panes" => t.root.panes().len().to_string(),
            "window_bigger" | "window_activity_flag" | "window_bell_flag" | "window_silence_flag" => "0".into(),
            "window_linked" | "window_marked_flag" => "0".into(),
            "window_start_flag" => ((index == 0) as u8).to_string(),
            "window_end_flag" => ((index + 1 == f.session_tabs(self.session).len()) as u8).to_string(),
            "window_offset_x" | "window_offset_y" | "window_activity" => String::new(),
            "history_limit" => HISTORY.to_string(),
            _ => return None,
        })
    }

    fn pane_var(&self, name: &str) -> Option<String> {
        let f = self.f;
        let pane = self.pane?;
        let tab = self.tab.or_else(|| f.tab_of(pane))?;
        let t = f.tab(tab)?;
        let rect = t.layout.panes.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r);
        let info = f.info(pane);
        let mirror = f.panes.get(&pane).and_then(|p| p.mirror.as_ref());
        let flag = |on: bool| (on as u8).to_string();
        let dec = |m: u16| flag(mirror.is_some_and(|e| e.dec_mode(m)));
        Some(match name {
            "pane_id" => format!("%{pane}"),
            "pane_index" => t.root.panes().iter().position(|p| *p == pane).unwrap_or(0).to_string(),
            "pane_active" => flag(f.current_pane(tab) == Some(pane)),
            "pane_width" => rect.map(|r| r.cols).unwrap_or(t.cols).to_string(),
            "pane_height" => rect.map(|r| r.rows).unwrap_or(t.rows).to_string(),
            "pane_left" => rect.map(|r| r.x).unwrap_or(0).to_string(),
            "pane_top" => rect.map(|r| r.y).unwrap_or(0).to_string(),
            "pane_right" => rect.map(|r| (r.x + r.cols).saturating_sub(1)).unwrap_or(t.cols - 1).to_string(),
            "pane_bottom" => rect.map(|r| (r.y + r.rows).saturating_sub(1)).unwrap_or(t.rows - 1).to_string(),
            "pane_at_left" => flag(rect.is_none_or(|r| r.x == 0)),
            "pane_at_top" => flag(rect.is_none_or(|r| r.y == 0)),
            "pane_at_right" => flag(rect.is_none_or(|r| r.x + r.cols >= t.cols)),
            "pane_at_bottom" => flag(rect.is_none_or(|r| r.y + r.rows >= t.rows)),
            "pane_current_path" => info.and_then(|i| i.cwd.clone()).unwrap_or_default(),
            "pane_current_command" => match info {
                Some(i) if i.kind != BlockType::Terminal => format!("{:?}", i.kind).to_lowercase(),
                Some(i) => i.command.clone().unwrap_or_else(super::front::shell_name),
                None => String::new(),
            },
            "pane_start_command" | "pane_tty" | "pane_pid" | "pane_title" | "pane_mode" | "pane_path" => String::new(),
            "pane_dead" => flag(info.is_some_and(|i| !i.running)),
            "pane_dead_status" => info.and_then(|i| i.last.as_ref()?.exit).map(|c| c.to_string()).unwrap_or_default(),
            "pane_in_mode" | "pane_synchronized" | "pane_marked" | "pane_marked_set" | "pane_pipe" => "0".into(),
            "pane_input_off" | "pane_unseen_changes" | "pane_last" => "0".into(),
            "pane_key_mode" => "VT10x".into(),
            // What tmux says until a program sets them.
            "cursor_shape" => "default".into(),
            "cursor_colour" => "none".into(),
            "cursor_blinking" => "0".into(),
            "pane_tabs" => mirror
                .map(|e| e.tab_stops().iter().map(u16::to_string).collect::<Vec<_>>().join(","))
                .unwrap_or_default(),
            "history_size" => mirror.map(|e| e.history_lines()).unwrap_or(0).to_string(),
            "history_bytes" => "0".into(),
            "cursor_x" => mirror.map(|e| e.cursor().0).unwrap_or(0).to_string(),
            "cursor_y" => mirror.map(|e| e.cursor().1).unwrap_or(0).to_string(),
            "alternate_on" => flag(mirror.is_some_and(|e| e.alt_screen())),
            "alternate_saved_x" => mirror.and_then(|e| e.alt_saved_cursor()).map_or(NONE, |c| c.0 as u32).to_string(),
            "alternate_saved_y" => mirror.and_then(|e| e.alt_saved_cursor()).map_or(NONE, |c| c.1 as u32).to_string(),
            "scroll_region_upper" => mirror.map(|e| e.scroll_region().0).unwrap_or(0).to_string(),
            "scroll_region_lower" => mirror
                .map(|e| e.scroll_region().1)
                .unwrap_or(rect.map(|r| r.rows).unwrap_or(t.rows).saturating_sub(1))
                .to_string(),
            "cursor_flag" => flag(mirror.is_none_or(|e| e.dec_mode(25))),
            "insert_flag" => flag(mirror.is_some_and(|e| e.ansi_mode(4))),
            "keypad_cursor_flag" => dec(1),
            "keypad_flag" => dec(66),
            "wrap_flag" => flag(mirror.is_none_or(|e| e.dec_mode(7))),
            "origin_flag" => dec(6),
            "mouse_standard_flag" => dec(1000),
            "mouse_button_flag" => dec(1002),
            "mouse_any_flag" | "mouse_all_flag" => dec(1003),
            "mouse_utf8_flag" => dec(1005),
            "mouse_sgr_flag" => dec(1006),
            // tmux 3.6 has no such variables: empty, as it answers.
            "bracket_paste_flag" | "bracketed_paste" | "focus_flag" => String::new(),
            _ => return None,
        })
    }

    fn global_var(&self, name: &str) -> Option<String> {
        Some(match name {
            "version" => VERSION.into(),
            "pid" => std::process::id().to_string(),
            "socket_path" => match &self.f.target {
                crate::http::Target::Socket(p) => p.display().to_string(),
                crate::http::Target::Url(u) => u.authority.clone(),
                crate::http::Target::Via(p, prefix) => format!("{}{prefix}", p.display()),
                crate::http::Target::Ssh(r) => format!("ssh:{}", r.dest),
                crate::http::Target::Control(l) => format!("control:{}", l.route),
            },
            "client_name" | "client_tty" => client_name(),
            "client_control_mode" => "1".into(),
            "client_session" => self.f.session_name(self.f.session),
            "client_pid" => std::process::id().to_string(),
            "client_termname" | "client_termtype" => "xterm-256color".into(),
            "client_utf8" => "1".into(),
            "client_readonly" | "client_prefix" => "0".into(),
            "host" | "host_short" => nix::unistd::gethostname()
                .ok()
                .and_then(|h| h.into_string().ok())
                .map(|h| if name == "host_short" { h.split('.').next().unwrap_or("").to_owned() } else { h })
                .unwrap_or_default(),
            "server_sessions" => self.f.state.sessions.len().to_string(),
            "start_time" => "0".into(),
            _ => return None,
        })
    }
}

impl Vars for Ctx<'_> {
    fn var(&self, name: &str) -> Option<String> {
        if name.starts_with('@') {
            return self.user_option(name);
        }
        self.pane_var(name)
            .or_else(|| self.window_var(name))
            .or_else(|| self.session_var(name))
            .or_else(|| self.global_var(name))
            .or_else(|| Front::builtin_option(name).map(str::to_owned))
    }
}

/// What tmux calls this client: its terminal, when it has one.
pub fn client_name() -> String {
    nix::unistd::ttyname(std::io::stdin())
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| format!("client-{}", std::process::id()))
}
