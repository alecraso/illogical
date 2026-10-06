//! #234: an agent's invites, waiting for the owner, on a small block beside
//! the agent (an invite block). Nothing is shared in an agent's name: MCP's
//! `invite_person` drafts here (`draft`), and the block shows its oldest
//! waiting draft as a form card it asks on itself, never on the agent's
//! pane (an agent block asks through its own methods, and a terminal holds
//! one card, Claude Code's own).
//!
//! The card is the owner's alone, by any route: the mux refuses anyone
//! else's answer ([`super::OWNER_ONLY`]) and the HTTP routes say 403 first;
//! here it's checked again. *Invite* (with the role, note and drive trust
//! as the owner left them) runs [`super::run`] as the owner; *Decline*
//! (with a reason, if they gave one) tells the agent. A card nobody answers
//! is dropped after a day (`ILLOGICAL_INVITE_TTL_MS` in tests).
//!
//! Config `{drafter, drafts}`: whose block it is (`%N` for an agent
//! block's token, `mcp:<client>@%N` for a full caller in pane N) and its
//! drafts, the waiting ones and the last settled, so a restart keeps them.
//! Methods: `draft {who, person, name, role, note, session, session_name,
//! pane, from, started}` (by `mcp:<client>`), `drafts`, `state`.
//!
//! What the card says is what sending it does: when it's shown, the
//! session's name is the session's now, and the person's name is who this
//! machine knows by their principal id, shown beside it. A draft whose
//! pane left its session, or whose person this machine no longer knows,
//! fails instead.
//!
//! Closing it is the owner's too (the routes refuse anyone else, and every
//! agent). What it still waits for is dropped then, and its drafts are
//! kept in `invites-closed.json` in the state directory, so `read_invite`
//! still says what became of each.

use std::{
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use futures_util::future::BoxFuture;
use illogical_core::{Role, SessionId};
use illogical_proto::{
    BlockType, PaneId,
    ask::{Ask, AskKind},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{
    block::{Block, BlockCtx, Summary, no_method},
    mux::AskReply,
    store::now_ms,
};

/// How long a card waits before it's dropped (a debug build's tests may
/// say, `ILLOGICAL_INVITE_TTL_MS`).
fn ttl() -> Duration {
    let day = Duration::from_secs(24 * 3600);
    if !cfg!(debug_assertions) {
        return day;
    }
    std::env::var("ILLOGICAL_INVITE_TTL_MS").ok().and_then(|v| v.parse().ok()).map(Duration::from_millis).unwrap_or(day)
}

/// Settled drafts kept.
const SETTLED: usize = 20;
/// The longest note.
pub const NOTE_MAX: usize = 500;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Waiting,
    Sent,
    Declined,
    Dropped,
    Failed,
}

/// An agent's invite, waiting for the owner or settled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    /// Who drafted it (`mcp:claude-code`), and from which pane.
    pub by: String,
    pub from: PaneId,
    /// Who started that agent, as the card says it (`you`, or the agent
    /// that did).
    #[serde(default)]
    pub started: String,
    /// Whom, as named, and as this machine knows them.
    pub who: String,
    pub person: String,
    pub name: String,
    pub role: Role,
    pub note: String,
    pub session: SessionId,
    pub session_name: String,
    /// Where it opens.
    pub pane: PaneId,
    pub at_ms: u64,
    #[serde(default)]
    pub status: Status,
    /// Who sent or declined it, and when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_ms: Option<u64>,
    /// Why it was declined, if they said, or dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Sent: #233's grant and delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_reason: Option<String>,
    /// Failed: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    drafter: String,
    #[serde(default)]
    drafts: Vec<Draft>,
}

pub struct InviteBlock {
    ctx: BlockCtx,
    me: Weak<Self>,
    config: Mutex<Config>,
    /// The draft on the card, and its ask's token.
    asking: Mutex<Option<(String, u64)>>,
    asking_lock: tokio::sync::Mutex<()>,
    /// The draft the owner sent, going out now: off the card.
    sending: Mutex<Option<String>>,
    closed: std::sync::atomic::AtomicBool,
}

impl InviteBlock {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let config: Config = serde_json::from_value(config).map_err(|e| format!("invite config: {e}"))?;
        if config.drafter.is_empty() {
            return Err("an invite block needs its drafter".into());
        }
        let b = Arc::new_cyclic(|me| Self {
            ctx,
            me: me.clone(),
            config: Mutex::new(config),
            asking: Mutex::new(None),
            asking_lock: tokio::sync::Mutex::new(()),
            sending: Mutex::new(None),
            closed: Default::default(),
        });
        let me = b.clone();
        b.ctx.rt.spawn(async move {
            me.raise().await;
            InviteBlock::expire(Arc::downgrade(&me)).await;
        });
        Ok(b)
    }

    fn closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn draft(&self, args: Value, by: Option<String>) -> Result<Value, String> {
        let by = by.filter(|b| b.starts_with("mcp:")).ok_or("invites are drafted by MCP's invite_person")?;
        let mut d: Draft = serde_json::from_value(json!({
            "id": format!("inv-{}", hex::encode(crate::push::random::<4>())),
            "by": by, "at_ms": now_ms(),
            "from": args["from"], "who": args["who"], "person": args["person"], "name": args["name"],
            "role": args["role"], "note": args["note"], "session": args["session"],
            "session_name": args["session_name"], "pane": args["pane"], "started": args["started"],
        }))
        .map_err(|e| format!("draft: {e}"))?;
        if d.role == Role::Owner {
            return Err("an invite makes someone a viewer or an editor".into());
        }
        d.note = d.note.trim().chars().take(NOTE_MAX).collect();
        info!(pane = self.ctx.id, id = d.id, by = d.by, who = d.person, "invite drafted");
        let id = d.id.clone();
        {
            let mut c = self.config.lock().unwrap();
            let waiting = c.drafts.iter().take_while(|x| x.status == Status::Waiting).count();
            c.drafts.insert(waiting, d);
        }
        self.ctx.changed();
        if let Some(me) = self.me.upgrade() {
            self.ctx.rt.spawn(async move { me.raise().await });
        }
        Ok(json!({ "draft": id, "status": "waiting", "block": self.ctx.id }))
    }

    /// The oldest waiting draft on the card, if none is.
    fn raise(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _one = self.asking_lock.lock().await;
            if self.asking.lock().unwrap().is_some() || self.closed() {
                return;
            }
            let sending = self.sending.lock().unwrap().clone();
            let next = self
                .config
                .lock()
                .unwrap()
                .drafts
                .iter()
                .find(|d| d.status == Status::Waiting && Some(&d.id) != sending.as_ref())
                .cloned();
            let Some(d) = next else { return };
            let d = match self.as_now(d.clone()).await {
                Ok(d) => d,
                Err(why) => {
                    warn!(pane = self.ctx.id, id = d.id, error = why, "an invite can't be shown");
                    self.settle(Draft { status: Status::Failed, settled_ms: Some(now_ms()), error: Some(why), ..d });
                    self.ctx.changed();
                    drop(_one);
                    return self.raise().await;
                }
            };
            match self.ctx.ask(card(&d)).await {
                Ok((token, rx)) => {
                    *self.asking.lock().unwrap() = Some((d.id.clone(), token));
                    let Some(me) = self.me.upgrade() else { return };
                    self.ctx.rt.spawn(async move {
                        let (reply, by) = rx.await.unwrap_or((AskReply::Withdrawn, None));
                        me.answered(&d.id, token, reply, by).await;
                    });
                }
                Err(e) => warn!(pane = self.ctx.id, error = e, "can't show the invite"),
            }
        })
    }

    /// A draft as sending it would go, now: its session's name, and its
    /// person's as this machine knows them; or why it can't.
    async fn as_now(&self, d: Draft) -> Result<Draft, String> {
        let Some(app) = self.ctx.invite.get().and_then(Weak::upgrade) else { return Ok(d) };
        let session_name = match app.mux.api(|r| crate::mux::Api::InviteTo(d.session, Some(d.pane), r)).await {
            Some(r) => r?.1,
            None => return Ok(d),
        };
        match super::resolve(&app, &d.person, None) {
            Ok(p) if p.id == d.person => Ok(Draft { name: p.name, session_name, ..d }),
            _ => Err(format!("this machine doesn't know {} any more", d.person)),
        }
    }

    /// Someone answered the card.
    async fn answered(&self, id: &str, token: u64, reply: AskReply, by: Option<illogical_proto::Driver>) {
        {
            let mut asking = self.asking.lock().unwrap();
            if asking.as_ref() != Some(&(id.to_owned(), token)) {
                return;
            }
            *asking = None;
        }
        let Some(d) = self.waiting(id) else { return };
        // The owner's card: anyone else's answer (which the mux refuses
        // already) changes nothing, and it's asked again.
        let owner = by.as_ref().filter(|b| b.who == "owner");
        match (reply, owner) {
            (AskReply::Answer(content), Some(by)) if content["decline"] == true => {
                let reason = content["reason"].as_str().map(str::trim).filter(|r| !r.is_empty()).map(str::to_owned);
                info!(pane = self.ctx.id, id, "invite declined");
                self.settle(Draft {
                    status: Status::Declined,
                    settled_by: Some(by.name.clone()),
                    settled_ms: Some(now_ms()),
                    reason,
                    ..d
                });
            }
            (AskReply::Answer(content), Some(by)) => {
                *self.sending.lock().unwrap() = Some(d.id.clone());
                self.send(d, &content, by).await;
                *self.sending.lock().unwrap() = None;
            }
            (AskReply::Decline, Some(by)) => {
                info!(pane = self.ctx.id, id, "invite declined");
                self.settle(Draft {
                    status: Status::Declined,
                    settled_by: Some(by.name.clone()),
                    settled_ms: Some(now_ms()),
                    ..d
                });
            }
            (AskReply::Withdrawn | AskReply::Terminal, _) => {}
            (_, None) => warn!(pane = self.ctx.id, id, by = ?by, "an invite answered by someone not the owner"),
            _ => {}
        }
        self.ctx.changed();
        if !self.closed() {
            self.raise().await;
        }
    }

    /// The owner sent it: #233's invite, as them, with their edits.
    async fn send(&self, d: Draft, content: &Value, by: &illogical_proto::Driver) {
        let role = match content["role"].as_str() {
            Some("editor") => Role::Editor,
            Some("viewer") => Role::Viewer,
            _ => d.role,
        };
        let note = match content["note"].as_str() {
            Some(n) => n.trim().chars().take(NOTE_MAX).collect(),
            None => d.note.clone(),
        };
        let drive = content["drive_minutes"].as_u64().filter(|m| *m > 0).map(|m| m.min(u64::from(u32::MAX)) as u32);
        let req = illogical_proto::api::InviteRequest {
            session: d.session,
            who: d.person.clone(),
            role: Some(role),
            note: Some(note.clone()),
            pane: Some(d.pane),
            history: false,
            drive_minutes: drive,
            root: None,
            thread: None,
            msg: None,
            whole_thread: false,
        };
        let drafted = super::Drafted { by: d.by.clone(), pane: d.from, approved_by: by.name.clone() };
        let app = self.ctx.invite.get().and_then(Weak::upgrade);
        let out = match app {
            Some(app) => super::run(&app, req, Some(&drafted)).await.map_err(|(_, why)| why),
            None => Err("the daemon isn't serving yet".into()),
        };
        let settled = Draft { role, note, settled_by: Some(by.name.clone()), settled_ms: Some(now_ms()), ..d };
        match out {
            Ok(v) => {
                info!(pane = self.ctx.id, id = settled.id, delivery = v.delivery.as_str(), "invite sent");
                self.settle(Draft {
                    status: Status::Sent,
                    grant: serde_json::to_value(&v.grant).ok(),
                    delivery: Some(v.delivery.as_str().to_owned()),
                    delivery_reason: v.reason,
                    ..settled
                });
            }
            Err(e) => {
                warn!(pane = self.ctx.id, id = settled.id, error = e, "invite failed");
                self.settle(Draft { status: Status::Failed, error: Some(e), ..settled });
            }
        }
    }

    fn waiting(&self, id: &str) -> Option<Draft> {
        self.config.lock().unwrap().drafts.iter().find(|d| d.id == id && d.status == Status::Waiting).cloned()
    }

    /// It's settled: after the waiting ones, newest first, the last few.
    fn settle(&self, d: Draft) {
        let mut c = self.config.lock().unwrap();
        c.drafts.retain(|x| x.id != d.id);
        let waiting = c.drafts.iter().take_while(|x| x.status == Status::Waiting).count();
        c.drafts.insert(waiting, d);
        c.drafts.truncate(waiting + SETTLED);
    }

    /// Drop what nobody answered in time, while the block lives.
    async fn expire(me: Weak<Self>) {
        let ttl = ttl();
        let every = (ttl / 10).clamp(Duration::from_millis(100), Duration::from_secs(60));
        loop {
            tokio::time::sleep(every).await;
            let Some(b) = me.upgrade() else { return };
            if b.closed() {
                return;
            }
            let due = now_ms().saturating_sub(ttl.as_millis() as u64);
            let old: Vec<Draft> = {
                let c = b.config.lock().unwrap();
                let sending = b.sending.lock().unwrap().clone();
                c.drafts
                    .iter()
                    .filter(|d| d.status == Status::Waiting && d.at_ms <= due && Some(&d.id) != sending.as_ref())
                    .cloned()
                    .collect()
            };
            if old.is_empty() {
                continue;
            }
            for d in old {
                let shown = {
                    let mut asking = b.asking.lock().unwrap();
                    match asking.as_ref() {
                        Some((id, token)) if *id == d.id => {
                            let t = *token;
                            *asking = None;
                            Some(t)
                        }
                        _ => None,
                    }
                };
                if let Some(token) = shown {
                    b.ctx.withdraw(&d.id, token);
                }
                info!(pane = b.ctx.id, id = d.id, "invite dropped: nobody answered");
                b.settle(Draft {
                    status: Status::Dropped,
                    settled_ms: Some(now_ms()),
                    reason: Some("nobody answered it in time".into()),
                    ..d
                });
            }
            b.ctx.changed();
            b.raise().await;
        }
    }
}

/// Drafts of closed invite blocks kept.
const CLOSED: usize = 200;

/// A closed invite block's drafts, as `read_invite` finds them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Closed {
    pub block: PaneId,
    pub drafter: String,
    pub draft: Draft,
}

fn closed_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join("invites-closed.json")
}

/// The drafts of invite blocks closed here, newest last.
pub fn closed(root: &std::path::Path) -> Vec<Closed> {
    std::fs::read(closed_path(root)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn keep_closed(root: &std::path::Path, block: PaneId, drafter: &str, drafts: Vec<Draft>) -> std::io::Result<()> {
    if drafts.is_empty() {
        return Ok(());
    }
    let mut all = closed(root);
    all.extend(drafts.into_iter().map(|draft| Closed { block, drafter: drafter.to_owned(), draft }));
    let skip = all.len().saturating_sub(CLOSED);
    let all: Vec<Closed> = all.into_iter().skip(skip).collect();
    let tmp = closed_path(root).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&all)?)?;
    std::fs::rename(tmp, closed_path(root))
}

/// A draft as the owner's card: who wants whom where, and why; the role,
/// note and drive trust to edit, and a reason to give if they decline.
pub fn card(d: &Draft) -> Ask {
    let agent = d.by.strip_prefix("mcp:").unwrap_or(&d.by).to_owned();
    let started = if d.started.is_empty() { String::new() } else { format!(", {}", d.started) };
    let message = format!(
        "{agent} (pane %{}{started}) wants to bring {} [{}] ({}) into {} at pane %{}: {}",
        d.from,
        d.name,
        d.person,
        d.role.as_str(),
        d.session_name,
        d.pane,
        d.note
    );
    let schema = json!({
        "type": "object",
        "properties": {
            "role": { "type": "string", "title": "As", "default": d.role.as_str(),
                "oneOf": [{ "const": "viewer", "title": "Viewer", "description": "Watches" },
                          { "const": "editor", "title": "Editor", "description": "Types and answers too" }] },
            "note": { "type": "string", "title": "Note", "default": d.note, "maxLength": NOTE_MAX,
                "description": "They read it in their notification" },
            "drive_minutes": { "type": "integer", "title": "Let them type on this machine (minutes)", "minimum": 0,
                "maximum": 1440, "description": "An editor, on your own machine's pane; empty or 0: no" },
            "reason": { "type": "string", "title": "If you decline: why",
                "description": "The agent is told" },
        },
        "required": [],
    });
    Ask {
        id: d.id.clone(),
        kind: AskKind::Form,
        message,
        questions: None,
        schema: Some(schema),
        url: None,
        accepted: false,
        tool_call_id: None,
        source: "invite".into(),
        agent: Some(agent),
        at_ms: d.at_ms,
        tool: None,
        input: None,
        suggestions: None,
        session: None,
    }
}

impl Block for InviteBlock {
    fn kind(&self) -> BlockType {
        BlockType::Invite
    }

    fn config(&self) -> Value {
        serde_json::to_value(&*self.config.lock().unwrap()).unwrap_or_default()
    }

    fn state(&self) -> Value {
        let c = self.config.lock().unwrap();
        let waiting = c.drafts.iter().filter(|d| d.status == Status::Waiting).count();
        json!({ "drafter": c.drafter, "drafts": c.drafts, "waiting": waiting })
    }

    fn text(&self) -> String {
        let c = self.config.lock().unwrap();
        let mut out = String::new();
        for d in &c.drafts {
            let status = serde_json::to_value(d.status).ok().and_then(|v| v.as_str().map(str::to_owned));
            out.push_str(&format!(
                "{} {}: {} ({}) into {} at %{}, by {}: {}\n",
                d.id,
                status.unwrap_or_default(),
                d.name,
                d.role.as_str(),
                d.session_name,
                d.pane,
                d.by,
                d.note
            ));
        }
        out
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        self.call_by(method, args, None)
    }

    fn call_by(&self, method: &str, args: Value, by: Option<&str>) -> BoxFuture<'static, Result<Value, String>> {
        let out = match method {
            "draft" => self.draft(args, by.map(str::to_owned)),
            "drafts" => Ok(json!({ "drafts": self.config.lock().unwrap().drafts })),
            "state" => Ok(self.state()),
            m => Err(no_method(BlockType::Invite, m)),
        };
        Box::pin(async move { out })
    }

    fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some((id, token)) = self.asking.lock().unwrap().take() {
            self.ctx.withdraw(&id, token);
        }
        let (drafter, drafts) = {
            let mut c = self.config.lock().unwrap();
            for d in c.drafts.iter_mut().filter(|d| d.status == Status::Waiting) {
                d.status = Status::Dropped;
                d.settled_ms = Some(now_ms());
                d.reason = Some("its invite block was closed".into());
            }
            (c.drafter.clone(), c.drafts.clone())
        };
        if let Some(root) = self.ctx.dir.parent().and_then(|b| b.parent())
            && let Err(e) = keep_closed(root, self.ctx.id, &drafter, drafts)
        {
            warn!(pane = self.ctx.id, error = %e, "can't keep a closed invite block's drafts");
        }
    }

    fn summary(&self) -> Summary {
        let c = self.config.lock().unwrap();
        let waiting = c.drafts.iter().filter(|d| d.status == Status::Waiting).count();
        Summary {
            title: Some(match waiting {
                0 => "Invites".into(),
                1 => "An invite waits for you".into(),
                n => format!("{n} invites wait for you"),
            }),
            ..Summary::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_as_its_card() {
        let d: Draft = serde_json::from_value(json!({
            "id": "inv-1", "by": "mcp:claude-code", "from": 3, "who": "sam", "person": "account:s1",
            "name": "Sam", "role": "editor", "note": "the flaky test", "session": 1,
            "session_name": "api-work", "pane": 3, "at_ms": 1, "started": "you started it",
        }))
        .unwrap();
        let a = card(&d);
        assert_eq!(
            a.message,
            "claude-code (pane %3, you started it) wants to bring Sam [account:s1] (editor) into api-work at pane %3: the flaky test"
        );
        assert_eq!((a.kind, a.source.as_str()), (AskKind::Form, "invite"));
        let s = a.schema.unwrap();
        assert_eq!(s["properties"]["role"]["default"], "editor");
        assert!(
            s["properties"]["drive_minutes"].get("default").is_none(),
            "drive trust is off unless the owner sets it"
        );
    }
}
