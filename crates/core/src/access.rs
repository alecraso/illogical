//! Who may do what (M12): roles per session, and the one function that
//! says what an intent needs. The daemon enforces it on every path (the
//! WebSocket, the API, the channel); this module only decides.
//!
//! The unit of sharing is the session: its tabs, panes and blocks inherit
//! its grants. The daemon's owner is owner of every session.

use serde::{Deserialize, Serialize};

use crate::{Error, Intent, Mux, OptionScope, SessionId};

/// Ordered: an owner can do anything an editor can, and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Role {
    /// Watch, scroll, select, copy, capture, tail.
    Viewer,
    /// Also: create, close and arrange blocks; drive panes; approve agents.
    Editor,
    /// Also: rename and close the session, share it.
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }
}

/// What an action needs: `role` on every one of `sessions`. With no
/// sessions, it needs the daemon's owner (a new session, a global option).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub sessions: Vec<SessionId>,
    pub role: Role,
}

impl Need {
    fn on(role: Role, sessions: Vec<SessionId>) -> Self {
        Self { sessions, role }
    }

    /// Only the daemon's owner.
    pub fn owner() -> Self {
        Self { sessions: vec![], role: Role::Owner }
    }

    /// Whether someone with `role_in` (their role per session, `None`
    /// where they have none) may.
    pub fn allowed(&self, role_in: impl Fn(SessionId) -> Option<Role>) -> bool {
        !self.sessions.is_empty() && self.sessions.iter().all(|s| role_in(*s).is_some_and(|r| r >= self.role))
    }
}

/// What `intent` needs, against the layout as it is now.
pub fn need(mux: &Mux, intent: &Intent) -> Result<Need, Error> {
    use Role::*;
    let of_pane = |p| mux.tab_of(p).and_then(|t| mux.session_of_tab(t));
    let of_tab = |t| mux.session_of_tab(t);
    let of_split =
        |n| mux.tabs.values().find(|t| t.root.has_split(n)).map(|t| t.id).ok_or(Error::NoSplit(n)).and_then(of_tab);
    Ok(match intent {
        Intent::NewSession { .. } => Need::owner(),
        Intent::RenameSession { session, .. } | Intent::CloseSession { session } => {
            mux.session(*session)?;
            Need::on(Owner, vec![*session])
        }
        Intent::NewTab { session, from_pane, .. } => {
            mux.session(*session)?;
            let mut s = vec![*session];
            // Its working directory comes from `from_pane`: one you can see.
            if let Some(p) = from_pane {
                s.push(of_pane(*p)?);
            }
            Need::on(Editor, s)
        }
        Intent::RenameTab { tab, .. } | Intent::CloseTab { tab } => Need::on(Editor, vec![of_tab(*tab)?]),
        Intent::MoveTab { tab, session, .. } => {
            mux.session(*session)?;
            Need::on(Editor, vec![of_tab(*tab)?, *session])
        }
        Intent::Split { pane, .. } | Intent::ClosePane { pane } => Need::on(Editor, vec![of_pane(*pane)?]),
        Intent::MovePane { pane, target, .. } => Need::on(Editor, vec![of_pane(*pane)?, of_pane(*target)?]),
        Intent::BreakPane { pane, session, .. } => {
            mux.session(*session)?;
            Need::on(Editor, vec![of_pane(*pane)?, *session])
        }
        Intent::DockTab { tab, target, .. } => Need::on(Editor, vec![of_tab(*tab)?, of_pane(*target)?]),
        Intent::ResizeSplit { split, .. } => Need::on(Editor, vec![of_split(*split)?]),
        Intent::SetOption { scope, .. } => match scope {
            OptionScope::Global => Need::owner(),
            OptionScope::Session(s) => {
                mux.session(*s)?;
                Need::on(Editor, vec![*s])
            }
            OptionScope::Tab(t) => Need::on(Editor, vec![of_tab(*t)?]),
            OptionScope::Pane(p) => Need::on(Editor, vec![of_pane(*p)?]),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::Edge;

    fn two_sessions() -> (Mux, SessionId, SessionId) {
        let mut m = Mux::new();
        m.apply(Intent::NewSession { name: Some("a".into()), from_pane: None }).unwrap();
        m.apply(Intent::NewSession { name: Some("b".into()), from_pane: None }).unwrap();
        let (a, b) = (m.sessions[0].id, m.sessions[1].id);
        (m, a, b)
    }

    #[test]
    fn roles_per_session() {
        let (m, a, b) = two_sessions();
        let pane_a = m.tab(m.sessions[0].tabs[0]).unwrap().root.panes()[0];
        let pane_b = m.tab(m.sessions[1].tabs[0]).unwrap().root.panes()[0];
        let grants: HashMap<SessionId, Role> = [(a, Role::Editor), (b, Role::Viewer)].into();
        let may = |i: Intent| need(&m, &i).unwrap().allowed(|s| grants.get(&s).copied());

        assert!(may(Intent::Split { pane: pane_a, edge: Edge::Right, local: false, cwd: None }));
        assert!(!may(Intent::Split { pane: pane_b, edge: Edge::Right, local: false, cwd: None }));
        // Moving a pane from where you edit to where you only watch.
        assert!(!may(Intent::MovePane { pane: pane_a, target: pane_b, edge: Edge::Left }));
        // Editors don't rename or close the session, or make new ones.
        assert!(!may(Intent::RenameSession { session: a, name: "x".into() }));
        assert!(!may(Intent::CloseSession { session: a }));
        assert!(!may(Intent::NewSession { name: None, from_pane: None }));
        assert!(!may(Intent::SetOption { scope: OptionScope::Global, name: "x".into(), value: None }));
        assert!(may(Intent::SetOption { scope: OptionScope::Pane(pane_a), name: "x".into(), value: None }));
        // A new tab can't borrow the working directory of a pane you can't see.
        let none: HashMap<SessionId, Role> = [(a, Role::Editor)].into();
        let i = Intent::NewTab { session: a, from_pane: Some(pane_b), cwd: None };
        assert!(!need(&m, &i).unwrap().allowed(|s| none.get(&s).copied()));
    }

    #[test]
    fn owners_and_strangers() {
        let (m, a, _) = two_sessions();
        let i = Intent::CloseSession { session: a };
        assert!(need(&m, &i).unwrap().allowed(|_| Some(Role::Owner)));
        assert!(!need(&m, &i).unwrap().allowed(|_| None));
        assert!(need(&m, &Intent::CloseSession { session: 999 }).is_err());
    }
}
