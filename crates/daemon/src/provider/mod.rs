//! Sandbox providers (M4b): whoever runs the machines that aren't this
//! host, behind one trait. The first adapter is the Sprites API
//! ([`sprites`]), which covers wisp here on geek and Fly's hosted sprites.
//!
//! A provider can:
//!
//! - **list** its sandboxes and say what state each is in (`running`,
//!   `warm`, `cold`), from its API: asking never wakes a sandbox;
//! - **create**, **delete** and **wake** one;
//! - open a **byte stream** to a port inside one (the Sprites proxy): how a
//!   browser block reaches a dev server, and how the home daemon reaches a
//!   resident daemon's port (the provider tunnel);
//! - run an **exec TTY** (create, attach, resize, kill): VM panes, and a
//!   shell on a sandbox with no daemon in it;
//! - run a non-TTY exec with pipes (agent servers in VMs), and a command to
//!   completion;
//! - optionally write files and keep a **service** running (start on boot,
//!   restart on exit): how a daemon becomes resident;
//! - optionally **browse files** (M7, [`Caps::fs_browse`]): list a
//!   directory and read part of a file, which is how the picker and `fs`
//!   reach a machine with no daemon of ours in it.
//!
//! **Differences are capabilities, not assumptions** ([`Caps`]). S4 found
//! two "compatible" implementations differ in how much exec output they
//! replay, who owns a reattached session, how a kill behaves and what
//! "cold" means; code asks instead of assuming wisp.

pub mod sprites;

use std::{io, sync::Arc, time::Duration};

use arugula_proto::fs::FsList;
use futures_util::{FutureExt, future::BoxFuture};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};

use crate::pane::Spawn;

/// A byte stream to a port, whichever way it was reached.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub type Conn = Box<dyn Io>;

/// What going "cold" does to a sandbox's processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cold {
    /// A real reboot: processes are gone, the disk stays; services start
    /// again at the next wake (wisp, after `--warm-ttl`).
    Reboot,
    /// Memory is restored: processes carry on (what S4 saw on Fly).
    Restore,
}

/// How a provider behaves where implementations differ (S4, M3b spike).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// How much output a detached exec keeps and resends on reattach, in
    /// bytes. A shell with no daemon behind it has no more history than
    /// this while nothing is attached: wisp keeps 1 MiB, Fly about 6.5 KB.
    pub exec_replay: u64,
    /// Reattaching can resume from a byte offset, so nothing is repeated.
    pub exec_offset: bool,
    /// The one who reattaches becomes the session's owner.
    pub reattach_owner: bool,
    /// A reattached session takes resizes (owner or not).
    pub reattach_resize: bool,
    /// Kill takes a chosen signal (HUP ends a shell at once).
    pub kill_signal: bool,
    /// How long a default kill waits before SIGKILL.
    pub kill_grace_ms: u64,
    pub cold: Cold,
    /// Files can be written into a sandbox.
    pub fs: bool,
    /// Files can be listed and read ([`Provider::fs_list`], `fs_read`).
    #[serde(default)]
    pub fs_browse: bool,
    /// Services: started on boot and restarted when they exit.
    pub services: bool,
}

/// A sandbox, as its provider lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sandbox {
    pub name: String,
    /// `running`, `warm` (paused, memory kept) or `cold`; whatever the
    /// provider says.
    pub status: String,
}

/// A process the provider keeps running in a sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDef {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

/// What an exec reports to its pane.
#[derive(Debug)]
pub enum ExecEvent {
    /// Attached to this session (first time or again).
    Session(String),
    Output(Vec<u8>),
    /// The program ended.
    Exited(Option<i32>),
    /// The machine is gone, or the session can't be reached.
    Lost {
        machine_gone: bool,
    },
}

pub(crate) enum ExecInput {
    Data(Vec<u8>),
    Resize(u16, u16),
    HangUp,
}

/// How to begin: a new session for `spawn`, or reattach to one we were
/// following, having received `received` bytes of it.
pub enum Begin {
    /// `create`: make the sandbox if it doesn't exist (our own machines);
    /// otherwise a missing sandbox is "machine gone" (someone else's).
    New {
        spawn: Spawn,
        image: Option<String>,
        create: bool,
    },
    Resume {
        session: String,
        received: u64,
    },
}

/// A pane's terminal on a machine. Dropping it detaches, leaving the session
/// running for the next daemon.
pub struct Exec {
    pub(crate) tx: mpsc::UnboundedSender<ExecInput>,
}

impl Exec {
    pub fn input(&self, data: Vec<u8>) {
        let _ = self.tx.send(ExecInput::Data(data));
    }
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.tx.send(ExecInput::Resize(cols, rows));
    }
    pub fn hang_up(&self) {
        let _ = self.tx.send(ExecInput::HangUp);
    }
}

pub type ExecSink = Box<dyn Fn(ExecEvent) -> bool + Send>;

/// How a piped (non-TTY) exec begins.
pub enum PipeBegin {
    New {
        argv: Vec<String>,
    },
    /// Reattach past the `received` bytes of output we already have.
    Resume {
        session: String,
        received: u64,
    },
}

/// What a piped exec sends.
#[derive(Debug)]
pub enum PipeEvent {
    Session(String),
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exited(Option<i32>),
}

/// One connection to a piped exec. `events` ends when the connection drops
/// (the session may still be running); dropping `stdin` detaches.
pub struct Pipe {
    pub events: mpsc::UnboundedReceiver<PipeEvent>,
    pub stdin: mpsc::UnboundedSender<Vec<u8>>,
}

/// A sandbox provider. Methods that take a name never create the sandbox
/// unless they say so; none of the status calls wake it.
pub trait Provider: Send + Sync + std::fmt::Debug {
    /// Short name, kept with machines and hosts: `wisp`, `sprites`.
    fn name(&self) -> &str;
    fn caps(&self) -> &Caps;

    /// Sandboxes whose names start with `prefix`.
    fn list<'a>(&'a self, prefix: &'a str) -> BoxFuture<'a, anyhow::Result<Vec<Sandbox>>>;
    /// One sandbox, or `None` if there's no such thing. Doesn't wake it.
    fn status<'a>(&'a self, name: &'a str) -> BoxFuture<'a, anyhow::Result<Option<Sandbox>>>;
    /// Create it; one that already exists counts as created.
    fn create<'a>(&'a self, name: &'a str, image: Option<&'a str>) -> BoxFuture<'a, anyhow::Result<()>>;
    /// Delete it and everything on it; already gone is fine.
    fn delete<'a>(&'a self, name: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;
    /// Wake it (and wait until it answers).
    fn wake<'a>(&'a self, name: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;

    /// A new connection to `port` inside it, waking it if needed.
    fn dial<'a>(&'a self, name: &'a str, port: u16) -> BoxFuture<'a, io::Result<Conn>>;

    /// A terminal session: events go to `sink` in order, from a task on `rt`.
    fn exec_tty(
        self: Arc<Self>,
        rt: &tokio::runtime::Handle,
        name: String,
        begin: Begin,
        size: (u16, u16),
        sink: ExecSink,
    ) -> Exec;
    /// Send a signal to one session's process.
    fn kill<'a>(&'a self, name: &'a str, session: &'a str, signal: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;
    /// Run a command to completion without a terminal: (stdout, exit code).
    fn run<'a>(&'a self, name: &'a str, argv: &'a [&'a str]) -> BoxFuture<'a, anyhow::Result<(Vec<u8>, Option<i32>)>>;
    /// A non-TTY exec with stdin, kept running for hours if we detach.
    fn pipe<'a>(&'a self, name: &'a str, begin: PipeBegin) -> BoxFuture<'a, anyhow::Result<Pipe>>;

    /// Write a file (atomically, making its directory).
    fn write_file<'a>(
        &'a self,
        name: &'a str,
        path: &'a str,
        data: Vec<u8>,
        mode: u32,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
    /// Define (or replace) a service and start it.
    fn put_service<'a>(
        &'a self,
        name: &'a str,
        service: &'a str,
        def: &'a ServiceDef,
    ) -> BoxFuture<'a, anyhow::Result<()>>;
    /// Stop and remove a service; no such service is fine.
    fn delete_service<'a>(&'a self, name: &'a str, service: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;

    /// A directory's entries, or (for anything else, a symlink included) a
    /// one-entry list of the thing itself, with `path` the absolute path the
    /// provider resolved. Relative paths (and `~`) are from the sandbox
    /// user's home. Errors are [`crate::fs::FsError`]s where the provider
    /// says why.
    fn fs_list<'a>(&'a self, name: &'a str, path: &'a str) -> BoxFuture<'a, anyhow::Result<FsList>> {
        let _ = (name, path);
        async { Err(crate::fs::FsError::Unavailable("this provider can't browse files".into()).into()) }.boxed()
    }
    /// Up to `len` bytes of a file from `offset`, and the file's size.
    fn fs_read<'a>(
        &'a self,
        name: &'a str,
        path: &'a str,
        offset: u64,
        len: u64,
    ) -> BoxFuture<'a, anyhow::Result<(Vec<u8>, u64)>> {
        let _ = (name, path, offset, len);
        async { Err(crate::fs::FsError::Unavailable("this provider can't read files".into()).into()) }.boxed()
    }
}

/// Tries, half a second apart, before giving up on a provider that isn't
/// answering (wispd may still be starting, at boot).
pub const PATIENCE: u32 = 120;

/// Make sure the sandbox exists, creating it if not; waits for a provider
/// that isn't answering yet.
pub async fn ensure(p: &dyn Provider, name: &str, image: Option<&str>) -> anyhow::Result<()> {
    let mut tries = 0;
    loop {
        match p.status(name).await {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => return p.create(name, image).await,
            Err(e) if tries >= PATIENCE => return Err(e),
            Err(_) => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

/// Whether the sandbox exists (`Err` if the provider can't say).
pub async fn exists(p: &dyn Provider, name: &str) -> anyhow::Result<bool> {
    Ok(p.status(name).await?.is_some())
}
