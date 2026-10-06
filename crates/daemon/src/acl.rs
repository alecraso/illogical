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

use illogical_core::{PaneId, Role, SessionId};
use illogical_proto::ThreadTarget;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::store::{now_ms, write_atomic};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Principal {
    Owner,
    /// `id` as in grants; `name` to show (a login); a picture, if the
    /// tailnet gave one.
    User {
        id: String,
        name: String,
        pic: Option<String>,
    },
}

impl Principal {
    pub fn tailnet(login: &str) -> Self {
        Principal::User { id: format!("tailnet:{login}"), name: login.to_owned(), pic: None }
    }

    pub fn with_pic(self, pic: Option<String>) -> Self {
        match self {
            Principal::User { id, name, .. } => Principal::User { id, name, pic },
            p => p,
        }
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
    /// A "from now" share (M13): each pane's stream offset when it was
    /// granted. Output before it is never sent: no history, no scrollback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<BTreeMap<PaneId, u64>>,
    /// For an `account:` principal (M19): the root device the owner saw
    /// for them when sharing; their devices must chain back to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// A read-only link (M19): its X25519 public key (hex), and when it
    /// stops working (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<u64>,
    /// An invite from a thread (#297): in that one thread, a "from now"
    /// share reads from here instead. Every other thread starts at `at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_from: Option<ThreadFrom>,
}

/// Where one thread starts for someone invited into it (#297): the message
/// that mentioned them, or (`from: 0`) the whole thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadFrom {
    /// `pane-N` or `session-N`.
    pub thread: String,
    /// The first message's `at` (ms); 0 for all of it.
    pub from: u64,
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
    /// A team daemon's members' roles on every session (M19), from the
    /// team's signed roster.
    team: RwLock<std::collections::HashMap<String, Role>>,
    /// Members of teams sessions were shared with (M30), by team, each with
    /// their role in it, from rosters this daemon verified.
    shared_teams: RwLock<std::collections::HashMap<String, std::collections::HashMap<String, Role>>>,
    /// Who wants "needs you" notifications about what (M29), by principal
    /// id; in `notify.json`.
    notify_path: PathBuf,
    notify: RwLock<BTreeMap<String, NotifyPref>>,
}

/// What someone other than the owner is notified about (M29): agents in
/// these sessions, or everything they may edit here ("this team's agents"
/// on a team daemon). The owner always is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyPref {
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub sessions: std::collections::BTreeSet<SessionId>,
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
        let notify_path = state_dir.join("notify.json");
        let notify = std::fs::read(&notify_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self {
            path,
            audit: state_dir.join("audit.jsonl"),
            grants: RwLock::new(grants),
            team: Default::default(),
            shared_teams: Default::default(),
            notify_path,
            notify: RwLock::new(notify),
        }
    }

    /// Someone's role on a session, by principal id (`owner`, or a grant's).
    pub fn role_of(&self, id: &str, session: SessionId) -> Option<Role> {
        if id == "owner" {
            return Some(Role::Owner);
        }
        self.role(&Principal::User { id: id.to_owned(), name: String::new(), pic: None }, session)
    }

    /// Whether `id` is told when an agent in `session` needs someone (M29):
    /// the owner always; anyone else who may edit it and opted in.
    pub fn notifies(&self, id: &str, session: Option<SessionId>) -> bool {
        if id == "owner" {
            return true;
        }
        let Some(session) = session else { return false };
        if self.role_of(id, session).is_none_or(|r| r < Role::Editor) {
            return false;
        }
        self.notify.read().unwrap().get(id).is_some_and(|p| p.all || p.sessions.contains(&session))
    }

    pub fn notify_pref(&self, id: &str) -> NotifyPref {
        self.notify.read().unwrap().get(id).cloned().unwrap_or_default()
    }

    /// Opt in or out: one session, or all of them (`session: None`).
    pub fn set_notify(&self, id: &str, session: Option<SessionId>, on: bool) -> std::io::Result<NotifyPref> {
        let mut n = self.notify.write().unwrap();
        let p = n.entry(id.to_owned()).or_default();
        match session {
            None => p.all = on,
            Some(s) if on => {
                p.sessions.insert(s);
            }
            Some(s) => {
                p.sessions.remove(&s);
            }
        }
        let out = p.clone();
        write_atomic(&self.notify_path, &serde_json::to_vec_pretty(&*n).map_err(std::io::Error::other)?)?;
        Ok(out)
    }

    /// Someone's role on a session: a grant, else their team role.
    pub fn role(&self, p: &Principal, session: SessionId) -> Option<Role> {
        match p {
            Principal::Owner => Some(Role::Owner),
            Principal::User { id, .. } => {
                let granted = self
                    .grants
                    .read()
                    .unwrap()
                    .iter()
                    .find(|g| g.session == session && &g.principal == id)
                    .map(|g| g.role);
                let team = self.team.read().unwrap().get(id).copied();
                // Shared with a team they're in: the grant's role, at most
                // their role in the team.
                let teams = self.shared_teams.read().unwrap();
                let via_team = self
                    .grants
                    .read()
                    .unwrap()
                    .iter()
                    .filter(|g| g.session == session)
                    .filter_map(|g| {
                        let in_team = teams.get(g.principal.strip_prefix("team:")?)?.get(id)?;
                        Some(g.role.min(*in_team))
                    })
                    .max();
                granted.max(team).max(via_team)
            }
        }
    }

    /// A team daemon's members' roles (from its verified roster).
    pub fn set_team_roles(&self, roles: std::collections::HashMap<String, Role>) {
        *self.team.write().unwrap() = roles;
    }

    /// Members of the teams sessions were shared with (M30).
    pub fn set_shared_teams(&self, teams: std::collections::HashMap<String, std::collections::HashMap<String, Role>>) {
        *self.shared_teams.write().unwrap() = teams;
    }

    pub fn team_role(&self, p: &Principal) -> Option<Role> {
        self.team.read().unwrap().get(p.id()).copied()
    }

    /// Until when read-only links may reach this daemon (M19).
    pub fn links_until(&self) -> Option<u64> {
        let now = now_ms();
        self.grants.read().unwrap().iter().filter_map(|g| g.key.as_ref().and(g.expires)).filter(|e| *e > now).max()
    }

    /// The read-only link whose key this is, while it works.
    pub fn link_by_key(&self, key_hex: &str) -> Option<Grant> {
        let now = now_ms();
        self.grants
            .read()
            .unwrap()
            .iter()
            .find(|g| g.key.as_deref() == Some(key_hex) && g.expires.is_some_and(|e| e > now))
            .cloned()
    }

    /// A read-only link to `session` for `key` until `expires`, from now on
    /// unless `from` says otherwise.
    pub fn add_link(
        &self,
        session: SessionId,
        key: &str,
        expires: u64,
        from: Option<BTreeMap<PaneId, u64>>,
    ) -> std::io::Result<String> {
        let id = format!("link:{}", &hex_sha(key)[..12]);
        let mut g = self.grants.write().unwrap();
        g.retain(|x| x.principal != id);
        g.push(Grant {
            session,
            principal: id.clone(),
            name: "someone with a link".into(),
            role: Role::Viewer,
            by: "owner".into(),
            at: now_ms(),
            from,
            root: None,
            key: Some(key.to_owned()),
            expires: Some(expires),
            thread_from: None,
        });
        let file = File { grants: g.clone() };
        drop(g);
        write_atomic(&self.path, &serde_json::to_vec_pretty(&file).unwrap())?;
        self.log(serde_json::json!({ "at": now_ms(), "by": "owner", "action": "link", "session": session, "principal": id, "expires": expires }));
        Ok(id)
    }

    /// Links that ran out go (true if any did).
    pub fn prune_links(&self) -> bool {
        let now = now_ms();
        let gone: Vec<Grant> =
            self.grants.read().unwrap().iter().filter(|g| g.expires.is_some_and(|e| e <= now)).cloned().collect();
        for g in &gone {
            let _ = self.set(g.session, &g.principal, &g.name, None, "expired");
        }
        !gone.is_empty()
    }

    /// For a "from now" share: where `pane`'s output may start for them.
    pub fn floor(&self, p: &Principal, session: SessionId, pane: PaneId) -> Option<u64> {
        let Principal::User { id, .. } = p else { return None };
        let g = self.grants.read().unwrap();
        let from = g.iter().find(|g| g.session == session && &g.principal == id)?.from.as_ref()?;
        // A pane made after the share has nothing from before it.
        Some(from.get(&pane).copied().unwrap_or(0))
    }

    /// For a "from now" share (M13): when it was made. Thread messages
    /// (M61) from before it aren't theirs to read either, but for the one
    /// thread they were invited into (#297): there, from its exception.
    pub fn thread_floor(&self, p: &Principal, session: SessionId, thread: ThreadTarget) -> Option<u64> {
        let Principal::User { id, .. } = p else { return None };
        let g = self.grants.read().unwrap();
        let grant = g.iter().find(|g| g.session == session && &g.principal == id)?;
        grant.from.as_ref()?;
        match &grant.thread_from {
            Some(t) if t.thread == thread.key() => Some(t.from),
            _ => Some(grant.at),
        }
    }

    /// Whether this principal may connect at all.
    pub fn knows(&self, p: &Principal) -> bool {
        if p.is_owner()
            || self.grants.read().unwrap().iter().any(|g| g.principal == p.id())
            || self.team_role(p).is_some()
        {
            return true;
        }
        // In a team something here was shared with (M30).
        let teams = self.shared_teams.read().unwrap();
        self.grants
            .read()
            .unwrap()
            .iter()
            .filter_map(|g| g.principal.strip_prefix("team:"))
            .any(|t| teams.get(t).is_some_and(|m| m.contains_key(p.id())))
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
        self.set_from(session, principal, name, role, by, None)
    }

    /// `from`: a "from now" share. Changing only the role of one keeps its
    /// floor.
    pub fn set_from(
        &self,
        session: SessionId,
        principal: &str,
        name: &str,
        role: Option<Role>,
        by: &str,
        from: Option<BTreeMap<PaneId, u64>>,
    ) -> std::io::Result<()> {
        self.set_full(session, principal, name, role, by, from, None, None)
    }

    /// With the root an `account:` principal's devices chain back to, and
    /// for a new grant from a thread's invite (#297), where that thread
    /// starts for them. A grant held keeps its own.
    #[allow(clippy::too_many_arguments)]
    pub fn set_full(
        &self,
        session: SessionId,
        principal: &str,
        name: &str,
        role: Option<Role>,
        by: &str,
        from: Option<BTreeMap<PaneId, u64>>,
        root: Option<String>,
        thread_from: Option<ThreadFrom>,
    ) -> std::io::Result<()> {
        let mut g = self.grants.write().unwrap();
        let before = g.clone();
        let old = g.iter().find(|x| x.session == session && x.principal == principal).cloned();
        let kept = old.as_ref().and_then(|x| x.from.clone());
        // A role change keeps when the grant began (it's the thread floor of
        // a "from now" share); a first grant, or one made after a revoke,
        // begins now.
        let at = match &old {
            Some(x) if from.is_none() => x.at,
            _ => now_ms(),
        };
        let thread_from = match &old {
            Some(x) => x.thread_from.clone(),
            None => thread_from,
        };
        let root = root.or_else(|| old.and_then(|x| x.root));
        g.retain(|x| !(x.session == session && x.principal == principal));
        if let Some(role) = role {
            g.push(Grant {
                session,
                principal: principal.into(),
                name: name.into(),
                role,
                by: by.into(),
                at,
                from: from.or(kept),
                root,
                key: None,
                expires: None,
                thread_from,
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

    /// Something else worth auditing (M29: who answered an agent, who sent
    /// it a follow-up).
    pub fn record(&self, entry: serde_json::Value) {
        self.log(entry);
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

    /// A role change keeps when a grant began (a "from now" share's thread
    /// floor); a grant made again after a revoke begins now.
    #[test]
    fn a_role_change_keeps_when_the_grant_began() {
        let dir = std::env::temp_dir().join(format!("illogical-acl-at-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let alice = Principal::tailnet("alice@example.com");
        let acl = Acl::open(&dir);
        let from: BTreeMap<PaneId, u64> = [(1, 5)].into();
        acl.set_from(3, alice.id(), "alice", Some(Role::Viewer), "owner", Some(from.clone())).unwrap();
        let first = acl.thread_floor(&alice, 3, ThreadTarget::Session(3)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        acl.set(3, alice.id(), "alice", Some(Role::Editor), "owner").unwrap();
        assert_eq!(acl.role(&alice, 3), Some(Role::Editor));
        assert_eq!(acl.thread_floor(&alice, 3, ThreadTarget::Session(3)), Some(first));
        assert_eq!(acl.floor(&alice, 3, 1), Some(5));
        acl.set(3, alice.id(), "alice", None, "owner").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        acl.set_from(3, alice.id(), "alice", Some(Role::Viewer), "owner", Some(from)).unwrap();
        assert!(acl.thread_floor(&alice, 3, ThreadTarget::Session(3)).unwrap() > first);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An invite from a thread (#297) opens that thread from its message
    /// and no other; a role change keeps it, a held grant isn't given one,
    /// and a share with history has no floor anywhere.
    #[test]
    fn a_thread_exception_is_that_threads_alone_and_outlives_a_role_change() {
        let dir = std::env::temp_dir().join(format!("illogical-acl-thread-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sam = Principal::tailnet("sam@example.com");
        let acl = Acl::open(&dir);
        let from: BTreeMap<PaneId, u64> = [(1, 5)].into();
        let here = ThreadFrom { thread: "pane-1".into(), from: 42 };
        acl.set_full(3, sam.id(), "sam", Some(Role::Viewer), "owner", Some(from.clone()), None, Some(here.clone()))
            .unwrap();
        let at = acl.thread_floor(&sam, 3, ThreadTarget::Session(3)).unwrap();
        assert!(at > 42);
        assert_eq!(acl.thread_floor(&sam, 3, ThreadTarget::Pane(1)), Some(42));
        assert_eq!(acl.thread_floor(&sam, 3, ThreadTarget::Pane(2)), Some(at));
        // A role change keeps it; one given to a grant held is ignored.
        let other = ThreadFrom { thread: "pane-2".into(), from: 0 };
        acl.set_full(3, sam.id(), "sam", Some(Role::Editor), "owner", None, None, Some(other.clone())).unwrap();
        assert_eq!(acl.thread_floor(&sam, 3, ThreadTarget::Pane(1)), Some(42));
        assert_eq!(acl.thread_floor(&sam, 3, ThreadTarget::Pane(2)), Some(at));
        assert_eq!(Acl::open(&dir).thread_floor(&sam, 3, ThreadTarget::Pane(1)), Some(42), "it's kept on disk");
        // With history: no floor at all.
        acl.set_full(5, sam.id(), "sam", Some(Role::Viewer), "owner", None, None, Some(other)).unwrap();
        assert_eq!(acl.thread_floor(&sam, 5, ThreadTarget::Pane(1)), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

fn hex_sha(s: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(s.as_bytes()))
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
        Router::new().route("/api/acl", get(list).post(set)).route("/api/links", axum::routing::post(link))
    }

    #[derive(Deserialize)]
    struct NewLink {
        session: SessionId,
        /// The link's X25519 public key, hex (its private half travels in
        /// the link's fragment and never reaches us).
        key: String,
        #[serde(default = "hour")]
        ttl_secs: u64,
        /// With history (default: from now on).
        #[serde(default)]
        history: bool,
    }

    fn hour() -> u64 {
        3600
    }

    /// A read-only link (M19): one session, live, until it expires.
    async fn link(State(app): State<Arc<App>>, Json(b): Json<NewLink>) -> Response {
        if b.key.len() != 64 || !b.key.bytes().all(|c| c.is_ascii_hexdigit()) {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": "key: 32 bytes of hex" }))).into_response();
        }
        let from = if b.history {
            None
        } else {
            match app.mux.api(|r| crate::mux::Api::SessionEnds(b.session, r)).await.flatten() {
                Some(ends) => Some(ends),
                None => return (StatusCode::NOT_FOUND, Json(json!({ "error": "no such session" }))).into_response(),
            }
        };
        let expires = crate::store::now_ms() + b.ttl_secs.clamp(10, 7 * 86_400) * 1000;
        match app.acl.add_link(b.session, &b.key.to_ascii_lowercase(), expires, from) {
            Ok(id) => {
                app.mux.send(Cmd::AclChanged);
                // Control lets its viewers through the relay once it knows.
                app.control.poke();
                Json(json!({ "link": id, "expires": expires })).into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
        }
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
        /// False: "from now", no history from before this (M13).
        #[serde(default = "yes")]
        history: bool,
        /// For `account:<id>` (M19): their root device, as the owner saw it
        /// (they compare its fingerprint with the person). For `team:<id>`
        /// (M30): `<founder device>.<founder's root>`, as the owner's
        /// browser pinned the team.
        #[serde(default)]
        root: Option<String>,
    }

    fn yes() -> bool {
        true
    }

    async fn set(State(app): State<Arc<App>>, Json(b): Json<Set>) -> Response {
        let ok_id = |rest: &str| !rest.is_empty() && rest.len() <= 200 && !rest.chars().any(char::is_control);
        let valid = b.principal.strip_prefix("tailnet:").is_some_and(ok_id)
            || b.principal.strip_prefix("account:").is_some_and(ok_id)
            || b.principal.strip_prefix("team:").is_some_and(ok_id);
        if !valid {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "principal is tailnet:<login>, account:<id> or team:<id>" })),
            )
                .into_response();
        }
        // Account ids are as control made them; logins aren't case-sensitive.
        let principal =
            if b.principal.starts_with("tailnet:") { b.principal.to_ascii_lowercase() } else { b.principal.clone() };
        if (principal.starts_with("account:") || principal.starts_with("team:"))
            && b.role.is_some()
            && b.root.is_none()
            && app.acl.list().iter().all(|g| g.principal != principal)
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "sharing with an account needs its root device (a team: its founder's)" })),
            )
                .into_response();
        }
        let name = b.name.unwrap_or_else(|| principal.split_once(':').map(|(_, n)| n.to_owned()).unwrap_or_default());
        let from = if b.history || b.role.is_none() {
            None
        } else {
            match app.mux.api(|r| crate::mux::Api::SessionEnds(b.session, r)).await.flatten() {
                Some(ends) => Some(ends),
                None => return (StatusCode::NOT_FOUND, Json(json!({ "error": "no such session" }))).into_response(),
            }
        };
        if let Err(e) = app.acl.set_full(b.session, &principal, &name, b.role, "owner", from, b.root, None) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
        }
        // Takes effect at once: new state for everyone, and a hang-up for
        // whoever has nothing left; someone new from control needs their
        // certificates fetched.
        app.mux.send(Cmd::AclChanged);
        app.control.poke();
        Json(json!({ "grants": app.acl.list() })).into_response()
    }
}
