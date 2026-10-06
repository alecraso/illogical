//! Agent blocks (M6b): an agent run as structured UI, driven over the Agent
//! Client Protocol (JSON-RPC 2.0 on the agent server's stdio).
//!
//! The block is an ACP client. It starts an agent server (Claude Code
//! through `claude-agent-acp`, Codex through `codex-acp`, a Fountain agent
//! through `fountain acp`, or any ACP command; see [`defs`]), opens a
//! session, and turns `session/update`s into a transcript: your prompts, the
//! agent's messages and thoughts, and tool calls with their commands'
//! output. Permission requests become cards with approve and deny.
//!
//! **The log is the JSON-RPC stream.** Every frame in either direction is a
//! line in the block's log (`{"t":ms,"d":"in"|"out","m":frame}`), plus notes
//! of our own (`"d":"note"`: the server started or stopped, a denial's
//! reason). Everything else (the transcript, open permission requests, our
//! outstanding requests and next id, cost) is rebuilt from it by the same
//! code that handles frames live, so a restarted daemon picks up exactly
//! where the last one was.
//!
//! **Turn state** comes from the protocol: `working` while our
//! `session/prompt` is outstanding, `needs-input` while a permission request
//! is open (which pushes to the phone), `done` or `idle` from the stop
//! reason.
//!
//! **Permissions.** "Always allow" is a rule in the block's config, answered
//! by the block itself: it never picks the agent's `allow_always`, which
//! `claude-agent-acp` writes into your repo's `.claude/settings.local.json`.
//! A standing rule (#166, [`crate::rules`]) is "always" kept by the daemon
//! for a directory or every block; blocks check those after their own.
//! Cancelling a turn answers open requests `cancelled`; a card goes when its
//! tool call ends (Fountain refuses an unanswered request after 5 minutes
//! without telling the client).
//!
//! **Questions and forms** (M6c). The block declares form and URL
//! elicitation, so Claude's AskUserQuestion arrives as `elicitation/create`
//! (without the capability the adapter disables the tool). One whose
//! `toolCallId` is an AskUserQuestion tool call becomes a question card
//! (its questions from the tool call's `rawInput`); any other form is drawn
//! from its schema, and a URL elicitation is a card with a link. They are
//! open requests like permissions: `needs-input`, in the log, answered by
//! id (`answer`, `decline`). Stop is `session/cancel` alone: the agent
//! withdraws its own request with `$/cancel_request`.
//!
//! **Restarts.** A local agent server keeps running through a daemon
//! restart (its own scope; its pipes in the FD store; see [`link`]). After a
//! reboot the block starts it again and reopens the session with
//! `session/resume` (no replay; the transcript is ours) or `session/load`
//! (whose replay is merged in). The restart policy decides whether that
//! happens by itself (`none` and `rerun-ask` wait for "Resume").

pub mod adapters;
pub mod defs;
pub mod images;
mod link;
pub mod transcript;

use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::BoxFuture;
use illogical_proto::{
    Attention, BlockType, Policy,
    ask::{self, Ask, AskKind},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{info, warn};

use self::{
    defs::{Def, Kind},
    link::{FromAgent, Link, Sink},
    transcript::{Applied, Entry, Transcript},
};
use crate::{
    block::{Block, BlockCtx, no_method},
    review::Runner,
    store::{Event, PaneLog, now_ms},
};

/// Entries in the state clients get (the rest are in `capture --text`).
const ENTRIES_IN_STATE: usize = 400;
/// Clients get the new state at most this often while the agent streams.
const PUBLISH_EVERY: Duration = Duration::from_millis(120);
/// How often to ask Fountain whether a turn that ran while we were away is
/// over.
const REMOTE_POLL: Duration = Duration::from_secs(15);
/// When an agent says "retry shortly" (Fountain provisioning a sandbox).
const RETRY_AFTER: Duration = Duration::from_secs(5);
const MAX_RETRIES: u32 = 12;
/// How often an opened conversation's transcript and holder are checked
/// (M33).
const FOLLOW_EVERY: Duration = Duration::from_secs(1);

/// What makes the block again (`layout.json`). Nothing secret.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(flatten)]
    pub def: Def,
    /// Where it works (on its machine, for a VM agent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The ACP session, once there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// MCP servers the session gets, as ACP takes them (`{name, command,
    /// args, env}` for a stdio one). Their forms and sign-in links come as
    /// questions (M6c).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<Value>,
    /// What "always allow" allowed; the block answers these itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<Rule>,
    /// The first prompt, sent once the session is open (not kept).
    #[serde(default, skip_serializing)]
    pub prompt: Option<String>,
    /// A Claude Code conversation this block opened (M33).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import: Option<Import>,
    /// Fork the session before reopening it (M33): set by `fork`, cleared
    /// when the fork is made.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fork: bool,
    /// Following a conversation that exists (M45b: a Fountain runner's):
    /// it's loaded, or the block stops with the reason. Never a new session.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub follow: bool,
}

/// Where an opened conversation came from (M33). Until it's continued the
/// block shows its transcript, read again as it grows; continuing freezes
/// what it had into the block (`imported.json`) and resumes the session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Import {
    /// Its transcript.
    pub path: String,
    /// `terminal`, `desktop` or `other`.
    #[serde(default)]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The model of its last reply, set again after resuming (a resume
    /// resets it, S20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// A request this block approves without asking: a tool (by name, else
/// kind), and the exact title (the command line) unless any is fine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The agent server is starting, or the session opening.
    Starting,
    /// Waiting for you.
    Ready,
    /// A turn is running.
    Working,
    /// A Fountain turn is running on Fountain while this block isn't
    /// following it (it reconnected mid-turn); it loads again when it ends.
    Remote,
    /// Not running (after a restart with policy `none`, until "Resume").
    Stopped,
    /// The agent server went away.
    Exited,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Opt {
    pub id: String,
    pub name: String,
    pub kind: String,
}

/// An open `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Perm {
    /// Its JSON-RPC id, as a string: what `approve`/`deny` take.
    pub id: String,
    #[serde(skip)]
    rpc: Value,
    pub tool_call_id: String,
    pub tool: String,
    pub title: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub options: Vec<Opt>,
    pub at_ms: u64,
}

/// An open `elicitation/create`.
#[derive(Debug, Clone, PartialEq)]
pub struct Elicit {
    pub ask: Ask,
    rpc: Value,
    /// A URL elicitation's id, which `elicitation/complete` names.
    elicitation_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TurnStat {
    pub prompt: String,
    pub started_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    /// This turn's share of the session's cumulative cost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Value>,
    #[serde(skip)]
    cost_base: Option<f64>,
}

/// A prompt waiting for the agent: its text, and images (M71) by name in
/// the block's folder.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Queued {
    pub text: String,
    pub images: Vec<String>,
}

impl Queued {
    /// What the block shows of it while it waits.
    fn label(&self) -> String {
        match self.images.len() {
            0 => self.text.clone(),
            1 => format!("{} [image]", self.text).trim_start().to_owned(),
            n => format!("{} [{n} images]", self.text).trim_start().to_owned(),
        }
    }
}

/// Why we sent a request, for when its answer comes.
#[derive(Debug, Clone, PartialEq)]
enum Purpose {
    Init,
    New,
    /// `merge`: we already have a transcript; fold the replay into it.
    Load {
        merge: bool,
    },
    Resume,
    /// `session/fork` (M33): reopen the new session when it answers.
    Fork,
    SetModel,
    SetMode(String),
    Prompt,
    Other,
}

/// What handling a frame asks for, live (nothing happens while rebuilding
/// from the log).
#[derive(Debug, Clone, PartialEq)]
enum Effect {
    /// The server is initialized: open (or reopen) the session.
    OpenSession,
    /// The session is open (`fresh`: just made).
    SessionOpen { fresh: bool },
    /// Reopening failed: make a new session instead.
    SessionLost(String),
    /// A turn ended: send what's queued.
    TurnEnded,
    /// A permission request came: answer it if a rule allows it.
    Permission(String),
    /// An agent request we don't serve.
    Unsupported(Value, String),
    /// A tool call finished (for history).
    ToolFinished(String),
    /// Fountain: a turn may still be running remotely.
    CheckRemote,
    /// The agent said to try the prompt again shortly.
    Retry(Queued),
}

struct Inner {
    cfg: Config,
    t: Transcript,
    /// A `session/load` replay being collected, to merge when it ends.
    replay: Option<Transcript>,
    status: Status,
    error: Option<String>,
    pending: Vec<Perm>,
    /// Open questions and forms.
    asks: Vec<Elicit>,
    ours: BTreeMap<u64, Purpose>,
    next_id: u64,
    agent_info: Value,
    caps: Value,
    title: Option<String>,
    prompt_id: Option<u64>,
    queue: VecDeque<Queued>,
    cost: Option<f64>,
    currency: Option<String>,
    turns: Vec<TurnStat>,
    last_stop: Option<String>,
    /// A turn was outstanding when the server we followed went away.
    interrupted: bool,
    log: Option<PaneLog>,
    link: Option<Link>,
    /// A VM agent's MCP relay (#59); it lasts as long as the block.
    relay: Option<crate::mcp::relay::Relay>,
    /// Bumped for each server started, so a dead one's last words are
    /// ignored.
    generation: u64,
    pid: Option<u32>,
    reported: Option<Attention>,
    closing: bool,
    /// While rebuilding from the log, effects aren't acted on.
    live: bool,
    /// An imported conversation was frozen into the block (M33).
    frozen: bool,
    /// Who else holds an imported conversation now (M33).
    held: Option<crate::conversations::Live>,
    /// The transcript's size and mtime when last read.
    import_stamp: Option<(u64, std::time::SystemTime)>,
    /// The transcript as read so far (#80).
    follow: crate::conversations::convert::Follow,
    /// The session's `configOptions` (for choosing a model).
    config_options: Value,
    /// Its adapter isn't installed, or has no Node (#111): what to show
    /// (`adapters::Status::json`), until it starts.
    adapter: Option<Value>,
    /// M44: the Fountain agent it wears, once put on (in memory only: its
    /// MCP servers' values may be secrets). Put on again after a restart.
    worn: Option<Arc<crate::fountain::wear::Worn>>,
    /// Putting it on now.
    wearing: bool,
    /// #161: waiting for this host's shell environment before spawning.
    awaiting_shell: bool,
    /// #128: illogical's own MCP token, when it goes by reference (a local
    /// Claude Code), to keep out of logs too.
    token: Option<String>,
}

enum Msg {
    Frame(u64, Value),
    Closed(u64, String),
    /// Something changed outside the actor (a method call).
    Changed,
}

pub struct Agent {
    ctx: BlockCtx,
    inner: Arc<Mutex<Inner>>,
    tx: mpsc::UnboundedSender<Msg>,
}

fn is_finished_stop(s: &str) -> bool {
    !matches!(s, "cancelled")
}

fn rpc_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

impl Inner {
    fn new(cfg: Config, log: Option<PaneLog>) -> Self {
        Self {
            cfg,
            t: Transcript::default(),
            replay: None,
            status: Status::Starting,
            error: None,
            pending: vec![],
            asks: vec![],
            ours: BTreeMap::new(),
            next_id: 1,
            agent_info: Value::Null,
            caps: Value::Null,
            title: None,
            prompt_id: None,
            queue: VecDeque::new(),
            cost: None,
            currency: None,
            turns: vec![],
            last_stop: None,
            interrupted: false,
            log,
            link: None,
            relay: None,
            generation: 0,
            pid: None,
            reported: None,
            closing: false,
            live: false,
            frozen: false,
            held: None,
            import_stamp: None,
            follow: Default::default(),
            config_options: Value::Null,
            adapter: None,
            worn: None,
            wearing: false,
            awaiting_shell: false,
            token: None,
        }
    }

    fn write_log(&mut self, d: &str, m: &Value) {
        if let Some(log) = self.log.as_mut() {
            let mut line = json!({ "t": now_ms(), "d": d, "m": m }).to_string().into_bytes();
            line.push(b'\n');
            if let Err(e) = log.append(&line) {
                warn!(error = %e, "can't write the agent log");
            }
        }
    }

    fn note(&mut self, e: Value) {
        self.write_log("note", &e);
        self.on_note(&e, now_ms());
    }

    /// Queue a prompt for the agent. It's in the log, so a prompt queued
    /// while the agent is down survives a daemon restart; sending it takes
    /// it off (see `session/prompt` in `on_out`).
    fn enqueue(&mut self, q: &Queued, front: bool) {
        self.note(json!({ "e": "queue", "text": q.text, "images": q.images, "front": front }));
    }

    /// The MCP servers a session gets: the block's own, and illogical's
    /// (M16), scoped to the block's tab. Over HTTP on loopback when the
    /// agent takes it, else `illogical mcp` on stdio with the token in its
    /// environment. A VM's agent can't reach the host, so it gets a client
    /// for the relay the daemon opens into its VM (#59). Not for a Fountain
    /// agent (it runs in Fountain's sandbox).
    fn servers(&self, ctx: &BlockCtx) -> Vec<Value> {
        let mut list = self.cfg.mcp_servers.clone();
        // M44: a worn Fountain agent's, values resolved.
        if let Some(w) = &self.worn {
            let extra: Vec<Value> =
                w.servers.iter().filter(|s| !list.iter().any(|o| o["name"] == s["name"])).cloned().collect();
            list.extend(extra);
        }
        let Some(link) = &ctx.mcp else { return list };
        if self.cfg.def.agent == Kind::Fountain || list.iter().any(|s| s["name"] == crate::mcp::SERVER_NAME) {
            return list;
        }
        if ctx.sprite.is_some() {
            list.push(crate::mcp::relay::server_entry(ctx.id));
            return list;
        }
        // #128: the SDK puts Claude Code's MCP config on its command line,
        // which anyone on the machine can read; Claude Code expands `${…}`
        // there itself (M44), so a local Claude Code gets a reference, and
        // the token is in its adapter's environment ([`MCP_TOKEN_ENV`]).
        // Other agents get the token itself, as before.
        let token = if by_reference(&self.cfg.def, ctx) {
            format!("${{{MCP_TOKEN_ENV}}}")
        } else {
            link.tokens.block_token(ctx.id)
        };
        list.push(if self.caps["mcpCapabilities"]["http"] == true {
            json!({
                "type": "http",
                "name": crate::mcp::SERVER_NAME,
                "url": link.url,
                "headers": [{ "name": "Authorization", "value": format!("Bearer {token}") }],
            })
        } else {
            json!({
                "name": crate::mcp::SERVER_NAME,
                "command": link.cli.display().to_string(),
                "args": ["mcp", "--socket", link.socket.display().to_string()],
                "env": [{ "name": "ILLOGICAL_MCP_TOKEN", "value": token }],
            })
        });
        list
    }

    /// What never goes into the log or the transcript: a worn agent's
    /// secrets, and illogical's own token where it went by reference.
    fn secrets(&self) -> Vec<String> {
        let mut out: Vec<String> = self.worn.as_ref().map(|w| w.secrets.clone()).unwrap_or_default();
        out.extend(self.token.clone());
        out
    }

    /// Send a frame: log it, account for it, then write it.
    fn out(&mut self, frame: Value) {
        let secrets = self.secrets();
        self.write_log("out", &without_images(&redacted(&frame, &secrets)));
        self.on_out(&frame, now_ms());
        if let Some(link) = &self.link {
            link.send(frame.to_string().into_bytes());
        }
    }

    fn request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.out(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        id
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.out(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn session(&self) -> Value {
        self.cfg.session_id.clone().map(Value::String).unwrap_or(Value::Null)
    }

    // ---- frames, live or from the log -----------------------------------

    fn on_note(&mut self, e: &Value, at: u64) {
        match e["e"].as_str().unwrap_or("") {
            "queue" => {
                let q =
                    Queued { text: e["text"].as_str().unwrap_or_default().to_owned(), images: strings(&e["images"]) };
                match e["front"].as_bool() {
                    Some(true) => self.queue.push_front(q),
                    _ => self.queue.push_back(q),
                }
            }
            "queue_clear" => self.queue.clear(),
            "spawn" => {
                // A new server: nothing outstanding carries over.
                if self.prompt_id.is_some() || !self.pending.is_empty() || !self.asks.is_empty() {
                    self.interrupted = true;
                }
                self.ours.clear();
                self.pending.clear();
                self.asks.clear();
                self.prompt_id = None;
                self.replay = None;
                self.status = Status::Starting;
                self.error = None;
                self.adapter = None;
                let just_continued =
                    matches!(self.t.entries.last(), Some(Entry::Note { text, .. }) if text == "Continued in illogical");
                // A block opened on a session it has no transcript of (M45b's
                // *Follow*: a Fountain conversation) didn't start before: it
                // loads the session, which replays it.
                if e["resume"].as_bool() == Some(true) && !just_continued && !self.t.entries.is_empty() {
                    self.t.note("Started the agent again", at);
                }
            }
            "exit" | "stopped" => {
                if self.prompt_id.is_some() {
                    self.interrupted = true;
                }
                self.ours.clear();
                self.pending.clear();
                self.asks.clear();
                self.prompt_id = None;
                self.replay = None;
                let why = e["why"].as_str().unwrap_or("stopped").to_owned();
                if e["e"] == "exit" && e["closing"].as_bool() != Some(true) {
                    self.t.note(format!("The agent {why}"), at);
                    self.status = Status::Exited;
                    self.error = Some(format!("the agent {why}"));
                    self.adapter = e.get("adapter").filter(|a| !a.is_null()).cloned();
                } else {
                    self.status = Status::Stopped;
                }
            }
            "deny" => {
                let title = e["title"].as_str().unwrap_or("that");
                let by = by_of(e);
                match e["reason"].as_str().filter(|r| !r.is_empty()) {
                    Some(r) => self.t.note(format!("Denied {title}{by}: {r}"), at),
                    None => self.t.note(format!("Denied {title}{by}"), at),
                }
            }
            "from" => {
                let who = e["by"].as_str().unwrap_or("someone");
                self.t.note(format!("A follow-up from {who}"), at);
            }
            "approve" => {
                let title = e["title"].as_str().unwrap_or("that");
                let how = e["how"].as_str().unwrap_or("once");
                let text = match how {
                    "rule" => format!("Allowed {title} (always allowed)"),
                    "standing" => format!("Allowed {title} (standing rule: {})", e["rule"].as_str().unwrap_or("?")),
                    "always" => format!("Allowed {title}, and always from now on"),
                    "standing-new" => {
                        format!("Allowed {title}, and from now on: {}", e["rule"].as_str().unwrap_or("?"))
                    }
                    _ => format!("Allowed {title}"),
                };
                self.t.note(format!("{text}{}", by_of(e)), at);
            }
            "remote" => {
                self.status = Status::Remote;
                self.t.note("The turn is still running on Fountain; it shows here when it ends", at);
            }
            "remote_done" => {
                if self.status == Status::Remote {
                    self.status = Status::Ready;
                    // It ended on Fountain, which sent us no stop reason.
                    self.last_stop = Some("end_turn".into());
                    if let Some(t) = self.turns.last_mut().filter(|t| t.ended_ms.is_none()) {
                        t.ended_ms = Some(at);
                        t.stop = Some("end_turn".into());
                    }
                }
            }
            "answered" => {
                let text = e["summary"].as_str().unwrap_or("");
                self.t.note(format!("Answered{}: {text}", by_of(e)), at);
            }
            "skipped" => {
                let what = e["question"].as_str().unwrap_or("the question");
                self.t.note(format!("Skipped{}: {what}", by_of(e)), at);
            }
            "dismissed" => {
                let key = e["id"].as_str().unwrap_or_default();
                self.asks.retain(|a| a.ask.id != key);
            }
            "imported" => {
                // What the conversation had before it was continued here.
                let entries = e["file"]
                    .as_str()
                    .and_then(|f| std::fs::read(f).ok())
                    .and_then(|b| serde_json::from_slice::<Vec<Entry>>(&b).ok())
                    .unwrap_or_default();
                self.t = Transcript::from_entries(entries);
                self.frozen = true;
                // No longer following whoever else has it.
                self.held = None;
                self.follow = Default::default();
                self.status = Status::Stopped;
                self.t.note("Continued in illogical", at);
            }
            "error" => {
                let msg = e["message"].as_str().unwrap_or("error").to_owned();
                self.t.note(msg.clone(), at);
                self.error = Some(msg);
            }
            _ => {}
        }
    }

    fn on_out(&mut self, m: &Value, at: u64) {
        let method = m["method"].as_str();
        match (m.get("id"), method) {
            (Some(id), Some(method)) => {
                let Some(id) = id.as_u64() else { return };
                self.next_id = self.next_id.max(id + 1);
                let purpose = match method {
                    "initialize" => Purpose::Init,
                    "session/new" => Purpose::New,
                    "session/load" => {
                        let merge = !self.t.entries.is_empty();
                        self.replay = Some(Transcript::default());
                        Purpose::Load { merge }
                    }
                    "session/resume" => Purpose::Resume,
                    "session/fork" => Purpose::Fork,
                    "session/set_config_option" => Purpose::SetModel,
                    "session/set_mode" => Purpose::SetMode(m["params"]["modeId"].as_str().unwrap_or("").to_owned()),
                    "session/prompt" => {
                        // An image (or, to an agent that takes none, its
                        // path) is named in its block's `_meta` (M71).
                        let blocks = m["params"]["prompt"].as_array().map(Vec::as_slice).unwrap_or_default();
                        let image = |c: &Value| c["_meta"][images::META].as_str().map(str::to_owned);
                        let text: String = blocks
                            .iter()
                            .filter(|c| image(c).is_none())
                            .filter_map(|c| c["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        let sent = Queued { text, images: blocks.iter().filter_map(image).collect() };
                        if self.queue.front() == Some(&sent) {
                            self.queue.pop_front();
                        }
                        let Queued { text, images } = sent;
                        self.t.user(&text, images, at);
                        self.prompt_id = Some(id);
                        self.status = Status::Working;
                        self.error = None;
                        self.last_stop = None;
                        self.interrupted = false;
                        self.turns.push(TurnStat {
                            prompt: text.chars().take(120).collect(),
                            started_ms: at,
                            cost_base: self.cost,
                            ..Default::default()
                        });
                        Purpose::Prompt
                    }
                    _ => Purpose::Other,
                };
                self.ours.insert(id, purpose);
            }
            // Our answer to one of the agent's requests.
            (Some(id), None) => {
                let key = rpc_key(id);
                self.pending.retain(|p| p.id != key);
                // A link that was opened stays until the agent says it's done.
                let opened = m["result"]["action"] == "accept";
                self.asks.retain_mut(|a| {
                    if a.ask.id != key {
                        return true;
                    }
                    if a.ask.kind == AskKind::Url && opened {
                        a.ask.accepted = true;
                        return true;
                    }
                    false
                });
            }
            _ => {}
        }
    }

    fn on_in(&mut self, m: &Value, at: u64) -> Vec<Effect> {
        let mut fx = vec![];
        let method = m["method"].as_str();
        match (m.get("id"), method) {
            // An answer to one of ours.
            (Some(id), None) => {
                let Some(purpose) = id.as_u64().and_then(|id| self.ours.remove(&id)) else { return fx };
                let error = m.get("error").map(|e| e["message"].as_str().unwrap_or("error").to_owned());
                let r = &m["result"];
                match (purpose, error) {
                    (Purpose::Init, None) => {
                        self.caps = r["agentCapabilities"].clone();
                        self.agent_info = r["agentInfo"].clone();
                        fx.push(Effect::OpenSession);
                    }
                    (Purpose::New, None) => {
                        self.config_options = r["configOptions"].clone();
                        self.cfg.session_id = r["sessionId"].as_str().map(str::to_owned);
                        fx.push(Effect::SessionOpen { fresh: true });
                    }
                    (Purpose::Load { merge }, None) => {
                        self.config_options = r["configOptions"].clone();
                        if let Some(replay) = self.replay.take() {
                            if merge {
                                self.t.merge(replay);
                            } else {
                                self.t = replay;
                            }
                        }
                        fx.push(Effect::SessionOpen { fresh: false });
                    }
                    (Purpose::Resume, None) => {
                        self.config_options = r["configOptions"].clone();
                        fx.push(Effect::SessionOpen { fresh: false });
                    }
                    (Purpose::Fork, None) => {
                        // The fork isn't open yet (S20): reopen it as ours.
                        self.cfg.session_id = r["sessionId"].as_str().map(str::to_owned);
                        self.cfg.fork = false;
                        let sid = self.cfg.session_id.clone().unwrap_or_default();
                        self.t.note(format!("Forked into a new session ({sid}); the original is left as it was"), at);
                        fx.push(Effect::OpenSession);
                    }
                    (Purpose::Fork, Some(e)) => {
                        self.cfg.fork = false;
                        self.t.note(format!("Couldn't fork the session: {e}"), at);
                        self.error = Some(format!("couldn't fork: {e}"));
                    }
                    (Purpose::Load { .. } | Purpose::Resume, Some(e)) => {
                        self.replay = None;
                        fx.push(Effect::SessionLost(e));
                    }
                    // Fountain, while it provisions a sandbox: "retry shortly".
                    // The prompt goes again, as if this one hadn't happened.
                    (Purpose::Prompt, Some(e)) if e.contains("retry") => {
                        self.prompt_id = None;
                        self.status = Status::Ready;
                        self.turns.pop();
                        if let Some(Entry::User { text, images, .. }) = self.t.entries.last().cloned() {
                            self.t.entries.pop();
                            fx.push(Effect::Retry(Queued { text, images }));
                        }
                    }
                    (Purpose::Prompt, result) => {
                        self.prompt_id = None;
                        if self.status == Status::Working {
                            self.status = Status::Ready;
                        }
                        let stop = match &result {
                            None => r["stopReason"].as_str().unwrap_or("end_turn").to_owned(),
                            Some(_) => "error".to_owned(),
                        };
                        if let Some(t) = self.turns.last_mut() {
                            t.ended_ms = Some(at);
                            t.stop = Some(stop.clone());
                            if r["usage"].is_object() {
                                t.tokens = Some(r["usage"].clone());
                            }
                        }
                        if let Some(e) = result {
                            self.t.note(format!("The turn failed: {e}"), at);
                            self.error = Some(e);
                        }
                        self.last_stop = Some(stop);
                        // Requests the turn left open are moot.
                        self.pending.clear();
                        self.asks.clear();
                        fx.push(Effect::TurnEnded);
                    }
                    (Purpose::SetMode(mode), Some(e)) => {
                        self.t.note(format!("Couldn't switch to permission mode {mode}: {e}"), at);
                        self.error = Some(format!("permission mode {mode}: {e}"));
                    }
                    (Purpose::Init | Purpose::New, Some(e)) => {
                        self.t.note(format!("The agent couldn't start a session: {e}"), at);
                        self.error = Some(e);
                    }
                    (_, Some(e)) => warn!(error = e, "agent request failed"),
                    _ => {}
                }
            }
            (None, Some("session/update")) => {
                let u = &m["params"]["update"];
                match u["sessionUpdate"].as_str().unwrap_or("") {
                    "usage_update" => {
                        if let Some(amount) = u["cost"]["amount"].as_f64() {
                            self.cost = Some(amount);
                            self.currency = u["cost"]["currency"].as_str().map(str::to_owned);
                            if let Some(t) = self.turns.last_mut() {
                                t.cost = Some(amount - t.cost_base.unwrap_or(0.0));
                            }
                        }
                    }
                    "session_info_update" => {
                        if let Some(title) = u["title"].as_str() {
                            self.title = Some(title.to_owned());
                        }
                    }
                    // Your prompts come from what we sent; an agent echoing
                    // them live would say them twice. (A replay's count.)
                    "user_message_chunk" if self.replay.is_none() => {}
                    _ => {
                        let applied = match self.replay.as_mut() {
                            Some(r) => r.apply(u, at),
                            None => self.t.apply(u, at),
                        };
                        if let Applied::ToolFinished(id) = applied {
                            // Its card is moot (Fountain's refusal says only this).
                            self.pending.retain(|p| p.tool_call_id != id);
                            self.asks.retain(|a| a.ask.tool_call_id.as_deref() != Some(id.as_str()));
                            if self.replay.is_none() {
                                fx.push(Effect::ToolFinished(id));
                            }
                        }
                    }
                }
            }
            (Some(id), Some("session/request_permission")) => {
                let key = rpc_key(id);
                if self.pending.iter().any(|p| p.id == key) {
                    return fx;
                }
                let p = &m["params"];
                let tc = &p["toolCall"];
                let tool_call_id = tc["toolCallId"].as_str().unwrap_or("").to_owned();
                // What the transcript knows about it, for a card that makes sense.
                let known = self.t.tool(&tool_call_id).cloned();
                let tool = tc["name"]
                    .as_str()
                    .or(tc["_meta"]["claudeCode"]["toolName"].as_str())
                    .or(tc["kind"].as_str())
                    .or(known.as_ref().map(|k| k.kind.as_str()))
                    .unwrap_or("tool")
                    .to_owned();
                let title = tc["title"]
                    .as_str()
                    .or(p["_meta"]["permission"]["title"].as_str())
                    .map(str::to_owned)
                    .or(known.as_ref().map(|k| k.title.clone()))
                    .unwrap_or_else(|| tool.clone());
                let command = tc["rawInput"]["command"].as_str().map(str::to_owned).or(known.and_then(|k| k.command));
                let options = p["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| Opt {
                        id: o["optionId"].as_str().unwrap_or("").to_owned(),
                        name: o["name"].as_str().unwrap_or("").to_owned(),
                        kind: o["kind"].as_str().unwrap_or("").to_owned(),
                    })
                    .collect();
                self.pending.push(Perm {
                    id: key.clone(),
                    rpc: id.clone(),
                    tool_call_id,
                    tool,
                    title,
                    kind: tc["kind"].as_str().unwrap_or("").to_owned(),
                    command,
                    options,
                    at_ms: at,
                });
                fx.push(Effect::Permission(key));
            }
            (Some(id), Some("elicitation/create")) => {
                let key = rpc_key(id);
                if self.asks.iter().any(|a| a.ask.id == key) {
                    return fx;
                }
                let e = self.elicit(key, id, &m["params"], at);
                self.asks.push(e);
            }
            // The agent withdrew a request of its own (after `session/cancel`).
            (None, Some("$/cancel_request")) => {
                let key = rpc_key(&m["params"]["requestId"]);
                if self.asks.iter().any(|a| a.ask.id == key) {
                    self.asks.retain(|a| a.ask.id != key);
                    self.t.note("The question was withdrawn", at);
                }
                self.pending.retain(|p| p.id != key);
            }
            (None, Some("elicitation/complete")) => {
                let done = m["params"]["elicitationId"].as_str();
                self.asks.retain(|a| a.elicitation_id.is_none() || a.elicitation_id.as_deref() != done);
            }
            (Some(id), Some(method)) => fx.push(Effect::Unsupported(id.clone(), method.to_owned())),
            _ => {}
        }
        if self.cfg.def.agent == Kind::Fountain && fx.iter().any(|f| matches!(f, Effect::SessionOpen { fresh: false }))
        {
            fx.push(Effect::CheckRemote);
        }
        fx
    }

    /// An `elicitation/create` as a card: AskUserQuestion's questions (by its
    /// tool call), a link, or a generic form.
    fn elicit(&self, key: String, rpc: &Value, p: &Value, at: u64) -> Elicit {
        let message = p["message"].as_str().unwrap_or_default().to_owned();
        let tool_call_id = p["toolCallId"].as_str().map(str::to_owned);
        let tool = tool_call_id.as_deref().and_then(|id| self.t.tool(id));
        let mut ask = Ask {
            id: key,
            kind: AskKind::Form,
            message,
            questions: None,
            schema: None,
            url: None,
            accepted: false,
            tool_call_id,
            source: "agent".into(),
            agent: None,
            at_ms: at,
            tool: None,
            input: None,
            suggestions: None,
            session: None,
        };
        if p["mode"] == "url" {
            ask.kind = AskKind::Url;
            ask.url = p["url"].as_str().map(str::to_owned);
            let elicitation_id = p["elicitationId"].as_str().map(str::to_owned);
            return Elicit { ask, rpc: rpc.clone(), elicitation_id };
        }
        let schema = p["requestedSchema"].clone();
        if let Some(t) = tool.filter(|t| t.is_question()) {
            ask.questions = t.questions.clone().or_else(|| ask::questions_from_schema(&ask.message, &schema));
            if ask.questions.is_some() {
                ask.kind = AskKind::Questions;
            }
        }
        ask.schema = Some(schema);
        Elicit { ask, rpc: rpc.clone(), elicitation_id: None }
    }

    /// The question waiting for an answer (not a link already opened).
    fn open_ask(&self) -> Option<&Elicit> {
        self.asks.iter().find(|a| !a.ask.accepted)
    }

    /// Replay a log line.
    fn rebuild_line(&mut self, line: &[u8]) {
        let Ok(v) = serde_json::from_slice::<Value>(line) else { return };
        let at = v["t"].as_u64().unwrap_or(0);
        match v["d"].as_str() {
            Some("in") => {
                self.on_in(&v["m"], at);
            }
            Some("out") => self.on_out(&v["m"], at),
            Some("note") => self.on_note(&v["m"], at),
            _ => {}
        }
    }

    // ---- what clients see -------------------------------------------------

    fn attention(&self) -> (Attention, String) {
        if let Some(p) = self.pending.first() {
            return (Attention::NeedsInput, format!("wants to run {}", p.title));
        }
        if let Some(a) = self.open_ask() {
            return (Attention::NeedsInput, a.ask.headline());
        }
        match self.status {
            Status::Exited => (Attention::NeedsInput, self.error.clone().unwrap_or_else(|| "the agent stopped".into())),
            Status::Working | Status::Remote => (Attention::Working, "working".into()),
            Status::Starting | Status::Ready if !self.queue.is_empty() => (Attention::Working, "starting".into()),
            Status::Ready | Status::Starting if self.error.is_some() => {
                (Attention::NeedsInput, self.error.clone().unwrap_or_default())
            }
            Status::Ready => match self.last_stop.as_deref() {
                Some(s) if is_finished_stop(s) => (Attention::Done, self.done_text()),
                _ => (Attention::Idle, "idle".into()),
            },
            _ => (Attention::Idle, "idle".into()),
        }
    }

    fn done_text(&self) -> String {
        self.t
            .entries
            .iter()
            .rev()
            .find_map(|e| match e {
                Entry::Agent { text, .. } => Some(text.trim().chars().take(140).collect::<String>()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "finished".into())
    }

    fn state(&self, ctx: &BlockCtx) -> Value {
        let (from, entries) = self.t.for_state(ENTRIES_IN_STATE);
        let (attention, _) = self.attention();
        let total_tokens: u64 = self.turns.iter().filter_map(|t| t.tokens.as_ref()?["totalTokens"].as_u64()).sum();
        json!({
            "agent": self.cfg.def.agent,
            "label": self.cfg.def.label(),
            "title": self.title,
            "cwd": self.cfg.cwd,
            "vm": ctx.sprite.is_some(),
            "session_id": self.cfg.session_id,
            "server": self.agent_info,
            "status": self.status,
            "pid": self.pid,
            "attention": attention,
            "error": self.error,
            "adapter": self.adapter,
            "last_stop": self.last_stop,
            "current_tool": self.t.current_tool().map(|t| json!({ "id": t.id, "title": t.title, "kind": t.kind })),
            "pending": self.pending,
            "asks": self.asks.iter().map(|a| &a.ask).collect::<Vec<_>>(),
            "queued": self.queue.iter().map(Queued::label).collect::<Vec<_>>(),
            "cost": self.cost.map(|c| json!({ "total": c, "currency": self.currency, "last_turn": self.turns.last().and_then(|t| t.cost) })),
            "tokens": { "total": total_tokens, "last_turn": self.turns.last().and_then(|t| t.tokens.clone()) },
            "turns": self.turns.len(),
            "recent_turns": self.turns.iter().rev().take(20).collect::<Vec<_>>(),
            "allow": self.cfg.allow,
            "permission_mode": self.cfg.def.permission_mode,
            "user_settings": self.cfg.def.user_settings,
            // M44: what it wears (nothing secret), or that it's putting it on.
            "as_fountain": self.cfg.def.as_fountain,
            "worn": self.worn.as_ref().map(|w| &w.info),
            "wearing": self.wearing,
            "import": self.cfg.import.as_ref().map(|i| json!({
                "source": i.source,
                "path": i.path,
                "title": i.title,
                "continued": self.frozen,
                "held": self.held.as_ref().map(|l| json!({ "pid": l.pid, "pane": l.pane, "block": l.block, "entrypoint": l.entrypoint, "status": l.status, "place": l.place() })),
            })),
            "entries_from": from,
            "entries": entries,
        })
    }
}

impl Agent {
    pub fn create(ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
        let mut config = config;
        // A command line as one string is split into words.
        if let Some(c) = config["command"].as_str() {
            config["command"] = json!(defs::split_command(c));
        }
        // An MCP server as `NAME=COMMAND LINE` (the CLI's `--mcp`).
        if let Some(list) = config.get_mut("mcp_servers").and_then(Value::as_array_mut) {
            for m in list.iter_mut() {
                let Some(spec) = m.as_str() else { continue };
                let (name, line) = spec.split_once('=').ok_or_else(|| format!("--mcp {spec}: want NAME=COMMAND"))?;
                let mut argv = defs::split_command(line);
                if argv.is_empty() {
                    return Err(format!("--mcp {spec}: no command"));
                }
                let command = argv.remove(0);
                *m = json!({ "name": name, "command": command, "args": argv, "env": [] });
            }
        }
        let cfg: Config = serde_json::from_value(config).map_err(|e| format!("agent config: {e}"))?;
        cfg.def.check()?;
        let vm = ctx.sprite.is_some();
        if vm && ctx.provider.is_none() {
            return Err("VM agents need wisp (VM panes aren't set up)".into());
        }
        // Fail now, with a clear reason, rather than in the VM later.
        if vm && cfg.def.agent == Kind::Claude && !ctx.restoring {
            secret_env(&ctx)?;
        }
        if vm && cfg.def.agent == Kind::Fountain {
            return Err("Fountain agents run in Fountain's sandboxes, not in a VM here".into());
        }
        if vm && cfg.def.as_fountain.is_some() {
            return Err("a worn Fountain agent runs on this host (with your secrets), not in a VM".into());
        }
        let log = ctx.log().map_err(|e| format!("agent log: {e}"))?;
        let _ = std::fs::write(ctx.dir.join("kind"), "agent\n");
        let mut inner = Inner::new(cfg, None);
        // Everything it did before, from its log.
        if ctx.restoring
            && let Ok((_, bytes)) = log.read_from(log.start())
        {
            for line in bytes.split(|b| *b == b'\n') {
                inner.rebuild_line(line);
            }
        }
        inner.log = Some(log);
        inner.live = true;
        if by_reference(&inner.cfg.def, &ctx)
            && let Some(link) = &ctx.mcp
        {
            inner.token = Some(link.tokens.block_token(ctx.id));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let agent = Arc::new(Agent { ctx: ctx.clone(), inner: Arc::new(Mutex::new(inner)), tx });
        agent.begin();
        ctx.rt.spawn(run(agent.ctx.clone(), agent.inner.clone(), agent.tx.clone(), rx));
        Ok(agent)
    }

    /// Start, adopt, or (by policy) wait.
    fn begin(&self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(p) = inner.cfg.prompt.take() {
            inner.enqueue(&Queued { text: p, images: vec![] }, false);
        }
        if inner.cfg.import.is_some() {
            if let Some(t) = inner.cfg.import.as_ref().and_then(|i| i.title.clone()) {
                inner.title.get_or_insert(t);
            }
            // Opened, not continued: its transcript, read as it grows, and
            // nothing running (M33).
            if !inner.frozen {
                inner.status = Status::Stopped;
                refresh_import(&self.ctx, &mut inner);
                return;
            }
        }
        if self.ctx.restoring {
            // M44: a worn agent is put on again before anything it says is
            // read (its secrets to scrub, its _meta and servers for a new
            // session), then taken over.
            if inner.cfg.def.as_fountain.is_some() && inner.worn.is_none() {
                self.wear(&mut inner, true);
                return;
            }
            self.take_over(&mut inner);
            return;
        }
        self.spawn(&mut inner);
    }

    /// After a restart: carry on with the agent server still running from
    /// before it, else (by policy) start it again or wait.
    fn take_over(&self, inner: &mut Inner) {
        {
            // Still running from before the restart: carry on with it. One
            // an older daemon started without illogical's token in its
            // environment can't use the references it'd get now (#128):
            // it starts again (its session is reopened).
            let stale = by_reference(&inner.cfg.def, &self.ctx)
                && self.ctx.mcp.is_some()
                && !self.ctx.dir.join(TOKEN_IN_ENV).exists();
            if self.adopt(inner) {
                if stale {
                    info!(block = self.ctx.id, "agent server from an older daemon: starting it again (#128)");
                    if let Some(l) = inner.link.take() {
                        l.stop();
                    }
                    if let Some(pid) = inner.pid.take() {
                        link::kill_group(pid, self.ctx.dir.clone());
                    }
                    self.spawn(inner);
                    return;
                }
                if inner.status == Status::Starting && inner.cfg.session_id.is_some() && inner.ours.is_empty() {
                    inner.status = Status::Ready;
                }
                return;
            }
            // It's gone. Kill a straggler we can't talk to.
            if let Some(pid) = link::alive_pid(&self.ctx.dir) {
                link::kill_group(pid, self.ctx.dir.clone());
            }
            let wait = matches!(self.ctx.policy, Policy::None | Policy::Rerun { confirm: true });
            if wait && inner.cfg.session_id.is_some() {
                inner.note(json!({ "e": "stopped" }));
                return;
            }
        }
        self.spawn(inner);
    }

    fn sink(&self, generation: u64) -> Sink {
        let (inner, tx) = (Arc::downgrade(&self.inner), self.tx.clone());
        Arc::new(move |f| match f {
            FromAgent::Line(line) => {
                let inner = inner.upgrade();
                let mut g = inner.as_ref().map(|i| i.lock().unwrap());
                // M44: a worn agent's secrets never get past here (into the
                // log, the transcript, clients).
                let secrets = g.as_ref().map(|g| g.secrets()).unwrap_or_default();
                let line = if secrets.is_empty() {
                    line
                } else {
                    let text = String::from_utf8_lossy(&line);
                    match crate::fountain::wear::scrub(&text, &secrets) {
                        Some(t) => t.into_bytes(),
                        None => line,
                    }
                };
                let v: Value = match serde_json::from_slice(&line) {
                    Ok(v) => v,
                    Err(_) => {
                        info!(line = %String::from_utf8_lossy(&line), "agent said something that isn't JSON");
                        return;
                    }
                };
                // Logged before it's taken off the pipe.
                if let Some(g) = g.as_mut()
                    && g.generation == generation
                {
                    g.write_log("in", &v);
                }
                drop(g);
                let _ = tx.send(Msg::Frame(generation, v));
            }
            FromAgent::Closed(why) => {
                let _ = tx.send(Msg::Closed(generation, why));
            }
        })
    }

    fn adopt(&self, inner: &mut Inner) -> bool {
        match &self.ctx.sprite {
            None => {
                let mut kept = self.ctx.kept.lock().unwrap();
                inner.generation += 1;
                let sink = self.sink(inner.generation);
                match link::adopt_local(self.ctx.id, &self.ctx.dir, &mut kept, self.ctx.launch.fd_store, sink) {
                    Some((l, pid)) => {
                        inner.link = Some(l);
                        inner.pid = Some(pid);
                        true
                    }
                    None => false,
                }
            }
            Some(sprite) => {
                let Some(rec) = link::ExecRecord::read(&self.ctx.dir) else { return false };
                let Some(provider) = self.ctx.provider.clone() else { return false };
                inner.generation += 1;
                let sink = self.sink(inner.generation);
                let begin = link::VmBegin::Resume(rec);
                inner.link = Some(link::spawn_vm(
                    &self.ctx.rt,
                    provider.clone(),
                    sprite.clone(),
                    self.ctx.dir.clone(),
                    begin,
                    sink,
                ));
                self.relay(inner, provider, sprite);
                true
            }
        }
    }

    /// A VM agent's way to illogical's MCP server (#59): a relay into its
    /// VM, opened once and kept for the block's life (an agent that
    /// restarts connects again).
    fn relay(&self, inner: &mut Inner, provider: Arc<dyn crate::provider::Provider>, sprite: &str) {
        let Some(mcp) = &self.ctx.mcp else { return };
        if inner.cfg.def.agent == Kind::Fountain || inner.relay.as_ref().is_some_and(|r| !r.finished()) {
            return;
        }
        let r = crate::mcp::relay::start(&self.ctx.rt, provider, sprite.to_owned(), self.ctx.id, mcp.serve.clone());
        inner.relay = Some(r);
    }

    /// Start the agent server and initialize it.
    fn spawn(&self, inner: &mut Inner) {
        if let Some(old) = inner.link.take() {
            old.stop();
        }
        // M44: put the Fountain agent on first (its bundle, its servers'
        // secrets), then start.
        if inner.cfg.def.as_fountain.is_some() && inner.worn.is_none() {
            self.wear(inner, false);
            return;
        }
        // #161: a local agent runs with the user's shell environment, which
        // is still being resolved just after the daemon starts.
        if self.ctx.sprite.is_none() && self.ctx.shell_env.local_now().is_none() {
            self.await_shell_env(inner);
            return;
        }
        let vm = self.ctx.sprite.is_some();
        let launch = match inner.cfg.def.launch(&self.ctx.home, vm) {
            Ok(l) => l,
            Err(e) => return self.failed(inner, e),
        };
        let resume = inner.cfg.session_id.is_some();
        inner.note(json!({ "e": "spawn", "argv": launch.argv, "resume": resume }));
        inner.generation += 1;
        let sink = self.sink(inner.generation);
        match &self.ctx.sprite {
            None => {
                let cwd = inner.cfg.cwd.clone().map(PathBuf::from).unwrap_or_else(|| self.ctx.home.clone());
                // #161: the user's shell environment over the daemon's, as a
                // pane gets it, so the adapter finds the user's node (nvm,
                // Homebrew), not only what a launchd or systemd PATH has.
                let shell = self.ctx.shell_env.local_now().unwrap_or_default();
                let mut env = crate::shellenv::merge(&self.ctx.env, &shell, self.ctx.launch.exe.parent());
                env.extend(launch.env.iter().cloned());
                // M44 and #128: what the session's `${…}`s stand for, in the
                // adapter's environment (its own and its children's: never
                // on a command line).
                if let Some(w) = &inner.worn {
                    env.extend(w.env.iter().cloned());
                }
                let marker = self.ctx.dir.join(TOKEN_IN_ENV);
                if by_reference(&inner.cfg.def, &self.ctx)
                    && let Some(link) = &self.ctx.mcp
                {
                    let token = link.tokens.block_token(self.ctx.id);
                    env.push((MCP_TOKEN_ENV.into(), token.clone()));
                    inner.token = Some(token);
                    let _ = std::fs::write(&marker, b"");
                } else {
                    let _ = std::fs::remove_file(&marker);
                }
                // Claude Code's and Codex's adapters: say what's missing,
                // and how to install it, rather than fail to run it (#111).
                if inner.cfg.def.command.is_empty()
                    && let Some(a) = adapters::of(inner.cfg.def.agent)
                {
                    let st = adapters::status(a, &self.ctx.home, &env);
                    if !st.ok() {
                        return self.failed_with(inner, st.why(), Some(st.json()));
                    }
                }
                with_node_on_path(&mut env, &self.ctx.home);
                let spawn = link::LocalSpawn {
                    id: self.ctx.id,
                    dir: &self.ctx.dir,
                    argv: &launch.argv,
                    cwd: &cwd,
                    env: &env,
                    remove: &launch.remove,
                    launch: &self.ctx.launch,
                };
                match link::spawn_local(spawn, sink) {
                    Ok((l, pid)) => {
                        inner.link = Some(l);
                        inner.pid = Some(pid);
                    }
                    Err(e) => return self.failed(inner, e.to_string()),
                }
            }
            Some(sprite) => {
                let Some(provider) = self.ctx.provider.clone() else {
                    return self.failed(inner, "VM agents need wisp".into());
                };
                let mut secret = match inner.cfg.def.agent {
                    Kind::Claude => match secret_env(&self.ctx) {
                        Ok(s) => s,
                        Err(e) => return self.failed(inner, e),
                    },
                    _ => vec![],
                };
                secret.extend(launch.env.iter().cloned());
                let begin = link::VmBegin::New {
                    npm: launch.npm.map(str::to_owned),
                    cwd: inner.cfg.cwd.clone().unwrap_or_else(|| "/home/sprite".into()),
                    argv: launch.argv.clone(),
                    secret_env: secret,
                };
                link::ExecRecord::clear(&self.ctx.dir);
                inner.link = Some(link::spawn_vm(
                    &self.ctx.rt,
                    provider.clone(),
                    sprite.clone(),
                    self.ctx.dir.clone(),
                    begin,
                    sink,
                ));
                self.relay(inner, provider, sprite);
            }
        }
        inner.request(
            "initialize",
            json!({
                "protocolVersion": 1,
                // Zed's extension: command output on the tool call. No client
                // fs or terminals: no adapter uses them (S7).
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                    "_meta": { "terminal_output": true },
                    // Questions and forms (M6c). Objects, not booleans: the
                    // ACP SDK drops `true`, and the adapter then disables
                    // AskUserQuestion (S13).
                    "elicitation": { "form": {}, "url": {} },
                },
                "clientInfo": { "name": "illogical", "version": env!("CARGO_PKG_VERSION") },
            }),
        );
    }

    /// #161: spawn once this host's shell environment is resolved.
    fn await_shell_env(&self, inner: &mut Inner) {
        if inner.awaiting_shell {
            return;
        }
        inner.awaiting_shell = true;
        inner.status = Status::Starting;
        inner.error = None;
        let agent = Agent { ctx: self.ctx.clone(), inner: self.inner.clone(), tx: self.tx.clone() };
        self.ctx.rt.spawn(async move {
            agent.ctx.shell_env.local().await;
            let mut g = agent.inner.lock().unwrap();
            g.awaiting_shell = false;
            if g.closing {
                return;
            }
            agent.spawn(&mut g);
            drop(g);
            agent.changed();
        });
    }

    /// Put on the Fountain agent it wears (M44), then start it; or say why
    /// it can't be.
    fn wear(&self, inner: &mut Inner, take_over: bool) {
        if inner.wearing {
            return;
        }
        inner.wearing = true;
        inner.status = Status::Starting;
        inner.error = None;
        let def = inner.cfg.def.clone();
        let agent = Agent { ctx: self.ctx.clone(), inner: self.inner.clone(), tx: self.tx.clone() };
        self.ctx.rt.spawn(async move {
            let which = def.as_fountain.clone().unwrap_or_default();
            let worn = match Runner::user(&agent.ctx).await {
                Ok(runner) => {
                    let (profile, specs, vault) = (def.profile.as_deref(), def.specs.as_deref(), def.vault.as_deref());
                    crate::fountain::wear::wear(&runner, profile, specs, vault, &which).await
                }
                Err(e) => Err(e),
            };
            let mut g = agent.inner.lock().unwrap();
            g.wearing = false;
            if g.closing {
                return;
            }
            match worn {
                Ok(w) => {
                    g.worn = Some(Arc::new(w));
                    if take_over {
                        agent.take_over(&mut g);
                    } else {
                        agent.spawn(&mut g);
                    }
                }
                Err(e) => {
                    // One left running from before can't be followed unworn.
                    if take_over && let Some(pid) = link::alive_pid(&agent.ctx.dir) {
                        link::kill_group(pid, agent.ctx.dir.clone());
                    }
                    agent.failed(&mut g, format!("can't wear {which}: {e}"))
                }
            }
            drop(g);
            agent.changed();
        });
    }

    /// It couldn't start: said once, as "the agent couldn't start: …".
    fn failed(&self, inner: &mut Inner, why: String) {
        self.failed_with(inner, why, None)
    }

    fn failed_with(&self, inner: &mut Inner, why: String, adapter: Option<Value>) {
        warn!(block = self.ctx.id, why, "agent failed to start");
        inner.note(json!({ "e": "exit", "why": format!("couldn't start: {why}"), "adapter": adapter }));
    }
}

/// The credentials an agent in a VM gets in its environment: an Anthropic
/// API key if there's one, else a Claude Code token.
fn secret_env(ctx: &BlockCtx) -> Result<Vec<(String, String)>, String> {
    let read = |p: &Path| std::fs::read_to_string(p).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    if let Some(k) = read(&ctx.secrets.anthropic_key) {
        return Ok(vec![("ANTHROPIC_API_KEY".into(), k)]);
    }
    if let Some(t) = read(&ctx.secrets.claude_token) {
        return Ok(vec![("CLAUDE_CODE_OAUTH_TOKEN".into(), t)]);
    }
    Err(format!(
        "Claude Code in a VM needs credentials: put a token from `claude setup-token` in {} (or an API key in {})",
        ctx.secrets.claude_token.display(),
        ctx.secrets.anthropic_key.display()
    ))
}

/// npm adapters are Node scripts: make sure a Node is on PATH even when the
/// daemon runs as a service with a bare one. mise's shims won't do: outside
/// a directory that pins Node they refuse to pick a version. So: a real
/// `node` already on PATH, else one mise installed (22, the version the
/// adapters were tested with, else the newest), put first.
fn with_node_on_path(env: &mut Vec<(String, String)>, home: &Path) {
    let path = env
        .iter()
        .rev()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let shims = home.join(".local/share/mise/shims");
    let real =
        path.split(':').filter(|d| !d.is_empty() && Path::new(d) != shims).any(|d| Path::new(d).join("node").is_file());
    if real {
        return;
    }
    let Some(bin) = mise_node(&home.join(".local/share/mise/installs/node")) else { return };
    env.retain(|(k, _)| k != "PATH");
    env.push(("PATH".into(), format!("{}:{path}", bin.display())));
}

/// The `bin` of a Node that mise installed: 22's if there, else the newest.
fn mise_node(installs: &Path) -> Option<std::path::PathBuf> {
    let mut versions: Vec<(Vec<u32>, std::path::PathBuf)> = std::fs::read_dir(installs)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let v: Vec<u32> = name.split('.').map(|p| p.parse().ok()).collect::<Option<_>>()?;
            let bin = e.path().join("bin");
            (v.len() == 3 && bin.join("node").is_file()).then_some((v, bin))
        })
        .collect();
    versions.sort();
    versions.iter().rev().find(|(v, _)| v[0] == 22).or_else(|| versions.last()).map(|(_, b)| b.clone())
}

/// The block's task: frames from the agent server, and publishing.
async fn run(
    ctx: BlockCtx,
    inner: Arc<Mutex<Inner>>,
    tx: mpsc::UnboundedSender<Msg>,
    mut rx: mpsc::UnboundedReceiver<Msg>,
) {
    let mut tick = tokio::time::interval(PUBLISH_EVERY);
    let mut dirty = true;
    let mut remote_check: Option<tokio::time::Instant> = None;
    let mut machine_up = false;
    let (mut retry_at, mut retries): (Option<tokio::time::Instant>, u32) = (None, 0);
    let mut follow_at = tokio::time::Instant::now() + FOLLOW_EVERY;
    // What it was before a restart, for the "needs you" list.
    publish(&ctx, &inner, true);
    loop {
        tokio::select! {
            m = rx.recv() => {
                let Some(m) = m else { return };
                let mut fx = vec![];
                {
                    let mut g = inner.lock().unwrap();
                    if g.closing {
                        return;
                    }
                    match m {
                        Msg::Frame(generation, v) if generation == g.generation => {
                            fx = g.on_in(&v, now_ms());
                            // Hearing from it means its machine is up.
                            if ctx.sprite.is_some() && !machine_up {
                                machine_up = true;
                                ctx.machine(true);
                            }
                        }
                        Msg::Closed(generation, why) if generation == g.generation => {
                            g.link = None;
                            g.pid = None;
                            info!(block = ctx.id, why, "agent server stopped");
                            g.note(json!({ "e": "exit", "why": why }));
                            if ctx.sprite.is_some() && why.contains("machine is gone") {
                                machine_up = false;
                                ctx.machine(false);
                            }
                        }
                        _ => {}
                    }
                    for f in fx.drain(..) {
                        if let Effect::Retry(q) = &f {
                            retries += 1;
                            if retries > MAX_RETRIES {
                                g.note(json!({ "e": "error", "message": "The agent kept asking to retry; send it again later" }));
                            } else {
                                g.enqueue(q, true);
                                retry_at = Some(tokio::time::Instant::now() + RETRY_AFTER);
                            }
                            continue;
                        }
                        if f == Effect::TurnEnded {
                            retries = 0;
                        }
                        if f == Effect::CheckRemote {
                            if g.interrupted {
                                g.note(json!({ "e": "remote" }));
                                remote_check = Some(tokio::time::Instant::now() + REMOTE_POLL);
                            }
                            continue;
                        }
                        act(&ctx, &mut g, f);
                    }
                }
                dirty = true;
                // A burst of frames: draw once it settles.
                if rx.is_empty() {
                    publish(&ctx, &inner, false);
                }
            }
            _ = tick.tick() => {
                if dirty {
                    dirty = false;
                    publish(&ctx, &inner, false);
                }
                if tokio::time::Instant::now() >= follow_at {
                    follow_at = tokio::time::Instant::now() + FOLLOW_EVERY;
                    let mut g = inner.lock().unwrap();
                    if g.cfg.import.is_some() && !g.frozen && refresh_import(&ctx, &mut g) {
                        dirty = true;
                    }
                }
                if retry_at.is_some_and(|t| tokio::time::Instant::now() >= t) {
                    retry_at = None;
                    send_next(&ctx, &mut inner.lock().unwrap());
                    dirty = true;
                }
                if remote_check.is_some_and(|t| tokio::time::Instant::now() >= t) {
                    remote_check = None;
                    let (session, def) = {
                        let g = inner.lock().unwrap();
                        (g.cfg.session_id.clone(), g.cfg.def.clone())
                    };
                    if let Some(sid) = session {
                        let busy = fountain_busy(&def, &sid).await;
                        let mut g = inner.lock().unwrap();
                        if busy == Some(false) && g.status == Status::Remote {
                            g.note(json!({ "e": "remote_done" }));
                            g.interrupted = false;
                            let session = g.session();
                            let cwd = g.cfg.cwd.clone().unwrap_or_else(|| "/".into());
                            let mcp = g.servers(&ctx);
                            g.request("session/load", json!({ "sessionId": session, "cwd": cwd, "mcpServers": mcp }));
                        } else if g.status == Status::Remote {
                            remote_check = Some(tokio::time::Instant::now() + REMOTE_POLL);
                        }
                        drop(g);
                        let _ = tx.send(Msg::Changed);
                    }
                }
            }
        }
    }
}

/// Whether a Fountain conversation is still running a turn.
async fn fountain_busy(def: &Def, session: &str) -> Option<bool> {
    let mut cmd = tokio::process::Command::new(def.launch(Path::new("/"), false).ok()?.argv.first()?.clone());
    if let Some(p) = &def.profile {
        cmd.args(["--profile", p]);
    }
    let out = cmd.args(["conv", "show", session]).output().await.ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let status = text.lines().find_map(|l| l.trim().strip_prefix("status:"))?.trim().to_owned();
    Some(matches!(status.as_str(), "running" | "pending" | "busy" | "active"))
}

/// An opened conversation, read again if its transcript changed; and who
/// holds it now. True if anything changed.
fn refresh_import(ctx: &BlockCtx, g: &mut Inner) -> bool {
    let Some(imp) = g.cfg.import.clone() else { return false };
    let held = g.cfg.session_id.as_deref().and_then(|sid| {
        crate::conversations::Index::global().lock().unwrap().live_for(sid).filter(|l| l.block != Some(ctx.id))
    });
    // A scope can name another daemon's pane (#77).
    let held = held.map(|l| l.ours(|p| ctx.ours(p)));
    let mut changed = held != g.held;
    if changed {
        // Whether a tool call without a result was cut off depends on it.
        g.import_stamp = None;
    }
    g.held = held;
    let stamp = std::fs::metadata(&imp.path).ok().and_then(|m| Some((m.len(), m.modified().ok()?)));
    if stamp.is_some() && stamp != g.import_stamp {
        // Only what was appended since (#80); a held change rereads none.
        if let Ok(new) = g.follow.read(Path::new(&imp.path)) {
            if new || g.import_stamp.is_none() {
                // Its prompts' images, kept for clients to show (M71).
                for bytes in g.follow.take_images() {
                    if let Err(e) = images::keep(&ctx.dir, &bytes) {
                        warn!(block = ctx.id, error = %e, "can't keep an imported image");
                    }
                }
                g.t = Transcript::from_entries(g.follow.entries(g.held.is_none()));
                changed = true;
            }
            g.import_stamp = stamp;
        }
    } else if stamp.is_none() && g.t.entries.is_empty() {
        g.t.note(format!("Its transcript ({}) is gone", imp.path), now_ms());
        changed = true;
    }
    changed
}

fn publish(ctx: &BlockCtx, inner: &Arc<Mutex<Inner>>, first: bool) {
    let mut g = inner.lock().unwrap();
    let (a, why) = g.attention();
    if g.reported != Some(a) {
        g.reported = Some(a);
        // Coming back after a restart, a finished turn isn't news.
        let a = if first && a == Attention::Done { Attention::Idle } else { a };
        ctx.attention(a, why);
    }
    drop(g);
    ctx.changed();
}

/// Act on what a frame asked for.
fn act(ctx: &BlockCtx, g: &mut Inner, f: Effect) {
    match f {
        Effect::OpenSession => {
            let cwd = g.cfg.cwd.clone().unwrap_or_else(|| match ctx.sprite {
                Some(_) => "/home/sprite".into(),
                None => ctx.home.display().to_string(),
            });
            let resume = g.caps["sessionCapabilities"]["resume"].is_object();
            let load = g.caps["loadSession"].as_bool() == Some(true);
            let fork = g.caps["sessionCapabilities"]["fork"].is_object();
            let with_meta = |mut p: Value, g: &Inner| {
                // #127: an ordinary block's too, so a reopened Claude
                // session keeps `settingSources: []`.
                let m = imported_meta(g).or_else(|| worn_meta(ctx, g)).or_else(|| launch_meta(ctx, g));
                if let Some(m) = m {
                    p["_meta"] = m;
                }
                p
            };
            match &g.cfg.session_id {
                Some(sid) if g.cfg.fork && fork => {
                    let sid = sid.clone();
                    let mcp = g.servers(ctx);
                    let p = with_meta(json!({ "sessionId": sid, "cwd": cwd, "mcpServers": mcp }), g);
                    g.request("session/fork", p);
                }
                Some(_) if g.cfg.fork => {
                    g.cfg.fork = false;
                    g.note(json!({ "e": "error", "message": "This agent can't fork sessions" }));
                }
                Some(sid) if resume && !g.t.entries.is_empty() => {
                    let sid = sid.clone();
                    let mcp = g.servers(ctx);
                    let p = with_meta(json!({ "sessionId": sid, "cwd": cwd, "mcpServers": mcp }), g);
                    g.request("session/resume", p);
                }
                Some(sid) if load => {
                    let sid = sid.clone();
                    let mcp = g.servers(ctx);
                    let p = with_meta(json!({ "sessionId": sid, "cwd": cwd, "mcpServers": mcp }), g);
                    g.request("session/load", p);
                }
                _ if g.cfg.follow => {
                    let why = if g.cfg.session_id.is_none() {
                        "it has no conversation to follow".to_owned()
                    } else {
                        "this agent can't load a conversation (no loadSession)".to_owned()
                    };
                    follow_failed(g, &why);
                }
                _ => new_session(ctx, g, &cwd),
            }
        }
        Effect::SessionLost(why) if g.cfg.follow => {
            follow_failed(g, &format!("couldn't load the conversation ({why})"));
        }
        Effect::SessionLost(why) => {
            g.t.note(format!("Couldn't reopen the session ({why}); starting a new one"), now_ms());
            g.cfg.session_id = None;
            let cwd = g.cfg.cwd.clone().unwrap_or_else(|| ctx.home.display().to_string());
            new_session(ctx, g, &cwd);
        }
        Effect::SessionOpen { fresh: _ } => {
            // An imported conversation goes on with the model it had (a
            // resume resets it to the adapter's default, S20).
            let model = g.cfg.def.model.clone().or_else(|| {
                let want = g.cfg.import.as_ref()?.model.clone()?;
                crate::conversations::model_option(&g.config_options, &want)
            });
            if let Some(model) = model {
                let session = g.session();
                g.request(
                    "session/set_config_option",
                    json!({ "sessionId": session, "configId": "model", "value": model }),
                );
            }
            // #163: after the model, which decides whether `auto` is there.
            // Again on each reopen: a resumed session starts in its default.
            if let Some(mode) = g.cfg.def.permission_mode.clone() {
                let session = g.session();
                g.request("session/set_mode", json!({ "sessionId": session, "modeId": mode }));
            }
            if g.status == Status::Starting {
                g.status = Status::Ready;
            }
            send_next(ctx, g);
        }
        Effect::TurnEnded => {
            // In history as a command of its own: the prompt, and whether
            // the turn finished.
            if let Some(t) = g.turns.last().cloned() {
                let cwd = g.cfg.cwd.clone();
                let label = format!("{}: {}", g.cfg.def.label(), t.prompt.lines().next().unwrap_or(""));
                if let Some(log) = g.log.as_mut() {
                    let at = log.end();
                    let exit = Some(if t.stop.as_deref() == Some("end_turn") { 0 } else { 1 });
                    let _ = log.record(at, Event::Command { at_ms: t.started_ms, text: Some(label), cwd, by: None });
                    let _ = log.record(at, Event::End { at_ms: t.ended_ms.unwrap_or(t.started_ms), exit });
                }
            }
            send_next(ctx, g)
        }
        Effect::Permission(key) => {
            let Some(p) = g.pending.iter().find(|p| p.id == key).cloned() else { return };
            let allowed =
                g.cfg.allow.iter().any(|r| r.tool == p.tool && r.title.as_ref().is_none_or(|t| *t == p.title));
            // #166: then the daemon's standing rules, as they are now.
            let standing = (!allowed)
                .then(|| {
                    let what = p.command.as_deref().unwrap_or(&p.title);
                    ctx.rules.allowing(&p.tool, what, g.cfg.cwd.as_deref(), ctx.sprite.as_deref())
                })
                .flatten();
            if (allowed || standing.is_some())
                && let Some(opt) = p.options.iter().find(|o| o.kind == "allow_once")
            {
                match standing {
                    Some(r) => {
                        g.note(json!({ "e": "approve", "title": p.title, "how": "standing", "rule": r.describe() }))
                    }
                    None => g.note(json!({ "e": "approve", "title": p.title, "how": "rule" })),
                }
                let opt = opt.id.clone();
                g.out(json!({ "jsonrpc": "2.0", "id": p.rpc, "result": { "outcome": { "outcome": "selected", "optionId": opt } } }));
            }
        }
        Effect::Unsupported(id, method) => {
            g.out(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": format!("{method} isn't supported") } }));
        }
        Effect::ToolFinished(id) => {
            let Some(t) = g.t.tool(&id).cloned() else { return };
            // Its question and answer are in history from `answer`.
            if t.is_question() {
                return;
            }
            let cwd = g.cfg.cwd.clone();
            if let Some(log) = g.log.as_mut() {
                let at = log.end();
                let exit = t.exit.or(Some(if t.status == "completed" { 0 } else { 1 }));
                let _ = log.record(at, Event::Command { at_ms: t.started_ms, text: Some(t.label()), cwd, by: None });
                let _ = log.record(at, Event::End { at_ms: t.ended_ms.unwrap_or(t.started_ms), exit });
            }
        }
        Effect::CheckRemote | Effect::Retry(_) => {}
    }
}

/// A frame as the log keeps it: without illogical's MCP token (M16), and
/// (M44) without a worn Fountain agent's secrets: its servers' headers and
/// env, and any of `secrets` anywhere else.
/// A prompt as the log keeps it: an image we kept (M71) by its name in the
/// block's folder, not its data.
fn without_images<'a>(frame: &'a Value) -> std::borrow::Cow<'a, Value> {
    use std::borrow::Cow;
    let kept = |c: &Value| c["_meta"][images::META].is_string() && c.get("data").is_some();
    if !frame["params"]["prompt"].as_array().is_some_and(|p| p.iter().any(kept)) {
        return Cow::Borrowed(frame);
    }
    let mut f = frame.clone();
    for c in f["params"]["prompt"].as_array_mut().into_iter().flatten().filter(|c| kept(c)) {
        if let Some(o) = c.as_object_mut() {
            o.remove("data");
        }
    }
    Cow::Owned(f)
}

fn redacted<'a>(frame: &'a Value, secrets: &[String]) -> std::borrow::Cow<'a, Value> {
    use std::borrow::Cow;
    let ours = |s: &Value| s["name"] == crate::mcp::SERVER_NAME;
    let servers = frame["params"]["mcpServers"].as_array();
    if !servers.is_some_and(|l| l.iter().any(ours)) && secrets.is_empty() {
        return Cow::Borrowed(frame);
    }
    let mut f = frame.clone();
    for s in f["params"]["mcpServers"].as_array_mut().into_iter().flatten().filter(|s| ours(s) || !secrets.is_empty()) {
        for key in ["headers", "env"] {
            for kv in s.get_mut(key).and_then(Value::as_array_mut).into_iter().flatten() {
                kv["value"] = json!("<redacted>");
            }
        }
    }
    if !secrets.is_empty()
        && let Some(text) = crate::fountain::wear::scrub(&f.to_string(), secrets)
    {
        f = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "redacted": true, "method": frame["method"] }));
    }
    Cow::Owned(f)
}

/// A worn Fountain agent's `_meta` (M44): Claude's, dressed as the agent.
fn worn_meta(ctx: &BlockCtx, g: &Inner) -> Option<Value> {
    let w = g.worn.as_ref()?;
    let mut meta = g.cfg.def.launch(&ctx.home, ctx.sprite.is_some()).map(|l| l.meta).unwrap_or_default();
    w.dress(&mut meta);
    Some(meta)
}

/// An imported conversation's `_meta` (M33): your settings, skills and
/// `CLAUDE.md`, as it had where it started, with every hook off.
fn imported_meta(g: &Inner) -> Option<Value> {
    (g.cfg.import.is_some() && g.cfg.def.agent == Kind::Claude).then(defs::user_settings_meta)
}

/// A followed conversation that can't be loaded: the agent stops, saying
/// why, and nothing new is started.
fn follow_failed(g: &mut Inner, why: &str) {
    warn!(why, "following a conversation");
    if let Some(l) = g.link.take() {
        l.stop();
    }
    // Its process's own exit (the signal) isn't news: this is why it stopped.
    g.generation += 1;
    g.note(json!({ "e": "exit", "why": format!("stopped: {why}; Follow never starts a new conversation") }));
}

/// The agent's own `_meta` (Claude's `settingSources: []`), if it has one.
fn launch_meta(ctx: &BlockCtx, g: &Inner) -> Option<Value> {
    let m = g.cfg.def.launch(&ctx.home, ctx.sprite.is_some()).ok()?.meta;
    m.as_object().is_some_and(|o| !o.is_empty()).then_some(m)
}

/// The adapter's environment variable that holds illogical's MCP token
/// (#128).
pub const MCP_TOKEN_ENV: &str = "ILLOGICAL_MCP_BLOCK_TOKEN";

/// Whether this block's MCP credentials go by reference (#128, M44): a
/// local Claude Code (its own adapter or another command), which expands
/// `${…}` in its MCP config itself. Codex and other ACP agents aren't
/// known to, so they get values.
fn by_reference(def: &Def, ctx: &BlockCtx) -> bool {
    def.agent == Kind::Claude && ctx.sprite.is_none()
}

/// In the block's directory while its agent server has the token in its
/// environment (#128): one started by an older daemon hasn't, and is
/// started again when taken over.
const TOKEN_IN_ENV: &str = "mcp-token-env";

fn new_session(ctx: &BlockCtx, g: &mut Inner, cwd: &str) {
    // A worn agent never opens a plain session (M44).
    if g.cfg.def.as_fountain.is_some() && g.worn.is_none() {
        g.note(json!({ "e": "error", "message": "The Fountain agent isn't worn yet: no session opened" }));
        return;
    }
    let meta = imported_meta(g)
        .or_else(|| worn_meta(ctx, g))
        .unwrap_or_else(|| g.cfg.def.launch(&ctx.home, ctx.sprite.is_some()).map(|l| l.meta).unwrap_or_default());
    let mcp = g.servers(ctx);
    g.request("session/new", json!({ "cwd": cwd, "mcpServers": mcp, "_meta": meta }));
}

/// Send the next queued prompt, if the agent can take it.
fn send_next(ctx: &BlockCtx, g: &mut Inner) {
    if g.status != Status::Ready || g.prompt_id.is_some() || g.cfg.session_id.is_none() {
        return;
    }
    let Some(q) = g.queue.front().cloned() else { return };
    let session = g.session();
    let mut prompt = vec![];
    if !q.text.is_empty() {
        prompt.push(json!({ "type": "text", "text": q.text }));
    }
    // M71: images as image blocks to an agent that takes them; to one that
    // doesn't, their paths (on this host) for it to read.
    let takes = g.caps["promptCapabilities"]["image"] == true;
    for name in &q.images {
        let block = match images::block(&ctx.dir, name) {
            Ok(b) if takes => b,
            Ok(_) => json!({
                "type": "text",
                "text": images::path(&ctx.dir, name).map(|p| p.display().to_string()).unwrap_or_default(),
                "_meta": { images::META: name },
            }),
            Err(e) => {
                warn!(block = ctx.id, image = %name, error = %e, "can't read a queued image");
                json!({ "type": "text", "text": format!("[image {name}: gone]"), "_meta": { images::META: name } })
            }
        };
        prompt.push(block);
    }
    let mut params = json!({ "sessionId": session, "prompt": prompt });
    if g.cfg.def.agent == Kind::Fountain {
        params["_meta"] = json!({ "clientRequestId": format!("illogical-{}-{}", ctx.id, g.next_id) });
    }
    g.request("session/prompt", params);
}

impl Agent {
    fn changed(&self) {
        let _ = self.tx.send(Msg::Changed);
    }

    fn find(g: &Inner, id: &Value) -> Result<Perm, String> {
        let key = match id {
            Value::Null => return g.pending.first().cloned().ok_or_else(|| "nothing is waiting for approval".into()),
            v => rpc_key(v),
        };
        g.pending
            .iter()
            .find(|p| p.id == key || p.tool_call_id == key)
            .cloned()
            .ok_or_else(|| format!("no open request {key} (it was answered, or its tool call ended)"))
    }

    fn approve(&self, args: &Value, by: Option<&str>) -> Result<Value, String> {
        let mut g = self.inner.lock().unwrap();
        let p = Self::find(&g, &args["id"])?;
        let option = args["option"].as_str().unwrap_or("once");
        let chosen = match option {
            "once" | "always" | "always_tool" => p.options.iter().find(|o| o.kind == "allow_once"),
            id => p.options.iter().find(|o| o.id == id),
        }
        .ok_or_else(|| {
            format!(
                "{option}: not an option here ({})",
                p.options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>().join(", ")
            )
        })?
        .clone();
        if chosen.kind == "allow_always" {
            // It would write a rule into the agent's repo (S7); ours is in
            // the block instead.
            return Err("use option \"always\": the block remembers it, not the agent".into());
        }
        // #166: "always" for this block (its config), or a standing rule
        // the daemon keeps for this directory or every block. Those are the
        // owner's to make, and they allow the whole tool unless given a
        // prefix.
        let scope = args["scope"].as_str().unwrap_or("block");
        let standing = match scope {
            "block" => None,
            "cwd" | "everywhere" if !option.starts_with("always") => {
                return Err(format!("scope {scope} goes with option \"always\""));
            }
            "cwd" | "everywhere" if by.is_some() => {
                return Err("only the owner makes standing rules".into());
            }
            "cwd" | "everywhere" => {
                let cwd = match scope {
                    "cwd" => Some(g.cfg.cwd.clone().ok_or("this block has no directory: use scope everywhere")?),
                    _ => None,
                };
                let prefix = args["prefix"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
                Some(crate::rules::Standing {
                    tool: p.tool.clone(),
                    prefix,
                    sprite: cwd.as_ref().and(self.ctx.sprite.clone()),
                    cwd,
                    at_ms: 0,
                    from: Some(p.title.clone()),
                })
            }
            s => return Err(format!("scope {s}: block, cwd or everywhere")),
        };
        if let Some(r) = &standing {
            self.ctx.rules.add(r.clone()).map_err(|e| format!("couldn't keep the rule: {e}"))?;
        } else if option.starts_with("always") {
            let rule = Rule { tool: p.tool.clone(), title: (option == "always").then(|| p.title.clone()) };
            if !g.cfg.allow.contains(&rule) {
                g.cfg.allow.push(rule);
            }
        }
        match &standing {
            Some(r) => g.note(
                json!({ "e": "approve", "title": p.title, "how": "standing-new", "rule": r.describe(), "by": by }),
            ),
            None => {
                let how = if option.starts_with("always") { "always" } else { "once" };
                g.note(json!({ "e": "approve", "title": p.title, "how": how, "by": by }));
            }
        }
        g.out(json!({ "jsonrpc": "2.0", "id": p.rpc, "result": { "outcome": { "outcome": "selected", "optionId": chosen.id } } }));
        drop(g);
        self.changed();
        Ok(json!({ "approved": p.id, "option": chosen.id }))
    }

    fn deny(&self, args: &Value, by: Option<&str>) -> Result<Value, String> {
        let mut g = self.inner.lock().unwrap();
        let p = Self::find(&g, &args["id"])?;
        let reason = args["reason"].as_str().unwrap_or("");
        let reject = p
            .options
            .iter()
            .find(|o| o.kind == "reject_once")
            .or(p.options.iter().find(|o| o.kind.starts_with("reject")));
        let outcome = match reject {
            Some(o) => json!({ "outcome": "selected", "optionId": o.id }),
            None => json!({ "outcome": "cancelled" }),
        };
        g.note(json!({ "e": "deny", "title": p.title, "reason": reason, "by": by }));
        g.out(json!({ "jsonrpc": "2.0", "id": p.rpc, "result": { "outcome": outcome } }));
        drop(g);
        self.changed();
        Ok(json!({ "denied": p.id }))
    }

    fn find_ask(g: &Inner, id: &Value) -> Result<Elicit, String> {
        match id {
            Value::Null => g.open_ask().cloned().ok_or_else(|| "no question is waiting for an answer".into()),
            v => {
                let key = rpc_key(v);
                g.asks
                    .iter()
                    .find(|a| a.ask.id == key || a.ask.tool_call_id.as_deref() == Some(key.as_str()))
                    .cloned()
                    .ok_or_else(|| format!("no open question {key} (it was answered, or withdrawn)"))
            }
        }
    }

    /// Record a question and its answer in history (live only).
    fn ask_history(g: &mut Inner, a: &Ask, text: String, exit: i32, by: Option<&str>) {
        let cwd = g.cfg.cwd.clone();
        if let Some(log) = g.log.as_mut() {
            let at = log.end();
            let _ = log.record(at, Event::Command { at_ms: a.at_ms, text: Some(text), cwd, by: by.map(str::to_owned) });
            let _ = log.record(at, Event::End { at_ms: now_ms(), exit: Some(exit) });
        }
    }

    /// `answer {id?, content}`: submit a question card or form (`content` is
    /// its fields; without a `content` key, the arguments are), or open a
    /// link.
    fn answer(&self, args: &Value, by: Option<&str>) -> Result<Value, String> {
        let mut g = self.inner.lock().unwrap();
        let e = Self::find_ask(&g, &args["id"])?;
        if e.ask.accepted {
            return Err("that link was already opened; it closes when the agent says it's done".into());
        }
        let content = match args.get("content") {
            Some(c) if c.is_object() => c.clone(),
            _ => {
                let mut c = args.as_object().cloned().unwrap_or_default();
                c.remove("id");
                Value::Object(c)
            }
        };
        let summary = ask::summary(&e.ask, &content);
        g.note(json!({ "e": "answered", "id": e.ask.id, "summary": summary, "by": by }));
        let result = match e.ask.kind {
            AskKind::Url => json!({ "action": "accept" }),
            _ => json!({ "action": "accept", "content": content }),
        };
        g.out(json!({ "jsonrpc": "2.0", "id": e.rpc, "result": result }));
        Self::ask_history(&mut g, &e.ask, format!("{} → {summary}", e.ask.headline()), 0, by);
        drop(g);
        self.changed();
        Ok(json!({ "answered": e.ask.id }))
    }

    /// `decline {id?}`: skip a question or form; for a link already opened,
    /// just close its card.
    fn decline(&self, args: &Value, by: Option<&str>) -> Result<Value, String> {
        let mut g = self.inner.lock().unwrap();
        let e = Self::find_ask(&g, &args["id"])?;
        if e.ask.accepted {
            g.note(json!({ "e": "dismissed", "id": e.ask.id }));
        } else {
            g.note(json!({ "e": "skipped", "id": e.ask.id, "question": e.ask.headline(), "by": by }));
            g.out(json!({ "jsonrpc": "2.0", "id": e.rpc, "result": { "action": "decline" } }));
            Self::ask_history(&mut g, &e.ask, format!("{} → skipped", e.ask.headline()), 1, by);
        }
        drop(g);
        self.changed();
        Ok(json!({ "declined": e.ask.id }))
    }

    fn cancel(&self) -> Result<Value, String> {
        let mut g = self.inner.lock().unwrap();
        g.note(json!({ "e": "queue_clear" }));
        // Open questions aren't answered: the agent withdraws them itself
        // (`$/cancel_request`), and answering `cancel` would only fail the
        // tool and let the turn carry on (S13).
        let open: Vec<Value> = g.pending.iter().map(|p| p.rpc.clone()).collect();
        if g.prompt_id.is_none() && open.is_empty() {
            drop(g);
            self.changed();
            return Ok(json!({ "cancelled": false }));
        }
        let session = g.session();
        g.notify("session/cancel", json!({ "sessionId": session }));
        // The spec: open requests are answered `cancelled`.
        for rpc in open {
            g.out(json!({ "jsonrpc": "2.0", "id": rpc, "result": { "outcome": { "outcome": "cancelled" } } }));
        }
        drop(g);
        self.changed();
        Ok(json!({ "cancelled": true }))
    }

    /// `send {text, files?}`: the next prompt. `files` (M71) are paths M70's
    /// upload route answered for this block.
    fn send(&self, args: &Value, by: Option<&str>) -> Result<Value, String> {
        let text = args["text"].as_str().map(str::trim).unwrap_or_default();
        let files = strings(&args["files"]);
        if text.is_empty() && files.is_empty() {
            return Err("send needs {\"text\": …}".into());
        }
        let mut q = Queued { text: text.to_owned(), images: vec![] };
        attach(&self.ctx, &mut q, &files)?;
        let mut g = self.inner.lock().unwrap();
        if let Some(by) = by {
            // A follow-up from someone (M29): the transcript says whose.
            g.note(json!({ "e": "from", "by": by }));
        }
        if matches!(g.status, Status::Exited | Status::Stopped) {
            self.continue_import(&mut g)?;
        }
        g.enqueue(&q, false);
        g.error = None;
        match g.status {
            Status::Exited | Status::Stopped => self.spawn(&mut g),
            _ => send_next(&self.ctx, &mut g),
        }
        let answer = json!({ "status": g.status, "queued": g.queue.len() });
        drop(g);
        self.changed();
        Ok(answer)
    }

    /// An opened conversation, about to run here (M33): refused while
    /// another process holds it (two writers lose one side's turns, S20
    /// Q5), else what it had is frozen into the block.
    fn continue_import(&self, g: &mut Inner) -> Result<(), String> {
        if g.cfg.import.is_none() || g.frozen {
            return Ok(());
        }
        refresh_import(&self.ctx, g);
        if let Some(l) = &g.held {
            return Err(format!("it's {}: fork it instead, or continue once that's closed", l.place()));
        }
        self.freeze(g)
    }

    fn freeze(&self, g: &mut Inner) -> Result<(), String> {
        if g.cfg.import.is_none() || g.frozen {
            return Ok(());
        }
        // A desktop session's scratch folder goes with it: make it again.
        if let Some(cwd) = &g.cfg.cwd
            && !Path::new(cwd).is_dir()
        {
            std::fs::create_dir_all(cwd).map_err(|e| format!("can't make {cwd}: {e}"))?;
        }
        let file = self.ctx.dir.join("imported.json");
        let bytes = serde_json::to_vec(&g.t.entries).map_err(|e| e.to_string())?;
        crate::store::write_atomic(&file, &bytes).map_err(|e| format!("can't keep the transcript: {e}"))?;
        g.note(json!({ "e": "imported", "file": file.display().to_string() }));
        Ok(())
    }

    /// `fork`: a new session with this one's history, then on in that
    /// (M33). The original is left as it was, whoever holds it.
    fn fork(&self) -> Result<Value, String> {
        let mut g = self.inner.lock().unwrap();
        if g.link.is_some() && !matches!(g.status, Status::Exited | Status::Stopped) {
            return Err("it's running: fork it once it's stopped".into());
        }
        if g.cfg.session_id.is_none() {
            return Err("there's no session to fork yet".into());
        }
        if g.cfg.import.is_some() && !g.frozen {
            refresh_import(&self.ctx, &mut g);
            self.freeze(&mut g)?;
        }
        g.cfg.fork = true;
        self.spawn(&mut g);
        drop(g);
        self.changed();
        Ok(json!({}))
    }

    fn start(&self) -> Result<Value, String> {
        let mut g = self.inner.lock().unwrap();
        if g.link.is_some() && !matches!(g.status, Status::Exited | Status::Stopped) {
            return Err("it's already running".into());
        }
        self.continue_import(&mut g)?;
        self.spawn(&mut g);
        drop(g);
        self.changed();
        Ok(json!({}))
    }
}

impl Block for Agent {
    fn kind(&self) -> BlockType {
        BlockType::Agent
    }

    fn config(&self) -> Value {
        serde_json::to_value(&self.inner.lock().unwrap().cfg).unwrap_or_default()
    }

    fn state(&self) -> Value {
        self.inner.lock().unwrap().state(&self.ctx)
    }

    fn text(&self) -> String {
        let g = self.inner.lock().unwrap();
        let mut head = format!("# {}", g.title.clone().unwrap_or_else(|| g.cfg.def.label()));
        if let Some(cwd) = &g.cfg.cwd {
            head.push_str(&format!(" in {cwd}"));
        }
        if let Some(w) = &g.worn {
            head.push_str(&format!("\n\n_{}_", w.info.text()));
        }
        let mut out = format!("{head}\n\n{}", g.t.markdown());
        for p in &g.pending {
            out.push_str(&format!("**Waiting for approval:** `{}` (`approve {}`)\n\n", p.title, p.id));
        }
        for a in g.asks.iter().filter(|a| !a.ask.accepted) {
            out.push_str(&format!("**Waiting for your answer:** {} (`answer {}`)\n\n", a.ask.headline(), a.ask.id));
        }
        out
    }

    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        self.call_by(method, args, None)
    }

    fn call_by(&self, method: &str, args: Value, by: Option<&str>) -> BoxFuture<'static, Result<Value, String>> {
        let result = match method {
            "send" => self.send(&args, by),
            "approve" => self.approve(&args, by),
            "deny" => self.deny(&args, by),
            "answer" => self.answer(&args, by),
            "decline" => self.decline(&args, by),
            "cancel" => self.cancel(),
            "start" | "resume" | "continue" => self.start(),
            "fork" => self.fork(),
            "forget" => {
                let mut g = self.inner.lock().unwrap();
                let before = g.cfg.allow.len();
                match args["index"].as_u64() {
                    Some(i) if (i as usize) < before => {
                        g.cfg.allow.remove(i as usize);
                    }
                    Some(_) => return Box::pin(async { Err("no such rule".into()) }),
                    None => g.cfg.allow.clear(),
                }
                drop(g);
                self.changed();
                Ok(json!({}))
            }
            "state" => Ok(self.state()),
            "image" => images::read(&self.ctx.dir, args["name"].as_str().unwrap_or_default()),
            m => Err(no_method(BlockType::Agent, m)),
        };
        Box::pin(async move { result })
    }

    fn close(&self) {
        let mut g = self.inner.lock().unwrap();
        g.closing = true;
        if let Some(l) = g.link.take() {
            l.stop();
        }
        if let Some(r) = g.relay.take() {
            r.stop();
        }
        if self.ctx.sprite.is_none() {
            link::forget_fds(self.ctx.id);
        }
        g.note(json!({ "e": "exit", "why": "closed", "closing": true }));
    }

    fn push_extra(&self) -> Option<Value> {
        let g = self.inner.lock().unwrap();
        if let Some(p) = g.pending.first() {
            return Some(json!({ "approve": { "id": p.id, "title": p.title } }));
        }
        let choice = g.open_ask()?.ask.push_choice()?;
        Some(json!({ "ask": choice }))
    }

    fn pid(&self) -> Option<u32> {
        self.inner.lock().unwrap().pid
    }

    fn waiting(&self) -> Option<crate::block::Waiting> {
        use illogical_proto::AskWhat;
        let g = self.inner.lock().unwrap();
        let agent = serde_json::to_value(g.cfg.def.agent).ok().and_then(|v| v.as_str().map(str::to_owned));
        let agent = agent.unwrap_or_else(|| "agent".into());
        let cwd = g.cfg.cwd.clone();
        if let Some(p) = g.pending.first() {
            let headline = format!("wants to run {}", p.title);
            return Some(crate::block::Waiting {
                id: p.id.clone(),
                what: AskWhat::Approve,
                headline,
                agent,
                cwd,
                at_ms: p.at_ms,
            });
        }
        let e = g.open_ask()?;
        Some(crate::block::Waiting {
            id: e.ask.id.clone(),
            what: AskWhat::Question,
            headline: e.ask.headline(),
            agent,
            cwd,
            at_ms: e.ask.at_ms,
        })
    }
}

/// A list of strings, skipping anything else.
fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|s| s.as_str().map(str::to_owned)).collect()
}

/// A prompt's files (M71), uploaded for this block with M70's route: an
/// image is kept in the block's folder (and its upload goes); anything else
/// goes as its path.
fn attach(ctx: &BlockCtx, q: &mut Queued, files: &[String]) -> Result<(), String> {
    #[cfg(unix)]
    for f in files {
        let path = crate::upload::take(ctx.id, f).map_err(|e| format!("{f}: {e}"))?;
        let bytes = std::fs::read(&path).map_err(|e| format!("{f}: {e}"))?;
        if images::sniff(&bytes).is_some() {
            q.images.push(images::keep(&ctx.dir, &bytes).map_err(|e| format!("can't keep {f}: {e}"))?);
            let _ = std::fs::remove_file(&path);
        } else {
            let sep = if q.text.is_empty() { "" } else { "\n\n" };
            q.text = format!("{}{sep}{}", q.text, path.display());
        }
    }
    #[cfg(not(unix))]
    if !files.is_empty() {
        let _ = (ctx, q);
        return Err("files can't be attached on this host yet".into());
    }
    Ok(())
}

/// " by Sam", when a note says who (M29).
fn by_of(e: &Value) -> String {
    e["by"].as_str().map(|b| format!(" by {b}")).unwrap_or_default()
}

/// An agent block's transcript from its directory alone (for search over
/// closed blocks too).
pub fn transcript_of(dir: &Path) -> Option<String> {
    if std::fs::read_to_string(dir.join("kind")).ok()?.trim() != "agent" {
        return None;
    }
    let log = PaneLog::open(dir.to_owned()).ok()?;
    let (_, bytes) = log.read_from(log.start()).ok()?;
    let mut inner = Inner::new(Config::default(), None);
    for line in bytes.split(|b| *b == b'\n') {
        inner.rebuild_line(line);
    }
    Some(inner.t.markdown())
}

#[cfg(test)]
mod tests;
