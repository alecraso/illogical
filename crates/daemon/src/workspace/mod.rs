//! M34: a chant workspace as a block (S21's spike, finished).
//!
//! Config `{root, env}` (env defaults to `local`: the environment whose
//! gates and releases `status` reads). It reads the workspace through
//! chant's read contract on the block's host, with the workspace's own
//! chant ([`model`]): `ls`, `check`, `records` and `status`, in one `sh -c`.
//!
//! **Freshness.** A full read costs about 7.5 CPU-seconds, so it runs on
//! open, on `refresh` and after `approve`, and otherwise only when a cheap
//! git fingerprint changes ([`model::FINGERPRINT`]). The fingerprint is
//! looked at every [`POLL`] while some client draws the block, and every
//! [`IDLE_POLL`] when none does and the workspace is on this host (so a gate
//! reached while nobody looks still reaches the swarm and push; a VM's is
//! left to sleep). Nothing here fetches.
//!
//! **Gates.** A gate waiting in any member is attention: `needs_input`
//! with a `gate` reason made from the [`Gate`] ([`crate::gate::reason`]).
//! `approve` resolves one with chant's own `approve` in the member's
//! directory, `--approver` the person who asked (#75: the owner or an
//! editor; guests can't call it). It's logged, with who, in the block's log
//! and its history.
//!
//! Methods: `refresh`, `approve {member, op, gate}` (or `{key}`; the first
//! gate if none), `member {name}` (its directory, for opening panes there),
//! `state`.

mod model;

use std::{
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use arugula_proto::api::HistoryKind;
use arugula_proto::{Attention, BlockType, Gate, GateSource, ReasonKind};
use futures_util::future::BoxFuture;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    block::{Block, BlockCtx, Summary, no_method},
    review::{Live, Runner, log},
    store::{Event, now_ms},
};

/// How often the fingerprint is looked at while drawn (a full read only
/// when it changes).
const POLL: Duration = Duration::from_secs(3);
/// ...and while nobody draws it, for a workspace on this host.
const IDLE_POLL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Deserialize)]
struct Config {
    root: String,
    #[serde(default = "local")]
    env: String,
}

fn local() -> String {
    "local".into()
}

pub struct Workspace {
    ctx: BlockCtx,
    me: Weak<Workspace>,
    config: Config,
    runner: tokio::sync::OnceCell<Result<Runner, String>>,
    state: Mutex<model::State>,
    /// The fingerprint at the last read.
    seen: Mutex<Option<String>>,
    /// The gate attention last asked for (its headline), so it's asked once
    /// per change. Starts as `Some("")` so the first read clears any left
    /// from before a restart.
    raised: Mutex<Option<String>>,
    live: Live,
    reading: tokio::sync::Mutex<()>,
}

impl Workspace {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let mut config: Config = serde_json::from_value(config).map_err(|e| format!("workspace config: {e}"))?;
        if config.root.is_empty() {
            return Err("a workspace block needs a root".into());
        }
        if let Some(rest) = config.root.strip_prefix("~/").filter(|_| ctx.sprite.is_none()) {
            config.root = ctx.home.join(rest).display().to_string();
        }
        let state =
            model::State { root: config.root.clone(), env: config.env.clone(), loading: true, ..Default::default() };
        log(&ctx, &json!({ "e": "view", "root": config.root, "env": config.env }));
        let w = Arc::new_cyclic(|me| Self {
            ctx,
            me: me.clone(),
            config,
            runner: tokio::sync::OnceCell::new(),
            state: Mutex::new(state),
            seen: Mutex::new(None),
            raised: Mutex::new(Some(String::new())),
            live: Live::default(),
            reading: tokio::sync::Mutex::new(()),
        });
        let me = w.clone();
        w.ctx.rt.spawn(async move {
            me.load().await;
            me.idle().await;
        });
        Ok(w)
    }

    async fn runner(&self) -> Result<Runner, String> {
        // The user's shell environment (#74): their node and chant, as a
        // pane would find them.
        self.runner.get_or_init(|| Runner::user(&self.ctx)).await.clone()
    }

    fn local(&self) -> bool {
        self.ctx.sprite.is_none()
    }

    /// A full read, and attention for what it found.
    async fn load(&self) {
        let _one = self.reading.lock().await;
        if self.live.closed() {
            return;
        }
        self.state.lock().unwrap().loading = true;
        self.ctx.changed();
        let print = self.fingerprint().await;
        *self.seen.lock().unwrap() = print;
        let mut st = match self.runner().await {
            Err(e) => model::State { error: Some(e), ..Default::default() },
            Ok(r) => {
                let args = [self.config.root.clone(), model::READER.to_owned(), self.config.env.clone()];
                match r.sh(model::SCRIPT, &args).await {
                    Err(e) => model::State { error: Some(e), ..Default::default() },
                    Ok((out, _)) => match serde_json::from_slice::<Value>(&out) {
                        Ok(raw) => model::compose(&raw, &self.config.env),
                        Err(e) => model::State {
                            error: Some(format!("the reader said something else: {e}")),
                            ..Default::default()
                        },
                    },
                }
            }
        };
        if st.root.is_empty() {
            st.root = self.config.root.clone();
        }
        if st.headline.is_none() {
            st.headline = st.error.clone();
        }
        st.env = self.config.env.clone();
        st.updated_ms = now_ms();
        for g in &mut st.gates {
            if let GateSource::Chant { machine, .. } = &mut g.source {
                machine.clone_from(&self.ctx.sprite);
            }
        }
        self.raise(&st.gates);
        *self.state.lock().unwrap() = st;
        self.ctx.changed();
    }

    /// Gates waiting are attention; none, and it's let go.
    fn raise(&self, gates: &[Gate]) {
        let reason = crate::gate::reason(gates);
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

    /// The fingerprint now, if it can be had.
    async fn fingerprint(&self) -> Option<String> {
        let r = self.runner().await.ok()?;
        let (out, _) = r.sh(model::FINGERPRINT, std::slice::from_ref(&self.config.root)).await.ok()?;
        Some(String::from_utf8_lossy(&out).trim().to_owned())
    }

    /// Reads again if the fingerprint moved.
    async fn check(&self) {
        let now = self.fingerprint().await;
        let changed = now.is_none() || *self.seen.lock().unwrap() != now;
        if changed {
            self.load().await;
        }
    }

    /// While drawn: the fingerprint every [`POLL`], from the start.
    fn watch(&self, round: u64) {
        let Some(me) = self.me.upgrade() else { return };
        self.ctx.rt.spawn(async move {
            while me.live.on(round) {
                me.check().await;
                tokio::time::sleep(POLL).await;
            }
        });
    }

    /// While nobody draws it: every [`IDLE_POLL`], on this host only.
    async fn idle(&self) {
        while !self.live.closed() {
            tokio::time::sleep(IDLE_POLL).await;
            if self.local() && !self.live.drawn() && !self.live.closed() {
                self.check().await;
            }
        }
    }

    /// The gate `args` names: `{member, op, gate}`, `{key}`, or the first.
    fn find_gate(&self, args: &Value) -> Result<Gate, String> {
        let st = self.state.lock().unwrap();
        let arg = |k: &str| args[k].as_str();
        let want = match (arg("key"), arg("member"), arg("op"), arg("gate")) {
            (Some(k), ..) => Some(k.to_owned()),
            (None, Some(m), Some(o), Some(g)) => Some(format!("{m}/{o}/{g}")),
            (None, None, None, None) => None,
            _ => {
                return Err(
                    "approve needs {\"member\", \"op\", \"gate\"} (or {\"key\"}, or nothing for the first)".into()
                );
            }
        };
        match want {
            None => st.gates.first().cloned().ok_or_else(|| "no gate is waiting".into()),
            Some(k) => {
                st.gates.iter().find(|g| g.key() == k).cloned().ok_or_else(|| {
                    format!("no gate {k} is waiting (it was approved, or hasn't been read yet: refresh)")
                })
            }
        }
    }

    async fn approve(&self, args: Value, by: Option<String>) -> Result<Value, String> {
        let gate = self.find_gate(&args)?;
        let chant = self.state.lock().unwrap().chant.clone().ok_or(model::NO_CHANT)?;
        let runner = self.runner().await?;
        let via = crate::gate::Via::Chant { runner: &runner, chant: &chant };
        let result = crate::gate::approve(&gate, by.as_deref(), &via).await;
        let (ok, said) = match &result {
            Ok(s) | Err(s) => (result.is_ok(), s.clone()),
        };
        log(
            &self.ctx,
            &json!({ "e": "approve", "member": gate.member, "op": gate.op, "gate": gate.gate, "by": by, "ok": ok, "said": said }),
        );
        // The block's history says who approved what (`arugula history`).
        if let Ok(mut l) = self.ctx.log() {
            let at = l.end();
            let dir = match &gate.source {
                GateSource::Chant { dir, .. } => dir,
                GateSource::Hud { box_url, .. } => box_url,
                GateSource::Forge { url, .. } => url,
            };
            let text = format!("approved {}: {} at gate {}", gate.member, gate.op, gate.gate);
            let _ = l.record(
                at,
                Event::Command {
                    at_ms: now_ms(),
                    text: Some(text),
                    cwd: Some(dir.clone()),
                    by: by.clone(),
                    kind: HistoryKind::Answer,
                },
            );
            let _ = l.record(at, Event::End { at_ms: now_ms(), exit: Some(if ok { 0 } else { 1 }) });
        }
        let said = result?;
        self.load().await;
        Ok(json!({ "approved": gate.key(), "gate": gate, "by": by, "said": said }))
    }
}

impl Block for Workspace {
    fn kind(&self) -> BlockType {
        BlockType::Workspace
    }

    fn config(&self) -> Value {
        json!({ "root": self.config.root, "env": self.config.env })
    }

    fn state(&self) -> Value {
        let mut v = serde_json::to_value(&*self.state.lock().unwrap()).unwrap_or_default();
        v["watching"] = self.live.drawn().into();
        v
    }

    fn text(&self) -> String {
        self.state.lock().unwrap().text()
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        self.call_by(method, args, None)
    }

    fn call_by(&self, method: &str, args: Value, by: Option<&str>) -> BoxFuture<'static, Result<Value, String>> {
        let me = self.me.upgrade();
        match method {
            "refresh" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                me.load().await;
                let st = me.state.lock().unwrap();
                match &st.error {
                    Some(e) => Err(e.clone()),
                    None => Ok(json!({ "members": st.members.len(), "gates": st.gates.len(), "ms": st.ms })),
                }
            }),
            "approve" => {
                let by = by.map(str::to_owned);
                Box::pin(async move { me.ok_or("closed")?.approve(args, by).await })
            }
            "member" => {
                let st = self.state.lock().unwrap();
                let name = args["name"].as_str().unwrap_or_default().to_owned();
                let m = st.members.iter().find(|m| m.name == name).map(
                    |m| json!({ "name": m.name, "path": m.path, "kind": m.kind, "nested": m.nested, "gates": m.gates }),
                );
                Box::pin(async move { m.ok_or_else(|| format!("no member {name:?}")) })
            }
            "state" => {
                let s = self.state();
                Box::pin(async move { Ok(s) })
            }
            m => {
                let e = no_method(BlockType::Workspace, m);
                Box::pin(async move { Err(e) })
            }
        }
    }

    fn drawn(&self, on: bool) {
        if let Some(round) = self.live.set(on) {
            self.watch(round);
        }
        self.ctx.changed();
    }

    fn close(&self) {
        self.live.close();
    }

    fn summary(&self) -> Summary {
        let st = self.state.lock().unwrap();
        Summary {
            project: crate::review::project(&self.config.root, self.local()),
            cwd: Some(self.config.root.clone()),
            title: Some(format!("{} (chant)", st.name.clone().unwrap_or_else(|| "workspace".into()))),
            ..Default::default()
        }
    }
}
