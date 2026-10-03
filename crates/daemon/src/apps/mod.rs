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
//! - **Gates** from hud's work board come with M34's gate type: the state
//!   has their slot (`gates`), empty in this version.
//! - **Restore.** Nothing to bring back but the config: after a restart
//!   the follower mints again, and so does each client's frame.

pub mod hud;
pub mod studio;

use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use illogical_proto::{BlockType, Project, WorkKind};
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
    /// The box's gates, from hud's work board (`/__hud/api/work`), as
    /// M34's gate attention shows them. Empty in this version.
    gates: Vec<Value>,
}

pub struct AppBlock {
    ctx: BlockCtx,
    config: Config,
    state: Arc<Mutex<State>>,
    follower: Mutex<Option<hud::Follower>>,
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
        let b = Arc::new(Self {
            ctx: ctx.clone(),
            config: config.clone(),
            state: state.clone(),
            follower: Mutex::new(None),
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
        let follower = hud::start(hud::Setup {
            origin: config.box_url.clone(),
            app: config.app.clone(),
            mint: mint(&config, None),
            on_behalf: config.follower,
            ctx,
            report,
            log,
        });
        *b.follower.lock().unwrap() = Some(follower);
        Ok(b)
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
        match method {
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
