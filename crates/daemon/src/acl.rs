//! Principals and their grants (M12).
//!
//! Every request has an author. The **owner** is the daemon's: a local
//! process, the tailnet login that owns it, or a device of the control
//! account it joined. Anyone else is a **user**, known by a principal id
//! (`tailnet:alice@example.com`, or `account:<id>` from control) and
//! reaching only the sessions granted to them, with the role granted.
//!
//! Grants are data: `<state>/acl.json`, written atomically, and every
//! change is appended to `<state>/audit.jsonl` (who, what, when).

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::RwLock,
};

use illogical_core::{Role, SessionId};
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::store::{now_ms, write_atomic};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Principal {
    Owner,
    /// `id` as in grants; `name` to show (a login).
    User {
        id: String,
        name: String,
    },
}

impl Principal {
    pub fn tailnet(login: &str) -> Self {
        Principal::User { id: format!("tailnet:{login}"), name: login.to_owned() }
    }

    pub fn id(&self) -> &str {
        match self {
            Principal::Owner => "owner",
            Principal::User { id, .. } => id,
        }
    }

    pub fn is_owner(&self) -> bool {
        matches!(self, Principal::Owner)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub session: SessionId,
    pub principal: String,
    pub name: String,
    pub role: Role,
    pub by: String,
    pub at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    grants: Vec<Grant>,
}

#[derive(Debug)]
pub struct Acl {
    path: PathBuf,
    audit: PathBuf,
    grants: RwLock<Vec<Grant>>,
}

impl Acl {
    pub fn open(state_dir: &Path) -> Self {
        let path = state_dir.join("acl.json");
        let grants = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice::<File>(&b).map(|f| f.grants).unwrap_or_else(|e| {
                warn!(error = %e, "acl.json is unreadable; no one but the owner gets in");
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Self { path, audit: state_dir.join("audit.jsonl"), grants: RwLock::new(grants) }
    }

    /// Someone's role on a session.
    pub fn role(&self, p: &Principal, session: SessionId) -> Option<Role> {
        match p {
            Principal::Owner => Some(Role::Owner),
            Principal::User { id, .. } => {
                self.grants.read().unwrap().iter().find(|g| g.session == session && &g.principal == id).map(|g| g.role)
            }
        }
    }

    /// A user's sessions and roles (empty: none).
    pub fn roles(&self, p: &Principal) -> BTreeMap<SessionId, Role> {
        let id = p.id();
        self.grants.read().unwrap().iter().filter(|g| g.principal == id).map(|g| (g.session, g.role)).collect()
    }

    /// Whether this principal may connect at all.
    pub fn knows(&self, p: &Principal) -> bool {
        p.is_owner() || self.grants.read().unwrap().iter().any(|g| g.principal == p.id())
    }

    pub fn list(&self) -> Vec<Grant> {
        self.grants.read().unwrap().clone()
    }

    /// Grant `role` (or revoke, with `None`), as `by`.
    pub fn set(
        &self,
        session: SessionId,
        principal: &str,
        name: &str,
        role: Option<Role>,
        by: &str,
    ) -> std::io::Result<()> {
        let mut g = self.grants.write().unwrap();
        let before = g.clone();
        g.retain(|x| !(x.session == session && x.principal == principal));
        if let Some(role) = role {
            g.push(Grant {
                session,
                principal: principal.into(),
                name: name.into(),
                role,
                by: by.into(),
                at: now_ms(),
            });
        }
        if let Err(e) = write_atomic(&self.path, &serde_json::to_vec_pretty(&File { grants: g.clone() }).unwrap()) {
            *g = before;
            return Err(e);
        }
        drop(g);
        self.log(serde_json::json!({
            "at": now_ms(), "by": by, "action": if role.is_some() { "grant" } else { "revoke" },
            "session": session, "principal": principal, "name": name, "role": role,
        }));
        Ok(())
    }

    /// A session closed: its grants go with it.
    pub fn forget_session(&self, session: SessionId) {
        let gone: Vec<Grant> = self.grants.read().unwrap().iter().filter(|g| g.session == session).cloned().collect();
        for g in gone {
            if let Err(e) = self.set(session, &g.principal, &g.name, None, "session closed") {
                warn!(error = %e, "can't drop a closed session's grant");
            }
        }
    }

    fn log(&self, entry: serde_json::Value) {
        let line = format!("{entry}\n");
        let r = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = r {
            warn!(error = %e, "can't write the audit log");
        }
    }

    /// The audit log, newest last.
    pub fn audit(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&self.audit)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_persist_and_are_audited() {
        let dir = std::env::temp_dir().join(format!("illogical-acl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let alice = Principal::tailnet("alice@example.com");
        let acl = Acl::open(&dir);
        assert!(!acl.knows(&alice));
        acl.set(3, alice.id(), "alice", Some(Role::Viewer), "owner").unwrap();
        acl.set(3, alice.id(), "alice", Some(Role::Editor), "owner").unwrap();
        let again = Acl::open(&dir);
        assert_eq!(again.role(&alice, 3), Some(Role::Editor));
        assert_eq!(again.role(&alice, 4), None);
        assert_eq!(again.role(&Principal::Owner, 4), Some(Role::Owner));
        again.forget_session(3);
        assert!(!again.knows(&alice));
        let log = again.audit();
        assert_eq!(log.iter().map(|e| e["action"].as_str().unwrap()).collect::<Vec<_>>(), ["grant", "grant", "revoke"]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

// ---------------------------------------------------------------- API

pub mod api {
    use std::sync::Arc;

    use axum::{
        Json, Router,
        extract::State,
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::get,
    };
    use illogical_core::{Role, SessionId};
    use serde::Deserialize;
    use serde_json::json;

    use crate::{mux::Cmd, server::App};

    pub fn routes() -> Router<Arc<App>> {
        Router::new().route("/api/acl", get(list).post(set))
    }

    async fn list(State(app): State<Arc<App>>) -> Response {
        let audit = app.acl.audit();
        let recent = &audit[audit.len().saturating_sub(200)..];
        Json(json!({ "grants": app.acl.list(), "audit": recent })).into_response()
    }

    #[derive(Deserialize)]
    struct Set {
        session: SessionId,
        /// `tailnet:<login>` (or `account:<id>` from control).
        principal: String,
        #[serde(default)]
        name: Option<String>,
        /// `null` revokes.
        role: Option<Role>,
    }

    async fn set(State(app): State<Arc<App>>, Json(b): Json<Set>) -> Response {
        let ok_id = |rest: &str| !rest.is_empty() && rest.len() <= 200 && !rest.chars().any(char::is_control);
        let valid = b.principal.strip_prefix("tailnet:").is_some_and(ok_id)
            || b.principal.strip_prefix("account:").is_some_and(ok_id);
        if !valid {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": "principal is tailnet:<login> or account:<id>" })))
                .into_response();
        }
        let principal = b.principal.to_ascii_lowercase();
        let name = b.name.unwrap_or_else(|| principal.split_once(':').map(|(_, n)| n.to_owned()).unwrap_or_default());
        if let Err(e) = app.acl.set(b.session, &principal, &name, b.role, "owner") {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
        }
        // Takes effect at once: new state for everyone, and a hang-up for
        // whoever has nothing left.
        app.mux.send(Cmd::AclChanged);
        Json(json!({ "grants": app.acl.list() })).into_response()
    }
}
