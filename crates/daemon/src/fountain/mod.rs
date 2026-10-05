//! M43: the person's Fountain account as a block, `view: catalog`: every
//! agent as a card, with where it comes from, filters, and ways to run one.
//!
//! **Config** `{profile?, view: catalog, filter?, specs?}`: the
//! credentials profile ([`login`]), what it shows (M45 adds `runner`), the
//! filters ([`catalog::Filter`]: search, source, runtime, provider), and
//! the agent-specs checkout *Spec* looks in. Never a key.
//!
//! **Reads** go through Fountain's HTTP API ([`api`]) with the key of the
//! person's own CLI login, held in memory only. The list is read when the
//! block opens, then every [`interval`] while a client draws it (none while
//! nobody does; drawn again, it reads if the last read is that old), and on
//! `refresh`. `ILLOGICAL_FOUNTAIN_POLL_MS` sets the interval (tests).
//! Environment names come from `GET /api/environments`. The catalog is
//! read-only: agent-specs stays the one place a curated agent is edited.
//!
//! **Actions:** `run {agent}` (*Run on Fountain*: an agent block running
//! `fountain acp --agent NAME`, beside this one); `run_here {agent, cwd?,
//! prompt?}` (*Run here*, M44: a Claude Code agent block on this host
//! wearing the agent, in `cwd`, beside this one; [`wear`]); `spec {agent}` (the
//! agent-specs file that declares a `managed-by: chant` agent, as a file
//! block; for any other, or with no checkout, Fountain's page for it in a
//! browser block). `filter {...}` changes the filters, `profile {name}` the
//! login, `specs {dir}` the checkout. `agents {query?, source?}` and `agent
//! {name}` read without changing anything (MCP's `list_agents` and
//! `read_agent`).
//!
//! **`view: runner`** (M45b, [`runner`]): this host as the account's
//! Fountain runner. `GET /api/runners` every minute while drawn and every
//! 5 minutes otherwise (so its attention still fires); this host's runner
//! (the `fountain-runner` unit's `--name`) with its status, version against
//! the installed `fountain --version`, last seen and sandbox count; every
//! other runner. Attention (`failed`, bundle `failed:fountain-runner`) when
//! the unit is active but Fountain has said offline for 5 minutes, or when
//! another runner is online (it would win placement). Then this runner's
//! sandboxes (`GET /api/sandboxes`, by runner), each with its directory,
//! agent and conversations, and: `follow {conversation}` (an agent block
//! on it: `fountain acp`'s `session/load`), `changes {sandbox}` (a diff
//! block per git checkout two levels down, its git run as `fountain`
//! through sudo) and `shell {sandbox}` (a terminal as `fountain` in its
//! directory, `HOME` there). `view {view}` switches. All four are the
//! owner's.
//!
//! **The log** (`blocks/%N/`): what it was pointed at, and what was opened
//! from it.

pub mod api;
pub mod catalog;
pub mod login;
pub mod runner;
pub mod wear;

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock, Mutex, Weak},
    time::Duration,
};

use futures_util::future::BoxFuture;
use illogical_proto::{
    Action, Attention, BlockType, Reason, ReasonKind, WorkKind,
    api::{OpenRequest, RunRequest},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use self::{
    api::{Agent, Client},
    catalog::{Card, Counts, Filter, Source},
    login::Login,
};
use crate::{
    block::{Block, BlockCtx, Summary, no_method},
    review::{Live, Runner},
    store::now_ms,
};

/// How often a drawn catalog reads the list again.
pub fn interval() -> Duration {
    static AT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        std::env::var("ILLOGICAL_FOUNTAIN_POLL_MS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(POLL)
    })
}

/// A few minutes: agents change when someone applies agent-specs or an
/// app makes one, not by the second.
const POLL: Duration = Duration::from_secs(180);

/// Where *Spec* looks when the block hasn't been told, if it's there.
pub const DEFAULT_SPECS: &str = "~/agent-specs";

/// What a Fountain block shows: the agents, or this host as the runner
/// (M45b).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    #[default]
    Catalog,
    Runner,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    #[serde(default)]
    view: View,
    #[serde(default, skip_serializing_if = "Filter::is_empty")]
    filter: Filter,
    /// The agent-specs checkout (`~/…` allowed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    specs: Option<String>,
    /// Where *Run here* last ran one (M44), to offer again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    here: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct State {
    view: View,
    /// The profile in use, and the ones the credentials file has.
    profile: Option<String>,
    profiles: Vec<String>,
    base_url: Option<String>,
    /// Where the key came from: `env` or `file`.
    key_from: Option<&'static str>,
    loading: bool,
    error: Option<String>,
    /// The agents the filter shows, in the catalog's order.
    agents: Vec<Card>,
    /// How many there are in all.
    total: usize,
    /// Rows Fountain sent that didn't parse, and what to say about them.
    unreadable: usize,
    unreadable_note: Option<String>,
    filter: Filter,
    /// How many each chip would show.
    counts: Counts,
    /// The agent-specs checkout *Spec* uses, and when there's none, why.
    specs: Option<String>,
    specs_why: Option<String>,
    /// Where *Run here* last ran one.
    here: Option<String>,
    updated_ms: u64,
    polls: u64,
    watching: bool,
    /// The last action's result, for the client.
    said: Option<String>,
    /// `view: runner`'s (M45b).
    #[serde(skip_serializing_if = "Option::is_none")]
    runner: Option<runner::ViewState>,
}

/// The last list read for an account, so MCP's tools don't read it again
/// within a poll: keyed by the host the login was read on, the base URL,
/// the profile and a hash of the key, so another host's account, or a new
/// login, never gets this one's list.
static LAST: LazyLock<Mutex<HashMap<String, Recalled>>> = LazyLock::new(Mutex::default);

/// When a list was read, the list, and how many rows didn't parse.
type Recalled = (u64, Arc<Vec<Agent>>, usize);

/// The account's agents, as read (or recalled).
pub(crate) struct Agents {
    pub login: Login,
    pub agents: Arc<Vec<Agent>>,
    pub unreadable: usize,
}

fn cache_key(runner: &Runner, login: &Login) -> String {
    use sha2::Digest;
    let host = match runner {
        Runner::Local { .. } => "local".to_owned(),
        Runner::Machine { sprite, .. } => format!("machine:{sprite}"),
    };
    let key = hex::encode(sha2::Sha256::digest(login.key.as_bytes()));
    format!("{host} {} {} {}", login.base_url, login.profile, &key[..16])
}

fn remember(runner: &Runner, login: &Login, agents: &Arc<Vec<Agent>>, unreadable: usize) {
    LAST.lock().unwrap().insert(cache_key(runner, login), (now_ms(), agents.clone(), unreadable));
}

fn recalled(runner: &Runner, login: &Login, within: Duration) -> Option<(Arc<Vec<Agent>>, usize)> {
    let (at, agents, unreadable) = LAST.lock().unwrap().get(&cache_key(runner, login)).cloned()?;
    (now_ms().saturating_sub(at) <= within.as_millis() as u64).then_some((agents, unreadable))
}

/// What one read gets.
struct Read {
    login: Login,
    profiles: Vec<String>,
    agents: Arc<Vec<Agent>>,
    unreadable: usize,
    envs: BTreeMap<String, String>,
}

/// Log in on the runner's host and read the account's agents (and its
/// environments' names).
async fn read_account(runner: &Runner, profile: Option<&str>) -> Result<Read, String> {
    let found = login::read(runner).await?;
    let profiles = login::profiles(&found);
    let login = login::resolve(&found, profile)?;
    let client = Client::new(&login.base_url, &login.key);
    let (agents, envs) = tokio::join!(client.agents(), client.environments());
    let listing = agents.map_err(|e| e.to_string())?;
    let (mut agents, unreadable) = (listing.items, listing.unreadable);
    catalog::sort(&mut agents);
    let envs = match envs {
        Ok(e) => catalog::env_names(&e),
        Err(e) => {
            warn!(error = %e, "fountain environments");
            BTreeMap::new()
        }
    };
    let agents = Arc::new(agents);
    remember(runner, &login, &agents, unreadable);
    Ok(Read { login, profiles, agents, unreadable, envs })
}

/// The daemon's own host, as a block's [`Runner::user`] would be: for
/// MCP's tools, which have no block.
pub(crate) async fn local_runner(shell_env: &crate::shellenv::ShellEnv) -> Runner {
    let env: Vec<(String, String)> = std::env::vars().collect();
    let shell = shell_env.local().await;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| "/".into());
    Runner::Local { env: crate::shellenv::merge(&env, &shell, None), home }
}

/// The account's agents for MCP (`list_agents`, `read_agent`): the last
/// list read within a poll, else a fresh one.
pub(crate) async fn agents_for(runner: &Runner, profile: Option<&str>) -> Result<Agents, String> {
    let found = login::read(runner).await?;
    let login = login::resolve(&found, profile)?;
    if let Some((agents, unreadable)) = recalled(runner, &login, interval().min(POLL)) {
        return Ok(Agents { login, agents, unreadable });
    }
    let r = read_account(runner, profile).await?;
    Ok(Agents { login: r.login, agents: r.agents, unreadable: r.unreadable })
}

/// What a client or an agent is told when some rows didn't parse.
pub fn unreadable_note(n: usize) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 agent couldn't be read (Fountain sent something this illogical doesn't understand)".into()),
        n => Some(format!("{n} agents couldn't be read (Fountain sent something this illogical doesn't understand)")),
    }
}

/// An agent by name or id.
pub fn find<'a>(agents: &'a [Agent], which: &str) -> Option<&'a Agent> {
    let which = which.trim();
    agents.iter().find(|a| a.id == which).or_else(|| agents.iter().find(|a| a.name == which)).or_else(|| {
        let mut m = agents.iter().filter(|a| a.name.eq_ignore_ascii_case(which));
        let one = m.next()?;
        m.next().is_none().then_some(one)
    })
}

/// Compact rows for `list_agents`: the filter's agents as cards.
pub fn rows(agents: &[Agent], filter: &Filter) -> Vec<Card> {
    agents.iter().filter(|a| filter.matches(a)).map(|a| catalog::card(a, &BTreeMap::new())).collect()
}

/// The whole recipe for `read_agent` and the block's `agent`: as Fountain
/// returns it, a `${VAR}` left as it is, but an MCP header or env value
/// typed in literally (which may be a secret) as `<redacted>`; plus where
/// it comes from.
pub fn recipe(a: &Agent) -> Value {
    let mut v = serde_json::to_value(a.redacted()).unwrap_or_default();
    let (src, app) = catalog::source(a);
    v["source"] = json!(src);
    if let Some(app) = app {
        v["app"] = json!(app);
    }
    v
}

pub struct FountainBlock {
    ctx: BlockCtx,
    me: Weak<FountainBlock>,
    config: Mutex<Config>,
    state: Mutex<State>,
    runner: tokio::sync::OnceCell<Result<Runner, String>>,
    /// The last list read, whole (the state holds the filter's cards).
    agents: Mutex<Arc<Vec<Agent>>>,
    envs: Mutex<BTreeMap<String, String>>,
    login: Mutex<Option<Login>>,
    live: Live,
    wake: tokio::sync::Notify,
    reading: tokio::sync::Mutex<()>,
    /// The runner view's: the reason raised now, when an offline runner
    /// becomes news, and agents' names by id.
    raised: Mutex<Option<String>>,
    due: Mutex<Option<u64>>,
    agent_names: Mutex<HashMap<String, String>>,
}

impl FountainBlock {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let config: Config = serde_json::from_value(config).map_err(|e| format!("fountain config: {e}"))?;
        let state = State {
            view: config.view,
            profile: config.profile.clone(),
            filter: config.filter.clone(),
            specs: config.specs.clone(),
            here: config.here.clone(),
            loading: true,
            ..State::default()
        };
        crate::review::log(&ctx, &json!({ "e": "view", "view": config.view, "profile": config.profile }));
        let b = Arc::new_cyclic(|me| Self {
            ctx,
            me: me.clone(),
            config: Mutex::new(config),
            state: Mutex::new(state),
            runner: tokio::sync::OnceCell::new(),
            agents: Mutex::new(Arc::default()),
            envs: Mutex::new(BTreeMap::new()),
            login: Mutex::new(None),
            live: Live::default(),
            wake: tokio::sync::Notify::new(),
            reading: tokio::sync::Mutex::new(()),
            raised: Mutex::new(None),
            due: Mutex::new(None),
            agent_names: Mutex::new(HashMap::new()),
        });
        let me = b.clone();
        b.ctx.rt.spawn(async move { me.run().await });
        Ok(b)
    }

    async fn runner(&self) -> Result<Runner, String> {
        self.runner.get_or_init(|| async { Runner::user(&self.ctx).await }).await.clone()
    }

    async fn run(self: Arc<Self>) {
        self.read().await;
        loop {
            // The catalog: every interval while drawn, else until it's
            // drawn. The runner view: every minute while drawn, every 5
            // otherwise, and when an offline runner becomes news.
            match self.next_wait() {
                Some(left) => tokio::select! {
                    _ = tokio::time::sleep(left) => {}
                    _ = self.wake.notified() => {}
                },
                None => self.wake.notified().await,
            }
            if self.live.closed() {
                break;
            }
            let due = self.due.lock().unwrap().is_some_and(|d| now_ms() >= d);
            if let Some(every) = self.every()
                && (due || self.age_ms() >= every.as_millis() as u64)
            {
                self.read().await;
            }
        }
    }

    fn view(&self) -> View {
        self.config.lock().unwrap().view
    }

    /// How often it reads now; `None`: not until it's drawn.
    fn every(&self) -> Option<Duration> {
        let drawn = self.live.drawn();
        match self.view() {
            View::Catalog => drawn.then(interval),
            View::Runner => Some(if drawn { runner::interval() } else { runner::idle_interval() }),
        }
    }

    fn next_wait(&self) -> Option<Duration> {
        let mut left = self.every()?.saturating_sub(Duration::from_millis(self.age_ms()));
        if let Some(due) = *self.due.lock().unwrap() {
            left = left.min(Duration::from_millis(due.saturating_sub(now_ms()) + 50));
        }
        Some(left)
    }

    /// How long since the last read (or try).
    fn age_ms(&self) -> u64 {
        now_ms().saturating_sub(self.state.lock().unwrap().updated_ms)
    }

    async fn read(&self) {
        if self.view() == View::Runner {
            return self.read_runner().await;
        }
        let _one = self.reading.lock().await;
        if self.live.closed() {
            return;
        }
        let runner = match self.runner().await {
            Ok(r) => r,
            Err(e) => return self.fail(e),
        };
        let profile = self.config.lock().unwrap().profile.clone();
        let read = read_account(&runner, profile.as_deref()).await;
        let specs = self.specs_dir(&runner).await;
        match read {
            Ok(r) => {
                info!(pane = self.ctx.id, agents = r.agents.len(), base = r.login.base_url, "fountain catalog read");
                {
                    let mut st = self.state.lock().unwrap();
                    st.profile = Some(r.login.profile.clone());
                    st.profiles = r.profiles;
                    st.base_url = Some(r.login.base_url.clone());
                    st.unreadable = r.unreadable;
                    st.unreadable_note = unreadable_note(r.unreadable);
                    st.key_from = Some(match r.login.from {
                        login::From::Env => "env",
                        login::From::File => "file",
                    });
                    st.loading = false;
                    st.error = None;
                    st.polls += 1;
                    st.updated_ms = now_ms();
                    (st.specs, st.specs_why) = specs;
                }
                *self.login.lock().unwrap() = Some(r.login);
                *self.envs.lock().unwrap() = r.envs;
                *self.agents.lock().unwrap() = r.agents;
                self.refilter();
            }
            Err(e) => {
                // The profiles to pick from, even when the one asked for
                // isn't there.
                let profiles = login::read(&runner).await.map(|f| login::profiles(&f)).unwrap_or_default();
                {
                    let mut st = self.state.lock().unwrap();
                    st.profiles = profiles;
                    (st.specs, st.specs_why) = specs;
                }
                self.fail(e)
            }
        }
    }

    fn fail(&self, e: String) {
        {
            let mut st = self.state.lock().unwrap();
            st.loading = false;
            st.error = Some(e);
            st.updated_ms = now_ms();
        }
        self.ctx.changed();
    }

    /// The state's cards and counts, from the list and the filter.
    fn refilter(&self) {
        let filter = self.config.lock().unwrap().filter.clone();
        let agents = self.agents.lock().unwrap().clone();
        let envs = self.envs.lock().unwrap().clone();
        let cards: Vec<Card> = agents.iter().filter(|a| filter.matches(a)).map(|a| catalog::card(a, &envs)).collect();
        {
            let mut st = self.state.lock().unwrap();
            st.total = agents.len();
            st.counts = catalog::counts(&agents);
            st.agents = cards;
            st.filter = filter;
        }
        self.ctx.changed();
    }

    /// The agent-specs checkout: the configured one, else the default if
    /// it's there; and if neither, why.
    async fn specs_dir(&self, runner: &Runner) -> (Option<String>, Option<String>) {
        let given = self.config.lock().unwrap().specs.clone();
        let dir = given.clone().unwrap_or_else(|| DEFAULT_SPECS.to_owned());
        let (out, _) = runner.sh(SPECS_DIR, std::slice::from_ref(&dir)).await.unwrap_or_default();
        let out = String::from_utf8_lossy(&out);
        match out.trim().strip_prefix("ok ") {
            Some(real) => (Some(real.to_owned()), None),
            None if given.is_some() => (None, Some(format!("{dir} has no src/agents: pick the agent-specs checkout"))),
            None => (None, Some("where's your agent-specs checkout? pick it to open specs from it".into())),
        }
    }

    fn agent(&self, which: &str) -> Result<Agent, String> {
        let agents = self.agents.lock().unwrap().clone();
        if agents.is_empty() {
            let e = self.state.lock().unwrap().error.clone();
            return Err(e.unwrap_or_else(|| "the agents aren't read yet".into()));
        }
        find(&agents, which).cloned().ok_or_else(|| format!("no agent {which:?} on this Fountain account"))
    }

    fn said(&self, s: String) {
        self.state.lock().unwrap().said = Some(s);
        self.ctx.changed();
    }

    /// *Run on Fountain*: today's agent block, beside the catalog.
    async fn run_fountain(&self, args: Value) -> Result<Value, String> {
        let which = args["agent"].as_str().filter(|a| !a.is_empty()).ok_or("run needs {\"agent\": NAME}")?;
        let a = self.agent(which)?;
        let mut config = json!({ "agent": "fountain", "fountain_agent": a.name });
        if let Some(p) = self.config.lock().unwrap().profile.clone() {
            config["profile"] = json!(p);
        }
        if let Some(p) = args["prompt"].as_str().filter(|p| !p.is_empty()) {
            config["prompt"] = json!(p);
        }
        let block = self.ctx.open(self.beside(BlockType::Agent, config)).await?;
        crate::review::log(&self.ctx, &json!({ "e": "run", "agent": a.name, "block": block }));
        self.said(format!("{} runs on Fountain in %{block}", a.name));
        Ok(json!({ "block": block, "agent": a.name }))
    }

    /// *Run here* (M44): a Claude Code agent block on this host wearing the
    /// agent (its prompt, skills and MCP servers), in `cwd`, beside the
    /// catalog. Refused, with the reason, for an agent that can't be worn.
    async fn run_here(&self, args: Value) -> Result<Value, String> {
        let which = args["agent"].as_str().filter(|a| !a.is_empty()).ok_or("run_here needs {\"agent\": NAME}")?;
        let a = self.agent(which)?;
        if let Some(why) = wear::refusal(&a) {
            return Err(why);
        }
        if self.ctx.sprite.is_some() {
            return Err("Run here wears an agent on this host: open the catalog on it".into());
        }
        let given = args["cwd"].as_str().map(str::trim).filter(|c| !c.is_empty()).map(str::to_owned);
        let cwd = given.clone().or_else(|| self.config.lock().unwrap().here.clone());
        let cwd = match cwd {
            Some(c) => {
                let p = wear::expand(&c, &self.ctx.home);
                if !p.is_dir() {
                    return Err(format!("{c} isn't a directory here"));
                }
                Some(p.display().to_string())
            }
            None => None,
        };
        let (profile, specs) = {
            let c = self.config.lock().unwrap();
            (c.profile.clone(), c.specs.clone())
        };
        let mut config = json!({ "agent": "claude", "as_fountain": a.name, "cwd": cwd });
        if let Some(p) = profile {
            config["profile"] = json!(p);
        }
        if let Some(s) = specs {
            config["specs"] = json!(s);
        }
        if let Some(p) = args["prompt"].as_str().filter(|p| !p.is_empty()) {
            config["prompt"] = json!(p);
        }
        let block = self.ctx.open(self.beside(BlockType::Agent, config)).await?;
        if given.is_some() {
            self.config.lock().unwrap().here = cwd.clone();
            self.state.lock().unwrap().here = cwd.clone();
        }
        crate::review::log(&self.ctx, &json!({ "e": "run_here", "agent": a.name, "block": block, "cwd": cwd }));
        let at = cwd.as_deref().map(|c| format!(" in {c}")).unwrap_or_default();
        self.said(format!("{} is worn by a Claude Code here{at}, in %{block}", a.name));
        Ok(json!({ "block": block, "agent": a.name, "cwd": cwd }))
    }

    /// *Spec*: the file that declares a chant-managed agent, else its page
    /// on Fountain.
    async fn spec(&self, args: Value) -> Result<Value, String> {
        let which = args["agent"].as_str().filter(|a| !a.is_empty()).ok_or("spec needs {\"agent\": NAME}")?;
        let a = self.agent(which)?;
        let chant = catalog::source(&a).0 == Source::AgentSpecs;
        let runner = self.runner().await?;
        let (dir, why) = self.specs_dir(&runner).await;
        {
            let mut st = self.state.lock().unwrap();
            (st.specs, st.specs_why) = (dir.clone(), why.clone());
        }
        let mut note = None;
        if chant {
            match &dir {
                Some(d) => {
                    let (out, _) = runner.sh(SPEC_FILE, &[d.clone(), a.name.clone()]).await?;
                    let out = String::from_utf8_lossy(&out);
                    if let Some((path, line)) = out.lines().find_map(|l| l.strip_prefix("ok ")?.rsplit_once(' ')) {
                        let line: u32 = line.parse().unwrap_or(1);
                        let block =
                            self.ctx.open(self.beside(BlockType::File, json!({ "path": path, "line": line }))).await?;
                        crate::review::log(&self.ctx, &json!({ "e": "spec", "agent": a.name, "path": path }));
                        self.said(format!("{}'s spec: {path}", a.name));
                        return Ok(json!({ "block": block, "kind": "file", "path": path, "line": line }));
                    }
                    note = Some(format!("no `name: \"{}\"` under {d}/src/agents", a.name));
                }
                None => note = why,
            }
        }
        let base = self.login.lock().unwrap().as_ref().map(|l| l.base_url.clone()).ok_or("not logged in")?;
        let url = format!("{base}/agents/{}", a.id);
        let block = self.ctx.open(self.beside(BlockType::Browser, json!({ "url": url }))).await?;
        crate::review::log(&self.ctx, &json!({ "e": "spec", "agent": a.name, "url": url }));
        self.said(match &note {
            Some(n) => format!("{}: {n}; opened its page on Fountain", a.name),
            None => format!("{}'s page on Fountain", a.name),
        });
        Ok(json!({ "block": block, "kind": "browser", "url": url, "note": note }))
    }

    // ------------------------------------------------------------ runner view

    /// The runner view is this host's: not from a block on a machine.
    fn here(&self) -> Result<(), String> {
        match self.ctx.sprite {
            None => Ok(()),
            Some(_) => Err("the runner view is for this host's runner: open it on the host, not on a machine".into()),
        }
    }

    /// Read `/api/runners` and this runner's sandboxes, and judge.
    async fn read_runner(&self) {
        let _one = self.reading.lock().await;
        // Only a read that judges sets when to read again early: a failed
        // one leaves the ordinary interval (no spinning on a stale time).
        *self.due.lock().unwrap() = None;
        if self.live.closed() {
            return;
        }
        if let Err(e) = self.here() {
            return self.fail(e);
        }
        let host = match self.runner().await {
            Ok(r) => r,
            Err(e) => return self.fail(e),
        };
        let profile = self.config.lock().unwrap().profile.clone();
        let found = match login::read(&host).await {
            Ok(f) => f,
            Err(e) => return self.fail(e),
        };
        let profiles = login::profiles(&found);
        self.state.lock().unwrap().profiles = profiles;
        let login = match login::resolve(&found, profile.as_deref()) {
            Ok(l) => l,
            Err(e) => return self.fail(e),
        };
        let client = Client::new(&login.base_url, &login.key);
        let unit = runner::unit();
        let active = async {
            match &unit {
                Some(_) => runner::unit_active().await,
                None => None,
            }
        };
        let (runners, sandboxes, active, local_version) = tokio::join!(
            client.runners(),
            client.sandboxes(Some(runner::LIVE_STATES)),
            active,
            local_version(&host, unit.as_ref())
        );
        let runners = match runners {
            Ok(l) => l.items,
            Err(e) => return self.fail(e.to_string()),
        };
        let now = now_ms();
        let this = unit.as_ref().and_then(|u| runner::this_runner(&runners, u)).cloned();
        let others: Vec<runner::RunnerRow> = runners
            .iter()
            .filter(|r| this.as_ref().is_none_or(|t| t.id != r.id))
            .map(runner::RunnerRow::from)
            .collect();
        let (mut rows, sandboxes_error) = match (&this, sandboxes) {
            (None, _) => (vec![], None),
            (Some(t), Ok(l)) => {
                // The unit's root only: never one Fountain names.
                let root = unit.as_ref().and_then(|u| u.root.clone());
                (runner::rows(&l.items, &t.id, root.as_deref()), None)
            }
            (Some(_), Err(e)) => (vec![], Some(e.to_string())),
        };
        // Agents' names (Follow needs one), each read once.
        let missing: Vec<String> = {
            let names = self.agent_names.lock().unwrap();
            let mut m: Vec<String> =
                rows.iter().filter_map(|r| r.agent_id.clone()).filter(|id| !names.contains_key(id)).collect();
            m.sort();
            m.dedup();
            m
        };
        for id in missing.into_iter().take(10) {
            match client.agent(&id).await {
                Ok(a) => {
                    self.agent_names.lock().unwrap().insert(id, a.name);
                }
                Err(e) => warn!(agent = id, error = %e, "fountain agent's name"),
            }
        }
        {
            let names = self.agent_names.lock().unwrap();
            for r in &mut rows {
                r.agent = r.agent_id.as_ref().and_then(|id| names.get(id).cloned());
            }
        }
        // When this view first saw it offline while the unit runs.
        let offline = active == Some(true) && this.as_ref().is_none_or(|t| !t.online) && unit.is_some();
        let seen = {
            let st = self.state.lock().unwrap();
            let prev = st.runner.as_ref().and_then(|r| r.offline_since_ms);
            if offline { Some(prev.unwrap_or(now)) } else { None }
        };
        let grace = runner::grace();
        let want = runner::judge(&runners, unit.as_ref(), active, seen, now, grace);
        *self.due.lock().unwrap() = runner::offline_due(&runners, unit.as_ref(), active, seen, now, grace);
        let headline = want.as_ref().map(runner::Want::headline);
        if let Some(u) = &unit {
            let count = this.as_ref().map(|_| rows.len() as u32);
            runner::remember(runner::summary(&runners, u, active, count, headline.clone()));
        }
        info!(pane = self.ctx.id, runners = runners.len(), sandboxes = rows.len(), want = ?headline, "fountain runner read");
        {
            let mut st = self.state.lock().unwrap();
            st.profile = Some(login.profile.clone());
            st.base_url = Some(login.base_url.clone());
            st.key_from = Some(match login.from {
                login::From::Env => "env",
                login::From::File => "file",
            });
            st.loading = false;
            st.error = None;
            st.polls += 1;
            st.updated_ms = now;
            st.runner = Some(runner::ViewState {
                unit: unit.clone(),
                unit_active: active,
                local_version,
                this: this.as_ref().map(runner::RunnerRow::from),
                others,
                sandboxes: rows,
                sandboxes_error,
                attention: headline,
                offline_since_ms: seen,
                note: runner::PARK_NOTE,
            });
        }
        *self.login.lock().unwrap() = Some(login);
        self.raise(want, seen.unwrap_or(now));
        self.ctx.changed();
    }

    /// Ask for attention once per change, and let go when it's over.
    fn raise(&self, want: Option<runner::Want>, since: u64) {
        let now = want.as_ref().map(runner::Want::headline);
        let mut raised = self.raised.lock().unwrap();
        if *raised == now {
            return;
        }
        if raised.is_some() {
            self.ctx.clear(ReasonKind::Failed);
        }
        if let (Some(w), Some(h)) = (&want, &now) {
            let since_ms = match w {
                runner::Want::Offline { since_ms, .. } => *since_ms,
                runner::Want::Others { .. } => since,
            };
            info!(pane = self.ctx.id, headline = h, "fountain runner attention");
            self.ctx.reason(
                Attention::Done,
                Reason {
                    kind: ReasonKind::Failed,
                    since_ms,
                    headline: h.clone(),
                    command: None,
                    exit: None,
                    duration_ms: None,
                    bundle: Some(runner::BUNDLE.into()),
                    ask: None,
                    gate: None,
                    actions: vec![Action::Dismiss],
                },
            );
        }
        *raised = now;
    }

    /// A sandbox on this runner, by its id, its name, or a conversation's id.
    fn sandbox(&self, which: &str) -> Result<runner::SandboxRow, String> {
        let st = self.state.lock().unwrap();
        let v = st.runner.as_ref().ok_or("the runner isn't read yet")?;
        if v.this.is_none() {
            return Err(match &v.unit {
                Some(u) => format!("Fountain doesn't list this host's runner {}", u.name),
                None => format!("this host runs no {} unit", runner::UNIT),
            });
        }
        v.sandboxes
            .iter()
            .find(|s| s.id == which || s.name == which || s.conversations.iter().any(|c| c.id == which))
            .cloned()
            .ok_or_else(|| format!("no sandbox or conversation {which:?} on this runner"))
    }

    /// The unit's `--root`, which every sandbox must be inside.
    fn unit_root(&self) -> Result<String, String> {
        let st = self.state.lock().unwrap();
        st.runner
            .as_ref()
            .and_then(|r| r.unit.as_ref())
            .and_then(|u| u.root.clone())
            .ok_or_else(|| format!("the {} unit names no --root", runner::UNIT))
    }

    /// `view {view}`: catalog or runner.
    async fn set_view(&self, args: &Value) -> Result<Value, String> {
        let view: View = serde_json::from_value(args["view"].clone())
            .map_err(|_| "view needs {\"view\": \"catalog\" or \"runner\"}".to_owned())?;
        if view == View::Runner {
            self.here()?;
        }
        self.config.lock().unwrap().view = view;
        {
            let mut st = self.state.lock().unwrap();
            st.view = view;
            st.loading = true;
            if view == View::Catalog {
                st.runner = None;
            }
        }
        if view == View::Catalog {
            self.raise(None, now_ms());
            *self.due.lock().unwrap() = None;
        }
        crate::review::log(&self.ctx, &json!({ "e": "view", "view": view }));
        self.ctx.changed();
        self.read().await;
        self.wake.notify_one();
        let st = self.state.lock().unwrap();
        match &st.error {
            Some(e) => Err(e.clone()),
            None => Ok(json!({ "view": view })),
        }
    }

    /// *Follow*: an agent block on the conversation, through `fountain
    /// acp`'s `session/load` (its id is the session's).
    async fn follow(&self, args: Value) -> Result<Value, String> {
        self.here()?;
        let conv =
            args["conversation"].as_str().filter(|c| !c.is_empty()).ok_or("follow needs {\"conversation\": ID}")?;
        let row = self.sandbox(conv)?;
        let conv = row
            .conversations
            .iter()
            .find(|c| c.id == conv)
            .cloned()
            .ok_or_else(|| format!("{conv:?} is a sandbox: follow takes one of its conversations' ids"))?;
        let agent = row.agent.clone().ok_or("Fountain didn't say this sandbox's agent's name: Follow needs it")?;
        // `follow`: it loads this conversation or stops; never a new one.
        let mut config = json!({ "agent": "fountain", "fountain_agent": agent, "session_id": conv.id, "follow": true });
        if let Some(p) = self.config.lock().unwrap().profile.clone() {
            config["profile"] = json!(p);
        }
        let block = self.ctx.open(self.beside(BlockType::Agent, config)).await?;
        crate::review::log(
            &self.ctx,
            &json!({ "e": "follow", "conversation": conv.id, "agent": agent, "block": block }),
        );
        self.said(format!("following {agent}'s conversation {} in %{block}", conv.id));
        Ok(json!({ "block": block, "agent": agent, "conversation": conv.id }))
    }

    /// *Changes*: a diff block per git checkout in the sandbox (two levels
    /// down), its git run as `fountain`.
    async fn changes(&self, args: Value) -> Result<Value, String> {
        self.here()?;
        let which = args["sandbox"].as_str().filter(|s| !s.is_empty()).ok_or("changes needs {\"sandbox\": ID}")?;
        let row = self.sandbox(which)?;
        let dir = row.path.clone().ok_or("Fountain doesn't say where this sandbox is")?;
        let root = self.unit_root()?;
        let script = format!("{}{}", runner::GIT_SAFE, runner::CHECKOUTS);
        let (out, _) = runner::sudo_sh(&script, &[root, dir.clone()]).await?;
        let found = runner::parse_checkouts(&String::from_utf8_lossy(&out))?;
        if found.is_empty() {
            let e = format!("no git checkout in {dir} (looked two levels down)");
            self.said(e.clone());
            return Err(e);
        }
        let mut opened = vec![];
        for (repo, base) in found {
            let config = json!({ "repo": repo, "rev_a": base, "run_as": runner::USER });
            let block = self.ctx.open(self.beside(BlockType::Diff, config)).await?;
            opened.push(json!({ "block": block, "repo": repo, "rev_a": base }));
        }
        crate::review::log(&self.ctx, &json!({ "e": "changes", "sandbox": row.id, "dir": dir, "opened": opened }));
        self.said(format!("{} checkout{} in {}", opened.len(), if opened.len() == 1 { "" } else { "s" }, row.name));
        Ok(json!({ "dir": dir, "checkouts": opened }))
    }

    /// *Shell*: a terminal as `fountain` in the sandbox, `HOME` there.
    async fn shell(&self, args: Value) -> Result<Value, String> {
        self.here()?;
        let which = args["sandbox"].as_str().filter(|s| !s.is_empty()).ok_or("shell needs {\"sandbox\": ID}")?;
        let row = self.sandbox(which)?;
        let dir = row.path.clone().ok_or("Fountain doesn't say where this sandbox is")?;
        let command = runner::shell_command(&self.unit_root()?, &dir);
        let req = RunRequest {
            command: Some(command.clone()),
            split: Some(self.ctx.id),
            from_pane: Some(self.ctx.id),
            ..RunRequest::default()
        };
        let pane = self.ctx.run(req).await?;
        crate::review::log(&self.ctx, &json!({ "e": "shell", "sandbox": row.id, "dir": dir, "pane": pane }));
        self.said(format!("a shell as {} in {} (%{pane})", runner::USER, row.name));
        Ok(json!({ "pane": pane, "dir": dir, "command": command, "parked": row.parked, "note": runner::PARK_NOTE }))
    }

    fn beside(&self, kind: BlockType, config: Value) -> OpenRequest {
        OpenRequest {
            kind,
            config,
            session: None,
            split: Some(self.ctx.id),
            from_pane: Some(self.ctx.id),
            vm: false,
            image: None,
            host: None,
            local: self.ctx.sprite.is_none(),
        }
    }
}

/// `$1` a directory (`~/…` allowed): `ok REAL` if it has `src/agents`.
const SPECS_DIR: &str = r#"dir=$1
case $dir in "~") dir=$HOME ;; "~/"*) dir=$HOME/${dir#"~/"} ;; esac
[ -d "$dir/src/agents" ] && cd -- "$dir" 2>/dev/null && echo "ok $(pwd -P)""#;

/// `$1` the checkout, `$2` the agent's name: `ok PATH LINE` for the first
/// `.ts` file under `src/agents` that declares `name: "$2"`.
const SPEC_FILE: &str = r#"cd -- "$1" 2>/dev/null || exit 0
grep -rnF --include='*.ts' -e "name: \"$2\"" -e "name: '$2'" -e "name:\"$2\"" src/agents 2>/dev/null | head -n 1 | {
  IFS=: read -r f n _ && [ -n "$f" ] && echo "ok $(pwd -P)/$f $n"
}"#;

/// The installed `fountain --version` (the stand-in tests name, else the
/// unit's binary, else `fountain` on the user's PATH).
async fn local_version(host: &Runner, unit: Option<&runner::Unit>) -> Option<String> {
    let bin = std::env::var("ILLOGICAL_FOUNTAIN_BIN")
        .ok()
        .filter(|b| !b.is_empty())
        .or_else(|| unit.and_then(|u| u.bin.clone()))
        .unwrap_or_else(|| "fountain".into());
    let (out, _) = host.sh(r#""$1" --version </dev/null 2>/dev/null"#, &[bin]).await.ok()?;
    runner::parse_version(&String::from_utf8_lossy(&out))
}

impl Block for FountainBlock {
    fn kind(&self) -> BlockType {
        BlockType::Fountain
    }

    fn config(&self) -> Value {
        serde_json::to_value(&*self.config.lock().unwrap()).unwrap_or_default()
    }

    fn state(&self) -> Value {
        let mut v = serde_json::to_value(&*self.state.lock().unwrap()).unwrap_or_default();
        v["watching"] = self.live.drawn().into();
        v
    }

    fn text(&self) -> String {
        let st = self.state.lock().unwrap();
        if st.view == View::Runner {
            let mut out = match &st.runner {
                Some(r) => r.text(st.base_url.as_deref(), now_ms()),
                None => "Fountain runner\n".to_owned(),
            };
            if let Some(e) = &st.error {
                out.push_str(&format!("{e}\n"));
            }
            if st.loading && st.runner.is_none() {
                out.push_str("reading…\n");
            }
            return out;
        }
        let mut out = String::from("Fountain agents");
        if let Some(b) = &st.base_url {
            out.push_str(&format!(" on {b}"));
        }
        if let Some(p) = st.profile.as_deref().filter(|p| *p != "default") {
            out.push_str(&format!(" (profile {p})"));
        }
        out.push('\n');
        if let Some(e) = &st.error {
            out.push_str(&format!("{e}\n"));
        }
        if let Some(n) = &st.unreadable_note {
            out.push_str(&format!("{n}\n"));
        }
        if st.loading {
            out.push_str("reading…\n");
            return out;
        }
        let f = st.filter.describe();
        if f.is_empty() {
            out.push_str(&format!("{} agents\n", st.total));
        } else {
            out.push_str(&format!("{} of {} agents ({f})\n", st.agents.len(), st.total));
        }
        for c in &st.agents {
            out.push_str(&format!("  {}\n", catalog::line(c)));
        }
        out
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let me = self.me.upgrade();
        match method {
            "refresh" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                me.read().await;
                let st = me.state.lock().unwrap();
                match &st.error {
                    Some(e) => Err(e.clone()),
                    None => Ok(json!({ "total": st.total, "shown": st.agents.len() })),
                }
            }),
            "filter" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                me.config.lock().unwrap().filter.apply(&args)?;
                me.refilter();
                let st = me.state.lock().unwrap();
                Ok(json!({ "filter": st.filter, "shown": st.agents.len(), "total": st.total }))
            }),
            "profile" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                let name = args["name"].as_str().filter(|n| !n.is_empty()).map(str::to_owned);
                me.config.lock().unwrap().profile = name.clone();
                me.state.lock().unwrap().loading = true;
                me.ctx.changed();
                me.read().await;
                let st = me.state.lock().unwrap();
                match &st.error {
                    Some(e) => Err(e.clone()),
                    None => Ok(json!({ "profile": st.profile, "total": st.total })),
                }
            }),
            "specs" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                let dir = args["dir"].as_str().map(str::trim).filter(|d| !d.is_empty()).map(str::to_owned);
                me.config.lock().unwrap().specs = dir;
                let runner = me.runner().await?;
                let (dir, why) = me.specs_dir(&runner).await;
                {
                    let mut st = me.state.lock().unwrap();
                    (st.specs, st.specs_why) = (dir.clone(), why.clone());
                }
                me.ctx.changed();
                match (dir, why) {
                    (Some(d), _) => Ok(json!({ "specs": d })),
                    (None, why) => Err(why.unwrap_or_default()),
                }
            }),
            "run" | "run_fountain" => Box::pin(async move { me.ok_or("closed")?.run_fountain(args).await }),
            "run_here" => Box::pin(async move { me.ok_or("closed")?.run_here(args).await }),
            "spec" => Box::pin(async move { me.ok_or("closed")?.spec(args).await }),
            "view" => Box::pin(async move { me.ok_or("closed")?.set_view(&args).await }),
            "follow" => Box::pin(async move { me.ok_or("closed")?.follow(args).await }),
            "changes" => Box::pin(async move { me.ok_or("closed")?.changes(args).await }),
            "shell" => Box::pin(async move { me.ok_or("closed")?.shell(args).await }),
            "agents" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                let mut f = Filter::default();
                f.apply(&args)?;
                let agents = me.agents.lock().unwrap().clone();
                let rows = rows(&agents, &f);
                Ok(json!({ "total": agents.len(), "agents": rows }))
            }),
            "agent" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                let which = args["name"].as_str().or(args["agent"].as_str()).ok_or("agent needs {\"name\": NAME}")?;
                Ok(recipe(&me.agent(which)?))
            }),
            "state" => {
                let s = self.state();
                Box::pin(async move { Ok(s) })
            }
            m => {
                let e = no_method(BlockType::Fountain, m);
                Box::pin(async move { Err(e) })
            }
        }
    }

    fn drawn(&self, on: bool) {
        self.live.set(on);
        self.wake.notify_one();
        self.ctx.changed();
    }

    fn close(&self) {
        self.live.close();
        self.wake.notify_one();
    }

    fn summary(&self) -> Summary {
        let st = self.state.lock().unwrap();
        if st.view == View::Runner {
            let title = match st.runner.as_ref() {
                Some(runner::ViewState { this: Some(t), sandboxes, .. }) => format!(
                    "Fountain runner {}: {} · {} sandbox{}",
                    t.name,
                    if t.online { "online" } else { "offline" },
                    sandboxes.len(),
                    if sandboxes.len() == 1 { "" } else { "es" }
                ),
                Some(runner::ViewState { unit: Some(u), .. }) => format!("Fountain runner {}: not on Fountain", u.name),
                _ => "Fountain runners".to_owned(),
            };
            return Summary { work: Some(WorkKind::Fountain), title: Some(title), ..Summary::default() };
        }
        let title = if st.loading {
            "Fountain agents".to_owned()
        } else if st.agents.len() == st.total {
            format!("Fountain agents ({})", st.total)
        } else {
            format!("Fountain agents ({} of {})", st.agents.len(), st.total)
        };
        Summary { work: Some(WorkKind::Fountain), title: Some(title), ..Summary::default() }
    }
}
