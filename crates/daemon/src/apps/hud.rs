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
//!   via: "illogical"}`, naming whoever answered in illogical; that needs
//!   hud's trusted-follower change (arugula-salad track A5). Without one,
//!   hud records the session's own player, the box's owner.
//!
//! A block shows one question at a time: with several tabs asking, the
//! oldest is on the card and the next follows when it's answered.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use futures_util::future::BoxFuture;
use illogical_proto::ask::{Ask, AskKind};
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{debug, info, warn};

use crate::{block::BlockCtx, mux::AskReply};

/// How often the box's tabs are listed again.
const TABS_EVERY: Duration = Duration::from_secs(30);
/// Backoff for a dropped stream or a session that can't be made.
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
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
        let http = reqwest::Client::builder()
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

    /// The box's chat tabs.
    pub async fn tabs(&self) -> Result<Vec<String>, HudError> {
        let res =
            Self::checked(self.req(reqwest::Method::GET, "/api/tabs").timeout(Duration::from_secs(15)).send().await)
                .await?;
        let v: Value = res.json().await.map_err(|e| HudError::Other(format!("tabs: {}", e.without_url())))?;
        Ok(v["tabs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["chatKey"].as_str().map(str::to_owned))
            .collect())
    }

    /// A tab's chat stream (server-sent events), from now.
    async fn stream(&self, chat: &str) -> Result<reqwest::Response, HudError> {
        let url = format!("/api/chat/stream?chatKey={}", enc(chat));
        Self::checked(self.req(reqwest::Method::GET, &url).header("accept", "text/event-stream").send().await).await
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
        by: Option<illogical_proto::Driver>,
    },
    Answered(Value),
}

/// The follower: runs until the handle is dropped (its block closed).
pub struct Follower {
    task: JoinHandle<()>,
}

impl Drop for Follower {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub fn start(setup: Setup) -> Follower {
    let rt = setup.ctx.rt.clone();
    Follower { task: rt.spawn(run(setup)) }
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

async fn run(s: Setup) {
    let (tx, mut rx) = mpsc::unbounded_channel();
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
            }
        };
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
        by: Option<illogical_proto::Driver>,
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
                        && let Some(n) = &name
                    {
                        body["onBehalfOf"] = json!({ "name": n, "via": "illogical" });
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
}
