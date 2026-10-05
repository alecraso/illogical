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
    path::PathBuf,
    sync::{Arc, Mutex},
};

use futures_util::future::BoxFuture;
use illogical_proto::{Attention, BlockType, PaneId, Policy};
use serde_json::Value;

use crate::{
    pane::{Kept, Launcher, Notice, NoticeSink, What},
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
    /// The process it runs on this host, if any (an agent's server: a
    /// Claude Code under it is this block's, #81).
    fn pid(&self) -> Option<u32> {
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
    /// The user's shell environment (#74), for blocks that run the user's
    /// tools: see [`crate::review::Runner::user`].
    pub shell_env: Arc<crate::shellenv::ShellEnv>,
    /// The multiplexer, for a block that raises questions on itself (M35).
    pub cmds: Option<tokio::sync::mpsc::UnboundedSender<crate::mux::Cmd>>,
    /// The daemon's panes and blocks.
    pub ids: PaneIds,
    /// Standing permission rules (#166).
    pub rules: Arc<crate::rules::Rules>,
    /// What runs an invite the owner sent from its card (#234).
    pub invite: crate::invite::Hook,
}

/// The ids of the daemon's panes and blocks, as the multiplexer keeps them.
pub type PaneIds = Arc<Mutex<std::collections::HashSet<PaneId>>>;

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
    pub kept: Arc<Mutex<HashMap<String, Kept>>>,
    pub secrets: Secrets,
    pub mcp: Option<crate::mcp::Link>,
    pub fs: Arc<crate::fs::Scope>,
    pub shell_env: Arc<crate::shellenv::ShellEnv>,
    cmds: Option<tokio::sync::mpsc::UnboundedSender<crate::mux::Cmd>>,
    ids: PaneIds,
    /// Standing permission rules (#166).
    pub rules: Arc<crate::rules::Rules>,
    pub invite: crate::invite::Hook,
}

impl BlockCtx {
    pub fn new(
        id: PaneId,
        dir: PathBuf,
        base: BlockEnv,
        sprite: Option<String>,
        restoring: bool,
        policy: Policy,
        kept: HashMap<String, Kept>,
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
            shell_env: base.shell_env,
            cmds: base.cmds,
            ids: base.ids,
            rules: base.rules,
            invite: base.invite,
        }
    }

    /// Whether a pane or block is this daemon's (#77).
    pub fn ours(&self, id: PaneId) -> bool {
        self.ids.lock().unwrap().contains(&id)
    }

    /// Raise a question on this block (M35), drawn and answered as a
    /// terminal's is: the token withdraws exactly this one, and the
    /// receiver gets the answer and who gave it.
    pub async fn ask(
        &self,
        ask: illogical_proto::ask::Ask,
    ) -> Result<(u64, tokio::sync::oneshot::Receiver<crate::mux::Replied>), String> {
        let cmds = self.cmds.as_ref().ok_or("this block can't ask")?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        cmds.send(crate::mux::Cmd::Api(crate::mux::Api::Ask(self.id, Box::new(ask), tx)))
            .map_err(|_| "the daemon is stopping".to_owned())?;
        rx.await.map_err(|_| "the daemon is stopping".to_owned())?
    }

    /// ...and take it back, if it's still that one.
    pub fn withdraw(&self, id: &str, token: u64) {
        if let Some(cmds) = &self.cmds {
            let _ = cmds.send(crate::mux::Cmd::Api(crate::mux::Api::AskWithdraw(
                self.id,
                Some(id.to_owned()),
                Some(token),
            )));
        }
    }

    /// Open another block (M36: a diff beside a PR), as the owner.
    pub async fn open(&self, req: illogical_proto::api::OpenRequest) -> Result<PaneId, String> {
        let cmds = self.cmds.as_ref().ok_or("this block can't open others")?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        cmds.send(crate::mux::Cmd::Api(crate::mux::Api::Open(req, None, tx)))
            .map_err(|_| "the daemon is stopping".to_owned())?;
        rx.await.map_err(|_| "the daemon is stopping".to_owned())?
    }

    /// Put this block in a tab of its own (M37: an issue, before its agent
    /// joins it), named `name` if the tab has no name.
    pub async fn own_tab(&self, name: Option<String>) -> Result<(), String> {
        let cmds = self.cmds.as_ref().ok_or("this block can't move")?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        cmds.send(crate::mux::Cmd::Api(crate::mux::Api::OwnTab(self.id, name, tx)))
            .map_err(|_| "the daemon is stopping".to_owned())?;
        rx.await.map_err(|_| "the daemon is stopping".to_owned())?
    }

    /// Whether another block is still open (M37: an issue's agent).
    pub async fn block_open(&self, id: PaneId) -> bool {
        let Some(cmds) = self.cmds.as_ref() else { return false };
        let (tx, rx) = tokio::sync::oneshot::channel();
        if cmds.send(crate::mux::Cmd::Api(crate::mux::Api::Block(id, tx))).is_err() {
            return false;
        }
        rx.await.ok().flatten().is_some()
    }

    /// Start a terminal (M36: a shell in a PR's worktree).
    pub async fn run(&self, req: illogical_proto::api::RunRequest) -> Result<PaneId, String> {
        let cmds = self.cmds.as_ref().ok_or("this block can't open others")?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        cmds.send(crate::mux::Cmd::Api(crate::mux::Api::Run(req, tx)))
            .map_err(|_| "the daemon is stopping".to_owned())?;
        rx.await.map_err(|_| "the daemon is stopping".to_owned())?
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

    /// Ask for attention with a reason of its own (M34: a gate), or say
    /// more about the same one.
    pub fn reason(&self, state: Attention, reason: illogical_proto::Reason) {
        let _ = self.notices.send(Notice { pane: self.id, what: What::Reason(state, reason) });
    }

    /// Let go of attention, if it's still for a reason of this kind (one
    /// dismissed or replaced meanwhile is left alone).
    pub fn clear(&self, kind: illogical_proto::ReasonKind) {
        let _ = self.notices.send(Notice { pane: self.id, what: What::Clear(kind) });
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
        BlockType::Remote => crate::remote::Remote::create(ctx, config),
        BlockType::Diff => crate::review::diff::Diff::create(ctx, config),
        BlockType::File => crate::review::file::FileView::create(ctx, config),
        BlockType::Workspace => crate::workspace::Workspace::create(ctx, config),
        BlockType::App => crate::apps::AppBlock::create(ctx, config),
        BlockType::Forge => crate::forge::ForgeBlock::create(ctx, config),
        BlockType::Fountain => crate::fountain::FountainBlock::create(ctx, config),
        BlockType::Invite => crate::invite::card::InviteBlock::create(ctx, config),
    }
}

/// A method name the type doesn't have.
pub fn no_method(kind: BlockType, method: &str) -> String {
    format!("{kind:?} blocks have no method {method:?}").to_lowercase()
}
