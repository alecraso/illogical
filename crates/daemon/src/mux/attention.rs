//! What needs a person: a pane's attention state and its reasons, the IDE
//! connection and diffs, questions asked in a terminal, hooks and follow-ups.

use super::{AskReply, Daemon, InboxReply, Replied, SAVE_DEBOUNCE, agent_in, plain_reason};
use crate::{acl::Principal, store::now_ms};
use illogical_proto::{
    Action, AskRef, AskWhat, Attention, BlockType, Driver, EventKind, PaneId, Policy, Reason, ReasonKind,
    api::HistoryKind,
    ask::{Ask, AskKind},
};
use illogical_vt::detect::AgentState;
use std::collections::HashMap;
use tokio::{sync::oneshot, time::Instant};
use tracing::{info, warn};

/// An edit Claude Code proposes through its IDE connection (M28).
pub(super) struct PendingDiff {
    /// Its pane, once its Claude Code's process is found in one.
    pub(super) pane: Option<PaneId>,
    /// The relay's connection, and the call's id there.
    pub(super) conn: u64,
    pub(super) call: serde_json::Value,
    pub(super) tab: String,
    /// The file as it is, and as it would be.
    pub(super) old: String,
    pub(super) new: String,
    pub(super) info: illogical_proto::DiffInfo,
}

/// How much of a file a diff card keeps.
const MAX_DIFF_FILE: u64 = 4 << 20;
/// ...and of its diff.
const MAX_DIFF_TEXT: usize = 16 << 10;

/// A follow-up waiter.
pub(super) struct Waiter {
    pub(super) token: u64,
    pub(super) reply: oneshot::Sender<InboxReply>,
}

/// How many follow-ups wait for an agent that isn't listening yet.
const MAX_QUEUED: usize = 8;

/// A question open in a terminal, or raised on a block (M35).
pub(super) struct TermAsk {
    pub(super) ask: Ask,
    pub(super) token: u64,
    pub(super) reply: oneshot::Sender<Replied>,
}

/// Claude Code's session and subagent, from a hook's input: `session/agent`.
fn session_key(hook: &serde_json::Value) -> String {
    format!("{}/{}", hook["session_id"].as_str().unwrap_or(""), hook["agent_id"].as_str().unwrap_or(""))
}

/// A hook input about the same tool call as a permission card: same
/// session and subagent, tool and input.
fn same_call(hook: &serde_json::Value, ask: &Ask) -> bool {
    ask.session.as_deref() == Some(session_key(hook).as_str())
        && ask.tool.as_deref() == hook["tool_name"].as_str()
        && ask.input.as_ref() == Some(&hook["tool_input"])
}

/// What a notification about a reason is titled.
fn push_title(state: Attention, reason: Option<&Reason>) -> &'static str {
    match reason.map(|r| r.kind) {
        Some(ReasonKind::Failed) => "Failed",
        Some(ReasonKind::Exited) => "Exited",
        Some(ReasonKind::Done) => "Done",
        Some(ReasonKind::Ask | ReasonKind::Input) => "Needs you",
        Some(ReasonKind::Paused) => "Paused",
        Some(ReasonKind::Errors) => "Errors",
        Some(ReasonKind::Conflict) => "Merge conflict",
        Some(ReasonKind::Diff) => "Wants to edit",
        Some(ReasonKind::Gate) => "Waits at a gate",
        None if state == Attention::Done => "Done",
        None => "Needs you",
    }
}

/// "Claude Code wants to edit src/main.rs (+3 −1)".
fn diff_headline(d: &illogical_proto::DiffInfo) -> String {
    let verb = if d.new { "create" } else { "edit" };
    format!("Claude Code wants to {verb} {} (+{} −{})", d.file, d.added, d.removed)
}

/// An `openDiff` call as a diff card: the file as it is now, and as it
/// would be.
fn new_diff(
    pane: Option<PaneId>,
    conn: u64,
    call: serde_json::Value,
    args: &serde_json::Value,
    cwd: Option<&str>,
) -> PendingDiff {
    use std::io::Read;
    let path = args["new_file_path"].as_str().or(args["old_file_path"].as_str()).unwrap_or("").to_owned();
    let old_path = args["old_file_path"].as_str().unwrap_or(&path).to_owned();
    let mut old = String::new();
    let exists = std::fs::File::open(&old_path).and_then(|f| f.take(MAX_DIFF_FILE).read_to_string(&mut old)).is_ok();
    let new: String = args["new_file_contents"].as_str().unwrap_or("").to_owned();
    let (added, removed, text) = crate::ide::diff::unified(&old, &new, MAX_DIFF_TEXT);
    let file = match cwd {
        Some(c) => crate::paths::relative(c, &path),
        None => path.clone(),
    };
    let id = format!("d{conn}-{}", call.to_string().trim_matches('"'));
    PendingDiff {
        pane,
        conn,
        call,
        tab: args["tab_name"].as_str().unwrap_or("").to_owned(),
        info: illogical_proto::DiffInfo {
            id,
            file,
            added,
            removed,
            text,
            new: !exists,
            at_ms: now_ms(),
            ide: crate::ide::NAME.into(),
        },
        old,
        new,
    }
}

/// What asks bundle by: the project a directory is in (its git root, from
/// M23's cached lookup), else the directory. A directory on a machine (not
/// this host) is taken as is.
pub fn project_key(cwd: Option<&str>, local: bool) -> String {
    let Some(cwd) = cwd else { return String::new() };
    match local.then(|| crate::classify::project(cwd)).flatten() {
        Some(p) => p.root,
        None => cwd.to_owned(),
    }
}

impl Daemon {
    pub(super) fn set_attention(&mut self, pane: PaneId, state: Attention, why: &str) {
        let reason = match state {
            Attention::NeedsInput => Some(plain_reason(ReasonKind::Input, why)),
            Attention::Done => Some(plain_reason(ReasonKind::Done, why)),
            _ => None,
        };
        self.set_attention_with(pane, state, why, reason);
    }

    /// Set a pane's attention, and why (M24). An open ask overrides the
    /// stored reason while it's open (see [`Self::live_reason`]).
    pub(super) fn set_attention_with(&mut self, pane: PaneId, state: Attention, why: &str, reason: Option<Reason>) {
        let old = self.attention.get(&pane).copied().unwrap_or_default();
        if !(self.panes.contains_key(&pane) || self.blocks.contains_key(&pane)) {
            return;
        }
        if old == state {
            // Another reason for the same state (a second command done, or a
            // failure after a bell): keep the newer one if it says more.
            let newer = match (&reason, self.reasons.get(&pane)) {
                (Some(r), Some(o)) => r.kind != o.kind && r.kind != ReasonKind::Input,
                (Some(_), None) => true,
                _ => false,
            };
            if newer && let Some(r) = reason {
                self.reasons.insert(pane, r);
                self.emit(Some(pane), EventKind::Attention { state, reason: self.live_reason(pane) });
                self.broadcast();
            }
            return;
        }
        info!(pane, ?state, why, "attention");
        self.attention.insert(pane, state);
        match reason {
            Some(r) => self.reasons.insert(pane, r),
            None => self.reasons.remove(&pane),
        };
        let reason = self.live_reason(pane);
        self.emit(Some(pane), EventKind::Attention { state, reason: reason.clone() });
        if matches!(state, Attention::NeedsInput | Attention::Done) && !self.focused(pane) {
            let title = push_title(state, reason.as_ref());
            let body = reason.as_ref().map_or(why, |r| r.headline.as_str()).to_owned();
            // What lets a notification answer it from its buttons: an
            // approval (an agent block's, or a terminal's permission card,
            // M29), or a question with one or two answers.
            let mut extra = self.blocks.get(&pane).and_then(|b| b.push_extra()).or_else(|| {
                let a = &self.asks.get(&pane)?.ask;
                if a.kind == AskKind::Permission {
                    return Some(serde_json::json!({ "approve": { "id": a.id, "title": a.headline() } }));
                }
                Some(serde_json::json!({ "ask": a.push_choice()? }))
            });
            if let Some(r) = &reason {
                let x = extra.get_or_insert_with(|| serde_json::json!({}));
                x["reason"] = serde_json::json!({ "kind": r.kind, "actions": r.actions, "bundle": r.bundle });
            }
            // An agent's invite (#234) is for the owner alone, and opens at
            // its card: no buttons to send it from.
            if self.is_invite(pane) {
                if let Some(x) = extra.as_mut().and_then(|x| x.as_object_mut()) {
                    x.remove("approve");
                    x.remove("ask");
                }
                if let Some(push) = &self.push {
                    push.send_to(pane, title, &body, extra.clone(), |who| who == "owner");
                }
                self.config.control.push(pane, title, &body, extra, |who| who.is_owner());
                self.touch(pane);
                return;
            }
            // The owner, and whoever may edit the session and opted in
            // (M29): on this daemon's own push, and through control (M21),
            // where the payload is encrypted to each device, so it can carry
            // what to approve too.
            let session = self.session_of(pane);
            let acl = self.config.acl.clone();
            if let Some(push) = &self.push {
                let acl = acl.clone();
                push.send_to(pane, title, &body, extra.clone(), move |who| acl.notifies(who, session));
            }
            self.config.control.push(pane, title, &body, extra, move |who| acl.notifies(who.id(), session));
        }
        self.touch(pane);
    }

    /// Why a pane wants you now (M24): an open question or approval while it
    /// needs input, else what was recorded when its state changed.
    pub(super) fn live_reason(&self, pane: PaneId) -> Option<Reason> {
        let state = self.attention.get(&pane).copied().unwrap_or_default();
        if !matches!(state, Attention::NeedsInput | Attention::Done) {
            return None;
        }
        if state == Attention::NeedsInput
            && let Some(r) = self.diff_reason(pane).or_else(|| self.ask_reason(pane))
        {
            return Some(r);
        }
        self.reasons.get(&pane).cloned()
    }

    /// An edit waiting as a diff in a pane (M28): it comes before the
    /// hook's permission card for the same edit.
    fn diff_reason(&self, pane: PaneId) -> Option<Reason> {
        let d = &self.diffs.iter().find(|d| d.pane == Some(pane))?.info;
        Some(Reason {
            kind: ReasonKind::Diff,
            since_ms: d.at_ms,
            headline: diff_headline(d),
            command: None,
            exit: None,
            duration_ms: None,
            bundle: None,
            ask: None,
            gate: None,
            actions: vec![Action::Accept, Action::Reject, Action::Dismiss],
        })
    }

    /// The process `pid` runs in one of our terminals: which (M28: the
    /// Claude Code behind an IDE connection).
    fn pane_of_pid(&self, pid: u32) -> Option<PaneId> {
        let shells: HashMap<u32, PaneId> = self.panes.iter().filter_map(|(id, h)| Some((h.pid_now()?, *id))).collect();
        let mut p = pid;
        for _ in 0..64 {
            if let Some(id) = shells.get(&p) {
                return Some(*id);
            }
            p = crate::procinfo::ppid(p).filter(|p| *p > 1)?;
        }
        None
    }

    /// What Claude Code's IDE connections did (M28).
    pub(super) fn ide_event(&mut self, ev: crate::ide::Event) {
        use crate::ide::Event;
        let Some(ide) = self.config.ide.clone() else { return };
        match ev {
            Event::Hello => {
                // The relay says again what's open; start from nothing.
                let panes: Vec<PaneId> = self.diffs.drain(..).filter_map(|d| d.pane).collect();
                self.ide_conns.clear();
                for p in panes {
                    self.diff_changed(p);
                }
            }
            Event::Conn { conn, pid } => {
                let pane = pid.and_then(|p| self.pane_of_pid(p));
                info!(conn, ?pid, ?pane, "Claude Code connected to its IDE");
                self.ide_conns.insert(conn, (pid, pane));
                self.rehome(conn, pane);
                if let Some(p) = pane {
                    self.touch(p);
                }
            }
            Event::Gone { conn } => {
                if let Some((_, Some(p))) = self.ide_conns.remove(&conn) {
                    self.touch(p);
                }
                let gone: Vec<PaneId> = self.diffs.iter().filter(|d| d.conn == conn).filter_map(|d| d.pane).collect();
                self.diffs.retain(|d| d.conn != conn);
                for p in gone {
                    self.diff_changed(p);
                }
            }
            Event::Call { conn, id, tool, args } => match tool.as_str() {
                "openDiff" | "openDiff/here" => {
                    // Another IDE gets diffs, unless passing it on failed.
                    if tool == "openDiff"
                        && let Some(to) = ide.target()
                    {
                        return ide.forward(to, conn, id, args, self.tx.clone());
                    }
                    let pane = self.ide_conns.get(&conn).and_then(|c| c.1);
                    let cwd = pane
                        .and_then(|p| self.panes.get(&p))
                        .and_then(|h| h.status().cwd.or_else(|| h.cwd().map(|c| c.display().to_string())));
                    let d = new_diff(pane, conn, id, &args, cwd.as_deref());
                    info!(conn, ?pane, file = d.info.file, "an edit waits as a diff");
                    self.diffs.retain(|x| !(x.conn == d.conn && x.call == d.call));
                    self.diffs.push(d);
                    if let Some(p) = pane {
                        self.diff_changed(p);
                    }
                }
                "getDiagnostics" => ide.reply(conn, &id, crate::ide::no_diagnostics()),
                _ => ide.reply(conn, &id, serde_json::json!({ "content": [] })),
            },
            Event::Closed { conn, ids, .. } => {
                ide.closed(conn, &ids);
                let (gone, keep): (Vec<PendingDiff>, Vec<PendingDiff>) =
                    self.diffs.drain(..).partition(|d| d.conn == conn && ids.contains(&d.call));
                self.diffs = keep;
                for d in gone {
                    let Some(p) = d.pane else { continue };
                    // The terminal answered it (M29's "Allowed, 14:02").
                    self.answered.insert(
                        p,
                        illogical_proto::ask::Answered {
                            id: d.info.id.clone(),
                            how: "answered".into(),
                            who: "terminal".into(),
                            name: "the terminal".into(),
                            at_ms: now_ms(),
                            headline: diff_headline(&d.info),
                        },
                    );
                    self.diff_changed(p);
                }
            }
        }
    }

    /// The pane an IDE connection's diffs belong to is known now.
    fn rehome(&mut self, conn: u64, pane: Option<PaneId>) {
        let Some(p) = pane else { return };
        let mut found = false;
        for d in self.diffs.iter_mut().filter(|d| d.conn == conn && d.pane.is_none()) {
            d.pane = Some(p);
            found = true;
        }
        if found {
            self.diff_changed(p);
        }
    }

    /// IDE connections whose Claude Code wasn't found in a pane yet (the
    /// panes come back after a restart a moment after the relay speaks):
    /// look again.
    pub(super) fn find_ide_panes(&mut self) {
        let lost: Vec<(u64, u32)> = self
            .ide_conns
            .iter()
            .filter(|(_, (_, p))| p.is_none())
            .filter_map(|(c, (pid, _))| Some((*c, (*pid)?)))
            .collect();
        for (conn, pid) in lost {
            if let Some(pane) = self.pane_of_pid(pid) {
                info!(conn, pid, pane, "found Claude Code's pane");
                self.ide_conns.insert(conn, (Some(pid), Some(pane)));
                self.rehome(conn, Some(pane));
                self.touch(pane);
            }
        }
    }

    /// A pane's diffs changed: its card and its attention follow.
    fn diff_changed(&mut self, pane: PaneId) {
        match self.diff_reason(pane) {
            Some(r) => {
                let state = self.attention.get(&pane).copied().unwrap_or_default();
                if state == Attention::NeedsInput {
                    // Already asking: the card shows the diff now.
                    self.reasons.insert(pane, r);
                    self.emit(Some(pane), EventKind::Attention { state, reason: self.live_reason(pane) });
                } else {
                    let why = r.headline.clone();
                    self.set_attention_with(pane, Attention::NeedsInput, &why, Some(r));
                }
            }
            None => {
                if self.reasons.get(&pane).is_some_and(|r| r.kind == ReasonKind::Diff) {
                    if self.asks.contains_key(&pane) {
                        self.reasons.remove(&pane);
                    } else {
                        self.set_attention(pane, Attention::Idle, "diff closed");
                    }
                }
            }
        }
        self.touch(pane);
    }

    /// Someone answered a diff card (M28).
    pub(super) fn diff_answer(
        &mut self,
        pane: PaneId,
        id: Option<String>,
        accept: bool,
        text: Option<String>,
        by: Driver,
    ) -> Result<(), String> {
        let ide = self.config.ide.clone().ok_or("illogical isn't Claude Code's IDE here")?;
        let at = self
            .diffs
            .iter()
            .position(|d| d.pane == Some(pane) && id.as_ref().is_none_or(|i| *i == d.info.id))
            .ok_or_else(|| format!("%{pane} has no edit waiting (the terminal answered it, or it was closed)"))?;
        let d = self.diffs.remove(at);
        let changed = text.as_ref().is_some_and(|t| *t != d.new);
        let result =
            if accept { crate::ide::saved(text.as_deref().unwrap_or(&d.new)) } else { crate::ide::rejected(&d.tab) };
        ide.reply(d.conn, &d.call, result);
        let how = match (accept, changed) {
            (true, false) => "accepted",
            (true, true) => "accepted with changes",
            (false, _) => "rejected",
        };
        self.record_answer(pane, &by, &d.info.id, how, &diff_headline(&d.info));
        self.diff_changed(pane);
        Ok(())
    }

    /// An open question in a terminal (Claude Code's hook), or a block's
    /// open permission request or question.
    fn ask_reason(&self, pane: PaneId) -> Option<Reason> {
        let local = self.meta.get(&pane).and_then(|m| m.host).is_none();
        let (id, what, headline, agent, project, at_ms) = if let Some(a) = self.asks.get(&pane) {
            let what = if a.ask.kind == AskKind::Permission { AskWhat::Approve } else { AskWhat::Question };
            let (agent, project) = match self.blocks.get(&pane) {
                // Raised on a block (M35): who asks is the ask's, and the
                // project the block's (a studio box's app), not a cwd.
                Some(b) => {
                    let s = b.summary();
                    let project = match s.project {
                        Some(p) => p.root,
                        None => project_key(s.cwd.as_deref(), local),
                    };
                    (a.ask.agent.clone().unwrap_or_else(|| a.ask.source.clone()), project)
                }
                None => {
                    let agent =
                        a.ask.agent.clone().or_else(|| self.agent_name(pane)).unwrap_or_else(|| "claude".into());
                    let cwd = self
                        .panes
                        .get(&pane)
                        .and_then(|h| h.status().cwd.or_else(|| h.cwd().map(|c| c.display().to_string())));
                    (agent, project_key(cwd.as_deref(), local))
                }
            };
            (a.ask.id.clone(), what, a.ask.headline(), agent, project, a.ask.at_ms)
        } else {
            let w = self.blocks.get(&pane)?.waiting()?;
            (w.id, w.what, w.headline, w.agent, project_key(w.cwd.as_deref(), local), w.at_ms)
        };
        let actions = match what {
            AskWhat::Approve => vec![Action::Allow, Action::Deny, Action::Dismiss],
            AskWhat::Question => vec![Action::Answer, Action::Deny, Action::Dismiss],
        };
        Some(Reason {
            kind: ReasonKind::Ask,
            since_ms: at_ms,
            headline,
            command: None,
            exit: None,
            duration_ms: None,
            bundle: Some(format!("ask:{project}:{agent}")),
            ask: Some(AskRef { id, what, agent }),
            gate: None,
            actions,
        })
    }

    /// The agent running in a terminal, by its command line.
    fn agent_name(&self, pane: PaneId) -> Option<String> {
        let h = self.panes.get(&pane)?;
        let text = h.status().current.and_then(|c| c.text).or_else(|| h.command()).unwrap_or_default();
        agent_in(&text)
    }

    /// The agent running in a terminal now: what the OS says runs in the
    /// foreground first (it sees past `cd x && claude`), else the command
    /// line the shell reported.
    fn agent_running(&self, pane: PaneId) -> Option<String> {
        let h = self.panes.get(&pane)?;
        match h.command() {
            Some(argv) => agent_in(&argv),
            None => agent_in(&h.status().current.and_then(|c| c.text)?),
        }
    }

    /// What a pane's failures bundle by: the machine it runs on.
    pub(super) fn machine_key(&self, pane: PaneId) -> String {
        match self.machine_of(pane) {
            Some(m) => m.name.clone().unwrap_or_else(|| m.sprite.clone()),
            None => "here".into(),
        }
    }

    /// Read the screen of the agent the pane runs now, if it has rules
    /// (#145), and stop reading it once it's gone.
    pub(super) fn watch_agent(&mut self, pane: PaneId) {
        let name = self.agent_running(pane);
        self.watch_named(pane, name);
    }

    pub(super) fn watch_named(&mut self, pane: PaneId, name: Option<String>) {
        let found = name.and_then(|name| illogical_vt::detect::agent(&name));
        // On this machine, only the rule sets of the agents configured here
        // run (the inventory, #145); a machine's panes aren't this
        // machine's config.
        let local = self.machine_of(pane).is_none();
        let off = found.filter(|a| local && !self.inventory.runs(a.id));
        if let Some(a) = off {
            self.inventory.missing(a.id);
        }
        let agent = found.filter(|_| off.is_none());
        let id = agent.map(|a| a.id);
        if self.watching.get(&pane).copied() == id && self.unread.get(&pane).copied() == off.map(|a| a.id) {
            return;
        }
        let Some(h) = self.panes.get(&pane) else { return };
        h.watch_agent(agent, off);
        match off {
            Some(a) => self.unread.insert(pane, a.id),
            None => self.unread.remove(&pane),
        };
        if self.watching.get(&pane).copied() == id {
            return;
        }
        self.screen.remove(&pane);
        // The line that started it isn't a turn.
        self.turn_typed.remove(&pane);
        match id {
            Some(id) => self.watching.insert(pane, id),
            None => self.watching.remove(&pane),
        };
    }

    /// What an agent's screen says it's doing now (#145). Hooks and open
    /// questions say more, so they win; a bell or a notification still
    /// wants you until you answer it.
    pub(super) fn agent_screen(&mut self, pane: PaneId, state: AgentState, headline: Option<String>) {
        let before = self.screen.insert(pane, state);
        if self.asks.contains_key(&pane) {
            return;
        }
        let now = self.attention.get(&pane).copied().unwrap_or_default();
        let name =
            self.watching.get(&pane).and_then(|id| illogical_vt::detect::agent(id)).map_or("The agent", |a| a.name);
        match state {
            AgentState::Working => {
                let answered = now == Attention::NeedsInput && before == Some(AgentState::Blocked);
                if matches!(now, Attention::Idle | Attention::Done) || answered {
                    self.set_attention(pane, Attention::Working, "agent working");
                }
            }
            AgentState::Blocked => {
                let why = headline.unwrap_or_else(|| format!("{name} is waiting for you"));
                self.set_attention(pane, Attention::NeedsInput, &why);
            }
            AgentState::Idle => match now {
                // A turn someone started ended: done, if nobody watched it
                // end.
                Attention::Working
                    if before == Some(AgentState::Working) && self.turn_typed.remove(&pane) && !self.focused(pane) =>
                {
                    let why = format!("{name} finished its turn");
                    self.set_attention(pane, Attention::Done, &why);
                }
                Attention::Working => {
                    self.turn_typed.remove(&pane);
                    self.set_attention(pane, Attention::Idle, "agent idle")
                }
                // Its prompt went away without an answer typed here (Esc in
                // another terminal attached to it, say).
                Attention::NeedsInput if before == Some(AgentState::Blocked) => {
                    self.set_attention(pane, Attention::Idle, "agent idle")
                }
                _ => {}
            },
        }
    }

    pub(super) fn looks_like_agent(&self, pane: PaneId) -> bool {
        let Some(h) = self.panes.get(&pane) else { return false };
        let text = h.status().current.and_then(|c| c.text).or_else(|| h.command()).unwrap_or_default();
        agent_in(&text).is_some()
    }

    /// Whether a block is an agent's invites (#234), the owner's to answer.
    pub(super) fn is_invite(&self, pane: PaneId) -> bool {
        self.blocks.get(&pane).is_some_and(|b| b.kind() == BlockType::Invite)
    }

    /// Show a terminal's question on every client, and ask for you.
    pub(super) fn ask(&mut self, pane: PaneId, ask: Ask) -> Result<(u64, oneshot::Receiver<Replied>), String> {
        // A terminal, or a block that doesn't ask through its own methods
        // (a web page, a studio box: M35). An agent block's questions are
        // its own.
        let block = self.blocks.get(&pane).map(|b| b.kind());
        match block {
            None if !self.panes.contains_key(&pane) => return Err(format!("no pane %{pane}")),
            Some(BlockType::Agent | BlockType::Remote) => {
                return Err(format!("%{pane} is an agent or remote block: it asks through its own methods"));
            }
            _ => {}
        }
        let mut ask = ask;
        if ask.kind == AskKind::Permission && ask.tool_call_id.is_none() {
            // The tool call it's for: the `PreToolUse` just before it.
            if let Some(id) = self.pre.get(&pane).and_then(|pre| pre.iter().rev().find(|p| same_call(p, &ask))) {
                let id = id["tool_use_id"].as_str().map(str::to_owned);
                ask.id = id.clone().unwrap_or(ask.id);
                ask.tool_call_id = id;
            }
        }
        self.answered.remove(&pane);
        let (tx, rx) = oneshot::channel();
        let token = self.next_ask;
        self.next_ask += 1;
        let why = ask.headline();
        info!(pane, id = ask.id, "question asked");
        // The same question again (its asker reconnected) or a newer one:
        // either way the older registration is over.
        if let Some(old) = self.asks.insert(pane, TermAsk { ask, token, reply: tx }) {
            let _ = old.reply.send((AskReply::Withdrawn, None));
        }
        if self.attention.get(&pane) == Some(&Attention::NeedsInput) {
            // Already asking for you (Claude Code's own hook, say): this is
            // what it wants, and the card changed.
            self.touch(pane);
        } else {
            self.set_attention(pane, Attention::NeedsInput, &why);
        }
        Ok((token, rx))
    }

    pub(super) fn ask_reply(
        &mut self,
        pane: PaneId,
        id: Option<String>,
        answer: AskReply,
        by: Option<Driver>,
    ) -> Result<Ask, String> {
        let a = self
            .asks
            .get(&pane)
            .filter(|a| id.as_ref().is_none_or(|id| *id == a.ask.id))
            .ok_or_else(|| format!("no open question in %{pane} (it was answered, or withdrawn)"))?;
        // An agent's invite (#234) is the owner's to answer, by whatever
        // route: editors may answer other cards, an agent none of these.
        if self.is_invite(pane) && !by.as_ref().is_some_and(|b| b.who == "owner") {
            return Err(crate::invite::OWNER_ONLY.into());
        }
        let ask = a.ask.clone();
        let permission = ask.kind == AskKind::Permission;
        let answer = match answer {
            AskReply::Allow { .. } | AskReply::Deny { .. } if !permission => {
                return Err(format!("%{pane} asks a question: answer it"));
            }
            AskReply::Answer(_) if permission => return Err(format!("%{pane} asks for approval: allow or deny it")),
            // "Always": one of Claude Code's own suggestions, by index.
            AskReply::Allow { always: Some(i) } => {
                let pick = ask.suggestions.as_ref().and_then(|s| s.get(i.as_u64().unwrap_or(0) as usize)).cloned();
                if pick.is_none() {
                    return Err("Claude Code suggested no rule to keep: allow it once".into());
                }
                AskReply::Allow { always: pick }
            }
            a => a,
        };
        let a = self.asks.remove(&pane).expect("just found");
        info!(pane, id = ask.id, ?answer, "question answered");
        let terminal = answer == AskReply::Terminal;
        let how = match &answer {
            AskReply::Answer(_) => Some("answered"),
            AskReply::Decline => Some("skipped"),
            AskReply::Allow { always: None } => Some("allowed"),
            AskReply::Allow { always: Some(_) } => Some("allowed always"),
            AskReply::Deny { .. } => Some("denied"),
            AskReply::Terminal | AskReply::Withdrawn => None,
        };
        let by = by.unwrap_or_else(|| self.driver_of(&Principal::Owner));
        let _ = a.reply.send((answer, Some(by.clone())));
        if let Some(how) = how {
            self.record_answer(pane, &by, &ask.id, how, &ask.headline());
        }
        if terminal {
            // It asks again in the terminal: still wants you.
            self.touch(pane);
        } else {
            self.after_ask(pane);
        }
        Ok(ask)
    }

    /// A terminal's question went: back to work, and its card off every
    /// client.
    pub(super) fn after_ask(&mut self, pane: PaneId) {
        if self.attention.get(&pane) == Some(&Attention::NeedsInput) {
            // A block still waiting for something else (a studio box's
            // gate) keeps wanting you, for that.
            if self.blocks.contains_key(&pane)
                && self.reasons.get(&pane).is_some_and(|r| !matches!(r.kind, ReasonKind::Ask | ReasonKind::Input))
            {
                self.emit(
                    Some(pane),
                    EventKind::Attention { state: Attention::NeedsInput, reason: self.live_reason(pane) },
                );
                self.touch(pane);
                return;
            }
            // A terminal's program goes on; a block that was asked for
            // someone else (a studio box) is just a page again.
            let next = if self.blocks.contains_key(&pane) { Attention::Idle } else { Attention::Working };
            self.set_attention(pane, next, "answered");
        } else {
            self.touch(pane);
        }
    }

    /// Someone answered a card (M29): every client's card closes saying who,
    /// and the pane's history and the audit log say so too.
    pub(super) fn record_answer(&mut self, pane: PaneId, by: &Driver, id: &str, how: &str, headline: &str) {
        info!(pane, who = by.who, how, "answered");
        let at_ms = now_ms();
        self.answered.insert(
            pane,
            illogical_proto::ask::Answered {
                id: id.to_owned(),
                how: how.to_owned(),
                who: by.who.clone(),
                name: by.name.clone(),
                at_ms,
                headline: headline.to_owned(),
            },
        );
        if let Some(p) = self.panes.get(&pane) {
            p.note(format!("{how}: {headline}"), by.name.clone(), HistoryKind::Answer);
        }
        self.config.acl.record(serde_json::json!({
            "at": at_ms, "by": by.who, "name": by.name, "action": "answer", "pane": pane, "how": how,
            "headline": headline,
        }));
        self.touch(pane);
    }

    /// The terminal answered a permission card first (M29): Claude Code
    /// never tells its hook about a "Yes", so its next step closes the card.
    pub(super) fn terminal_answered(&mut self, pane: PaneId, ask: &Ask, how: &str) {
        self.answered.insert(
            pane,
            illogical_proto::ask::Answered {
                id: ask.id.clone(),
                how: how.to_owned(),
                who: "terminal".into(),
                name: "the terminal".into(),
                at_ms: now_ms(),
                headline: ask.headline(),
            },
        );
    }

    /// One of Claude Code's hook events in a terminal (M29).
    /// The conversation Claude Code in a pane holds, from any of its hooks
    /// (#146): every hook's input names it.
    fn note_session(&mut self, pane: PaneId, hook: &serde_json::Value) {
        let Some(id) = hook["session_id"].as_str() else { return };
        if !self.panes.contains_key(&pane) {
            return;
        }
        if !crate::resume::valid_id(id) {
            warn!(pane, "a hook gave a session id that isn't one; not keeping it");
            return;
        }
        let transcript = hook["transcript_path"].as_str().map(str::to_owned);
        let cwd = hook["cwd"].as_str().map(str::to_owned);
        self.set_session(pane, "claude", id, transcript, cwd);
    }

    pub(super) fn set_session(
        &mut self,
        pane: PaneId,
        agent: &str,
        id: &str,
        transcript: Option<String>,
        cwd: Option<String>,
    ) {
        let m = self.meta.entry(pane).or_default();
        let old = m.session.as_ref().filter(|s| s.agent == agent && s.id == id);
        let new = |a: Option<String>, b: Option<&String>| a.is_none() || a.as_ref() == b;
        // The same conversation, still known to be running: nothing new.
        if old.is_some_and(|s| {
            s.running && new(transcript.clone(), s.transcript.as_ref()) && new(cwd.clone(), s.cwd.as_ref())
        }) {
            return;
        }
        let transcript = transcript.or_else(|| old.and_then(|s| s.transcript.clone()));
        let cwd = cwd.or_else(|| old.and_then(|s| s.cwd.clone()));
        info!(pane, agent, session = id, "agent conversation");
        // Its hooks speak while it runs.
        m.session =
            Some(crate::store::AgentSession { agent: agent.into(), id: id.into(), transcript, cwd, running: true });
        // A pane running Claude Code resumes it, unless someone said
        // otherwise.
        if !m.policy_set && m.policy == Policy::Shell {
            m.policy = Policy::Resume;
        }
        self.mark(pane);
        self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
    }

    pub(super) fn hook(&mut self, pane: PaneId, hook: &serde_json::Value) {
        self.note_session(pane, hook);
        let event = hook["hook_event_name"].as_str().unwrap_or_default();
        let session = session_key(hook);
        let open = self
            .asks
            .get(&pane)
            .filter(|a| a.ask.kind == AskKind::Permission)
            .map(|a| a.ask.clone())
            .filter(|a| a.session.as_deref().is_some_and(|s| s.split('/').next() == session.split('/').next()));
        let close = match (event, &open) {
            ("PreToolUse", Some(a)) if a.session.as_deref() == Some(session.as_str()) => {
                if a.tool_call_id.is_none() && same_call(hook, a) {
                    // The card came first: this is its tool call.
                    if let Some(t) = self.asks.get_mut(&pane) {
                        t.ask.tool_call_id = hook["tool_use_id"].as_str().map(str::to_owned);
                    }
                    None
                } else if a.tool_call_id.as_deref() != hook["tool_use_id"].as_str() {
                    Some("closed")
                } else {
                    None
                }
            }
            ("PostToolUse" | "PostToolUseFailure", Some(a))
                if a.tool_call_id.is_some() && a.tool_call_id.as_deref() == hook["tool_use_id"].as_str()
                    || a.tool_call_id.is_none() && same_call(hook, a) =>
            {
                Some("allowed in the terminal")
            }
            ("Stop" | "UserPromptSubmit" | "SessionStart", Some(_)) => Some("closed"),
            _ => None,
        };
        if event == "PreToolUse" {
            let pre = self.pre.entry(pane).or_default();
            pre.push_back(hook.clone());
            while pre.len() > 16 {
                pre.pop_front();
            }
        }
        if let (Some(how), Some(a)) = (close, open)
            && let Some(t) = self.asks.remove(&pane)
        {
            info!(pane, id = a.id, event, "permission card closed: the terminal answered");
            let _ = t.reply.send((AskReply::Withdrawn, None));
            self.terminal_answered(pane, &a, how);
            self.after_ask(pane);
        }
    }

    /// `illogical inbox` waits for a follow-up: one queued goes now.
    pub(super) fn wait_inbox(&mut self, pane: PaneId) -> Result<(u64, oneshot::Receiver<InboxReply>), String> {
        if !self.panes.contains_key(&pane) {
            return Err(format!("no terminal %{pane}"));
        }
        let (tx, rx) = oneshot::channel();
        let token = self.next_ask;
        self.next_ask += 1;
        if let Some((text, by)) = self.queued.get_mut(&pane).and_then(|q| q.pop_front()) {
            let _ = tx.send(InboxReply::FollowUp { text, by });
            return Ok((token, rx));
        }
        if let Some(old) = self.inbox.insert(pane, Waiter { token, reply: tx }) {
            let _ = old.reply.send(InboxReply::Replaced);
        } else {
            self.touch(pane);
        }
        Ok((token, rx))
    }

    /// A follow-up for Claude Code in a terminal (M29), recorded as input
    /// from whoever sent it.
    pub(super) fn follow_up(&mut self, pane: PaneId, text: String, by: Driver) -> Result<bool, String> {
        let Some(p) = self.panes.get(&pane) else { return Err(format!("no terminal %{pane}")) };
        let text = text.trim().to_owned();
        if text.is_empty() {
            return Err("an empty follow-up".into());
        }
        info!(pane, who = by.who, "follow-up");
        p.note(format!("follow-up: {text}"), by.name.clone(), HistoryKind::Answer);
        self.config.acl.record(serde_json::json!({
            "at": now_ms(), "by": by.who, "name": by.name, "action": "follow_up", "pane": pane, "text": text,
        }));
        let now = match self.inbox.remove(&pane) {
            Some(w) => match w.reply.send(InboxReply::FollowUp { text: text.clone(), by: by.clone() }) {
                Ok(()) => true,
                Err(InboxReply::FollowUp { text, by }) => {
                    self.queued.entry(pane).or_default().push_back((text, by));
                    false
                }
                Err(_) => false,
            },
            None => {
                let q = self.queued.entry(pane).or_default();
                if q.len() >= MAX_QUEUED {
                    return Err("too many follow-ups waiting: is Claude Code's inbox hook set up?".into());
                }
                q.push_back((text, by));
                false
            }
        };
        self.touch(pane);
        Ok(now)
    }
}
