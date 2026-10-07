//! Calls on a shared session: who may join, who is in one and the signals
//! passed between them.

use super::Daemon;
use crate::{
    acl::Principal,
    pane::{Subscriber, ToClient},
};
use arugula_core::Role;
use arugula_proto::{Call, CallMember, ClientId, ServerMsg, SessionId};
use tracing::info;

impl Daemon {
    /// Who is connected and where they look, within what `viewer` sees.
    /// `who`'s role in the session a huddle is on: anyone with one may
    /// join it (watchers, drivers, a shared session's guests), except
    /// whoever holds a read-only link, who could be anyone.
    fn call_role(&self, who: &Principal, session: SessionId) -> Option<Role> {
        self.mux.sessions.iter().find(|s| s.id == session)?;
        if who.is_owner() {
            return Some(Role::Owner);
        }
        if who.id().starts_with("link:") {
            return None;
        }
        self.config.acl.role(who, session)
    }

    /// The huddles on sessions `who` has a role in.
    pub(super) fn calls_for(&self, who: &Principal) -> Vec<Call> {
        self.calls.all().filter(|c| self.call_role(who, c.session).is_some()).cloned().collect()
    }

    pub(super) fn call_join(&mut self, sub: &Subscriber, session: SessionId) {
        let who = &sub.principal;
        let refuse = |message: String| {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
        };
        if self.call_role(who, session).is_none() {
            return refuse("no such session".into());
        }
        // Huddles on sessions that closed end with them.
        let sessions: std::collections::HashSet<SessionId> = self.mux.sessions.iter().map(|s| s.id).collect();
        self.calls.retain(|s, _| sessions.contains(&s));
        let member = CallMember {
            client: sub.client,
            who: who.id().to_owned(),
            name: sub.name.clone().unwrap_or_else(|| self.name_of(who)),
            pic: self.pic_of(who),
            muted: false,
            joined: crate::store::now_ms(),
            device: sub.device.as_ref().map(|d| d.device.clone()),
        };
        match self.calls.join(session, member) {
            Ok(true) => {
                info!(session, client = sub.client, who = who.id(), "joined a huddle");
                self.soon();
            }
            Ok(false) => {}
            Err(why) => refuse(why),
        }
    }

    /// Pass a description from one huddle member to another, with the
    /// sender's device certificate for the receiver to check it against.
    pub(super) fn call_signal(
        &mut self,
        sub: &Subscriber,
        session: SessionId,
        to: ClientId,
        signal: serde_json::Value,
    ) {
        if self.calls.member(session, sub.client).is_none() || self.calls.member(session, to).is_none() {
            tracing::debug!(session, from = sub.client, to, "dropped a signal outside a huddle");
            return;
        }
        let Some(dest) = self.clients.get(&to) else { return };
        let cert = sub.device.as_ref().and_then(|d| serde_json::to_value(d).ok());
        let msg = ServerMsg::CallSignal { session, from: sub.client, signal, cert };
        let _ = dest.ctrl.send(ToClient::Msg(msg));
    }
}
