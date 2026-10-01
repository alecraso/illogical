//! The multiplexer task: owns the layout (`illogical_core::Mux`), the panes,
//! the connected clients and what's saved to disk. Every client message, API
//! call and pane notice goes through here, so layout changes, pane starts and
//! stops, resizes, attention and saves happen in one order.

use std::{
    collections::{BTreeMap, HashMap},
    os::fd::OwnedFd,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use illogical_core::{Effect, Intent, Mux};
use illogical_proto::{
    Attention, BlockType, ClientId, ClientMsg, CommandInfo, Event, EventKind, Machine, MachineId, MachineState, Owner,
    PaneId, PaneInfo, PaneOp, Policy, ServerMsg, State, TabId, TabView,
    api::{OpenRequest, PaneSummary, RunRequest},
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    time::{Instant, sleep_until},
};
use tracing::{info, warn};

use crate::{
    block::{Block, BlockCtx},
    machine::Wisp,
    osc::Signal,
    pane::{
        self, CommandRec, ExecRecord, Notice, NoticeSink, PaneHandle, Setup, Spawn, Start, Subscriber, ToClient, What,
    },
    push::Push,
    shellint::Integration,
    store::{LAYOUT_VERSION, PaneLog, PaneMeta, Saved, StateDir, now_ms},
    sys,
};

/// Layout changes are saved this long after the last one.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(250);
/// Working directories and foreground commands change without layout
/// changes; look at them this often.
const REFRESH: Duration = Duration::from_secs(5);
/// A command that ran at least this long, finishing unwatched, is "done".
const DONE_AFTER_MS: u64 = 5_000;
/// Programs that wait for you quietly: one of these going quiet mid-command
/// means it probably needs input.
const AGENTS: &[&str] = &["claude", "codex", "aider", "gemini", "opencode", "goose", "amp"];

pub enum Cmd {
    Connect {
        sub: Subscriber,
    },
    Disconnect {
        client: ClientId,
    },
    Msg {
        client: ClientId,
        msg: ClientMsg,
    },
    Input {
        pane: PaneId,
        data: Vec<u8>,
    },
    Api(Api),
    /// A reset machine's sprite is gone: start its panes again on a new one.
    MachineReset(MachineId),
    /// Checkpoint every pane and save the layout, then reply.
    Shutdown(oneshot::Sender<()>),
}

/// Requests from the HTTP API and the CLI.
pub enum Api {
    Panes(oneshot::Sender<Vec<PaneSummary>>),
    Run(RunRequest, oneshot::Sender<Result<PaneId, String>>),
    Pane(PaneId, oneshot::Sender<Option<PaneHandle>>),
    Attention(PaneId, Attention, oneshot::Sender<bool>),
    Close(PaneId, oneshot::Sender<bool>),
    /// Open a block of any type.
    Open(OpenRequest, oneshot::Sender<Result<PaneId, String>>),
    /// A non-terminal block, to describe or call.
    Block(PaneId, oneshot::Sender<Option<Arc<dyn Block>>>),
    Machines(oneshot::Sender<Vec<Machine>>),
    /// The machine a pane runs on.
    MachineOf(PaneId, oneshot::Sender<Option<Machine>>),
    /// Give a pane's machine to its tab.
    ShareMachine(PaneId, oneshot::Sender<Result<(), String>>),
    /// Delete and recreate a machine; its panes restart by policy.
    ResetMachine(MachineId, oneshot::Sender<Result<(), String>>),
}

#[derive(Clone)]
pub struct MuxHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    events: broadcast::Sender<Event>,
    pub store: StateDir,
    pub wisp: Option<Arc<Wisp>>,
    /// Tags execs on machines (`ILLOGICAL_EXEC`; see `mux::exec_tag`).
    pub daemon_id: String,
}

impl MuxHandle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Shutdown(tx));
        let _ = rx.await;
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub async fn api<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Api) -> Option<T> {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Api(make(tx)));
        rx.await.ok()
    }
}

/// How panes run their shell.
#[derive(Clone, Debug)]
pub struct Config {
    pub shell: String,
    /// Arguments for an interactive shell, e.g. `["-l"]`.
    pub shell_args: Vec<String>,
    pub home: PathBuf,
    /// Merge the systemd user manager's environment into new panes.
    pub manager_env: bool,
    pub launch: pane::Launcher,
    pub integration: Option<Integration>,
    /// The CLI's socket, for `ILLOGICAL_SOCK` in panes.
    pub socket: PathBuf,
    /// Where VM panes get their machines; `None` if not set up.
    pub wisp: Option<Arc<Wisp>>,
    /// Names this daemon's sprites, so a crash sweep only touches ours.
    pub daemon_id: String,
}

impl Config {
    fn env(&self, pane: PaneId) -> Vec<(String, String)> {
        let mut env = if self.manager_env { sys::manager_env() } else { vec![] };
        // The CLI is installed next to the daemon; panes find it on PATH and
        // know which pane (and which daemon) they are.
        let base = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        if let Some(bin) = self.launch.exe.parent() {
            let bin = bin.display().to_string();
            let path = if base.split(':').any(|p| p == bin) { base } else { format!("{bin}:{base}") };
            env.retain(|(k, _)| k != "PATH");
            env.push(("PATH".into(), path));
        }
        env.push(("ILLOGICAL_PANE".into(), pane.to_string()));
        env.push(("ILLOGICAL_SOCK".into(), self.socket.display().to_string()));
        env
    }

    /// An interactive shell, with integration unless it's off for the pane.
    fn shell(&self, pane: PaneId, cwd: PathBuf, integrate: bool) -> Spawn {
        let mut s = Spawn { program: self.shell.clone(), args: self.shell_args.clone(), cwd, env: self.env(pane) };
        if integrate && let Some(i) = &self.integration {
            i.apply(&mut s);
        }
        s
    }

    /// Run `command`, then carry on with an interactive shell in the pane.
    fn run_then_shell(&self, pane: PaneId, cwd: PathBuf, command: &str, integrate: bool) -> Spawn {
        let shell = self.shell(pane, cwd, integrate);
        let then = std::iter::once(shell.program.as_str()).chain(shell.args.iter().map(String::as_str));
        let mut args: Vec<String> = self.shell_args.iter().filter(|a| *a != "--posix").cloned().collect();
        args.extend(["-c".into(), format!("{command}; exec {}", then.collect::<Vec<_>>().join(" "))]);
        Spawn { args, ..shell }
    }

    /// Run `command` by itself (`illogical run`): the pane holds when it
    /// ends, so its output and exit code can still be read.
    fn run_only(&self, pane: PaneId, cwd: PathBuf, command: &str) -> Spawn {
        let mut args = self.shell_args.clone();
        args.extend(["-c".into(), command.into()]);
        Spawn { program: self.shell.clone(), args, cwd, env: self.env(pane) }
    }

    /// What a restored pane does, by its policy.
    fn restore(&self, pane: PaneId, meta: &PaneMeta) -> Start {
        let cwd = meta.cwd.as_ref().map(PathBuf::from).unwrap_or_else(|| self.home.clone());
        let on = meta.integration.unwrap_or(true);
        let shell = match meta.host {
            Some(_) => self.guest_shell(pane, on),
            None => self.shell(pane, cwd.clone(), on),
        };
        let then = |command: &str| match meta.host {
            Some(_) => self.guest_run_then_shell(pane, command, on),
            None => self.run_then_shell(pane, cwd.clone(), command, on),
        };
        let note = |s: &str| format!("\x1b[2m[{s}]\x1b[0m\r\n");
        match (&meta.policy, &meta.command) {
            (Policy::None, _) => Start::Wait { banner: note("press Enter for a shell"), enter: shell, escape: None },
            (Policy::Rerun { confirm: true }, Some(cmd)) => Start::Wait {
                banner: note(&format!("press Enter to re-run: {cmd}  ·  Esc for a shell")),
                enter: then(cmd),
                escape: Some(shell),
            },
            (Policy::Rerun { confirm: false }, Some(cmd)) => Start::Now(then(cmd)),
            (Policy::Hook { command }, _) => Start::Now(then(command)),
            (Policy::Shell | Policy::Rerun { .. }, _) => Start::Now(shell),
        }
    }

    /// The environment of a pane's program on a machine: what the terminal
    /// is, and a tag `process` finds its shell by (machines are shared, so
    /// it names the pane). None of this host's.
    fn guest_env(&self, pane: PaneId) -> Vec<(String, String)> {
        vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("ILLOGICAL_EXEC".into(), exec_tag(&self.daemon_id, pane)),
        ]
    }

    /// The sprite names this daemon's machines get.
    fn sprite_prefix(&self) -> String {
        format!("illogical-eph-{}-", self.daemon_id)
    }

    /// A login shell on a machine (in its home directory).
    fn guest_shell(&self, pane: PaneId, integrate: bool) -> Spawn {
        let mut s =
            Spawn { program: "bash".into(), args: vec!["-l".into()], cwd: PathBuf::new(), env: self.guest_env(pane) };
        if integrate && self.integration.is_some() {
            crate::shellint::apply_guest(&mut s);
        }
        s
    }

    fn guest_run(&self, pane: PaneId, cwd: Option<PathBuf>, command: &str) -> Spawn {
        Spawn {
            program: "bash".into(),
            args: vec!["-lc".into(), command.into()],
            cwd: cwd.unwrap_or_default(),
            env: self.guest_env(pane),
        }
    }

    fn guest_run_then_shell(&self, pane: PaneId, command: &str, integrate: bool) -> Spawn {
        let shell = self.guest_shell(pane, integrate);
        let then = std::iter::once(shell.program.as_str()).chain(shell.args.iter().map(String::as_str));
        let args = vec!["-lc".into(), format!("{command}; exec {}", then.collect::<Vec<_>>().join(" "))];
        Spawn { args, ..shell }
    }
}

/// What was last written to layout.json, to skip writing it unchanged.
type SavedParts = (Mux, BTreeMap<PaneId, PaneMeta>, BTreeMap<MachineId, Machine>);

/// Tags a pane's execs on machines (`ILLOGICAL_EXEC`).
pub fn exec_tag(daemon_id: &str, pane: PaneId) -> String {
    format!("{daemon_id}-p{pane}")
}

struct Daemon {
    mux: Mux,
    panes: HashMap<PaneId, PaneHandle>,
    meta: HashMap<PaneId, PaneMeta>,
    attention: HashMap<PaneId, Attention>,
    clients: HashMap<ClientId, Subscriber>,
    /// The pane each client's focused window is looking at.
    focus: HashMap<ClientId, PaneId>,
    /// Size each pane was last given.
    sizes: BTreeMap<PaneId, (u16, u16)>,
    config: Config,
    store: StateDir,
    notices: NoticeSink,
    events: broadcast::Sender<Event>,
    push: Option<Push>,
    /// Non-terminal blocks (terminals are in `panes`).
    blocks: HashMap<PaneId, Arc<dyn Block>>,
    /// The next block an intent spawns is this type, with this config,
    /// instead of a terminal.
    next_block: Option<(BlockType, serde_json::Value)>,
    /// Why the last block failed to start, for `open_block` to report.
    last_block_error: Option<String>,
    /// The next pane an intent spawns runs this instead of a shell.
    next_spawn: Option<(Spawn, Option<String>)>,
    /// ...and runs it on this machine.
    next_host: Option<MachineId>,
    /// ...which belongs to the new pane's tab, not the pane.
    next_owner_tab: bool,
    /// To ourselves, for work finished in the background.
    tx: mpsc::UnboundedSender<Cmd>,
    machines: BTreeMap<MachineId, Machine>,
    next_machine: MachineId,
    save_due: Option<Instant>,
    last_saved: Option<SavedParts>,
    shutting_down: bool,
}

/// `kept`: pane terminals systemd kept for us across a restart, by FD name.
pub fn start(config: Config, store: StateDir, kept: HashMap<String, OwnedFd>, push: Option<Push>) -> MuxHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (notices, notices_rx) = mpsc::unbounded_channel();
    let (events, _) = broadcast::channel(1024);
    let mut d = Daemon {
        mux: Mux::new(),
        panes: HashMap::new(),
        meta: HashMap::new(),
        attention: HashMap::new(),
        clients: HashMap::new(),
        focus: HashMap::new(),
        sizes: BTreeMap::new(),
        config,
        store: store.clone(),
        notices,
        events: events.clone(),
        push,
        blocks: HashMap::new(),
        next_block: None,
        last_block_error: None,
        next_spawn: None,
        next_host: None,
        next_owner_tab: false,
        tx: tx.clone(),
        machines: BTreeMap::new(),
        next_machine: 1,
        save_due: None,
        last_saved: None,
        shutting_down: false,
    };
    if !d.restore(kept) {
        // Something to attach to on first start.
        if let Err(e) = d.intent(None, Intent::NewSession { name: None, from_pane: None }) {
            warn!(error = %e, "could not create the first session");
        }
    }
    d.sweep_machines();
    let (wisp, daemon_id) = (d.config.wisp.clone(), d.config.daemon_id.clone());
    tokio::spawn(d.run(rx, notices_rx));
    MuxHandle { tx, events, store, wisp, daemon_id }
}

fn info_of(rec: CommandRec) -> CommandInfo {
    CommandInfo {
        text: rec.text,
        cwd: rec.cwd,
        exit: rec.exit,
        started_ms: rec.started_ms,
        ended_ms: rec.ended_ms,
        start: rec.start,
        end: rec.end,
    }
}

impl Daemon {
    /// Bring back the saved layout and every pane in it. False if there was
    /// nothing (usable) to restore.
    fn restore(&mut self, mut kept: HashMap<String, OwnedFd>) -> bool {
        let saved = match self.store.load_layout() {
            Ok(Some(saved)) => saved,
            Ok(None) => return false,
            Err(e) => {
                let aside = self.store.root().join(format!("layout.json.unreadable-{}", now_ms()));
                warn!(error = %e, aside = %aside.display(), "can't read the saved layout; starting fresh");
                let _ = std::fs::rename(self.store.root().join("layout.json"), aside);
                return false;
            }
        };
        let Saved { mux, panes: meta, machines, next_machine, .. } = saved;
        self.mux = mux;
        self.next_machine = next_machine.max(1);
        // A machine is only kept with what owns it.
        let panes: Vec<PaneId> = self.mux.panes();
        for (id, m) in machines {
            let owned = match m.owner {
                Owner::Pane(p) => panes.contains(&p),
                Owner::Tab(t) => self.mux.tab(t).is_ok(),
            };
            if owned {
                self.machines.insert(id, Machine { state: MachineState::Starting, ..m });
            }
        }
        // Nobody is connected yet; whoever views a tab next sizes it.
        let owners: Vec<ClientId> = self.mux.tabs.values().filter_map(|t| t.owner).collect();
        for o in owners {
            self.mux.release(o);
        }
        let ids = self.mux.panes();
        self.store.remove_strays(&ids);
        let rects = self.mux.pane_rects();
        for id in ids {
            let meta = meta.get(&id).cloned().unwrap_or_default();
            let (cols, rows) = rects.get(&id).map(|r| (r.cols, r.rows)).unwrap_or((80, 24));
            let cwd = meta.cwd.as_ref().map(PathBuf::from).unwrap_or_else(|| self.config.home.clone());
            // Still running on a terminal systemd kept for us: carry on with
            // it. Otherwise restore by policy.
            let record = crate::shim::read_record(&self.store.pane_dir(id).join("process"));
            let mut meta = meta;
            meta.host = meta.host.filter(|m| self.machines.contains_key(m));
            let start = match (kept.remove(&format!("pane-{id}")), meta.host) {
                (Some(master), None) if crate::shim::alive(&record) => Start::Adopt(master),
                // On a machine: reattach to the session if there is one.
                (_, Some(_)) => match ExecRecord::read(&self.store.pane_dir(id)) {
                    Some(r) => Start::Resume {
                        session: r.session,
                        received: r.received,
                        otherwise: Box::new(self.config.restore(id, &meta)),
                    },
                    None => self.config.restore(id, &meta),
                },
                _ => self.config.restore(id, &meta),
            };
            if meta.kind != BlockType::Terminal {
                let config = meta.config.clone().unwrap_or_default();
                match self.make_block(id, meta.kind, config, meta.host, true) {
                    Ok(()) => {
                        self.meta.insert(id, meta);
                    }
                    Err(e) => {
                        warn!(block = id, error = %e, "can't restore block; dropping it");
                        let _ = self.mux.apply(Intent::ClosePane { pane: id });
                    }
                }
                continue;
            }
            let integrate = meta.integration.unwrap_or(true);
            // Still running what `run` started: keep holding it.
            let hold = meta.hold && matches!(start, Start::Adopt(_) | Start::Resume { .. });
            match self.open_pane(id, cols, rows, true, start, cwd, integrate, hold, meta.host) {
                Ok(()) => {
                    self.meta.insert(id, meta);
                }
                Err(e) => {
                    warn!(pane = id, error = %e, "can't restore pane; dropping it");
                    let _ = self.mux.apply(Intent::ClosePane { pane: id });
                }
            }
        }
        info!(sessions = self.mux.sessions.len(), panes = self.panes.len(), "restored");
        if self.mux.sessions.is_empty() {
            return false;
        }
        self.machines.retain(|_, m| match m.owner {
            Owner::Pane(p) => self.meta.get(&p).is_some_and(|p| p.host == Some(m.id)),
            Owner::Tab(_) => true,
        });
        self.last_saved = Some((self.mux.clone(), self.meta.clone().into_iter().collect(), self.machines.clone()));
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn open_pane(
        &mut self,
        id: PaneId,
        cols: u16,
        rows: u16,
        restore: bool,
        start: Start,
        cwd: PathBuf,
        integrate: bool,
        hold: bool,
        machine: Option<MachineId>,
    ) -> std::io::Result<()> {
        let host = match machine {
            None => None,
            Some(m) => {
                let wisp = self.config.wisp.clone().ok_or_else(|| std::io::Error::other("VM panes aren't set up"))?;
                let machine = self.machines.get(&m).ok_or_else(|| std::io::Error::other("no such machine"))?;
                Some(pane::Host {
                    wisp,
                    sprite: machine.sprite.clone(),
                    image: machine.image.clone(),
                    rt: tokio::runtime::Handle::current(),
                })
            }
        };
        let shell = match machine {
            Some(m) => self.config.guest_shell(m, integrate),
            None => self.config.shell(id, cwd, integrate),
        };
        let log = PaneLog::open(self.store.pane_dir(id))?;
        let h = pane::spawn_pane(Setup {
            id,
            cols,
            rows,
            log,
            restore,
            start,
            shell,
            launch: self.config.launch.clone(),
            hold,
            notices: self.notices.clone(),
            host,
        })?;
        self.panes.insert(id, h);
        self.sizes.insert(id, (cols, rows));
        Ok(())
    }

    /// Make a non-terminal block for `id` and keep it.
    fn make_block(
        &mut self,
        id: PaneId,
        kind: BlockType,
        config: serde_json::Value,
        host: Option<MachineId>,
        restoring: bool,
    ) -> Result<(), String> {
        let sprite = host.and_then(|m| self.machines.get(&m)).map(|m| m.sprite.clone());
        let dir = self.store.pane_dir(id);
        let ctx = BlockCtx::new(id, dir, self.notices.clone(), self.config.wisp.clone(), sprite, restoring);
        let b = crate::block::create(kind, ctx, config)?;
        self.blocks.insert(id, b);
        Ok(())
    }

    fn block_msg(&self, id: PaneId) -> Option<ServerMsg> {
        Some(ServerMsg::Block { block: id, state: self.blocks.get(&id)?.state() })
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Cmd>, mut notices: mpsc::UnboundedReceiver<Notice>) {
        let mut refresh = tokio::time::interval(REFRESH);
        loop {
            let due = self.save_due.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Shutdown(done)) => {
                        self.shutdown();
                        let _ = done.send(());
                    }
                    Some(cmd) => self.handle(cmd),
                    None => return,
                },
                Some(n) = notices.recv() => self.notice(n),
                _ = sleep_until(due), if self.save_due.is_some() => {
                    self.save_due = None;
                    self.save();
                }
                _ = refresh.tick() => self.save(),
            }
        }
    }

    fn emit(&self, pane: Option<PaneId>, kind: EventKind) {
        let _ = self.events.send(Event { at_ms: now_ms(), pane, kind });
    }

    fn focused(&self, pane: PaneId) -> bool {
        self.focus.values().any(|p| *p == pane)
    }

    fn set_attention(&mut self, pane: PaneId, state: Attention, why: &str) {
        let old = self.attention.get(&pane).copied().unwrap_or_default();
        if old == state || !(self.panes.contains_key(&pane) || self.blocks.contains_key(&pane)) {
            return;
        }
        info!(pane, ?state, why, "attention");
        self.attention.insert(pane, state);
        self.emit(Some(pane), EventKind::Attention { state });
        if matches!(state, Attention::NeedsInput | Attention::Done)
            && !self.focused(pane)
            && let Some(push) = &self.push
        {
            let title = match state {
                Attention::NeedsInput => "Needs you",
                _ => "Done",
            };
            push.send(pane, title, why);
        }
        self.broadcast();
    }

    fn looks_like_agent(&self, pane: PaneId) -> bool {
        let Some(h) = self.panes.get(&pane) else { return false };
        let text = h.status().current.and_then(|c| c.text).or_else(|| h.command()).unwrap_or_default();
        text.split_whitespace().take(3).any(|w| {
            let name = w.rsplit('/').next().unwrap_or(w);
            AGENTS.iter().any(|a| name == *a || name.starts_with(&format!("{a}-")))
        })
    }

    fn notice(&mut self, n: Notice) {
        if self.shutting_down {
            return;
        }
        let pane = n.pane;
        let running_command = || self.panes.get(&pane).is_some_and(|h| h.status().current.is_some());
        match n.what {
            What::Exited { code, close } => {
                let machine_gone = self.machine_of(pane).is_some_and(|m| m.state == MachineState::Gone);
                self.emit(Some(pane), EventKind::Exit { code, machine_gone });
                if close {
                    info!(pane, ?code, "pane exited; closing it");
                    let _ = self.intent(None, Intent::ClosePane { pane });
                    return;
                }
                if !self.focused(pane) {
                    self.set_attention(pane, Attention::Done, &format!("exited with code {}", code.unwrap_or(-1)));
                }
                self.changed();
            }
            What::Machine(up) => {
                let state = if up { MachineState::Running } else { MachineState::Gone };
                let id = self.meta.get(&pane).and_then(|m| m.host);
                if let Some(m) = id.and_then(|id| self.machines.get_mut(&id))
                    && m.state != state
                {
                    m.state = state;
                    let machine = m.id;
                    info!(pane, machine, ?state, "machine");
                    self.emit(Some(pane), EventKind::Machine { machine, state });
                    self.broadcast();
                }
            }
            What::BlockChanged => {
                if let Some(msg) = self.block_msg(pane) {
                    for sub in self.clients.values() {
                        let _ = sub.ctrl.send(ToClient::Msg(msg.clone()));
                    }
                }
                // Its config may have changed with it.
                self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
            }
            What::Attention(state, why) => self.set_attention(pane, state, &why),
            What::Started => {
                // What `run` started is gone; the new program isn't held.
                if let Some(m) = self.meta.get_mut(&pane) {
                    m.hold = false;
                }
                self.set_attention(pane, Attention::Idle, "started");
                self.changed();
            }
            What::Busy(true) => {
                if running_command()
                    && matches!(
                        self.attention.get(&pane).copied().unwrap_or_default(),
                        Attention::Idle | Attention::Done
                    )
                {
                    self.set_attention(pane, Attention::Working, "output");
                }
            }
            What::Busy(false) => {
                if running_command()
                    && self.looks_like_agent(pane)
                    && self.attention.get(&pane) == Some(&Attention::Working)
                {
                    self.set_attention(pane, Attention::NeedsInput, "an agent went quiet");
                }
            }
            What::Signal(signal) => match signal {
                Signal::Prompt => {
                    self.emit(Some(pane), EventKind::Prompt);
                    if self.attention.get(&pane) == Some(&Attention::Working) {
                        self.set_attention(pane, Attention::Idle, "prompt");
                    }
                }
                Signal::CommandLine { .. } => {}
                Signal::CommandStart => {
                    let text = self.panes.get(&pane).and_then(|h| h.status().current.and_then(|c| c.text));
                    self.emit(Some(pane), EventKind::CommandStart { text });
                    self.set_attention(pane, Attention::Working, "command started");
                    self.broadcast();
                }
                Signal::CommandEnd { exit } => {
                    let last = self.panes.get(&pane).and_then(|h| h.status().last);
                    let took = last.as_ref().map(|l| l.ended_ms.unwrap_or(l.started_ms) - l.started_ms).unwrap_or(0);
                    let text = last.and_then(|l| l.text);
                    self.emit(Some(pane), EventKind::CommandEnd { text: text.clone(), exit });
                    // Something that asked for you still wants you after its
                    // command ends; only input (or a client) clears that.
                    if self.attention.get(&pane) == Some(&Attention::NeedsInput) {
                        self.broadcast();
                        return;
                    }
                    if took >= DONE_AFTER_MS && !self.focused(pane) {
                        let what = format!("{} exited {}", text.as_deref().unwrap_or("command"), exit.unwrap_or(-1));
                        self.set_attention(pane, Attention::Done, &what);
                    } else {
                        self.set_attention(pane, Attention::Idle, "command ended");
                    }
                    self.broadcast();
                }
                Signal::Cwd { path } => {
                    self.emit(Some(pane), EventKind::Cwd { path });
                    self.broadcast();
                }
                Signal::Notify { title, body } => {
                    self.emit(Some(pane), EventKind::Notify { title: title.clone(), body: body.clone() });
                    let why = if body.is_empty() { title } else { body };
                    self.set_attention(pane, Attention::NeedsInput, &why);
                }
                Signal::Bell => {
                    self.emit(Some(pane), EventKind::Bell);
                    if !self.focused(pane) {
                        self.set_attention(pane, Attention::NeedsInput, "bell");
                    }
                }
            },
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Connect { sub } => {
                let hello = ServerMsg::Hello {
                    version: env!("CARGO_PKG_VERSION").into(),
                    client: sub.client,
                    state: self.state(),
                };
                let _ = sub.ctrl.send(ToClient::Msg(hello));
                for id in self.blocks.keys() {
                    if let Some(msg) = self.block_msg(*id) {
                        let _ = sub.ctrl.send(ToClient::Msg(msg));
                    }
                }
                self.clients.insert(sub.client, sub);
            }
            Cmd::Disconnect { client } => {
                self.clients.remove(&client);
                self.focus.remove(&client);
                for p in self.panes.values() {
                    p.detach(client);
                }
                if self.mux.release(client) {
                    self.changed();
                }
            }
            Cmd::Input { pane, data } => self.input(pane, data),
            Cmd::Msg { client, msg } => self.message(client, msg),
            Cmd::Api(api) => self.api(api),
            Cmd::MachineReset(id) => self.machine_reset(id),
            Cmd::Shutdown(_) => unreachable!("handled in run"),
        }
    }

    /// Someone typed in a pane: whatever it wanted, it has their attention.
    fn input(&mut self, pane: PaneId, data: Vec<u8>) {
        let Some(p) = self.panes.get(&pane) else { return };
        p.input(data);
        if matches!(self.attention.get(&pane), Some(Attention::NeedsInput | Attention::Done)) {
            let next = if p.status().current.is_some() { Attention::Working } else { Attention::Idle };
            self.set_attention(pane, next, "input");
        }
    }

    fn api(&mut self, api: Api) {
        match api {
            Api::Panes(reply) => {
                let _ = reply.send(self.summaries());
            }
            Api::Pane(pane, reply) => {
                let _ = reply.send(self.panes.get(&pane).cloned());
            }
            Api::Attention(pane, state, reply) => {
                let known = self.panes.contains_key(&pane);
                self.set_attention(pane, state, "set by the API");
                let _ = reply.send(known);
            }
            Api::Machines(reply) => {
                let _ = reply.send(self.machines.values().cloned().collect());
            }
            Api::MachineOf(pane, reply) => {
                let _ = reply.send(self.machine_of(pane).cloned());
            }
            Api::ShareMachine(pane, reply) => {
                let _ = reply.send(self.share_machine(pane));
            }
            Api::ResetMachine(id, reply) => {
                let _ = reply.send(self.reset_machine(id));
            }
            Api::Open(req, reply) => {
                let _ = reply.send(self.open_block(req));
            }
            Api::Block(id, reply) => {
                let _ = reply.send(self.blocks.get(&id).cloned());
            }
            Api::Close(pane, reply) => {
                let known = self.panes.contains_key(&pane) || self.blocks.contains_key(&pane);
                if known {
                    let _ = self.intent(None, Intent::ClosePane { pane });
                }
                let _ = reply.send(known);
            }
            Api::Run(req, reply) => {
                let _ = reply.send(self.run_command(req));
            }
        }
    }

    /// `illogical run`: a new tab (or a split) running a command.
    fn run_command(&mut self, req: RunRequest) -> Result<PaneId, String> {
        let from = req.from_pane.filter(|p| self.panes.contains_key(p));
        let cwd = req
            .cwd
            .clone()
            .map(PathBuf::from)
            .or_else(|| from.and_then(|p| self.panes.get(&p)?.cwd()))
            .unwrap_or_else(|| self.config.home.clone());
        let session = self.resolve_session(req.session.as_deref(), from)?;
        let before: Vec<PaneId> = self.mux.panes();
        let host = if req.vm || req.vm_tab { Some(self.new_machine(req.image.clone())?) } else { None };
        self.next_spawn = req.command.as_ref().map(|command| {
            let spawn = match host {
                // Not this host's directory: the guest's, if one was asked for.
                Some(_) => self.config.guest_run(0, req.cwd.clone().map(PathBuf::from), command),
                None => self.config.run_only(0, cwd, command),
            };
            (spawn, Some(command.clone()))
        });
        self.next_host = host;
        self.next_owner_tab = req.vm_tab;
        let split = req.split.filter(|_| !req.vm_tab);
        let intent = match (split, session) {
            // Here: a script's command is for this host, even in a VM tab.
            (Some(pane), _) => Intent::Split { pane, edge: illogical_proto::Edge::Right, local: true },
            (None, Some(session)) => Intent::NewTab { session, from_pane: from },
            (None, None) => Intent::NewSession { name: None, from_pane: from },
        };
        let result = self.intent(None, intent);
        self.next_spawn = None;
        self.next_owner_tab = false;
        if let Some(m) = self.next_host.take() {
            // Nothing took it.
            self.machines.remove(&m);
        }
        result?;
        let pane = self.mux.panes().into_iter().find(|p| !before.contains(p)).ok_or("no pane was created")?;
        if let Some(policy) = req.policy {
            self.meta.entry(pane).or_default().policy = policy;
        }
        Ok(pane)
    }

    /// Which session a new tab goes in: one named (made if missing), else
    /// `from`'s, else the first.
    fn resolve_session(&mut self, name: Option<&str>, from: Option<PaneId>) -> Result<Option<u32>, String> {
        Ok(match name {
            Some(name) => match self.mux.sessions.iter().find(|s| s.name == name || s.id.to_string() == name) {
                Some(s) => Some(s.id),
                None => {
                    // A new session starts with a shell; the block gets a
                    // tab of its own next to it.
                    self.intent(None, Intent::NewSession { name: Some(name.to_owned()), from_pane: None })?;
                    self.mux.sessions.last().map(|s| s.id)
                }
            },
            None => from
                .and_then(|p| self.mux.tab_of(p).ok())
                .and_then(|t| self.mux.session_of_tab(t).ok())
                .or_else(|| self.mux.sessions.first().map(|s| s.id)),
        })
    }

    /// `POST /api/blocks`: a new block of any type, in a tab of its own or
    /// split beside another. In a VM tab it runs on the tab's machine.
    fn open_block(&mut self, req: OpenRequest) -> Result<PaneId, String> {
        if req.kind == BlockType::Terminal {
            return Err("terminals are opened with run".into());
        }
        let from = req.from_pane.filter(|p| self.panes.contains_key(p) || self.blocks.contains_key(p));
        let session = self.resolve_session(req.session.as_deref(), from)?;
        let before: Vec<PaneId> = self.mux.panes();
        self.next_block = Some((req.kind, req.config));
        let intent = match (req.split, session) {
            (Some(pane), _) => Intent::Split { pane, edge: illogical_proto::Edge::Right, local: false },
            (None, Some(session)) => Intent::NewTab { session, from_pane: from },
            (None, None) => Intent::NewSession { name: None, from_pane: from },
        };
        let result = self.intent(None, intent);
        let unused = self.next_block.take();
        result?;
        if let Some((kind, _)) = unused {
            return Err(format!("no {kind:?} block was made").to_lowercase());
        }
        let id = self.mux.panes().into_iter().find(|p| !before.contains(p)).ok_or("no block was made")?;
        if !self.blocks.contains_key(&id) {
            return Err(self.last_block_error.take().unwrap_or_else(|| "the block couldn't start".into()));
        }
        Ok(id)
    }

    /// A new machine for the next pane; it's created when its first
    /// program starts.
    fn new_machine(&mut self, image: Option<String>) -> Result<MachineId, String> {
        if self.config.wisp.is_none() {
            return Err("VM panes aren't set up: illogicald found no wisp token (see --wisp-token-file)".into());
        }
        let id = self.next_machine;
        self.next_machine += 1;
        let sprite = format!("{}{id}", self.config.sprite_prefix());
        let owner = Owner::Pane(0);
        let m = Machine { id, provider: "wisp".into(), sprite, image, owner, state: MachineState::Starting };
        self.machines.insert(id, m);
        Ok(id)
    }

    fn machine_of(&self, pane: PaneId) -> Option<&Machine> {
        self.machines.get(&self.meta.get(&pane)?.host?)
    }

    /// The machine a tab owns.
    fn tab_machine(&self, tab: TabId) -> Option<MachineId> {
        self.machines.values().find(|m| m.owner == Owner::Tab(tab)).map(|m| m.id)
    }

    /// Whether a pane runs on its own tab's machine (and so can't leave it).
    fn on_tab_machine(&self, pane: PaneId) -> bool {
        let tab = self.mux.tab_of(pane).ok();
        let host = self.meta.get(&pane).and_then(|m| m.host);
        host.is_some() && tab.and_then(|t| self.tab_machine(t)) == host
    }

    /// Refuse moves that would take a pane away from its tab's machine.
    fn check_move(&self, intent: &Intent) -> Result<(), String> {
        let stuck = |pane: PaneId| format!("%{pane} runs on this tab's machine, so it stays in the tab");
        match *intent {
            Intent::MovePane { pane, target, .. }
                if self.on_tab_machine(pane) && self.mux.tab_of(pane).ok() != self.mux.tab_of(target).ok() =>
            {
                Err(stuck(pane))
            }
            Intent::BreakPane { pane, .. } if self.on_tab_machine(pane) => Err(stuck(pane)),
            Intent::DockTab { tab, .. } if self.tab_machine(tab).is_some() => {
                Err("this tab has a machine: move the whole tab instead".into())
            }
            _ => Ok(()),
        }
    }

    /// Delete machines whose tab has closed.
    fn reap_machines(&mut self) {
        let gone: Vec<MachineId> = self
            .machines
            .values()
            .filter(|m| matches!(m.owner, Owner::Tab(t) if self.mux.tab(t).is_err()))
            .map(|m| m.id)
            .collect();
        for m in gone {
            self.delete_machine(m);
        }
    }

    /// "Share machine with tab": the pane's own machine becomes its tab's,
    /// and new splits in the tab join it.
    fn share_machine(&mut self, pane: PaneId) -> Result<(), String> {
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        if self.tab_machine(tab).is_some() {
            return Err("this tab already has a machine".into());
        }
        let id = self.meta.get(&pane).and_then(|m| m.host).ok_or("that pane runs on this host")?;
        let m = self.machines.get_mut(&id).ok_or("no such machine")?;
        if m.owner != Owner::Pane(pane) {
            return Err("that pane's machine isn't its own".into());
        }
        m.owner = Owner::Tab(tab);
        info!(pane, tab, machine = id, "machine shared with tab");
        self.changed();
        Ok(())
    }

    /// "Reset machine": delete its sprite, then start every pane on it again
    /// by its policy, on a new one.
    fn reset_machine(&mut self, id: MachineId) -> Result<(), String> {
        let m = self.machines.get_mut(&id).ok_or("no such machine")?;
        m.state = MachineState::Starting;
        let sprite = m.sprite.clone();
        let wisp = self.config.wisp.clone().ok_or("VM panes aren't set up")?;
        info!(machine = id, sprite, "resetting machine");
        self.emit(None, EventKind::Machine { machine: id, state: MachineState::Starting });
        self.broadcast();
        // Let go first, so its panes don't report the machine gone.
        for (pane, meta) in &self.meta {
            if meta.host == Some(id)
                && let Some(h) = self.panes.get(pane)
            {
                h.release();
            }
        }
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Err(e) = wisp.delete(&sprite).await {
                warn!(sprite, error = %e, "can't delete machine to reset it");
            }
            let _ = tx.send(Cmd::MachineReset(id));
        });
        Ok(())
    }

    fn machine_reset(&mut self, id: MachineId) {
        let panes: Vec<PaneId> = self.meta.iter().filter(|(_, m)| m.host == Some(id)).map(|(p, _)| *p).collect();
        for pane in panes {
            let (Some(h), Some(meta)) = (self.panes.get(&pane), self.meta.get(&pane)) else { continue };
            h.restart(self.config.restore(pane, meta), "machine reset");
        }
    }

    /// Delete a machine and everything on it, in the background.
    fn delete_machine(&mut self, id: MachineId) {
        let Some(m) = self.machines.remove(&id) else { return };
        self.emit(None, EventKind::Machine { machine: id, state: MachineState::Gone });
        let Some(wisp) = self.config.wisp.clone() else { return };
        tokio::spawn(async move {
            match wisp.delete(&m.sprite).await {
                Ok(()) => info!(machine = m.id, sprite = m.sprite, "deleted machine"),
                Err(e) => warn!(machine = m.id, sprite = m.sprite, error = %e, "can't delete machine"),
            }
        });
    }

    /// Delete our sprites that no machine owns: left by a crash, or by a
    /// pane closed while the daemon was down.
    fn sweep_machines(&self) {
        let Some(wisp) = self.config.wisp.clone() else { return };
        let prefix = self.config.sprite_prefix();
        let keep: Vec<String> = self.machines.values().map(|m| m.sprite.clone()).collect();
        tokio::spawn(async move {
            let names = match wisp.list(&prefix).await {
                Ok(n) => n,
                Err(e) => return warn!(error = %e, "can't list machines to sweep"),
            };
            for name in names.into_iter().filter(|n| !keep.contains(n)) {
                match wisp.delete(&name).await {
                    Ok(()) => info!(sprite = name, "deleted a machine nothing owns"),
                    Err(e) => warn!(sprite = name, error = %e, "can't delete stray machine"),
                }
            }
        });
    }

    fn message(&mut self, client: ClientId, msg: ClientMsg) {
        let Some(sub) = self.clients.get(&client).cloned() else {
            return;
        };
        match msg {
            ClientMsg::Attach { panes } => {
                for a in panes {
                    if let Some(p) = self.panes.get(&a.pane) {
                        p.attach(sub.clone(), a.offset);
                    }
                }
            }
            ClientMsg::Detach { panes } => {
                for id in panes {
                    if let Some(p) = self.panes.get(&id) {
                        p.detach(client);
                    }
                }
            }
            ClientMsg::View { tab, cols, rows, zoom, claim } => {
                if let Ok(true) = self.mux.view(client, tab, cols, rows, zoom, claim) {
                    self.changed();
                }
            }
            ClientMsg::Intent { id, intent } => {
                if let Err(message) = self.intent(Some(client), intent) {
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id, message }));
                }
            }
            ClientMsg::Focus { pane } => {
                match pane {
                    Some(p) => {
                        self.focus.insert(client, p);
                        // Seeing a finished command is enough.
                        if self.attention.get(&p) == Some(&Attention::Done) {
                            self.set_attention(p, Attention::Idle, "seen");
                        }
                    }
                    None => {
                        self.focus.remove(&client);
                    }
                }
            }
            ClientMsg::Pane { pane, op } => {
                let Some(handle) = self.panes.get(&pane) else {
                    let message = format!("no pane %{pane}");
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
                    return;
                };
                match op {
                    PaneOp::SetPolicy { policy } => {
                        info!(pane, ?policy, "restart policy");
                        self.meta.entry(pane).or_default().policy = policy;
                    }
                    PaneOp::Purge => {
                        info!(pane, "purging history");
                        handle.purge();
                    }
                    PaneOp::SetIntegration { on } => {
                        info!(pane, on, "shell integration");
                        self.meta.entry(pane).or_default().integration = Some(on);
                    }
                    PaneOp::Attention { state } => self.set_attention(pane, state, "set by a client"),
                }
                self.changed();
            }
        }
    }

    fn intent(&mut self, client: Option<ClientId>, intent: Intent) -> Result<(), String> {
        let refused = |why: String| {
            info!(?client, ?intent, why, "intent refused");
            why
        };
        self.check_move(&intent).map_err(refused)?;
        let local = matches!(intent, Intent::Split { local: true, .. });
        let effects = self.mux.apply(intent.clone()).map_err(|e| refused(e.to_string()))?;
        info!(?client, ?intent, "intent");
        let rects = self.mux.pane_rects();
        for e in effects {
            match e {
                Effect::Spawn { pane, cwd_from } => {
                    let from_meta = cwd_from.and_then(|p| self.meta.get(&p)).and_then(|m| m.integration);
                    let integrate = from_meta.unwrap_or(true);
                    let cwd =
                        cwd_from.and_then(|p| self.panes.get(&p)?.cwd()).unwrap_or_else(|| self.config.home.clone());
                    let (cols, rows) = rects.get(&pane).map(|r| (r.cols, r.rows)).unwrap_or((80, 24));
                    // A machine made for it, else its tab's (unless asked
                    // for this host).
                    let tab = self.mux.tab_of(pane).ok();
                    let made = self.next_host.take();
                    let host = made.or_else(|| tab.filter(|_| !local).and_then(|t| self.tab_machine(t)));
                    if let Some((kind, config)) = self.next_block.take() {
                        match self.make_block(pane, kind, config.clone(), host, false) {
                            Ok(()) => {
                                let meta = PaneMeta { host, kind, config: Some(config), ..Default::default() };
                                self.meta.insert(pane, meta);
                                self.emit(Some(pane), EventKind::Opened);
                            }
                            Err(e) => {
                                warn!(block = pane, ?kind, error = %e, "could not start block");
                                self.last_block_error = Some(e);
                                let _ = self.mux.apply(Intent::ClosePane { pane });
                            }
                        }
                        continue;
                    }
                    let (start, hold, cwd) = match self.next_spawn.take() {
                        // A command from `run`: fill in the pane id it gets.
                        Some((mut spawn, Some(text))) => {
                            spawn.env.retain(|(k, _)| k != "ILLOGICAL_PANE" && k != "ILLOGICAL_EXEC");
                            spawn.env.push(("ILLOGICAL_PANE".into(), pane.to_string()));
                            if host.is_some() {
                                spawn.env.push(("ILLOGICAL_EXEC".into(), exec_tag(&self.config.daemon_id, pane)));
                            }
                            let cwd = spawn.cwd.clone();
                            (Start::Run { spawn, text }, true, cwd)
                        }
                        _ => {
                            let shell = match host {
                                Some(_) => self.config.guest_shell(pane, integrate),
                                None => self.config.shell(pane, cwd.clone(), integrate),
                            };
                            (Start::Now(shell), false, cwd)
                        }
                    };
                    if let Some(m) = made.and_then(|m| self.machines.get_mut(&m)) {
                        m.owner = match (self.next_owner_tab, tab) {
                            (true, Some(t)) => Owner::Tab(t),
                            _ => Owner::Pane(pane),
                        };
                    }
                    match self.open_pane(pane, cols, rows, false, start, cwd, integrate, hold, host) {
                        Ok(()) => {
                            let meta = PaneMeta { integration: from_meta, host, hold, ..Default::default() };
                            self.meta.insert(pane, meta);
                            self.emit(Some(pane), EventKind::Opened);
                        }
                        Err(e) => {
                            warn!(pane, error = %e, "could not start pane");
                            if let Some(m) = made {
                                self.machines.remove(&m);
                            }
                            let _ = self.mux.apply(Intent::ClosePane { pane });
                        }
                    }
                }
                Effect::Kill { pane } => {
                    if let Some(p) = self.panes.remove(&pane) {
                        p.close();
                    }
                    if let Some(b) = self.blocks.remove(&pane) {
                        b.close();
                        // Its history is kept like a closed pane's.
                        if let Ok(log) = PaneLog::open(self.store.pane_dir(pane)) {
                            log.retire(pane);
                        }
                    }
                    // A pane's own machine goes with it (a tab's, with the tab).
                    if let Some(m) = self.machine_of(pane).filter(|m| m.owner == Owner::Pane(pane)) {
                        self.delete_machine(m.id);
                    }
                    self.sizes.remove(&pane);
                    self.meta.remove(&pane);
                    self.attention.remove(&pane);
                    self.emit(Some(pane), EventKind::Closed);
                }
            }
        }
        self.reap_machines();
        self.changed();
        self.emit(None, EventKind::Layout { rev: self.mux.rev });
        Ok(())
    }

    /// Resize panes whose cells changed, tell every client, and save soon.
    fn changed(&mut self) {
        for (pane, r) in self.mux.pane_rects() {
            let size = (r.cols, r.rows);
            if self.sizes.get(&pane) != Some(&size)
                && let Some(b) = self.blocks.get(&pane)
            {
                b.resize(r.cols, r.rows);
                self.sizes.insert(pane, size);
            }
            if self.sizes.get(&pane) != Some(&size)
                && let Some(p) = self.panes.get(&pane)
            {
                p.resize(r.cols, r.rows);
                self.sizes.insert(pane, size);
            }
        }
        self.broadcast();
        self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
    }

    fn broadcast(&self) {
        let state = self.state();
        for sub in self.clients.values() {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::State { state: state.clone() }));
        }
    }

    /// Note each running pane's directory and the command a re-run would
    /// run. True if any changed.
    fn refresh_meta(&mut self) -> bool {
        let mut changed = false;
        for (id, h) in &self.panes {
            if !h.running() {
                continue;
            }
            let status = h.status();
            let m = self.meta.entry(*id).or_default();
            let cwd = status.cwd.clone().or_else(|| h.cwd().map(|c| c.display().to_string())).or(m.cwd.clone());
            // The command line as typed, when the shell integration reported
            // it; otherwise what /proc says is in the foreground.
            let command = match status.current {
                Some(c) => c.text.or_else(|| h.command()),
                None => h.command(),
            };
            changed |= m.cwd != cwd || m.command != command;
            (m.cwd, m.command) = (cwd, command);
        }
        changed
    }

    /// Write the layout and pane details if anything changed since the last
    /// write.
    fn save(&mut self) {
        // Whichever notices a new directory or command tells the clients.
        if self.refresh_meta() {
            self.broadcast();
        }
        for (id, b) in &self.blocks {
            if let Some(m) = self.meta.get_mut(id) {
                m.config = Some(b.config());
            }
        }
        let panes: BTreeMap<PaneId, PaneMeta> = self.meta.iter().map(|(k, v)| (*k, v.clone())).collect();
        if self.last_saved.as_ref().is_some_and(|(m, p, ms)| *m == self.mux && *p == panes && *ms == self.machines) {
            return;
        }
        let saved = Saved {
            version: LAYOUT_VERSION,
            saved_at_ms: now_ms(),
            mux: self.mux.clone(),
            panes,
            machines: self.machines.clone(),
            next_machine: self.next_machine,
        };
        match self.store.save_layout(&saved) {
            Ok(()) => self.last_saved = Some((saved.mux, saved.panes, saved.machines)),
            Err(e) => warn!(error = %e, "can't save layout"),
        }
    }

    /// Before the daemon exits: every pane's terminal and the layout to disk.
    /// Exits from here on (panes hung up by our own exit) change nothing.
    fn shutdown(&mut self) {
        self.shutting_down = true;
        for p in self.panes.values() {
            p.checkpoint(Duration::from_secs(3));
        }
        self.last_saved = None;
        self.save();
        info!(panes = self.panes.len(), "saved for shutdown");
    }

    fn pane_info(&self, p: &PaneHandle) -> PaneInfo {
        let meta = self.meta.get(&p.id).cloned().unwrap_or_default();
        let running = p.running();
        let status = p.status();
        PaneInfo {
            id: p.id,
            epoch: p.epoch,
            cwd: status.cwd.clone().or_else(|| p.cwd().map(|c| c.display().to_string())).or(meta.cwd),
            command: if running { p.command() } else { meta.command },
            running,
            policy: meta.policy,
            current: status.current.map(info_of),
            last: status.last.map(info_of),
            attention: self.attention.get(&p.id).copied().unwrap_or_default(),
            integration: meta.integration.unwrap_or(true),
            kind: BlockType::Terminal,
            host: meta.host,
        }
    }

    /// A non-terminal block's entry in the state.
    fn block_info(&self, id: PaneId, b: &Arc<dyn Block>) -> PaneInfo {
        let meta = self.meta.get(&id).cloned().unwrap_or_default();
        PaneInfo {
            id,
            epoch: 0,
            cwd: None,
            command: None,
            running: true,
            policy: meta.policy,
            current: None,
            last: None,
            attention: self.attention.get(&id).copied().unwrap_or_default(),
            integration: false,
            kind: b.kind(),
            host: meta.host,
        }
    }

    fn info_of_any(&self, id: PaneId) -> Option<PaneInfo> {
        match (self.panes.get(&id), self.blocks.get(&id)) {
            (Some(h), _) => Some(self.pane_info(h)),
            (_, Some(b)) => Some(self.block_info(id, b)),
            _ => None,
        }
    }

    fn summaries(&self) -> Vec<PaneSummary> {
        let mut out = Vec::new();
        for s in &self.mux.sessions {
            for tab in &s.tabs {
                let Ok(t) = self.mux.tab(*tab) else { continue };
                for pane in t.root.panes() {
                    let Some(info) = self.info_of_any(pane) else { continue };
                    out.push(PaneSummary {
                        session: s.id,
                        session_name: s.name.clone(),
                        tab: t.id,
                        tab_name: t.name.clone(),
                        info,
                    });
                }
            }
        }
        out
    }

    fn state(&self) -> State {
        let tabs = self
            .mux
            .sessions
            .iter()
            .flat_map(|s| &s.tabs)
            .filter_map(|id| {
                let t = self.mux.tab(*id).ok()?;
                Some(TabView {
                    id: t.id,
                    name: t.name.clone(),
                    root: t.root.clone(),
                    cols: t.cols,
                    rows: t.rows,
                    owner: t.owner,
                    zoom: t.zoom,
                    layout: self.mux.layout(t.id).ok()?,
                })
            })
            .collect();
        let mut panes: Vec<PaneInfo> = self.panes.values().map(|p| self.pane_info(p)).collect();
        panes.extend(self.blocks.iter().map(|(id, b)| self.block_info(*id, b)));
        panes.sort_by_key(|p| p.id);
        let machines = self.machines.values().cloned().collect();
        State { rev: self.mux.rev, sessions: self.mux.sessions.clone(), tabs, panes, machines }
    }
}
