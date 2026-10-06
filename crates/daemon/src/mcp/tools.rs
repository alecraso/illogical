//! The tools (M16): about twenty, shaped for agents rather than mirroring
//! every endpoint; the ones that do several jobs (list, show, draft) take a
//! `kind`, and history's `kind` takes `output` for what panes printed, so a
//! client's list stays short (#349). Each answers with a JSON object that has a `summary`
//! sentence in it (the text block is the same JSON), or fails with a
//! sentence an agent can act on (`isError`), never a protocol error.

use std::{future::Future, sync::Arc, time::Duration};

use illogical_proto::{
    Attention, BlockType, Driver, PaneId, Policy, SessionId, StartedBy, ThreadTarget,
    api::{ActRequest, HistoryEntry, HistoryKind, OpenRequest, PaneSummary, RunRequest, WaitResult},
};
use rmcp::{
    Peer, RoleServer,
    handler::server::common::schema_for_type,
    model::{CallToolResult, ContentBlock, ProgressNotificationParam, ProgressToken, Tool, ToolAnnotations},
    service::RequestContext,
};
use schemars::JsonSchema;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::time::Instant;

use super::{Caller, Scope};
use crate::{
    history::{self, Filter},
    mux::Api,
    osc::strip,
    pane::{CaptureFormat, CaptureScope},
    server::App,
    store::{PaneLog, now_ms},
};

/// A page of output, by default (characters).
const PAGE: usize = 16_000;
/// The most any one result carries: Claude Code swaps anything over about
/// 50,000 for a preview and a file (S14).
const PAGE_MAX: usize = 40_000;
/// How long a wait (or `run` with `wait`) waits before answering "still
/// running": under interactive Claude Code's 120s auto-background.
const WAIT_DEFAULT: Duration = Duration::from_secs(100);
const WAIT_MAX: Duration = Duration::from_secs(3600);
/// Progress while waiting: over HTTP, Claude Code drops a call that's
/// silent for 60s.
const PROGRESS_EVERY: Duration = Duration::from_secs(15);
/// The last lines a finished command's result carries.
const TAIL_LINES: usize = 40;
/// Invites one caller may have waiting for the user (#234).
const INVITES_WAITING: u64 = 5;
/// Permission modes that approve everything (Claude Code's, codex-acp's):
/// start_agent won't start an agent in one (#163).
const SKIPS_CHECKS: &[&str] = &["bypassPermissions", "full-access"];

fn progress_every() -> Duration {
    // Tests make it short.
    std::env::var("ILLOGICAL_MCP_PROGRESS_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(PROGRESS_EVERY)
}

// ---------------------------------------------------------------- arguments

/// A pane or block: `7` or `"%7"`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum PaneArg {
    Id(PaneId),
    Name(String),
}

impl PaneArg {
    fn id(&self) -> Result<PaneId, String> {
        match self {
            PaneArg::Id(n) => Ok(*n),
            PaneArg::Name(s) => s
                .trim()
                .trim_start_matches('%')
                .parse()
                .map_err(|_| format!("{s:?} isn't a pane: give its number, like 7 or \"%7\"")),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub struct RunArgs {
    /// The command line, typed into a new shell (so it's in history, and the
    /// shell stays for you to take over). None: just a shell.
    #[serde(default)]
    pub command: Option<String>,
    /// Where it starts: a directory (on its machine, for a VM).
    #[serde(default)]
    pub cwd: Option<String>,
    /// Split this pane instead of opening a new tab; "self" is the caller's
    /// own pane. An agent block's token always splits (beside the agent, by
    /// default).
    #[serde(default)]
    pub split: Option<PaneArg>,
    /// With split: run where that pane runs (its tab's VM) instead of on
    /// this host.
    #[serde(default)]
    pub join: bool,
    /// On a new throwaway VM of its own, deleted when the pane closes.
    #[serde(default)]
    pub vm: bool,
    /// In a new tab whose panes share a new throwaway VM.
    #[serde(default)]
    pub vm_tab: bool,
    /// The VM's image (the provider's default if none).
    #[serde(default)]
    pub image: Option<String>,
    /// On an existing sandbox, by the provider's name for it.
    #[serde(default)]
    pub machine: Option<String>,
    /// The session (by name or id) for a new tab; made if it doesn't exist.
    #[serde(default)]
    pub session: Option<String>,
    /// After a reboot: `shell` (default), `none`, `rerun` or `rerun-ask`.
    #[serde(default)]
    pub policy: Option<String>,
    /// Wait for the command to finish (up to timeout); else return at once.
    #[serde(default)]
    pub wait: bool,
    /// Seconds to wait before answering "still running" (default 100).
    #[serde(default)]
    pub timeout: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SendArgs {
    pub pane: PaneArg,
    /// Text to type (to an agent block: its next prompt).
    #[serde(default)]
    pub text: Option<String>,
    /// Press Enter after the text (default true).
    #[serde(default)]
    pub enter: Option<bool>,
    /// Named keys, after the text: C-c, C-d, Up, Down, Enter, Escape, Tab,
    /// F5, M-x, or single characters.
    #[serde(default)]
    pub keys: Vec<String>,
    /// To an app block: which of its box's chat tabs (title or chat key);
    /// default the first.
    #[serde(default)]
    pub tab: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AttachArgs {
    pub pane: PaneArg,
    /// A file on this host (a screenshot, a log). An agent on a machine of
    /// its own sends data instead.
    #[serde(default)]
    pub path: Option<String>,
    /// Or the file itself, base64.
    #[serde(default)]
    pub data: Option<String>,
    /// With data: its extension (png, jpg, txt); an image's is found.
    #[serde(default)]
    pub ext: Option<String>,
    /// To an agent block: a prompt to send with it.
    #[serde(default)]
    pub text: Option<String>,
    /// Into a terminal: paste the path even if what's in front isn't a
    /// shell or an agent.
    #[serde(default)]
    pub force: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadArgs {
    pub pane: PaneArg,
    /// Where to start (a stream offset: a previous result's next_offset).
    /// Without it: the latest output, or with last_command, its output.
    #[serde(default)]
    pub offset: Option<u64>,
    /// The output of the pane's last (or current) command.
    #[serde(default)]
    pub last_command: bool,
    /// At most this many characters (default 16000, at most 40000).
    #[serde(default)]
    pub max_chars: Option<usize>,
    /// What the pane shows right now instead, as text (for full-screen
    /// programs); the other arguments don't apply.
    #[serde(default)]
    pub screen: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct PaneOnly {
    pub pane: PaneArg,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Until {
    /// The command running (or the next one to start) finished.
    CommandEnd,
    /// The pane's process exited.
    Exit,
    /// The output matched `pattern` (a regex).
    Match,
    /// It's not working any more (an agent finished its turn, say).
    Idle,
    /// It asks for you: an agent's approval or question.
    NeedsInput,
}

#[derive(Deserialize, JsonSchema)]
pub struct WaitArgs {
    pub pane: PaneArg,
    pub until: Until,
    /// For until: match, a regular expression.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Seconds before answering "still running" (default 100).
    #[serde(default)]
    pub timeout: Option<f64>,
}

#[derive(Deserialize, JsonSchema, Default)]
pub struct ListArgs {}

#[derive(Deserialize, JsonSchema)]
pub struct ThreadArgs {
    /// The pane whose thread it is (`7` or `"%7"`). Neither this nor a session: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub pane: Option<PaneArg>,
    /// Or a session's thread, by its id.
    #[serde(default)]
    pub session: Option<SessionId>,
    /// Only messages after this id (what an earlier call returned as last).
    #[serde(default)]
    pub after: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PostThreadArgs {
    /// The pane whose thread to post in (`7` or `"%7"`). Neither this nor a session: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub pane: Option<PaneArg>,
    /// Or a session's thread, by its id.
    #[serde(default)]
    pub session: Option<SessionId>,
    /// The message. `@name` mentions someone (they're notified).
    pub text: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct HistoryArgs {
    /// Only commands that failed (exit code not 0); answers and agent
    /// steps never count.
    #[serde(default)]
    pub failed: bool,
    /// `command` (ran in a shell), `answer` (an answer or approval, with
    /// who gave it) or `agent` (an agent block's non-shell steps). Without
    /// it, all three. Or `output`: lines of output across panes (and agents'
    /// transcripts) matching pattern, with the pane and command each came
    /// from; it takes pattern, since and limit.
    #[serde(default)]
    pub kind: Option<String>,
    /// For kind output: a regular expression, matched against each line of
    /// output.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Started at most this long ago: `90m`, `36h`, `2d`, or seconds.
    #[serde(default)]
    pub since: Option<String>,
    /// Started at least this long ago (with since: a window, like since
    /// 2d before 1d for yesterday).
    #[serde(default)]
    pub before: Option<String>,
    /// Ran in this directory or below it.
    #[serde(default)]
    pub cwd: Option<String>,
    /// The command line matches this regex.
    #[serde(rename = "match", default)]
    pub matching: Option<String>,
    /// One pane's.
    #[serde(default)]
    pub pane: Option<PaneArg>,
    /// At most this many, newest first (default 30, at most 200).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// A regular expression, matched against each line of output.
    pub pattern: String,
    /// Output from at most this long ago: `90m`, `36h`, `2d`, or seconds.
    #[serde(default)]
    pub since: Option<String>,
    /// At most this many lines (default 30, at most 200).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OpenPortArgs {
    /// The port, on the machine the pane runs on.
    pub port: u16,
    /// The page's path (default /).
    #[serde(default)]
    pub path: Option<String>,
    /// Beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OpenAppArgs {
    /// The app's name in the user's studio. Leave it out to list their
    /// apps instead.
    #[serde(default)]
    pub app: Option<String>,
    /// Beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
}

#[derive(Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// Claude Code.
    #[default]
    Claude,
    Codex,
    /// An agent in a Fountain sandbox (fountain_agent names it).
    Fountain,
    /// Any ACP agent server, by command.
    Acp,
}

#[derive(Deserialize, JsonSchema)]
pub struct StartAgentArgs {
    #[serde(default)]
    pub agent: AgentKind,
    /// What to ask it.
    pub prompt: String,
    /// For acp: the agent server's command line.
    #[serde(default)]
    pub command: Option<String>,
    /// For fountain: the agent's name or id.
    #[serde(default)]
    pub fountain_agent: Option<String>,
    /// For claude: wear this Fountain agent (name or id), on this host: its
    /// system prompt, skills and MCP servers (list kind fountain_agents
    /// lists them).
    #[serde(default)]
    pub as_fountain: Option<String>,
    /// A model to switch to (`haiku`, ...).
    #[serde(default)]
    pub model: Option<String>,
    /// Where it works.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Tools whose requests it may make without asking (`Read`, `Edit`,
    /// `Bash`, ...), as "always" on a card.
    #[serde(default)]
    pub allow: Vec<String>,
    /// The permission mode its session starts in: `default`, `acceptEdits`,
    /// `plan`, `auto` (Claude Code's), or the agent's own. Not one that
    /// skips every check: the user picks that.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// For claude: the user's Claude Code settings (allow and deny lists,
    /// default mode, CLAUDE.md), without their hooks.
    #[serde(default)]
    pub user_settings: bool,
    /// Open it beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
    /// The session for a new tab, when not beside a pane.
    #[serde(default)]
    pub session: Option<String>,
    /// On a new throwaway VM of its own.
    #[serde(default)]
    pub vm: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListConversationsArgs {
    /// Words in the title, prompts or folder.
    #[serde(default)]
    pub query: Option<String>,
    /// Only ones under this folder.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Only ones open somewhere now.
    #[serde(default)]
    pub live: bool,
    /// Everything: `claude -p` and SDK runs, archived ones, ones whose folder is gone.
    #[serde(default)]
    pub all: bool,
    /// How many [default 30].
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OpenConversationArgs {
    /// The conversation's id, or the start of it (list kind conversations).
    pub id: String,
    /// Then continue it, or fork it (for one open somewhere else) and go on in the fork.
    #[serde(default)]
    pub then: Option<ConversationThen>,
    /// Beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ConversationThen {
    Continue,
    Fork,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Response {
    /// Approve the pending permission request.
    Allow,
    /// Refuse it (message: why).
    Deny,
    /// Answer the pending question (answers: its fields).
    Answer,
    /// Skip the pending question.
    Skip,
}

#[derive(Deserialize, JsonSchema)]
pub struct PromptArgs {
    /// An agent block, or a terminal running an agent (Claude Code, Codex).
    pub pane: PaneArg,
    /// The prompt.
    pub text: String,
    /// It's waiting on an approval or a question and this answers it.
    /// Without it, an agent waiting on someone isn't typed at: its
    /// question comes back instead.
    #[serde(default)]
    pub answering: bool,
    /// Seconds before answering "still running" (default 100): then wait
    /// until idle.
    #[serde(default)]
    pub timeout: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RespondArgs {
    /// The agent block, or a terminal running Claude Code.
    pub pane: PaneArg,
    pub action: Response,
    /// allow: `once` (default) or `always`.
    #[serde(default)]
    pub option: Option<String>,
    /// deny: why, for the agent.
    #[serde(default)]
    pub message: Option<String>,
    /// answer: the question's fields, by name, as wait (until needs_input)
    /// showed them, e.g. {"question_0": "Blue"}.
    #[serde(default)]
    pub answers: Option<serde_json::Map<String, Value>>,
    /// Which request it answers (from wait); default whatever it asks now.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadFileArgs {
    /// The file. Relative paths are from the pane's directory, or home.
    pub path: String,
    /// On the machine this pane runs on. Default: the caller's own pane (an
    /// agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub pane: Option<PaneArg>,
    /// A byte offset to start at (a previous result's next_offset).
    #[serde(default)]
    pub offset: Option<u64>,
    /// At most this many characters (default 16000, at most 40000).
    #[serde(default)]
    pub max_chars: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ShowChangesArgs {
    /// The pane whose repository it is (its directory, on its machine). Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
    /// Any directory in the repository, instead (on the pane's machine).
    #[serde(default)]
    pub repo: Option<String>,
    /// Compare the working tree with this revision instead of HEAD.
    #[serde(default)]
    pub rev_a: Option<String>,
    /// With rev_a: the range rev_a..rev_b instead of the working tree.
    #[serde(default)]
    pub rev_b: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ShowFileArgs {
    /// The file. Relative paths are from the pane's directory, or home.
    pub path: String,
    /// The line to mark and scroll to.
    #[serde(default)]
    pub line: Option<u32>,
    /// Beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OpenWorkspaceArgs {
    /// The workspace's root: a directory holding chant.workspace.json (an
    /// absolute path, or ~/...).
    pub dir: String,
    /// The environment whose gates and releases to read (default local).
    #[serde(default)]
    pub env: Option<String>,
    /// Beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OpenPrArgs {
    /// The pull request: its link (a Forgejo PR's or a GitLab MR's),
    /// OWNER/REPO#N, GROUP/PROJECT!N (GitLab), or N (in dir's repository).
    pub pr: String,
    /// A clone of the repository on this host (absolute): what N means,
    /// and where diff and checkout fetch the PR's code.
    #[serde(default)]
    pub dir: Option<String>,
    /// Beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OpenIssueArgs {
    /// The issue: its link, OWNER/REPO#N, or N (in dir's repository).
    pub issue: String,
    /// A clone of the repository on this host (absolute): what N means,
    /// and where the user's "Agent on this" makes its worktree.
    #[serde(default)]
    pub dir: Option<String>,
    /// Beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
}

/// A PR or issue block, to read.
#[derive(Deserialize, JsonSchema)]
pub struct ReadForgeArgs {
    /// The PR or issue block (from show).
    pub block: PaneArg,
}

#[derive(Deserialize, JsonSchema)]
pub struct CommentArgs {
    /// The PR or issue block (from show).
    pub block: PaneArg,
    /// The comment, in markdown.
    pub body: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct IssueNewArgs {
    /// The repository, OWNER/REPO (default: dir's, or the one beside you).
    #[serde(default)]
    pub repo: Option<String>,
    /// A clone of it on this host (absolute).
    #[serde(default)]
    pub dir: Option<String>,
    /// The new issue's title.
    pub title: String,
    /// The issue's text, in markdown.
    #[serde(default)]
    pub body: Option<String>,
    /// Beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PrReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

#[derive(Deserialize, JsonSchema)]
pub struct PrReviewArgs {
    /// The PR block (from show).
    pub block: PaneArg,
    pub event: PrReviewEvent,
    /// The review's text (needed unless it approves).
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PrMergeArgs {
    /// The PR block (from show).
    pub block: PaneArg,
    /// merge, rebase, rebase-merge, squash or fast-forward-only (default merge).
    #[serde(default)]
    pub style: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct InvitePersonArgs {
    /// Whom: a name this machine knows (a teammate, someone the session is
    /// shared with), tailnet:<login> or account:<id>.
    pub who: String,
    /// viewer (the default) or editor.
    #[serde(default)]
    pub role: Option<String>,
    /// Where it opens, and so the session (default: your own pane; a full
    /// client outside a pane must say).
    #[serde(default)]
    pub pane: Option<PaneArg>,
    /// Why you want them there, at most 500 characters: the user reads it on
    /// the card, the person in their notification.
    pub note: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadInviteArgs {
    /// The draft id invite_person returned.
    pub draft: String,
    /// The pane you invited from, if you gave one then.
    #[serde(default)]
    pub pane: Option<PaneArg>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListAgentsArgs {
    /// Words to look for in each agent's name, description, skills and MCP
    /// servers (all must match): a skill's name finds the agents that have
    /// it.
    #[serde(default)]
    pub query: Option<String>,
    /// Where they come from: agent-specs (the curated ones, managed by
    /// chant), hand (hand-made) or app (made by an app). Default: all.
    #[serde(default)]
    pub source: Option<String>,
    /// The Fountain credentials profile (default: the user's default).
    #[serde(default)]
    pub profile: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ReadAgentArgs {
    /// The agent's name (or id), as kind fountain_agents gives it.
    pub name: String,
    /// The Fountain credentials profile (default: the user's default).
    #[serde(default)]
    pub profile: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct OpenFountainArgs {
    /// "catalog" (the default: the agents) or "runner" (this host as the
    /// Fountain runner, and its sandboxes).
    #[serde(default)]
    pub view: Option<String>,
    /// Start with this search (as kind fountain_agents' query).
    #[serde(default)]
    pub query: Option<String>,
    /// Start with this source: agent-specs, hand or app.
    #[serde(default)]
    pub source: Option<String>,
    /// The Fountain credentials profile (default: the user's default).
    #[serde(default)]
    pub profile: Option<String>,
    /// Beside this pane. Default: the caller's own pane (an agent block, or the terminal pane `illogical mcp` runs in).
    #[serde(default)]
    pub beside: Option<PaneArg>,
}

// ---------------------------------------------------------------- the list

type Schema = Arc<serde_json::Map<String, Value>>;

struct Def {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    args: Args,
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
}

/// What a tool takes.
enum Args {
    /// One set of arguments, for a tool that does one thing.
    One(fn() -> Schema),
    /// A grouped tool (#349): its `kind` argument picks one of these jobs,
    /// and its schema is theirs merged. `default`: the kind when none is
    /// given (none: kind is required).
    Kinds { kinds: Vec<&'static Kind>, default: Option<&'static str> },
}

/// One job of a grouped tool, like `show {kind: "port", port: 3000}`. Each
/// is a row, so the kinds that belong to one feature (Fountain's, the
/// studio's) can be left out of their group, and their arguments with them,
/// by filtering its table ([`defs`]).
struct Kind {
    name: &'static str,
    /// What it does, for the tool's description.
    description: &'static str,
    schema: fn() -> Schema,
}

/// `show`'s blocks.
const SHOW: &[Kind] = &[
    Kind {
        name: "port",
        description: "A browser block on a port of the machine a pane runs on (a dev server), beside that pane, so the user sees it next to its terminal.",
        schema: schema_for_type::<OpenPortArgs>,
    },
    Kind {
        name: "changes",
        description: "A diff block (M11): what changed in a pane's git repository (the working tree against HEAD, or against rev_a, or rev_a..rev_b), as a file list with +/- the user can open to hunks and files, live while they look. Returns the files; read_output on the block gives the unified diff.",
        schema: schema_for_type::<ShowChangesArgs>,
    },
    Kind {
        name: "file",
        description: "A file block (M11): a file on a pane's machine, read-only, scrolled to a line and followed live, for the user to look at.",
        schema: schema_for_type::<ShowFileArgs>,
    },
    Kind {
        name: "pr",
        description: "A pull request on the user's Forgejo or GitHub, or a GitLab merge request (M36, M38, M39): its checks, reviews and timeline, read with the user's own tea, gh (or glab) login, and what it waits on them for (a review asked of them, red checks, changes requested) as attention on the phone and the swarm. Returns the PR as text; draft writes to it.",
        schema: schema_for_type::<OpenPrArgs>,
    },
    Kind {
        name: "issue",
        description: "An issue on the user's Forgejo (M37): its labels, assignees, the pull requests that refer to it and its timeline, read with the user's own tea login; one assigned to them or mentioning them is attention on the phone and the swarm. From the block the user can start an agent on it in a worktree of its own. Returns the issue as text.",
        schema: schema_for_type::<OpenIssueArgs>,
    },
    Kind {
        name: "conversation",
        description: "A Claude Code conversation (list kind conversations) as an agent block, stopped, with its transcript; then: continue (refused while it's open somewhere else) or fork (a new session with its history; the original is left alone). send_input to the block goes on.",
        schema: schema_for_type::<OpenConversationArgs>,
    },
    Kind {
        name: "app",
        description: "One of the user's studio apps (a box with its own agent, hud) as an app block: the app in a frame, and its agent's questions as asks on the block; send_input to the block prompts its agent. Without app: lists the user's apps.",
        schema: schema_for_type::<OpenAppArgs>,
    },
    Kind {
        name: "workspace",
        description: "A chant workspace block (M34): its members as cards (open a shell, an agent or the changes in one), its records, and the gates waiting for a person, which the user can approve there and which show as attention on the phone and the swarm. Read through the workspace's own chant. Returns its members and the gates waiting.",
        schema: schema_for_type::<OpenWorkspaceArgs>,
    },
    Kind {
        name: "fountain",
        description: "The user's Fountain agents as a catalog block (M43): a card per agent with its skills, servers and where it comes from, filters, and Run on Fountain / Spec for each. Returns the (filtered) list as text. With view \"runner\" (M45b): this host as the account's Fountain runner instead: its status, the other runners, and its sandboxes with their conversations.",
        schema: schema_for_type::<OpenFountainArgs>,
    },
];

/// `draft`'s writes to a forge.
const DRAFT: &[Kind] = &[
    Kind {
        name: "comment",
        description: "A comment on a PR or issue block's pull request or issue.",
        schema: schema_for_type::<CommentArgs>,
    },
    Kind {
        name: "review",
        description: "A review (approve, request_changes or comment) of a PR block's pull request.",
        schema: schema_for_type::<PrReviewArgs>,
    },
    Kind { name: "merge", description: "Merging a PR block's pull request.", schema: schema_for_type::<PrMergeArgs> },
    Kind {
        name: "issue",
        description: "A new issue on the user's Forgejo: a block beside you holding the draft as a card with its title and text, which then shows the issue once sent. Returns the block.",
        schema: schema_for_type::<IssueNewArgs>,
    },
];

/// `list`'s lists.
const LIST: &[Kind] = &[
    Kind {
        name: "panes",
        description: "Every pane and block (terminals, browsers, agents): where it is, what it runs, whether it needs attention, who started it.",
        schema: schema_for_type::<ListArgs>,
    },
    Kind {
        name: "conversations",
        description: "Claude Code conversations on this machine, from a terminal or the desktop app's Code tab, newest first: id, title, folder, first and last prompt, where it's open now, and the agent block that has it. show kind conversation opens one.",
        schema: schema_for_type::<ListConversationsArgs>,
    },
    Kind {
        name: "devices",
        description: "The user's devices (their phone, say) that lend tools to agents (S33): each with its tools and their arguments, and whether it's connected now. One that isn't can still be called with device_call: it's woken with a notification, which the user has to open.",
        schema: schema_for_type::<NoArgs>,
    },
    Kind {
        name: "fountain_agents",
        description: "The agents on the user's Fountain account (M43), one compact row each: name, runtime and model, where it comes from (agent-specs: curated; hand: hand-made; app: made by an app), skills, MCP servers and description. query searches names, descriptions, skills and servers. To hand one a task, start_agent {agent: fountain, fountain_agent: NAME}.",
        schema: schema_for_type::<ListAgentsArgs>,
    },
    Kind {
        name: "fountain_agent",
        description: "One Fountain agent's whole recipe, by name (M43): its system prompt, skills (inline or from GitHub), MCP servers, model, runtime, environment, sandbox provider and metadata, as Fountain returns it. Secrets are never in it: a server's credentials show as their ${VAR} references.",
        schema: schema_for_type::<ReadAgentArgs>,
    },
];

/// The jobs that work but aren't listed without `labs`: they're for setups
/// a stranger doesn't have (Fountain, the studio, chant workspaces). Each is
/// a kind of a grouped tool, so without `labs` it's filtered out of its
/// group's table, and its arguments and its line in the description go with
/// it. A caller who names one still reaches it (`dispatch`), by its kind or
/// its name before #349.
const UNLISTED: [(&str, &str); 5] = [
    ("show", "app"),
    ("show", "workspace"),
    ("show", "fountain"),
    ("list", "fountain_agents"),
    ("list", "fountain_agent"),
];

/// Chat's two tools, unlisted without `labs` like the rest.
const UNLISTED_THREADS: [&str; 2] = ["read_thread", "post_thread"];

/// Whether a grouped tool's kind is listed, with `labs` or without.
fn listed_kind(labs: bool, tool: &str, kind: &str) -> bool {
    labs || !UNLISTED.contains(&(tool, kind))
}

/// The tools `tools/list` shows: all of them with `labs`, else without those
/// a stranger can't use, and without the kinds they can't use in the rest.
fn defs(labs: bool) -> Vec<Def> {
    all_defs()
        .into_iter()
        .filter(|d| labs || !UNLISTED_THREADS.contains(&d.name))
        .map(|mut d| {
            let name = d.name;
            if let Args::Kinds { kinds, .. } = &mut d.args {
                kinds.retain(|k| listed_kind(labs, name, k.name));
            }
            d
        })
        .collect()
}

/// A group's table as a tool holds it.
fn table(kinds: &'static [Kind]) -> Vec<&'static Kind> {
    kinds.iter().collect()
}

/// Every tool there is, listed or not, with every kind.
fn all_defs() -> Vec<Def> {
    vec![
        Def {
            name: "run",
            title: "Run a command in a pane",
            description: "Run a command in a new terminal pane (a new tab, or a split). The command is typed into a shell, so the user can watch it, scroll it and take over, and it outlives this conversation. With wait, waits for it to finish (up to timeout) and returns its exit code and last lines; past the timeout it answers \"still running\": call wait.",
            args: Args::One(schema_for_type::<RunArgs>),
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        },
        Def {
            name: "send_input",
            title: "Type into a pane",
            description: "Type text (Enter after it unless enter is false) and/or press named keys (C-c, Up, Escape, ...) in a pane. To an agent block, text is its next prompt.",
            args: Args::One(schema_for_type::<SendArgs>),
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: false,
        },
        Def {
            name: "attach",
            title: "Attach a file to a pane",
            description: "Put a file (a screenshot, an image, a log) into a pane: into a terminal, its path is pasted where a shell or an agent like claude reads it (whatever else is in front is refused unless force); to an agent block, it goes with text as its next prompt, an image as an image. Give a path on this host, or the file as base64 data.",
            args: Args::One(schema_for_type::<AttachArgs>),
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: false,
        },
        Def {
            name: "read_output",
            title: "Read a pane's output",
            description: "A pane's output as text (escape sequences stripped): the latest, from an offset, or its last command's. Paged: pass next_offset back as offset for more. With screen: what it shows right now (for full-screen programs).",
            args: Args::One(schema_for_type::<ReadArgs>),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        Def {
            name: "wait",
            title: "Wait for a pane",
            description: "Wait until a pane's command ends, its process exits, its output matches a pattern, or it's idle or asks for input (an agent's approval or question). Answers \"still running\" with the offset after timeout seconds (default 100): call it again.",
            args: Args::One(schema_for_type::<WaitArgs>),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        Def {
            name: "list",
            title: "List panes and more",
            description: "What's here to work with, by kind (default panes).",
            args: Args::Kinds { kinds: table(LIST), default: Some("panes") },
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: true,
        },
        Def {
            name: "close",
            title: "Close a pane",
            description: "Close a pane or block, ending what runs in it.",
            args: Args::One(schema_for_type::<PaneOnly>),
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: false,
        },
        Def {
            name: "read_thread",
            title: "Read a thread",
            description: "The people's conversation about a pane or a session (M61): who said what and when, oldest first, with quoted terminal output. When someone writes @agent in a pane's thread, it reaches that pane's agent as a follow-up; answer with post_thread.",
            args: Args::One(schema_for_type::<ThreadArgs>),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        Def {
            name: "post_thread",
            title: "Post in a thread",
            description: "Post a message in a pane's or a session's thread, where the people working on it talk; it shows as from an agent. Use it to answer an @agent message or to tell the people something they should see. The result's `unreached` lists any @name that reached no one, and why. Someone who can't see the thread isn't told: call invite_person to ask the user to bring them in.",
            args: Args::One(schema_for_type::<PostThreadArgs>),
            read_only: false,
            destructive: false,
            idempotent: false,
            open_world: false,
        },
        Def {
            name: "history",
            title: "Command history and output",
            description: "What happened across panes (open and recently closed), newest first: commands with exit codes, directories, when, and who ran them, and answers and approvals with who gave them. Each has a kind (command, answer or agent); filter by kind, failed (commands only), since/before (\"2d\", \"36h\"), cwd, a regex. kind output instead: lines of output across panes (and agents' transcripts) matching pattern, a regex, with the pane and command each came from (since and limit apply).",
            args: Args::One(schema_for_type::<HistoryArgs>),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        Def {
            name: "show",
            title: "Show the user a block",
            description: "Show the user something as a block beside a pane (beside; default the caller's own pane), instead of describing it. kind says what.",
            args: Args::Kinds { kinds: table(SHOW), default: None },
            read_only: false,
            destructive: false,
            idempotent: false,
            open_world: true,
        },
        Def {
            name: "start_agent",
            title: "Start an agent",
            description: "Start an agent (Claude Code, Codex, any ACP agent) in an agent block with a prompt. Its approvals and questions come to the block; wait until needs_input, then agent_respond, or leave them for the user.",
            args: Args::One(schema_for_type::<StartAgentArgs>),
            read_only: false,
            destructive: false,
            idempotent: false,
            open_world: true,
        },
        Def {
            name: "prompt_agent",
            title: "Prompt an agent and wait",
            description: "Give an agent (an agent block, or Claude Code or Codex in a terminal) a prompt and wait for its turn in one call: returns when the turn ends (done), when it asks for someone (needs_input, with the question: agent_respond answers it), or stalled with its screen's last lines if it shows no sign of work within a few seconds (no agent there, the prompt not submitted, the agent gone). An agent already waiting on an approval or question isn't typed at; its question comes back (pass answering to type the answer).",
            args: Args::One(schema_for_type::<PromptArgs>),
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: false,
        },
        Def {
            name: "agent_respond",
            title: "Answer an agent",
            description: "Allow or deny an agent's pending permission request, or answer or skip its pending question (as wait until needs_input showed it).",
            args: Args::One(schema_for_type::<RespondArgs>),
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: false,
        },
        Def {
            name: "read_forge",
            title: "Read a PR or issue block",
            description: "A PR or issue block's pull request or issue as text (header, body, checks, reviews, linked pull requests, timeline), what it waits on the user for, an issue's agent and its PR, and your drafts: each waiting, sent (with its link and who sent it) or dropped (and by whom).",
            args: Args::One(schema_for_type::<ReadForgeArgs>),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: true,
        },
        Def {
            name: "draft",
            title: "Draft a forge write",
            description: "Draft a write to a pull request or issue, by kind. Nothing reaches the forge in an agent's name: the draft waits on its block as a card the user (or an editor) edits and sends, or drops. Returns its draft id (a new issue: its block) at once; read_forge shows what became of it.",
            args: Args::Kinds { kinds: table(DRAFT), default: None },
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        },
        Def {
            name: "invite_person",
            title: "Ask to invite a person",
            description: "Ask the user to bring someone into the session you work in (your pane's): a teammate, someone it's shared with, or tailnet:<login>, as a viewer (default) or an editor, with a note saying why. Nothing is shared in an agent's name: it waits as a card beside you that only the session's owner sends (after editing the role or note, if they like) or declines. Only the owner's own agents may ask (not one a guest started). Returns its draft id at once with status waiting; read_invite shows what became of it.",
            args: Args::One(schema_for_type::<InvitePersonArgs>),
            read_only: false,
            destructive: false,
            idempotent: false,
            open_world: true,
        },
        Def {
            name: "read_invite",
            title: "Read an invite draft",
            description: "What became of an invite_person draft: waiting, sent (with its grant and delivery: sent, pending or unreachable, and why), declined (and the owner's reason), dropped (nobody answered in a day) or failed (and why); who settled it and when.",
            args: Args::One(schema_for_type::<ReadInviteArgs>),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        Def {
            name: "read_file",
            title: "Read a file",
            description: "A text file on this host or the machine a pane runs on, paged by byte offset.",
            args: Args::One(schema_for_type::<ReadFileArgs>),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
        },
        Def {
            name: "device_call",
            title: "Use a tool on the user's device",
            description: "Call a tool on one of the user's devices (list kind devices shows them): take a photo, where the phone is, and so on. The user sees each call on the device and allows or denies it there, so it can take a while; it waits up to timeout seconds (default 100). A photo comes back as a file path on this host. If the device isn't connected, it's woken with a notification first.",
            args: Args::One(schema_for_type::<DeviceCallArgs>),
            read_only: false,
            destructive: false,
            idempotent: false,
            open_world: true,
        },
    ]
}

/// What a tool's name meant before #349 grouped them: the tool now, and
/// the argument that picks the old job. Answered for a client that kept
/// the old names (a script, a list cached before an upgrade); never listed.
fn renamed(old: &str) -> Option<(&'static str, Option<(&'static str, Value)>)> {
    let kind = |tool: &'static str, k: &str| Some((tool, Some(("kind", json!(k)))));
    match old {
        "capture_screen" => Some(("read_output", Some(("screen", json!(true))))),
        "search" => kind("history", "output"),
        "list_conversations" => kind("list", "conversations"),
        "list_devices" => kind("list", "devices"),
        "list_agents" => kind("list", "fountain_agents"),
        "read_agent" => kind("list", "fountain_agent"),
        "open_port" => kind("show", "port"),
        "show_changes" => kind("show", "changes"),
        "show_file" => kind("show", "file"),
        "open_pr" => kind("show", "pr"),
        "open_issue" => kind("show", "issue"),
        "open_conversation" => kind("show", "conversation"),
        "open_app" => kind("show", "app"),
        "open_workspace" => kind("show", "workspace"),
        "open_fountain" => kind("show", "fountain"),
        "read_pr" | "read_issue" => Some(("read_forge", None)),
        "pr_comment" | "issue_comment" => kind("draft", "comment"),
        "pr_review" => kind("draft", "review"),
        "pr_merge" => kind("draft", "merge"),
        "issue_new" => kind("draft", "issue"),
        _ => None,
    }
}

/// A grouped tool's schema: `kind`, then every kind's arguments. One that
/// several kinds take has one entry, its description saying which kind
/// means what; what each kind needs is in the tool's description
/// ([`kinds_text`]), since a flat schema can't say it.
fn grouped_schema(kinds: &[&Kind], default: Option<&str>) -> Schema {
    let mut kind = json!({
        "type": "string",
        "enum": kinds.iter().map(|k| k.name).collect::<Vec<_>>(),
        "description": "Which one (see the tool's description).",
    });
    if let Some(d) = default {
        kind["default"] = json!(d);
    }
    let mut props = serde_json::Map::new();
    props.insert("kind".into(), kind);
    let mut defs = serde_json::Map::new();
    // Each argument's descriptions, with the kinds that give each one, in
    // the order the kinds name them.
    let mut said: Vec<(String, Said)> = Vec::new();
    for k in kinds {
        let s = (k.schema)();
        for (name, p) in s.get("properties").and_then(Value::as_object).into_iter().flatten() {
            let mut p = p.clone();
            let what = p.as_object_mut().and_then(|o| o.remove("description"));
            let what = what.and_then(|w| w.as_str().map(str::to_owned)).unwrap_or_default();
            match props.get(name) {
                None => {
                    props.insert(name.clone(), p);
                }
                // The same argument, optional for one kind and needed by
                // another: the schema that allows null.
                Some(was) if was != &p && nullable(&p) => {
                    props.insert(name.clone(), p);
                }
                Some(_) => {}
            }
            let at = match said.iter().position(|(n, _)| n == name) {
                Some(i) => i,
                None => {
                    said.push((name.clone(), Vec::new()));
                    said.len() - 1
                }
            };
            match said[at].1.iter_mut().find(|(w, _)| *w == what) {
                Some((_, ks)) => ks.push(k.name),
                None => said[at].1.push((what, vec![k.name])),
            }
        }
        for (name, d) in s.get("$defs").and_then(Value::as_object).into_iter().flatten() {
            defs.entry(name.clone()).or_insert_with(|| d.clone());
        }
    }
    for (name, whats) in said {
        let whats: Vec<(String, Vec<&str>)> = whats.into_iter().filter(|(w, _)| !w.is_empty()).collect();
        let text = match whats.as_slice() {
            [] => continue,
            [(w, ks)] if ks.len() == kinds.len() => w.clone(),
            _ => whats.iter().map(|(w, ks)| format!("{}: {w}", ks.join(", "))).collect::<Vec<_>>().join(" "),
        };
        if let Some(o) = props.get_mut(&name).and_then(Value::as_object_mut) {
            o.insert("description".into(), json!(text));
        }
    }
    let mut out = serde_json::Map::new();
    out.insert("$schema".into(), json!("https://json-schema.org/draft/2020-12/schema"));
    out.insert("type".into(), json!("object"));
    out.insert("properties".into(), Value::Object(props));
    if default.is_none() {
        out.insert("required".into(), json!(["kind"]));
    }
    if !defs.is_empty() {
        out.insert("$defs".into(), Value::Object(defs));
    }
    Arc::new(out)
}

/// An argument's descriptions in a group, each with the kinds that give it.
type Said<'a> = Vec<(String, Vec<&'a str>)>;

/// Whether an argument's schema allows null (an optional one).
fn nullable(p: &Value) -> bool {
    let null = |t: &Value| t == "null" || t.as_array().is_some_and(|a| a.iter().any(|t| t == "null"));
    p.get("type").is_some_and(null)
        || ["anyOf", "oneOf"].iter().any(|k| {
            p.get(*k).and_then(Value::as_array).is_some_and(|a| a.iter().any(|s| s.get("type").is_some_and(null)))
        })
}

/// A kind's arguments as its line in the tool's description shows them,
/// the ones it needs first: `port, beside?, path?`.
fn kind_args(k: &Kind) -> Vec<String> {
    let s = (k.schema)();
    let needed: Vec<&str> =
        s.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let (mut first, mut then) = (Vec::new(), Vec::new());
    for (n, _) in s.get("properties").and_then(Value::as_object).into_iter().flatten() {
        match needed.contains(&n.as_str()) {
            true => first.push(n.clone()),
            false => then.push(format!("{n}?")),
        }
    }
    first.extend(then);
    first
}

/// A grouped tool's description: its own, then a line per kind with what
/// it takes.
fn kinds_text(lead: &str, kinds: &[&Kind], default: Option<&str>) -> String {
    let mut text = lead.to_owned();
    for k in kinds {
        let args = kind_args(k);
        let dflt = if default == Some(k.name) { " (the default)" } else { "" };
        let args = if args.is_empty() { String::new() } else { format!(" {{{}}}", args.join(", ")) };
        text.push_str(&format!("\n- {}{dflt}{args}: {}", k.name, k.description));
    }
    text
}

/// No arguments.
#[derive(Deserialize, JsonSchema)]
pub struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
pub struct DeviceCallArgs {
    /// The tool's name, from list kind devices.
    pub tool: String,
    /// Its arguments.
    #[serde(default)]
    pub args: Option<Value>,
    /// Which device: its id or part of its name. Optional when there's one.
    #[serde(default)]
    pub device: Option<String>,
    /// Seconds to wait for the answer, including waking the device
    /// (default 100, at most 3600).
    #[serde(default)]
    pub timeout: Option<f64>,
}

/// The tools `scope` may call, with `labs` or without.
pub fn list(scope: Scope, labs: bool) -> Vec<Tool> {
    defs(labs)
        .into_iter()
        .filter(|d| scope != Scope::Read || d.read_only)
        .map(|d| {
            let (description, schema) = match &d.args {
                Args::One(schema) => (d.description.to_owned(), schema()),
                Args::Kinds { kinds, default } => {
                    (kinds_text(d.description, kinds, *default), grouped_schema(kinds, *default))
                }
            };
            Tool::new(d.name, description, schema).with_title(d.title).with_annotations(
                ToolAnnotations::with_title(d.title)
                    .read_only(d.read_only)
                    .destructive(d.destructive)
                    .idempotent(d.idempotent)
                    .open_world(d.open_world),
            )
        })
        .collect()
}

/// What a call reaches: its tool (by its name now, for an old one), the
/// kind it picks for a grouped one, and its arguments without the kind.
/// Every kind answers, listed or not; `labs` only says which ones an error
/// names.
fn route(name: &str, mut args: Value, labs: bool) -> Result<(Def, Option<&'static str>, Value), String> {
    let name = match renamed(name) {
        Some((tool, set)) => {
            if let (Some((key, v)), Some(o)) = (set, args.as_object_mut()) {
                o.insert(key.into(), v);
            }
            tool
        }
        None => name,
    };
    let def = all_defs().into_iter().find(|d| d.name == name).ok_or_else(|| format!("no tool {name}"))?;
    let kind = match &def.args {
        Args::One(_) => None,
        Args::Kinds { kinds, default } => {
            let shown: Vec<&str> = kinds.iter().map(|k| k.name).filter(|k| listed_kind(labs, def.name, k)).collect();
            Some(pick_kind(def.name, kinds, &shown, *default, &mut args)?)
        }
    };
    Ok((def, kind, args))
}

/// A grouped tool's call: the kind it names (or the default), taken out
/// of `args`, which must then be arguments that kind takes. `shown`: the
/// kinds an error names.
fn pick_kind(
    tool: &str,
    kinds: &[&Kind],
    shown: &[&str],
    default: Option<&'static str>,
    args: &mut Value,
) -> Result<&'static str, String> {
    let names = || shown.join(", ");
    let given = args.as_object_mut().and_then(|o| o.remove("kind"));
    let name = match &given {
        Some(Value::String(s)) => s.as_str(),
        Some(v) => return Err(format!("{tool}'s kind is a string, one of {}; not {v}", names())),
        None => default.ok_or_else(|| format!("{tool} needs a kind: one of {}", names()))?,
    };
    let k = kinds
        .iter()
        .find(|k| k.name == name)
        .ok_or_else(|| format!("{tool} has no kind {name:?}: it's one of {}", names()))?;
    let takes = kind_args(k);
    for key in args.as_object().into_iter().flatten().map(|(n, _)| n) {
        if !takes.iter().any(|t| t.trim_end_matches('?') == key) {
            let takes = match takes.is_empty() {
                true => "it takes nothing else".to_owned(),
                false => format!("it takes {}", takes.join(", ")),
            };
            return Err(format!("{key} isn't an argument of {tool} kind {}: {takes}", k.name));
        }
    }
    Ok(k.name)
}

// ---------------------------------------------------------------- calls

type Out = Result<Value, String>;

fn done(summary: impl Into<String>, mut v: Value) -> Out {
    v["summary"] = Value::String(summary.into());
    Ok(v)
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("bad arguments: {e}"))
}

/// One tool call (or resource read): who, and how to tell them it's still
/// going.
pub struct Call<'a> {
    app: &'a Arc<App>,
    caller: Caller,
    /// The client's name (`claude-code`), for "started by mcp:<client>".
    client: String,
    peer: Peer<RoleServer>,
    progress: Option<ProgressToken>,
    ctx: RequestContext<RoleServer>,
}

impl<'a> Call<'a> {
    pub fn new(app: &'a Arc<App>, caller: Caller, client: String, ctx: &RequestContext<RoleServer>) -> Self {
        Self { app, caller, client, peer: ctx.peer.clone(), progress: ctx.meta.get_progress_token(), ctx: ctx.clone() }
    }

    fn by(&self) -> String {
        format!("mcp:{}", self.client)
    }

    fn driver(&self) -> Driver {
        Driver { who: self.by(), name: self.by() }
    }

    pub async fn dispatch(&self, name: &str, args: Value) -> CallToolResult {
        let fail = |e: String| CallToolResult::error(vec![ContentBlock::text(e)]);
        let labs = illogical_proto::hosts::labs(self.app.control.state_dir());
        let (def, kind, args) = match route(name, args, labs) {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        let name = def.name;
        if self.caller.scope == Scope::Read && !def.read_only {
            return fail(format!("this token may only read; {name} changes things (ask for a full token)"));
        }
        let out = match (name, kind) {
            ("run", _) => match parse(args) {
                Ok(a) => self.run(a).await,
                Err(e) => Err(e),
            },
            ("send_input", _) => match parse(args) {
                Ok(a) => self.send_input(a).await,
                Err(e) => Err(e),
            },
            ("attach", _) => match parse(args) {
                Ok(a) => self.attach(a).await,
                Err(e) => Err(e),
            },
            ("read_output", _) => match parse(args) {
                Ok(a) => self.read_output(a).await,
                Err(e) => Err(e),
            },
            ("device_call", _) => match parse(args) {
                Ok(a) => self.device_call(a).await,
                Err(e) => Err(e),
            },
            ("wait", _) => match parse(args) {
                Ok(a) => self.wait(a).await,
                Err(e) => Err(e),
            },
            ("close", _) => match parse(args) {
                Ok(a) => self.close(a).await,
                Err(e) => Err(e),
            },
            ("read_thread", _) => match parse(args) {
                Ok(a) => self.read_thread(a).await,
                Err(e) => Err(e),
            },
            ("post_thread", _) => match parse(args) {
                Ok(a) => self.post_thread(a).await,
                Err(e) => Err(e),
            },
            ("history", _) => match parse(args) {
                Ok(a) => self.history_or_output(a).await,
                Err(e) => Err(e),
            },
            ("start_agent", _) => match parse(args) {
                Ok(a) => self.start_agent(a).await,
                Err(e) => Err(e),
            },
            ("prompt_agent", _) => match parse(args) {
                Ok(a) => self.prompt_agent(a).await,
                Err(e) => Err(e),
            },
            ("agent_respond", _) => match parse(args) {
                Ok(a) => self.respond(a).await,
                Err(e) => Err(e),
            },
            ("read_file", _) => match parse(args) {
                Ok(a) => self.read_file(a).await,
                Err(e) => Err(e),
            },
            ("read_forge", _) => match parse(args) {
                Ok(a) => self.read_forge(a).await,
                Err(e) => Err(e),
            },
            ("invite_person", _) => match parse(args) {
                Ok(a) => self.invite_person(a).await,
                Err(e) => Err(e),
            },
            ("read_invite", _) => match parse(args) {
                Ok(a) => self.read_invite(a).await,
                Err(e) => Err(e),
            },
            // list
            ("list", Some("panes")) => self.list().await,
            ("list", Some("conversations")) => match parse(args) {
                Ok(a) => self.list_conversations(a).await,
                Err(e) => Err(e),
            },
            ("list", Some("devices")) => self.list_devices(),
            ("list", Some("fountain_agents")) => match parse(args) {
                Ok(a) => self.list_agents(a).await,
                Err(e) => Err(e),
            },
            ("list", Some("fountain_agent")) => match parse(args) {
                Ok(a) => self.read_agent(a).await,
                Err(e) => Err(e),
            },
            // show
            ("show", Some("port")) => match parse(args) {
                Ok(a) => self.open_port(a).await,
                Err(e) => Err(e),
            },
            ("show", Some("changes")) => match parse(args) {
                Ok(a) => self.show_changes(a).await,
                Err(e) => Err(e),
            },
            ("show", Some("file")) => match parse(args) {
                Ok(a) => self.show_file(a).await,
                Err(e) => Err(e),
            },
            ("show", Some("pr")) => match parse::<OpenPrArgs>(args) {
                Ok(a) => self.open_forge(json!({ "pr": a.pr }), a.dir, a.beside).await,
                Err(e) => Err(e),
            },
            ("show", Some("issue")) => match parse::<OpenIssueArgs>(args) {
                Ok(a) => self.open_forge(json!({ "issue": a.issue }), a.dir, a.beside).await,
                Err(e) => Err(e),
            },
            ("show", Some("conversation")) => match parse(args) {
                Ok(a) => self.open_conversation(a).await,
                Err(e) => Err(e),
            },
            ("show", Some("app")) => match parse(args) {
                Ok(a) => self.open_app(a).await,
                Err(e) => Err(e),
            },
            ("show", Some("workspace")) => match parse(args) {
                Ok(a) => self.open_workspace(a).await,
                Err(e) => Err(e),
            },
            ("show", Some("fountain")) => match parse(args) {
                Ok(a) => self.open_fountain(a).await,
                Err(e) => Err(e),
            },
            // draft
            ("draft", Some("comment")) => match parse::<CommentArgs>(args) {
                Ok(a) => self.pr_write(&a.block, "comment", json!({ "body": a.body })).await,
                Err(e) => Err(e),
            },
            ("draft", Some("review")) => match parse::<PrReviewArgs>(args) {
                Ok(a) => {
                    let event = match a.event {
                        PrReviewEvent::Approve => "approve",
                        PrReviewEvent::RequestChanges => "request_changes",
                        PrReviewEvent::Comment => "comment",
                    };
                    self.pr_write(&a.block, "review", json!({ "event": event, "body": a.body })).await
                }
                Err(e) => Err(e),
            },
            ("draft", Some("merge")) => match parse::<PrMergeArgs>(args) {
                Ok(a) => self.pr_write(&a.block, "merge", json!({ "style": a.style })).await,
                Err(e) => Err(e),
            },
            ("draft", Some("issue")) => match parse::<IssueNewArgs>(args) {
                Ok(a) => {
                    let c = json!({ "issue": "new", "title": a.title, "body": a.body.unwrap_or_default(),
                        "repo": a.repo, "by": self.by(), "agent": true });
                    self.open_forge(c, a.dir, a.beside).await
                }
                Err(e) => Err(e),
            },
            (_, Some(k)) => Err(format!("{name} kind {k} isn't wired up")),
            (_, None) => Err(format!("no tool {name}")),
        };
        match out {
            Ok(v) => CallToolResult::structured(v),
            Err(e) => fail(e),
        }
    }

    fn list_devices(&self) -> Out {
        let devices = self.app.hands.list();
        let summary = match devices.len() {
            0 => "No device lends tools here yet.".to_owned(),
            n => format!(
                "{n} device(s): {}",
                devices.iter().map(|d| d["name"].as_str().unwrap_or("?")).collect::<Vec<_>>().join(", ")
            ),
        };
        done(summary, json!({ "devices": devices }))
    }

    /// `history`: its entries, or with kind output, lines of output
    /// (search before #349). Each takes its own arguments.
    async fn history_or_output(&self, a: HistoryArgs) -> Out {
        if a.kind.as_deref() != Some("output") {
            if a.pattern.is_some() {
                return Err("pattern goes with kind output (lines of output); match filters command lines".into());
            }
            return self.history(a).await;
        }
        let others = [
            ("failed", a.failed),
            ("before", a.before.is_some()),
            ("cwd", a.cwd.is_some()),
            ("match", a.matching.is_some()),
            ("pane", a.pane.is_some()),
        ];
        if let Some((key, _)) = others.iter().find(|(_, set)| *set) {
            return Err(format!("{key} isn't an argument of history kind output: it takes pattern, since?, limit?"));
        }
        let pattern = a.pattern.ok_or("history kind output needs a pattern: a regex for the lines to find")?;
        self.search(SearchArgs { pattern, since: a.since, limit: a.limit }).await
    }

    // ------------------------------------------------------------ helpers

    async fn panes(&self) -> Vec<PaneSummary> {
        self.app.mux.api(Api::Panes).await.unwrap_or_default()
    }

    /// Whether this caller is an agent on a machine (a guest's, say): what
    /// it creates must land on a machine too ([`confine`]).
    async fn on_machine(&self) -> Result<bool, String> {
        match self.me() {
            Some(me) => Ok(self.readable(me).await?.info.host.is_some()),
            None => Ok(false),
        }
    }

    /// The block whose token this is.
    async fn device_call(&self, a: DeviceCallArgs) -> Out {
        let from = format!("{} on {}", self.client, self.app.hosts.name());
        let call = self.app.hands.call(
            &self.app.control,
            a.device.as_deref(),
            &a.tool,
            a.args.unwrap_or_else(|| json!({})),
            &from,
        );
        let limit = Self::limit(a.timeout);
        let Some(r) = self.waiting(&format!("waiting for the device ({})", a.tool), limit, call).await else {
            return Err(format!("no answer from the device within {}s", limit.as_secs()));
        };
        let c = r?;
        let woke = c.woke_ms.map_or(String::new(), |ms| format!(" (woken: back in {:.1}s)", ms as f64 / 1000.0));
        done(
            format!("{} answered {}{woke}", c.name, a.tool),
            json!({ "device": c.device, "name": c.name, "result": c.result, "woke_ms": c.woke_ms, "answer_ms": c.answer_ms }),
        )
    }

    /// Where a tool defaults to when it's given no pane: the caller's own
    /// (an agent block, or the terminal pane `illogical mcp` runs in).
    fn own_pane(&self) -> Option<PaneId> {
        self.caller.pane
    }

    /// `run`'s `split`: a pane, or "self" for the caller's own.
    fn split_pane(&self, split: Option<&PaneArg>) -> Result<Option<PaneId>, String> {
        match split {
            Some(PaneArg::Name(s)) if s.trim().eq_ignore_ascii_case("self") => self.own_pane().map(Some).ok_or_else(|| {
                "split \"self\": this caller has no pane of its own (run illogical mcp inside an illogical pane, or give a pane number)".into()
            }),
            other => other.map(PaneArg::id).transpose(),
        }
    }

    /// #379: the `CLAUDE_CONFIG_DIR` the stdio bridge sent for its client.
    fn claude_config_dir(&self) -> Option<String> {
        let parts = self.ctx.extensions.get::<axum::http::request::Parts>()?;
        let d = illogical_proto::rename::either(illogical_proto::rename::CLAUDE_CONFIG_DIR, |n| parts.headers.get(n))?
            .to_str()
            .ok()?
            .trim();
        (!d.is_empty()).then(|| d.to_owned())
    }

    /// The agent block whose token this is: what confines a caller.
    fn me(&self) -> Option<PaneId> {
        match self.caller.scope {
            Scope::Block(b) => Some(b),
            _ => None,
        }
    }

    /// An agent in a VM of its own (#59) works on that machine: the first
    /// time it starts something there, its machine becomes its tab's (as
    /// "Share machine with tab" does), so what it starts can join it and
    /// the machine stays while they do. Nothing for other callers, or when
    /// the machine isn't the agent's own.
    async fn share_my_machine(&self) {
        let Some(me) = self.me() else { return };
        let _ = self.app.mux.api(|r| Api::ShareMachine(me, r)).await;
    }

    /// An open pane or block, if this caller may see it.
    async fn readable(&self, pane: PaneId) -> Result<PaneSummary, String> {
        let panes = self.panes().await;
        let Some(p) = panes.iter().find(|p| p.info.id == pane).cloned() else {
            return Err(self.gone(pane).await);
        };
        if let Some(me) = self.me() {
            let tab = panes.iter().find(|p| p.info.id == me).map(|p| p.tab);
            if tab != Some(p.tab) {
                return Err(format!("%{pane} is in another tab: this agent's token reaches its own tab only"));
            }
        }
        Ok(p)
    }

    /// An open pane or block this caller may type in, answer for or close:
    /// anything, or for an agent block's token, what it started.
    async fn drivable(&self, pane: PaneId) -> Result<PaneSummary, String> {
        let p = self.readable(pane).await?;
        if let Some(me) = self.me()
            && p.info.started_by.as_ref().and_then(|s| s.block) != Some(me)
        {
            return Err(format!("%{pane} wasn't started by this agent: it may read it, not drive or close it"));
        }
        Ok(p)
    }

    /// Why a pane isn't there, as specifically as history can say.
    async fn gone(&self, pane: PaneId) -> String {
        let store = self.app.mux.store.clone();
        let last = tokio::task::spawn_blocking(move || {
            history::history(
                &store,
                &Filter { pane: Some(pane), kind: Some(HistoryKind::Command), ..Default::default() },
                1,
            )
        })
        .await
        .ok()
        .and_then(|h| h.into_iter().next());
        match last {
            Some(h) => {
                let what = h.text.as_deref().map(|t| format!(" `{}`", one_line(t, 80))).unwrap_or_default();
                let how = match h.exit {
                    Some(code) => format!("exited {code}"),
                    None => "was still running".into(),
                };
                let when = ago(h.ended_ms.unwrap_or(h.started_ms));
                format!("pane %{pane} is gone; its last command{what} {how} {when} (read_output still reads it)")
            }
            None => format!("no pane %{pane} (list shows what's open)"),
        }
    }

    async fn started(&self, pane: PaneId) {
        let by = StartedBy { by: self.by(), block: self.me() };
        self.app.mux.send(crate::mux::Cmd::Api(Api::StartedBy(pane, by)));
    }

    async fn notify(&self, done: f64, total: Option<f64>, message: String) {
        let Some(token) = &self.progress else { return };
        let mut p = ProgressNotificationParam::new(token.clone(), done).with_message(message);
        if let Some(t) = total {
            p = p.with_total(t);
        }
        let _ = self.peer.notify_progress(p).await;
    }

    /// `fut`, with progress every 15s, for at most `limit`: `None` if it
    /// took longer (or the client gave up).
    async fn waiting<T>(&self, what: &str, limit: Duration, fut: impl Future<Output = T>) -> Option<T> {
        let start = Instant::now();
        let every = progress_every();
        let mut tick = tokio::time::interval_at(start + every, every);
        let deadline = tokio::time::sleep(limit);
        tokio::pin!(fut, deadline);
        loop {
            tokio::select! {
                r = &mut fut => return Some(r),
                _ = &mut deadline => return None,
                _ = self.ctx.ct.cancelled() => return None,
                _ = tick.tick() => {
                    let secs = start.elapsed().as_secs_f64();
                    self.notify(secs, Some(limit.as_secs_f64()), format!("{what}: {}s", secs.round())).await;
                }
            }
        }
    }

    fn limit(timeout: Option<f64>) -> Duration {
        timeout
            .filter(|t| t.is_finite() && *t >= 0.0)
            .map(Duration::from_secs_f64)
            .unwrap_or(WAIT_DEFAULT)
            .min(WAIT_MAX)
    }

    /// The end of a pane's output stream now.
    async fn end_of(&self, pane: PaneId) -> u64 {
        match self.app.mux.api(|r| Api::Pane(pane, r)).await.flatten() {
            Some(p) => p.status().end,
            None => PaneLog::open(self.app.mux.store.pane_dir(pane)).map(|l| l.end()).unwrap_or(0),
        }
    }

    /// The last lines of a pane's output between two offsets.
    fn last_lines(&self, pane: PaneId, from: u64, to: Option<u64>) -> String {
        let Ok(log) = PaneLog::open(self.app.mux.store.pane_dir(pane)) else { return String::new() };
        let to = to.unwrap_or(log.end());
        let from = from.max(to.saturating_sub(64 * 1024));
        let Ok((start, bytes)) = log.read_range(from, (to.saturating_sub(from)) as usize) else { return String::new() };
        let _ = start;
        let text = strip(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
        cap_tail(&tail, 4_000)
    }

    // ------------------------------------------------------------ tools

    async fn run(&self, a: RunArgs) -> Out {
        if a.command.as_deref().is_some_and(|c| c.trim().is_empty()) {
            return Err("the command is empty".into());
        }
        let policy = match a.policy.as_deref() {
            None | Some("shell") => None,
            Some("none") => Some(Policy::None),
            Some("rerun") => Some(Policy::Rerun { confirm: false }),
            Some("rerun-ask") => Some(Policy::Rerun { confirm: true }),
            Some(p) => return Err(format!("policy {p}: shell, none, rerun or rerun-ask")),
        };
        let mut req = RunRequest { cwd: a.cwd.clone(), policy, ..Default::default() };
        match self.me() {
            Some(me) => {
                if a.vm || a.vm_tab || a.machine.is_some() || a.session.is_some() {
                    return Err("this agent's token reaches its own tab only: run splits a pane there (no vm, vm_tab, machine or session)".into());
                }
                let split = self.split_pane(a.split.as_ref())?.unwrap_or(me);
                let p = self.readable(split).await?;
                if p.info.host.is_some() {
                    self.share_my_machine().await;
                }
                req.split = Some(split);
                // On the tab's machine, in a VM tab.
                req.join = p.info.host.is_some();
                req.from_pane = Some(split);
            }
            None => {
                req.split = self.split_pane(a.split.as_ref())?;
                req.join = a.join;
                req.vm = a.vm;
                req.vm_tab = a.vm_tab;
                req.image = a.image.clone();
                req.sandbox = a.machine.clone();
                req.session = a.session.clone();
                req.from_pane = req.split;
            }
        }
        confine(self.on_machine().await?, run_lands_on_machine(&req))?;
        let on_vm = a.vm || a.vm_tab || a.machine.is_some() || req.join;
        let pane = match self.app.mux.api(|r| Api::Run(req, r)).await {
            Some(Ok(p)) => p,
            Some(Err(e)) => return Err(e),
            None => return Err("the daemon is shutting down".into()),
        };
        self.started(pane).await;
        let limit = Self::limit(a.timeout);
        let start = Instant::now();
        let place = self.place(pane).await;
        let Some(command) = a.command else {
            return done(format!("Opened a shell in %{pane} ({place})"), json!({ "pane": pane, "state": "started" }));
        };
        // The shell's first prompt (a VM takes a while), then the command.
        let mux = self.app.mux.clone();
        let ready = self
            .waiting(
                if on_vm { "starting the machine" } else { "starting the shell" },
                if on_vm { Duration::from_secs(300) } else { Duration::from_secs(20) },
                async move {
                    loop {
                        if let Some(p) = mux.api(|r| Api::Pane(pane, r)).await.flatten() {
                            let st = p.status();
                            if st.at_prompt || st.exited.is_some() {
                                return st.exited;
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
            )
            .await;
        if let Some(Some(code)) = ready {
            return Err(format!("%{pane}'s shell ended (exit {code:?}) before the command could be typed"));
        }
        let Some(p) = self.app.mux.api(|r| Api::Pane(pane, r)).await.flatten() else {
            return Err(self.gone(pane).await);
        };
        // Whatever ends after this is the command's.
        p.mark_input();
        let mut line = command.clone().into_bytes();
        line.push(b'\r');
        self.app.mux.send(crate::mux::Cmd::Api(Api::InputBy(pane, line, self.by())));
        if !a.wait {
            return done(
                format!(
                    "Started `{}` in %{pane} ({place}). Follow it with wait (until command_end) or read_output.",
                    one_line(&command, 80)
                ),
                json!({ "pane": pane, "state": "started" }),
            );
        }
        let left = limit.saturating_sub(start.elapsed());
        self.wait_command(pane, left, &command).await
    }

    /// Where a pane is, in words.
    async fn place(&self, pane: PaneId) -> String {
        let panes = self.panes().await;
        match panes.iter().find(|p| p.info.id == pane) {
            Some(p) => {
                let tab = p.tab_name.clone().unwrap_or_else(|| format!("tab {}", p.tab));
                match p.info.host {
                    Some(m) => format!("{tab} in {}, on machine m{m}", p.session_name),
                    None => format!("{tab} in {}", p.session_name),
                }
            }
            None => "closing".into(),
        }
    }

    async fn wait_command(&self, pane: PaneId, limit: Duration, what: &str) -> Out {
        let app = self.app.clone();
        let r = self
            .waiting(&format!("`{}` running", one_line(what, 40)), limit, async move {
                crate::api::wait_until(&app, pane, "command-end", None).await
            })
            .await;
        match r {
            Some(Ok(r)) => self.waited(pane, r).await,
            Some(Err(e)) => Err(e),
            None => self.still_running(pane, limit).await,
        }
    }

    /// Nothing yet: what it's doing, and where to pick up. `until`: what
    /// was waited for, when it isn't a command ending.
    async fn still_running(&self, pane: PaneId, waited: Duration) -> Out {
        self.not_yet(pane, waited, None).await
    }

    async fn not_yet(&self, pane: PaneId, waited: Duration, until: Option<&str>) -> Out {
        let p = self.app.mux.api(|r| Api::Pane(pane, r)).await.flatten();
        let status = p.map(|p| p.status());
        let current = status.as_ref().and_then(|s| s.current.clone());
        let end = self.end_of(pane).await;
        let from = current.as_ref().map(|c| c.start).unwrap_or(end.saturating_sub(4096));
        let what = current
            .as_ref()
            .and_then(|c| c.text.clone())
            .map(|t| format!(" `{}`", one_line(&t, 60)))
            .unwrap_or_default();
        let secs = waited.as_secs();
        let then = format!("call wait again (or read_output with offset {end} for what's new)");
        let summary = match (until, current.is_some()) {
            (None, true) => format!("%{pane} is still running{what} after {secs}s: {then}"),
            (u, _) => format!("%{pane}: no {} yet after {secs}s: {then}", u.unwrap_or("command end")),
        };
        done(
            summary,
            json!({
                "pane": pane,
                "state": "still running",
                "command": current.and_then(|c| c.text),
                "next_offset": end,
                "last_lines": self.last_lines(pane, from, None),
            }),
        )
    }

    async fn waited(&self, pane: PaneId, r: WaitResult) -> Out {
        match r {
            WaitResult::CommandEnd { text, exit, start, end } => {
                let what =
                    text.as_deref().map(|t| format!("`{}`", one_line(t, 60))).unwrap_or_else(|| "the command".into());
                let how = match exit {
                    Some(0) => "succeeded".to_owned(),
                    Some(c) => format!("failed (exit {c})"),
                    None => "ended".to_owned(),
                };
                let next = match end {
                    Some(e) => e,
                    None => self.end_of(pane).await,
                };
                done(
                    format!("{what} in %{pane} {how}; read_output with last_command for all of its output"),
                    json!({
                        "pane": pane,
                        "state": "done",
                        "command": text,
                        "exit": exit,
                        "output_offset": start,
                        "next_offset": next,
                        "last_lines": self.last_lines(pane, start, end),
                    }),
                )
            }
            WaitResult::Exit { code } => done(
                format!("%{pane}'s process exited{}", code.map(|c| format!(" {c}")).unwrap_or_default()),
                json!({ "pane": pane, "state": "exited", "exit": code, "last_lines": self.last_lines(pane, 0, None) }),
            ),
            WaitResult::Match { text, offset } => done(
                format!("%{pane}'s output matched: {}", one_line(&text, 80)),
                json!({ "pane": pane, "state": "matched", "match": text, "offset": offset, "next_offset": self.end_of(pane).await }),
            ),
            WaitResult::Attention { state, ask } => {
                let summary = match (&state, &ask) {
                    (Attention::NeedsInput, Some(a)) => {
                        format!("%{pane} asks: {}. Answer with agent_respond (or leave it for the user)", a.headline())
                    }
                    (Attention::NeedsInput, None) => format!("%{pane} needs input"),
                    (Attention::Done, _) => format!("%{pane} finished"),
                    _ => format!("%{pane} is idle"),
                };
                done(summary, json!({ "pane": pane, "state": state, "ask": ask }))
            }
            WaitResult::Timeout => self.still_running(pane, Duration::ZERO).await,
        }
    }

    /// M71: a file into a terminal (M70's upload and paste) or an agent
    /// block (its prompt's file), under the checks the routes make.
    #[cfg(unix)]
    async fn attach(&self, a: AttachArgs) -> Out {
        use base64::Engine;
        const MAX: u64 = 20 << 20;
        let pane = a.pane.id()?;
        let p = self.drivable(pane).await?;
        if !matches!(p.info.kind, BlockType::Terminal | BlockType::Agent) {
            return Err("attach puts a file into a terminal or an agent block".into());
        }
        let (bytes, ext) = match (a.path, a.data) {
            (Some(path), None) => {
                // Its paths are on its machine, not here.
                if let Some(me) = self.me()
                    && self.app.mux.api(|r| Api::MachineOf(me, r)).await.flatten().is_some()
                {
                    return Err("this agent runs on a machine of its own: send the file as data".into());
                }
                let size = tokio::fs::metadata(&path).await.map_err(|e| format!("{path}: {e}"))?.len();
                if size > MAX {
                    return Err("a file can be 20 MB at most".into());
                }
                let bytes = tokio::fs::read(&path).await.map_err(|e| format!("{path}: {e}"))?;
                let ext = std::path::Path::new(&path).extension().map(|e| e.to_string_lossy().into_owned());
                (bytes, ext)
            }
            (None, Some(data)) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data.trim())
                    .map_err(|e| format!("data isn't base64: {e}"))?;
                (bytes, a.ext)
            }
            _ => return Err("give path or data".into()),
        };
        let ext = match crate::agent::images::sniff(&bytes) {
            Some(m) => m.trim_start_matches("image/").replace("jpeg", "jpg"),
            None => ext.unwrap_or_default(),
        };
        let path = crate::upload::store_whole(self.app, pane, &ext, bytes).await.map_err(|e| e.1)?;
        let by = self.by();
        if p.info.kind == BlockType::Agent {
            let b =
                self.app.mux.api(|r| Api::Block(pane, r)).await.flatten().ok_or_else(|| format!("no block %{pane}"))?;
            b.call_by("send", json!({ "text": a.text.unwrap_or_default(), "files": [path] }), Some(&by)).await?;
            return done(format!("Sent %{pane} a prompt with the file"), json!({ "pane": pane }));
        }
        let out =
            crate::upload::paste_into(self.app, pane, vec![path.clone()], a.force, Some(&by)).await.map_err(|e| e.1)?;
        if out["pasted"] != true {
            let front = out["front"].as_str().unwrap_or("something");
            return Err(format!(
                "%{pane} is running {front}, which wouldn't read a path; it's at {path} (force pastes it anyway)"
            ));
        }
        done(format!("Pasted {path} into %{pane}"), json!({ "pane": pane, "path": path }))
    }

    #[cfg(not(unix))]
    async fn attach(&self, a: AttachArgs) -> Out {
        let _ = (a.pane, a.path, a.data, a.ext, a.text, a.force);
        Err("files can't be attached on this host yet".into())
    }

    async fn send_input(&self, a: SendArgs) -> Out {
        let pane = a.pane.id()?;
        let p = self.drivable(pane).await?;
        if a.text.is_none() && a.keys.is_empty() {
            return Err("give text, keys, or both".into());
        }
        if p.info.kind != BlockType::Terminal {
            let b =
                self.app.mux.api(|r| Api::Block(pane, r)).await.flatten().ok_or_else(|| format!("no block %{pane}"))?;
            let text = a.text.ok_or("an agent block takes text (its next prompt), not keys")?;
            if p.info.kind == BlockType::App {
                let out = b.call_by("send", json!({ "text": text, "tab": a.tab }), Some(&self.by())).await?;
                let tab = out["tab"].as_str().unwrap_or("its tab");
                let queued = match out["position"].as_u64() {
                    Some(n) if n > 0 => format!(", queued {n} behind a running turn"),
                    _ => String::new(),
                };
                return done(
                    format!("Prompted %{pane}'s agent in {tab}{queued}; wait until needs_input for its questions"),
                    json!({ "pane": pane, "tab": tab, "chat": out["chat"], "position": out["position"] }),
                );
            }
            b.call_by("send", json!({ "text": text }), Some(&self.by())).await?;
            return done(format!("Sent %{pane} a prompt"), json!({ "pane": pane }));
        }
        let handle =
            self.app.mux.api(|r| Api::Pane(pane, r)).await.flatten().ok_or_else(|| format!("no pane %{pane}"))?;
        let mut data = a.text.clone().unwrap_or_default().into_bytes();
        if a.text.is_some() && a.enter.unwrap_or(true) {
            data.push(b'\r');
        }
        let modes = handle.status().modes;
        for k in &a.keys {
            let bytes = crate::keys::key(k, modes);
            if bytes.is_empty() {
                return Err(format!("unknown key {k:?}: try C-c, Up, Enter, Escape, Tab, F5, M-x"));
            }
            data.extend(bytes);
        }
        handle.mark_input();
        let offset = handle.status().end;
        self.app.mux.send(crate::mux::Cmd::Api(Api::InputBy(pane, data, self.by())));
        done(
            format!("Typed into %{pane}; its output from here starts at offset {offset}"),
            json!({ "pane": pane, "next_offset": offset }),
        )
    }

    async fn read_output(&self, a: ReadArgs) -> Out {
        if a.screen {
            return self.capture(PaneOnly { pane: a.pane }).await;
        }
        let pane = a.pane.id()?;
        let max = a.max_chars.unwrap_or(PAGE).clamp(200, PAGE_MAX);
        // Open, or closed and still on disk (a full token only: a closed
        // pane's tab is unknown).
        let info = match self.readable(pane).await {
            Ok(p) => Some(p),
            Err(e) if self.me().is_some() => return Err(e),
            Err(_) => None,
        };
        if let Some(p) = &info
            && p.info.kind != BlockType::Terminal
        {
            let b =
                self.app.mux.api(|r| Api::Block(pane, r)).await.flatten().ok_or_else(|| format!("no block %{pane}"))?;
            return page_text(pane, &b.text(), a.offset, max);
        }
        let closed = || {
            self.app
                .mux
                .store
                .pane_dirs()
                .into_iter()
                .find(|(p, _, _)| *p == pane)
                .map(|(_, _, d)| d)
                .ok_or_else(|| format!("no pane %{pane}, open or closed"))
        };
        let status = self.app.mux.api(|r| Api::Pane(pane, r)).await.flatten().map(|p| p.status());
        // A pane closing as this runs has its directory moved to closed/
        // between the lookup and the open: read it from there.
        let log = match info {
            Some(_) => match PaneLog::open(self.app.mux.store.pane_dir(pane)) {
                Ok(log) => Ok(log),
                Err(_) => PaneLog::open(closed()?),
            },
            None => PaneLog::open(closed()?),
        }
        .map_err(|e| format!("can't read %{pane}'s output: {e}"))?;
        let mut command = None;
        let (from, until) = if a.last_command {
            let st =
                status.as_ref().ok_or("a closed pane's commands: use history, then read_output with its offset")?;
            let c = st.current.clone().or(st.last.clone()).ok_or("no command recorded in that pane yet")?;
            command = Some(json!({ "text": c.text, "exit": c.exit, "running": st.current.is_some() }));
            (Some(a.offset.unwrap_or(c.start)), if st.current.is_some() { None } else { c.end })
        } else {
            (a.offset, None)
        };
        let end = until.unwrap_or(log.end()).min(log.end());
        let (start, text, next) = match from {
            Some(from) => {
                let from = from.clamp(log.start(), end);
                let (_, raw) = log
                    .read_range(from, ((end - from) as usize).min(max * 4).min(256 * 1024))
                    .map_err(|e| format!("can't read %{pane}'s output: {e}"))?;
                let (text, used) = head_page(&raw, max);
                (from, text, from + used as u64)
            }
            None => {
                let from = end.saturating_sub((max * 4).min(256 * 1024) as u64).max(log.start());
                let (_, raw) = log
                    .read_range(from, (end - from) as usize)
                    .map_err(|e| format!("can't read %{pane}'s output: {e}"))?;
                let (text, skipped) = tail_page(&raw, max);
                (from + skipped as u64, text, end)
            }
        };
        let more = next < end;
        let summary = if more {
            format!("%{pane}: {} characters from offset {start}; more: call again with offset {next}", text.len())
        } else if until.is_some() {
            format!("%{pane}: {} characters, to the end of its last command", text.len())
        } else {
            format!("%{pane}: {} characters, up to now (offset {next}); later output starts there", text.len())
        };
        done(
            summary,
            json!({
                "pane": pane,
                "offset": start,
                "next_offset": next,
                "more": more,
                "command": command,
                "text": text,
            }),
        )
    }

    async fn capture(&self, a: PaneOnly) -> Out {
        let pane = a.pane.id()?;
        let p = self.readable(pane).await?;
        let text = if p.info.kind == BlockType::Terminal {
            let h =
                self.app.mux.api(|r| Api::Pane(pane, r)).await.flatten().ok_or_else(|| format!("no pane %{pane}"))?;
            tokio::task::spawn_blocking(move || h.capture(CaptureFormat::Text, CaptureScope::Screen))
                .await
                .ok()
                .flatten()
                .ok_or("the pane didn't answer")?
        } else {
            let b =
                self.app.mux.api(|r| Api::Block(pane, r)).await.flatten().ok_or_else(|| format!("no block %{pane}"))?;
            b.text()
        };
        let text = cap_tail(text.trim_end(), PAGE_MAX);
        done(format!("%{pane}'s screen ({} lines)", text.lines().count()), json!({ "pane": pane, "text": text }))
    }

    async fn wait(&self, a: WaitArgs) -> Out {
        let pane = a.pane.id()?;
        let p = self.readable(pane).await?;
        let until = match a.until {
            Until::CommandEnd => "command-end",
            Until::Exit => "exit",
            Until::Match => "match",
            Until::Idle => "idle",
            Until::NeedsInput => "needs-input",
        };
        if p.info.kind != BlockType::Terminal && !matches!(until, "idle" | "needs-input") {
            return Err(format!("%{pane} is an agent or browser block: wait until idle or needs_input"));
        }
        if until == "match" && a.pattern.is_none() {
            return Err("until match needs a pattern".into());
        }
        let limit = Self::limit(a.timeout);
        let app = self.app.clone();
        let pattern = a.pattern.clone();
        let what = match until {
            "needs-input" => format!("waiting for %{pane} to ask"),
            "idle" => format!("waiting for %{pane} to finish"),
            _ => format!("waiting for %{pane}"),
        };
        let r =
            self.waiting(&what, limit, async move { crate::api::wait_until(&app, pane, until, pattern).await }).await;
        match r {
            Some(Ok(r)) => self.waited(pane, r).await,
            Some(Err(e)) if e.contains("closed") => Err(self.gone(pane).await),
            Some(Err(e)) => Err(e),
            None => {
                let label = match until {
                    "command-end" => None,
                    "exit" => Some("exit"),
                    "match" => Some("match"),
                    "idle" => Some("end of its turn"),
                    _ => Some("question"),
                };
                self.not_yet(pane, limit, label).await
            }
        }
    }

    async fn list(&self) -> Out {
        let panes = self.panes().await;
        let tab = self.me().and_then(|me| panes.iter().find(|p| p.info.id == me).map(|p| p.tab));
        let own = self.own_pane();
        let entries: Vec<Value> = panes
            .iter()
            .filter(|p| tab.is_none_or(|t| p.tab == t))
            .take(300)
            .map(|p| {
                let mut e = entry(p);
                if own == Some(p.info.id) {
                    e["you"] = json!(true);
                }
                e
            })
            .collect();
        let needs: Vec<String> = panes
            .iter()
            .filter(|p| tab.is_none_or(|t| p.tab == t) && p.info.attention == Attention::NeedsInput)
            .map(|p| format!("%{}", p.info.id))
            .collect();
        let mut summary = match tab {
            Some(_) => format!("{} panes and blocks in this agent's tab", entries.len()),
            None => format!("{} panes and blocks", entries.len()),
        };
        if !needs.is_empty() {
            summary.push_str(&format!("; {} need input", needs.join(", ")));
        }
        done(summary, json!({ "panes": entries }))
    }

    async fn close(&self, a: PaneOnly) -> Out {
        let pane = a.pane.id()?;
        // An invite block (#234) is the owner's to close, never an agent's.
        if self.drivable(pane).await?.info.kind == BlockType::Invite {
            return Err(crate::invite::CLOSE_OWNER_ONLY.into());
        }
        match self.app.mux.api(|r| Api::Close(pane, r)).await {
            Some(true) => done(format!("Closed %{pane}"), json!({ "pane": pane })),
            _ => Err(self.gone(pane).await),
        }
    }

    /// Panes this caller may read the history of: all, or its tab's.
    async fn tab_panes(&self) -> Option<Vec<PaneId>> {
        let me = self.me()?;
        let panes = self.panes().await;
        let tab = panes.iter().find(|p| p.info.id == me).map(|p| p.tab);
        Some(panes.iter().filter(|p| Some(p.tab) == tab).map(|p| p.info.id).collect())
    }

    /// The thread a call names: a pane's or a session's, within what this
    /// caller reaches (an agent block's token: its own tab and session).
    async fn thread_of(&self, pane: Option<&PaneArg>, session: Option<SessionId>) -> Result<ThreadTarget, String> {
        match (pane.map(PaneArg::id).transpose()?, session) {
            (Some(_), Some(_)) => Err("give a pane or a session, not both".into()),
            (Some(p), None) => self.readable(p).await.map(|_| ThreadTarget::Pane(p)),
            (None, Some(s)) => {
                if let Some(me) = self.me()
                    && self.readable(me).await?.session != s
                {
                    return Err(format!("session {s} isn't this agent's: its token reaches its own session only"));
                }
                Ok(ThreadTarget::Session(s))
            }
            (None, None) => match self.own_pane() {
                Some(own) => self.readable(own).await.map(|_| ThreadTarget::Pane(own)),
                None => Err("which thread? give a pane or a session".into()),
            },
        }
    }

    async fn read_thread(&self, a: ThreadArgs) -> Out {
        let target = self.thread_of(a.pane.as_ref(), a.session).await?;
        let who = crate::acl::Principal::Owner;
        let msgs = self
            .app
            .mux
            .api(|r| Api::ThreadGet(target, who, r))
            .await
            .ok_or("daemon is shutting down")?
            .map_err(|e| e.1)?;
        let after = a.after.unwrap_or(0);
        let msgs: Vec<_> = msgs.into_iter().filter(|m| m.id > after).collect();
        let last = msgs.last().map(|m| m.id).unwrap_or(after);
        let text: Vec<String> = msgs
            .iter()
            .map(|m| {
                let mut t = format!("#{} {}: {}", m.id, m.name, m.text);
                if let Some(q) = &m.quote {
                    t.push_str(&format!("\n  > (%{}) {}", q.pane, q.text.replace('\n', "\n  > ")));
                }
                t
            })
            .collect();
        done(
            format!(
                "{} message(s) in {}{}",
                msgs.len(),
                target.key(),
                if text.is_empty() { String::new() } else { format!(":\n{}", text.join("\n")) }
            ),
            json!({ "thread": target, "messages": msgs, "last": last }),
        )
    }

    async fn post_thread(&self, a: PostThreadArgs) -> Out {
        let target = self.thread_of(a.pane.as_ref(), a.session).await?;
        let as_agent = Driver { who: self.by(), name: format!("{} (agent)", self.client) };
        let post = crate::mux::ThreadPost {
            target,
            who: crate::acl::Principal::Owner,
            as_agent: Some(as_agent),
            text: a.text,
            quote: None,
        };
        let (msg, _, unreached) =
            self.app.mux.api(|r| Api::ThreadPost(post, r)).await.ok_or("daemon is shutting down")?.map_err(|e| e.1)?;
        done(
            format!("posted #{} in {}", msg.id, target.key()),
            json!({ "thread": target, "message": msg, "unreached": unreached }),
        )
    }

    async fn history(&self, a: HistoryArgs) -> Out {
        let limit = a.limit.unwrap_or(30).clamp(1, 200);
        let since = a.since.as_deref().map(seconds).transpose()?;
        let before = a.before.as_deref().map(seconds).transpose()?;
        let matching = a.matching.as_deref().map(regex::Regex::new).transpose().map_err(|e| format!("match: {e}"))?;
        let pane = a.pane.as_ref().map(PaneArg::id).transpose()?;
        let only = self.tab_panes().await;
        let kind = a
            .kind
            .as_deref()
            .map(|k| {
                HistoryKind::parse(k).ok_or_else(|| {
                    format!("kind: {k:?} isn't command, answer or agent (or output, for lines of output)")
                })
            })
            .transpose()?;
        let filter = Filter {
            pane,
            failed: a.failed,
            kind,
            since_ms: since.map(|s| now_ms().saturating_sub(s * 1000)),
            cwd: a.cwd.clone(),
            matching,
        };
        let store = self.app.mux.store.clone();
        let all =
            tokio::task::spawn_blocking(move || history::history(&store, &filter, 100_000)).await.unwrap_or_default();
        let cut = before.map(|b| now_ms().saturating_sub(b * 1000));
        let mut hits: Vec<HistoryEntry> = all
            .into_iter()
            .filter(|h| cut.is_none_or(|c| h.started_ms <= c))
            .filter(|h| only.as_ref().is_none_or(|o| o.contains(&h.pane)))
            .collect();
        let total = hits.len();
        hits.reverse();
        hits.truncate(limit);
        let entries: Vec<Value> = hits
            .iter()
            .map(|h| {
                json!({
                    "pane": h.pane,
                    "open": h.open,
                    "command": h.text,
                    "cwd": h.cwd,
                    "exit": h.exit,
                    "started_ms": h.started_ms,
                    "started": ago(h.started_ms),
                    "seconds": h.ended_ms.map(|e| e.saturating_sub(h.started_ms) / 1000),
                    "by": h.by,
                    "kind": h.kind,
                    "output_offset": h.start,
                })
            })
            .collect();
        let summary = match (total, a.failed) {
            (0, true) => "No failed commands match".to_owned(),
            (0, false) => "No commands match".to_owned(),
            (n, f) => format!(
                "{n} {}command{} match; the newest {} here (read_output with pane and output_offset for one's output)",
                if f { "failed " } else { "" },
                if n == 1 { "" } else { "s" },
                entries.len()
            ),
        };
        done(summary, json!({ "commands": entries, "total": total }))
    }

    async fn search(&self, a: SearchArgs) -> Out {
        let re = regex::Regex::new(&a.pattern).map_err(|e| format!("pattern: {e}"))?;
        let limit = a.limit.unwrap_or(30).clamp(1, 200);
        let since = a.since.as_deref().map(seconds).transpose()?.map(|s| now_ms().saturating_sub(s * 1000));
        let only = self.tab_panes().await;
        let store = self.app.mux.store.clone();
        let want = if only.is_some() { 2000 } else { limit };
        let hits =
            tokio::task::spawn_blocking(move || history::search(&store, &re, since, want)).await.unwrap_or_default();
        let hits: Vec<Value> = hits
            .into_iter()
            .filter(|h| only.as_ref().is_none_or(|o| o.contains(&h.pane)))
            .take(limit)
            .map(|h| json!({ "pane": h.pane, "open": h.open, "offset": h.offset, "command": h.command, "thread": h.thread, "line": one_line(&h.line, 300) }))
            .collect();
        done(format!("{} matching lines", hits.len()), json!({ "hits": hits }))
    }

    async fn open_port(&self, a: OpenPortArgs) -> Out {
        let beside = match (a.beside.as_ref().map(PaneArg::id).transpose()?, self.own_pane()) {
            (Some(b), _) => Some(b),
            (None, own) => own,
        };
        let host = match beside {
            Some(b) => self.readable(b).await?.info.host,
            None => None,
        };
        let req = OpenRequest {
            kind: BlockType::Browser,
            config: json!({ "port": a.port, "path": a.path }),
            session: None,
            split: beside,
            from_pane: beside,
            vm: false,
            image: None,
            host,
            local: host.is_none(),
        };
        let block = self.open(req).await?;
        let place = match beside {
            Some(b) => format!("beside %{b}"),
            None => "in a new tab".into(),
        };
        done(
            format!("Opened port {} in browser block %{block} {place}", a.port),
            json!({ "block": block, "port": a.port }),
        )
    }

    async fn open_app(&self, a: OpenAppArgs) -> Out {
        let studio = crate::apps::studio::get().ok_or("no studio here")?;
        let Some(name) = a.app else {
            let apps = studio.apps().await?;
            let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
            return done(format!("{} apps: {}", apps.len(), names.join(", ")), json!({ "apps": apps }));
        };
        let beside = match (a.beside.as_ref().map(PaneArg::id).transpose()?, self.own_pane()) {
            (Some(b), _) => Some(b),
            (None, own) => own,
        };
        if let Some(b) = beside {
            self.readable(b).await?;
        }
        let config = crate::api::app_config(&json!({ "app": name })).await?;
        let req = OpenRequest {
            kind: BlockType::App,
            config,
            session: None,
            split: beside,
            from_pane: beside,
            vm: false,
            image: None,
            host: None,
            local: true,
        };
        let block = self.open(req).await?;
        done(format!("Opened app {name} in block %{block}"), json!({ "block": block, "app": name }))
    }

    /// Beside `beside`, or the agent itself; on its machine.
    async fn beside(&self, beside: Option<&PaneArg>) -> Result<(Option<PaneId>, Option<u32>), String> {
        let beside = match (beside.map(PaneArg::id).transpose()?, self.own_pane()) {
            (Some(b), _) => Some(b),
            (None, own) => own,
        };
        let host = match beside {
            Some(b) => self.readable(b).await?.info.host,
            None => None,
        };
        Ok((beside, host))
    }

    async fn show_changes(&self, a: ShowChangesArgs) -> Out {
        let (beside, host) = self.beside(a.beside.as_ref()).await?;
        if beside.is_none() && a.repo.is_none() {
            return Err("say whose changes: beside (a pane in the repository) or repo (a directory in it)".into());
        }
        let req = OpenRequest {
            kind: BlockType::Diff,
            config: json!({ "repo": a.repo, "rev_a": a.rev_a, "rev_b": a.rev_b }),
            session: None,
            split: beside,
            from_pane: beside,
            vm: false,
            image: None,
            host,
            local: host.is_none(),
        };
        let block = self.open(req).await?;
        // It reads in the background: wait for that, briefly.
        let deadline = Instant::now() + Duration::from_secs(30);
        let state = loop {
            let b = self.app.mux.api(|r| Api::Block(block, r)).await.flatten().ok_or("the block closed")?;
            let st = b.state();
            if st["loading"] != true || Instant::now() > deadline {
                break st;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        if let Some(e) = state["error"].as_str() {
            return Err(format!("diff block %{block}: {e}"));
        }
        let files: Vec<Value> = state["files"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| json!({ "path": f["path"], "old": f["old"], "status": f["status"], "add": f["add"], "del": f["del"], "binary": f["binary"], "big": f["big"] }))
            .collect();
        let n = files.len();
        done(
            format!(
                "Diff block %{block}: {n} file{} changed, +{} -{} ({}); read_output on %{block} has the diff",
                if n == 1 { "" } else { "s" },
                state["add"],
                state["del"],
                state["against"].as_str().unwrap_or("")
            ),
            json!({ "block": block, "repo": state["repo"], "files": files }),
        )
    }

    async fn show_file(&self, a: ShowFileArgs) -> Out {
        let (beside, host) = self.beside(a.beside.as_ref()).await?;
        let req = OpenRequest {
            kind: BlockType::File,
            config: json!({ "path": a.path, "line": a.line }),
            session: None,
            split: beside,
            from_pane: beside,
            vm: false,
            image: None,
            host,
            local: host.is_none(),
        };
        let block = self.open(req).await?;
        let at = a.line.map(|l| format!(" at line {l}")).unwrap_or_default();
        done(format!("Showing {}{at} in file block %{block}", a.path), json!({ "block": block }))
    }

    async fn open_workspace(&self, a: OpenWorkspaceArgs) -> Out {
        let (beside, host) = self.beside(a.beside.as_ref()).await?;
        let dir = a.dir.trim_end_matches('/').to_owned();
        if !dir.starts_with('/') && !dir.starts_with("~/") {
            return Err(format!("{dir:?}: give the workspace's directory as an absolute path"));
        }
        let req = OpenRequest {
            kind: BlockType::Workspace,
            config: json!({ "root": dir, "env": a.env.as_deref().unwrap_or("local") }),
            session: None,
            split: beside,
            from_pane: beside,
            vm: false,
            image: None,
            host,
            local: host.is_none(),
        };
        let block = self.open(req).await?;
        // Its first read takes a second or two (four chant processes).
        let deadline = Instant::now() + Duration::from_secs(40);
        let state = loop {
            let b = self.app.mux.api(|r| Api::Block(block, r)).await.flatten().ok_or("the block closed")?;
            let st = b.state();
            if st["updated_ms"].as_u64().is_some_and(|t| t > 0) || Instant::now() > deadline {
                break st;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        if let Some(e) = state["error"].as_str() {
            return Err(format!("workspace block %{block}: {e}"));
        }
        let members: Vec<Value> = state["members"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| json!({ "name": m["name"], "dir": m["dir"], "kind": m["kind"], "errors": m["errors"], "gates": m["gates"] }))
            .collect();
        let gates = state["gates"].clone();
        let waiting = gates.as_array().map_or(0, Vec::len);
        done(
            format!(
                "Workspace block %{block}: {} ({} members{}); read_output on %{block} has the whole of it",
                state["name"].as_str().unwrap_or("workspace"),
                members.len(),
                match waiting {
                    0 => String::new(),
                    1 => ", a gate waits for the user".into(),
                    n => format!(", {n} gates wait for the user"),
                }
            ),
            json!({ "block": block, "root": state["root"], "members": members, "gates": gates }),
        )
    }

    /// The account's agents (M43), read with the user's own login on this
    /// host.
    async fn fountain_agents(&self, profile: Option<&str>) -> Result<crate::fountain::Agents, String> {
        let runner = crate::fountain::local_runner(&self.app.mux.shell_env).await;
        crate::fountain::agents_for(&runner, profile).await
    }

    async fn list_agents(&self, a: ListAgentsArgs) -> Out {
        let mut f = crate::fountain::catalog::Filter::default();
        f.apply(&json!({ "query": a.query, "source": a.source }))?;
        let got = self.fountain_agents(a.profile.as_deref()).await?;
        let (login, agents) = (&got.login, &got.agents);
        let rows = crate::fountain::rows(agents, &f);
        let mut text = match f.describe() {
            d if d.is_empty() => format!("{} agents on {}:\n", agents.len(), login.base_url),
            d => format!("{} of {} agents on {} ({d}):\n", rows.len(), agents.len(), login.base_url),
        };
        for r in &rows {
            text.push_str(&crate::fountain::catalog::line(r));
            text.push('\n');
        }
        let note = crate::fountain::unreadable_note(got.unreadable);
        if let Some(n) = &note {
            text.push_str(n);
            text.push('\n');
        }
        let rows: Vec<Value> = rows
            .iter()
            .map(|c| {
                json!({ "name": c.name, "id": c.id, "runtime": c.runtime, "model": c.model, "source": c.source,
                    "app": c.app, "skills": c.skills, "mcp": c.mcp, "description": c.description })
            })
            .collect();
        done(text, json!({ "total": agents.len(), "agents": rows, "unreadable": got.unreadable }))
    }

    async fn read_agent(&self, a: ReadAgentArgs) -> Out {
        let got = self.fountain_agents(a.profile.as_deref()).await?;
        let agent = crate::fountain::find(&got.agents, &a.name).ok_or_else(|| {
            format!("no agent {:?} on this Fountain account (list kind fountain_agents lists them)", a.name)
        })?;
        let recipe = crate::fountain::recipe(agent);
        done(format!("{} ({}, {})", agent.name, agent.runtime, agent.model), recipe)
    }

    async fn open_fountain(&self, a: OpenFountainArgs) -> Out {
        let (beside, _) = self.beside(a.beside.as_ref()).await?;
        let runner = match a.view.as_deref().unwrap_or("catalog") {
            "catalog" => false,
            "runner" => true,
            v => return Err(format!("view is \"catalog\" or \"runner\", not {v:?}")),
        };
        let mut filter = crate::fountain::catalog::Filter::default();
        filter.apply(&json!({ "query": a.query, "source": a.source }))?;
        let config = if runner {
            json!({ "profile": a.profile, "view": "runner" })
        } else {
            json!({ "profile": a.profile, "view": "catalog", "filter": filter })
        };
        let req = OpenRequest {
            kind: BlockType::Fountain,
            config,
            session: None,
            split: beside,
            from_pane: beside,
            vm: false,
            image: None,
            host: None,
            local: true,
        };
        let block = self.open(req).await?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let b = loop {
            let b = self.app.mux.api(|r| Api::Block(block, r)).await.flatten().ok_or("the block closed")?;
            if b.state()["loading"] != true || Instant::now() > deadline {
                break b;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let st = b.state();
        if let Some(e) = st["error"].as_str() {
            return Err(format!("Fountain block %{block}: {e}"));
        }
        if runner {
            let text = b.text();
            return done(format!("Fountain block %{block}:\n{text}"), json!({ "block": block, "text": text }));
        }
        done(
            format!(
                "Fountain block %{block}: {} of {} agents",
                st["agents"].as_array().map_or(0, Vec::len),
                st["total"]
            ),
            json!({ "block": block, "text": b.text() }),
        )
    }

    /// A forge block (M36's PR, M37's issue or new issue) beside a pane.
    async fn open_forge(&self, mut what: Value, dir: Option<String>, beside: Option<PaneArg>) -> Out {
        let (beside, host) = self.beside(beside.as_ref()).await?;
        // N alone means the repository beside it, if no dir is given.
        let dir = match (&dir, beside) {
            (Some(d), _) => Some(d.clone()),
            (None, Some(b)) if host.is_none() => self.readable(b).await?.info.cwd,
            _ => None,
        };
        what["dir"] = json!(dir);
        let config = crate::forge::open_config(&what).await?;
        let req = OpenRequest {
            kind: BlockType::Forge,
            config,
            session: None,
            split: beside,
            from_pane: beside,
            vm: false,
            image: None,
            host: None,
            local: true,
        };
        let block = self.open(req).await?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let b = loop {
            let b = self.app.mux.api(|r| Api::Block(block, r)).await.flatten().ok_or("the block closed")?;
            if b.state()["loading"] != true || Instant::now() > deadline {
                break b;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let st = b.state();
        let what = if st["kind"] == "issue" { "Issue" } else { "PR" };
        if let Some(e) = st["error"].as_str() {
            return Err(format!("{what} block %{block}: {e}"));
        }
        if st["number"] == 0 {
            return done(
                format!(
                    "Drafted a new issue on {} in %{block}: it waits for the user to send, edit or drop it (read_forge shows what became of it)",
                    st["repo"].as_str().unwrap_or("")
                ),
                json!({ "block": block, "status": "waiting", "text": b.text() }),
            );
        }
        let title = st["pr"]["item"]["title"].as_str().or(st["issue"]["item"]["title"].as_str()).unwrap_or("");
        done(
            format!("{what} block %{block}: {}#{} {title}", st["repo"].as_str().unwrap_or(""), st["number"]),
            json!({ "block": block, "text": b.text(), "wants": st["wants"] }),
        )
    }

    /// A forge block this caller may read.
    async fn forge(&self, block: &PaneArg) -> Result<(PaneId, Arc<dyn crate::block::Block>), String> {
        let id = block.id()?;
        let p = self.readable(id).await?;
        if p.info.kind != BlockType::Forge {
            return Err(format!("%{id} isn't a PR or issue block: show (kind pr or issue) opens one"));
        }
        let b = self.app.mux.api(|r| Api::Block(id, r)).await.flatten().ok_or_else(|| format!("no block %{id}"))?;
        Ok((id, b))
    }

    async fn read_forge(&self, a: ReadForgeArgs) -> Out {
        let (id, b) = self.forge(&a.block).await?;
        let st = b.state();
        let drafts = st["drafts"].clone();
        let what = if st["kind"] == "issue" { "Issue" } else { "PR" };
        done(
            format!("{what} block %{id}: {}#{}", st["repo"].as_str().unwrap_or(""), st["number"]),
            json!({ "block": id, "text": b.text(), "wants": st["wants"], "drafts": drafts, "error": st["error"] }),
        )
    }

    /// An agent's write: a draft on the block, at once.
    async fn pr_write(&self, block: &PaneArg, method: &str, args: Value) -> Out {
        let (id, b) = self.forge(block).await?;
        let out = b.call_by(method, args, Some(&self.by())).await?;
        let draft = out["draft"].as_str().unwrap_or("?").to_owned();
        done(
            format!(
                "Drafted on %{id} as {draft}: it waits for the user to send, edit or drop it (read_forge shows what became of it)"
            ),
            out,
        )
    }

    /// An invite's panes (#234): the caller's own, and where it opens. An
    /// agent's own block, or for a full caller the pane `illogical mcp`
    /// runs in, else the one it names.
    fn invite_panes(&self, pane: Option<&PaneArg>) -> Result<(PaneId, PaneId), String> {
        let pane = pane.map(PaneArg::id).transpose()?;
        let mine = self.own_pane();
        match (mine.or(pane), pane.or(mine)) {
            (Some(from), Some(to)) => Ok((from, to)),
            _ => Err("pane: which pane's session to invite them into (illogical mcp in a pane says its own)".into()),
        }
    }

    /// Whose invite drafts these are: an agent block's, or a full client's
    /// in a pane.
    fn drafter(&self, from: PaneId) -> String {
        match self.me() {
            Some(me) => format!("%{me}"),
            None => format!("{}@%{from}", self.by()),
        }
    }

    /// The invite blocks (#234), with their sessions.
    async fn invite_blocks(&self) -> Vec<(PaneId, illogical_core::SessionId, Arc<dyn crate::block::Block>)> {
        let mut out = vec![];
        for p in self.panes().await.into_iter().filter(|p| p.info.kind == BlockType::Invite) {
            if let Some(b) = self.app.mux.api(|r| Api::Block(p.info.id, r)).await.flatten() {
                out.push((p.info.id, p.session, b));
            }
        }
        out
    }

    async fn invite_person(&self, a: InvitePersonArgs) -> Out {
        let role = match a.role.as_deref().map(str::trim) {
            None | Some("" | "viewer") => illogical_core::Role::Viewer,
            Some("editor") => illogical_core::Role::Editor,
            Some(r) => return Err(format!("role {r}: viewer or editor (an invite never makes an owner)")),
        };
        let note = a.note.trim();
        if note.is_empty() {
            return Err("note: say why you want them there (the user reads it on the card)".into());
        }
        if note.chars().count() > crate::invite::card::NOTE_MAX {
            return Err(format!("note: at most {} characters", crate::invite::card::NOTE_MAX));
        }
        let (from, pane) = self.invite_panes(a.pane.as_ref())?;
        let at = self.readable(pane).await?;
        // The owner's agents ask; one a guest started (or an agent of
        // theirs did) learns nothing of who this machine knows.
        let started = self.started_text().await?;
        // Someone this machine knows, or no card at all.
        let person = crate::invite::resolve(self.app, &a.who, None).map_err(|(_, why)| why)?;
        if crate::invite::is_me(self.app, &person.id) {
            return Err(format!("{} owns this machine", person.name));
        }
        let drafter = self.drafter(from);
        let blocks = self.invite_blocks().await;
        let mine: Vec<_> = blocks.iter().filter(|(_, _, b)| b.config()["drafter"] == drafter.as_str()).collect();
        let waiting: u64 = mine.iter().map(|(_, _, b)| b.state()["waiting"].as_u64().unwrap_or(0)).sum();
        if waiting >= INVITES_WAITING {
            return Err(format!(
                "{waiting} of your invites wait for the user already: read_invite until they answer one"
            ));
        }
        let block = match mine.iter().find(|(_, s, _)| *s == at.session) {
            Some((id, _, b)) => (*id, b.clone()),
            None => {
                // A card of its own beside the pane, on this host; not the
                // agent's to drive or close.
                let req = OpenRequest {
                    kind: BlockType::Invite,
                    config: json!({ "drafter": drafter }),
                    session: None,
                    split: Some(pane),
                    from_pane: Some(pane),
                    vm: false,
                    image: None,
                    host: None,
                    local: true,
                };
                let id = match self.app.mux.api(|r| Api::Open(req, None, r)).await {
                    Some(r) => r?,
                    None => return Err("the daemon is shutting down".into()),
                };
                let by = StartedBy { by: self.by(), block: None };
                self.app.mux.send(crate::mux::Cmd::Api(Api::StartedBy(id, by)));
                let b = self.app.mux.api(|r| Api::Block(id, r)).await.flatten().ok_or("the invite block closed")?;
                (id, b)
            }
        };
        let args = json!({ "who": a.who.trim(), "person": person.id, "name": person.name, "role": role,
            "note": note, "session": at.session, "session_name": at.session_name, "pane": pane, "from": from,
            "started": started });
        let out = block.1.call_by("draft", args, Some(&self.by())).await?;
        let draft = out["draft"].as_str().unwrap_or("?").to_owned();
        done(
            format!(
                "Asked the user to invite {} ({}) into {} at %{pane}: {draft} waits on their card in %{} (read_invite shows what became of it)",
                person.name,
                role.as_str(),
                at.session_name,
                block.0
            ),
            json!({ "draft": draft, "status": "waiting", "block": block.0, "who": person.id, "name": person.name,
                "role": role, "session": at.session, "pane": pane }),
        )
    }

    /// Who started the agent asking (#234), as its card says it: the
    /// owner, or the agent that did. An agent a guest stands behind may
    /// not ask at all.
    async fn started_text(&self) -> Result<String, String> {
        let Some(me) = self.me() else { return Ok("your own client".into()) };
        if self.app.mux.api(|r| Api::GuestBehind(me, r)).await.flatten().is_some() {
            return Err("only the owner's own agents may ask to invite someone".into());
        }
        Ok(match self.readable(me).await?.info.started_by {
            Some(StartedBy { by, block: Some(b) }) => format!("started by {by} in %{b}"),
            Some(StartedBy { by, block: None }) => format!("started by {by}"),
            None => "you started it".into(),
        })
    }

    async fn read_invite(&self, a: ReadInviteArgs) -> Out {
        let drafter = match self.me() {
            Some(me) => format!("%{me}"),
            None => self.drafter(self.invite_panes(a.pane.as_ref())?.0),
        };
        let id = a.draft.trim();
        for (block, _, b) in self.invite_blocks().await {
            if b.config()["drafter"] != drafter.as_str() {
                continue;
            }
            let st = b.state();
            let Some(d) = st["drafts"].as_array().and_then(|ds| ds.iter().find(|d| d["id"] == id)).cloned() else {
                continue;
            };
            let status = d["status"].as_str().unwrap_or("waiting").to_owned();
            let name = d["name"].as_str().unwrap_or("").to_owned();
            let by = d["settled_by"].as_str().unwrap_or("the user");
            let summary = match status.as_str() {
                "waiting" => format!("{id} waits on the user's card in %{block}"),
                "sent" => format!(
                    "{by} invited {name}; their notification: {}{}",
                    d["delivery"].as_str().unwrap_or("?"),
                    d["delivery_reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default()
                ),
                "declined" => match d["reason"].as_str() {
                    Some(r) => format!("{by} declined inviting {name}: {r}"),
                    None => format!("{by} declined inviting {name}"),
                },
                "dropped" => format!("Nobody answered {id} in time: nothing was shared"),
                _ => format!("Inviting {name} failed: {}", d["error"].as_str().unwrap_or("?")),
            };
            let mut v = d;
            v["draft"] = json!(id);
            v["block"] = json!(block);
            return done(summary, v);
        }
        // Its block was closed: what it said then.
        let root = self.app.mux.store.root().to_owned();
        let kept = tokio::task::spawn_blocking(move || crate::invite::card::closed(&root)).await.unwrap_or_default();
        if let Some(c) = kept.into_iter().rev().find(|c| c.drafter == drafter && c.draft.id == id) {
            let mut v = serde_json::to_value(&c.draft).unwrap_or_default();
            v["draft"] = json!(id);
            v["block"] = json!(c.block);
            let status = v["status"].as_str().unwrap_or("").to_owned();
            return done(format!("{id} is {status}; its invite block %{} was closed", c.block), v);
        }
        Err(format!("no invite {id} of yours (invite_person returns one)"))
    }

    async fn open(&self, req: OpenRequest) -> Result<PaneId, String> {
        confine(self.on_machine().await?, open_lands_on_machine(&req))?;
        match self.app.mux.api(|r| Api::Open(req, None, r)).await {
            Some(Ok(b)) => {
                self.started(b).await;
                Ok(b)
            }
            Some(Err(e)) => Err(e),
            None => Err("the daemon is shutting down".into()),
        }
    }

    async fn start_agent(&self, a: StartAgentArgs) -> Out {
        if a.prompt.trim().is_empty() {
            return Err("give the agent a prompt".into());
        }
        let beside = match (a.beside.as_ref().map(PaneArg::id).transpose()?, self.own_pane()) {
            (Some(b), _) => Some(b),
            (None, own) => own,
        };
        if self.me().is_some() && (a.vm || a.session.is_some()) {
            return Err(
                "this agent's token reaches its own tab only: start_agent opens beside it (no vm or session)".into()
            );
        }
        let host = match beside {
            Some(b) => self.readable(b).await?.info.host,
            None => None,
        };
        may_start(self.on_machine().await?, host.is_some(), a.vm, a.as_fountain.is_some())?;
        // An agent doesn't hand another one every check switched off.
        if let Some(m) = a.permission_mode.as_deref().filter(|m| SKIPS_CHECKS.contains(m)) {
            return Err(format!("permission_mode {m} skips every check: only the user starts an agent like that"));
        }
        if let Some(name) = a.as_fountain.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            // M44: it runs on this host with the owner's secrets.
            if !matches!(a.agent, AgentKind::Claude) {
                return Err("as_fountain is for agent claude: a Claude Code wears the Fountain agent".into());
            }
            // Refused up front, with the reason (an orchestrator, a codex agent).
            let runner = crate::fountain::local_runner(&self.app.mux.shell_env).await;
            crate::fountain::wear::find(&runner, None, name).await?;
        }
        if host.is_some() {
            self.share_my_machine().await;
        }
        let agent = match a.agent {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Fountain => "fountain",
            AgentKind::Acp => "acp",
        };
        let mut config = json!({ "agent": agent, "prompt": a.prompt });
        if let Some(c) = &a.command {
            config["command"] = json!(c);
        }
        if let Some(f) = &a.fountain_agent {
            config["fountain_agent"] = json!(f);
        }
        if let Some(f) = a.as_fountain.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            config["as_fountain"] = json!(f);
        }
        if let Some(m) = &a.model {
            config["model"] = json!(m);
        }
        if let Some(c) = &a.cwd {
            config["cwd"] = json!(c);
        }
        // #163: a lead pre-authorizing what it hands out.
        if !a.allow.is_empty() {
            config["allow"] = a.allow.iter().map(|t| json!({ "tool": t })).collect();
        }
        if let Some(m) = &a.permission_mode {
            config["permission_mode"] = json!(m);
        }
        if a.user_settings {
            config["user_settings"] = json!(true);
        }
        // #379: Claude Code's login is the caller's (`illogical mcp` says
        // its client's directory), not the daemon's default one. Without
        // it, a block beside an agent or a pane takes theirs (the mux).
        if matches!(a.agent, AgentKind::Claude | AgentKind::Acp)
            && !a.vm
            && host.is_none()
            && let Some(d) = self.claude_config_dir()
        {
            config["claude_config_dir"] = json!(d);
        }
        let req = OpenRequest {
            kind: BlockType::Agent,
            config,
            session: if beside.is_none() { a.session.clone() } else { None },
            split: beside,
            from_pane: beside,
            vm: a.vm,
            image: None,
            host: if a.vm { None } else { host },
            local: false,
        };
        let block = self.open(req).await?;
        done(
            format!(
                "Started an agent in %{block}; wait on it (until needs_input or idle), then read_output for its transcript"
            ),
            json!({ "block": block }),
        )
    }

    async fn list_conversations(&self, a: ListConversationsArgs) -> Out {
        let q = crate::api::ConversationsQuery {
            all: a.all,
            q: a.query,
            cwd: a.cwd,
            live: a.live,
            limit: Some(a.limit.unwrap_or(30)),
        };
        let mut v = crate::api::list_conversations(self.app, q).await?;
        // The transcript's path is the daemon's business.
        for c in v["conversations"].as_array_mut().into_iter().flatten() {
            if let Some(o) = c.as_object_mut() {
                o.remove("path");
            }
        }
        let n = v["conversations"].as_array().map_or(0, Vec::len);
        done(format!("{n} conversations; show (kind conversation) shows one as a block"), v)
    }

    async fn open_conversation(&self, a: OpenConversationArgs) -> Out {
        // Conversations are this host's, and continue as a Claude Code here.
        confine(self.on_machine().await?, false)?;
        let beside = match (a.beside.as_ref().map(PaneArg::id).transpose()?, self.own_pane()) {
            (Some(b), _) => Some(b),
            (None, own) => own,
        };
        if let Some(b) = beside {
            self.readable(b).await?;
        }
        let req = illogical_proto::api::OpenConversationRequest {
            id: a.id,
            then: a.then.map(|t| match t {
                ConversationThen::Continue => "continue".into(),
                ConversationThen::Fork => "fork".into(),
            }),
            session: None,
            split: beside,
            from_pane: beside,
        };
        let v = crate::api::open_conversation_as(self.app, None, req).await.map_err(|e| e.1)?;
        let block = v.block;
        let summary = match v.error.as_deref() {
            Some(e) => format!("Opened it in %{block}, but: {e}"),
            None => format!("It's in %{block}; send_input to it to go on, then wait and read_output"),
        };
        done(summary, serde_json::to_value(&v).unwrap_or_default())
    }

    async fn prompt_agent(&self, a: PromptArgs) -> Out {
        use illogical_proto::api::PromptResult;
        let pane = a.pane.id()?;
        self.drivable(pane).await?;
        let limit = Self::limit(a.timeout);
        let app = self.app.clone();
        let by = self.by();
        let fut = async move { crate::api::prompt(&app, pane, a.text, a.answering, crate::api::STALL, Some(by)).await };
        let r = self.waiting(&format!("waiting for %{pane}'s turn"), limit, fut).await;
        let r = match r {
            Some(r) => r?,
            None => return self.not_yet(pane, limit, Some("end of its turn")).await,
        };
        let summary = match &r {
            PromptResult::Done => {
                format!("%{pane} finished its turn; read_output (screen: true for what it shows) for what it said")
            }
            PromptResult::NeedsInput { question, .. } => {
                format!("%{pane} asks: {}; agent_respond answers it", question.as_deref().unwrap_or("for someone"))
            }
            PromptResult::Blocked { question, .. } => format!(
                "%{pane} was already waiting on someone ({}), so nothing was typed; agent_respond answers it, or pass answering",
                question.as_deref().unwrap_or("a question")
            ),
            PromptResult::Stalled { why, .. } => format!("%{pane} stalled: {why}"),
            PromptResult::StillRunning => format!("%{pane} is still working: wait until idle"),
        };
        let mut v = serde_json::to_value(&r).map_err(|e| e.to_string())?;
        v["pane"] = json!(pane);
        done(summary, v)
    }

    async fn respond(&self, a: RespondArgs) -> Out {
        use illogical_proto::Action;
        let pane = a.pane.id()?;
        self.drivable(pane).await?;
        let (action, content) = match a.action {
            Response::Allow => (Action::Allow, None),
            Response::Deny | Response::Skip => (Action::Deny, None),
            Response::Answer => {
                let answers = a.answers.clone().ok_or("answer needs answers: the question's fields, by name")?;
                (Action::Answer, Some(Value::Object(answers)))
            }
        };
        if let Some(o) = a.option.as_deref()
            && !matches!(o, "once" | "always")
        {
            return Err(format!("option {o}: once or always"));
        }
        let req = ActRequest {
            action,
            pane: Some(pane),
            panes: vec![],
            id: a.id.clone(),
            content,
            option: a.option.clone(),
            suggestion: None,
            message: a.message.clone(),
            text: None,
        };
        crate::api::act_as(self.app, pane, &req, self.driver()).await?;
        let did = match a.action {
            Response::Allow => "Allowed",
            Response::Deny => "Denied",
            Response::Answer => "Answered",
            Response::Skip => "Skipped",
        };
        done(format!("{did} what %{pane} asked"), json!({ "pane": pane }))
    }

    async fn read_file(&self, a: ReadFileArgs) -> Out {
        let max = a.max_chars.unwrap_or(PAGE).clamp(200, PAGE_MAX);
        let pane = match (a.pane.as_ref().map(PaneArg::id).transpose()?, self.own_pane()) {
            (Some(p), _) => Some(p),
            (None, own) => own,
        };
        let mut path = a.path.clone();
        if let Some(p) = pane {
            let info = self.readable(p).await?;
            if !path.starts_with('/')
                && !path.starts_with('~')
                && let Some(cwd) = info.info.cwd.as_deref()
            {
                path = format!("{}/{path}", cwd.trim_end_matches('/'));
            }
        }
        let offset = a.offset.unwrap_or(0);
        let (bytes, size) = crate::fs::read_on(self.app, pane, &path, offset, max as u64).await?;
        if bytes.iter().take(8000).any(|b| *b == 0) {
            return Err(format!("{path} looks binary ({size} bytes); read_file reads text"));
        }
        // Whole characters only: a page may end mid-character.
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        let mut used = bytes.len();
        if text.ends_with('\u{fffd}') && (offset + bytes.len() as u64) < size {
            let cut = text.char_indices().last().map(|(i, _)| i).unwrap_or(0);
            text.truncate(cut);
            used = text.len().min(used);
        }
        let next = offset + used as u64;
        let more = next < size;
        let summary = if more {
            format!("{path}: bytes {offset}–{next} of {size}; more: call again with offset {next}")
        } else {
            format!("{path}: {size} bytes, to the end")
        };
        done(
            summary,
            json!({ "path": path, "size": size, "offset": offset, "next_offset": next, "more": more, "text": text }),
        )
    }

    // ------------------------------------------------------------ resources

    /// A resource's text: `illogical://history`, `illogical://pane/N/output`,
    /// `illogical://pane/N/screen` or `illogical://block/N`.
    pub async fn resource(&self, uri: &str) -> Result<String, String> {
        let rest = uri.strip_prefix("illogical://").ok_or_else(|| format!("not an illogical resource: {uri}"))?;
        let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
        let pane_of = |s: &str| PaneArg::Name(s.to_owned()).id();
        let out = match parts.as_slice() {
            ["history"] => {
                self.history(HistoryArgs {
                    failed: false,
                    kind: None,
                    pattern: None,
                    since: None,
                    before: None,
                    cwd: None,
                    matching: None,
                    pane: None,
                    limit: Some(50),
                })
                .await?
            }
            ["pane", id, "output"] => {
                let v = self
                    .read_output(ReadArgs {
                        pane: PaneArg::Id(pane_of(id)?),
                        offset: None,
                        last_command: false,
                        max_chars: None,
                        screen: false,
                    })
                    .await?;
                return Ok(v["text"].as_str().unwrap_or_default().to_owned());
            }
            ["pane", id, "screen"] => {
                let v = self.capture(PaneOnly { pane: PaneArg::Id(pane_of(id)?) }).await?;
                return Ok(v["text"].as_str().unwrap_or_default().to_owned());
            }
            ["block", id] => {
                let id = pane_of(id)?;
                let p = self.readable(id).await?;
                let state = match self.app.mux.api(|r| Api::Block(id, r)).await.flatten() {
                    Some(b) => b.state(),
                    None => Value::Null,
                };
                json!({ "pane": entry(&p), "state": state })
            }
            _ => return Err(format!("no such resource: {uri}")),
        };
        Ok(serde_json::to_string_pretty(&out).unwrap_or_default())
    }
}

/// A pane in `list`.
fn entry(p: &PaneSummary) -> Value {
    let i = &p.info;
    json!({
        "pane": i.id,
        "type": i.kind,
        "session": p.session_name,
        "tab": p.tab,
        "tab_name": p.tab_name,
        "machine": i.host.map(|m| format!("m{m}")),
        "cwd": i.cwd,
        "command": i.command,
        "title": i.title,
        "running": i.running,
        "attention": i.attention,
        "why": i.reason.as_ref().map(|r| r.headline.clone()),
        "current": i.current.as_ref().and_then(|c| c.text.clone()),
        "last": i.last.as_ref().map(|c| json!({ "command": c.text, "exit": c.exit })),
        "started_by": i.started_by.as_ref().map(|s| s.by.clone()),
    })
}

/// A block's text (an agent's transcript), paged by character.
fn page_text(pane: PaneId, text: &str, offset: Option<u64>, max: usize) -> Out {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    let from = match offset {
        Some(o) => (o as usize).min(total),
        None => total.saturating_sub(max),
    };
    let to = (from + max).min(total);
    let page: String = chars[from..to].iter().collect();
    let more = to < total;
    let summary = if more {
        format!("%{pane}: characters {from}–{to} of {total}; more: call again with offset {to}")
    } else {
        format!("%{pane}: characters {from}–{to}, to the end")
    };
    done(summary, json!({ "pane": pane, "offset": from, "next_offset": to, "more": more, "text": page }))
}

/// The longest start of `raw` whose text fits in `max` characters, cut at a
/// line's end where one is near: (text, raw bytes used).
fn head_page(raw: &[u8], max: usize) -> (String, usize) {
    let all = strip(raw);
    if all.len() <= max {
        return (all, raw.len());
    }
    let (mut lo, mut hi) = (0usize, raw.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if strip(&raw[..mid]).len() <= max { lo = mid } else { hi = mid - 1 }
    }
    let cut = match raw[..lo].iter().rposition(|b| *b == b'\n') {
        Some(nl) if nl + 1 > lo / 2 => nl + 1,
        _ => lo,
    };
    (strip(&raw[..cut]), cut)
}

/// The shortest end of `raw` whose text fits in `max` characters, from a
/// line's start where one is near: (text, raw bytes skipped).
fn tail_page(raw: &[u8], max: usize) -> (String, usize) {
    let all = strip(raw);
    if all.len() <= max {
        return (all, 0);
    }
    let (mut lo, mut hi) = (0usize, raw.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if strip(&raw[mid..]).len() <= max { hi = mid } else { lo = mid + 1 }
    }
    let skip = match raw[lo..].iter().position(|b| *b == b'\n') {
        Some(nl) if nl < (raw.len() - lo) / 2 => lo + nl + 1,
        _ => lo,
    };
    (strip(&raw[skip..]), skip)
}

/// The end of `s`, at most `max` bytes, on a character boundary.
fn cap_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut cut = s.len() - max;
    while !s.is_char_boundary(cut) {
        cut += 1;
    }
    s[cut..].to_owned()
}

fn one_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

/// `90m`, `36h`, `2d`, `45s` or plain seconds.
fn seconds(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = n.parse().map_err(|_| format!("{s:?}: a duration like 90m, 36h, 2d or seconds"))?;
    let mult = match unit.trim() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        u => return Err(format!("{s:?}: unit {u} (use s, m, h or d)")),
    };
    Ok(n * mult)
}

fn ago(ms: u64) -> String {
    let secs = now_ms().saturating_sub(ms) / 1000;
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// The one rule for every tool that creates something (a shell, an agent,
/// a block): an agent on a machine (a guest's, say) creates things on a
/// machine, never on this host, where they'd run as the owner (with the
/// owner's logins and, for a worn Fountain agent, secrets).
fn confine(caller_on_machine: bool, lands_on_machine: bool) -> Result<(), String> {
    if caller_on_machine && !lands_on_machine {
        return Err("an agent on a machine creates things on its machine (or a new VM), not on this host".into());
    }
    Ok(())
}

/// Where `run` puts its shell: a machine when it joins one or makes one.
fn run_lands_on_machine(req: &RunRequest) -> bool {
    req.join || req.vm || req.vm_tab || req.sandbox.is_some()
}

/// Where a block opens: a machine when it has one or makes one.
fn open_lands_on_machine(req: &OpenRequest) -> bool {
    req.vm || req.host.is_some()
}

/// Whether `start_agent` may start this agent ([`confine`]), and a worn
/// Fountain agent on this host only.
fn may_start(caller_on_machine: bool, on_machine: bool, vm: bool, worn: bool) -> Result<(), String> {
    confine(caller_on_machine, on_machine || vm)?;
    if worn && (on_machine || vm) {
        return Err("a worn Fountain agent runs on this host, not on a machine".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn who_may_start_what_where() {
        use super::may_start;
        // The owner's agents (and the CLI), here.
        assert!(may_start(false, false, false, true).is_ok());
        assert!(may_start(false, false, false, false).is_ok());
        assert!(may_start(false, true, false, false).is_ok());
        // A guest's VM agent: on its machine or a VM, never here.
        assert!(may_start(true, true, false, false).is_ok());
        assert!(may_start(true, false, true, false).is_ok());
        assert!(may_start(true, false, false, false).unwrap_err().contains("not on this host"));
        assert!(may_start(true, false, false, true).unwrap_err().contains("not on this host"));
        // A worn agent never goes on a machine.
        assert!(may_start(false, true, false, true).is_err());
        assert!(may_start(false, false, true, true).is_err());
    }

    /// run, open (show's blocks, start_agent's…) and show's conversation
    /// all go through `confine`.
    #[test]
    fn an_agent_on_a_machine_creates_on_machines() {
        use super::*;
        // run: a split of the owner's local pane is a shell here.
        let here = RunRequest { split: Some(3), join: false, ..Default::default() };
        let there = RunRequest { split: Some(4), join: true, ..Default::default() };
        let vm = RunRequest { vm: true, ..Default::default() };
        assert!(confine(true, run_lands_on_machine(&here)).unwrap_err().contains("not on this host"));
        assert!(confine(true, run_lands_on_machine(&there)).is_ok());
        assert!(confine(true, run_lands_on_machine(&vm)).is_ok());
        assert!(confine(false, run_lands_on_machine(&here)).is_ok(), "the owner's agents, as before");
        // open: a block without a machine is this host's.
        let block = |host: Option<u32>, vm: bool| OpenRequest {
            kind: BlockType::Agent,
            config: json!({}),
            session: None,
            split: Some(3),
            from_pane: Some(3),
            vm,
            image: None,
            host,
            local: host.is_none(),
        };
        assert!(confine(true, open_lands_on_machine(&block(None, false))).is_err());
        assert!(confine(true, open_lands_on_machine(&block(Some(2), false))).is_ok());
        assert!(confine(true, open_lands_on_machine(&block(None, true))).is_ok());
        assert!(confine(false, open_lands_on_machine(&block(None, false))).is_ok());
        // show kind conversation: always this host's.
        assert!(confine(true, false).is_err() && confine(false, false).is_ok());
    }

    use super::*;

    #[test]
    fn pages_cut_at_lines_and_fit() {
        let raw: Vec<u8> = (0..2000).flat_map(|i| format!("\x1b[32mline {i:04}\x1b[0m\r\n").into_bytes()).collect();
        let (text, used) = head_page(&raw, 1000);
        assert!(text.len() <= 1000 && text.len() > 900, "{}", text.len());
        assert!(text.starts_with("line 0000\n") && text.ends_with('\n'), "{text:?}");
        let (next, _) = head_page(&raw[used..], 1000);
        assert!(next.starts_with("line "), "the next page starts on a line: {next:?}");
        let (tail, skipped) = tail_page(&raw, 1000);
        assert!(tail.len() <= 1000 && tail.ends_with("line 1999\n"), "{tail:?}");
        assert!(tail.starts_with("line "), "{tail:?}");
        assert_eq!(strip(&raw[skipped..]), tail);
        let (small, used) = head_page(b"hi\r\n", 1000);
        assert_eq!((small.as_str(), used), ("hi\n", 4));
    }

    #[test]
    fn durations_and_panes() {
        assert_eq!(seconds("2d"), Ok(172800));
        assert_eq!(seconds("90m"), Ok(5400));
        assert_eq!(seconds("45"), Ok(45));
        assert!(seconds("2w").is_err());
        assert_eq!(PaneArg::Name("%7".into()).id(), Ok(7));
        assert_eq!(PaneArg::Id(3).id(), Ok(3));
        assert!(PaneArg::Name("x".into()).id().is_err());
    }

    #[test]
    fn annotations_are_honest() {
        let all = list(Scope::Full, false);
        assert_eq!(all.len(), 18);
        let ro: Vec<&str> = all
            .iter()
            .filter(|t| t.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true))
            .map(|t| t.name.as_ref())
            .collect();
        assert_eq!(ro, ["read_output", "wait", "list", "history", "read_forge", "read_invite", "read_file"]);
        assert_eq!(list(Scope::Read, false).len(), ro.len(), "a read token sees the read-only tools only");
        let hint = |n: &str| all.iter().find(|t| t.name == n).unwrap().annotations.clone().unwrap();
        assert_eq!(hint("close").destructive_hint, Some(true));
        // A group is as careful as its most careful kind: draft has merge.
        assert_eq!(hint("draft").destructive_hint, Some(true));
        assert_eq!((hint("show").destructive_hint, hint("show").open_world_hint), (Some(false), Some(true)));
    }

    /// The kinds of a group a tool has, listed with `labs` or without.
    fn kinds_of(labs: bool, tool: &str) -> Vec<&'static str> {
        defs(labs)
            .into_iter()
            .find(|d| d.name == tool)
            .map(|d| match d.args {
                Args::Kinds { kinds, .. } => kinds.iter().map(|k| k.name).collect(),
                Args::One(_) => Vec::new(),
            })
            .unwrap_or_default()
    }

    /// Fountain, studio apps, chant workspaces and chat are for what a
    /// stranger doesn't have: without `labs` chat's tools and the others'
    /// kinds aren't listed (18 tools of 20), with it all are, and a caller
    /// who names one reaches it either way.
    #[test]
    fn unlisted_tools_are_not_listed_without_labs_but_are_there() {
        for scope in [Scope::Full, Scope::Read] {
            let listed: Vec<String> = list(scope, false).iter().map(|t| t.name.to_string()).collect();
            for name in UNLISTED_THREADS {
                assert!(!listed.contains(&name.to_string()), "{name} is listed for {scope:?} without labs");
            }
            let with: Vec<String> = list(scope, true).iter().map(|t| t.name.to_string()).collect();
            for d in all_defs().iter().filter(|d| scope == Scope::Full || d.read_only) {
                assert!(with.contains(&d.name.to_string()), "{} isn't listed for {scope:?} with labs", d.name);
            }
        }
        // A hidden kind is filtered out of its group: not in kind's enum,
        // nor its line in the description, nor the arguments only it takes.
        for (tool, kind) in UNLISTED {
            assert!(!kinds_of(false, tool).contains(&kind), "{tool} kind {kind} is listed without labs");
            assert!(kinds_of(true, tool).contains(&kind), "{tool} kind {kind} isn't listed with labs");
            let t = list(Scope::Full, false).into_iter().find(|t| t.name == tool).unwrap();
            let kinds = t.input_schema["properties"]["kind"]["enum"].clone();
            assert!(!kinds.as_array().unwrap().contains(&json!(kind)), "{tool}: {kinds}");
            assert!(!t.description.as_deref().unwrap().contains(&format!("\n- {kind}")), "{tool}: {kind}");
            // Still there: by its kind, and by its name before #349.
            route(tool, json!({ "kind": kind }), false).unwrap_or_else(|e| panic!("{tool} {kind}: {e}"));
        }
        for old in ["open_app", "open_workspace", "open_fountain", "list_agents", "read_agent"] {
            let (def, kind, _) = route(old, json!({}), false).unwrap_or_else(|e| panic!("{old}: {e}"));
            assert!(UNLISTED.contains(&(def.name, kind.unwrap())), "{old}");
        }
        let show = list(Scope::Full, false).into_iter().find(|t| t.name == "show").unwrap();
        for arg in ["app", "env", "view", "source", "profile"] {
            assert!(show.input_schema["properties"].get(arg).is_none(), "show takes {arg} without labs");
        }
        let list_tool = list(Scope::Full, false).into_iter().find(|t| t.name == "list").unwrap();
        for arg in ["name", "source", "profile"] {
            assert!(list_tool.input_schema["properties"].get(arg).is_none(), "list takes {arg} without labs");
        }
        // An error names only the kinds that are listed.
        let e = route("show", json!({ "kind": "terminal" }), false).err().unwrap();
        assert!(e.contains("port") && !e.contains("fountain") && !e.contains("workspace"), "{e}");
        let every: Vec<&str> = all_defs().iter().map(|d| d.name).collect();
        for name in UNLISTED_THREADS {
            assert!(every.contains(&name), "{name} no longer has a definition");
        }
        assert_eq!(every.len(), 20);
        assert_eq!(defs(false).len(), 18);
        assert_eq!(defs(true).len(), 20);
        assert_eq!(every.len(), defs(false).len() + UNLISTED_THREADS.len());
    }

    /// The 39 tools before #349, by their old names.
    const BEFORE: [&str; 39] = [
        "run",
        "send_input",
        "attach",
        "read_output",
        "capture_screen",
        "wait",
        "list",
        "close",
        "read_thread",
        "post_thread",
        "history",
        "search",
        "open_port",
        "open_app",
        "start_agent",
        "list_conversations",
        "open_conversation",
        "prompt_agent",
        "agent_respond",
        "show_changes",
        "show_file",
        "open_workspace",
        "open_pr",
        "read_pr",
        "pr_comment",
        "pr_review",
        "pr_merge",
        "open_issue",
        "read_issue",
        "issue_comment",
        "issue_new",
        "invite_person",
        "read_invite",
        "list_agents",
        "read_agent",
        "open_fountain",
        "read_file",
        "list_devices",
        "device_call",
    ];

    /// About half as many tools (#349), and every old job is still there:
    /// each old name is a tool now, or reaches the tool and kind that does
    /// its job, with its old arguments; and it stays read-only or not.
    #[test]
    fn every_old_tool_is_a_tool_or_a_kind() {
        let now = all_defs();
        assert!(now.len() * 2 <= BEFORE.len() + 1, "{} tools", now.len());
        let mut jobs = std::collections::BTreeSet::new();
        for old in BEFORE {
            let (def, kind, _) = route(old, json!({}), true).unwrap_or_else(|e| panic!("{old}: {e}"));
            let was_ro = matches!(
                old,
                "read_output"
                    | "capture_screen"
                    | "wait"
                    | "list"
                    | "read_thread"
                    | "history"
                    | "search"
                    | "list_conversations"
                    | "read_pr"
                    | "read_issue"
                    | "read_invite"
                    | "list_agents"
                    | "read_agent"
                    | "read_file"
                    | "list_devices"
            );
            assert_eq!(def.read_only, was_ro, "{old} is now {}, read-only {}", def.name, def.read_only);
            // search is history's kind output, an argument of a tool that
            // does one thing.
            jobs.insert((def.name, kind));
        }
        // Every tool and kind there is now does one of the old jobs.
        let mut all = std::collections::BTreeSet::new();
        for d in &now {
            match &d.args {
                Args::One(_) => {
                    all.insert((d.name, None));
                }
                Args::Kinds { kinds, .. } => all.extend(kinds.iter().map(|k| (d.name, Some(k.name)))),
            }
        }
        assert_eq!(jobs, all);
    }

    /// The old tools' arguments still parse as the kind that does their
    /// job, kind added.
    #[test]
    fn old_calls_reach_their_kind_with_their_arguments() {
        let calls = [
            ("capture_screen", json!({ "pane": 3 }), "read_output", None),
            ("search", json!({ "pattern": "x", "since": "1d", "limit": 5 }), "history", None),
            ("list_conversations", json!({ "query": "q", "live": true }), "list", Some("conversations")),
            ("list_devices", json!({}), "list", Some("devices")),
            ("list_agents", json!({ "query": "q", "source": "hand" }), "list", Some("fountain_agents")),
            ("read_agent", json!({ "name": "pr-reviewer" }), "list", Some("fountain_agent")),
            ("open_port", json!({ "port": 3000, "path": "/x", "beside": "%2" }), "show", Some("port")),
            ("show_changes", json!({ "repo": "/r", "rev_a": "main" }), "show", Some("changes")),
            ("show_file", json!({ "path": "a.rs", "line": 4 }), "show", Some("file")),
            ("open_pr", json!({ "pr": "o/r#1", "dir": "/r" }), "show", Some("pr")),
            ("open_issue", json!({ "issue": "o/r#2" }), "show", Some("issue")),
            ("open_conversation", json!({ "id": "abc", "then": "fork" }), "show", Some("conversation")),
            ("open_app", json!({ "app": "hud" }), "show", Some("app")),
            ("open_workspace", json!({ "dir": "/w", "env": "prod" }), "show", Some("workspace")),
            ("open_fountain", json!({ "view": "runner" }), "show", Some("fountain")),
            ("read_pr", json!({ "block": 4 }), "read_forge", None),
            ("read_issue", json!({ "block": "%4" }), "read_forge", None),
            ("pr_comment", json!({ "block": 4, "body": "b" }), "draft", Some("comment")),
            ("issue_comment", json!({ "block": 4, "body": "b" }), "draft", Some("comment")),
            ("pr_review", json!({ "block": 4, "event": "request_changes", "body": "b" }), "draft", Some("review")),
            ("pr_merge", json!({ "block": 4, "style": "squash" }), "draft", Some("merge")),
            ("issue_new", json!({ "repo": "o/r", "title": "t" }), "draft", Some("issue")),
        ];
        for (old, args, tool, kind) in calls {
            let (def, k, args) = route(old, args, false).unwrap_or_else(|e| panic!("{old}: {e}"));
            assert_eq!((def.name, k), (tool, kind), "{old}");
            let ok = match (tool, kind) {
                ("read_output", _) => parse::<ReadArgs>(args).map(|a| assert!(a.screen)).is_ok(),
                ("history", _) => parse::<HistoryArgs>(args)
                    .map(|a| assert_eq!((a.kind.as_deref(), a.pattern.as_deref()), (Some("output"), Some("x"))))
                    .is_ok(),
                ("list", Some("conversations")) => parse::<ListConversationsArgs>(args).is_ok(),
                ("list", Some("devices")) => parse::<NoArgs>(args).is_ok(),
                ("list", Some("fountain_agents")) => parse::<ListAgentsArgs>(args).is_ok(),
                ("list", Some("fountain_agent")) => parse::<ReadAgentArgs>(args).is_ok(),
                ("show", Some("port")) => parse::<OpenPortArgs>(args).is_ok(),
                ("show", Some("changes")) => parse::<ShowChangesArgs>(args).is_ok(),
                ("show", Some("file")) => parse::<ShowFileArgs>(args).is_ok(),
                ("show", Some("pr")) => parse::<OpenPrArgs>(args).is_ok(),
                ("show", Some("issue")) => parse::<OpenIssueArgs>(args).is_ok(),
                ("show", Some("conversation")) => parse::<OpenConversationArgs>(args).is_ok(),
                ("show", Some("app")) => parse::<OpenAppArgs>(args).is_ok(),
                ("show", Some("workspace")) => parse::<OpenWorkspaceArgs>(args).is_ok(),
                ("show", Some("fountain")) => parse::<OpenFountainArgs>(args).is_ok(),
                ("read_forge", _) => parse::<ReadForgeArgs>(args).is_ok(),
                ("draft", Some("comment")) => parse::<CommentArgs>(args).is_ok(),
                ("draft", Some("review")) => parse::<PrReviewArgs>(args).is_ok(),
                ("draft", Some("merge")) => parse::<PrMergeArgs>(args).is_ok(),
                ("draft", Some("issue")) => parse::<IssueNewArgs>(args).is_ok(),
                _ => unreachable!("{tool} {kind:?}"),
            };
            assert!(ok, "{old}'s arguments don't parse as {tool} {kind:?}");
        }
    }

    /// A grouped call says what's wrong with it: no kind, a kind it doesn't
    /// have, or an argument its kind doesn't take. list has a default kind;
    /// show and draft don't.
    #[test]
    fn kinds_are_picked_and_checked() {
        let kind = |tool: &str, args: Value| route(tool, args, true).map(|(_, k, a)| (k, a));
        assert_eq!(kind("list", json!({})).unwrap(), (Some("panes"), json!({})));
        assert_eq!(kind("list", json!({ "kind": "conversations", "live": true })).unwrap().0, Some("conversations"));
        let e = kind("show", json!({ "port": 3000 })).unwrap_err();
        assert!(e.contains("show needs a kind") && e.contains("port, changes, file"), "{e}");
        let e = kind("show", json!({ "kind": "terminal" })).unwrap_err();
        assert!(e.contains("no kind \"terminal\""), "{e}");
        let e = kind("show", json!({ "kind": 3 })).unwrap_err();
        assert!(e.contains("kind is a string"), "{e}");
        let e = kind("show", json!({ "kind": "port", "port": 3000, "line": 4 })).unwrap_err();
        assert!(e.contains("line isn't an argument of show kind port: it takes port, beside?, path?"), "{e}");
        let e = kind("list", json!({ "kind": "devices", "query": "x" })).unwrap_err();
        assert!(e.contains("it takes nothing else"), "{e}");
        let (k, a) = kind("draft", json!({ "kind": "merge", "block": 4 })).unwrap();
        assert_eq!((k, a), (Some("merge"), json!({ "block": 4 })), "kind is taken out");
        // history isn't a group: its kind is its own argument, passed on.
        let (k, a) = kind("history", json!({ "kind": "answer" })).unwrap();
        assert_eq!((k, a), (None, json!({ "kind": "answer" })));
        assert!(kind("nope", json!({})).unwrap_err().contains("no tool nope"));
    }

    /// A grouped tool's schema has every kind's arguments, kind as an enum
    /// of its kinds, and a description that says what each kind takes. An
    /// argument several kinds share is the same thing in each, give or take
    /// being optional.
    #[test]
    fn grouped_schemas_merge_their_kinds() {
        for labs in [false, true] {
            for d in defs(labs) {
                let Args::Kinds { kinds, default } = d.args else { continue };
                let s = grouped_schema(&kinds, default);
                let props = s["properties"].as_object().unwrap();
                let names: Vec<&str> = kinds.iter().map(|k| k.name).collect();
                assert_eq!(props["kind"]["enum"], json!(names), "{}", d.name);
                assert_eq!(
                    s.get("required").is_some(),
                    default.is_none(),
                    "{}: kind is required without a default",
                    d.name
                );
                let text = kinds_text(d.description, &kinds, default);
                for k in &kinds {
                    let ks = (k.schema)();
                    assert!(text.contains(&format!("\n- {}", k.name)), "{}: {text}", d.name);
                    for (name, p) in ks.get("properties").and_then(Value::as_object).into_iter().flatten() {
                        let merged = &props[name];
                        let strip = |v: &Value| {
                            let mut v = v.clone();
                            if let Some(o) = v.as_object_mut() {
                                o.remove("description");
                            }
                            v
                        };
                        if strip(merged) != strip(p) {
                            assert!(
                                nullable(merged) && !nullable(p),
                                "{} {name}: {merged} vs {p} ({})",
                                d.name,
                                k.name
                            );
                        }
                    }
                    // The refs it uses are there.
                    for r in ks.get("$defs").and_then(Value::as_object).into_iter().flatten().map(|(n, _)| n) {
                        assert!(s["$defs"].get(r).is_some(), "{} lacks $defs/{r}", d.name);
                    }
                }
            }
        }
        // What show's port and file mean by path, both said.
        let show = grouped_schema(&table(SHOW), None);
        let path = show["properties"]["path"]["description"].as_str().unwrap();
        assert!(path.starts_with("port: The page's path") && path.contains("file: The file."), "{path}");
        let text = kinds_text("", &table(SHOW), None);
        assert!(text.contains("\n- port {port, beside?, path?}: "), "{text}");
        assert!(text.contains("\n- file {path, beside?, line?}: "), "{text}");
        let text = kinds_text("", &table(LIST), Some("panes"));
        assert!(text.contains("\n- panes (the default): "), "{text}");
    }

    /// What a stranger can't use isn't in the instructions or in the tools'
    /// descriptions either; chat's text is there only with `labs`.
    #[test]
    fn the_prose_leaves_out_what_a_stranger_cant_use() {
        let mut prose = vec![crate::mcp::instructions(false)];
        prose.extend(
            list(Scope::Full, false)
                .iter()
                .map(|t| format!("{}: {}", t.name, t.description.as_deref().unwrap_or_default())),
        );
        for text in &prose {
            let lower = text.to_lowercase();
            for word in [
                "fountain",
                "studio",
                "chant",
                "workspace",
                "throwaway vm",
                "sandbox",
                "wisp",
                "open_app",
                "thread",
                "@agent",
            ] {
                assert!(!lower.contains(word), "{word} in: {text}");
            }
        }
        let with = crate::mcp::instructions(true);
        assert!(with.contains("read_thread and post_thread") && with.contains("@agent"), "{with}");
        // Labs only adds the thread sentence.
        let without = crate::mcp::instructions(false);
        assert!(with.len() > without.len() && with.replace(crate::mcp::THREAD_INSTRUCTIONS, "") == without);
    }

    /// The README's permissions snippet allows the read-only tools and asks
    /// for the rest, every one of them: a new tool fails this until it's
    /// listed there (#114).
    #[test]
    fn readme_allowlist_is_every_tool() {
        const README: &str = include_str!("../../../../README.md");
        let list = |key: &str| -> Vec<&str> {
            let at = README.find(&format!("\"{key}\": [")).unwrap_or_else(|| panic!("no {key} list in README.md"));
            let body = &README[at..];
            let body = &body[..body.find(']').unwrap()];
            body.split('"').filter_map(|s| s.strip_prefix("mcp__illogical__")).collect()
        };
        // The README lists what a stranger sees; the two chat tools join with labs.
        let defs = defs(false);
        let want = |ro: bool| -> Vec<&str> { defs.iter().filter(|d| d.read_only == ro).map(|d| d.name).collect() };
        assert_eq!(list("allow"), want(true), "README.md's allow list: the read-only tools, in defs() order");
        assert_eq!(list("ask"), want(false), "README.md's ask list: every other tool, in defs() order");
    }
}
