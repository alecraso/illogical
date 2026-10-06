//! Conversations on a pane or session: posting, reading, summaries and who
//! gets told about a mention.

use super::{Daemon, SAVE_DEBOUNCE};
use crate::{acl::Principal, pane::ToClient, store::now_ms};
use illogical_core::Role;
use illogical_proto::{
    Driver, Quote, ServerMsg, SessionId, ThreadMsg, ThreadSummary, ThreadTarget,
    api::{Unreached, UnreachedWhy},
};
use tokio::time::Instant;

/// A post to a thread (M61).
pub struct ThreadPost {
    pub target: ThreadTarget,
    pub who: crate::acl::Principal,
    /// An agent posting through MCP: its name. Access is still `who`'s.
    pub as_agent: Option<Driver>,
    pub text: String,
    pub quote: Option<Quote>,
}

/// What a post comes to: the message, whether it went to the pane's agent,
/// and the `@`s that reached no one.
pub type Posted = (ThreadMsg, bool, Vec<Unreached>);

/// Why a thread request failed: an HTTP status and what to say.
#[derive(Debug)]
pub struct ThreadError(pub u16, pub String);

impl Daemon {
    /// Whether a thread's pane or session is still here.
    fn thread_exists(&self, target: ThreadTarget) -> bool {
        match target {
            ThreadTarget::Pane(p) => self.panes.contains_key(&p) || self.blocks.contains_key(&p),
            ThreadTarget::Session(s) => self.mux.session(s).is_ok(),
        }
    }

    /// The session a thread is in, if a grant on it could open the thread:
    /// not a private pane's (its owner's alone).
    pub(super) fn thread_session(&self, target: ThreadTarget) -> Option<SessionId> {
        if !self.thread_exists(target) {
            return None;
        }
        match target {
            ThreadTarget::Pane(p) if self.meta.get(&p).is_some_and(|m| m.private) => None,
            ThreadTarget::Pane(p) => self.session_of(p),
            ThreadTarget::Session(s) => Some(s),
        }
    }

    /// `who`'s role in a thread, and the time its messages start for them:
    /// a pane's thread is read by whoever may read the pane (a private
    /// pane's only by its owner), a session's by whoever has a role in it.
    /// A "from now" share sees messages from when it was made.
    pub(super) fn thread_role(&self, who: &Principal, target: ThreadTarget) -> Option<(Role, u64)> {
        if !self.thread_exists(target) {
            return None;
        }
        if who.is_owner() {
            return Some((Role::Owner, 0));
        }
        let session = match target {
            ThreadTarget::Pane(p) => self.session_of(p).filter(|_| self.readable(who, p))?,
            ThreadTarget::Session(s) => s,
        };
        let role = self.config.acl.role(who, session)?;
        Some((role, self.config.acl.thread_floor(who, session, target).unwrap_or(0)))
    }

    pub(super) fn thread_get(&self, target: ThreadTarget, who: &Principal) -> Result<Vec<ThreadMsg>, ThreadError> {
        let (_, floor) = self.thread_role(who, target).ok_or_else(|| ThreadError(404, "no such thread".into()))?;
        Ok(self.threads.get(target).iter().filter(|m| m.at >= floor).cloned().collect())
    }

    /// The threads `who` may read that have messages, with what they
    /// haven't read.
    pub(super) fn threads_for(&self, who: &Principal) -> Vec<ThreadSummary> {
        let mut out: Vec<ThreadSummary> = self
            .threads
            .targets()
            .filter_map(|t| {
                let (_, floor) = self.thread_role(who, t)?;
                let msgs: Vec<&ThreadMsg> = self.threads.get(t).iter().filter(|m| m.at >= floor).collect();
                let last = msgs.last()?;
                let read = self.threads.read_upto(who.id(), t);
                let new: Vec<&&ThreadMsg> = msgs.iter().filter(|m| m.id > read && m.who != who.id()).collect();
                Some(ThreadSummary {
                    target: t,
                    last: last.id,
                    at: last.at,
                    unread: new.len() as u32,
                    mention: new.iter().any(|m| m.mentions.iter().any(|x| x == who.id())),
                })
            })
            .collect();
        out.sort_by_key(|t| t.target);
        out
    }

    /// Everyone a message could @mention: the owner, the team's other
    /// owners (#386), everyone shared with, team members (connected or
    /// not), and whoever is connected.
    fn mentionable(&self) -> Vec<Principal> {
        let mut out = vec![Principal::Owner];
        out.extend(self.config.control.co_owners());
        for g in self.config.acl.list() {
            out.push(Principal::User { id: g.principal.clone(), name: g.name.clone(), pic: None });
        }
        out.extend(self.config.control.team_people());
        for c in self.clients.values() {
            out.push(c.principal.clone());
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|p| seen.insert(p.id().to_owned()));
        out
    }

    pub(super) fn thread_post(&mut self, post: ThreadPost) -> Result<Posted, ThreadError> {
        let ThreadPost { target, who, as_agent, text, quote } = post;
        let (role, _) = self.thread_role(&who, target).ok_or_else(|| ThreadError(404, "no such thread".into()))?;
        if role < Role::Editor {
            return Err(ThreadError(403, "you're watching this session; you can't post in its threads".into()));
        }
        let text = text.trim().to_owned();
        if text.is_empty() && quote.is_none() {
            return Err(ThreadError(400, "nothing to post".into()));
        }
        if let Some(q) = &quote
            && !self.readable(&who, q.pane)
        {
            return Err(ThreadError(403, format!("you can't read %{}", q.pane)));
        }
        let by = as_agent.clone().unwrap_or_else(|| self.driver_of(&who));
        let tokens = crate::threads::mentions(&text);
        // A team's other owner reads every thread, as the owner does.
        let co_owner =
            |p: &Principal| p.id().strip_prefix("account:").is_some_and(|a| self.config.control.owns_here(a));
        let mentions: Vec<String> = self
            .mentionable()
            .into_iter()
            .filter(|p| p.id() != by.who)
            .filter(|p| self.thread_role(p, target).is_some() || (co_owner(p) && self.thread_exists(target)))
            .filter(|p| {
                let name = self.name_of(p);
                tokens.iter().any(|t| crate::threads::names(t, p.id(), &name))
            })
            .map(|p| p.id().to_owned())
            .collect();
        // An @agent goes to the pane's agent as a follow-up: an instruction,
        // so only from someone who may drive it (and never from an agent).
        let to_agent = as_agent.is_none()
            && crate::threads::calls_agent(&tokens)
            && matches!(target, ThreadTarget::Pane(p) if self.may_drive_here(&who, p).is_ok());
        // Which tokens went anywhere; the rest are the poster's to hear about.
        let me = self.name_of(&who);
        let (mut landed, mut unreached) = (Vec::new(), Vec::new());
        for t in &tokens {
            let person = self
                .mentionable()
                .into_iter()
                .any(|p| mentions.iter().any(|m| m == p.id()) && crate::threads::names(t, p.id(), &self.name_of(&p)));
            if person || (to_agent && crate::threads::calls_agent(std::slice::from_ref(t))) {
                landed.push(t.clone());
            } else if crate::threads::names(t, who.id(), &me) {
                // Yourself: nothing to say.
            } else if crate::threads::calls_agent(std::slice::from_ref(t)) {
                let why = if matches!(target, ThreadTarget::Pane(_)) {
                    UnreachedWhy::MayNotDrive
                } else {
                    UnreachedWhy::AgentNeedsPane
                };
                unreached.push(Unreached { token: t.clone(), why });
            } else {
                unreached.push(Unreached { token: t.clone(), why: UnreachedWhy::Nobody });
            }
        }
        let msg = ThreadMsg {
            id: 0,
            at: now_ms(),
            who: by.who.clone(),
            name: by.name.clone(),
            pic: if as_agent.is_some() { None } else { self.pic_of(&who) },
            text,
            quote,
            mentions,
            landed,
            to_agent,
            agent: as_agent.is_some(),
        };
        let msg = self.threads.post(target, msg).map_err(|e| ThreadError(500, format!("can't save it: {e}")))?;
        // What you post, you've read.
        self.threads.mark_read(&by.who, target, msg.id);
        self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
        for c in self.clients.values() {
            if self.thread_role(&c.principal, target).is_some() {
                let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Thread { target, msg: msg.clone() }));
            }
        }
        self.soon();
        self.notify_mentions(target, &msg);
        Ok((msg, to_agent, unreached))
    }

    /// Tell everyone a message mentions, on their phones too.
    fn notify_mentions(&self, target: ThreadTarget, msg: &ThreadMsg) {
        if msg.mentions.is_empty() {
            return;
        }
        // A notification opens a pane: the thread's, or the session's first.
        let pane = match target {
            ThreadTarget::Pane(p) => Some(p),
            ThreadTarget::Session(s) => self
                .mux
                .session(s)
                .ok()
                .and_then(|s| s.tabs.first().copied())
                .and_then(|t| self.mux.tab(t).ok())
                .and_then(|t| t.root.panes().into_iter().next()),
        };
        let Some(pane) = pane else { return };
        let title = format!("{} mentioned you", msg.name);
        let body: String = msg.text.chars().take(200).collect();
        // Its own notification, not the pane's.
        let extra = serde_json::json!({ "thread": target.key(), "tag": format!("thread-{}", target.key()) });
        for id in &msg.mentions {
            let id = id.clone();
            if let Some(push) = &self.push {
                let id = id.clone();
                push.send_to(pane, &title, &body, Some(extra.clone()), move |w| w == id);
            }
            self.config.control.push(pane, &title, &body, Some(extra.clone()), move |p| p.id() == id);
        }
    }
}
