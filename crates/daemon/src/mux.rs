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

use illogical_core::{Effect, Intent, Mux, Role};
use illogical_proto::{
    Attention, BlockType, ClientId, ClientMsg, CommandInfo, Driver, Event, EventKind, Machine, MachineId, MachineState,
    Owner, PaneId, PaneInfo, PaneOp, Policy, Presence, ServerMsg, SessionId, State, TabId, TabView,
    api::{OpenRequest, PaneSummary, RunRequest},
    ask::Ask,
};
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    time::{Instant, sleep_until},
};
use tracing::{info, warn};

use crate::{
    acl::Principal,
    block::{Block, BlockCtx},
    osc::Signal,
    pane::{
        self, CommandRec, ExecRecord, Notice, NoticeSink, PaneHandle, Setup, Spawn, Start, Subscriber, ToClient, What,
    },
    provider::Provider,
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
    /// Typing. `client`: whose, to check (M12); `None` when the caller
    /// already did (the API).
    Input {
        client: Option<ClientId>,
        pane: PaneId,
        data: Vec<u8>,
    },
    Api(Api),
    /// Grants changed (M12): show each client what it may see now, and hang
    /// up on anyone left with nothing.
    AclChanged,
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
    /// Open a block of any type; for a guest (M14), their principal: then
    /// it must be an agent beside a pane they edit, and runs on a VM of
    /// theirs.
    Open(OpenRequest, Option<crate::acl::Principal>, oneshot::Sender<Result<PaneId, String>>),
    /// A non-terminal block, to describe or call.
    Block(PaneId, oneshot::Sender<Option<Arc<dyn Block>>>),
    Machines(oneshot::Sender<Vec<Machine>>),
    /// The machine a pane runs on.
    MachineOf(PaneId, oneshot::Sender<Option<Machine>>),
    /// Give a pane's machine to its tab.
    ShareMachine(PaneId, oneshot::Sender<Result<(), String>>),
    /// Delete and recreate a machine; its panes restart by policy.
    ResetMachine(MachineId, oneshot::Sender<Result<(), String>>),
    /// A question asked in a terminal (`illogical ask`, from Claude Code's
    /// hook): shown beside it until answered. The reply carries a token
    /// (for withdrawing exactly this one) and where the answer will come.
    Ask(PaneId, Ask, oneshot::Sender<Result<(u64, oneshot::Receiver<AskReply>), String>>),
    /// A client answered a terminal's question (`id`: which; `None`: the
    /// one open). Replies with the question, or why not.
    AskReply(PaneId, Option<String>, AskReply, oneshot::Sender<Result<Ask, String>>),
    /// The asker gave up (Claude Code interrupted it): close the card.
    /// With a token, only if it's still that registration's.
    AskWithdraw(PaneId, Option<String>, Option<u64>),
    /// Someone's role on the session a pane or block is in (M12), and for
    /// a "from now" share where its output may start for them (M13).
    RoleOn(crate::acl::Principal, PaneId, oneshot::Sender<Option<(Role, Option<u64>)>>),
    /// Whether a guest may type in a pane on this machine (M14).
    MayDrive(crate::acl::Principal, PaneId, oneshot::Sender<Result<(), String>>),
    /// Where each pane of a session's output ends now (a "from now" share
    /// starts there).
    SessionEnds(SessionId, oneshot::Sender<Option<BTreeMap<PaneId, u64>>>),
}

/// What a terminal's question got.
#[derive(Debug, Clone, PartialEq)]
pub enum AskReply {
    /// The card's fields (as AskUserQuestion's form names them).
    Answer(serde_json::Value),
    /// Skipped.
    Decline,
    /// "Answer in terminal": let the program show its own picker.
    Terminal,
    /// It went away (its pane closed, or a newer one replaced it).
    Withdrawn,
}

/// A question open in a terminal.
struct TermAsk {
    ask: Ask,
    token: u64,
    reply: oneshot::Sender<AskReply>,
}

#[derive(Clone)]
pub struct MuxHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    events: broadcast::Sender<Event>,
    pub store: StateDir,
    pub provider: Option<Arc<dyn Provider>>,
    /// Tags execs on machines (`ILLOGICAL_EXEC`; see `mux::exec_tag`).
    pub daemon_id: String,
    /// This host's files, as the `fs` methods may read them.
    pub fs: Arc<crate::fs::Scope>,
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
    /// Who else may reach which sessions (M12).
    pub acl: Arc<crate::acl::Acl>,
    /// Illogical control: notifications through it go to people's
    /// devices (M21).
    pub control: Arc<crate::control::Control>,
    /// What to call the owner to others (M13): their login, else "owner".
    pub owner_name: String,
    /// The owner's picture, if the tailnet gave one.
    pub owner_pic: Option<String>,
    /// How many VMs each guest may have at once (M14).
    pub guest_machines: usize,
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
    pub provider: Option<Arc<dyn Provider>>,
    /// Names this daemon's sprites, so a crash sweep only touches ours.
    pub daemon_id: String,
    /// Where agents in VMs get their credentials from.
    pub secrets: crate::block::Secrets,
    /// Secrets the `fs` methods never serve (the provider's token, agents'
    /// credentials); the state directory is added to these.
    pub private: Vec<PathBuf>,
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
            Some(_) => self.guest_shell(pane, on, None),
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

    /// A login shell on a machine, in its home directory unless given one
    /// of its own directories.
    fn guest_shell(&self, pane: PaneId, integrate: bool, cwd: Option<PathBuf>) -> Spawn {
        let (program, args, env) = ("bash".into(), vec!["-l".into()], self.guest_env(pane));
        let mut s = Spawn { program, args, cwd: cwd.unwrap_or_default(), env };
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
        let shell = self.guest_shell(pane, integrate, None);
        let then = std::iter::once(shell.program.as_str()).chain(shell.args.iter().map(String::as_str));
        let args = vec!["-lc".into(), format!("{command}; exec {}", then.collect::<Vec<_>>().join(" "))];
        Spawn { args, ..shell }
    }
}

/// What was last written to layout.json, to skip writing it unchanged.
type SavedParts = (Mux, BTreeMap<PaneId, PaneMeta>, BTreeMap<MachineId, Machine>);

/// Where `run --split %N --join` puts the new pane.
enum Join {
    /// This host.
    Here,
    /// The split pane's tab's machine.
    TabMachine,
    /// A sandbox the split pane has a shell on: borrowed again.
    Borrow(String),
}

/// A random number for generated names.
fn seed() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(now_ms())
}

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
    /// When each client was last told it can't type somewhere (once is
    /// enough while it keeps trying).
    refused: HashMap<ClientId, Instant>,
    /// The tab each client shows (M13 presence).
    viewing: HashMap<ClientId, TabId>,
    /// Who drives each pane (M13), and panes in pair mode.
    drivers: HashMap<PaneId, Driver>,
    pair: std::collections::HashSet<PaneId>,
    /// Guests trusted to drive a pane on this machine (M14), until when.
    trust: HashMap<(PaneId, String), u64>,
    /// Size each pane was last given.
    sizes: BTreeMap<PaneId, (u16, u16)>,
    config: Config,
    store: StateDir,
    notices: NoticeSink,
    events: broadcast::Sender<Event>,
    push: Option<Push>,
    /// Non-terminal blocks (terminals are in `panes`).
    blocks: HashMap<PaneId, Arc<dyn Block>>,
    /// Questions open in terminals, one per pane (M6c).
    asks: HashMap<PaneId, TermAsk>,
    next_ask: u64,
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
    /// ...and starts its shell here (a directory on its host).
    next_cwd: Option<PathBuf>,
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
        refused: HashMap::new(),
        viewing: HashMap::new(),
        drivers: HashMap::new(),
        pair: Default::default(),
        trust: HashMap::new(),
        sizes: BTreeMap::new(),
        config,
        store: store.clone(),
        notices,
        events: events.clone(),
        push,
        blocks: HashMap::new(),
        asks: HashMap::new(),
        next_ask: 1,
        next_block: None,
        last_block_error: None,
        next_spawn: None,
        next_host: None,
        next_owner_tab: false,
        next_cwd: None,
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
    let (provider, daemon_id) = (d.config.provider.clone(), d.config.daemon_id.clone());
    let mut private = d.config.private.clone();
    private.push(store.root().to_path_buf());
    let fs = Arc::new(crate::fs::Scope::new(d.config.home.clone(), private));
    tokio::spawn(d.run(rx, notices_rx));
    MuxHandle { tx, events, store, provider, daemon_id, fs }
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
        by: rec.by,
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
                // What systemd kept for it (an agent server's pipes).
                let prefix = format!("agent-{id}-");
                let names: Vec<String> = kept.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
                let mine = names.into_iter().filter_map(|k| kept.remove_entry(&k)).collect();
                match self.make_block(id, meta.kind, config, meta.host, Some((meta.policy.clone(), mine))) {
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
                let provider =
                    self.config.provider.clone().ok_or_else(|| std::io::Error::other("VM panes aren't set up"))?;
                let machine = self.machines.get(&m).ok_or_else(|| std::io::Error::other("no such machine"))?;
                Some(pane::Host {
                    provider,
                    borrowed: machine.borrowed,
                    sprite: machine.sprite.clone(),
                    image: machine.image.clone(),
                    rt: tokio::runtime::Handle::current(),
                })
            }
        };
        let shell = match machine {
            Some(m) => self.config.guest_shell(m, integrate, None),
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

    /// Make a non-terminal block for `id` and keep it. `restoring`: its
    /// restart policy, and what systemd kept for it.
    fn make_block(
        &mut self,
        id: PaneId,
        kind: BlockType,
        config: serde_json::Value,
        host: Option<MachineId>,
        restoring: Option<(Policy, HashMap<String, OwnedFd>)>,
    ) -> Result<(), String> {
        let sprite = host.and_then(|m| self.machines.get(&m)).map(|m| m.sprite.clone());
        let dir = self.store.pane_dir(id);
        let base = crate::block::BlockEnv {
            notices: self.notices.clone(),
            provider: self.config.provider.clone(),
            launch: self.config.launch.clone(),
            env: self.config.env(id),
            home: self.config.home.clone(),
            secrets: self.config.secrets.clone(),
        };
        let is_restore = restoring.is_some();
        let (policy, kept) = restoring.unwrap_or_default();
        let ctx = BlockCtx::new(id, dir, base, sprite, is_restore, policy, kept);
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
            let extra = self.blocks.get(&pane).and_then(|b| b.push_extra()).or_else(|| {
                let choice = self.asks.get(&pane)?.ask.push_choice()?;
                Some(serde_json::json!({ "ask": choice }))
            });
            push.send(pane, title, why, extra);
        }
        if matches!(state, Attention::NeedsInput | Attention::Done) && !self.focused(pane) {
            // Through control (M21): the owner, and whoever may edit the
            // session. Approve and answer actions need the daemon's own
            // page, so those notifications just open the pane.
            let title = if state == Attention::NeedsInput { "Needs you" } else { "Done" };
            let session = self.session_of(pane);
            let acl = self.config.acl.clone();
            self.config.control.push(pane, title, why, None, move |who| {
                who.is_owner() || session.and_then(|s| acl.role(who, s)).is_some_and(|r| r >= Role::Editor)
            });
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
                    for sub in self.clients.values().filter(|c| self.sees(&c.principal, pane)) {
                        let _ = sub.ctrl.send(ToClient::Msg(msg.clone()));
                    }
                }
                // Its config may have changed with it.
                self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
            }
            What::Attention(state, why) => self.set_attention(pane, state, &why),
            What::Event(kind) => self.emit(Some(pane), kind),
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
                    state: self.state_for(&sub.principal),
                };
                let _ = sub.ctrl.send(ToClient::Msg(hello));
                for id in self.blocks.keys().filter(|id| self.sees(&sub.principal, **id)) {
                    if let Some(msg) = self.block_msg(*id) {
                        let _ = sub.ctrl.send(ToClient::Msg(msg));
                    }
                }
                self.clients.insert(sub.client, sub);
            }
            Cmd::Disconnect { client } => {
                let gone = self.clients.remove(&client).map(|c| c.principal);
                self.focus.remove(&client);
                self.refused.remove(&client);
                self.viewing.remove(&client);
                // Their last client left: they no longer drive anything.
                if let Some(who) = gone
                    && !self.clients.values().any(|c| c.principal.id() == who.id())
                {
                    self.drivers.retain(|_, d| d.who != who.id());
                }
                if !self.clients.is_empty() {
                    self.broadcast();
                }
                for p in self.panes.values() {
                    p.detach(client);
                }
                if self.mux.release(client) {
                    self.changed();
                }
            }
            Cmd::Input { client, pane, data } => {
                let Some(c) = client else { return self.input(pane, data, None) };
                if let Some(why) = self.cant(c, &[pane], Role::Editor) {
                    return self.tell_once(c, why);
                }
                let Some(who) = self.clients.get(&c).map(|x| x.principal.clone()) else { return };
                if let Err(why) = self.may_drive_here(&who, pane) {
                    return self.tell_once(c, why);
                }
                // One driver per pane, unless it's in pair mode: the first to
                // type drives; anyone else is told how to take over.
                if !self.pair.contains(&pane) {
                    match self.drivers.get(&pane) {
                        Some(d) if d.who != who.id() => {
                            let why = format!("{} is driving this pane: take control (pane menu) or ask them", d.name);
                            return self.tell_once(c, why);
                        }
                        Some(_) => {}
                        None if self.panes.contains_key(&pane) => {
                            self.drivers.insert(pane, self.driver_of(&who));
                            self.broadcast();
                        }
                        None => {}
                    }
                }
                let name = self.name_of(&who);
                self.input(pane, data, Some(name))
            }
            Cmd::AclChanged => self.acl_changed(),
            Cmd::Msg { client, msg } => self.message(client, msg),
            Cmd::Api(api) => self.api(api),
            Cmd::MachineReset(id) => self.machine_reset(id),
            Cmd::Shutdown(_) => unreachable!("handled in run"),
        }
    }

    /// Someone typed in a pane: whatever it wanted, it has their attention.
    fn input(&mut self, pane: PaneId, data: Vec<u8>, by: Option<String>) {
        let Some(p) = self.panes.get(&pane) else { return };
        match by {
            Some(by) => p.input_by(data, by),
            None => p.input(data),
        }
        // A question open beside it still wants an answer (typing in Claude
        // Code's prompt box doesn't answer it).
        if self.asks.contains_key(&pane) {
            return;
        }
        if matches!(self.attention.get(&pane), Some(Attention::NeedsInput | Attention::Done)) {
            let next = if p.status().current.is_some() { Attention::Working } else { Attention::Idle };
            self.set_attention(pane, next, "input");
        }
    }

    fn api(&mut self, api: Api) {
        match api {
            Api::RoleOn(who, pane, reply) => {
                let r = if who.is_owner() {
                    Some((Role::Owner, None))
                } else {
                    self.session_of(pane).filter(|_| self.readable(&who, pane)).and_then(|s| {
                        let role = self.config.acl.role(&who, s)?;
                        Some((role, self.config.acl.floor(&who, s, pane)))
                    })
                };
                let _ = reply.send(r);
            }
            Api::MayDrive(who, pane, reply) => {
                let _ = reply.send(self.may_drive_here(&who, pane));
            }
            Api::SessionEnds(session, reply) => {
                let ends = self.mux.session(session).ok().map(|s| {
                    s.tabs
                        .iter()
                        .filter_map(|t| self.mux.tab(*t).ok())
                        .flat_map(|t| t.root.panes())
                        .filter_map(|p| self.panes.get(&p).map(|h| (p, h.status().end)))
                        .collect()
                });
                let _ = reply.send(ends);
            }
            Api::Panes(reply) => {
                let _ = reply.send(self.summaries());
            }
            Api::Pane(pane, reply) => {
                let _ = reply.send(self.panes.get(&pane).cloned());
            }
            Api::Attention(pane, state, reply) => {
                let known = self.panes.contains_key(&pane) || self.blocks.contains_key(&pane);
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
            Api::Open(req, who, reply) => {
                let r = match who.filter(|w| !w.is_owner()) {
                    None => self.open_block(req),
                    Some(who) => self.guest_block(req, &who),
                };
                let _ = reply.send(r);
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
            Api::Ask(pane, ask, reply) => {
                let _ = reply.send(self.ask(pane, ask));
            }
            Api::AskReply(pane, id, answer, reply) => {
                let _ = reply.send(self.ask_reply(pane, id, answer));
            }
            Api::AskWithdraw(pane, id, token) => {
                let open = self.asks.get(&pane).is_some_and(|a| {
                    id.as_ref().is_none_or(|id| *id == a.ask.id) && token.is_none_or(|t| t == a.token)
                });
                if open && let Some(a) = self.asks.remove(&pane) {
                    info!(pane, id = a.ask.id, "question withdrawn");
                    let _ = a.reply.send(AskReply::Withdrawn);
                    self.after_ask(pane);
                }
            }
        }
    }

    /// Show a terminal's question on every client, and ask for you.
    fn ask(&mut self, pane: PaneId, ask: Ask) -> Result<(u64, oneshot::Receiver<AskReply>), String> {
        if !self.panes.contains_key(&pane) {
            return Err(format!("no terminal %{pane}"));
        }
        let (tx, rx) = oneshot::channel();
        let token = self.next_ask;
        self.next_ask += 1;
        let why = ask.headline();
        info!(pane, id = ask.id, "question asked");
        // The same question again (its asker reconnected) or a newer one:
        // either way the older registration is over.
        if let Some(old) = self.asks.insert(pane, TermAsk { ask, token, reply: tx }) {
            let _ = old.reply.send(AskReply::Withdrawn);
        }
        if self.attention.get(&pane) == Some(&Attention::NeedsInput) {
            // Already asking for you (Claude Code's own hook, say): this is
            // what it wants, and the card changed.
            self.broadcast();
        } else {
            self.set_attention(pane, Attention::NeedsInput, &why);
        }
        Ok((token, rx))
    }

    fn ask_reply(&mut self, pane: PaneId, id: Option<String>, answer: AskReply) -> Result<Ask, String> {
        let a = self
            .asks
            .get(&pane)
            .filter(|a| id.as_ref().is_none_or(|id| *id == a.ask.id))
            .ok_or_else(|| format!("no open question in %{pane} (it was answered, or withdrawn)"))?;
        let ask = a.ask.clone();
        let a = self.asks.remove(&pane).expect("just found");
        info!(pane, id = ask.id, ?answer, "question answered");
        let terminal = answer == AskReply::Terminal;
        let _ = a.reply.send(answer);
        if terminal {
            // It asks again in the terminal: still wants you.
            self.broadcast();
        } else {
            self.after_ask(pane);
        }
        Ok(ask)
    }

    /// A terminal's question went: back to work, and its card off every
    /// client.
    fn after_ask(&mut self, pane: PaneId) {
        if self.attention.get(&pane) == Some(&Attention::NeedsInput) {
            self.set_attention(pane, Attention::Working, "answered");
        } else {
            self.broadcast();
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
        // Joining a split pane's host: its tab's machine (which a split
        // takes anyway), or a sandbox it has a shell on (borrowed again).
        let join = match req.split.filter(|_| req.join) {
            Some(pane) => self.join_host(pane)?,
            None => Join::Here,
        };
        let host = match (&req.sandbox, &join) {
            (Some(sandbox), _) => Some(self.borrow_machine(sandbox)?),
            (None, Join::Borrow(sprite)) => Some(self.borrow_machine(&sprite.clone())?),
            (None, _) if req.vm || req.vm_tab => Some(self.new_machine(req.image.clone())?),
            (None, _) => None,
        };
        let on_machine = host.is_some() || matches!(join, Join::TabMachine);
        // A shell started in a directory: this host's (the default is the
        // pane it came from), or the machine's when one is given.
        self.next_cwd = match (&req.command, on_machine) {
            (Some(_), _) => None,
            (None, true) => req.cwd.clone().map(PathBuf::from),
            (None, false) => req.cwd.is_some().then(|| cwd.clone()),
        };
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
            // Here: a script's command is for this host, even in a VM tab
            // (unless it asked to join the pane's machine).
            (Some(pane), _) => {
                let local = !matches!(join, Join::TabMachine);
                Intent::Split { pane, edge: illogical_proto::Edge::Right, local, cwd: None }
            }
            (None, Some(session)) => Intent::NewTab { session, from_pane: from, cwd: None },
            (None, None) => Intent::NewSession { name: None, from_pane: from },
        };
        let result = self.intent(None, intent);
        self.next_spawn = None;
        self.next_owner_tab = false;
        self.next_cwd = None;
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

    /// Where a pane joining `pane`'s host runs (`run --split --join`).
    fn join_host(&self, pane: PaneId) -> Result<Join, String> {
        let Some(m) = self.machine_of(pane) else { return Ok(Join::Here) };
        if m.borrowed {
            return Ok(Join::Borrow(m.sprite.clone()));
        }
        let tab = self.mux.tab_of(pane).map_err(|e| e.to_string())?;
        if self.tab_machine(tab) == Some(m.id) {
            return Ok(Join::TabMachine);
        }
        Err(format!("%{pane}'s machine is its own: share it with the tab first (Share machine with tab)"))
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
    /// A guest's block (M14): an agent, beside a pane in a session they
    /// edit, on a VM of their own (within their quota).
    fn guest_block(&mut self, mut req: OpenRequest, who: &Principal) -> Result<PaneId, String> {
        if req.kind != BlockType::Agent {
            return Err("guests can start agents; other blocks are the owner's".into());
        }
        let pane = req.split.or(req.from_pane).ok_or("start it beside a pane")?;
        let session = self.session_of(pane).ok_or("no such pane")?;
        if self.config.acl.role(who, session).is_none_or(|r| r < Role::Editor) {
            return Err("you can't start agents in this session".into());
        }
        let mine = self.machines.values().filter(|m| m.by.as_deref() == Some(who.id())).count();
        if mine >= self.config.guest_machines {
            return Err(format!("you have {mine} VMs here, the most a guest may have: close one first"));
        }
        (req.vm, req.local, req.host, req.session) = (true, false, None, None);
        let before: Vec<MachineId> = self.machines.keys().copied().collect();
        let block = self.open_block(req)?;
        for m in self.machines.values_mut().filter(|m| !before.contains(&m.id)) {
            m.by = Some(who.id().to_owned());
        }
        Ok(block)
    }

    fn open_block(&mut self, req: OpenRequest) -> Result<PaneId, String> {
        if req.kind == BlockType::Terminal {
            return Err("terminals are opened with run".into());
        }
        let from = req.from_pane.filter(|p| self.panes.contains_key(p) || self.blocks.contains_key(p));
        let session = self.resolve_session(req.session.as_deref(), from)?;
        let before: Vec<PaneId> = self.mux.panes();
        self.last_block_error = None;
        // On a new machine of its own, or one that exists (a tab's).
        if let Some(m) = req.host
            && !self.machines.contains_key(&m)
        {
            return Err(format!("no machine m{m}"));
        }
        self.next_host = match (req.vm, req.host) {
            (true, _) => Some(self.new_machine(req.image.clone())?),
            (false, host) => host,
        };
        let made = req.vm.then_some(self.next_host).flatten();
        // Here if asked; an agent also runs here unless asked for a machine;
        // a page in a VM tab is the tab's machine's.
        let local = req.local || (req.kind == BlockType::Agent && self.next_host.is_none());
        self.next_block = Some((req.kind, req.config));
        let intent = match (req.split, session) {
            (Some(pane), _) => Intent::Split { pane, edge: illogical_proto::Edge::Right, local, cwd: None },
            (None, Some(session)) => Intent::NewTab { session, from_pane: from, cwd: None },
            (None, None) => Intent::NewSession { name: None, from_pane: from },
        };
        let result = self.intent(None, intent);
        let unused = self.next_block.take();
        if self.next_host.take().is_some()
            && let Some(m) = made
        {
            // Nothing took it.
            self.machines.remove(&m);
        }
        result?;
        if let Some((kind, _)) = unused {
            return Err(format!("no {kind:?} block was made").to_lowercase());
        }
        let made = self.mux.panes().into_iter().find(|p| !before.contains(p));
        let Some(id) = made else {
            return Err(self.last_block_error.take().unwrap_or_else(|| "no block was made".into()));
        };
        if !self.blocks.contains_key(&id) {
            return Err(self.last_block_error.take().unwrap_or_else(|| "the block couldn't start".into()));
        }
        Ok(id)
    }

    /// A new machine for the next pane; it's created when its first
    /// program starts.
    fn new_machine(&mut self, image: Option<String>) -> Result<MachineId, String> {
        if self.config.provider.is_none() {
            return Err("VM panes aren't set up: illogicald found no wisp token (see --wisp-token-file)".into());
        }
        let id = self.next_machine;
        let sprite = format!("{}{id}", self.config.sprite_prefix());
        Ok(self.add_machine(sprite, image, false))
    }

    /// Someone else's sandbox, borrowed for the next pane's shell ("open
    /// shell", M4b): never created, reset or deleted by us.
    fn borrow_machine(&mut self, sprite: &str) -> Result<MachineId, String> {
        if self.config.provider.is_none() {
            return Err("no sandbox provider: illogicald found no wisp token (see --wisp-token-file)".into());
        }
        if sprite.starts_with(&self.config.sprite_prefix()) {
            return Err(format!("{sprite} is one of this daemon's own machines"));
        }
        Ok(self.add_machine(sprite.to_owned(), None, true))
    }

    fn add_machine(&mut self, sprite: String, image: Option<String>, borrowed: bool) -> MachineId {
        let id = self.next_machine;
        self.next_machine += 1;
        let provider = self.config.provider.as_ref().map_or("wisp", |p| p.name()).to_owned();
        let owner = Owner::Pane(0);
        // Ours get a name to show; a borrowed sandbox has its own.
        let name = (!borrowed).then(|| {
            let taken = |n: &str| self.machines.values().any(|m| m.name.as_deref() == Some(n));
            illogical_core::names::generate(seed(), taken)
        });
        let state = MachineState::Starting;
        let m = Machine { id, provider, sprite, name, image, owner, state, borrowed, by: None };
        self.machines.insert(id, m);
        id
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
        if m.borrowed {
            return Err(format!("{} isn't ours to reset", m.sprite));
        }
        m.state = MachineState::Starting;
        let sprite = m.sprite.clone();
        let provider = self.config.provider.clone().ok_or("VM panes aren't set up")?;
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
            if let Err(e) = provider.delete(&sprite).await {
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
        if m.borrowed {
            // Its shells were hung up as their panes closed.
            return info!(machine = m.id, sprite = m.sprite, "let go of a borrowed machine");
        }
        let Some(provider) = self.config.provider.clone() else { return };
        tokio::spawn(async move {
            match provider.delete(&m.sprite).await {
                Ok(()) => info!(machine = m.id, sprite = m.sprite, "deleted machine"),
                Err(e) => warn!(machine = m.id, sprite = m.sprite, error = %e, "can't delete machine"),
            }
        });
    }

    /// Delete our sprites that no machine owns: left by a crash, or by a
    /// pane closed while the daemon was down.
    fn sweep_machines(&self) {
        let Some(provider) = self.config.provider.clone() else { return };
        let prefix = self.config.sprite_prefix();
        let keep: Vec<String> = self.machines.values().map(|m| m.sprite.clone()).collect();
        tokio::spawn(async move {
            let names = match provider.list(&prefix).await {
                Ok(n) => n.into_iter().map(|s| s.name).collect::<Vec<_>>(),
                Err(e) => return warn!(error = %e, "can't list machines to sweep"),
            };
            for name in names.into_iter().filter(|n| !keep.contains(n)) {
                match provider.delete(&name).await {
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
        let who = sub.principal.clone();
        match msg {
            ClientMsg::Attach { panes } => {
                for a in panes {
                    if !self.readable(&who, a.pane) {
                        continue;
                    }
                    let floor = self.session_of(a.pane).and_then(|s| self.config.acl.floor(&who, s, a.pane));
                    if let Some(p) = self.panes.get(&a.pane) {
                        match floor {
                            Some(f) => p.attach_from(sub.clone(), a.offset, f),
                            None => p.attach(sub.clone(), a.offset),
                        }
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
                if self.viewing.insert(client, tab) != Some(tab) {
                    self.broadcast();
                }
                // Only editors size a tab; viewers letterbox.
                let editor = who.is_owner()
                    || self
                        .mux
                        .session_of_tab(tab)
                        .ok()
                        .and_then(|s| self.config.acl.role(&who, s))
                        .is_some_and(|r| r >= Role::Editor);
                if !editor {
                    return;
                }
                if let Ok(true) = self.mux.view(client, tab, cols, rows, zoom, claim) {
                    self.changed();
                }
            }
            ClientMsg::Intent { id, intent } => {
                if !who.is_owner() {
                    let allowed = illogical_core::access::need(&self.mux, &intent)
                        .map(|n| n.allowed(|s| self.config.acl.role(&who, s)))
                        .unwrap_or(false);
                    if !allowed {
                        let message = "you can't do that here (ask the owner for more access)".to_owned();
                        let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id, message }));
                        return;
                    }
                }
                let done = if who.is_owner() {
                    self.intent(Some(client), intent)
                } else {
                    self.guest_intent(client, &who, intent)
                };
                if let Err(message) = done {
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id, message }));
                }
            }
            ClientMsg::Ping { id } => {
                let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Pong { id }));
            }
            ClientMsg::Focus { pane } => {
                let before = self.focus.get(&client).copied();
                match pane.filter(|p| self.sees(&who, *p)) {
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
                if self.focus.get(&client).copied() != before {
                    self.broadcast();
                }
            }
            ClientMsg::Pane { pane, op } => {
                if let Some(why) = self.cant(client, &[pane], Role::Editor) {
                    let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message: why }));
                    return;
                }
                if self.control_op(client, &who, pane, &op) {
                    return;
                }
                // Blocks of other types take what applies to them.
                if self.blocks.contains_key(&pane) {
                    match op {
                        PaneOp::Attention { state } => self.set_attention(pane, state, "set by a client"),
                        PaneOp::SetPolicy { policy } => {
                            info!(pane, ?policy, "restart policy");
                            self.meta.entry(pane).or_default().policy = policy;
                        }
                        _ => {}
                    }
                    self.changed();
                    return;
                }
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
                    // Driving: handled before this.
                    PaneOp::TakeControl
                    | PaneOp::RequestControl
                    | PaneOp::GiveControl { .. }
                    | PaneOp::ReleaseControl
                    | PaneOp::SetPair { .. }
                    | PaneOp::RequestTrust
                    | PaneOp::GrantTrust { .. }
                    | PaneOp::RevokeTrust { .. }
                    | PaneOp::SetPrivate { .. } => {}
                }
                self.changed();
            }
        }
    }

    fn intent(&mut self, client: Option<ClientId>, intent: Intent) -> Result<(), String> {
        // A new session without a name gets one ("drifting cedar").
        let intent = match intent {
            Intent::NewSession { name: None, from_pane } => {
                let taken = |n: &str| self.mux.sessions.iter().any(|s| s.name == n);
                Intent::NewSession { name: Some(illogical_core::names::generate(seed(), taken)), from_pane }
            }
            i => i,
        };
        let refused = |why: String| {
            info!(?client, ?intent, why, "intent refused");
            why
        };
        self.check_move(&intent).map_err(refused)?;
        let local = matches!(intent, Intent::Split { local: true, .. });
        let before: Vec<SessionId> = self.mux.sessions.iter().map(|s| s.id).collect();
        let effects = self.mux.apply(intent.clone()).map_err(|e| refused(e.to_string()))?;
        for gone in before.into_iter().filter(|s| self.mux.session(*s).is_err()) {
            self.config.acl.forget_session(gone);
        }
        info!(?client, ?intent, "intent");
        let rects = self.mux.pane_rects();
        for e in effects {
            match e {
                Effect::Spawn { pane, cwd_from, cwd } => {
                    let from_meta = cwd_from.and_then(|p| self.meta.get(&p)).and_then(|m| m.integration);
                    let integrate = from_meta.unwrap_or(true);
                    let asked = self.next_cwd.take();
                    // A directory asked for (if it exists), else the source
                    // pane's, else home.
                    let cwd = cwd
                        .map(PathBuf::from)
                        .filter(|d| d.is_dir())
                        .or_else(|| cwd_from.and_then(|p| self.panes.get(&p)?.cwd()))
                        .unwrap_or_else(|| self.config.home.clone());
                    let (cols, rows) = rects.get(&pane).map(|r| (r.cols, r.rows)).unwrap_or((80, 24));
                    // A machine made for it, else its tab's (unless asked
                    // for this host).
                    let tab = self.mux.tab_of(pane).ok();
                    let made = self.next_host.take();
                    let host = made.or_else(|| tab.filter(|_| !local).and_then(|t| self.tab_machine(t)));
                    if let Some((kind, config)) = self.next_block.take() {
                        // A machine made for it is its own.
                        if let Some(m) = made.and_then(|m| self.machines.get_mut(&m))
                            && m.owner == Owner::Pane(0)
                        {
                            m.owner = Owner::Pane(pane);
                        }
                        match self.make_block(pane, kind, config.clone(), host, None) {
                            Ok(()) => {
                                let meta = PaneMeta { host, kind, config: Some(config), ..Default::default() };
                                self.meta.insert(pane, meta);
                                self.emit(Some(pane), EventKind::Opened);
                            }
                            Err(e) => {
                                warn!(block = pane, ?kind, error = %e, "could not start block");
                                self.last_block_error = Some(e);
                                if let Some(m) = made
                                    && self.machines.get(&m).is_some_and(|x| x.owner == Owner::Pane(pane))
                                {
                                    self.machines.remove(&m);
                                }
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
                            let cwd = match (host, asked.clone()) {
                                (None, Some(c)) => c,
                                _ => cwd,
                            };
                            let shell = match host {
                                Some(_) => self.config.guest_shell(pane, integrate, asked),
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
                    if let Some(a) = self.asks.remove(&pane) {
                        let _ = a.reply.send(AskReply::Withdrawn);
                    }
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
        let mut states: HashMap<&Principal, State> = HashMap::new();
        for sub in self.clients.values() {
            let state = states.entry(&sub.principal).or_insert_with(|| self.state_for(&sub.principal));
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::State { state: state.clone() }));
        }
    }

    // ---- who may see and do what (M12)

    /// The session a pane or block is in.
    fn session_of(&self, pane: PaneId) -> Option<SessionId> {
        self.mux.tab_of(pane).and_then(|t| self.mux.session_of_tab(t)).ok()
    }

    fn sees(&self, who: &Principal, pane: PaneId) -> bool {
        who.is_owner() || self.session_of(pane).and_then(|s| self.config.acl.role(who, s)).is_some()
    }

    /// Why `client` can't act on `panes` with `role` (`None`: it can).
    fn cant(&self, client: ClientId, panes: &[PaneId], role: Role) -> Option<String> {
        let who = &self.clients.get(&client)?.principal;
        if who.is_owner() {
            return None;
        }
        for p in panes {
            match self.session_of(*p).and_then(|s| self.config.acl.role(who, s)) {
                Some(r) if r >= role => {}
                Some(_) => return Some("you're watching this session; you can't type or change it".into()),
                None => return Some(format!("no pane %{p}")),
            }
        }
        None
    }

    fn name_of(&self, who: &Principal) -> String {
        match who {
            Principal::Owner => self.config.owner_name.clone(),
            Principal::User { name, .. } => name.clone(),
        }
    }

    fn driver_of(&self, who: &Principal) -> Driver {
        Driver { who: who.id().to_owned(), name: self.name_of(who) }
    }

    fn tell(&self, who: &str, msg: ServerMsg) {
        for c in self.clients.values().filter(|c| c.principal.id() == who) {
            let _ = c.ctrl.send(ToClient::Msg(msg.clone()));
        }
    }

    /// Driving a pane (M13). True if `op` was one of these.
    fn control_op(&mut self, client: ClientId, who: &Principal, pane: PaneId, op: &PaneOp) -> bool {
        let me = self.driver_of(who);
        let title = format!("%{pane}");
        match op {
            PaneOp::TakeControl => {
                if let Some(prev) = self.drivers.insert(pane, me.clone())
                    && prev.who != me.who
                {
                    let message = format!("{} took control of {title}", me.name);
                    self.tell(&prev.who, ServerMsg::Notice { message });
                }
                info!(pane, who = me.who, "took control");
            }
            PaneOp::RequestControl => match self.drivers.get(&pane).cloned() {
                Some(d) if d.who != me.who && self.clients.values().any(|c| c.principal.id() == d.who) => {
                    self.tell(&d.who, ServerMsg::ControlRequest { pane, who: me.who.clone(), name: me.name.clone() });
                    if let Some(c) = self.clients.get(&client) {
                        let message = format!("asked {} for control of {title}", d.name);
                        let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Notice { message }));
                    }
                    return true;
                }
                // Nobody (here) drives it: just take it.
                _ => {
                    self.drivers.insert(pane, me);
                }
            },
            PaneOp::GiveControl { to } => {
                let current = self.drivers.get(&pane).map(|d| d.who.clone());
                if current.as_deref().is_some_and(|c| c != me.who) && !who.is_owner() {
                    if let Some(c) = self.clients.get(&client) {
                        let message = "only whoever drives it can hand it over".to_owned();
                        let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
                    }
                    return true;
                }
                let Some(to_who) = self.clients.values().map(|c| c.principal.clone()).find(|p| p.id() == to) else {
                    return true;
                };
                let next = self.driver_of(&to_who);
                let message = format!("{} handed you control of {title}", me.name);
                self.drivers.insert(pane, next);
                self.tell(to, ServerMsg::Notice { message });
            }
            PaneOp::ReleaseControl => {
                if self.drivers.get(&pane).is_some_and(|d| d.who == me.who) {
                    self.drivers.remove(&pane);
                }
            }
            PaneOp::SetPair { on } => {
                if *on {
                    self.pair.insert(pane);
                } else {
                    self.pair.remove(&pane);
                }
            }
            PaneOp::RequestTrust => {
                let msg = ServerMsg::TrustRequest { pane, who: me.who.clone(), name: me.name.clone() };
                self.tell("owner", msg);
                // The owner may only have a phone in their pocket.
                if let Some(push) = &self.push {
                    let body = format!("{} asks to drive %{pane}, which runs on this machine", me.name);
                    push.send(
                        pane,
                        "Someone asks to drive a pane",
                        &body,
                        Some(serde_json::json!({ "trust": me.who })),
                    );
                }
                let body = format!("{} asks to drive %{pane}, which runs on this machine", me.name);
                self.config.control.push(pane, "Someone asks to drive a pane", &body, None, |who| who.is_owner());
                if let Some(c) = self.clients.get(&client) {
                    let message = "asked the owner to trust you with it".to_owned();
                    let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Notice { message }));
                }
                return true;
            }
            PaneOp::GrantTrust { .. } | PaneOp::RevokeTrust { .. } if !who.is_owner() => {
                self.refuse_to(client, "only the owner trusts people with panes on this machine");
                return true;
            }
            PaneOp::GrantTrust { to, minutes } => {
                let minutes = (*minutes).clamp(1, 24 * 60);
                let until = now_ms() + u64::from(minutes) * 60_000;
                self.trust.insert((pane, to.clone()), until);
                info!(pane, to, minutes, "trusted with a local pane");
                let message = format!("you may drive %{pane} for {minutes} minutes");
                self.tell(to, ServerMsg::Notice { message });
                let expire = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(u64::from(minutes) * 60_000 + 50)).await;
                    // Show everyone it ended.
                    let _ = expire.send(Cmd::AclChanged);
                });
            }
            PaneOp::RevokeTrust { to } => {
                self.trust.remove(&(pane, to.clone()));
                if self.drivers.get(&pane).is_some_and(|d| &d.who == to) {
                    self.drivers.remove(&pane);
                }
            }
            PaneOp::SetPrivate { on } => {
                if !who.is_owner() {
                    self.refuse_to(client, "only the owner makes a pane private");
                    return true;
                }
                self.meta.entry(pane).or_default().private = *on;
                if *on {
                    // Whoever else watches it stops getting it.
                    let others: Vec<ClientId> =
                        self.clients.values().filter(|c| !c.principal.is_owner()).map(|c| c.client).collect();
                    if let Some(p) = self.panes.get(&pane) {
                        for c in others {
                            p.detach(c);
                        }
                    }
                }
                self.save_due.get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
            }
            _ => return false,
        }
        self.broadcast();
        true
    }

    fn refuse_to(&self, client: ClientId, why: &str) {
        if let Some(c) = self.clients.get(&client) {
            let _ = c.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message: why.to_owned() }));
        }
    }

    /// A guest may drive a pane on a machine of its own; one on this
    /// machine only while the owner trusts them with it (M14).
    fn may_drive_here(&self, who: &Principal, pane: PaneId) -> Result<(), String> {
        if who.is_owner() || self.machine_of(pane).is_some() || self.blocks.contains_key(&pane) {
            return Ok(());
        }
        // A team's machine is the team's: its members drive it by their
        // team role, no one person's trust needed (M19).
        if self.config.acl.team_role(who).is_some_and(|r| r >= Role::Editor) {
            return Ok(());
        }
        let until = self.trust.get(&(pane, who.id().to_owned())).copied().unwrap_or(0);
        if until > now_ms() {
            return Ok(());
        }
        Err(format!(
            "%{pane} runs on {}'s own machine: ask them to trust you with it (pane menu), or work in a VM tab",
            self.config.owner_name
        ))
    }

    /// Someone other than the owner may read this pane (not private).
    fn readable(&self, who: &Principal, pane: PaneId) -> bool {
        who.is_owner() || (self.sees(who, pane) && !self.meta.get(&pane).is_some_and(|m| m.private))
    }

    /// A guest's intent that makes a pane: on a VM, never this machine
    /// (M14). A new tab is a VM tab; a split joins its tab's machine, or
    /// gets one of its own. Their VMs count against their quota.
    fn guest_intent(&mut self, client: ClientId, who: &Principal, intent: Intent) -> Result<(), String> {
        let vm = match &intent {
            Intent::NewTab { .. } => Some(true),
            Intent::Split { pane, .. } => {
                let tab = self.mux.tab_of(*pane).map_err(|e| e.to_string())?;
                if self.tab_machine(tab).is_some() { None } else { Some(false) }
            }
            _ => None,
        };
        let intent = match intent {
            Intent::Split { pane, edge, cwd, .. } => Intent::Split { pane, edge, local: false, cwd },
            i => i,
        };
        let Some(tab) = vm else { return self.intent(Some(client), intent) };
        let mine = self.machines.values().filter(|m| m.by.as_deref() == Some(who.id())).count();
        if mine >= self.config.guest_machines {
            return Err(format!("you have {mine} VMs here, the most a guest may have: close one first",));
        }
        let m = self.new_machine(None)?;
        if let Some(machine) = self.machines.get_mut(&m) {
            machine.by = Some(who.id().to_owned());
        }
        self.next_host = Some(m);
        self.next_owner_tab = tab;
        let r = self.intent(Some(client), intent);
        self.next_owner_tab = false;
        if let Some(m) = self.next_host.take() {
            self.machines.remove(&m);
        }
        r
    }

    /// Who is connected and where they look, within what `viewer` sees.
    fn presence(&self, viewer: &Principal) -> Vec<Presence> {
        let mut out: Vec<Presence> = self
            .clients
            .values()
            .map(|c| Presence {
                client: c.client,
                who: c.principal.id().to_owned(),
                name: self.name_of(&c.principal),
                pic: match &c.principal {
                    Principal::User { pic, .. } => pic.clone(),
                    Principal::Owner => self.config.owner_pic.clone(),
                },
                tab: self.viewing.get(&c.client).copied(),
                pane: self.focus.get(&c.client).copied(),
            })
            .filter(|p| {
                viewer.is_owner()
                    || p.who == viewer.id()
                    || p.tab
                        .and_then(|t| self.mux.session_of_tab(t).ok())
                        .and_then(|s| self.config.acl.role(viewer, s))
                        .is_some()
            })
            .map(|mut p| {
                // Only where the viewer can see.
                if !viewer.is_owner() {
                    p.pane = p.pane.filter(|x| self.sees(viewer, *x));
                    p.tab = p.tab.filter(|t| {
                        self.mux.session_of_tab(*t).ok().and_then(|s| self.config.acl.role(viewer, s)).is_some()
                    });
                }
                p
            })
            .collect();
        out.sort_by_key(|p| p.client);
        out
    }

    fn tell_once(&mut self, client: ClientId, message: String) {
        let now = Instant::now();
        if self.refused.get(&client).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(5)) {
            return;
        }
        self.refused.insert(client, now);
        if let Some(sub) = self.clients.get(&client) {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
        }
    }

    /// Grants changed: hang up on whoever has nothing left, let go of panes
    /// they no longer see, and show everyone their state.
    fn acl_changed(&mut self) {
        let acl = self.config.acl.clone();
        let gone: Vec<ClientId> =
            self.clients.iter().filter(|(_, c)| !acl.knows(&c.principal)).map(|(id, _)| *id).collect();
        for id in gone {
            if let Some(sub) = self.clients.remove(&id) {
                info!(client = id, who = sub.principal.id(), "access revoked: disconnecting");
                let message = "your access was removed".to_owned();
                let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::Error { id: None, message }));
                let _ = sub.ctrl.send(ToClient::Close);
            }
            self.focus.remove(&id);
            for p in self.panes.values() {
                p.detach(id);
            }
            self.mux.release(id);
        }
        let hidden: Vec<(ClientId, PaneId)> = self
            .clients
            .values()
            .flat_map(|c| self.panes.keys().filter(|p| !self.sees(&c.principal, **p)).map(|p| (c.client, *p)))
            .collect();
        for (client, pane) in hidden {
            if let Some(p) = self.panes.get(&pane) {
                p.detach(client);
            }
        }
        self.broadcast();
    }

    /// What `who` sees: everything for the owner; for anyone else only the
    /// sessions granted to them, their tabs, panes and machines, and their
    /// role in each.
    fn state_for(&self, who: &Principal) -> State {
        let mut st = self.state();
        st.presence = self.presence(who);
        if who.is_owner() {
            return st;
        }
        // A grant per session, or a team role on all of them (M19).
        let roles: BTreeMap<SessionId, Role> =
            st.sessions.iter().filter_map(|s| Some((s.id, self.config.acl.role(who, s.id)?))).collect();
        st.sessions.retain(|s| roles.contains_key(&s.id));
        let tabs: std::collections::HashSet<TabId> = st.sessions.iter().flat_map(|s| s.tabs.iter().copied()).collect();
        st.tabs.retain(|t| tabs.contains(&t.id));
        let panes: std::collections::HashSet<PaneId> = st.tabs.iter().flat_map(|t| t.root.panes()).collect();
        st.panes.retain(|p| panes.contains(&p.id));
        let machines: std::collections::HashSet<MachineId> = st.panes.iter().filter_map(|p| p.host).collect();
        st.machines.retain(|m| machines.contains(&m.id));
        st.roles = Some(roles.into_iter().collect());
        st
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
            ask: self.asks.get(&p.id).map(|a| a.ask.clone()),
            driver: self.drivers.get(&p.id).cloned(),
            pair: self.pair.contains(&p.id),
            private: meta.private,
            trusted: {
                let now = now_ms();
                let mut t: Vec<(String, u64)> = self
                    .trust
                    .iter()
                    .filter(|((pane, _), until)| *pane == p.id && **until > now)
                    .map(|((_, w), u)| (w.clone(), *u))
                    .collect();
                t.sort();
                t
            },
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
            ask: None,
            driver: None,
            pair: false,
            private: meta.private,
            trusted: Vec::new(),
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
        let options = Box::new(self.mux.options.clone());
        State {
            rev: self.mux.rev,
            sessions: self.mux.sessions.clone(),
            tabs,
            panes,
            machines,
            options,
            roles: None,
            presence: Vec::new(),
        }
    }
}
