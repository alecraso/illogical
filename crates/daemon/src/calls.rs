//! Huddles (M63): a voice call on a session, peer to peer between its
//! members. The daemon only keeps who's in each one and passes WebRTC
//! descriptions between them; audio never comes through here. Calls live
//! in memory: a daemon restart ends them, and members join again.
//!
//! Who may join, and when someone has to leave, is the mux's call.

use std::collections::BTreeMap;

use illogical_proto::{CALL_MAX, Call, CallMember, ClientId, SessionId};

#[derive(Default)]
pub struct Calls {
    calls: BTreeMap<SessionId, Call>,
}

impl Calls {
    pub fn all(&self) -> impl Iterator<Item = &Call> {
        self.calls.values()
    }

    #[cfg(test)]
    pub fn get(&self, session: SessionId) -> Option<&Call> {
        self.calls.get(&session)
    }

    /// `client`'s membership of the huddle on `session`, if it's in it.
    pub fn member(&self, session: SessionId, client: ClientId) -> Option<&CallMember> {
        self.calls.get(&session)?.members.iter().find(|m| m.client == client)
    }

    /// Add `member` to the huddle on `session`, starting one if needed.
    /// `Ok(false)`: it was already in.
    pub fn join(&mut self, session: SessionId, member: CallMember) -> Result<bool, String> {
        let call = self.calls.entry(session).or_insert_with(|| Call {
            session,
            id: new_id(),
            started: member.joined,
            members: Vec::new(),
        });
        if call.members.iter().any(|m| m.client == member.client) {
            return Ok(false);
        }
        if call.members.len() >= CALL_MAX {
            return Err(format!("this huddle is full ({CALL_MAX} people)"));
        }
        call.members.push(member);
        Ok(true)
    }

    /// Whether anything changed.
    pub fn leave(&mut self, session: SessionId, client: ClientId) -> bool {
        self.retain(|s, m| s != session || m.client != client)
    }

    /// Take `client` out of every huddle (it disconnected).
    pub fn leave_all(&mut self, client: ClientId) -> bool {
        self.retain(|_, m| m.client != client)
    }

    pub fn mute(&mut self, session: SessionId, client: ClientId, muted: bool) -> bool {
        let Some(m) = self.calls.get_mut(&session).and_then(|c| c.members.iter_mut().find(|m| m.client == client))
        else {
            return false;
        };
        let changed = m.muted != muted;
        m.muted = muted;
        changed
    }

    /// Keep the members `keep` says may stay; a huddle with nobody left
    /// ends. Whether anything changed.
    pub fn retain(&mut self, mut keep: impl FnMut(SessionId, &CallMember) -> bool) -> bool {
        let mut changed = false;
        for (s, call) in self.calls.iter_mut() {
            let before = call.members.len();
            call.members.retain(|m| keep(*s, m));
            changed |= call.members.len() != before;
        }
        self.calls.retain(|_, c| !c.members.is_empty());
        changed
    }
}

fn new_id() -> String {
    let mut b = [0u8; 12];
    getrandom::fill(&mut b).expect("the OS's random source");
    hex::encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(client: ClientId) -> CallMember {
        CallMember {
            client,
            who: format!("tailnet:p{client}"),
            name: format!("p{client}"),
            pic: None,
            muted: false,
            joined: 1,
            device: None,
        }
    }

    #[test]
    fn join_leave_and_end() {
        let mut c = Calls::default();
        assert_eq!(c.join(1, m(10)), Ok(true));
        let id = c.get(1).unwrap().id.clone();
        assert_eq!(c.join(1, m(10)), Ok(false));
        assert_eq!(c.join(1, m(11)), Ok(true));
        assert!(c.mute(1, 11, true));
        assert!(!c.mute(1, 11, true));
        assert!(c.member(1, 11).unwrap().muted);
        assert!(c.leave(1, 10));
        assert_eq!(c.get(1).unwrap().id, id);
        assert!(c.leave_all(11));
        assert!(c.get(1).is_none());
        // A new huddle on the same session gets a new id.
        c.join(1, m(12)).unwrap();
        assert_ne!(c.get(1).unwrap().id, id);
    }

    #[test]
    fn full() {
        let mut c = Calls::default();
        for i in 0..CALL_MAX as ClientId {
            c.join(1, m(i)).unwrap();
        }
        assert!(c.join(1, m(99)).is_err());
        assert!(c.join(2, m(99)).is_ok());
    }
}
