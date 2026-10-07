//! hud in a studio box, from the daemon (M35): a session of its own, and
//! the follower that turns the box agent's questions into asks on the app
//! block (S22's `bridge.mjs`, moved in).
//!
//! **The session.** An entry link (studio's `/__enter`, or a follower link
//! the box's owner made with `hud share --role follower`) is followed with
//! a cookie jar of our own, redirect by redirect, until it lands; the
//! cookies it set (hud's session) are kept in memory only. When hud says
//! 401, a new link is minted and followed again. Nothing about it is saved:
//! after a restart the follower mints again.
//!
//! **The follower**, against hud's routes as S22 used them:
//!
//! - `GET /__hud/api/tabs`: `{tabs: [{chatKey, …}]}`, read again every
//!   half minute for new tabs;
//! - `GET /__hud/api/chat/stream?chatKey=…`: server-sent events, one per
//!   tab, followed again with backoff when the stream drops. A
//!   `hud-chat-queue` frame carries the question the running turn waits on
//!   (`question: {requestId, summary, options: [{optionId, name,
//!   description?}], askedAt, expiresAt}`), in every frame, the first
//!   included, so a follower needs no history. A question gone from the
//!   queue (answered in hud, interrupted, or expired) withdraws its card;
//!   so does `expiresAt` passing.
//! - `POST /__hud/api/chat/answer {chatKey, requestId, optionId}`, with the
//!   box's own `Origin` (hud refuses cross-site writes). With a follower
//!   credential (the block's config says so) it adds `onBehalfOf: {name,
//!   via: "arugula"}`, naming whoever answered in Arugula; that needs
//!   hud's trusted-follower change (arugula-salad track A5). Without one,
//!   hud records the session's own player, the box's owner.
//! - `POST /__hud/api/chat/prompt {chatKey, text}`, for the block's
//!   `send`: a prompt to the box's agent in one of `/api/tabs`' tabs
//!   (`{chatKey, title}`, oldest first), queued behind a running turn.
//!   hud takes no `onBehalfOf` here, so the prompt is the session's
//!   (the owner's, or the follower's); the block's log says who sent it.
//!
//! A block shows one question at a time: with several tabs asking, the
//! oldest is on the card and the next follows when it's answered.
//!
//! **Gates**, from hud's work board: `GET /__hud/api/work` (its `approve`
//! group's items, each with `gate: {member, component, name, env, needed,
//! approvals, approve}`), read again whenever hud's live feed
//! (`/__hud/api/live/stream`) says something changed, never on a timer.
//! Each read is a `chant workspace status` in the box (a few hundred MB,
//! tens of seconds on a busy one), and the feed moves all through an
//! agent's turn, so reads are paced: one at a time, the next no sooner
//! than the last took (at least `BOARD_GAP`), except after an approve.
//! A read waits longer than hud gives chant, so hud is never left running
//! one we gave up on while we start the next.
//! Approving one is hud's `POST /__hud/api/work/gates/approve {member,
//! component, gate, env}` (arugula-salad/hud#735), which approves only a
//! gate `workspace status` lists as pending; with a follower credential it
//! adds `onBehalfOf` (hud#736).
//!
//! **Names for hud.** hud takes a person's name only as a display name
//! (at most 32 characters: letters, digits, spaces and `-_.'`, not one of
//! its role labels), so [`hud_name`] makes Arugula's name fit: an email
//! address by its local part, other characters as `-`. A name that can't
//! fit (or is `owner`) is left out, and hud records its session's player.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use arugula_proto::ask::{Ask, AskKind};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{debug, info, warn};

use crate::{block::BlockCtx, mux::AskReply};

/// How often the box's tabs are listed again.
const TABS_EVERY: Duration = Duration::from_secs(30);
/// Backoff for a dropped stream or a session that can't be made.
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// The least time between work board reads the live feed asks for.
const BOARD_GAP: Duration = Duration::from_secs(5);
/// How long a work board read may take: past hud's own 120s for chant.
const BOARD_TIMEOUT: Duration = Duration::from_secs(150);
/// Redirects followed into a box.
const MAX_HOPS: usize = 10;

/// Mints an entry link each time it's called.
pub type Mint = Arc<dyn Fn() -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

#[derive(Debug)]
pub enum HudError {
    /// hud wants a session (401): mint and enter again.
    Unauthorized,
    Other(String),
}

impl std::fmt::Display for HudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("hud wants a session (401)"),
            Self::Other(e) => f.write_str(e),
        }
    }
}

/// A session with a box's hud: its origin and the cookies the entry link
/// set.
pub struct Session {
    origin: String,
    cookies: BTreeMap<String, String>,
    http: reqwest::Client,
}

impl Session {
    /// Follow `link` into the box at `origin`, keeping the cookies it sets.
    pub async fn enter(origin: &str, link: &str) -> Result<Self, String> {
        let http = crate::roots::http()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        let mut cookies = BTreeMap::new();
        let mut at = reqwest::Url::parse(link).map_err(|e| format!("entry link: {e}"))?;
        if at.origin().ascii_serialization() != origin {
            return Err("the entry link is for another box".into());
        }
        for _ in 0..MAX_HOPS {
            let res = http
                .get(at.clone())
                .header("cookie", cookie_header(&cookies))
                .timeout(Duration::from_secs(15))
                .send()
                .await
                .map_err(|e| format!("entering the box: {}", e.without_url()))?;
            for v in res.headers().get_all("set-cookie") {
                if let Ok(v) = v.to_str() {
                    set_cookie(&mut cookies, v);
                }
            }
            let status = res.status();
            if status.is_redirection() {
                let to = res.headers().get("location").and_then(|l| l.to_str().ok()).ok_or("a redirect to nowhere")?;
                let next = at.join(to).map_err(|e| format!("a redirect: {e}"))?;
                if next.origin().ascii_serialization() != origin {
                    // Out of the box: whatever it set is what we have.
                    break;
                }
                at = next;
                continue;
            }
            if status.as_u16() == 401 || status.as_u16() == 403 || status.as_u16() == 410 {
                return Err(format!("the box refused the link ({status})"));
            }
            if !status.is_success() {
                return Err(format!("the box answered {status}"));
            }
            break;
        }
        if cookies.is_empty() {
            return Err("the box set no session".into());
        }
        Ok(Self { origin: origin.to_owned(), cookies, http })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}/__hud{path}", self.origin))
            .header("cookie", cookie_header(&self.cookies))
            .header("origin", &self.origin)
    }

    async fn checked(res: Result<reqwest::Response, reqwest::Error>) -> Result<reqwest::Response, HudError> {
        let res = res.map_err(|e| HudError::Other(e.without_url().to_string()))?;
        match res.status().as_u16() {
            401 => Err(HudError::Unauthorized),
            s if (200..300).contains(&s) => Ok(res),
            s => Err(HudError::Other(format!("hud answered {s}"))),
        }
    }

    /// The box's chat tabs' keys.
    pub async fn tabs(&self) -> Result<Vec<String>, HudError> {
        Ok(self.tab_list().await?.into_iter().map(|t| t.chat).collect())
    }

    /// The box's chat tabs, oldest first (hud always has one).
    pub async fn tab_list(&self) -> Result<Vec<Tab>, HudError> {
        let res =
            Self::checked(self.req(reqwest::Method::GET, "/api/tabs").timeout(Duration::from_secs(15)).send().await)
                .await?;
        let v: Value = res.json().await.map_err(|e| HudError::Other(format!("tabs: {}", e.without_url())))?;
        Ok(parse_tabs(&v))
    }

    /// Prompt the box's agent in a tab (hud queues it behind a running
    /// turn). hud's answer (`{promptId, position, queued}`), or why not.
    pub async fn prompt(&self, chat: &str, text: &str) -> Result<Value, String> {
        let res = self
            .req(reqwest::Method::POST, "/api/chat/prompt")
            .timeout(Duration::from_secs(30))
            .json(&json!({ "chatKey": chat, "text": text }))
            .send()
            .await
            .map_err(|e| format!("hud: {}", e.without_url()))?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or_default();
        if status.is_success() {
            return Ok(v);
        }
        Err(prompt_refused(status.as_u16(), &v))
    }

    /// A tab's chat stream (server-sent events), from now.
    async fn stream(&self, chat: &str) -> Result<reqwest::Response, HudError> {
        let url = format!("/api/chat/stream?chatKey={}", enc(chat));
        Self::checked(self.req(reqwest::Method::GET, &url).header("accept", "text/event-stream").send().await).await
    }

    /// The gates hud's work board lists as pending.
    pub async fn work(&self, app: &str) -> Result<Vec<arugula_proto::Gate>, HudError> {
        let res =
            Self::checked(self.req(reqwest::Method::GET, "/api/work").timeout(BOARD_TIMEOUT).send().await).await?;
        let v: Value = res.json().await.map_err(|e| HudError::Other(format!("work: {}", e.without_url())))?;
        if let Some(e) = v["error"]["message"].as_str() {
            return Err(HudError::Other(format!("hud couldn't read the board: {e}")));
        }
        Ok(board_gates(&self.origin, app, &v))
    }

    /// Approve a pending gate (hud#735). What hud said, or why not.
    pub async fn approve_gate(&self, body: &Value) -> Result<String, String> {
        let res = self
            .req(reqwest::Method::POST, "/api/work/gates/approve")
            .timeout(Duration::from_secs(120))
            .json(body)
            .send()
            .await
            .map_err(|e| format!("hud: {}", e.without_url()))?;
        let status = res.status();
        let v: Value = res.json().await.unwrap_or_default();
        if status.is_success() {
            return Ok(v["approval"]["output"]
                .as_str()
                .or(v["approval"]["command"].as_str())
                .unwrap_or("approved")
                .to_owned());
        }
        let said = v["error"].as_str().unwrap_or("").to_owned();
        Err(match status.as_u16() {
            401 => "hud wants a new session: try again in a moment".into(),
            409 if !said.is_empty() => format!("hud: {said} (it may have been approved already)"),
            _ if !said.is_empty() => format!("hud: {said}"),
            s => format!("hud answered {s}"),
        })
    }

    /// Answer a question with one of its options.
    pub async fn answer(&self, body: &Value) -> Result<Value, HudError> {
        let res = Self::checked(
            self.req(reqwest::Method::POST, "/api/chat/answer")
                .timeout(Duration::from_secs(15))
                .json(body)
                .send()
                .await,
        )
        .await?;
        Ok(res.json().await.unwrap_or_default())
    }
}

/// One of the box's chat tabs.
#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub chat: String,
    pub title: String,
}

fn parse_tabs(v: &Value) -> Vec<Tab> {
    v["tabs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let chat = t["chatKey"].as_str()?.to_owned();
            Some(Tab { title: t["title"].as_str().unwrap_or(&chat).to_owned(), chat })
        })
        .collect()
}

/// The tab `want` names (its key, or its title in any case); none named,
/// the box's first.
pub fn pick_tab<'a>(tabs: &'a [Tab], want: Option<&str>) -> Result<&'a Tab, String> {
    let Some(w) = want else { return tabs.first().ok_or_else(|| "hud listed no tabs".into()) };
    tabs.iter().find(|t| t.chat == w).or_else(|| tabs.iter().find(|t| t.title.eq_ignore_ascii_case(w))).ok_or_else(
        || {
            let names: Vec<_> = tabs.iter().map(|t| t.title.as_str()).collect();
            format!("no tab {w:?} in the box (it has: {})", names.join(", "))
        },
    )
}

/// Why hud refused a prompt, for whoever sent it.
fn prompt_refused(status: u16, v: &Value) -> String {
    let said = v["error"].as_str().unwrap_or("");
    match (status, v["reason"].as_str()) {
        (401, _) => "hud wants a new session: try again in a moment".into(),
        (403, _) => format!("hud refused: {}", if said.is_empty() { "this session may only watch" } else { said }),
        (429, Some("queue-full")) => "the tab's queue is full: wait for its turn to finish".into(),
        (_, Some(r)) if !said.is_empty() => format!("hud: {said} ({r})"),
        (s, _) if said.is_empty() => format!("hud answered {s}"),
        _ => format!("hud: {said}"),
    }
}

fn cookie_header(c: &BTreeMap<String, String>) -> String {
    c.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
}

/// One `Set-Cookie`: kept, or dropped when it clears (`Max-Age=0`, or an
/// empty value).
fn set_cookie(jar: &mut BTreeMap<String, String>, header: &str) {
    let mut parts = header.split(';');
    let Some((name, value)) = parts.next().and_then(|kv| kv.split_once('=')) else { return };
    let (name, value) = (name.trim(), value.trim());
    let cleared = parts.any(|a| a.trim().eq_ignore_ascii_case("max-age=0"));
    if name.is_empty() {
        return;
    }
    if cleared || value.is_empty() {
        jar.remove(name);
    } else {
        jar.insert(name.to_owned(), value.to_owned());
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A question hud waits on, from a queue frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub chat: String,
    pub id: String,
    pub summary: String,
    /// `(optionId, name, description)`.
    pub options: Vec<(String, String, Option<String>)>,
    pub asked_at: u64,
    pub expires_at: Option<u64>,
}

impl Question {
    fn parse(chat: &str, q: &Value) -> Option<Self> {
        let options = q["options"]
            .as_array()?
            .iter()
            .filter_map(|o| {
                Some((
                    o["optionId"].as_str()?.to_owned(),
                    o["name"].as_str()?.to_owned(),
                    o["description"].as_str().map(str::to_owned),
                ))
            })
            .collect::<Vec<_>>();
        Some(Self {
            chat: chat.to_owned(),
            id: q["requestId"].as_str()?.to_owned(),
            summary: q["summary"].as_str().unwrap_or("hud asks").to_owned(),
            options,
            asked_at: q["askedAt"].as_u64().unwrap_or_else(crate::store::now_ms),
            expires_at: q["expiresAt"].as_u64(),
        })
    }

    /// AskUserQuestion's card: one single-select question, hud's options
    /// by name.
    fn ask(&self) -> Ask {
        let options: Vec<Value> = self
            .options
            .iter()
            .map(|(_, name, d)| match d {
                Some(d) => json!({ "label": name, "description": d }),
                None => json!({ "label": name }),
            })
            .collect();
        Ask {
            id: self.id.clone(),
            kind: AskKind::Questions,
            message: self.summary.clone(),
            questions: Some(json!([{
                "question": self.summary, "header": "hud", "multiSelect": false, "options": options,
            }])),
            schema: None,
            url: None,
            accepted: false,
            tool_call_id: None,
            source: "hud".into(),
            agent: Some("hud".into()),
            at_ms: self.asked_at,
            tool: None,
            input: None,
            suggestions: None,
            session: None,
        }
    }

    /// The option a card's answer picked: by label (`question_0`).
    fn picked(&self, content: &Value) -> Option<&str> {
        let label = match &content["question_0"] {
            Value::String(s) => Some(s.as_str()),
            Value::Array(a) => a.first().and_then(Value::as_str),
            _ => None,
        }?;
        self.options.iter().find(|(_, name, _)| name == label).map(|(id, _, _)| id.as_str())
    }
}

/// What the follower reports to its block.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Status {
    /// `starting`, `following`, `entering`, or `error`.
    pub state: String,
    pub error: Option<String>,
    /// Tabs followed now.
    pub tabs: usize,
    /// Questions hud waits on now.
    pub questions: usize,
    /// The last answer sent to hud, and what hud said.
    pub last_answer: Option<Value>,
}

/// What the block gives its follower.
pub struct Setup {
    pub origin: String,
    pub app: String,
    pub mint: Mint,
    /// Send `onBehalfOf` with answers (a follower credential).
    pub on_behalf: bool,
    pub ctx: BlockCtx,
    /// Called with every change of status.
    pub report: Arc<dyn Fn(Status) + Send + Sync>,
    /// Lines for the block's log.
    pub log: Arc<dyn Fn(Value) + Send + Sync>,
    /// The gates hud's work board lists, each time it's read.
    pub gates: Arc<dyn Fn(Vec<arugula_proto::Gate>) + Send + Sync>,
    /// The session now, for approving a gate through it.
    pub session: Arc<std::sync::Mutex<Option<Arc<Session>>>>,
}

enum Ev {
    Queue {
        chat: String,
        question: Option<Value>,
    },
    /// A question settled in hud (a `permission_request` block, answered).
    Settled(String),
    Connected(String),
    Dropped(String),
    Unauthorized,
    Replied {
        id: String,
        token: u64,
        reply: AskReply,
        by: Option<arugula_proto::Driver>,
    },
    Answered(Value),
    /// hud's live feed moved (or the work board should be read anyway).
    Live,
    /// Read the work board as soon as the read in flight is done (after an
    /// approve).
    Reread,
    /// The work board, read.
    Board(Result<Vec<arugula_proto::Gate>, HudError>),
}

/// The follower: runs until the handle is dropped (its block closed).
pub struct Follower {
    task: JoinHandle<()>,
    tx: mpsc::UnboundedSender<Ev>,
}

impl Follower {
    /// Read the work board again (after an approve).
    pub fn reread(&self) {
        let _ = self.tx.send(Ev::Reread);
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn start(setup: Setup) -> Follower {
    let rt = setup.ctx.rt.clone();
    let (tx, rx) = mpsc::unbounded_channel();
    Follower { task: rt.spawn(run(setup, tx.clone(), rx)), tx }
}

/// The card on the block now.
struct Raised {
    id: String,
    token: u64,
}

struct Run {
    s: Setup,
    status: Status,
    /// Every question hud waits on, by request id.
    open: BTreeMap<String, Question>,
    /// Answered, skipped or expired here: never raised again.
    done: HashSet<String>,
    raised: Option<Raised>,
    tx: mpsc::UnboundedSender<Ev>,
}

async fn run(s: Setup, tx: mpsc::UnboundedSender<Ev>, mut rx: mpsc::UnboundedReceiver<Ev>) {
    let mut r = Run {
        s,
        status: Status { state: "starting".into(), ..Status::default() },
        open: BTreeMap::new(),
        done: HashSet::new(),
        raised: None,
        tx,
    };
    r.report();
    let mut backoff = BACKOFF_MIN;
    loop {
        r.status.state = "entering".into();
        r.report();
        let session = match (r.s.mint)().await {
            Ok(link) => Session::enter(&r.s.origin, &link).await,
            Err(e) => Err(e),
        };
        let session = match session {
            Ok(s) => Arc::new(s),
            Err(e) => {
                warn!(app = r.s.app, error = e, "can't get into the box");
                r.status.state = "error".into();
                r.status.error = Some(e);
                r.report();
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        };
        info!(app = r.s.app, "following the box's hud");
        *r.s.session.lock().unwrap() = Some(session.clone());
        (r.s.log)(json!({ "e": "entered" }));
        r.status.error = None;
        if r.follow(&session, &mut rx).await {
            // hud wants a new session; any time spent here was spent working.
            backoff = BACKOFF_MIN;
            (r.s.log)(json!({ "e": "session_expired" }));
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

impl Run {
    fn report(&self) {
        (self.s.report)(self.status.clone());
    }

    /// Follow every tab until hud wants a new session (true), or the tabs
    /// can't be read (false).
    async fn follow(&mut self, session: &Arc<Session>, rx: &mut mpsc::UnboundedReceiver<Ev>) -> bool {
        let mut streams: HashMap<String, JoinHandle<()>> = HashMap::new();
        let mut live: HashSet<String> = HashSet::new();
        let mut tabs_due = tokio::time::Instant::now();
        // The live feed: each change reads the work board again (one read
        // at a time; a change meanwhile reads once more after it).
        let feed = self.s.ctx.rt.spawn(live_feed(session.clone(), self.tx.clone()));
        let mut board = Board::default();
        let out = loop {
            if tokio::time::Instant::now() >= tabs_due {
                match session.tabs().await {
                    Ok(tabs) => {
                        for t in &tabs {
                            if !streams.contains_key(t) {
                                streams.insert(
                                    t.clone(),
                                    self.s.ctx.rt.spawn(stream(session.clone(), t.clone(), self.tx.clone())),
                                );
                            }
                        }
                        // Gone tabs: their questions go with them.
                        let gone: Vec<String> = streams.keys().filter(|k| !tabs.contains(k)).cloned().collect();
                        for g in gone {
                            if let Some(h) = streams.remove(&g) {
                                h.abort();
                            }
                            live.remove(&g);
                            self.open.retain(|_, q| q.chat != g);
                        }
                        self.status.state = "following".into();
                        self.status.tabs = streams.len();
                        self.reconcile().await;
                    }
                    Err(HudError::Unauthorized) => break true,
                    Err(HudError::Other(e)) => {
                        self.status.state = "error".into();
                        self.status.error = Some(format!("tabs: {e}"));
                        self.report();
                        break false;
                    }
                }
                tabs_due = tokio::time::Instant::now() + TABS_EVERY;
            }
            let expiry = self.next_expiry();
            let ev = tokio::select! {
                ev = rx.recv() => ev,
                _ = tokio::time::sleep_until(board.next), if board.again && !board.reading => Some(Ev::Reread),
                _ = tokio::time::sleep_until(tabs_due) => continue,
                _ = sleep_until_ms(expiry) => {
                    self.reconcile().await;
                    continue;
                }
            };
            let Some(ev) = ev else { break false };
            match ev {
                Ev::Unauthorized => break true,
                Ev::Connected(chat) => {
                    live.insert(chat);
                    self.status.error = None;
                    self.report();
                }
                Ev::Dropped(chat) => {
                    live.remove(&chat);
                }
                Ev::Queue { chat, question } => {
                    let q = question.as_ref().and_then(|q| Question::parse(&chat, q));
                    // A question gone from the queue was answered elsewhere,
                    // expired or interrupted.
                    self.open.retain(|id, o| o.chat != chat || q.as_ref().is_some_and(|q| q.id == *id));
                    if let Some(q) = q
                        && !self.done.contains(&q.id)
                    {
                        self.open.insert(q.id.clone(), q);
                    }
                    self.reconcile().await;
                }
                Ev::Settled(id) => {
                    self.open.remove(&id);
                    self.reconcile().await;
                }
                Ev::Replied { id, token, reply, by } => self.replied(session, id, token, reply, by).await,
                Ev::Answered(v) => {
                    self.status.last_answer = Some(v);
                    self.report();
                }
                Ev::Live | Ev::Reread
                    if board.reading || (matches!(ev, Ev::Live) && tokio::time::Instant::now() < board.next) =>
                {
                    board.again = true;
                    board.now |= matches!(ev, Ev::Reread);
                }
                Ev::Live | Ev::Reread => {
                    board.start();
                    let (session, tx, app) = (session.clone(), self.tx.clone(), self.s.app.clone());
                    self.s.ctx.rt.spawn(async move {
                        let _ = tx.send(Ev::Board(session.work(&app).await));
                    });
                }
                Ev::Board(r) => {
                    board.done();
                    match r {
                        Ok(gates) => (self.s.gates)(gates),
                        Err(HudError::Unauthorized) => break true,
                        Err(HudError::Other(e)) => {
                            debug!(app = self.s.app, error = e, "the work board");
                            (self.s.log)(json!({ "e": "work_board", "error": e }));
                        }
                    }
                    if board.again && board.now {
                        // Not again at `next` too.
                        (board.again, board.now) = (false, false);
                        let _ = self.tx.send(Ev::Reread);
                    }
                }
            }
        };
        feed.abort();
        *self.s.session.lock().unwrap() = None;
        for (_, h) in streams {
            h.abort();
        }
        if let Some(r) = self.raised.take() {
            self.s.ctx.withdraw(&r.id, r.token);
        }
        self.open.clear();
        self.status.tabs = 0;
        self.status.questions = 0;
        self.report();
        out
    }

    fn next_expiry(&self) -> Option<u64> {
        self.open.values().filter_map(|q| q.expires_at).min()
    }

    /// The card shows the oldest open question; one gone is withdrawn.
    async fn reconcile(&mut self) {
        let now = crate::store::now_ms();
        let expired: Vec<String> =
            self.open.values().filter(|q| q.expires_at.is_some_and(|e| e <= now)).map(|q| q.id.clone()).collect();
        for id in expired {
            debug!(id, "question expired");
            self.open.remove(&id);
            self.done.insert(id.clone());
            (self.s.log)(json!({ "e": "expired", "id": id }));
        }
        if let Some(r) = &self.raised
            && !self.open.contains_key(&r.id)
        {
            let r = self.raised.take().expect("just seen");
            info!(app = self.s.app, id = r.id, "hud's question went: card withdrawn");
            (self.s.log)(json!({ "e": "withdrawn", "id": r.id }));
            self.s.ctx.withdraw(&r.id, r.token);
        }
        if self.raised.is_none()
            && let Some(q) =
                self.open.values().filter(|q| !self.done.contains(&q.id)).min_by_key(|q| q.asked_at).cloned()
        {
            match self.s.ctx.ask(q.ask()).await {
                Ok((token, reply)) => {
                    info!(
                        app = self.s.app,
                        id = q.id,
                        lag_ms = crate::store::now_ms().saturating_sub(q.asked_at),
                        "hud asks"
                    );
                    (self.s.log)(json!({ "e": "asked", "id": q.id, "question": q.summary }));
                    self.raised = Some(Raised { id: q.id.clone(), token });
                    let tx = self.tx.clone();
                    let id = q.id.clone();
                    self.s.ctx.rt.spawn(async move {
                        let (reply, by) = reply.await.unwrap_or((AskReply::Withdrawn, None));
                        let _ = tx.send(Ev::Replied { id, token, reply, by });
                    });
                }
                Err(e) => {
                    warn!(app = self.s.app, error = e, "can't raise hud's question");
                    self.done.insert(q.id.clone());
                }
            }
        }
        self.status.questions = self.open.len();
        self.report();
    }

    async fn replied(
        &mut self,
        session: &Arc<Session>,
        id: String,
        token: u64,
        reply: AskReply,
        by: Option<arugula_proto::Driver>,
    ) {
        let ours = self.raised.as_ref().is_some_and(|r| r.id == id && r.token == token);
        if !ours {
            return; // withdrawn by us, or long gone
        }
        self.raised = None;
        let q = self.open.get(&id).cloned();
        self.done.insert(id.clone());
        match (reply, q) {
            (AskReply::Answer(content), Some(q)) => match q.picked(&content) {
                Some(option) => {
                    let mut body = json!({ "chatKey": q.chat, "requestId": q.id, "optionId": option });
                    let name = by.as_ref().map(|b| b.name.clone());
                    if self.s.on_behalf
                        && let Some(n) = name.as_deref().and_then(hud_name)
                    {
                        body["onBehalfOf"] = json!({ "name": n, "via": "arugula" });
                    }
                    (self.s.log)(json!({ "e": "answered", "id": q.id, "option": option, "by": name }));
                    let (session, tx, app) = (session.clone(), self.tx.clone(), self.s.app.clone());
                    let sent = crate::store::now_ms();
                    self.s.ctx.rt.spawn(async move {
                        let r = session.answer(&body).await;
                        let ms = crate::store::now_ms().saturating_sub(sent);
                        let v = match r {
                            Ok(v) => {
                                info!(app, id = body["requestId"].as_str(), ms, "answered in hud");
                                json!({ "id": body["requestId"], "ok": true, "hud": v, "ms": ms })
                            }
                            Err(e) => {
                                warn!(app, error = %e, "hud didn't take the answer");
                                json!({ "id": body["requestId"], "ok": false, "error": e.to_string() })
                            }
                        };
                        let _ = tx.send(Ev::Answered(v));
                    });
                }
                None => {
                    // "Other" text: hud only takes one of its options.
                    (self.s.log)(json!({ "e": "unanswerable", "id": q.id }));
                    self.status.last_answer = Some(
                        json!({ "id": q.id, "ok": false, "error": "hud takes one of its options, not other text" }),
                    );
                }
            },
            (AskReply::Withdrawn, _) => {
                // Something else asked on the block and took the card: the
                // question stays hud's to answer, and isn't raised again
                // over it.
                (self.s.log)(json!({ "e": "replaced", "id": id }));
            }
            (other, _) => {
                (self.s.log)(json!({ "e": "skipped", "id": id, "how": format!("{other:?}") }));
            }
        }
        self.reconcile().await;
    }
}

async fn sleep_until_ms(at: Option<u64>) {
    match at {
        Some(at) => {
            let now = crate::store::now_ms();
            tokio::time::sleep(Duration::from_millis(at.saturating_sub(now) + 5)).await
        }
        None => std::future::pending().await,
    }
}

/// One tab's chat stream, followed again with backoff whenever it drops.
async fn stream(session: Arc<Session>, chat: String, tx: mpsc::UnboundedSender<Ev>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        match session.stream(&chat).await {
            Ok(mut res) => {
                let _ = tx.send(Ev::Connected(chat.clone()));
                backoff = BACKOFF_MIN;
                let mut buf = String::new();
                loop {
                    match res.chunk().await {
                        Ok(Some(bytes)) => {
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                            while let Some(at) = buf.find("\n\n") {
                                let frame: String = buf.drain(..at + 2).collect();
                                frame_events(&chat, &frame, &tx);
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            debug!(chat, error = %e.without_url(), "chat stream dropped");
                            break;
                        }
                    }
                }
                let _ = tx.send(Ev::Dropped(chat.clone()));
            }
            Err(HudError::Unauthorized) => {
                let _ = tx.send(Ev::Unauthorized);
                return;
            }
            Err(HudError::Other(e)) => debug!(chat, error = e, "chat stream"),
        }
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Pacing for the work board's reads.
struct Board {
    reading: bool,
    /// Another read is wanted after this one ...
    again: bool,
    /// ... at once (an approve), not at `next`.
    now: bool,
    started: tokio::time::Instant,
    /// The soonest the live feed may start the next read.
    next: tokio::time::Instant,
}

impl Default for Board {
    fn default() -> Self {
        let now = tokio::time::Instant::now();
        Self { reading: false, again: false, now: false, started: now, next: now }
    }
}

impl Board {
    fn start(&mut self) {
        (self.reading, self.again, self.now) = (true, false, false);
        self.started = tokio::time::Instant::now();
    }

    /// A read done: the next waits as long as this one took.
    fn done(&mut self) {
        self.reading = false;
        self.next = tokio::time::Instant::now() + self.started.elapsed().max(BOARD_GAP);
    }
}

/// hud's live feed: a change (any frame, the first included) is a reason
/// to read the work board again. Followed again with backoff.
async fn live_feed(session: Arc<Session>, tx: mpsc::UnboundedSender<Ev>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        let res = Session::checked(
            session.req(reqwest::Method::GET, "/api/live/stream").header("accept", "text/event-stream").send().await,
        )
        .await;
        match res {
            Ok(mut res) => {
                backoff = BACKOFF_MIN;
                let _ = tx.send(Ev::Live);
                let mut buf = String::new();
                while let Ok(Some(bytes)) = res.chunk().await {
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    let mut moved = false;
                    while let Some(at) = buf.find("\n\n") {
                        let frame: String = buf.drain(..at + 2).collect();
                        // Comments (keepalives) aren't changes.
                        moved |= frame.lines().any(|l| l.starts_with("data:"));
                    }
                    if moved {
                        let _ = tx.send(Ev::Live);
                    }
                }
            }
            Err(HudError::Unauthorized) => {
                let _ = tx.send(Ev::Unauthorized);
                return;
            }
            Err(HudError::Other(e)) => {
                debug!(error = e, "hud's live feed");
                // No feed (an older hud): read the board once anyway.
                let _ = tx.send(Ev::Live);
            }
        }
        if tx.is_closed() {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// The pending gates on hud's work board, as M34's [`Gate`]s.
///
/// [`Gate`]: arugula_proto::Gate
pub fn board_gates(origin: &str, app: &str, board: &Value) -> Vec<arugula_proto::Gate> {
    board["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|g| g["id"] == "approve")
        .flat_map(|g| g["items"].as_array().cloned().unwrap_or_default())
        .filter_map(|i| {
            let g = &i["gate"];
            Some(arugula_proto::Gate {
                member: g["member"].as_str()?.to_owned(),
                op: g["component"].as_str()?.to_owned(),
                gate: g["name"].as_str()?.to_owned(),
                env: g["env"].as_str().map(str::to_owned),
                since: None,
                expires: None,
                approvals: g["approvals"].as_u64().unwrap_or(0),
                needed: g["needed"].as_u64().unwrap_or(1),
                command: g["approve"].as_str().map(str::to_owned),
                source: arugula_proto::GateSource::Hud { box_url: origin.to_owned(), app: app.to_owned() },
            })
        })
        .collect()
}

/// Arugula's name for a person as hud takes a display name: an email by
/// its local part, characters hud refuses as `-`, at most 32; `None` for
/// what can't be one (empty, or one of hud's role labels).
pub fn hud_name(name: &str) -> Option<String> {
    let name = name.trim();
    let name = match name.split_once('@') {
        Some((local, _)) if !local.is_empty() => local,
        _ => name,
    };
    let mapped: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_.'".contains(c) { c } else { '-' })
        .collect::<String>()
        .split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let start = mapped.trim_start_matches(|c: char| !c.is_alphanumeric());
    let out: String = start.chars().take(32).collect::<String>().trim_end().to_owned();
    let lower = out.to_lowercase();
    let reserved = ["owner", "player", "spectator"].iter().any(|r| {
        lower == *r
            || lower
                .strip_prefix(r)
                .and_then(|rest| rest.strip_prefix(' '))
                .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
    });
    (!out.is_empty() && !reserved).then_some(out)
}

/// What one server-sent event says, for the follower.
fn frame_events(chat: &str, frame: &str, tx: &mpsc::UnboundedSender<Ev>) {
    let data: String = frame
        .split('\n')
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|l| l.strip_prefix(' ').unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return;
    }
    let Ok(m) = serde_json::from_str::<Value>(&data) else { return };
    if m["type"] == "hud-chat-queue" {
        let question = m.get("question").filter(|q| q.is_object()).cloned();
        let _ = tx.send(Ev::Queue { chat: chat.to_owned(), question });
    }
    for b in [&m, &m["block"]] {
        if b["kind"] == "permission_request"
            && b["answered"].as_bool() == Some(true)
            && let Some(id) = b["id"].as_str()
        {
            let _ = tx.send(Ev::Settled(id.to_owned()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_hud_takes() {
        assert_eq!(hud_name("friend@example.com").as_deref(), Some("friend"));
        assert_eq!(hud_name("Sam Doe").as_deref(), Some("Sam Doe"));
        assert_eq!(hud_name("`sam` (admin): x").as_deref(), Some("sam- -admin-- x"));
        assert_eq!(hud_name(&"a".repeat(40)).map(|n| n.len()), Some(32));
        assert_eq!(hud_name("owner"), None);
        assert_eq!(hud_name("Player 2"), None);
        assert_eq!(hud_name("players"), Some("players".into()));
        assert_eq!(hud_name("  "), None);
        assert_eq!(hud_name("@x").as_deref(), Some("x"));
    }

    #[test]
    fn gates_from_the_work_board() {
        let board = json!({ "v": 1, "groups": [
            { "id": "decide", "items": [{ "key": "d" }] },
            { "id": "approve", "items": [
                { "key": "gate:delivery/release/ship@prod", "gate": { "member": "delivery", "component": "release",
                  "name": "ship", "env": "prod", "needed": 2, "approvals": 1, "approve": "chant approve release ship --env prod" } },
                { "key": "odd" },
            ] },
        ]});
        let gates = board_gates("https://b.example", "pinboard", &board);
        assert_eq!(gates.len(), 1);
        let g = &gates[0];
        assert_eq!(
            (g.key().as_str(), g.env.as_deref(), g.approvals, g.needed),
            ("delivery/release/ship", Some("prod"), 1, 2)
        );
        assert_eq!(g.bundle(), "gate:https://b.example");
    }

    #[test]
    fn cookies_are_kept_and_cleared() {
        let mut jar = BTreeMap::new();
        set_cookie(&mut jar, "hud_session=abc; Path=/; HttpOnly; SameSite=None; Secure; Partitioned");
        set_cookie(&mut jar, "box_next=%2F__hud%2Fwork; Path=/; Max-Age=120");
        assert_eq!(cookie_header(&jar), "box_next=%2F__hud%2Fwork; hud_session=abc");
        set_cookie(&mut jar, "box_next=; Path=/; Max-Age=0");
        assert_eq!(cookie_header(&jar), "hud_session=abc");
    }

    #[test]
    fn a_queue_frame_is_a_card() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let q = json!({ "requestId": "r1", "summary": "Which colour?", "askedAt": 5, "expiresAt": 300005,
            "options": [{ "optionId": "o0", "name": "Green", "description": "Matches" }, { "optionId": "o1", "name": "Blue" }] });
        let frame =
            format!("event: x\ndata: {}\n\n", json!({ "type": "hud-chat-queue", "chatKey": "c", "question": q }));
        frame_events("c", &frame, &tx);
        frame_events("c", "data: {\"type\":\"hud-chat-presence\"}\n\n", &tx);
        frame_events(
            "c",
            &format!(
                "data: {}\n\n",
                json!({ "block": { "kind": "permission_request", "id": "r1", "answered": true } })
            ),
            &tx,
        );
        let Some(Ev::Queue { question: Some(v), .. }) = rx.try_recv().ok() else { panic!("no queue frame") };
        let q = Question::parse("c", &v).unwrap();
        assert!(matches!(rx.try_recv(), Ok(Ev::Settled(id)) if id == "r1"));
        let ask = q.ask();
        assert_eq!(
            (ask.source.as_str(), ask.agent.as_deref(), ask.headline().as_str()),
            ("hud", Some("hud"), "Which colour?")
        );
        assert_eq!(
            ask.questions.as_ref().unwrap()[0]["options"][0],
            json!({ "label": "Green", "description": "Matches" })
        );
        assert_eq!(q.picked(&json!({ "question_0": "Blue" })), Some("o1"));
        assert_eq!(q.picked(&json!({ "question_0_custom": "Red" })), None);
        assert_eq!(q.expires_at, Some(300005));
    }

    #[test]
    fn a_prompt_goes_to_the_tab_named_or_the_first() {
        let tabs = parse_tabs(&json!({ "tabs": [{ "chatKey": "k1", "title": "Main" }, { "chatKey": "k2" }] }));
        assert_eq!(tabs[1], Tab { chat: "k2".into(), title: "k2".into() }, "untitled: its key");
        assert_eq!(pick_tab(&tabs, None).unwrap().chat, "k1");
        assert_eq!(pick_tab(&tabs, Some("k2")).unwrap().chat, "k2");
        assert_eq!(pick_tab(&tabs, Some("main")).unwrap().chat, "k1");
        assert_eq!(pick_tab(&tabs, Some("nope")).unwrap_err(), "no tab \"nope\" in the box (it has: Main, k2)");
        assert!(pick_tab(&[], None).is_err());
    }

    #[test]
    fn why_hud_refused_a_prompt() {
        let full = json!({ "error": "queue is full", "reason": "queue-full", "queued": 5 });
        assert_eq!(prompt_refused(429, &full), "the tab's queue is full: wait for its turn to finish");
        let budget = json!({ "error": "out of turns today", "reason": "turn-budget" });
        assert_eq!(prompt_refused(429, &budget), "hud: out of turns today (turn-budget)");
        assert_eq!(prompt_refused(403, &json!({})), "hud refused: this session may only watch");
        assert_eq!(prompt_refused(502, &json!({})), "hud answered 502");
    }
}
