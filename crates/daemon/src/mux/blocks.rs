//! Starting things: running a command in a pane, opening a block, guest blocks,
//! and the defaults an editor or a viewer block gets from the pane it opens beside
//! (and an agent block, its login).

use super::{Daemon, config::Join};
use crate::acl::Principal;
use illogical_core::{Intent, Role};
use illogical_proto::{
    BlockType, MachineId, PaneId,
    api::{OpenRequest, RunRequest},
};
use std::path::PathBuf;

impl Daemon {
    /// `illogical run`: a new tab (or a split) running a command.
    pub(super) fn run_command(&mut self, req: RunRequest) -> Result<PaneId, String> {
        let from = req.from_pane.filter(|p| self.panes.contains_key(p));
        let cwd = req
            .cwd
            .clone()
            .map(PathBuf::from)
            .or_else(|| from.and_then(|p| self.panes.get(&p)?.cwd()))
            .unwrap_or_else(|| self.config.home.clone());
        // A session that doesn't exist yet starts with this pane, not a
        // shell beside it (#17: a home daemon's remote panes go in a
        // session of its name here).
        let fresh = req.session.clone().filter(|n| {
            req.split.is_none()
                && !req.vm_tab
                && !self.mux.sessions.iter().any(|s| s.name == *n || s.id.to_string() == *n)
        });
        let session = match fresh {
            Some(_) => None,
            None => self.resolve_session(req.session.as_deref(), from)?,
        };
        let before: Vec<PaneId> = self.mux.panes();
        // Joining a split pane's host: its tab's machine (which a split
        // takes anyway), or a sandbox it has a shell on (borrowed again).
        let join = match req.split.filter(|_| req.join) {
            Some(pane) => self.join_host(pane)?,
            None => Join::Here,
        };
        let host = match (&req.sandbox, &join) {
            (Some(sandbox), _) => Some(self.borrow_machine(sandbox)?),
            (None, Join::Borrow(sprite)) => Some(self.borrow_machine(&sprite.clone())?),
            (None, _) if req.vm || req.vm_tab => Some(self.new_machine(req.image.clone())?),
            (None, _) => None,
        };
        let on_machine = host.is_some() || matches!(join, Join::TabMachine);
        // A shell started in a directory: this host's (the default is the
        // pane it came from), or the machine's when one is given.
        self.next_cwd = match (&req.command, on_machine) {
            (Some(_), _) => None,
            (None, true) => req.cwd.clone().map(PathBuf::from),
            (None, false) => req.cwd.is_some().then(|| cwd.clone()),
        };
        self.next_spawn = req.command.as_ref().map(|command| {
            let spawn = match host {
                // Not this host's directory: the guest's, if one was asked for.
                Some(_) => self.config.guest_run(0, req.cwd.clone().map(PathBuf::from), command),
                None => self.config.run_only(0, cwd, command),
            };
            (spawn, Some(command.clone()))
        });
        self.next_host = host;
        self.next_owner_tab = req.vm_tab;
        let split = req.split.filter(|_| !req.vm_tab);
        let intent = match (split, session) {
            // Here: a script's command is for this host, even in a VM tab
            // (unless it asked to join the pane's machine).
            (Some(pane), _) => {
                let local = !matches!(join, Join::TabMachine);
                Intent::Split { pane, edge: illogical_proto::Edge::Right, local, cwd: None }
            }
            (None, Some(session)) => Intent::NewTab { session, from_pane: from, cwd: None },
            (None, None) => Intent::NewSession { name: fresh, from_pane: from },
        };
        let result = self.intent(None, intent);
        self.next_spawn = None;
        self.next_owner_tab = false;
        self.next_cwd = None;
        if let Some(m) = self.next_host.take() {
            // Nothing took it.
            self.machines.remove(&m);
        }
        result?;
        let pane = self.mux.panes().into_iter().find(|p| !before.contains(p)).ok_or("no pane was created")?;
        if let Some(policy) = req.policy {
            let m = self.meta.entry(pane).or_default();
            (m.policy, m.policy_set) = (policy, true);
        }
        Ok(pane)
    }

    /// Where a pane joining `pane`'s host runs (`run --split --join`).
    fn join_host(&self, pane: PaneId) -> Result<Join, String> {
        let Some(m) = self.machine_of(pane) else { return Ok(Join::Here) };
        if m.borrowed {
            return Ok(Join::Borrow(m.sprite.clone()));
        }
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        if self.tab_machine(tab) == Some(m.id) {
            return Ok(Join::TabMachine);
        }
        Err(format!("%{pane}'s machine is its own: share it with the tab first (Share machine with tab)"))
    }

    /// Which session a new tab goes in: one named (made if missing), else
    /// `from`'s, else the first.
    fn resolve_session(&mut self, name: Option<&str>, from: Option<PaneId>) -> Result<Option<u32>, String> {
        Ok(match name {
            Some(name) => match self.mux.sessions.iter().find(|s| s.name == name || s.id.to_string() == name) {
                Some(s) => Some(s.id),
                None => {
                    // A new session starts with a shell; the block gets a
                    // tab of its own next to it.
                    self.intent(None, Intent::NewSession { name: Some(name.to_owned()), from_pane: None })?;
                    self.mux.sessions.last().map(|s| s.id)
                }
            },
            None => from
                .and_then(|p| self.mux.tab_of(p).ok())
                .and_then(|t| self.mux.session_of_tab(t).ok())
                .or_else(|| self.mux.sessions.first().map(|s| s.id)),
        })
    }

    /// `POST /api/blocks`: a new block of any type, in a tab of its own or
    /// split beside another. In a VM tab it runs on the tab's machine.
    /// A guest's block (M14): an agent, beside a pane in a session they
    /// edit, on a VM of their own (within their quota).
    pub(super) fn guest_block(&mut self, mut req: OpenRequest, who: &Principal) -> Result<PaneId, String> {
        if req.kind != BlockType::Agent {
            return Err("guests can start agents; other blocks are the owner's".into());
        }
        if !req.config["as_fountain"].is_null() {
            return Err("only the owner can wear a Fountain agent here".into());
        }
        let pane = req.split.or(req.from_pane).ok_or("start it beside a pane")?;
        let session = self.session_of(pane).ok_or("no such pane")?;
        if self.config.acl.role(who, session).is_none_or(|r| r < Role::Editor) {
            return Err("you can't start agents in this session".into());
        }
        let mine = self.machines.values().filter(|m| m.by.as_deref() == Some(who.id())).count();
        if mine >= self.config.guest_machines {
            return Err(format!("you have {mine} VMs here, the most a guest may have: close one first"));
        }
        (req.vm, req.local, req.host, req.session) = (true, false, None, None);
        let before: Vec<MachineId> = self.machines.keys().copied().collect();
        let block = self.open_block(req)?;
        for m in self.machines.values_mut().filter(|m| !before.contains(&m.id)) {
            m.by = Some(who.id().to_owned());
        }
        self.meta.entry(block).or_default().guest = Some(who.id().to_owned());
        Ok(block)
    }

    /// The guest behind a pane (by principal id): who started it, as its
    /// meta says, or whose VM it runs on.
    pub(super) fn guest_behind(&self, pane: PaneId) -> Option<String> {
        self.meta.get(&pane).and_then(|m| m.guest.clone()).or_else(|| self.machine_of(pane)?.by.clone())
    }

    pub(super) fn open_block(&mut self, mut req: OpenRequest) -> Result<PaneId, String> {
        if req.kind == BlockType::Terminal {
            return Err("terminals are opened with run".into());
        }
        let from = req.from_pane.filter(|p| self.panes.contains_key(p) || self.blocks.contains_key(p));
        match req.kind {
            BlockType::Editor => self.editor_defaults(&mut req, from),
            BlockType::Diff | BlockType::File => self.view_defaults(&mut req, from),
            BlockType::Agent => self.agent_login(&mut req, from),
            _ => {}
        }
        // A pane on another daemon (#17): only its place is here, never on
        // a machine of ours.
        if req.kind == BlockType::Remote {
            crate::remote::parse(&req.config)?;
            (req.vm, req.host, req.local) = (false, None, true);
        }
        // A studio box (M35) is its own site: nothing of it runs here or on
        // a machine of ours.
        if req.kind == BlockType::App {
            (req.vm, req.host, req.local) = (false, None, true);
        }
        let session = self.resolve_session(req.session.as_deref(), from)?;
        let before: Vec<PaneId> = self.mux.panes();
        self.last_block_error = None;
        // On a new machine of its own, or one that exists (a tab's).
        if let Some(m) = req.host
            && !self.machines.contains_key(&m)
        {
            return Err(format!("no machine m{m}"));
        }
        self.next_host = match (req.vm, req.host) {
            (true, _) => Some(self.new_machine(req.image.clone())?),
            (false, host) => host,
        };
        let made = req.vm.then_some(self.next_host).flatten();
        // Here if asked; an agent also runs here unless asked for a machine;
        // a page in a VM tab is the tab's machine's.
        let local = req.local || (req.kind == BlockType::Agent && self.next_host.is_none());
        self.next_block = Some((req.kind, req.config));
        let intent = match (req.split, session) {
            (Some(pane), _) => Intent::Split { pane, edge: illogical_proto::Edge::Right, local, cwd: None },
            (None, Some(session)) => Intent::NewTab { session, from_pane: from, cwd: None },
            (None, None) => Intent::NewSession { name: None, from_pane: from },
        };
        let result = self.intent(None, intent);
        let unused = self.next_block.take();
        if self.next_host.take().is_some()
            && let Some(m) = made
        {
            // Nothing took it.
            self.machines.remove(&m);
        }
        result?;
        if let Some((kind, _)) = unused {
            return Err(format!("no {kind:?} block was made").to_lowercase());
        }
        let made = self.mux.panes().into_iter().find(|p| !before.contains(p));
        let Some(id) = made else {
            return Err(self.last_block_error.take().unwrap_or_else(|| "no block was made".into()));
        };
        if !self.blocks.contains_key(&id) {
            return Err(self.last_block_error.take().unwrap_or_else(|| "the block couldn't start".into()));
        }
        Ok(id)
    }

    /// `pane` alone in a tab, right after the one it was in; that tab
    /// named `name` unless it has a name.
    pub(super) fn own_tab(&mut self, pane: PaneId, name: Option<String>) -> Result<(), String> {
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        let session = self.mux.session_of_tab(tab).map_err(|e| e.to_string())?;
        if self.mux.tab(tab).map_err(|e| e.to_string())?.root != illogical_core::Node::pane(pane) {
            let index =
                self.mux.session(session).ok().and_then(|s| s.tabs.iter().position(|t| *t == tab)).map(|i| i + 1);
            self.intent(None, Intent::BreakPane { pane, session, index })?;
        }
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        if let Some(name) = name
            && self.mux.tab(tab).is_ok_and(|t| t.name.is_none())
        {
            self.intent(None, Intent::RenameTab { tab, name: Some(name) })?;
        }
        Ok(())
    }

    /// An editor opens on the machine of the pane it's opened from, in
    /// that pane's directory, unless told otherwise.
    fn editor_defaults(&self, req: &mut OpenRequest, from: Option<PaneId>) {
        let beside = req.split.or(from);
        if req.host.is_none() && !req.local && !req.vm {
            req.host = beside.and_then(|p| self.meta.get(&p)).and_then(|m| m.host);
        }
        let has_path = ["path", "folder"].iter().any(|k| req.config.get(*k).is_some_and(|v| !v.is_null()));
        if !has_path {
            let here = req.host.is_none();
            let cwd = beside.and_then(|p| self.info_of_any(p)).and_then(|i| i.cwd);
            let path = match cwd {
                Some(c) => c,
                None if here => self.config.home.display().to_string(),
                None => "~".into(),
            };
            if !req.config.is_object() {
                req.config = serde_json::json!({});
            }
            req.config["path"] = path.into();
        }
    }

    /// #379: a Claude Code block started beside a pane uses that pane's
    /// login, unless the request said whose (the CLI and the MCP bridge
    /// send their own `CLAUDE_CONFIG_DIR`): an agent block's
    /// `claude_config_dir`, or a terminal's `CLAUDE_CONFIG_DIR`, read from
    /// its foreground program's environment (`CLAUDE_CONFIG_DIR=… claude`),
    /// else its shell's. Only that variable is read. None found: the
    /// daemon's environment decides, as before.
    fn agent_login(&self, req: &mut OpenRequest, from: Option<PaneId>) {
        let c = &req.config;
        let claude = matches!(c["agent"].as_str(), None | Some("claude"));
        // An opened conversation (M33) is in the daemon's own directory.
        let opened = !c["import"].is_null() || !c["session_id"].is_null();
        if req.vm || req.host.is_some() || !claude || opened || !c["claude_config_dir"].is_null() {
            return;
        }
        let Some(beside) = req.split.or(from) else { return };
        let dir = if let Some(b) = self.blocks.get(&beside) {
            (b.kind() == BlockType::Agent)
                .then(|| b.config()["claude_config_dir"].as_str().map(str::to_owned))
                .flatten()
        } else if self.meta.get(&beside).is_some_and(|m| m.host.is_some()) {
            None
        } else {
            self.panes.get(&beside).and_then(|h| h.pid_now()).and_then(|shell| {
                let var = crate::agent::defs::CLAUDE_CONFIG_DIR;
                let fg = crate::procinfo::foreground(shell).filter(|p| *p != shell);
                fg.and_then(|p| crate::procinfo::env_var(p, var)).or_else(|| crate::procinfo::env_var(shell, var))
            })
        };
        if let Some(d) = dir.filter(|d| !d.is_empty()) {
            if !req.config.is_object() {
                req.config = serde_json::json!({});
            }
            req.config["claude_config_dir"] = d.into();
        }
    }

    /// A diff or file block (M11) is on the machine of the pane it's opened
    /// from (a VM tab's, say), unless told otherwise; a diff's repository
    /// is that pane's directory's, and a file's relative path is from it.
    fn view_defaults(&self, req: &mut OpenRequest, from: Option<PaneId>) {
        let beside = req.split.or(from);
        if req.host.is_none() && !req.local && !req.vm {
            req.host = beside.and_then(|p| self.meta.get(&p)).and_then(|m| m.host);
        }
        if !req.config.is_object() {
            req.config = serde_json::json!({});
        }
        let cwd = beside.and_then(|p| self.info_of_any(p)).and_then(|i| i.cwd);
        let key = if req.kind == BlockType::Diff { "repo" } else { "path" };
        let given = req.config[key].as_str().filter(|p| !p.is_empty()).map(str::to_owned);
        let here = req.host.is_none();
        let home = || if here { self.config.home.display().to_string() } else { "~".into() };
        let whole = match (given, cwd) {
            (Some(p), _) if p.starts_with('/') || p.starts_with('~') => p,
            (Some(p), Some(c)) => format!("{}/{p}", c.trim_end_matches('/')),
            (Some(p), None) => p,
            (None, Some(c)) => c,
            (None, None) => home(),
        };
        req.config[key] = whole.into();
    }

    /// Tell blocks whether anyone draws them (M11): a client that isn't
    /// summaries-only shows their tab, and on a phone, them.
    pub(super) fn sync_drawn(&mut self) {
        if self.blocks.is_empty() && self.drawn.is_empty() {
            return;
        }
        let mut now = std::collections::HashSet::new();
        for (client, tab) in &self.viewing {
            if self.summary.contains(client) || !self.clients.contains_key(client) {
                continue;
            }
            let Ok(t) = self.mux.tab(*tab) else { continue };
            let panes = t.root.panes();
            let zoom = self.zoomed.get(client).filter(|z| panes.contains(z));
            now.extend(panes.into_iter().filter(|p| self.blocks.contains_key(p) && zoom.is_none_or(|z| z == p)));
        }
        if now == self.drawn {
            return;
        }
        for id in now.symmetric_difference(&self.drawn) {
            if let Some(b) = self.blocks.get(id) {
                b.drawn(now.contains(id));
            }
        }
        self.drawn = now;
    }
}
