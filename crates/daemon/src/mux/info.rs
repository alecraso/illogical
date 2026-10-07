//! What the multiplexer knows about itself: refreshing pane metadata, saving
//! the layout, shutdown, and the pane, block and state summaries sent out.

use super::{Daemon, ProcSeen, agent_in};
use crate::{
    block::Block,
    pane::{CommandRec, PaneHandle},
    store::{LAYOUT_VERSION, PaneMeta, Saved, now_ms},
};
use arugula_proto::{BlockType, CommandInfo, PaneId, PaneInfo, State, TabView, WorkKind, api::PaneSummary};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::time::Instant;
use tracing::{info, warn};

/// What the OS says a pane runs is read again after this.
const PROC_FRESH: Duration = Duration::from_secs(1);

fn info_of(rec: CommandRec) -> CommandInfo {
    CommandInfo {
        text: rec.text,
        cwd: rec.cwd,
        exit: rec.exit,
        started_ms: rec.started_ms,
        ended_ms: rec.ended_ms,
        start: rec.start,
        end: rec.end,
        by: rec.by,
    }
}

impl Daemon {
    /// Note each running pane's directory and the command a re-run would
    /// run, and mark the panes where either changed.
    fn refresh_meta(&mut self) {
        let mut changed = vec![];
        let mut changed_running = vec![];
        let mut agents = vec![];
        for (id, h) in &self.panes {
            if !h.running() {
                continue;
            }
            let status = h.status();
            let m = self.meta.entry(*id).or_default();
            let cwd = status.cwd.clone().or_else(|| h.cwd().map(|c| c.display().to_string())).or(m.cwd.clone());
            // The command line as typed, when the shell integration reported
            // it; otherwise what /proc says is in the foreground.
            let fg = h.command();
            // An agent started without a word to the shell integration (or
            // after `cd x &&`) is found here, at the latest.
            let typed = status.current.as_ref().and_then(|c| c.text.as_deref());
            agents.push((*id, fg.as_deref().or(typed).and_then(agent_in)));
            let command = match status.current {
                Some(c) => c.text.or_else(|| fg.clone()),
                None => fg.clone(),
            };
            // What clients were told runs there (and so its kind), too.
            let told = self.procs.borrow().get(id).map(|s| s.command.clone());
            if m.cwd != cwd || m.command != command || told.is_some_and(|t| t != fg) {
                changed.push(*id);
            }
            (m.cwd, m.command) = (cwd, command);
        }
        for id in changed {
            self.procs.borrow_mut().remove(&id);
            self.mark(id);
        }
        // Whether each pane's agent conversation is still the one running.
        for (id, agent) in &agents {
            if let Some(s) = self.meta.get_mut(id).and_then(|m| m.session.as_mut())
                && s.running != (agent.as_deref() == Some(s.agent.as_str()))
            {
                s.running = !s.running;
                changed_running.push(*id);
            }
        }
        for id in changed_running {
            self.mark(id);
        }
        // Claude Code without hooks: its session files say which
        // conversation each holds (#146).
        if agents.iter().any(|(_, a)| a.as_deref() == Some("claude")) {
            let ours = crate::conversations::Ours {
                panes: self.panes.iter().filter_map(|(id, h)| Some((h.pid_now()?, *id))).collect(),
                ..Default::default()
            };
            let live = crate::conversations::live_in_panes(&crate::conversations::Dirs::from_env(), &ours);
            for (id, (sid, cwd)) in live {
                let claude = agents.iter().any(|(p, a)| *p == id && a.as_deref() == Some("claude"));
                if claude && crate::resume::valid_id(&sid) {
                    self.set_session(id, "claude", &sid, None, cwd);
                }
            }
        }
        for (id, agent) in agents {
            self.watch_named(id, agent);
        }
    }

    /// Write the layout and pane details if anything changed since the last
    /// write.
    pub(super) fn save(&mut self) {
        self.threads.save();
        // Whichever notices a new directory or command tells the clients
        // (at the next tick).
        self.refresh_meta();
        for (id, b) in &self.blocks {
            if let Some(m) = self.meta.get_mut(id) {
                m.config = Some(b.config());
            }
        }
        let panes: BTreeMap<PaneId, PaneMeta> = self.meta.iter().map(|(k, v)| (*k, v.clone())).collect();
        if self.last_saved.as_ref().is_some_and(|(m, p, ms)| *m == self.mux && *p == panes && *ms == self.machines) {
            return;
        }
        let saved = Saved {
            version: LAYOUT_VERSION,
            saved_at_ms: now_ms(),
            mux: self.mux.clone(),
            panes,
            machines: self.machines.clone(),
            next_machine: self.next_machine,
        };
        match self.store.save_layout(&saved) {
            Ok(()) => self.last_saved = Some((saved.mux, saved.panes, saved.machines)),
            Err(e) => warn!(error = %e, "can't save layout"),
        }
    }

    /// Before the daemon exits: every pane's terminal and the layout to disk.
    /// Exits from here on (panes hung up by our own exit) change nothing.
    pub(super) fn shutdown(&mut self) {
        self.shutting_down = true;
        for p in self.panes.values() {
            p.checkpoint(Duration::from_secs(3));
        }
        self.last_saved = None;
        self.save();
        info!(panes = self.panes.len(), "saved for shutdown");
    }

    /// A pane's directory and foreground command from the OS, read at most
    /// once a second (or again after something happened in it).
    fn proc_seen(&self, p: &PaneHandle) -> ProcSeen {
        if let Some(seen) = self.procs.borrow().get(&p.id).filter(|s| s.at.elapsed() < PROC_FRESH) {
            return seen.clone();
        }
        let command = p.command();
        let work = match &command {
            Some(c) => crate::classify::kind(c),
            None => p.own_command().map_or(WorkKind::Shell, |c| crate::classify::kind(&c)),
        };
        let seen = ProcSeen { at: Instant::now(), cwd: p.cwd().map(|c| c.display().to_string()), command, work };
        self.procs.borrow_mut().insert(p.id, seen.clone());
        seen
    }

    fn pane_info(&self, p: &PaneHandle) -> PaneInfo {
        let meta = self.meta.get(&p.id).cloned().unwrap_or_default();
        let running = p.running();
        let status = p.status();
        let seen = if running { Some(self.proc_seen(p)) } else { None };
        let resumes = crate::resume::applies(&meta).map(|s| {
            format!("{} conversation {}", crate::resume::name(&s.agent), s.id.chars().take(8).collect::<String>())
        });
        let cwd = status.cwd.clone().or_else(|| seen.as_ref().and_then(|s| s.cwd.clone())).or(meta.cwd);
        // The process's own command line sees through aliases; the typed
        // text is next best (a command that hasn't started its process yet).
        let typed = status.current.as_ref().and_then(|c| c.text.as_deref()).map(crate::classify::kind);
        let work = match &seen {
            Some(s) if s.command.is_some() => s.work,
            _ => typed.or(seen.as_ref().map(|s| s.work)).unwrap_or(WorkKind::Shell),
        };
        let command = if running { seen.and_then(|s| s.command) } else { meta.command };
        PaneInfo {
            resumes,
            id: p.id,
            epoch: p.epoch,
            project: cwd.as_deref().and_then(crate::classify::project),
            cwd,
            command,
            work: Some(work),
            activity: self.activity.get(&p.id).map(|(_, a)| *a).filter(|a| a.last_ms > 0),
            title: status.title.clone(),
            file: None,
            editor: None,
            diff: self.diffs.iter().find(|d| d.pane == Some(p.id)).map(|d| d.info.clone()),
            claude_ide: self.ide_conns.values().any(|c| c.1 == Some(p.id)),
            started_by: meta.started_by.clone(),
            running,
            policy: meta.policy,
            current: status.current.map(info_of),
            last: status.last.map(info_of),
            attention: self.attention.get(&p.id).copied().unwrap_or_default(),
            reason: self.live_reason(p.id),
            integration: meta.integration.unwrap_or(true),
            kind: BlockType::Terminal,
            host: meta.host,
            ask: self.asks.get(&p.id).map(|a| a.ask.clone()),
            answered: self.answered.get(&p.id).cloned(),
            inbox: self.inbox.contains_key(&p.id),
            driver: self.drivers.get(&p.id).cloned(),
            typing: self.typing.contains(&p.id),
            pair: self.pair.contains(&p.id),
            private: meta.private,
            trusted: {
                let now = now_ms();
                let mut t: Vec<(String, u64)> = self
                    .trust
                    .iter()
                    .filter(|((pane, _), until)| *pane == p.id && **until > now)
                    .map(|((_, w), u)| (w.clone(), *u))
                    .collect();
                t.sort();
                t
            },
        }
    }

    /// A non-terminal block's entry in the state.
    pub(super) fn block_info(&self, id: PaneId, b: &Arc<dyn Block>) -> PaneInfo {
        let meta = self.meta.get(&id).cloned().unwrap_or_default();
        let s = b.summary();
        PaneInfo {
            id,
            epoch: 0,
            project: s.project,
            cwd: s.cwd,
            file: s.file,
            command: None,
            resumes: None,
            running: true,
            policy: meta.policy,
            current: None,
            last: None,
            attention: self.attention.get(&id).copied().unwrap_or_default(),
            reason: self.live_reason(id),
            integration: false,
            kind: b.kind(),
            host: meta.host,
            // A question raised on it (M35), as a terminal's is drawn.
            ask: self.asks.get(&id).map(|a| a.ask.clone()),
            answered: self.answered.get(&id).cloned(),
            inbox: false,
            driver: None,
            typing: false,
            pair: false,
            private: meta.private,
            trusted: Vec::new(),
            work: s.work.or(match b.kind() {
                BlockType::Agent => Some(WorkKind::Agent),
                BlockType::App => Some(WorkKind::App),
                BlockType::Forge => Some(WorkKind::Pr),
                BlockType::Fountain => Some(WorkKind::Fountain),
                _ => None,
            }),
            activity: None,
            title: s.title,
            started_by: meta.started_by.clone(),
            editor: s.editor,
            diff: None,
            claude_ide: false,
        }
    }

    pub(super) fn info_of_any(&self, id: PaneId) -> Option<PaneInfo> {
        match (self.panes.get(&id), self.blocks.get(&id)) {
            (Some(h), _) => Some(self.pane_info(h)),
            (_, Some(b)) => Some(self.block_info(id, b)),
            _ => None,
        }
    }

    pub(super) fn summaries(&self) -> Vec<PaneSummary> {
        let mut out = Vec::new();
        for s in &self.mux.sessions {
            for tab in &s.tabs {
                let Ok(t) = self.mux.tab(*tab) else { continue };
                for pane in t.root.panes() {
                    let Some(info) = self.info_of_any(pane) else { continue };
                    out.push(PaneSummary {
                        session: s.id,
                        session_name: s.name.clone(),
                        tab: t.id,
                        tab_name: t.name.clone(),
                        info,
                    });
                }
            }
        }
        out
    }

    pub(super) fn state(&self) -> State {
        let tabs = self
            .mux
            .sessions
            .iter()
            .flat_map(|s| &s.tabs)
            .filter_map(|id| {
                let t = self.mux.tab(*id).ok()?;
                Some(TabView {
                    id: t.id,
                    name: t.name.clone(),
                    root: t.root.clone(),
                    cols: t.cols,
                    rows: t.rows,
                    owner: t.owner,
                    zoom: t.zoom,
                    layout: self.mux.layout(t.id).ok()?,
                })
            })
            .collect();
        let mut panes: Vec<PaneInfo> = self.panes.values().map(|p| self.pane_info(p)).collect();
        panes.extend(self.blocks.iter().map(|(id, b)| self.block_info(*id, b)));
        panes.sort_by_key(|p| p.id);
        let machines = self.machines.values().cloned().collect();
        let options = Box::new(self.mux.options.clone());
        State {
            rev: self.mux.rev,
            sessions: self.mux.sessions.clone(),
            tabs,
            panes,
            machines,
            options,
            roles: None,
            presence: Vec::new(),
            threads: Vec::new(),
            calls: Vec::new(),
        }
    }
}
