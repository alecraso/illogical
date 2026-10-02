//! Blocks that aren't terminals: what every type provides, and how the
//! multiplexer makes them.
//!
//! A terminal is the first block type and keeps its own fast path
//! (`pane.rs`: PTY bytes, snapshots and offsets). Every other type
//! implements [`Block`]:
//!
//! - **config**, saved in `layout.json`: what it needs to be made again;
//! - **state**, as JSON: what its renderer in the client draws and what
//!   `describe` returns, pushed to clients whenever it changes;
//! - **attention**, through the same notices as terminals, so badges, the
//!   "needs you" list and push notifications work for every type;
//! - **text**, a plain rendering for `capture --text`, history and search;
//! - **methods**, called as `illogical call %N <method> [json]`;
//! - **a log** in its own block directory, in the M2 segment store: what
//!   it did, or for a view of something kept elsewhere (a file, a repo),
//!   only what it was pointed at, when (M11);
//! - **drawn**: told whether any client draws it now (a full client
//!   showing its tab; summaries don't count), so a view that watches a
//!   machine can stop and let the machine sleep (M11).
//!
//! All blocks share one id space (`%N`) and one place in the layout tree.

use std::{
    collections::HashMap,
    os::fd::OwnedFd,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use futures_util::future::BoxFuture;
use illogical_proto::{Attention, BlockType, PaneId, Policy};
use serde_json::Value;

use crate::{
    pane::{Launcher, Notice, NoticeSink, What},
    provider::Provider,
    store::PaneLog,
};

/// A non-terminal block, owned by the multiplexer.
pub trait Block: Send + Sync {
    fn kind(&self) -> BlockType;
    /// What `layout.json` keeps to make it again.
    fn config(&self) -> Value;
    /// What clients draw and `describe` returns.
    fn state(&self) -> Value;
    /// A plain-text rendering, for `capture --text`, history and search.
    fn text(&self) -> String;
    /// One of the type's methods.
    fn call(&self, method: &str, args: Value) -> BoxFuture<'static, Result<Value, String>>;
    /// ...on behalf of someone (M29: their name, for its transcript and
    /// history when they approve, answer or send it a follow-up).
    fn call_by(&self, method: &str, args: Value, _by: Option<&str>) -> BoxFuture<'static, Result<Value, String>> {
        self.call(method, args)
    }
    /// Its cells changed size (a terminal-like renderer may care).
    fn resize(&self, _cols: u16, _rows: u16) {}
    /// Some client draws it now (true), or none does any more (M11).
    fn drawn(&self, _on: bool) {}
    /// It's closing: stop whatever it runs. Its directory is retired after.
    fn close(&self);
    /// Extra fields for a push notification about it (an agent's pending
    /// approval, so the notification can approve it).
    fn push_extra(&self) -> Option<Value> {
        None
    }
    /// What it waits on you for (M24): its first open permission request
    /// or question.
    fn waiting(&self) -> Option<Waiting> {
        None
    }
    /// What it says about itself in summaries (M23), beyond its type.
    fn summary(&self) -> Summary {
        Summary::default()
    }
    /// The editor connected to it (M28): an editor block's window, or an
    /// editor that joined the swarm.
    fn link(&self) -> Option<Arc<crate::editor::link::Link>> {
        None
    }
    /// An editor says it's this block's window (M28): keep it, if this is
    /// an editor block.
    fn attach(&self, _link: Arc<crate::editor::link::Link>) -> bool {
        false
    }
    /// ...and it went away.
    fn detach(&self, _link: &Arc<crate::editor::link::Link>) {}
    /// It isn't in the layout: an editor that joined the swarm (M28).
    fn detached(&self) -> bool {
        false
    }
}

/// A block's part of its summary (M23): what it's busy with, where, and
/// for an editor, which file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub work: Option<illogical_proto::WorkKind>,
    /// Where it works, and the git repository that is (on its machine).
    pub cwd: Option<String>,
    pub project: Option<illogical_proto::Project>,
    pub file: Option<String>,
    pub title: Option<String>,
    /// An editor's own report (M28).
    pub editor: Option<illogical_proto::EditorInfo>,
}

/// An open permission request or question in a block (M24's `ask` reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub id: String,
    pub what: illogical_proto::AskWhat,
    pub headline: String,
    /// Which agent asks.
    pub agent: String,
    /// Where it works, for the bundle key.
    pub cwd: Option<String>,
    pub at_ms: u64,
}

/// Files that hold credentials an agent in a VM needs. They're read when
/// the agent starts and passed to it in its environment only: never saved,
/// logged, or put on the VM's disk.
#[derive(Clone, Debug, Default)]
pub struct Secrets {
    /// An Anthropic API key (`ANTHROPIC_API_KEY`).
    pub anthropic_key: PathBuf,
    /// A Claude Code token from `claude setup-token`
    /// (`CLAUDE_CODE_OAUTH_TOKEN`), used if there's no API key.
    pub claude_token: PathBuf,
}

/// What the daemon gives every block it makes, besides its id and place.
#[derive(Clone)]
pub struct BlockEnv {
    pub notices: NoticeSink,
    pub provider: Option<Arc<dyn Provider>>,
    /// How processes are started on this host (shim, scope, FD store).
    pub launch: Launcher,
    /// The environment they get (as a pane's shell would).
    pub env: Vec<(String, String)>,
    pub home: PathBuf,
    pub secrets: Secrets,
    /// How an agent reaches the daemon's MCP server (M16).
    pub mcp: Option<crate::mcp::Link>,
    /// This host's files, as `/api/fs` serves them (M7).
    pub fs: Arc<crate::fs::Scope>,
}

/// What a block gets from the daemon.
#[derive(Clone)]
#[allow(dead_code)] // not every type uses every field
pub struct BlockCtx {
    pub id: PaneId,
    /// Its own directory, for its log and anything else it keeps.
    pub dir: PathBuf,
    notices: NoticeSink,
    pub rt: tokio::runtime::Handle,
    pub provider: Option<Arc<dyn Provider>>,
    /// The sprite it runs on, if not this host.
    pub sprite: Option<String>,
    /// Whether it's being brought back after a restart.
    pub restoring: bool,
    /// What it does when brought back after a restart.
    pub policy: Policy,
    pub launch: Launcher,
    pub env: Vec<(String, String)>,
    pub home: PathBuf,
    /// Descriptors systemd kept for it across a restart, by name; take what
    /// you use.
    pub kept: Arc<Mutex<HashMap<String, OwnedFd>>>,
    pub secrets: Secrets,
    pub mcp: Option<crate::mcp::Link>,
    pub fs: Arc<crate::fs::Scope>,
}

impl BlockCtx {
    pub fn new(
        id: PaneId,
        dir: PathBuf,
        base: BlockEnv,
        sprite: Option<String>,
        restoring: bool,
        policy: Policy,
        kept: HashMap<String, OwnedFd>,
    ) -> Self {
        Self {
            id,
            dir,
            notices: base.notices,
            rt: tokio::runtime::Handle::current(),
            provider: base.provider,
            sprite,
            restoring,
            policy,
            launch: base.launch,
            env: base.env,
            home: base.home,
            kept: Arc::new(Mutex::new(kept)),
            secrets: base.secrets,
            mcp: base.mcp,
            fs: base.fs,
        }
    }

    /// Where its files are: this host's, or its machine's.
    pub fn files(&self) -> Result<crate::fs::Target, String> {
        match (&self.sprite, &self.provider) {
            (None, _) => Ok(crate::fs::Target::Local(self.fs.clone())),
            (Some(sprite), Some(p)) => Ok(crate::fs::Target::machine(p.clone(), sprite.clone())),
            (Some(_), None) => Err("this block's machine can't be reached: VM panes aren't set up".into()),
        }
    }

    /// Where its notices go (M28: an editor's link sends its own).
    pub fn sink(&self) -> NoticeSink {
        self.notices.clone()
    }

    /// Its state changed: clients get the new one.
    pub fn changed(&self) {
        let _ = self.notices.send(Notice { pane: self.id, what: What::BlockChanged });
    }

    /// Ask for (or let go of) the user's attention.
    pub fn attention(&self, state: Attention, why: impl Into<String>) {
        let _ = self.notices.send(Notice { pane: self.id, what: What::Attention(state, why.into()) });
    }

    /// Something happened that the event stream should carry.
    pub fn event(&self, kind: illogical_proto::EventKind) {
        let _ = self.notices.send(Notice { pane: self.id, what: What::Event(kind) });
    }

    /// Its machine is up (true) or gone (false).
    pub fn machine(&self, up: bool) {
        let _ = self.notices.send(Notice { pane: self.id, what: What::Machine(up) });
    }

    /// The block's log: its own segment store.
    pub fn log(&self) -> std::io::Result<PaneLog> {
        PaneLog::open(self.dir.clone())
    }
}

/// Make a block of `kind` from its config.
pub fn create(kind: BlockType, ctx: BlockCtx, config: Value) -> Result<Arc<dyn Block>, String> {
    match kind {
        BlockType::Terminal => Err("terminals aren't made here".into()),
        BlockType::Browser => crate::browser::Browser::create(ctx, config),
        BlockType::Agent => crate::agent::Agent::create(ctx, config),
        BlockType::Editor => crate::editor::Editor::create(ctx, config),
        BlockType::Diff => crate::review::diff::Diff::create(ctx, config),
        BlockType::File => crate::review::file::FileView::create(ctx, config),
    }
}

/// A method name the type doesn't have.
pub fn no_method(kind: BlockType, method: &str) -> String {
    format!("{kind:?} blocks have no method {method:?}").to_lowercase()
}
