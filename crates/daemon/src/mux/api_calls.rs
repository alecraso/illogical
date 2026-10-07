//! What the HTTP API and the CLI ask of the multiplexer: one match on [`Api`],
//! answered here in the same order as every other change.

use super::{Api, AskReply, Daemon, SAVE_DEBOUNCE};
use crate::acl::Principal;
use arugula_core::{Intent, Role};
use arugula_proto::{PaneId, ServerMsg, ask::AskKind};
use tokio::time::Instant;
use tracing::info;

impl Daemon {
    pub(super) fn api(&mut self, api: Api) {
        match api {
            Api::RoleOn(who, pane, reply) => {
                let r = if who.is_owner() {
                    Some((Role::Owner, None))
                } else if self.is_presence(pane) {
                    self.presence_role(&who).map(|r| (r, None))
                } else {
                    self.session_of(pane).filter(|_| self.readable(&who, pane)).and_then(|s| {
                        let role = self.config.acl.role(&who, s)?;
                        Some((role, self.config.acl.floor(&who, s, pane)))
                    })
                };
                let _ = reply.send(r);
            }
            Api::MayDrive(who, pane, reply) => {
                let _ = reply.send(self.may_drive_here(&who, pane));
            }
            Api::ThreadGet(target, who, reply) => {
                let _ = reply.send(self.thread_get(target, &who));
            }
            Api::ThreadPost(post, reply) => {
                let _ = reply.send(self.thread_post(post));
            }
            Api::ThreadRead(target, who, upto) => {
                if self.thread_role(&who, target).is_some() && self.threads.mark_read(who.id(), target, upto) {
                    self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
                    self.soon();
                }
            }
            Api::CanRead(target, ids, reply) => {
                let r = self.thread_session(target).map(|_| {
                    ids.into_iter()
                        .map(|id| {
                            let p = Principal::User { id, name: String::new(), pic: None };
                            self.thread_role(&p, target).is_some()
                        })
                        .collect()
                });
                let _ = reply.send(r);
            }
            Api::ThreadPlace(target, msg, reply) => {
                let r = self
                    .thread_session(target)
                    .ok_or_else(|| "no such thread, or it's a private pane's".to_owned())
                    .map(|s| {
                        (s, msg.and_then(|id| self.threads.get(target).iter().find(|m| m.id == id).map(|m| m.at)))
                    });
                let _ = reply.send(r);
            }
            Api::InviteTo(session, pane, reply) => {
                let r = match self.mux.session(session) {
                    Err(_) => Err(format!("no session ${session}")),
                    Ok(s) => {
                        let panes: Vec<PaneId> =
                            s.tabs.iter().filter_map(|t| self.mux.tab(*t).ok()).flat_map(|t| t.root.panes()).collect();
                        let name = s.name.clone();
                        match pane {
                            Some(p) if panes.contains(&p) => Ok((p, name)),
                            Some(p) => Err(format!("%{p} isn't in {name}")),
                            None => panes.first().map(|p| (*p, name.clone())).ok_or(format!("{name} has no panes")),
                        }
                    }
                };
                let _ = reply.send(r);
            }
            Api::Trust(pane, to, minutes, reply) => {
                let here = self.machine_of(pane).is_none() && !self.blocks.contains_key(&pane);
                let ok = here && !self.config.control.is_team();
                if ok {
                    self.trust_with(pane, &to, minutes);
                    self.broadcast();
                }
                let _ = reply.send(ok);
            }
            Api::Tell(who, message) => self.tell(&who, ServerMsg::Notice { message }),
            Api::SessionEnds(session, reply) => {
                let ends = self.mux.session(session).ok().map(|s| {
                    s.tabs
                        .iter()
                        .filter_map(|t| self.mux.tab(*t).ok())
                        .flat_map(|t| t.root.panes())
                        .filter_map(|p| self.panes.get(&p).map(|h| (p, h.status().end)))
                        .collect()
                });
                let _ = reply.send(ends);
            }
            Api::Panes(reply) => {
                let _ = reply.send(self.summaries());
            }
            Api::Pane(pane, reply) => {
                let _ = reply.send(self.panes.get(&pane).cloned());
            }
            Api::Attention(pane, state, why, reply) => {
                let known = self.panes.contains_key(&pane) || self.blocks.contains_key(&pane);
                self.set_attention(pane, state, why.as_deref().unwrap_or("set by the API"));
                let _ = reply.send(known);
            }
            Api::AttentionList(who, reply) => {
                let mut out = Vec::new();
                for (pane, state) in &self.attention {
                    if who.as_ref().is_some_and(|w| !self.readable(w, *pane)) {
                        continue;
                    }
                    let Some(reason) = self.live_reason(*pane) else { continue };
                    let session = self.session_of(*pane);
                    if session.is_none() && !self.is_presence(*pane) {
                        continue;
                    }
                    out.push(arugula_proto::api::AttentionItem { pane: *pane, session, state: *state, reason });
                }
                out.sort_by_key(|i| (i.reason.since_ms, i.pane));
                let _ = reply.send(out);
            }
            Api::Reason(pane, reply) => {
                // A block answers through its own methods, unless the daemon
                // holds the question (M35: raised on it through `Ask`).
                let block = self.blocks.contains_key(&pane) && !self.asks.contains_key(&pane);
                let _ = reply.send(self.live_reason(pane).map(|r| (r, block)));
            }
            Api::Machines(reply) => {
                let _ = reply.send(self.machines.values().cloned().collect());
            }
            Api::MachineOf(pane, reply) => {
                let _ = reply.send(self.machine_of(pane).cloned());
            }
            Api::ShareMachine(pane, reply) => {
                let _ = reply.send(self.share_machine(pane));
            }
            Api::ResetMachine(id, reply) => {
                let _ = reply.send(self.reset_machine(id));
            }
            Api::Open(req, who, reply) => {
                let r = match who.filter(|w| !w.is_owner()) {
                    None => self.open_block(req),
                    Some(who) => self.guest_block(req, &who),
                };
                let _ = reply.send(r);
            }
            Api::Block(id, reply) => {
                let _ = reply.send(self.blocks.get(&id).cloned());
            }
            Api::OwnTab(pane, name, reply) => {
                let _ = reply.send(self.own_tab(pane, name));
            }
            Api::Close(pane, reply) => {
                let known = self.panes.contains_key(&pane) || self.blocks.contains_key(&pane);
                if known {
                    let _ = self.intent(None, Intent::ClosePane { pane });
                }
                let _ = reply.send(known);
            }
            Api::Run(req, reply) => {
                let _ = reply.send(self.run_command(req));
            }
            Api::AgentEnv(reply) => {
                let _ = reply.send((self.config.home.clone(), self.config.env(0)));
            }
            Api::InputBy(pane, data, by) => self.input(pane, data, Some(by)),
            Api::GuestInput { pane, client, by, data, size, reply } => {
                if !self.panes.contains_key(&pane) {
                    let _ = reply.send(Err("the pane closed".into()));
                    return;
                }
                if !self.pair.contains(&pane) {
                    match self.drivers.get(&pane) {
                        Some(d) if d.who != by.who => {
                            let _ = reply.send(Err(format!("{} is driving this pane", d.name)));
                            return;
                        }
                        Some(_) => self.typed(pane),
                        None => {
                            self.drive(pane, by.clone());
                            self.typed(pane);
                            self.guest_view(client, pane, size);
                            self.broadcast();
                        }
                    }
                }
                self.hold.typed(&self.mux, client, pane, std::time::Instant::now());
                self.input(pane, data, Some(by.name));
                let _ = reply.send(Ok(()));
            }
            Api::GuestSize { pane, client, who, size } => {
                if self.drivers.get(&pane).is_some_and(|d| d.who == who) {
                    self.guest_view(client, pane, size);
                }
            }
            Api::GuestLeft { client, who } => {
                let before = self.drivers.len();
                self.drivers.retain(|_, d| d.who != who);
                if self.mux.release(client) {
                    self.changed();
                }
                if self.drivers.len() != before {
                    self.broadcast();
                }
            }
            Api::Ide(ev) => self.ide_event(ev),
            Api::IdeConns(pane, reply) => {
                let mut v: Vec<u64> =
                    self.ide_conns.iter().filter(|(_, c)| c.1 == Some(pane)).map(|(n, _)| *n).collect();
                v.sort();
                let _ = reply.send(v);
            }
            Api::Editors(who, reply) => {
                let mut out: Vec<(PaneId, serde_json::Value)> = self
                    .blocks
                    .iter()
                    .filter(|(id, b)| b.link().is_some() && who.as_ref().is_none_or(|w| self.readable(w, **id)))
                    .map(|(id, b)| {
                        let i = self.block_info(*id, b);
                        let v = serde_json::json!({
                            "pane": id, "block": !b.detached(), "editor": i.editor, "folder": i.cwd,
                            "project": i.project, "file": i.file, "title": i.title, "attention": i.attention,
                            "reason": i.reason,
                        });
                        (*id, v)
                    })
                    .collect();
                out.sort_by_key(|(id, _)| *id);
                let _ = reply.send(out.into_iter().map(|(_, v)| v).collect());
            }
            Api::DiffAnswer(pane, id, accept, text, by, reply) => {
                let _ = reply.send(self.diff_answer(pane, id, accept, text, by));
            }
            Api::DiffOf(pane, reply) => {
                let d = self.diffs.iter().find(|d| d.pane == Some(pane));
                let _ = reply.send(d.map(|d| (d.info.clone(), d.old.clone(), d.new.clone())));
            }
            Api::EditorJoin(link, reply) => {
                let id = self.mux.reserve_pane();
                link.bind(id, self.notices.clone());
                self.blocks.insert(id, crate::editor::presence::Presence::make(link));
                // A new entry: everyone gets a whole State with it.
                self.full = true;
                self.broadcast();
                self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
                let _ = reply.send(id);
            }
            Api::EditorLeave(id) => {
                if self.blocks.get(&id).is_some_and(|b| b.detached()) {
                    self.blocks.remove(&id);
                    self.attention.remove(&id);
                    self.reasons.remove(&id);
                    self.follows.remove(&id);
                    self.full = true;
                    self.broadcast();
                }
            }
            Api::StartedBy(pane, by) => {
                if self.panes.contains_key(&pane) || self.blocks.contains_key(&pane) {
                    // An agent's: whoever stands behind that agent stands
                    // behind this too.
                    let guest = by.block.and_then(|b| self.guest_behind(b));
                    let meta = self.meta.entry(pane).or_default();
                    meta.started_by = Some(by);
                    if guest.is_some() {
                        meta.guest = guest;
                    }
                    self.touch(pane);
                }
            }
            Api::GuestBehind(pane, reply) => {
                let _ = reply.send(self.guest_behind(pane));
            }
            Api::Ask(pane, ask, reply) => {
                let _ = reply.send(self.ask(pane, *ask));
            }
            Api::Holds(pane, reply) => {
                let _ = reply.send(self.asks.contains_key(&pane));
            }
            Api::AskReply(pane, id, answer, by, reply) => {
                let _ = reply.send(self.ask_reply(pane, id, answer, by));
            }
            Api::Hook(pane, hook) => self.hook(pane, &hook),
            Api::Inbox(pane, hook, reply) => {
                self.hook(pane, &hook);
                let _ = reply.send(self.wait_inbox(pane));
            }
            Api::InboxGone(pane, token) => {
                if self.inbox.get(&pane).is_some_and(|w| w.token == token) {
                    self.inbox.remove(&pane);
                    self.touch(pane);
                }
            }
            Api::FollowUp(pane, text, by, reply) => {
                let _ = reply.send(self.follow_up(pane, text, by));
            }
            Api::Answered(pane, by, id, how, headline) => self.record_answer(pane, &by, &id, &how, &headline),
            Api::Who(who, reply) => {
                let _ = reply.send(self.driver_of(&who));
            }
            Api::AskWithdraw(pane, id, token) => {
                let open = self.asks.get(&pane).is_some_and(|a| {
                    id.as_ref().is_none_or(|id| *id == a.ask.id) && token.is_none_or(|t| t == a.token)
                });
                if open && let Some(a) = self.asks.remove(&pane) {
                    info!(pane, id = a.ask.id, "question withdrawn");
                    let _ = a.reply.send((AskReply::Withdrawn, None));
                    if a.ask.kind == AskKind::Permission && token.is_none() {
                        // Its hook was stopped: "No" or Esc in the terminal.
                        self.terminal_answered(pane, &a.ask, "denied in the terminal");
                    }
                    self.after_ask(pane);
                }
            }
        }
    }
}
