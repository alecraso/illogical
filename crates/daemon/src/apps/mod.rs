//! Studio app blocks (M35): a studio box (arugula-salad's studio) as a
//! block. The frame is the box itself, on its own origin, so there's no
//! block site or proxy; what the box waits on comes to illogical as
//! attention, read through hud in the box (never by running anything
//! there).
//!
//! - **Config** `{box_url, app, studio, title?, follower?}`: the box's
//!   origin, the app's name in studio, and which studio. No entry link is
//!   ever in it, nor in the block's log or state.
//! - **Getting in.** A client's frame asks for a way in with the `enter`
//!   method (`{to?}`: one of hud's pages): the daemon mints a fresh
//!   ten-minute `/__enter` link from studio and hands it back, once. The
//!   client navigates the frame to it without keeping it; the box's door
//!   then sets hud's partitioned cookie, which carries every reload after.
//!   A client enters each time it draws the block anew (it can't see a
//!   cross-site frame's 401), and `reload` makes every client enter again.
//!   Only the owner may enter: a link is the owner's way into the box.
//! - **Questions.** The follower (`hud.rs`) puts the box agent's questions
//!   on the block as asks (`source: "hud"`), answered like a terminal's;
//!   the answer goes back to hud naming who gave it.
//! - **Gates** from hud's work board (read again whenever hud's live feed
//!   moves) are M34's gates with a `hud` source: the same `gate` reason,
//!   card, rail and phone sheet as a workspace's. `approve` goes to hud's
//!   approve route through the follower's session, naming who approves
//!   with a follower credential. The card stays until hud's board no
//!   longer lists the gate; an approve that fails puts its error on it.
//! - **Prompts.** `send` (`{text, tab?}`) prompts the box's agent through
//!   the follower's session, in the tab named (its title or chat key) or
//!   else the box's first; hud queues it behind a running turn. The block's
//!   log and history say who sent what, to which tab.
//! - **Restore.** Nothing to bring back but the config: after a restart
//!   the follower mints again, and so does each client's frame.

pub mod hud;
pub mod studio;

use std::sync::{Arc, Mutex, Weak};

use futures_util::future::BoxFuture;
use illogical_proto::api::HistoryKind;
use illogical_proto::{Attention, BlockType, Gate, Project, ReasonKind, WorkKind};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::block::{Block, BlockCtx, Summary, no_method};

#[derive(Debug, Clone, Deserialize)]
struct Config {
    box_url: String,
    app: String,
    studio: String,
    #[serde(default)]
    title: Option<String>,
    /// The daemon's session is a hud follower credential (the link kept
    /// with `illogical studio follower APP`): answers name who gave them
    /// (`onBehalfOf`). Else it enters as the owner, through studio.
    #[serde(default)]
    follower: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
struct State {
    app: String,
    title: Option<String>,
    box_url: String,
    studio: String,
    /// Whether the follower holds a follower credential (so hud is told
    /// who answered).
    follower_credential: bool,
    follower: hud::Status,
    /// Bumped by `reload`: every client enters again.
    reloads: u64,
    /// The box's gates waiting, from hud's work board.
    gates: Vec<Gate>,
    /// The last approve that failed: the gate's key and why.
    gate_error: Option<(String, String)>,
}

pub struct AppBlock {
    me: Weak<AppBlock>,
    ctx: BlockCtx,
    config: Config,
    state: Arc<Mutex<State>>,
    follower: Mutex<Option<hud::Follower>>,
    session: Arc<Mutex<Option<Arc<hud::Session>>>>,
    /// The gate reason's headline raised now, if any.
    raised: Mutex<Option<String>>,
}

impl AppBlock {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let mut config: Config = serde_json::from_value(config).map_err(|e| format!("app config: {e}"))?;
        config.box_url = studio::box_origin(&config.box_url)?;
        config.studio = studio::studio_url(&config.studio)?;
        if config.app.is_empty() || !config.app.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
            return Err(format!("not an app name: {:?}", config.app));
        }
        let state = Arc::new(Mutex::new(State {
            app: config.app.clone(),
            title: config.title.clone(),
            box_url: config.box_url.clone(),
            studio: config.studio.clone(),
            follower_credential: config.follower,
            ..State::default()
        }));
        let b = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            ctx: ctx.clone(),
            config: config.clone(),
            state: state.clone(),
            follower: Mutex::new(None),
            session: Arc::default(),
            raised: Mutex::new(None),
        });
        let (st, c) = (state.clone(), ctx.clone());
        let report = Arc::new(move |s: hud::Status| {
            let mut now = st.lock().unwrap();
            if now.follower != s {
                now.follower = s;
                drop(now);
                c.changed();
            }
        });
        let c = ctx.clone();
        let log = Arc::new(move |v: Value| append(&c, &v));
        let me = Arc::downgrade(&b);
        let gates = Arc::new(move |g: Vec<Gate>| {
            if let Some(b) = me.upgrade() {
                b.gates(g);
            }
        });
        let follower = hud::start(hud::Setup {
            origin: config.box_url.clone(),
            app: config.app.clone(),
            mint: mint(&config, None),
            on_behalf: config.follower,
            ctx,
            report,
            log,
            gates,
            session: b.session.clone(),
        });
        *b.follower.lock().unwrap() = Some(follower);
        Ok(b)
    }
}

impl AppBlock {
    /// hud's board was read: these gates wait.
    fn gates(&self, gates: Vec<Gate>) {
        {
            let mut st = self.state.lock().unwrap();
            if st.gates == gates {
                return;
            }
            if st.gate_error.as_ref().is_some_and(|(k, _)| !gates.iter().any(|g| g.key() == *k)) {
                st.gate_error = None;
            }
            st.gates = gates;
        }
        self.raise();
        self.ctx.changed();
    }

    /// Gates waiting are attention (with the last approve's error, if it
    /// failed); none, and it's let go.
    fn raise(&self) {
        let (reason, error) = {
            let st = self.state.lock().unwrap();
            (crate::gate::reason(&st.gates), st.gate_error.clone())
        };
        let reason = reason.map(|mut r| {
            if let (Some((key, e)), Some(g)) = (&error, r.gate.as_ref())
                && g.key() == *key
            {
                r.headline = format!("{}: approving failed: {e}", r.headline);
            }
            r
        });
        let now = reason.as_ref().map(|r| r.headline.clone());
        let mut raised = self.raised.lock().unwrap();
        if *raised == now {
            return;
        }
        match reason {
            Some(r) => self.ctx.reason(Attention::NeedsInput, r),
            None => self.ctx.clear(ReasonKind::Gate),
        }
        *raised = now;
    }

    /// Approve the gate `args` names (`{key}`, `{member, op, gate}`, or the
    /// first) through hud, as `by`.
    async fn approve(&self, args: Value, by: Option<String>) -> Result<Value, String> {
        let gate = {
            let st = self.state.lock().unwrap();
            let arg = |k: &str| args[k].as_str().map(str::to_owned);
            let want = arg("key").or_else(|| Some(format!("{}/{}/{}", arg("member")?, arg("op")?, arg("gate")?)));
            match want {
                None => st.gates.first().cloned().ok_or("no gate is waiting")?,
                Some(k) => st.gates.iter().find(|g| g.key() == k).cloned().ok_or_else(|| {
                    format!("no gate {k} is waiting (it was approved, or hud's board hasn't shown it yet)")
                })?,
            }
        };
        let session = self.session.lock().unwrap().clone().ok_or("not in the box yet: try again in a moment")?;
        let via = crate::gate::Via::Hud { session: &session, follower: self.config.follower };
        let result = crate::gate::approve(&gate, by.as_deref(), &via).await;
        let (ok, said) = match &result {
            Ok(s) | Err(s) => (result.is_ok(), s.clone()),
        };
        append(
            &self.ctx,
            &json!({ "e": "approve", "member": gate.member, "op": gate.op, "gate": gate.gate, "by": by, "ok": ok, "said": said }),
        );
        if let Ok(mut l) = self.ctx.log() {
            let at = l.end();
            let text = format!("approved {}: {} at gate {}", gate.member, gate.op, gate.gate);
            let _ = l.record(
                at,
                crate::store::Event::Command {
                    at_ms: crate::store::now_ms(),
                    text: Some(text),
                    cwd: None,
                    by: by.clone(),
                    kind: HistoryKind::Answer,
                },
            );
            let _ = l.record(
                at,
                crate::store::Event::End { at_ms: crate::store::now_ms(), exit: Some(if ok { 0 } else { 1 }) },
            );
        }
        self.state.lock().unwrap().gate_error = if ok { None } else { Some((gate.key(), said.clone())) };
        self.raise();
        self.ctx.changed();
        // hud's board says when it's gone; until then the card stays.
        if let Some(f) = &*self.follower.lock().unwrap() {
            f.reread();
        }
        let said = result?;
        Ok(json!({ "approved": gate.key(), "gate": gate, "by": by, "said": said }))
    }

    /// Prompt the box's agent (`{text, tab?}`) through hud, as `by`.
    async fn send(&self, args: Value, by: Option<String>) -> Result<Value, String> {
        let text = args["text"].as_str().map(str::trim).filter(|t| !t.is_empty()).ok_or("send needs {\"text\": …}")?;
        let session = self.session.lock().unwrap().clone().ok_or("not in the box yet: try again in a moment")?;
        let tabs = session.tab_list().await.map_err(|e| format!("listing the box's tabs: {e}"))?;
        let tab = hud::pick_tab(&tabs, args["tab"].as_str().filter(|t| !t.is_empty()))?;
        let result = session.prompt(&tab.chat, text).await;
        let ok = result.is_ok();
        append(
            &self.ctx,
            &json!({ "e": "prompt", "chat": tab.chat, "tab": tab.title, "by": by, "ok": ok, "error": result.as_ref().err() }),
        );
        if let Ok(mut l) = self.ctx.log() {
            let at = l.end();
            let _ = l.record(
                at,
                crate::store::Event::Command {
                    at_ms: crate::store::now_ms(),
                    text: Some(format!("prompted {}: {text}", tab.title)),
                    cwd: None,
                    by: by.clone(),
                    kind: HistoryKind::Command,
                },
            );
            let _ = l.record(
                at,
                crate::store::Event::End { at_ms: crate::store::now_ms(), exit: Some(if ok { 0 } else { 1 }) },
            );
        }
        let v = result?;
        Ok(json!({
            "tab": tab.title,
            "chat": tab.chat,
            "prompt_id": v["promptId"],
            "position": v["position"],
            "queued": v["queued"].as_bool().unwrap_or(false),
            "by": by,
        }))
    }
}

/// How the block gets into its box: a fresh studio link each time, or the
/// follower link kept for it.
fn mint(c: &Config, to: Option<String>) -> hud::Mint {
    let (studio_url, app, follower) = (c.studio.clone(), c.app.clone(), c.follower);
    Arc::new(move || {
        let (studio_url, app, to) = (studio_url.clone(), app.clone(), to.clone());
        Box::pin(async move {
            let s = studio::get().ok_or("no studio here")?;
            if follower {
                return s.follower(&app).ok_or_else(|| {
                    format!("no follower link for {app}: `hud share --role follower` in the box, then `illogical studio follower {app}`")
                });
            }
            s.enter_link(&studio_url, &app, to.as_deref()).await
        })
    })
}

fn append(ctx: &BlockCtx, v: &Value) {
    if let Ok(mut log) = ctx.log() {
        let mut line = v.to_string().into_bytes();
        line.push(b'\n');
        let _ = log.append(&line);
    }
}

impl Block for AppBlock {
    fn kind(&self) -> BlockType {
        BlockType::App
    }

    fn config(&self) -> Value {
        let c = &self.config;
        let mut v = json!({ "box_url": c.box_url, "app": c.app, "studio": c.studio });
        if let Some(t) = &c.title {
            v["title"] = json!(t);
        }
        if c.follower {
            v["follower"] = json!(true);
        }
        v
    }

    fn state(&self) -> Value {
        serde_json::to_value(&*self.state.lock().unwrap()).unwrap_or_default()
    }

    fn text(&self) -> String {
        let s = self.state.lock().unwrap();
        format!("{}\n{}\n", s.title.as_deref().unwrap_or(&s.app), s.box_url)
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        self.call_by(method, args, None)
    }

    fn call_by(&self, method: &str, args: Value, by: Option<&str>) -> BoxFuture<'static, Result<Value, String>> {
        match method {
            "approve" => {
                let (me, by) = (self.me.upgrade(), by.map(str::to_owned));
                Box::pin(async move { me.ok_or("closed")?.approve(args, by).await })
            }
            "send" => {
                let (me, by) = (self.me.upgrade(), by.map(str::to_owned));
                Box::pin(async move { me.ok_or("closed")?.send(args, by).await })
            }
            // A fresh way in, for a frame: used once, never kept.
            "enter" => {
                let to = args["to"].as_str().filter(|t| !t.is_empty()).map(str::to_owned);
                if let Some(t) = &to
                    && !studio::hud_page(t)
                {
                    let e = format!("not one of hud's pages: {t}");
                    return Box::pin(async move { Err(e) });
                }
                // The frame enters as the owner, never with the follower's
                // credential.
                let c = Config { follower: false, ..self.config.clone() };
                let mint = mint(&c, to.clone());
                let ctx = self.ctx.clone();
                Box::pin(async move {
                    let url = mint().await?;
                    append(&ctx, &json!({ "e": "enter", "to": to }));
                    Ok(json!({ "url": url }))
                })
            }
            "reload" => {
                self.state.lock().unwrap().reloads += 1;
                self.ctx.changed();
                Box::pin(async { Ok(json!({})) })
            }
            "state" => {
                let s = self.state();
                Box::pin(async move { Ok(s) })
            }
            m => {
                let e = no_method(BlockType::App, m);
                Box::pin(async move { Err(e) })
            }
        }
    }

    fn summary(&self) -> Summary {
        let s = self.state.lock().unwrap();
        Summary {
            work: Some(WorkKind::App),
            project: Some(Project { root: s.box_url.clone(), name: s.app.clone() }),
            title: Some(s.title.clone().unwrap_or_else(|| s.app.clone())),
            ..Summary::default()
        }
    }

    fn close(&self) {
        self.follower.lock().unwrap().take();
    }
}
