//! Connected clients: their messages and layout intents, and the changes,
//! ticks and state pushes that send them what happened.

use super::{AskReply, Daemon, Dirty, PaneView, People, SAVE_DEBOUNCE, Sent, TYPING, exec_tag, seed};
use crate::{
    acl::Principal,
    pane::{Start, ToClient, Want},
    store::{PaneLog, PaneMeta},
};
use illogical_core::{Effect, Intent, Role};
use illogical_proto::{
    Activity, Attention, ClientId, ClientMsg, Delta, Driver, EventKind, MachineId, Owner, PaneId, PaneInfo, PaneOp,
    Presence, ServerMsg, SessionId, State, TabId,
};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    time::Duration,
};
use tokio::time::Instant;
use tracing::{info, warn};

/// Changes a card depends on (attention, a question, who drives) reach
/// clients within this (M23); several in a row go together.
const URGENT: Duration = Duration::from_millis(40);
/// Pane fields a summary leaves out (M23): a client that needs them reads
/// the pane.
const NOT_IN_SUMMARIES: &[&str] = &["epoch", "policy", "resumes", "integration"];

impl Daemon {
    pub(super) fn message(&mut self, client: ClientId, msg: ClientMsg) {
        let Some(sub) = self.clients.get(&client).cloned() else {
            return;
        };
        let who = sub.principal.clone();
        match msg {
            ClientMsg::Attach { panes, zstd, acks, kitty_keys } => {
                for a in panes {
                    if !self.readable(&who, a.pane) {
                        continue;
                    }
                    let floor = self.session_of(a.pane).and_then(|s| self.config.acl.floor(&who, s, a.pane));
                    if let Some(p) = self.panes.get(&a.pane) {
                        p.attach_with(
                            sub.clone(),
                            Want { offset: a.offset, history: a.history, zstd, floor, acks, kitty_keys },
                        );
                    }
                }
            }
            ClientMsg::Ack { pane, offset } => {
                if let Some(p) = self.panes.get(&pane) {
                    p.ack(client, offset);
                }
            }
            ClientMsg::Detach { panes } => {
                for id in panes {
                    if let Some(p) = self.panes.get(&id) {
                        p.detach(client);
                    }
                }
            }
            ClientMsg::View { tab, cols, rows, zoom, claim } => {
                match zoom {
                    Some(z) => self.zoomed.insert(client, z),
                    None => self.zoomed.remove(&client),
                };
                if self.viewing.insert(client, tab) != Some(tab) {
                    // Presence only: no pane changed.
                    self.soon();
                }
                // Only editors size a tab; viewers letterbox.
                let editor = who.is_owner()
                    || self
                        .mux
                        .session_of_tab(tab)
                        .ok()
                        .and_then(|s| self.config.acl.role(&who, s))
                        .is_some_and(|r| r >= Role::Editor);
                if !editor {
                    return;
                }
                if let Ok(true) = self.mux.view(client, tab, cols, rows, zoom, claim) {
                    self.changed();
                }
            }
            ClientMsg::Intent { id, intent } => {
                if !who.is_owner() {
                    let allowed = illogical_core::access::need(&self.mux, &intent)
                        .map(|n| n.allowed(|s| self.config.acl.role(&who, s)))
                        .unwrap_or(false);
                    if !allowed {
                        let message = "you can't do that here (ask the owner for more access)".to_owned();
                        let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id, message }));
                        return;
                    }
                    // An agent's invites (#234): the owner's to close, alone
                    // or with their tab or session.
                    let panes = |tab| self.mux.tab(tab).map(|t| t.root.panes()).unwrap_or_default();
                    let closes = match &intent {
                        Intent::ClosePane { pane } => vec![*pane],
                        Intent::CloseTab { tab } => panes(*tab),
                        Intent::CloseSession { session } => self
                            .mux
                            .session(*session)
                            .map(|s| s.tabs.iter().flat_map(|t| panes(*t)).collect())
                            .unwrap_or_default(),
                        _ => vec![],
                    };
                    if closes.iter().any(|p| self.is_invite(*p)) {
                        let message = crate::invite::CLOSE_OWNER_ONLY.to_owned();
                        let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id, message }));
                        return;
                    }
                }
                let done = if who.is_owner() {
                    self.intent(Some(client), intent)
                } else {
                    self.guest_intent(client, &who, intent)
                };
                if let Err(message) = done {
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id, message }));
                }
            }
            ClientMsg::Ping { id } => {
                // Whatever it did has been sent before the answer.
                self.flush();
                let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Pong { id }));
            }
            ClientMsg::Subscribe { summary } => {
                if summary {
                    self.summary.insert(client);
                    for p in self.panes.values() {
                        p.detach(client);
                    }
                } else {
                    self.summary.remove(&client);
                }
                let state = self.state_for(&who);
                self.send_state(client, state, summary, false);
                // It leaves (or joins) everyone's presence.
                self.soon();
            }
            ClientMsg::Follow { pane, on } => self.follow(client, pane, on),
            ClientMsg::CallJoin { session } => self.call_join(&sub, session),
            ClientMsg::CallLeave { session } => {
                if self.calls.leave(session, client) {
                    self.soon();
                }
            }
            ClientMsg::CallMute { session, muted } => {
                if self.calls.mute(session, client, muted) {
                    self.soon();
                }
            }
            ClientMsg::CallSignal { session, to, signal } => self.call_signal(&sub, session, to, signal),
            // The server hands these to `App::hands` (S33).
            ClientMsg::Hand { .. } | ClientMsg::HandReply { .. } => {}
            ClientMsg::Focus { pane } => {
                let before = self.focus.get(&client).copied();
                match pane.filter(|p| self.sees(&who, *p)) {
                    Some(p) => {
                        self.focus.insert(client, p);
                        // Seeing a finished command is enough.
                        if self.attention.get(&p) == Some(&Attention::Done) {
                            self.set_attention(p, Attention::Idle, "seen");
                        }
                    }
                    None => {
                        self.focus.remove(&client);
                    }
                }
                if self.focus.get(&client).copied() != before {
                    self.soon();
                }
            }
            ClientMsg::Pane { pane, op } => {
                if let Some(why) = self.cant(client, &[pane], Role::Editor) {
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message: why }));
                    return;
                }
                if self.control_op(client, &who, pane, &op) {
                    return;
                }
                // Blocks of other types take what applies to them.
                if self.blocks.contains_key(&pane) {
                    match op {
                        PaneOp::Attention { state } => self.set_attention(pane, state, "set by a client"),
                        PaneOp::SetPolicy { policy } => {
                            info!(pane, ?policy, "restart policy");
                            let m = self.meta.entry(pane).or_default();
                            (m.policy, m.policy_set) = (policy, true);
                        }
                        _ => {}
                    }
                    self.changed();
                    return;
                }
                let Some(handle) = self.panes.get(&pane) else {
                    let message = format!("no pane %{pane}");
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
                    return;
                };
                match op {
                    PaneOp::SetPolicy { policy } => {
                        info!(pane, ?policy, "restart policy");
                        let m = self.meta.entry(pane).or_default();
                        (m.policy, m.policy_set) = (policy, true);
                    }
                    PaneOp::Purge => {
                        info!(pane, "purging history");
                        handle.purge();
                    }
                    PaneOp::SetIntegration { on } => {
                        info!(pane, on, "shell integration");
                        self.meta.entry(pane).or_default().integration = Some(on);
                    }
                    PaneOp::Attention { state } => self.set_attention(pane, state, "set by a client"),
                    // Driving: handled before this.
                    PaneOp::TakeControl
                    | PaneOp::RequestControl
                    | PaneOp::GiveControl { .. }
                    | PaneOp::ReleaseControl
                    | PaneOp::SetPair { .. }
                    | PaneOp::RequestTrust
                    | PaneOp::GrantTrust { .. }
                    | PaneOp::RevokeTrust { .. }
                    | PaneOp::SetPrivate { .. } => {}
                }
                self.changed();
            }
        }
    }

    pub(super) fn intent(&mut self, client: Option<ClientId>, intent: Intent) -> Result<(), String> {
        // A new session without a name gets one ("drifting cedar").
        let intent = match intent {
            Intent::NewSession { name: None, from_pane } => {
                let taken = |n: &str| self.mux.sessions.iter().any(|s| s.name == n);
                Intent::NewSession { name: Some(illogical_core::names::generate(seed(), taken)), from_pane }
            }
            i => i,
        };
        let refused = |why: String| {
            info!(?client, ?intent, why, "intent refused");
            why
        };
        self.check_move(&intent).map_err(refused)?;
        let local = matches!(intent, Intent::Split { local: true, .. });
        let before: Vec<SessionId> = self.mux.sessions.iter().map(|s| s.id).collect();
        let effects = self.mux.apply(intent.clone()).map_err(|e| refused(e.to_string()))?;
        for gone in before.into_iter().filter(|s| self.mux.session(*s).is_err()) {
            self.config.acl.forget_session(gone);
        }
        if self.config.sandbox_of_control && self.mux.sessions.is_empty() {
            self.config.control.sandbox_done();
        }
        info!(?client, ?intent, "intent");
        let rects = self.mux.pane_rects();
        for e in effects {
            match e {
                Effect::Spawn { pane, cwd_from, cwd } => {
                    let from_meta = cwd_from.and_then(|p| self.meta.get(&p)).and_then(|m| m.integration);
                    let integrate = from_meta.unwrap_or(true);
                    let asked = self.next_cwd.take();
                    // A directory asked for (if it exists), else the source
                    // pane's, else home.
                    let cwd = cwd
                        .map(PathBuf::from)
                        .filter(|d| d.is_dir())
                        .or_else(|| cwd_from.and_then(|p| self.panes.get(&p)?.cwd()))
                        .unwrap_or_else(|| self.config.home.clone());
                    let (cols, rows) = rects.get(&pane).map(|r| (r.cols, r.rows)).unwrap_or((80, 24));
                    // A machine made for it, else its tab's (unless asked
                    // for this host).
                    let tab = self.mux.tab_of(pane).ok();
                    let made = self.next_host.take();
                    let host = made.or_else(|| tab.filter(|_| !local).and_then(|t| self.tab_machine(t)));
                    if let Some((kind, config)) = self.next_block.take() {
                        // A machine made for it is its own.
                        if let Some(m) = made.and_then(|m| self.machines.get_mut(&m))
                            && m.owner == Owner::Pane(0)
                        {
                            m.owner = Owner::Pane(pane);
                        }
                        match self.make_block(pane, kind, config.clone(), host, None) {
                            Ok(()) => {
                                let meta = PaneMeta { host, kind, config: Some(config), ..Default::default() };
                                self.meta.insert(pane, meta);
                                self.emit(Some(pane), EventKind::Opened);
                            }
                            Err(e) => {
                                warn!(block = pane, ?kind, error = %e, "could not start block");
                                self.last_block_error = Some(e);
                                if let Some(m) = made
                                    && self.machines.get(&m).is_some_and(|x| x.owner == Owner::Pane(pane))
                                {
                                    self.machines.remove(&m);
                                }
                                let _ = self.mux.apply(Intent::ClosePane { pane });
                            }
                        }
                        continue;
                    }
                    let (start, hold, cwd) = match self.next_spawn.take() {
                        // A command from `run`: fill in the pane id it gets.
                        Some((mut spawn, Some(text))) => {
                            spawn.env.retain(|(k, _)| k != "ILLOGICAL_PANE" && k != "ILLOGICAL_EXEC");
                            spawn.env.push(("ILLOGICAL_PANE".into(), pane.to_string()));
                            if host.is_some() {
                                spawn.env.push(("ILLOGICAL_EXEC".into(), exec_tag(&self.config.daemon_id, pane)));
                            }
                            let cwd = spawn.cwd.clone();
                            (Start::Run { spawn, text }, true, cwd)
                        }
                        _ => {
                            let cwd = match (host, asked.clone()) {
                                (None, Some(c)) => c,
                                _ => cwd,
                            };
                            let shell = match host {
                                Some(_) => self.config.guest_shell(pane, integrate, asked),
                                None => self.config.shell(pane, cwd.clone(), integrate),
                            };
                            (Start::Now(shell), false, cwd)
                        }
                    };
                    if let Some(m) = made.and_then(|m| self.machines.get_mut(&m)) {
                        m.owner = match (self.next_owner_tab, tab) {
                            (true, Some(t)) => Owner::Tab(t),
                            _ => Owner::Pane(pane),
                        };
                    }
                    match self.open_pane(pane, cols, rows, false, start, cwd, integrate, hold, host) {
                        Ok(()) => {
                            let meta = PaneMeta { integration: from_meta, host, hold, ..Default::default() };
                            self.meta.insert(pane, meta);
                            self.emit(Some(pane), EventKind::Opened);
                        }
                        Err(e) => {
                            warn!(pane, error = %e, "could not start pane");
                            if let Some(m) = made {
                                self.machines.remove(&m);
                            }
                            let _ = self.mux.apply(Intent::ClosePane { pane });
                        }
                    }
                }
                Effect::Kill { pane } => {
                    self.ids.lock().unwrap().remove(&pane);
                    if let Some(p) = self.panes.remove(&pane) {
                        p.close();
                        #[cfg(unix)]
                        crate::upload::forget(pane);
                    }
                    if let Some(b) = self.blocks.remove(&pane) {
                        b.close();
                        // Its history is kept like a closed pane's.
                        if let Ok(log) = PaneLog::open(self.store.pane_dir(pane)) {
                            log.retire(pane);
                        }
                    }
                    // A pane's own machine goes with it (a tab's, with the tab).
                    if let Some(m) = self.machine_of(pane).filter(|m| m.owner == Owner::Pane(pane)) {
                        self.delete_machine(m.id);
                    } else if let (Some(m), Some(p)) = (self.machine_of(pane), &self.config.provider) {
                        // M70: a machine that stays keeps nothing of the pane's.
                        #[cfg(unix)]
                        crate::upload::forget_on(p.clone(), m.sprite.clone(), pane);
                        #[cfg(not(unix))]
                        let _ = (m, p);
                    }
                    self.sizes.remove(&pane);
                    self.meta.remove(&pane);
                    self.attention.remove(&pane);
                    self.watching.remove(&pane);
                    self.unread.remove(&pane);
                    self.screen.remove(&pane);
                    self.turn_typed.remove(&pane);
                    if let Some(a) = self.asks.remove(&pane) {
                        let _ = a.reply.send((AskReply::Withdrawn, None));
                    }
                    self.emit(Some(pane), EventKind::Closed);
                }
            }
        }
        self.reap_machines();
        self.changed();
        self.emit(None, EventKind::Layout { rev: self.mux.rev });
        Ok(())
    }

    /// Resize panes whose cells changed, tell every client, and save soon.
    pub(super) fn changed(&mut self) {
        for (pane, r) in self.mux.pane_rects() {
            let size = (r.cols, r.rows);
            if self.sizes.get(&pane) != Some(&size)
                && let Some(b) = self.blocks.get(&pane)
            {
                b.resize(r.cols, r.rows);
                self.sizes.insert(pane, size);
            }
            if self.sizes.get(&pane) != Some(&size)
                && let Some(p) = self.panes.get(&pane)
            {
                p.resize(r.cols, r.rows);
                self.sizes.insert(pane, size);
            }
        }
        self.broadcast();
        self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
    }

    /// Something changed: every client hears what, shortly (M23). Layout
    /// changes go as a whole `State`; anything else as a `Delta` of the
    /// fields that changed. Where it's one pane, [`Self::touch`] is cheaper.
    pub(super) fn broadcast(&mut self) {
        self.dirty = Dirty::All;
        self.soon();
    }

    /// One pane's details changed (attention, a question, its driver).
    pub(super) fn touch(&mut self, pane: PaneId) {
        self.procs.borrow_mut().remove(&pane);
        self.mark(pane);
        self.soon();
    }

    /// One pane's details changed, but nobody needs it before the next
    /// tick (its directory).
    pub(super) fn mark(&mut self, pane: PaneId) {
        match &mut self.dirty {
            Dirty::All => {}
            Dirty::Panes(s) => {
                s.insert(pane);
            }
            Dirty::Clean => self.dirty = Dirty::Panes([pane].into()),
        }
    }

    pub(super) fn soon(&mut self) {
        let at = Instant::now() + URGENT;
        if self.flush_due.is_none_or(|d| d > at) {
            self.flush_due = Some(at);
        }
    }

    /// Work out each pane's output rate from its byte count, without
    /// touching the pane's thread (a parked pane stays parked).
    pub(super) fn tick_activity(&mut self) {
        let now = Instant::now();
        let secs = now.duration_since(self.last_tick).as_secs_f64().max(0.001);
        self.last_tick = now;
        let mut changed = vec![];
        for (id, h) in &self.panes {
            let (end, last_ms) = h.output_seen();
            let (prev, old) = self.activity.get(id).copied().unwrap_or((end, Activity::default()));
            let bps = (end.saturating_sub(prev) as f64 / secs).round().min(u32::MAX as f64) as u32;
            let next = Activity { bps, last_ms };
            self.activity.insert(*id, (end, next));
            if next != old {
                changed.push(*id);
            }
        }
        self.activity.retain(|id, _| self.panes.contains_key(id));
        for id in changed {
            self.mark(id);
        }
    }

    /// An ssh guest's window (M65) sizes the pane's tab, zoomed to it.
    pub(super) fn guest_view(&mut self, client: ClientId, pane: PaneId, (cols, rows): (u16, u16)) {
        let Ok(tab) = self.mux.tab_of(pane) else { return };
        if let Ok(true) = self.mux.view(client, tab, cols, rows, Some(pane), true) {
            self.changed();
        }
    }

    /// `who` drives `pane` from now (M13); they haven't typed in it yet.
    pub(super) fn drive(&mut self, pane: PaneId, who: Driver) -> Option<Driver> {
        self.drove.insert(pane, Instant::now());
        self.typing.remove(&pane);
        self.drivers.insert(pane, who)
    }

    /// Its driver typed in `pane` (#118).
    pub(super) fn typed(&mut self, pane: PaneId) {
        self.drove.insert(pane, Instant::now());
        if self.typing.insert(pane) {
            self.touch(pane);
        }
    }

    /// Typing stops showing a few seconds after the last keystroke, and a
    /// driver who has stopped typing for long lets go (#118).
    /// A new inventory (#145): which panes' screens to read may change.
    pub(super) fn tick_inventory(&mut self) {
        let generation = self.inventory.generation();
        if generation == self.inventory_seen {
            return;
        }
        self.inventory_seen = generation;
        let panes: Vec<PaneId> = self.panes.keys().copied().collect();
        for pane in panes {
            self.watch_agent(pane);
        }
    }

    pub(super) fn tick_drivers(&mut self) {
        let now = Instant::now();
        self.drove.retain(|p, _| self.drivers.contains_key(p));
        self.typing.retain(|p| self.drivers.contains_key(p));
        let panes: Vec<PaneId> = self.drivers.keys().copied().collect();
        for pane in panes {
            let idle = now.duration_since(*self.drove.entry(pane).or_insert(now));
            if idle >= self.lapse {
                if let Some(d) = self.drivers.remove(&pane) {
                    info!(pane, who = d.who, "stopped driving: no typing for {}s", idle.as_secs());
                }
                self.drove.remove(&pane);
                self.typing.remove(&pane);
                self.touch(pane);
            } else if idle >= TYPING && self.typing.remove(&pane) {
                self.mark(pane);
            }
        }
    }

    /// Send every client what changed since it was last sent anything.
    pub(super) fn flush(&mut self) {
        self.flush_due = None;
        let dirty = std::mem::take(&mut self.dirty);
        let full = std::mem::take(&mut self.full);
        if self.clients.is_empty() {
            return;
        }
        let ids: Vec<PaneId> = match dirty {
            Dirty::Clean => vec![],
            Dirty::Panes(s) => s.into_iter().collect(),
            Dirty::All => self.panes.keys().chain(self.blocks.keys()).copied().collect(),
        };
        let infos: HashMap<PaneId, PaneInfo> =
            ids.iter().filter_map(|id| Some((*id, self.info_of_any(*id)?))).collect();
        let clients: Vec<(ClientId, Principal)> =
            self.clients.values().map(|c| (c.client, c.principal.clone())).collect();
        // Most clients are one person's: work each view out once.
        let mut states: HashMap<Principal, State> = HashMap::new();
        let mut views: HashMap<(Principal, bool), PaneView> = HashMap::new();
        let mut people: HashMap<Principal, People> = HashMap::new();
        for (client, who) in clients {
            let summary = self.summary.contains(&client);
            let stale = full || self.sent.get(&client).is_none_or(|s| s.rev != self.mux.rev);
            if stale {
                let state = states.entry(who.clone()).or_insert_with(|| self.state_for(&who)).clone();
                self.send_state(client, state, summary, false);
                continue;
            }
            let view = views.entry((who.clone(), summary)).or_insert_with(|| {
                ids.iter()
                    .map(|id| {
                        let v = infos
                            .get(id)
                            .filter(|_| self.sees(&who, *id))
                            .map(|i| self.pane_value(&who, i.clone(), summary));
                        (*id, v)
                    })
                    .collect()
            });
            let (machines, presence, threads, calls) = people.entry(who.clone()).or_insert_with(|| {
                (self.machines_for(&who), self.presence(&who), self.threads_for(&who), self.calls_for(&who))
            });
            let Some(sent) = self.sent.get_mut(&client) else { continue };
            let mut delta = Delta::default();
            for (id, v) in view.iter() {
                match v {
                    Some(new) => {
                        let old = sent.panes.get(id);
                        let mut patch = serde_json::Map::new();
                        for (k, val) in new {
                            if old.and_then(|o| o.get(k)) != Some(val) {
                                patch.insert(k.clone(), val.clone());
                            }
                        }
                        for k in old.into_iter().flat_map(|o| o.keys()) {
                            if !new.contains_key(k) {
                                patch.insert(k.clone(), serde_json::Value::Null);
                            }
                        }
                        if !patch.is_empty() {
                            patch.insert("id".into(), (*id).into());
                            delta.panes.push(patch);
                            sent.panes.insert(*id, new.clone());
                        }
                    }
                    None => {
                        if sent.panes.remove(id).is_some() {
                            delta.gone.push(*id);
                        }
                    }
                }
            }
            if sent.machines != *machines {
                sent.machines = machines.clone();
                delta.machines = Some(machines.clone());
            }
            if sent.presence != *presence {
                sent.presence = presence.clone();
                delta.presence = Some(presence.clone());
            }
            if sent.threads != *threads {
                sent.threads = threads.clone();
                delta.threads = Some(threads.clone());
            }
            if sent.calls != *calls {
                sent.calls = calls.clone();
                delta.calls = Some(calls.clone());
            }
            if !delta.is_empty()
                && let Some(c) = self.clients.get(&client)
            {
                let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Delta { delta }));
            }
        }
    }

    /// A whole `State` for one client (and what it now has), or its
    /// `Hello`.
    pub(super) fn send_state(&mut self, client: ClientId, state: State, summary: bool, hello: bool) {
        let Some(c) = self.clients.get(&client) else { return };
        let who = c.principal.clone();
        let mut sent = Sent {
            rev: state.rev,
            panes: HashMap::new(),
            machines: state.machines.clone(),
            presence: state.presence.clone(),
            threads: state.threads.clone(),
            calls: state.calls.clone(),
        };
        let mut panes = Vec::with_capacity(state.panes.len());
        for p in &state.panes {
            let v = self.pane_value(&who, p.clone(), summary);
            sent.panes.insert(p.id, v.clone());
            panes.push(serde_json::Value::Object(v));
        }
        let msg = if hello {
            ServerMsg::Hello { version: env!("CARGO_PKG_VERSION").into(), client, state }
        } else {
            ServerMsg::State { state }
        };
        // Panes as this client has them (private ones blanked, summaries
        // trimmed): the same objects deltas will be worked out against.
        let mut json = serde_json::to_value(&msg).unwrap_or_default();
        json["state"]["panes"] = serde_json::Value::Array(panes);
        let _ = c.ctrl.send(ToClient::Json(json.to_string()));
        self.sent.insert(client, sent);
    }

    /// A pane as `who` gets it: someone else's private pane shows only that
    /// it's there and private (M14); a summary leaves out what the swarm
    /// doesn't need.
    fn pane_value(
        &self,
        who: &Principal,
        mut info: PaneInfo,
        summary: bool,
    ) -> serde_json::Map<String, serde_json::Value> {
        if info.private && !who.is_owner() {
            info = PaneInfo {
                cwd: None,
                command: None,
                current: None,
                last: None,
                ask: None,
                reason: None,
                answered: None,
                work: None,
                project: None,
                activity: None,
                title: None,
                file: None,
                editor: None,
                diff: None,
                resumes: None,
                ..info
            };
        }
        let serde_json::Value::Object(mut m) = serde_json::to_value(&info).unwrap_or_default() else {
            return Default::default();
        };
        if summary {
            for k in NOT_IN_SUMMARIES {
                m.remove(*k);
            }
        }
        m
    }

    /// A guest's intent that makes a pane: on a VM, never this machine
    /// (M14). A new tab is a VM tab; a split joins its tab's machine, or
    /// gets one of its own. Their VMs count against their quota.
    fn guest_intent(&mut self, client: ClientId, who: &Principal, intent: Intent) -> Result<(), String> {
        let vm = match &intent {
            Intent::NewTab { .. } => Some(true),
            Intent::Split { pane, .. } => {
                let tab = self.mux.tab_of(*pane).map_err(|e| e.to_string())?;
                if self.tab_machine(tab).is_some() { None } else { Some(false) }
            }
            _ => None,
        };
        let intent = match intent {
            Intent::Split { pane, edge, cwd, .. } => Intent::Split { pane, edge, local: false, cwd },
            i => i,
        };
        let Some(tab) = vm else { return self.intent(Some(client), intent) };
        let mine = self.machines.values().filter(|m| m.by.as_deref() == Some(who.id())).count();
        if mine >= self.config.guest_machines {
            return Err(format!("you have {mine} VMs here, the most a guest may have: close one first",));
        }
        let m = self.new_machine(None)?;
        if let Some(machine) = self.machines.get_mut(&m) {
            machine.by = Some(who.id().to_owned());
        }
        self.next_host = Some(m);
        self.next_owner_tab = tab;
        let r = self.intent(Some(client), intent);
        self.next_owner_tab = false;
        if let Some(m) = self.next_host.take() {
            self.machines.remove(&m);
        }
        r
    }

    pub(super) fn presence(&self, viewer: &Principal) -> Vec<Presence> {
        let mut out: Vec<Presence> = self
            .clients
            .values()
            // A summaries-only connection (the fleet, M25) looks at nothing.
            .filter(|c| !self.summary.contains(&c.client))
            .map(|c| Presence {
                client: c.client,
                who: c.principal.id().to_owned(),
                name: c.name.clone().unwrap_or_else(|| self.name_of(&c.principal)),
                pic: match &c.principal {
                    Principal::User { pic, .. } => pic.clone(),
                    Principal::Owner => self.config.owner_pic.clone(),
                },
                tab: self.viewing.get(&c.client).copied(),
                pane: self.focus.get(&c.client).copied(),
            })
            .filter(|p| {
                viewer.is_owner()
                    || p.who == viewer.id()
                    || p.tab
                        .and_then(|t| self.mux.session_of_tab(t).ok())
                        .and_then(|s| self.config.acl.role(viewer, s))
                        .is_some()
            })
            .map(|mut p| {
                // Only where the viewer can see.
                if !viewer.is_owner() {
                    p.pane = p.pane.filter(|x| self.sees(viewer, *x));
                    p.tab = p.tab.filter(|t| {
                        self.mux.session_of_tab(*t).ok().and_then(|s| self.config.acl.role(viewer, s)).is_some()
                    });
                }
                p
            })
            .collect();
        out.sort_by_key(|p| p.client);
        out
    }

    pub(super) fn tell_once(&mut self, client: ClientId, message: String) {
        let now = Instant::now();
        if self.refused.get(&client).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(5)) {
            return;
        }
        self.refused.insert(client, now);
        if let Some(sub) = self.clients.get(&client) {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
        }
    }

    /// Grants changed: hang up on whoever has nothing left, let go of panes
    /// they no longer see, and show everyone their state.
    pub(super) fn acl_changed(&mut self) {
        let acl = self.config.acl.clone();
        let gone: Vec<ClientId> =
            self.clients.iter().filter(|(_, c)| !acl.knows(&c.principal)).map(|(id, _)| *id).collect();
        for id in gone {
            if let Some(sub) = self.clients.remove(&id) {
                info!(client = id, who = sub.principal.id(), "access revoked: disconnecting");
                let message = "your access was removed".to_owned();
                let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
                let _ = sub.ctrl.send(ToClient::Close);
            }
            self.focus.remove(&id);
            for p in self.panes.values() {
                p.detach(id);
            }
            self.mux.release(id);
        }
        let hidden: Vec<(ClientId, PaneId)> = self
            .clients
            .values()
            .flat_map(|c| self.panes.keys().filter(|p| !self.sees(&c.principal, **p)).map(|p| (c.client, *p)))
            .collect();
        for (client, pane) in hidden {
            if let Some(p) = self.panes.get(&pane) {
                p.detach(client);
            }
        }
        // Whoever lost the session leaves its huddle (and whoever was
        // disconnected, every huddle).
        let who: HashMap<ClientId, Principal> = self.clients.iter().map(|(id, c)| (*id, c.principal.clone())).collect();
        self.calls.retain(|s, m| {
            who.get(&m.client)
                .is_some_and(|p| p.is_owner() || (!p.id().starts_with("link:") && acl.role(p, s).is_some()))
        });
        // Sessions and roles changed with them: everyone starts over.
        self.full = true;
        self.broadcast();
    }

    /// What `who` sees: everything for the owner; for anyone else only the
    /// sessions granted to them, their tabs, panes and machines, and their
    /// role in each.
    pub(super) fn state_for(&self, who: &Principal) -> State {
        let mut st = self.state();
        st.presence = self.presence(who);
        st.threads = self.threads_for(who);
        st.calls = self.calls_for(who);
        if who.is_owner() {
            return st;
        }
        // A grant per session, or a team role on all of them (M19).
        let roles: BTreeMap<SessionId, Role> =
            st.sessions.iter().filter_map(|s| Some((s.id, self.config.acl.role(who, s.id)?))).collect();
        st.sessions.retain(|s| roles.contains_key(&s.id));
        let tabs: std::collections::HashSet<TabId> = st.sessions.iter().flat_map(|s| s.tabs.iter().copied()).collect();
        st.tabs.retain(|t| tabs.contains(&t.id));
        let panes: std::collections::HashSet<PaneId> = st.tabs.iter().flat_map(|t| t.root.panes()).collect();
        // Editors that joined (M28) are in no tab: by team role.
        let editors = self.presence_role(who).is_some();
        st.panes.retain(|p| panes.contains(&p.id) || (editors && self.is_presence(p.id)));
        let machines: std::collections::HashSet<MachineId> = st.panes.iter().filter_map(|p| p.host).collect();
        st.machines.retain(|m| machines.contains(&m.id));
        st.roles = Some(roles.into_iter().collect());
        st
    }
}
