//! Who may see and do what: the roles people and guests hold on a pane, who
//! follows whom, control operations and trust.

use super::{Cmd, Daemon, SAVE_DEBOUNCE};
use crate::{acl::Principal, pane::ToClient, store::now_ms};
use illogical_core::Role;
use illogical_proto::{ClientId, Driver, PaneId, PaneOp, ServerMsg, SessionId};
use std::time::Duration;
use tokio::time::Instant;
use tracing::info;

/// A minute of trust (M14), or in a debug build `ILLOGICAL_TRUST_MINUTE_MS`
/// (for tests).
fn trust_minute_ms() -> u64 {
    if !cfg!(debug_assertions) {
        return 60_000;
    }
    std::env::var("ILLOGICAL_TRUST_MINUTE_MS").ok().and_then(|ms| ms.parse().ok()).unwrap_or(60_000)
}

impl Daemon {
    /// The session a pane or block is in.
    pub(super) fn session_of(&self, pane: PaneId) -> Option<SessionId> {
        self.mux.tab_of(pane).and_then(|t| self.mux.session_of_tab(t)).ok()
    }

    pub(super) fn sees(&self, who: &Principal, pane: PaneId) -> bool {
        who.is_owner()
            || match self.session_of(pane) {
                Some(s) => self.config.acl.role(who, s).is_some(),
                None => self.is_presence(pane) && self.presence_role(who).is_some(),
            }
    }

    /// An editor that joined the swarm (M28): in no session.
    pub(super) fn is_presence(&self, pane: PaneId) -> bool {
        self.blocks.get(&pane).is_some_and(|b| b.detached())
    }

    /// Someone's role on the editors that joined this daemon: the owner's
    /// editors are theirs; a team's members have their team role (M19).
    pub(super) fn presence_role(&self, who: &Principal) -> Option<Role> {
        if who.is_owner() { Some(Role::Owner) } else { self.config.acl.team_role(who) }
    }

    /// Follow an editor (M28), or stop (`pane: None`: every one).
    pub(super) fn follow(&mut self, client: ClientId, pane: PaneId, on: bool) {
        if !on {
            return self.unfollow(client, Some(pane));
        }
        let Some(link) = self.blocks.get(&pane).and_then(|b| b.link()) else {
            return self.tell_once(client, format!("%{pane} isn't an editor that can be followed"));
        };
        let Some(sub) = self.clients.get(&client) else { return };
        if !self.readable(&sub.principal, pane) {
            return self.tell_once(client, format!("no pane %{pane}"));
        }
        // What it shows now, then what it sends from here.
        for msg in link.snapshot() {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Follow { pane, msg }));
        }
        let set = self.follows.entry(pane).or_default();
        if set.insert(client) {
            info!(pane, client, "following");
            link.followers(set.len() as u32, true);
        }
    }

    pub(super) fn unfollow(&mut self, client: ClientId, pane: Option<PaneId>) {
        let ids: Vec<PaneId> = match pane {
            Some(p) => vec![p],
            None => self.follows.keys().copied().collect(),
        };
        for id in ids {
            let Some(set) = self.follows.get_mut(&id) else { continue };
            if !set.remove(&client) {
                continue;
            }
            let n = set.len() as u32;
            if n == 0 {
                self.follows.remove(&id);
            }
            if let Some(link) = self.blocks.get(&id).and_then(|b| b.link()) {
                link.followers(n, false);
            }
        }
    }

    /// Why `client` can't act on `panes` with `role` (`None`: it can).
    pub(super) fn cant(&self, client: ClientId, panes: &[PaneId], role: Role) -> Option<String> {
        let who = &self.clients.get(&client)?.principal;
        if who.is_owner() {
            return None;
        }
        for p in panes {
            let got = match self.session_of(*p) {
                Some(s) => self.config.acl.role(who, s),
                None if self.is_presence(*p) => self.presence_role(who),
                None => None,
            };
            match got {
                Some(r) if r >= role => {}
                Some(_) => return Some("you're watching this session; you can't type or change it".into()),
                None => return Some(format!("no pane %{p}")),
            }
        }
        None
    }

    pub(super) fn name_of(&self, who: &Principal) -> String {
        match who {
            Principal::Owner => self.config.owner_name.clone(),
            Principal::User { name, .. } => name.clone(),
        }
    }

    /// A person's picture, if they have one.
    pub(super) fn pic_of(&self, who: &Principal) -> Option<String> {
        match who {
            Principal::User { pic, .. } => pic.clone(),
            Principal::Owner => self.config.owner_pic.clone(),
        }
    }

    pub(super) fn driver_of(&self, who: &Principal) -> Driver {
        Driver { who: who.id().to_owned(), name: self.name_of(who) }
    }

    /// A connected client's person, by the name it came with if it has one
    /// (an owner through control, M30).
    pub(super) fn driver_for(&self, client: ClientId) -> Driver {
        match self.clients.get(&client) {
            Some(c) => Driver {
                who: c.principal.id().to_owned(),
                name: c.name.clone().unwrap_or_else(|| self.name_of(&c.principal)),
            },
            None => self.driver_of(&Principal::Owner),
        }
    }

    pub(super) fn tell(&self, who: &str, msg: ServerMsg) {
        for c in self.clients.values().filter(|c| c.principal.id() == who) {
            let _ = c.ctrl.send(ToClient::Msg(msg.clone()));
        }
    }

    /// Driving a pane (M13). True if `op` was one of these.
    pub(super) fn control_op(&mut self, client: ClientId, who: &Principal, pane: PaneId, op: &PaneOp) -> bool {
        let me = self.driver_for(client);
        let title = format!("%{pane}");
        match op {
            PaneOp::TakeControl => {
                if let Some(prev) = self.drive(pane, me.clone())
                    && prev.who != me.who
                {
                    let message = format!("{} took control of {title}", me.name);
                    self.tell(&prev.who, ServerMsg::Notice { message });
                }
                info!(pane, who = me.who, "took control");
            }
            PaneOp::RequestControl => match self.drivers.get(&pane).cloned() {
                Some(d) if d.who != me.who && self.clients.values().any(|c| c.principal.id() == d.who) => {
                    self.tell(&d.who, ServerMsg::ControlRequest { pane, who: me.who.clone(), name: me.name.clone() });
                    if let Some(c) = self.clients.get(&client) {
                        let message = format!("asked {} for control of {title}", d.name);
                        let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Notice { message }));
                    }
                    return true;
                }
                // Nobody (here) drives it: just take it.
                _ => {
                    self.drive(pane, me);
                }
            },
            PaneOp::GiveControl { to } => {
                let current = self.drivers.get(&pane).map(|d| d.who.clone());
                if current.as_deref().is_some_and(|c| c != me.who) && !who.is_owner() {
                    if let Some(c) = self.clients.get(&client) {
                        let message = "only whoever drives it can hand it over".to_owned();
                        let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
                    }
                    return true;
                }
                let Some(to_client) = self.clients.values().find(|c| c.principal.id() == to).map(|c| c.client) else {
                    return true;
                };
                let next = self.driver_for(to_client);
                let message = format!("{} handed you control of {title}", me.name);
                self.drive(pane, next);
                self.tell(to, ServerMsg::Notice { message });
            }
            PaneOp::ReleaseControl => {
                if self.drivers.get(&pane).is_some_and(|d| d.who == me.who) {
                    self.drivers.remove(&pane);
                }
            }
            PaneOp::SetPair { on } => {
                if *on {
                    self.pair.insert(pane);
                } else {
                    self.pair.remove(&pane);
                }
            }
            PaneOp::RequestTrust => {
                let msg = ServerMsg::TrustRequest { pane, who: me.who.clone(), name: me.name.clone() };
                self.tell("owner", msg);
                // The owner may only have a phone in their pocket.
                if let Some(push) = &self.push {
                    let body = format!("{} asks to drive %{pane}, which runs on this machine", me.name);
                    push.send(
                        pane,
                        "Someone asks to drive a pane",
                        &body,
                        Some(serde_json::json!({ "trust": me.who })),
                    );
                }
                let body = format!("{} asks to drive %{pane}, which runs on this machine", me.name);
                self.config.control.push(pane, "Someone asks to drive a pane", &body, None, |who| who.is_owner());
                if let Some(c) = self.clients.get(&client) {
                    let message = "asked the owner to trust you with it".to_owned();
                    let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Notice { message }));
                }
                return true;
            }
            PaneOp::GrantTrust { .. } | PaneOp::RevokeTrust { .. } if !who.is_owner() => {
                self.refuse_to(client, "only the owner trusts people with panes on this machine");
                return true;
            }
            PaneOp::GrantTrust { to, minutes } => self.trust_with(pane, to, *minutes),
            PaneOp::RevokeTrust { to } => {
                self.trust.remove(&(pane, to.clone()));
                if self.drivers.get(&pane).is_some_and(|d| &d.who == to) {
                    self.drivers.remove(&pane);
                }
            }
            PaneOp::SetPrivate { on } => {
                if !who.is_owner() {
                    self.refuse_to(client, "only the owner makes a pane private");
                    return true;
                }
                self.meta.entry(pane).or_default().private = *on;
                if *on {
                    // Whoever else watches it stops getting it.
                    let others: Vec<ClientId> =
                        self.clients.values().filter(|c| !c.principal.is_owner()).map(|c| c.client).collect();
                    if let Some(p) = self.panes.get(&pane) {
                        for c in others {
                            p.detach(c);
                        }
                    }
                }
                self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
            }
            _ => return false,
        }
        self.broadcast();
        true
    }

    /// Trust `to` with a pane on this machine for `minutes` (M14).
    pub(super) fn trust_with(&mut self, pane: PaneId, to: &str, minutes: u32) {
        let minutes = minutes.clamp(1, 24 * 60);
        let ms = u64::from(minutes) * trust_minute_ms();
        self.trust.insert((pane, to.to_owned()), now_ms() + ms);
        info!(pane, to, minutes, "trusted with a local pane");
        let message = format!("you may drive %{pane} for {minutes} minutes");
        self.tell(to, ServerMsg::Notice { message });
        let expire = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms + 50)).await;
            // Show everyone it ended.
            let _ = expire.send(Cmd::AclChanged);
        });
    }

    fn refuse_to(&self, client: ClientId, why: &str) {
        if let Some(c) = self.clients.get(&client) {
            let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message: why.to_owned() }));
        }
    }

    /// A guest may drive a pane on a machine of its own; one on this
    /// machine only while the owner trusts them with it (M14).
    pub(super) fn may_drive_here(&self, who: &Principal, pane: PaneId) -> Result<(), String> {
        if who.is_owner() || self.machine_of(pane).is_some() || self.blocks.contains_key(&pane) {
            return Ok(());
        }
        // A team's machine is the team's: its members drive it by their
        // team role, no one person's trust needed (M19).
        if self.config.acl.team_role(who).is_some_and(|r| r >= Role::Editor) {
            return Ok(());
        }
        let until = self.trust.get(&(pane, who.id().to_owned())).copied().unwrap_or(0);
        if until > now_ms() {
            return Ok(());
        }
        Err(format!(
            "%{pane} runs on {}'s own machine: ask them to trust you with it (pane menu), or work in a VM tab",
            self.config.owner_name
        ))
    }

    /// Someone other than the owner may read this pane (not private).
    pub(super) fn readable(&self, who: &Principal, pane: PaneId) -> bool {
        who.is_owner() || (self.sees(who, pane) && !self.meta.get(&pane).is_some_and(|m| m.private))
    }

    // ---- threads (M61)
}
