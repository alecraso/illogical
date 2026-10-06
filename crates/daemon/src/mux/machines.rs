//! Machines: creating, borrowing, sharing, resetting and deleting the
//! machines (sprites) panes can run on.

use super::{Cmd, Daemon, seed};
use crate::acl::Principal;
use illogical_core::Intent;
use illogical_proto::{EventKind, Machine, MachineId, MachineState, Owner, PaneId, TabId};
use tracing::{info, warn};

impl Daemon {
    /// A new machine for the next pane; it's created when its first
    /// program starts.
    pub(super) fn new_machine(&mut self, image: Option<String>) -> Result<MachineId, String> {
        if self.config.provider.is_none() {
            return Err("VM panes aren't set up: illogicald found no wisp token (see --wisp-token-file)".into());
        }
        let id = self.next_machine;
        let sprite = format!("{}{id}", self.config.sprite_prefix());
        Ok(self.add_machine(sprite, image, false))
    }

    /// Someone else's sandbox, borrowed for the next pane's shell ("open
    /// shell", M4b): never created, reset or deleted by us.
    pub(super) fn borrow_machine(&mut self, sprite: &str) -> Result<MachineId, String> {
        if self.config.provider.is_none() {
            return Err("no sandbox provider: illogicald found no wisp token (see --wisp-token-file)".into());
        }
        if sprite.starts_with(&self.config.sprite_prefix()) {
            return Err(format!("{sprite} is one of this daemon's own machines"));
        }
        Ok(self.add_machine(sprite.to_owned(), None, true))
    }

    fn add_machine(&mut self, sprite: String, image: Option<String>, borrowed: bool) -> MachineId {
        let id = self.next_machine;
        self.next_machine += 1;
        let provider = self.config.provider.as_ref().map_or("wisp", |p| p.name()).to_owned();
        let owner = Owner::Pane(0);
        // Ours get a name to show; a borrowed sandbox has its own.
        let name = (!borrowed).then(|| {
            let taken = |n: &str| self.machines.values().any(|m| m.name.as_deref() == Some(n));
            illogical_core::names::generate(seed(), taken)
        });
        let state = MachineState::Starting;
        let m = Machine { id, provider, sprite, name, image, owner, state, borrowed, by: None };
        self.machines.insert(id, m);
        id
    }

    pub(super) fn machine_of(&self, pane: PaneId) -> Option<&Machine> {
        self.machines.get(&self.meta.get(&pane)?.host?)
    }

    /// The machine a tab owns.
    pub(super) fn tab_machine(&self, tab: TabId) -> Option<MachineId> {
        self.machines.values().find(|m| m.owner == Owner::Tab(tab)).map(|m| m.id)
    }

    /// Whether a pane runs on its own tab's machine (and so can't leave it).
    fn on_tab_machine(&self, pane: PaneId) -> bool {
        let tab = self.mux.tab_of(pane).ok();
        let host = self.meta.get(&pane).and_then(|m| m.host);
        host.is_some() && tab.and_then(|t| self.tab_machine(t)) == host
    }

    /// Refuse moves that would take a pane away from its tab's machine.
    pub(super) fn check_move(&self, intent: &Intent) -> Result<(), String> {
        let stuck = |pane: PaneId| format!("%{pane} runs on this tab's machine, so it stays in the tab");
        match *intent {
            Intent::MovePane { pane, target, .. }
                if self.on_tab_machine(pane) && self.mux.tab_of(pane).ok() != self.mux.tab_of(target).ok() =>
            {
                Err(stuck(pane))
            }
            Intent::BreakPane { pane, .. } if self.on_tab_machine(pane) => Err(stuck(pane)),
            Intent::DockTab { tab, .. } if self.tab_machine(tab).is_some() => {
                Err("this tab has a machine: move the whole tab instead".into())
            }
            _ => Ok(()),
        }
    }

    /// Delete machines whose tab has closed.
    pub(super) fn reap_machines(&mut self) {
        let gone: Vec<MachineId> = self
            .machines
            .values()
            .filter(|m| matches!(m.owner, Owner::Tab(t) if self.mux.tab(t).is_err()))
            .map(|m| m.id)
            .collect();
        for m in gone {
            self.delete_machine(m);
        }
    }

    /// "Share machine with tab": the pane's own machine becomes its tab's,
    /// and new splits in the tab join it.
    pub(super) fn share_machine(&mut self, pane: PaneId) -> Result<(), String> {
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        if self.tab_machine(tab).is_some() {
            return Err("this tab already has a machine".into());
        }
        let id = self.meta.get(&pane).and_then(|m| m.host).ok_or("that pane runs on this host")?;
        let m = self.machines.get_mut(&id).ok_or("no such machine")?;
        if m.owner != Owner::Pane(pane) {
            return Err("that pane's machine isn't its own".into());
        }
        m.owner = Owner::Tab(tab);
        info!(pane, tab, machine = id, "machine shared with tab");
        self.changed();
        Ok(())
    }

    /// "Reset machine": delete its sprite, then start every pane on it again
    /// by its policy, on a new one.
    pub(super) fn reset_machine(&mut self, id: MachineId) -> Result<(), String> {
        let m = self.machines.get_mut(&id).ok_or("no such machine")?;
        if m.borrowed {
            return Err(format!("{} isn't ours to reset", m.sprite));
        }
        m.state = MachineState::Starting;
        let sprite = m.sprite.clone();
        let provider = self.config.provider.clone().ok_or("VM panes aren't set up")?;
        info!(machine = id, sprite, "resetting machine");
        self.emit(None, EventKind::Machine { machine: id, state: MachineState::Starting });
        self.broadcast();
        // Let go first, so its panes don't report the machine gone.
        for (pane, meta) in &self.meta {
            if meta.host == Some(id)
                && let Some(h) = self.panes.get(pane)
            {
                h.release();
            }
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Err(e) = provider.delete(&sprite).await {
                warn!(sprite, error = %e, "can't delete machine to reset it");
            }
            let _ = tx.send(Cmd::MachineReset(id));
        });
        Ok(())
    }

    pub(super) fn machine_reset(&mut self, id: MachineId) {
        let panes: Vec<PaneId> = self.meta.iter().filter(|(_, m)| m.host == Some(id)).map(|(p, _)| *p).collect();
        for pane in panes {
            let (Some(h), Some(meta)) = (self.panes.get(&pane), self.meta.get(&pane)) else { continue };
            h.restart(self.config.restore(pane, meta), "machine reset");
        }
    }

    /// Delete a machine and everything on it, in the background.
    pub(super) fn delete_machine(&mut self, id: MachineId) {
        let Some(m) = self.machines.remove(&id) else { return };
        self.emit(None, EventKind::Machine { machine: id, state: MachineState::Gone });
        if m.borrowed {
            // Its shells were hung up as their panes closed.
            return info!(machine = m.id, sprite = m.sprite, "let go of a borrowed machine");
        }
        let Some(provider) = self.config.provider.clone() else { return };
        tokio::spawn(async move {
            match provider.delete(&m.sprite).await {
                Ok(()) => info!(machine = m.id, sprite = m.sprite, "deleted machine"),
                Err(e) => warn!(machine = m.id, sprite = m.sprite, error = %e, "can't delete machine"),
            }
        });
    }

    /// Delete our sprites that no machine owns: left by a crash, or by a
    /// pane closed while the daemon was down.
    pub(super) fn sweep_machines(&self) {
        let Some(provider) = self.config.provider.clone() else { return };
        let prefix = self.config.sprite_prefix();
        let keep: Vec<String> = self.machines.values().map(|m| m.sprite.clone()).collect();
        tokio::spawn(async move {
            let names = match provider.list(&prefix).await {
                Ok(n) => n.into_iter().map(|s| s.name).collect::<Vec<_>>(),
                Err(e) => return warn!(error = %e, "can't list machines to sweep"),
            };
            for name in names.into_iter().filter(|n| !keep.contains(n)) {
                match provider.delete(&name).await {
                    Ok(()) => info!(sprite = name, "deleted a machine nothing owns"),
                    Err(e) => warn!(sprite = name, error = %e, "can't delete stray machine"),
                }
            }
        });
    }

    /// The machines `who` sees: those their panes run on.
    pub(super) fn machines_for(&self, who: &Principal) -> Vec<Machine> {
        if who.is_owner() {
            return self.machines.values().cloned().collect();
        }
        let hosts: std::collections::HashSet<MachineId> = self
            .mux
            .sessions
            .iter()
            .filter(|s| self.config.acl.role(who, s.id).is_some())
            .flat_map(|s| &s.tabs)
            .filter_map(|t| self.mux.tab(*t).ok())
            .flat_map(|t| t.root.panes())
            .filter_map(|p| self.meta.get(&p).and_then(|m| m.host))
            .collect();
        self.machines.values().filter(|m| hosts.contains(&m.id)).cloned().collect()
    }

    // ---- who may see and do what (M12)
}
