//! M36: a pull request on a git forge as a block (S23, finished for
//! Forgejo; M38 adds GitHub, through `gh`'s login: see [`github`]).
//!
//! **Config** `{provider, api?, login?, repo, kind: pr, number, host?,
//! dir?}`: the forge, the person's `tea` login it's read and written with
//! (picked by [`login::resolve`] from `host`, the remote's or the link's,
//! and kept), the PR, and the person's clone (`dir`) for `diff` and
//! `checkout`. Besides those, what it has seen (`seen_ms`, `done_ack`,
//! `log_mark`) and the drafts still waiting. Never a token.
//!
//! **Reads** go through a provider [`Adapter`] ([`forgejo`]) onto the
//! normalized [`model`]. Forgejo sends no ETags, so a poll is the item and
//! its head's combined status (2 requests); reviews and the timeline are
//! read again only when the item's fingerprint moves. It polls every
//! [`intervals`]`.0` while a client draws it or it wants you, else every
//! `.1` (`ILLOGICAL_FORGE_POLL_MS=fast,slow` in tests).
//!
//! **Attention** (S23's rules, against `GET /user`): a review requested
//! from you is M34's `gate` reason with a `forge` source, so the rail, the
//! phone's sheet and push approve it (`allow` → `review {event:
//! approve}`); your PR's checks red is `failed` (Forgejo has no rerun API:
//! the state links the run instead of offering *Rerun*); changes requested
//! on your PR, or a mention newer than when you last looked, is `input`;
//! your PR merged, or green and not held, is `done`, once per change. All
//! bundle by repository. One reason at a time, most pressing first.
//!
//! **Writes** (`comment`, `review`, `merge`): a person's (the web, the
//! CLI) go straight out with the owner's login, naming who sent them in the
//! block's log. An agent's (MCP's `mcp:<client>`, or the CLI under Claude
//! Code: `agent: true`) become drafts: queued in the block, shown one at a
//! time as a form ask with the text to edit. The owner or an editor sends
//! (the answer, with the edited text), or drops it (decline); viewers can't
//! answer. It posts with the owner's login, and the draft and the log say
//! who sent it. "Agents draft, people send" holds on illogical's own
//! surfaces; an agent on the person's account can still run `tea` itself.
//!
//! **The log** (`blocks/%N/`): the timeline's events as JSON lines, and
//! every write as a command with who did it, so `history` and `search`
//! find a PR's comments beside the work.
//!
//! Methods: `refresh`, `login {name}`, `comment {body}`, `review {event:
//! approve|request_changes|comment, body?}`, `merge {style?}`,
//! `rerun_checks` (GitLab), `diff {dir?}`, `checkout {dir?}`, `drafts`,
//! `state`. There's no `approve`: on a block holding an ask that name
//! answers the card.
//!
//! **GitLab** (M39, [`gitlab`]): `provider: gitlab`, a merge request by its
//! `iid` in a project path (`group/sub/proj`), read with the token of the
//! person's `glab` for the host, or anonymously (public projects) when glab
//! has none: then the state's `read_only` says so and writes are refused.
//! Its failed pipeline offers *Rerun* (`rerun_checks`).

pub mod forgejo;
pub mod github;
pub mod gitlab;
pub mod issue;
pub mod live;
pub mod login;
pub mod model;

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use futures_util::future::BoxFuture;
use illogical_proto::{
    Action, Attention, BlockType, Gate, GateSource, Project, Reason, ReasonKind, WorkKind,
    api::HistoryKind,
    api::{OpenRequest, RunRequest},
    ask::{Ask, AskKind},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use self::{
    issue::{AgentLink, NewIssue},
    login::{Login, Tea, TokenSource},
    model::{Check, Event, Issue, Item, ItemKind, Me, Pr, Provider, Review, Reviewer, Want},
};
use crate::{
    block::{Block, BlockCtx, Summary, no_method},
    mux::AskReply,
    review::{Live, Runner},
    store::now_ms,
};

/// How often it polls: while drawn or wanting you, and otherwise.
pub fn intervals() -> (Duration, Duration) {
    static AT: std::sync::OnceLock<(Duration, Duration)> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        let given = std::env::var("ILLOGICAL_FORGE_POLL_MS").ok().and_then(|v| {
            let (a, b) = v.split_once(',')?;
            Some((Duration::from_millis(a.trim().parse().ok()?), Duration::from_millis(b.trim().parse().ok()?)))
        });
        given.unwrap_or((FAST, SLOW))
    })
}

/// A few seconds, as M11's drawn views.
const FAST: Duration = Duration::from_secs(5);
/// A few minutes.
const SLOW: Duration = Duration::from_secs(180);
/// Settled drafts kept in the state.
const SETTLED: usize = 20;
/// How often an issue an agent works on looks for its PR (M37).
const LINKED: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------- adapters

/// What went wrong talking to a forge.
#[derive(Debug, Clone)]
pub enum Error {
    /// No login, or no token for it.
    Login(String),
    /// 401 or 403.
    Denied(String),
    NotFound(String),
    Http(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Login(e) => write!(f, "{e}"),
            Error::Denied(e) => write!(f, "the forge refused the login: {e}"),
            Error::NotFound(e) => write!(f, "not found: {e}"),
            Error::Http(e) => write!(f, "{e}"),
        }
    }
}

/// A URL as it may be logged (no query: nothing secret is ever in one,
/// but a forge's paging is noise).
pub fn redact(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// The cheap part of a read: the item and its checks.
pub struct Polled {
    pub item: Item,
    pub checks: Vec<Check>,
    /// What changes when reviews or the timeline might have.
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

/// A write to the forge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Write {
    Comment {
        body: String,
    },
    Review {
        event: ReviewEvent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    },
    Merge {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        style: Option<String>,
    },
    /// Run the failed checks again (GitLab: retry the head pipeline).
    #[serde(rename = "rerun_checks")]
    Rerun,
    /// M40: a webhook to this daemon on the repository, made or removed
    /// with the person's login (Forgejo, GitLab).
    Live {
        on: bool,
    },
}

impl Write {
    fn from_call(method: &str, args: &Value) -> Result<Self, String> {
        let body = args["body"].as_str().map(str::to_owned);
        match method {
            "comment" => {
                let body = body.filter(|b| !b.trim().is_empty()).ok_or("comment needs {\"body\": TEXT}")?;
                Ok(Write::Comment { body })
            }
            "review" => {
                let event = serde_json::from_value(args["event"].clone()).map_err(
                    |_| "review needs {\"event\": \"approve\" | \"request_changes\" | \"comment\", \"body\"?}",
                )?;
                if event != ReviewEvent::Approve && body.as_deref().is_none_or(|b| b.trim().is_empty()) {
                    return Err("that review needs a body".into());
                }
                Ok(Write::Review { event, body: body.filter(|b| !b.trim().is_empty()) })
            }
            "merge" => {
                let style = args["style"].as_str().map(str::to_owned);
                if let Some(s) = &style
                    && !["merge", "rebase", "rebase-merge", "squash", "fast-forward-only"].contains(&s.as_str())
                {
                    return Err(format!("merge style {s}? merge, rebase, rebase-merge, squash or fast-forward-only"));
                }
                Ok(Write::Merge { style })
            }
            "rerun_checks" => Ok(Write::Rerun),
            "live" => Ok(Write::Live { on: args["on"].as_bool().ok_or("live needs {\"on\": true | false}")? }),
            m => Err(format!("{m} isn't a write")),
        }
    }

    /// "a comment", "an approval", for cards and logs.
    fn what(&self) -> &'static str {
        match self {
            Write::Comment { .. } => "a comment",
            Write::Review { event: ReviewEvent::Approve, .. } => "an approval",
            Write::Review { event: ReviewEvent::RequestChanges, .. } => "a review asking for changes",
            Write::Review { event: ReviewEvent::Comment, .. } => "a review",
            Write::Merge { .. } => "a merge",
            Write::Rerun => "a rerun of the checks",
            Write::Live { on: true } => "a webhook for live updates",
            Write::Live { on: false } => "removing the live updates webhook",
        }
    }

    fn body(&self) -> Option<&str> {
        match self {
            Write::Comment { body } => Some(body),
            Write::Review { body, .. } => body.as_deref(),
            Write::Merge { .. } | Write::Rerun | Write::Live { .. } => None,
        }
    }
}

/// What a write did.
pub struct Sent {
    pub url: Option<String>,
    pub said: String,
}

/// A forge, through one login (M38 and M39 add GitHub's and GitLab's).
pub trait Adapter: Send + Sync {
    /// Who the login is ("you").
    fn me(&self) -> BoxFuture<'_, Result<Me, Error>>;
    /// The item and its checks: what every poll reads.
    fn poll<'a>(&'a self, repo: &'a str, number: u64) -> BoxFuture<'a, Result<Polled, Error>>;
    /// Reviews, the requests still pending, and the newest events: read
    /// when a poll says something moved.
    #[allow(clippy::type_complexity)]
    fn rest<'a>(
        &'a self,
        repo: &'a str,
        number: u64,
    ) -> BoxFuture<'a, Result<(Vec<Review>, Vec<Reviewer>, Vec<Event>), Error>>;
    fn write<'a>(&'a self, repo: &'a str, number: u64, w: &'a Write) -> BoxFuture<'a, Result<Sent, Error>>;
    /// The repository's clone URLs, for matching a remote to a login.
    fn repo_urls<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<Vec<String>, Error>>;

    // M37: issues. Defaults say the forge can't yet, so an adapter that
    // doesn't read issues needs nothing more.

    /// An issue (not a PR), and what changes when its timeline might have.
    fn issue<'a>(&'a self, repo: &'a str, number: u64) -> BoxFuture<'a, Result<(Item, String), Error>> {
        let _ = (repo, number);
        Box::pin(async { Err(Error::Http("issues aren't read from this forge yet".into())) })
    }
    /// An issue's newest events, and the pull requests that refer to it.
    #[allow(clippy::type_complexity)]
    fn issue_events<'a>(
        &'a self,
        repo: &'a str,
        number: u64,
    ) -> BoxFuture<'a, Result<(Vec<Event>, Vec<model::Linked>), Error>> {
        let _ = (repo, number);
        Box::pin(async { Err(Error::Http("issues aren't read from this forge yet".into())) })
    }
    /// The newest pull request from `branch` in the repository itself.
    fn pr_by_head<'a>(&'a self, repo: &'a str, branch: &'a str) -> BoxFuture<'a, Result<Option<model::Linked>, Error>> {
        let _ = (repo, branch);
        Box::pin(async { Err(Error::Http("this forge can't look a pull request up by branch yet".into())) })
    }
    /// Open an issue: its number, and where it is.
    fn new_issue<'a>(
        &'a self,
        repo: &'a str,
        title: &'a str,
        body: &'a str,
    ) -> BoxFuture<'a, Result<(u64, Sent), Error>> {
        let _ = (repo, title, body);
        Box::pin(async { Err(Error::Http("issues can't be opened on this forge yet".into())) })
    }
    /// The branch work starts from.
    fn default_branch<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<String, Error>> {
        let _ = repo;
        Box::pin(async { Err(Error::Http("this forge doesn't say its default branch yet".into())) })
    }
    /// Why it can only read (GitLab with no glab login), if it can't write.
    fn read_only(&self) -> Option<String> {
        None
    }
    /// Whether failed checks can be run again through the API.
    fn rerun_api(&self) -> bool {
        false
    }
    /// The rate limit as last heard, and any backing off (M38: GitHub).
    fn rate(&self) -> Option<Value> {
        None
    }
    /// M40: make (`on`) or remove a repository webhook to this daemon:
    /// the forge's id for a new one.
    fn hook<'a>(
        &'a self,
        repo: &'a str,
        on: bool,
        rec: &'a live::HookRec,
    ) -> BoxFuture<'a, Result<Option<u64>, Error>> {
        let _ = (repo, on, rec);
        Box::pin(async { Err(Error::Http("this forge's webhooks don't come to the daemon".into())) })
    }
}

/// The HTTP client forge blocks (and M43's Fountain client) use: a
/// timeout, and illogical's own User-Agent.
pub(crate) fn http() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        crate::roots::http()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("illogical/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("an HTTP client")
    })
    .clone()
}

fn adapter(provider: Provider, login: &Login, tea: Arc<Tea>) -> Arc<dyn Adapter> {
    match provider {
        Provider::Forgejo => Arc::new(forgejo::Forgejo::new(&login.api(), http(), TokenSource::new(tea, login))),
        Provider::Github => {
            let host = login::url_host(&login.url).unwrap_or_default();
            let token = github::GhToken::new(tea.runner.clone(), &host);
            Arc::new(github::Github::new(&github::api_for(&host), http(), token))
        }
        // GitLab connects through glab ([`ForgeBlock::connect_gitlab`]),
        // never a tea login: anything else reads anonymously.
        Provider::Gitlab => {
            let api = format!("{}/api/v4", login.url.trim_end_matches('/'));
            let note = gitlab::read_only_note(&login::url_host(&login.url).unwrap_or_default(), "not a glab login");
            Arc::new(gitlab::Gitlab::new(&api, http(), None, Some(note)))
        }
    }
}

// ---------------------------------------------------------------- the block

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftStatus {
    #[default]
    Waiting,
    Sent,
    Dropped,
}

/// An agent's write, waiting for a person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    #[serde(flatten)]
    pub write: Write,
    /// Who drafted it (`mcp:claude-code`, `agent`).
    pub by: String,
    pub at_ms: u64,
    #[serde(default)]
    pub status: DraftStatus,
    /// Who sent or dropped it, and when.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The last send failed: why (it waits again, text kept).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn pr_kind() -> ItemKind {
    ItemKind::Pr
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    provider: Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    login: Option<String>,
    repo: String,
    #[serde(default = "pr_kind")]
    kind: ItemKind,
    number: u64,
    /// The host to pick a login by (the remote's, or the link's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    /// The person's clone, for `diff` and `checkout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dir: Option<String>,
    /// When the person last looked (mentions before it are seen).
    #[serde(default)]
    seen_ms: i64,
    /// The `done` they've seen (`merged`, `green:<sha>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    done_ack: Option<String>,
    /// Events up to here are in the log.
    #[serde(default)]
    log_mark: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    drafts: Vec<Draft>,
    /// M37: the agent working on this issue, its branch, and its PR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    link: Option<AgentLink>,
    /// M37: a new issue (`number` 0 until it's on the forge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    new: Option<NewIssue>,
}

/// What it wants of you, as the client draws it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct WantView {
    kind: &'static str,
    why: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct State {
    provider: Provider,
    /// M37: `pr` or `issue`.
    kind: ItemKind,
    repo: String,
    number: u64,
    api: Option<String>,
    login: Option<String>,
    host: Option<String>,
    dir: Option<String>,
    loading: bool,
    error: Option<String>,
    /// It can only read, and why (GitLab with no glab login: anonymous).
    read_only: Option<String>,
    /// Logins to pick from, when none (or several) matched.
    logins: Vec<LoginView>,
    /// "You", on the forge.
    me: Option<String>,
    pr: Option<Pr>,
    /// M37: the issue, for `kind: issue`.
    issue: Option<Issue>,
    /// M37: the agent on it.
    link: Option<AgentLink>,
    /// M37: a new issue, before (and after) it went out.
    new: Option<NewIssue>,
    wants: Vec<WantView>,
    /// Whether *Rerun checks* is possible here, and where the failed run
    /// is when it isn't (Forgejo).
    rerun: Option<Value>,
    drafts: Vec<Draft>,
    updated_ms: u64,
    polls: u64,
    /// M40: pokes heard (webhooks through control or straight here).
    pokes: u64,
    reads: u64,
    watching: bool,
    /// The last write's result (sent directly), for the client.
    said: Option<String>,
    /// The forge's rate limit and any backing off (M38: GitHub).
    rate: Option<Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct LoginView {
    name: String,
    url: String,
    user: String,
}

pub struct ForgeBlock {
    ctx: BlockCtx,
    me: Weak<ForgeBlock>,
    config: Mutex<Config>,
    state: Mutex<State>,
    tea: tokio::sync::OnceCell<Result<Arc<Tea>, String>>,
    adapter: Mutex<Option<Arc<dyn Adapter>>>,
    you: Mutex<Option<Me>>,
    fingerprint: Mutex<Option<String>>,
    /// The reason raised now: its kind and headline.
    raised: Mutex<Option<(ReasonKind, String)>>,
    /// The draft on the card now, and its ask's token.
    asking: Mutex<Option<(String, u64)>>,
    asking_lock: tokio::sync::Mutex<()>,
    logged: Mutex<HashSet<String>>,
    live: Live,
    wake: tokio::sync::Notify,
    /// M40: the webhook path's standing changed.
    relook: tokio::sync::Notify,
    reading: tokio::sync::Mutex<()>,
    next_draft: std::sync::atomic::AtomicU64,
    restored_mark: i64,
    /// The review gate raised now, for the card that approves it.
    gate: Mutex<Option<Gate>>,
}

impl ForgeBlock {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let mut config: Config = serde_json::from_value(config).map_err(|e| format!("forge config: {e}"))?;
        config.repo = config.repo.trim_matches('/').to_owned();
        // GitLab's projects can sit in subgroups (group/sub/proj).
        let parts = config.repo.split('/').count();
        let fits = if config.provider == Provider::Gitlab { parts >= 2 } else { parts == 2 };
        if !fits || config.repo.split('/').any(str::is_empty) {
            return Err(format!("not a repository: {:?} (owner/name)", config.repo));
        }
        if config.number == 0 && !(config.kind == ItemKind::Issue && config.new.is_some()) {
            return Err("a forge block needs the PR's (or issue's) number".into());
        }
        if config.kind == ItemKind::Issue
            && let Some(n) = &config.new
            && config.number == 0
            && n.title.trim().is_empty()
        {
            return Err("a new issue needs a title".into());
        }
        let state = State {
            provider: config.provider,
            kind: config.kind,
            link: config.link.clone(),
            new: config.new.clone(),
            repo: config.repo.clone(),
            number: config.number,
            api: config.api.clone(),
            login: config.login.clone(),
            host: config.host.clone(),
            dir: config.dir.clone(),
            // A new issue an agent drafted has nothing to read until it's
            // sent; a person's is read once it's opened.
            loading: config.number != 0
                || config.new.as_ref().is_some_and(|n| !n.agent && n.status == DraftStatus::Waiting),
            ..State::default()
        };
        crate::review::log(
            &ctx,
            &json!({ "e": "view", "repo": config.repo, "number": config.number, "api": config.api }),
        );
        let restored_mark = config.log_mark;
        let b = Arc::new_cyclic(|me| Self {
            gate: Mutex::new(None),
            restored_mark,
            ctx,
            me: me.clone(),
            config: Mutex::new(config),
            state: Mutex::new(state),
            tea: tokio::sync::OnceCell::new(),
            adapter: Mutex::new(None),
            you: Mutex::new(None),
            fingerprint: Mutex::new(None),
            // As a workspace's: the first raise clears what a restart left.
            raised: Mutex::new(Some((ReasonKind::Input, String::new()))),
            asking: Mutex::new(None),
            asking_lock: tokio::sync::Mutex::new(()),
            logged: Mutex::new(HashSet::new()),
            live: Live::default(),
            wake: tokio::sync::Notify::new(),
            relook: tokio::sync::Notify::new(),
            reading: tokio::sync::Mutex::new(()),
            next_draft: std::sync::atomic::AtomicU64::new(now_ms()),
        });
        b.sync_drafts();
        live::register(Arc::downgrade(&b));
        let me = b.clone();
        b.ctx.rt.spawn(async move { me.run().await });
        Ok(b)
    }

    fn repo(&self) -> (String, u64) {
        let c = self.config.lock().unwrap();
        (c.repo.clone(), c.number)
    }

    /// M40: what pokes are matched on: (provider, host, repo, number).
    pub fn live_key(&self) -> (Provider, String, String, u64) {
        let c = self.config.lock().unwrap();
        let api_host = || {
            c.api.as_deref().map(|a| {
                if c.provider == Provider::Github {
                    github::host_of_api(a)
                } else {
                    login::url_host(a).unwrap_or_default()
                }
            })
        };
        let host = c.host.clone().or_else(api_host).unwrap_or_default().to_ascii_lowercase();
        let host =
            if c.provider == Provider::Github && github::is_github_host(&host) { "github.com".into() } else { host };
        (c.provider, host, c.repo.clone(), c.number)
    }

    pub fn is_closed(&self) -> bool {
        self.live.closed()
    }

    /// Something changed on the forge: poll now.
    pub fn poked(&self) {
        self.state.lock().unwrap().pokes += 1;
        self.wake.notify_one();
    }

    /// The webhook path's standing changed: clients redraw, and the poll
    /// loop looks at its interval again.
    pub fn live_changed(&self) {
        self.relook.notify_one();
        self.ctx.changed();
    }

    fn standing(&self) -> live::Standing {
        let (p, host, repo, _) = self.live_key();
        live::standing(p, &host, &repo)
    }

    /// `live {on}` sent: make or remove the webhook with the owner's login.
    async fn set_hook(&self, a: &Arc<dyn Adapter>, on: bool) -> Result<Sent, Error> {
        let (p, host, repo, _) = self.live_key();
        if p == Provider::Github {
            return Err(Error::Http(
                "GitHub's live updates come through illogical control's GitHub App: join control (`illogicald join`), sign in there with GitHub and install the App; there's no webhook to make here".into(),
            ));
        }
        let had = live::hook_of(p, &host, &repo);
        if on {
            if had.as_ref().is_some_and(|r| r.id.is_some()) {
                return Ok(Sent { url: None, said: "live updates were already on".into() });
            }
            let mut rec = live::new_hook(p, &host, &repo).map_err(Error::Http)?;
            rec.id = a.hook(&repo, true, &rec).await?;
            live::keep_hook(p, &host, &repo, Some(rec)).map_err(Error::Http)?;
            self.relook.notify_one();
            Ok(Sent { url: None, said: "turned on live updates (a webhook to this daemon)".into() })
        } else {
            let Some(rec) = had else { return Ok(Sent { url: None, said: "live updates were off".into() }) };
            if rec.id.is_some() {
                a.hook(&repo, false, &rec).await?;
            }
            live::keep_hook(p, &host, &repo, None).map_err(Error::Http)?;
            self.relook.notify_one();
            Ok(Sent { url: None, said: "turned off live updates (the webhook is gone)".into() })
        }
    }

    async fn tea(&self) -> Result<Arc<Tea>, String> {
        self.tea
            .get_or_init(|| async { Runner::user(&self.ctx).await.map(|runner| Arc::new(Tea { runner })) })
            .await
            .clone()
    }

    /// The adapter, once a login is known: from config, or resolved.
    async fn connect(&self) -> Result<Arc<dyn Adapter>, String> {
        if let Some(a) = self.adapter.lock().unwrap().clone() {
            return Ok(a);
        }
        if self.config.lock().unwrap().provider == Provider::Gitlab {
            return self.connect_gitlab().await;
        }
        let tea = self.tea().await?;
        if let Some(a) = self.connect_github(&tea).await? {
            return Ok(a);
        }
        let logins = tea.logins().await?;
        let (want, host, provider, repo) = {
            let c = self.config.lock().unwrap();
            let host = c.host.clone().or_else(|| c.api.as_deref().and_then(login::url_host));
            (c.login.clone(), host, c.provider, c.repo.clone())
        };
        let login = match want {
            Some(name) => logins
                .iter()
                .find(|l| l.name == name)
                .cloned()
                .ok_or_else(|| format!("tea has no login {name:?} any more: `tea login add`, or pick another"))
                .inspect_err(|_| self.candidates(&logins))?,
            None => {
                let Some(host) = host else {
                    // No host to go by: the default login, or the only one.
                    let pick = logins.iter().find(|l| l.default).or(logins.first().filter(|_| logins.len() == 1));
                    let l = pick.cloned().ok_or_else(|| {
                        self.candidates(&logins);
                        "which forge? pick a tea login".to_owned()
                    })?;
                    return Ok(self.connected(provider, l, tea));
                };
                let lookup_tea = tea.clone();
                let lookup = move |l: Login| -> BoxFuture<'static, Result<Vec<String>, String>> {
                    let (tea, repo) = (lookup_tea.clone(), repo.clone());
                    Box::pin(
                        async move { adapter(provider, &l, tea).repo_urls(&repo).await.map_err(|e| e.to_string()) },
                    )
                };
                let r = login::resolve(logins.clone(), &host, &lookup).await;
                match r.login {
                    Some(l) => l,
                    None => {
                        self.candidates(&r.candidates);
                        return Err(r.error.unwrap_or_else(|| format!("no tea login for {host}")));
                    }
                }
            }
        };
        Ok(self.connected(provider, login, tea))
    }

    /// GitLab (M39): glab's token for the host, or anonymous and read-only.
    async fn connect_gitlab(&self) -> Result<Arc<dyn Adapter>, String> {
        let runner = Runner::user(&self.ctx).await?;
        let (host, api) = {
            let c = self.config.lock().unwrap();
            let host = c.host.clone().or_else(|| c.api.as_deref().and_then(login::url_host));
            let host = host.ok_or("which GitLab? open it from a link or a clone")?;
            let api = c.api.clone().unwrap_or_else(|| format!("https://{host}/api/v4"));
            (host, api)
        };
        let g = gitlab::connect(runner, &host, &api, http()).await;
        let a: Arc<dyn Adapter> = Arc::new(g.adapter);
        {
            let mut c = self.config.lock().unwrap();
            c.login = g.login.clone();
            c.api = Some(api.clone());
        }
        {
            let mut st = self.state.lock().unwrap();
            st.login = g.login.clone();
            st.api = Some(api);
            st.read_only = a.read_only();
            st.logins.clear();
        }
        info!(pane = self.ctx.id, host, login = g.login, "gitlab block connected");
        *self.adapter.lock().unwrap() = Some(a.clone());
        Ok(a)
    }

    fn connected(&self, provider: Provider, login: Login, tea: Arc<Tea>) -> Arc<dyn Adapter> {
        let a = adapter(provider, &login, tea);
        let api = login.api();
        self.attach(login.name, api, a)
    }

    fn attach(&self, name: String, api: String, a: Arc<dyn Adapter>) -> Arc<dyn Adapter> {
        {
            let mut c = self.config.lock().unwrap();
            c.login = Some(name.clone());
            c.api = Some(api.clone());
        }
        {
            let mut st = self.state.lock().unwrap();
            st.provider = self.config.lock().unwrap().provider;
            st.login = Some(name.clone());
            st.api = Some(api);
            st.logins.clear();
        }
        info!(pane = self.ctx.id, login = name, "forge block connected");
        *self.adapter.lock().unwrap() = Some(a.clone());
        a
    }

    /// M38: a GitHub block reads with `gh`'s login for its host. A host
    /// that isn't github.com's is GitHub Enterprise when `gh` is logged in
    /// there (asked once, when the block has no login yet).
    async fn connect_github(&self, tea: &Arc<Tea>) -> Result<Option<Arc<dyn Adapter>>, String> {
        let (provider, host, api, login) = {
            let c = self.config.lock().unwrap();
            (c.provider, c.host.clone(), c.api.clone(), c.login.clone())
        };
        let host = match provider {
            Provider::Github => login
                .clone()
                .or(host)
                .or_else(|| api.as_deref().map(github::host_of_api))
                .unwrap_or("github.com".into()),
            Provider::Gitlab => return Ok(None),
            Provider::Forgejo => {
                let Some(h) = host.filter(|_| login.is_none()) else { return Ok(None) };
                if github::is_github_host(&h) || !github::knows(&tea.runner, &h).await {
                    return Ok(None);
                }
                let mut c = self.config.lock().unwrap();
                c.provider = Provider::Github;
                c.api = Some(github::api_for(&h));
                h
            }
        };
        let api = match (provider, api) {
            (Provider::Github, Some(a)) => a,
            _ => github::api_for(&host),
        };
        let repo = self.config.lock().unwrap().repo.clone();
        let token = github::GhToken::new(tea.runner.clone(), &host).for_repo(&repo);
        let a: Arc<dyn Adapter> = Arc::new(github::Github::new(&api, http(), token));
        Ok(Some(self.attach(host, api, a)))
    }

    fn candidates(&self, logins: &[Login]) {
        self.state.lock().unwrap().logins = logins
            .iter()
            .map(|l| LoginView { name: l.name.clone(), url: l.url.clone(), user: l.user.clone() })
            .collect();
    }

    async fn run(self: Arc<Self>) {
        self.start_new().await;
        self.read(true).await;
        self.raise_draft().await;
        loop {
            let (fast, slow) = intervals();
            let wants = self.raised.lock().unwrap().as_ref().is_some_and(|(_, h)| !h.is_empty());
            let (p, host, repo, _) = self.live_key();
            let healthy = live::fresh(&live::key(p, &host, &repo));
            let wait = if let Some(left) = healthy {
                // M40: pokes say when to read; poll slowly meanwhile, and
                // look again when the webhook path would go quiet.
                slow.min(left + Duration::from_millis(20))
            } else if self.live.drawn() || wants {
                fast
            } else if self.waits_for_pr() {
                // M37: an agent works on it; its PR shows within a minute.
                slow.min(LINKED)
            } else {
                slow
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = self.wake.notified() => {}
                // The webhook path's standing changed: work the wait out again.
                _ = self.relook.notified() => {
                    self.ctx.changed();
                    continue;
                }
            }
            if self.live.closed() {
                break;
            }
            self.read(false).await;
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

    /// A poll, and a whole read when something moved (or `force`).
    async fn read(&self, force: bool) {
        let _one = self.reading.lock().await;
        if self.live.closed() {
            return;
        }
        let (repo, number) = self.repo();
        // M37: a new issue isn't on the forge yet: nothing to read.
        if number == 0 {
            return;
        }
        let a = match self.connect().await {
            Ok(a) => a,
            Err(e) => return self.fail(e),
        };
        if self.you.lock().unwrap().is_none() {
            match a.me().await {
                Ok(m) => {
                    self.state.lock().unwrap().me = Some(m.login.clone()).filter(|l| !l.is_empty());
                    *self.you.lock().unwrap() = Some(m);
                }
                Err(e) => warn!(pane = self.ctx.id, error = %e, "who am I on the forge"),
            }
        }
        if self.config.lock().unwrap().kind == ItemKind::Issue {
            return self.read_issue(a, force).await;
        }
        let polled = match a.poll(&repo, number).await {
            Ok(p) => p,
            Err(Error::Denied(e)) => {
                *self.adapter.lock().unwrap() = None;
                return self.fail(format!("the forge refused the login: {e}"));
            }
            Err(e) => return self.fail(e.to_string()),
        };
        let had = self.state.lock().unwrap().pr.clone();
        let moved = force || had.is_none() || self.fingerprint.lock().unwrap().as_deref() != Some(&polled.fingerprint);
        let (reviews, requested, events) = if moved {
            match a.rest(&repo, number).await {
                Ok(r) => r,
                Err(e) => return self.fail(e.to_string()),
            }
        } else {
            let h = had.clone().unwrap_or_default();
            (h.reviews, h.item.requested, h.events)
        };
        *self.fingerprint.lock().unwrap() = Some(polled.fingerprint);
        let mut item = polled.item;
        item.requested = requested;
        let rollup = model::rollup(&polled.checks);
        let pr = Pr { item, reviews, checks: polled.checks, rollup, events };
        self.log_events(&pr.events);
        {
            let mut st = self.state.lock().unwrap();
            st.loading = false;
            st.error = None;
            st.polls += 1;
            if moved {
                st.reads += 1;
            }
            st.updated_ms = now_ms();
            st.read_only = a.read_only();
            st.rerun = rerun(&pr, a.rerun_api() && st.read_only.is_none());
            st.rate = a.rate();
            st.pr = Some(pr);
        }
        self.after_read(had.is_none());
    }

    /// What every read ends with: what's seen, and what's raised.
    fn after_read(&self, first: bool) {
        if self.live.drawn() {
            self.look();
        } else if first && !self.ctx.restoring {
            // `done` is for a change: a PR opened already merged (or
            // green) is seen as it is. After a restart, what changed while
            // the daemon was down still is one.
            let done = self.wants().into_iter().find_map(|w| match w {
                Want::Done { key, .. } => Some(key),
                _ => None,
            });
            let mut c = self.config.lock().unwrap();
            if c.done_ack.is_none() {
                c.done_ack = done;
            }
        }
        self.raise();
        self.ctx.changed();
    }

    /// Events not yet in the log go there (those up to the mark it had when
    /// made were logged before a restart).
    fn log_events(&self, events: &[Event]) {
        let mut logged = self.logged.lock().unwrap();
        let mut top = self.config.lock().unwrap().log_mark;
        for e in events {
            if !logged.insert(e.id.clone()) || (self.restored_mark > 0 && e.at <= self.restored_mark) {
                continue;
            }
            crate::review::log(&self.ctx, &json!({ "e": "event", "event": e, "line": e.line() }));
            top = top.max(e.at);
        }
        self.config.lock().unwrap().log_mark = top;
    }

    /// The person looks at it now: mentions so far are seen, and so is
    /// what's done.
    fn look(&self) {
        let done = self.wants().into_iter().find_map(|w| match w {
            Want::Done { key, .. } => Some(key),
            _ => None,
        });
        let mut c = self.config.lock().unwrap();
        c.seen_ms = now_ms() as i64;
        if done.is_some() {
            c.done_ack = done;
        }
    }

    fn wants(&self) -> Vec<Want> {
        let st = self.state.lock().unwrap();
        let Some(me) = self.you.lock().unwrap().clone() else { return vec![] };
        let seen = self.config.lock().unwrap().seen_ms;
        if let Some(issue) = st.issue.as_ref() {
            return model::issue_attention(issue, &me, seen);
        }
        let Some(pr) = st.pr.as_ref() else { return vec![] };
        model::attention(pr, &me, seen)
    }

    /// The item's link and title (a PR's or an issue's).
    fn headline(&self) -> (String, String) {
        let st = self.state.lock().unwrap();
        match (st.pr.as_ref(), st.issue.as_ref()) {
            (Some(p), _) => (p.item.url.clone(), p.item.title.clone()),
            (None, Some(i)) => (i.item.url.clone(), i.item.title.clone()),
            (None, None) => (String::new(), String::new()),
        }
    }

    /// The reason for the most pressing want, as `(state, reason)`.
    fn top(&self, wants: &[Want]) -> Option<(Attention, Reason)> {
        let (repo, number, api, ack) = {
            let c = self.config.lock().unwrap();
            (c.repo.clone(), c.number, c.api.clone().unwrap_or_default(), c.done_ack.clone())
        };
        let (url, title) = self.headline();
        let bundle = format!("forge:{}/{repo}", illogical_proto::host_of(&api));
        let plain = |kind: ReasonKind, why: &str| Reason {
            kind,
            since_ms: now_ms(),
            headline: format!("{repo}#{number} {why}: {title}"),
            command: None,
            exit: None,
            duration_ms: None,
            bundle: Some(bundle.clone()),
            ask: None,
            gate: None,
            actions: vec![Action::Dismiss],
        };
        let w = wants.iter().find(|w| !matches!(w, Want::Done { key, .. } if Some(key) == ack.as_ref()))?;
        Some(match w {
            Want::Review { why } => {
                let gate = Gate {
                    member: repo.clone(),
                    op: format!("#{number}"),
                    gate: "review".into(),
                    env: None,
                    since: None,
                    expires: None,
                    approvals: 0,
                    needed: 1,
                    command: Some(format!("illogical call %{} review '{{\"event\":\"approve\"}}'", self.ctx.id)),
                    source: GateSource::Forge { api, url, number },
                };
                *self.gate.lock().unwrap() = Some(gate.clone());
                let mut r = crate::gate::reason(&[gate])?;
                r.headline = format!("{repo}#{number} {why}: {title}");
                (Attention::NeedsInput, r)
            }
            Want::Failed { why, .. } => {
                // Like a failed command, it's `done` with a `failed` reason;
                // Forgejo can't rerun, so there's no `rerun` action. GitLab
                // retries the pipeline (`rerun_checks`).
                let mut r = plain(ReasonKind::Failed, why);
                r.bundle = Some(format!("failed:{bundle}"));
                if self.state.lock().unwrap().rerun.as_ref().is_some_and(|v| v["api"] == true) {
                    r.actions.insert(0, Action::Rerun);
                }
                (Attention::Done, r)
            }
            Want::Changes { why } | Want::Mention { why, .. } | Want::Assigned { why } => {
                (Attention::NeedsInput, plain(ReasonKind::Input, why))
            }
            Want::Done { why, .. } => (Attention::Done, plain(ReasonKind::Done, why)),
        })
    }

    /// Ask for attention for what it wants now, once per change.
    fn raise(&self) {
        let wants = self.wants();
        {
            let mut st = self.state.lock().unwrap();
            st.wants = wants
                .iter()
                .map(|w| WantView {
                    kind: match w {
                        Want::Review { .. } => "review",
                        Want::Failed { .. } => "failed",
                        Want::Changes { .. } => "changes",
                        Want::Mention { .. } => "mention",
                        Want::Done { .. } => "done",
                        Want::Assigned { .. } => "assigned",
                    },
                    why: w.why().to_owned(),
                })
                .collect();
        }
        let top = self.top(&wants);
        // A draft on the card holds the block's attention; a `done` or
        // `failed` (which aren't needs-input) waits for it to settle.
        if self.asking.lock().unwrap().is_some() && top.as_ref().is_some_and(|(a, _)| *a == Attention::Done) {
            return;
        }
        let now = top.as_ref().map(|(_, r)| (r.kind, r.headline.clone()));
        let mut raised = self.raised.lock().unwrap();
        if *raised == now {
            return;
        }
        // A different kind replaces the reason: let go of the old one first
        // (the multiplexer keeps a needs-input reason over a newer input).
        if let Some((old, _)) = raised.as_ref()
            && now.as_ref().is_none_or(|(k, _)| k != old)
        {
            self.ctx.clear(*old);
            if *old == ReasonKind::Input {
                // ...and what a restart may have left of the others.
                for k in [ReasonKind::Gate, ReasonKind::Failed, ReasonKind::Done] {
                    if now.as_ref().is_none_or(|(n, _)| *n != k) {
                        self.ctx.clear(k);
                    }
                }
            }
        }
        if let Some((state, r)) = top {
            info!(pane = self.ctx.id, kind = ?r.kind, "forge attention");
            self.ctx.reason(state, r);
        }
        *raised = now;
    }

    /// Raise it again from scratch (after a draft's card closed, which
    /// leaves the block idle).
    fn reassert(&self) {
        let old = self.raised.lock().unwrap().take();
        if let Some((k, _)) = old {
            *self.raised.lock().unwrap() = Some((k, String::new()));
        }
        self.raise();
    }

    // ------------------------------------------------------------ writes

    /// A write: an agent's becomes a draft; a person's goes out now.
    async fn write(&self, method: &str, args: Value, by: Option<String>) -> Result<Value, String> {
        let w = Write::from_call(method, &args)?;
        if let Some(why) = self.adapter.lock().unwrap().as_ref().and_then(|a| a.read_only()) {
            return Err(why);
        }
        let agent = args["agent"] == true || by.as_deref().is_some_and(|b| b.starts_with("mcp:"));
        if agent {
            let id = format!("d{}", self.next_draft.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
            // MCP names its client; the CLI under an agent runs as the
            // owner, so "an agent" is all that's known.
            let by = by.filter(|b| b.starts_with("mcp:")).unwrap_or_else(|| "an agent".into());
            info!(pane = self.ctx.id, id, by, what = w.what(), "forge draft");
            crate::review::log(&self.ctx, &json!({ "e": "drafted", "id": id, "by": by, "write": w }));
            let d = Draft {
                id: id.clone(),
                write: w,
                by,
                at_ms: now_ms(),
                status: DraftStatus::Waiting,
                settled_by: None,
                settled_ms: None,
                url: None,
                error: None,
            };
            self.config.lock().unwrap().drafts.push(d);
            self.sync_drafts();
            self.ctx.changed();
            if let Some(me) = self.me.upgrade() {
                self.ctx.rt.spawn(async move { me.raise_draft().await });
            }
            let waiting = self.config.lock().unwrap().drafts.len();
            return Ok(json!({ "draft": id, "status": "waiting", "queued": waiting,
                "note": "a person sends, edits or drops it; read_pr (or describe) shows what became of it" }));
        }
        // Approving the review asked of you (the rail's `allow`): its card
        // closes saying who.
        let gate = args["key"].as_str().and_then(|k| self.gate.lock().unwrap().clone().filter(|g| g.key() == k));
        let sent = self.send(&w, by.as_deref(), None).await?;
        let mut out = json!({ "sent": w.what(), "url": sent.url, "said": sent.said, "by": by });
        if let Some(g) = gate {
            out["gate"] = json!(g);
        }
        Ok(out)
    }

    /// Send a write as the owner's login, saying who sent it (and who
    /// drafted it).
    async fn send(&self, w: &Write, by: Option<&str>, drafted: Option<&str>) -> Result<Sent, String> {
        let a = self.connect().await?;
        let (repo, number) = self.repo();
        let r = match w {
            Write::Live { on } => self.set_hook(&a, *on).await,
            _ => a.write(&repo, number, w).await,
        }
        .map_err(|e| e.to_string());
        let ok = r.is_ok();
        let url = r.as_ref().ok().and_then(|s| s.url.clone());
        let err = r.as_ref().err().cloned();
        crate::review::log(
            &self.ctx,
            &json!({ "e": "sent", "write": w, "by": by, "drafted_by": drafted, "ok": ok, "url": url, "error": err }),
        );
        if let Ok(mut l) = self.ctx.log() {
            let at = l.end();
            let mut text = format!("{} on {repo}#{number}", w.what());
            if let Some(d) = drafted {
                text.push_str(&format!(" (drafted by {d})"));
            }
            if let Some(b) = w.body() {
                text.push_str(&format!(": {}", b.lines().next().unwrap_or("")));
            }
            let _ = l.record(
                at,
                crate::store::Event::Command {
                    at_ms: now_ms(),
                    text: Some(text),
                    cwd: None,
                    by: by.map(str::to_owned),
                    kind: HistoryKind::Command,
                },
            );
            let _ = l.record(at, crate::store::Event::End { at_ms: now_ms(), exit: Some(if ok { 0 } else { 1 }) });
        }
        let sent = r?;
        self.state.lock().unwrap().said = Some(format!("{} by {}", sent.said, by.unwrap_or("the owner")));
        // What it changed, read now.
        if let Some(me) = self.me.upgrade() {
            self.ctx.rt.spawn(async move { me.read(true).await });
        }
        Ok(sent)
    }

    /// The state's drafts: the waiting ones (from config), then the newest
    /// settled ones.
    fn sync_drafts(&self) {
        let waiting = self.config.lock().unwrap().drafts.clone();
        let mut st = self.state.lock().unwrap();
        let settled: Vec<Draft> = st.drafts.iter().filter(|d| d.status != DraftStatus::Waiting).cloned().collect();
        st.drafts = waiting;
        let keep = settled.len().saturating_sub(SETTLED);
        st.drafts.extend(settled.into_iter().skip(keep));
    }

    /// The oldest waiting draft on the card, if none is.
    fn raise_draft(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.raise_draft_now())
    }

    async fn raise_draft_now(&self) {
        let _one = self.asking_lock.lock().await;
        if self.asking.lock().unwrap().is_some() || self.live.closed() {
            return;
        }
        let Some(d) = self.config.lock().unwrap().drafts.first().cloned() else { return };
        let (repo, number) = self.repo();
        let ask = draft_ask(&d, &repo, number);
        match self.ctx.ask(ask).await {
            Ok((token, rx)) => {
                *self.asking.lock().unwrap() = Some((d.id.clone(), token));
                let Some(me) = self.me.upgrade() else { return };
                let settle: BoxFuture<'static, ()> = Box::pin(async move {
                    let (reply, by) = rx.await.unwrap_or((AskReply::Withdrawn, None));
                    me.settle(&d.id, token, reply, by).await;
                });
                self.ctx.rt.spawn(settle);
            }
            Err(e) => warn!(pane = self.ctx.id, error = e, "can't show the draft"),
        }
    }

    /// A person answered a draft's card.
    async fn settle(&self, id: &str, token: u64, reply: AskReply, by: Option<illogical_proto::Driver>) {
        {
            let mut asking = self.asking.lock().unwrap();
            if asking.as_ref() != Some(&(id.to_owned(), token)) {
                return;
            }
            *asking = None;
        }
        let Some(d) = self.config.lock().unwrap().drafts.iter().find(|d| d.id == id).cloned() else { return };
        let name = by.as_ref().map(|b| b.name.clone());
        match reply {
            AskReply::Answer(content) => {
                let w = edited(&d.write, &content);
                match self.send(&w, name.as_deref(), Some(&d.by)).await {
                    Ok(sent) => {
                        info!(pane = self.ctx.id, id, by = name, "draft sent");
                        self.settled(Draft {
                            write: w,
                            status: DraftStatus::Sent,
                            settled_by: name,
                            settled_ms: Some(now_ms()),
                            url: sent.url,
                            error: None,
                            ..d
                        });
                    }
                    Err(e) => {
                        // It waits again, with its text, saying why.
                        warn!(pane = self.ctx.id, id, error = e, "draft not sent");
                        let mut c = self.config.lock().unwrap();
                        if let Some(x) = c.drafts.iter_mut().find(|x| x.id == id) {
                            x.write = w;
                            x.error = Some(e);
                        }
                    }
                }
            }
            AskReply::Decline => {
                crate::review::log(&self.ctx, &json!({ "e": "dropped", "id": id, "by": name }));
                self.settled(Draft { status: DraftStatus::Dropped, settled_by: name, settled_ms: Some(now_ms()), ..d });
            }
            // Gone from the card some other way (the block closing): it
            // waits, and comes back.
            _ => {}
        }
        self.sync_drafts();
        self.ctx.changed();
        self.reassert();
        if !self.live.closed() {
            self.raise_draft().await;
        }
    }

    fn settled(&self, d: Draft) {
        self.config.lock().unwrap().drafts.retain(|x| x.id != d.id);
        self.state.lock().unwrap().drafts.push(d);
    }

    // ------------------------------------------------------------ the code

    /// Fetch the PR's head into the person's clone and make (or update) a
    /// worktree of it. `(worktree, merge base)`.
    async fn worktree(&self, args: &Value) -> Result<(String, String), String> {
        let dir = args["dir"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| self.config.lock().unwrap().dir.clone())
            .ok_or_else(|| {
                let (repo, n) = self.repo();
                format!(
                    "no clone of {repo} known here: give {{\"dir\": …}}, or open it from one (`illogical pr {n}` there)"
                )
            })?;
        let pr = self.state.lock().unwrap().pr.clone().ok_or("not read yet: try again in a moment")?;
        let (repo, number) = self.repo();
        let runner = Runner::user(&self.ctx).await?;
        let args = [
            dir.clone(),
            number.to_string(),
            pr.item.head_ref.clone(),
            pr.item.base.branch.clone(),
            repo.clone(),
            pr.item.merge_base.clone().unwrap_or_default(),
        ];
        let (out, _) = runner.sh(WORKTREE, &args).await?;
        let out = String::from_utf8_lossy(&out);
        let last = out.lines().last().unwrap_or_default();
        if let Some(rest) = last.strip_prefix("ok ") {
            let (wt, mb) = rest.rsplit_once(' ').ok_or("the worktree script said something else")?;
            self.config.lock().unwrap().dir.get_or_insert(dir);
            crate::review::log(&self.ctx, &json!({ "e": "worktree", "path": wt, "merge_base": mb }));
            return Ok((wt.to_owned(), mb.to_owned()));
        }
        Err(last.strip_prefix("err ").unwrap_or(last).to_owned())
    }

    async fn diff(&self, args: Value) -> Result<Value, String> {
        let (wt, mb) = self.worktree(&args).await?;
        let (_, number) = self.repo();
        let req = OpenRequest {
            kind: BlockType::Diff,
            config: json!({ "repo": wt, "rev_a": mb, "rev_b": format!("refs/illogical/pr/{number}") }),
            session: None,
            split: Some(self.ctx.id),
            from_pane: Some(self.ctx.id),
            vm: false,
            image: None,
            host: None,
            local: self.ctx.sprite.is_none(),
        };
        let block = self.ctx.open(req).await?;
        Ok(json!({ "block": block, "worktree": wt, "rev_a": mb, "rev_b": format!("refs/illogical/pr/{number}") }))
    }

    async fn checkout(&self, args: Value) -> Result<Value, String> {
        let (wt, _) = self.worktree(&args).await?;
        let req = RunRequest {
            cwd: Some(wt.clone()),
            split: Some(self.ctx.id),
            from_pane: Some(self.ctx.id),
            join: self.ctx.sprite.is_some(),
            ..RunRequest::default()
        };
        let pane = self.ctx.run(req).await?;
        Ok(json!({ "pane": pane, "worktree": wt }))
    }
}

/// `$1` the clone, `$2` the number, `$3` the head ref, `$4` the base
/// branch, `$5` owner/name, `$6` the forge's merge base. Fetches the head
/// to `refs/illogical/pr/N` (no branch is touched), the base to
/// `refs/illogical/pr/N-base`, and makes a detached worktree in
/// `.illogical/worktrees/pr-N` (or `.claude/worktrees/pr-N` where the repo
/// keeps its worktrees there), left alone if it has changes of its own.
/// Says `ok WORKTREE MERGE_BASE` or `err WHY` last.
const WORKTREE: &str = r#"dir=$1; n=$2; head=$3; base=$4; repo=$5; mb=$6
case $dir in "~") dir=$HOME ;; "~/"*) dir=$HOME/${dir#"~/"} ;; esac
cd -- "$dir" 2>/dev/null || { printf 'err no such directory: %s\n' "$dir"; exit 0; }
top=$(git rev-parse --show-toplevel 2>/dev/null) || { printf 'err not a git repository: %s\n' "$dir"; exit 0; }
cd -- "$top" || exit 0
remote=
for r in $(git remote); do
  case $(git remote get-url "$r" 2>/dev/null) in *"/$repo"|*"/$repo.git"|*":$repo"|*":$repo.git"|*"/$repo/") remote=$r; break ;; esac
done
[ -n "$remote" ] || remote=origin
out=$(git fetch -q --no-tags "$remote" "+$head:refs/illogical/pr/$n" "+refs/heads/$base:refs/illogical/pr/$n-base" 2>&1) ||
  { printf 'err git fetch %s failed: %s\n' "$remote" "$(printf '%s' "$out" | tail -n 1)"; exit 0; }
if [ -d .claude/worktrees ]; then w=.claude/worktrees/pr-$n; else
  w=.illogical/worktrees/pr-$n; mkdir -p .illogical/worktrees
  ex=$(git rev-parse --git-common-dir)/info/exclude; mkdir -p "$(dirname "$ex")"
  grep -qx '.illogical/' "$ex" 2>/dev/null || echo '.illogical/' >> "$ex"
fi
if [ -e "$w/.git" ]; then
  if [ -z "$(git -C "$w" status --porcelain 2>/dev/null)" ] && ! git -C "$w" symbolic-ref -q HEAD >/dev/null; then
    git -C "$w" checkout -q --detach "refs/illogical/pr/$n" 2>/dev/null
  fi
else
  out=$(git worktree add -q --detach "$w" "refs/illogical/pr/$n" 2>&1) || { printf 'err git worktree add failed: %s\n' "$(printf '%s' "$out" | tail -n 1)"; exit 0; }
fi
if [ -z "$mb" ] || ! git cat-file -e "$mb^{commit}" 2>/dev/null; then
  mb=$(git merge-base "refs/illogical/pr/$n-base" "refs/illogical/pr/$n" 2>/dev/null) || { echo 'err no merge base between the PR and its base'; exit 0; }
fi
printf 'ok %s/%s %s\n' "$top" "$w" "$mb"
"#;

/// What the failed checks' rerun is: through the API (`api`: GitLab, as
/// `rerun_checks`), or on a forge with none (or no login), the run's page.
fn rerun(pr: &Pr, api: bool) -> Option<Value> {
    if pr.rollup != Some(model::CheckState::Failure) {
        return None;
    }
    let red = pr.checks.iter().find(|c| c.state.red() && !c.allow_failure);
    let url = red.and_then(|c| c.url.clone());
    if api && red.is_some_and(|c| c.source == model::CheckSource::CheckRun) {
        // M38: GitHub reruns each red workflow run's failed jobs.
        let runs = pr.checks.iter().filter(|c| c.state.red() && c.run.is_some()).count();
        return Some(json!({ "api": true, "url": url, "runs": runs,
            "note": "Rerun reruns the failed jobs of each red workflow run" }));
    }
    if api {
        let pipeline = red.and_then(|c| c.run.as_ref()).map(|r| r.id.clone());
        return Some(json!({ "api": true, "url": url, "pipeline": pipeline,
            "note": "Rerun retries the pipeline's failed jobs" }));
    }
    let note = match pr.checks.first().map(|c| c.source) {
        Some(model::CheckSource::PipelineJob) => "Rerun needs a glab login: rerun them on the pipeline's page",
        _ => "Forgejo has no API to rerun checks: rerun them on the run's page",
    };
    Some(json!({ "api": false, "url": url, "note": note }))
}

/// A draft as a form card: the text to edit (and a review's event).
fn draft_ask(d: &Draft, repo: &str, number: u64) -> Ask {
    let who = d.by.strip_prefix("mcp:").unwrap_or(&d.by).to_owned();
    let mut props = serde_json::Map::new();
    if let Write::Review { event, .. } = &d.write {
        props.insert(
            "event".into(),
            json!({ "type": "string", "title": "Review", "default": event,
                "oneOf": [{ "const": "approve", "title": "Approve" }, { "const": "request_changes", "title": "Request changes" }, { "const": "comment", "title": "Comment" }] }),
        );
    }
    let mut required = vec![];
    if let Some(body) =
        d.write.body().map(str::to_owned).or_else(|| matches!(d.write, Write::Review { .. }).then(String::new))
    {
        props.insert(
            "body".into(),
            json!({ "type": "string", "title": "Text", "default": body, "format": "markdown",
                "description": "Edit it before sending; it goes out with your forge login" }),
        );
        if matches!(d.write, Write::Comment { .. }) {
            required.push("body");
        }
    }
    let mut message = format!("{who} drafted {} on {repo}#{number}", d.write.what());
    if let Write::Merge { style } = &d.write {
        message.push_str(&format!(" ({})", style.as_deref().unwrap_or("merge")));
    }
    if let Some(e) = &d.error {
        message.push_str(&format!(". Sending it failed: {e}"));
    }
    Ask {
        id: d.id.clone(),
        kind: AskKind::Form,
        message,
        questions: None,
        schema: Some(json!({ "type": "object", "properties": props, "required": required })),
        url: None,
        accepted: false,
        tool_call_id: None,
        source: "forge".into(),
        agent: Some(who),
        at_ms: d.at_ms,
        tool: None,
        input: None,
        suggestions: None,
        session: None,
    }
}

/// A draft with the card's edits.
fn edited(w: &Write, content: &Value) -> Write {
    let body = content["body"].as_str().map(str::to_owned);
    match w {
        Write::Comment { body: b } => {
            Write::Comment { body: body.filter(|x| !x.trim().is_empty()).unwrap_or_else(|| b.clone()) }
        }
        Write::Review { event, body: b } => Write::Review {
            event: serde_json::from_value(content["event"].clone()).unwrap_or(*event),
            body: match body {
                Some(x) if x.trim().is_empty() => None,
                Some(x) => Some(x),
                None => b.clone(),
            },
        },
        Write::Merge { style } => Write::Merge { style: style.clone() },
        Write::Rerun => Write::Rerun,
        Write::Live { on } => Write::Live { on: *on },
    }
}

impl Block for ForgeBlock {
    fn kind(&self) -> BlockType {
        BlockType::Forge
    }

    fn config(&self) -> Value {
        serde_json::to_value(&*self.config.lock().unwrap()).unwrap_or_default()
    }

    fn state(&self) -> Value {
        let mut v = serde_json::to_value(&*self.state.lock().unwrap()).unwrap_or_default();
        v["watching"] = self.live.drawn().into();
        // M40: webhook or polling, and why.
        let s = self.standing();
        v["live"] = json!(s.live);
        v["live_via"] = json!(s.via);
        v["live_why"] = json!(s.why);
        v["live_heard_ms"] = json!(s.heard_ms);
        v["hook"] = json!(s.hook);
        v
    }

    fn text(&self) -> String {
        let st = self.state.lock().unwrap();
        let mut out = match (&st.pr, &st.issue, &st.error) {
            (Some(pr), _, _) => pr.text(&st.repo),
            (None, Some(i), _) => i.text(&st.repo),
            (None, None, Some(e)) => format!("{}#{}\n{e}\n", st.repo, st.number),
            (None, None, None) if st.number == 0 => String::new(),
            (None, None, None) => format!("{}#{}\nreading…\n", st.repo, st.number),
        };
        out.push_str(&issue::text(&st));
        let live = self.standing();
        out.push_str(&match &live.why {
            Some(w) => format!("live: {} ({w})\n", live.live),
            None => format!("live: {}\n", live.live),
        });
        if let Some(r) = &st.read_only {
            out.push_str(&format!("{r}\n"));
        }
        if !st.wants.is_empty() {
            out.push_str("waiting on you:\n");
            for w in &st.wants {
                out.push_str(&format!("  {}: {}\n", w.kind, w.why));
            }
        }
        let waiting: Vec<&Draft> = st.drafts.iter().filter(|d| d.status == DraftStatus::Waiting).collect();
        if !waiting.is_empty() {
            out.push_str("drafts waiting for a person:\n");
            for d in waiting {
                let b = d.write.body().map(|b| format!(": {}", b.lines().next().unwrap_or(""))).unwrap_or_default();
                out.push_str(&format!("  {} {} by {}{b}\n", d.id, d.write.what(), d.by));
            }
        }
        out
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        self.call_by(method, args, None)
    }

    fn call_by(&self, method: &str, args: Value, by: Option<&str>) -> BoxFuture<'static, Result<Value, String>> {
        let me = self.me.upgrade();
        let by = by.map(str::to_owned);
        match method {
            "refresh" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                *me.adapter.lock().unwrap() = None;
                me.read(true).await;
                let st = me.state.lock().unwrap();
                match &st.error {
                    Some(e) => Err(e.clone()),
                    None => Ok(json!({ "reads": st.reads, "polls": st.polls })),
                }
            }),
            "login" => Box::pin(async move {
                let me = me.ok_or("closed")?;
                let name = args["name"].as_str().filter(|n| !n.is_empty()).ok_or("login needs {\"name\": LOGIN}")?;
                if me.config.lock().unwrap().provider == Provider::Github {
                    return Err("a GitHub block reads with gh's login for its host: `gh auth login` or `gh auth switch` there, then refresh".into());
                }
                if me.config.lock().unwrap().provider == Provider::Gitlab {
                    return Err("a GitLab block uses glab's login for its host: `glab auth login`, then refresh".into());
                }
                let tea = me.tea().await?;
                let logins = tea.logins().await?;
                let l = logins
                    .iter()
                    .find(|l| l.name == name)
                    .cloned()
                    .ok_or_else(|| format!("tea has no login {name:?}"))?;
                *me.you.lock().unwrap() = None;
                let provider = me.config.lock().unwrap().provider;
                me.connected(provider, l, tea);
                me.read(true).await;
                me.ctx.changed();
                Ok(json!({ "login": name }))
            }),
            "review" | "merge" | "rerun_checks" if self.config.lock().unwrap().kind == ItemKind::Issue => {
                let e = format!("an issue has no {method}: comment on it, or open its pull request");
                Box::pin(async move { Err(e) })
            }
            "comment" | "review" | "merge" | "rerun_checks" if self.config.lock().unwrap().number == 0 => {
                Box::pin(async move { Err("this issue isn't on the forge yet".into()) })
            }
            "comment" | "review" | "merge" | "rerun_checks" | "live" => {
                let method = method.to_owned();
                Box::pin(async move { me.ok_or("closed")?.write(&method, args, by).await })
            }
            // M37: an agent on this issue, in a worktree and a tab.
            "agent" => Box::pin(async move { me.ok_or("closed")?.agent_on(args).await }),
            "diff" => Box::pin(async move { me.ok_or("closed")?.diff(args).await }),
            "checkout" => Box::pin(async move { me.ok_or("closed")?.checkout(args).await }),
            "drafts" => {
                let d = self.state.lock().unwrap().drafts.clone();
                Box::pin(async move { Ok(json!({ "drafts": d })) })
            }
            "state" => {
                let s = self.state();
                Box::pin(async move { Ok(s) })
            }
            m => {
                let e = no_method(BlockType::Forge, m);
                Box::pin(async move { Err(e) })
            }
        }
    }

    fn drawn(&self, on: bool) {
        self.live.set(on);
        if on {
            self.look();
            self.raise();
            self.wake.notify_one();
        }
        self.ctx.changed();
    }

    fn close(&self) {
        self.live.close();
        self.wake.notify_one();
        live::resubscribe();
        if let Some((id, token)) = self.asking.lock().unwrap().take() {
            self.ctx.withdraw(&id, token);
        }
    }

    fn summary(&self) -> Summary {
        let st = self.state.lock().unwrap();
        let c = self.config.lock().unwrap();
        let name = c.repo.rsplit('/').next().unwrap_or(&c.repo).to_owned();
        let root = c.dir.clone().unwrap_or_else(|| st.pr.as_ref().map(|p| p.item.url.clone()).unwrap_or_default());
        let title = match (&st.pr, &st.issue, &st.new) {
            (Some(p), _, _) => format!("{}#{} {}", c.repo, c.number, p.item.title),
            (None, Some(i), _) => format!("{}#{} {}", c.repo, c.number, i.item.title),
            (None, None, Some(n)) if c.number == 0 => format!("{} new issue: {}", c.repo, n.title),
            _ => format!("{}#{}", c.repo, c.number),
        };
        let root = match &st.issue {
            Some(i) if c.dir.is_none() => i.item.url.clone(),
            _ => root,
        };
        Summary {
            work: Some(if c.kind == ItemKind::Issue { WorkKind::Issue } else { WorkKind::Pr }),
            project: Some(
                c.dir
                    .as_deref()
                    .and_then(|d| crate::review::project(d, self.ctx.sprite.is_none()))
                    .unwrap_or(Project { root, name }),
            ),
            title: Some(title),
            ..Summary::default()
        }
    }
}

// ---------------------------------------------------------------- opening

/// A forge block's config from what a person or agent gave: `{pr: URL |
/// OWNER/REPO#N | N, dir?}` (N: the PR in `dir`'s repository), `{issue:
/// URL | OWNER/REPO#N | N, dir?}` (M37; a link to either opens what it
/// links to), `{issue: "new", title, body?, repo? | dir?, by?, agent?}`
/// (M37: a new issue, an agent's a draft), or a config already whole
/// (`{repo, number}`).
pub async fn open_config(c: &Value) -> Result<Value, String> {
    let dir = c["dir"].as_str().filter(|d| !d.is_empty()).map(str::to_owned);
    let issue = c["issue"].is_string();
    let mut out = json!({ "provider": "forgejo", "kind": if issue { "issue" } else { "pr" } });
    for k in ["provider", "api", "login", "host"] {
        if let Some(v) = c[k].as_str() {
            out[k] = json!(v);
        }
    }
    if let Some(k) = c["kind"].as_str().filter(|k| ["pr", "issue"].contains(k)) {
        out["kind"] = json!(k);
    }
    if let (Some(repo), Some(n)) = (c["repo"].as_str(), c["number"].as_u64()) {
        out["repo"] = json!(repo);
        out["number"] = json!(n);
        if let Some(d) = &dir {
            out["dir"] = json!(d);
        }
        return Ok(out);
    }
    let what = c[if issue { "issue" } else { "pr" }]
        .as_str()
        .or(c["ref"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("say which pull request or issue: {\"pr\" | \"issue\": URL | OWNER/REPO#N | N}")?;
    if issue && what == "new" {
        return issue::new_config(c, out, dir).await;
    }
    // M38: a GitHub link (`…/pull/N`), on github.com or an Enterprise host.
    if let Some((host, repo, n)) = github::pr_url(what) {
        out["provider"] = json!("github");
        if c["api"].as_str().is_none() {
            out["api"] = json!(github::api_for(&host));
        }
        out["host"] = json!(host);
        out["repo"] = json!(repo);
        out["number"] = json!(n);
        if let Some(d) = &dir
            && let Some((top, _)) = clone_of(d, Some(&repo)).await
        {
            out["dir"] = json!(top);
        }
        return Ok(out);
    }
    // M37: a GitHub issue's link.
    if let Some((host, repo, n)) = github::issue_url(what) {
        out["provider"] = json!("github");
        out["kind"] = json!("issue");
        if c["api"].as_str().is_none() {
            out["api"] = json!(github::api_for(&host));
        }
        out["host"] = json!(host);
        out["repo"] = json!(repo);
        out["number"] = json!(n);
        if let Some(d) = &dir
            && let Some((top, _)) = clone_of(d, Some(&repo)).await
        {
            out["dir"] = json!(top);
        }
        return Ok(out);
    }
    if let Some((host, repo, n, api)) = gitlab::parse_mr_url(what) {
        // M39: a GitLab merge request's link.
        out["provider"] = json!("gitlab");
        out["host"] = json!(host);
        out["repo"] = json!(repo);
        out["number"] = json!(n);
        if c["api"].as_str().is_none() {
            out["api"] = json!(api);
        }
        if let Some(d) = &dir
            && let Some((top, _)) = clone_of(d, Some(&repo)).await
        {
            out["dir"] = json!(top);
        }
        return Ok(out);
    }
    if what.starts_with("http://") || what.starts_with("https://") {
        let (host, repo, n, api, kind) = parse_item_url(what)?;
        out["kind"] = json!(kind);
        out["host"] = json!(host);
        out["repo"] = json!(repo);
        out["number"] = json!(n);
        if out.get("api").is_none() {
            out["api"] = json!(api);
        }
        if let Some(d) = &dir
            && let Some((top, _)) = clone_of(d, Some(&repo)).await
        {
            out["dir"] = json!(top);
        }
        return Ok(out);
    }
    // `GROUP/PROJECT!N` is GitLab's way of naming a merge request.
    let bang = what.contains('!');
    if bang {
        out["provider"] = json!("gitlab");
    }
    let (repo, n) = match what.split_once('#').or_else(|| what.split_once('!')) {
        Some((r, n)) if !r.is_empty() => (Some(r.trim_matches('/').to_owned()), n),
        Some((_, n)) => (None, n),
        None => (None, what),
    };
    let thing = if issue { "an issue" } else { "a PR" };
    let n: u64 = n.parse().map_err(|_| format!("{what}: not {thing} (URL, OWNER/REPO#N or N)"))?;
    out["number"] = json!(n);
    match (repo, dir) {
        (Some(repo), dir) => {
            if let Some(d) = dir {
                if let Some((top, host)) = clone_of(&d, Some(&repo)).await {
                    out["dir"] = json!(top);
                    out["host"] = json!(host);
                } else if let Ok((_, host, _)) = remote_of(&d).await
                    && github::is_github_host(&host)
                {
                    // M38: OWNER/REPO#N from a GitHub clone is on GitHub.
                    out["host"] = json!("github.com");
                }
            }
            out["repo"] = json!(repo);
        }
        (None, Some(d)) => {
            let (top, host, repo) = remote_of(&d).await?;
            out["dir"] = json!(top);
            out["host"] = json!(host);
            out["repo"] = json!(repo);
        }
        (None, None) => return Err(format!("#{n} in which repository? Run it in a clone, or say OWNER/REPO#{n}")),
    }
    // M38: a clone on github.com is a GitHub PR (its remote's SSH host may
    // be `ssh.github.com`).
    if c["provider"].as_str().is_none()
        && let Some(h) = out["host"].as_str().filter(|h| github::is_github_host(h))
    {
        let api = c["api"].as_str().map_or_else(|| github::api_for(h), str::to_owned);
        out["provider"] = json!("github");
        out["host"] = json!("github.com");
        out["api"] = json!(api);
    }
    // A clone on a GitLab host is GitLab's (others are told by a link, `!N`
    // or `provider`).
    if let Some(h) = out["host"].as_str()
        && gitlab::known_host(h)
        && c["provider"].as_str().is_none()
    {
        out["provider"] = json!("gitlab");
    }
    if out["provider"] == "gitlab" && out["api"].is_null() {
        let host = out["host"].as_str().unwrap_or("gitlab.com").to_owned();
        out["host"] = json!(host);
        out["api"] = json!(format!("https://{host}/api/v4"));
    }
    Ok(out)
}

/// `https://host/owner/name/pulls/N` → (host, owner/name, N, API base).
#[allow(dead_code)] // M37's callers take issues too: `parse_item_url`.
pub fn parse_pr_url(u: &str) -> Result<(String, String, u64, String), String> {
    match parse_item_url(u)? {
        (host, repo, n, api, ItemKind::Pr) => Ok((host, repo, n, api)),
        _ => Err(format!("{u}: not a pull request's address (…/OWNER/REPO/pulls/N)")),
    }
}

/// A PR's or (M37) an issue's address (`…/OWNER/REPO/pulls/N`,
/// `…/issues/N`) → (host, owner/name, N, API base, which).
pub fn parse_item_url(u: &str) -> Result<(String, String, u64, String, ItemKind), String> {
    let url = url::Url::parse(u).map_err(|e| format!("{u}: {e}"))?;
    let host = login::url_host(u).ok_or_else(|| format!("{u}: no host"))?;
    let segs: Vec<&str> = url.path_segments().map(|s| s.filter(|x| !x.is_empty()).collect()).unwrap_or_default();
    let at = segs.iter().position(|s| matches!(*s, "pulls" | "pull" | "issues")).filter(|i| *i >= 2);
    let (Some(i), Some(n)) = (at, at.and_then(|i| segs.get(i + 1)).and_then(|n| n.parse::<u64>().ok())) else {
        return Err(format!("{u}: not a pull request's or issue's address (…/OWNER/REPO/pulls/N, …/issues/N)"));
    };
    let kind = if segs[i] == "issues" { ItemKind::Issue } else { ItemKind::Pr };
    let base = segs[..i - 2].join("/");
    let repo = format!("{}/{}", segs[i - 2], segs[i - 1]);
    let prefix = if base.is_empty() { String::new() } else { format!("/{base}") };
    let api = format!("{}://{host}{prefix}/api/v1", url.scheme());
    Ok((host, repo, n, api, kind))
}

/// `git remote -v` in a local directory: (its top, url) per remote.
async fn remotes(dir: &str) -> Option<(String, Vec<(String, String)>)> {
    let git = |args: &[&str]| {
        tokio::process::Command::new("git").arg("-C").arg(dir).args(args).env("GIT_OPTIONAL_LOCKS", "0").output()
    };
    let top = git(&["rev-parse", "--show-toplevel"]).await.ok().filter(|o| o.status.success())?;
    let top = String::from_utf8_lossy(&top.stdout).trim().to_owned();
    let out = git(&["remote", "-v"]).await.ok()?;
    let mut rs: Vec<(String, String)> = Vec::new();
    for l in String::from_utf8_lossy(&out.stdout).lines() {
        let mut p = l.split_whitespace();
        if let (Some(name), Some(url)) = (p.next(), p.next())
            && !rs.iter().any(|(n, _)| n == name)
        {
            rs.push((name.to_owned(), url.to_owned()));
        }
    }
    Some((top, rs))
}

/// The clone in `dir` of `repo` (any, if `None`): its top and the remote's
/// host.
async fn clone_of(dir: &str, repo: Option<&str>) -> Option<(String, String)> {
    let (top, rs) = remotes(dir).await?;
    rs.iter()
        .filter_map(|(_, u)| login::parse_remote(u))
        .find(|(_, p)| repo.is_none_or(|r| r.eq_ignore_ascii_case(p)))
        .map(|(h, _)| (top.clone(), h))
}

/// The repository `dir` is a clone of: `upstream` is where PRs go, else
/// `origin`, else the first remote.
async fn remote_of(dir: &str) -> Result<(String, String, String), String> {
    let (top, rs) = remotes(dir).await.ok_or_else(|| format!("{dir} isn't in a git repository"))?;
    let pick = ["upstream", "origin"].iter().find_map(|n| rs.iter().find(|(r, _)| r == n)).or(rs.first());
    let (_, url) = pick.ok_or_else(|| format!("{top} has no remote"))?;
    let (host, repo) = login::parse_remote(url).ok_or_else(|| format!("can't tell the forge from the remote {url}"))?;
    Ok((top, host, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_links() {
        let p = parse_pr_url("https://git.inevitable.fyi/jhgaylor/illogical/pulls/84").unwrap();
        assert_eq!(
            p,
            ("git.inevitable.fyi".into(), "jhgaylor/illogical".into(), 84, "https://git.inevitable.fyi/api/v1".into())
        );
        let p = parse_pr_url("http://127.0.0.1:3000/o/r/pulls/7/files").unwrap();
        assert_eq!(p.3, "http://127.0.0.1:3000/api/v1");
        let p = parse_pr_url("https://h.example/forge/o/r/pulls/2").unwrap();
        assert_eq!((p.1.as_str(), p.3.as_str()), ("o/r", "https://h.example/forge/api/v1"));
        assert!(parse_pr_url("https://h.example/o/r/issues/2").is_err());
    }

    #[tokio::test]
    async fn configs_from_what_was_given() {
        let c = open_config(&json!({ "pr": "https://git.inevitable.fyi/jhgaylor/illogical/pulls/84" })).await.unwrap();
        assert_eq!(c["repo"], "jhgaylor/illogical");
        assert_eq!(c["number"], 84);
        assert_eq!(c["host"], "git.inevitable.fyi");
        let c = open_config(&json!({ "pr": "o/r#12" })).await.unwrap();
        assert_eq!((c["repo"].as_str(), c["number"].as_u64()), (Some("o/r"), Some(12)));
        assert!(open_config(&json!({ "pr": "12" })).await.unwrap_err().contains("which repository"));
        assert!(open_config(&json!({ "pr": "o/r#x" })).await.is_err());
        assert!(open_config(&json!({})).await.is_err());
    }

    #[tokio::test]
    async fn github_configs() {
        let c = open_config(&json!({ "pr": "https://github.com/cli/cli/pull/14519" })).await.unwrap();
        assert_eq!((c["provider"].as_str(), c["api"].as_str()), (Some("github"), Some("https://api.github.com")));
        assert_eq!(
            (c["repo"].as_str(), c["number"].as_u64(), c["host"].as_str()),
            (Some("cli/cli"), Some(14519), Some("github.com"))
        );
        let c = open_config(&json!({ "pr": "https://ghe.example.com/o/r/pull/3/files" })).await.unwrap();
        assert_eq!(
            (c["provider"].as_str(), c["api"].as_str()),
            (Some("github"), Some("https://ghe.example.com/api/v3"))
        );
        // Forgejo's links stay Forgejo's.
        let c = open_config(&json!({ "pr": "https://git.inevitable.fyi/jhgaylor/illogical/pulls/84" })).await.unwrap();
        assert_eq!(c["provider"], "forgejo");
        // A whole config keeps its provider.
        let c = open_config(&json!({ "provider": "github", "api": "http://127.0.0.1:1", "repo": "o/r", "number": 2 }))
            .await
            .unwrap();
        assert_eq!((c["provider"].as_str(), c["api"].as_str()), (Some("github"), Some("http://127.0.0.1:1")));
        assert_eq!(Write::from_call("rerun_checks", &json!({})).unwrap(), Write::Rerun);
        assert_eq!(serde_json::to_value(Write::Rerun).unwrap(), json!({ "method": "rerun_checks" }));
    }

    #[tokio::test]
    async fn gitlab_configs() {
        let c = open_config(&json!({ "pr": "https://gitlab.com/gitlab-org/cli/-/merge_requests/3941" })).await.unwrap();
        assert_eq!(
            (c["provider"].as_str(), c["repo"].as_str(), c["number"].as_u64()),
            (Some("gitlab"), Some("gitlab-org/cli"), Some(3941))
        );
        assert_eq!((c["host"].as_str(), c["api"].as_str()), (Some("gitlab.com"), Some("https://gitlab.com/api/v4")));
        let c = open_config(&json!({ "pr": "http://127.0.0.1:9/g/sub/p/-/merge_requests/2" })).await.unwrap();
        assert_eq!((c["repo"].as_str(), c["api"].as_str()), (Some("g/sub/p"), Some("http://127.0.0.1:9/api/v4")));
        // GitLab's own notation; a host it's known by.
        let c = open_config(&json!({ "pr": "gitlab-org/cli!12" })).await.unwrap();
        assert_eq!((c["provider"].as_str(), c["number"].as_u64()), (Some("gitlab"), Some(12)));
        assert_eq!(c["api"], "https://gitlab.com/api/v4");
        let c = open_config(&json!({ "pr": "o/r#3", "host": "gitlab.example.org" })).await.unwrap();
        assert_eq!(
            (c["provider"].as_str(), c["api"].as_str()),
            (Some("gitlab"), Some("https://gitlab.example.org/api/v4"))
        );
        // Forgejo stays Forgejo.
        let c = open_config(&json!({ "pr": "o/r#3", "host": "git.inevitable.fyi" })).await.unwrap();
        assert_eq!(c["provider"], "forgejo");
        assert_eq!(Write::from_call("rerun_checks", &json!({})).unwrap(), Write::Rerun);
        assert_eq!(serde_json::to_value(Write::Rerun).unwrap(), json!({ "method": "rerun_checks" }));
    }

    #[test]
    fn drafts_as_cards() {
        let d = Draft {
            id: "d1".into(),
            write: Write::Comment { body: "LGTM".into() },
            by: "mcp:claude-code".into(),
            at_ms: 5,
            status: DraftStatus::Waiting,
            settled_by: None,
            settled_ms: None,
            url: None,
            error: None,
        };
        let a = draft_ask(&d, "o/r", 84);
        assert_eq!((a.kind, a.source.as_str(), a.agent.as_deref()), (AskKind::Form, "forge", Some("claude-code")));
        assert_eq!(a.message, "claude-code drafted a comment on o/r#84");
        let s = a.schema.unwrap();
        assert_eq!(s["properties"]["body"]["default"], "LGTM");
        assert_eq!(s["properties"]["body"]["format"], "markdown");
        assert_eq!(s["required"], json!(["body"]));
        // The card's edits are what's sent.
        assert_eq!(
            edited(&d.write, &json!({ "body": "LGTM, thanks" })),
            Write::Comment { body: "LGTM, thanks".into() }
        );
        let r = Write::Review { event: ReviewEvent::Comment, body: Some("hm".into()) };
        assert_eq!(
            edited(&r, &json!({ "event": "request_changes" })),
            Write::Review { event: ReviewEvent::RequestChanges, body: Some("hm".into()) }
        );
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!((v["method"].as_str(), v["body"].as_str()), (Some("comment"), Some("LGTM")));
    }

    #[test]
    fn writes_from_calls() {
        assert!(Write::from_call("comment", &json!({})).is_err());
        assert_eq!(
            Write::from_call("review", &json!({ "event": "approve" })).unwrap(),
            Write::Review { event: ReviewEvent::Approve, body: None }
        );
        assert!(Write::from_call("review", &json!({ "event": "request_changes" })).is_err());
        assert!(Write::from_call("review", &json!({ "event": "yes" })).is_err());
        assert!(Write::from_call("merge", &json!({ "style": "octopus" })).is_err());
        assert_eq!(
            Write::from_call("merge", &json!({ "style": "squash" })).unwrap(),
            Write::Merge { style: Some("squash".into()) }
        );
    }
}
