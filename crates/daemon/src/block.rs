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
//! - **a log** in its own block directory, in the M2 segment store.
//!
//! All blocks share one id space (`%N`) and one place in the layout tree.

use std::{path::PathBuf, sync::Arc};

use futures_util::future::BoxFuture;
use illogical_proto::{Attention, BlockType, PaneId};
use serde_json::Value;

use crate::{
    machine::Wisp,
    pane::{Notice, NoticeSink, What},
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
    /// Its cells changed size (a terminal-like renderer may care).
    fn resize(&self, _cols: u16, _rows: u16) {}
    /// It's closing: stop whatever it runs. Its directory is retired after.
    fn close(&self);
}

/// What a block gets from the daemon.
#[derive(Clone)]
#[allow(dead_code)] // `wisp`, `sprite` and `restoring` are for machine-backed types
pub struct BlockCtx {
    pub id: PaneId,
    /// Its own directory, for its log and anything else it keeps.
    pub dir: PathBuf,
    notices: NoticeSink,
    pub rt: tokio::runtime::Handle,
    pub wisp: Option<Arc<Wisp>>,
    /// The sprite it runs on, if not this host.
    pub sprite: Option<String>,
    /// Whether it's being brought back after a restart.
    pub restoring: bool,
}

impl BlockCtx {
    pub fn new(
        id: PaneId,
        dir: PathBuf,
        notices: NoticeSink,
        wisp: Option<Arc<Wisp>>,
        sprite: Option<String>,
        restoring: bool,
    ) -> Self {
        Self { id, dir, notices, rt: tokio::runtime::Handle::current(), wisp, sprite, restoring }
    }

    /// Its state changed: clients get the new one.
    pub fn changed(&self) {
        let _ = self.notices.send(Notice { pane: self.id, what: What::BlockChanged });
    }

    /// Ask for (or let go of) the user's attention.
    pub fn attention(&self, state: Attention, why: impl Into<String>) {
        let _ = self.notices.send(Notice { pane: self.id, what: What::Attention(state, why.into()) });
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
        BlockType::Agent => Err("agent blocks aren't built yet".into()),
    }
}

/// A method name the type doesn't have.
pub fn no_method(kind: BlockType, method: &str) -> String {
    format!("{kind:?} blocks have no method {method:?}").to_lowercase()
}
