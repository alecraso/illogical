//! The multiplexer task: owns the layout (`illogical_core::Mux`), the panes,
//! the connected clients and what's saved to disk. Every client message goes
//! through here, so layout changes, pane starts and stops, resizes and saves
//! happen in one order.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    time::Duration,
};

use illogical_core::{Effect, Intent, Mux};
use illogical_proto::{
    ClientId, ClientMsg, PaneId, PaneInfo, PaneOp, Policy, ServerMsg, State, TabView,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::{Instant, sleep_until},
};
use tracing::{info, warn};

use crate::{
    pane::{self, Exit, ExitSink, PaneHandle, Setup, Spawn, Start, Subscriber, ToClient},
    store::{LAYOUT_VERSION, PaneLog, PaneMeta, Saved, StateDir, now_ms},
    sys,
};

/// Layout changes are saved this long after the last one.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(250);
/// Working directories and foreground commands change without layout
/// changes; look at them this often.
const REFRESH: Duration = Duration::from_secs(5);

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
    /// Checkpoint every pane and save the layout, then reply.
    Shutdown(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct MuxHandle {
    tx: mpsc::UnboundedSender<Cmd>,
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
}

impl Config {
    fn env(&self) -> Vec<(String, String)> {
        if self.manager_env {
            sys::manager_env()
        } else {
            vec![]
        }
    }

    fn shell(&self, cwd: PathBuf) -> Spawn {
        Spawn {
            program: self.shell.clone(),
            args: self.shell_args.clone(),
            cwd,
            env: self.env(),
        }
    }

    /// Run `command`, then carry on with an interactive shell in the pane.
    fn run(&self, cwd: PathBuf, command: &str) -> Spawn {
        let mut args = self.shell_args.clone();
        let then =
            std::iter::once(self.shell.as_str()).chain(self.shell_args.iter().map(String::as_str));
        args.extend([
            "-c".into(),
            format!("{command}; exec {}", then.collect::<Vec<_>>().join(" ")),
        ]);
        Spawn {
            program: self.shell.clone(),
            args,
            cwd,
            env: self.env(),
        }
    }

    /// What a restored pane does, by its policy.
    fn restore(&self, meta: &PaneMeta) -> Start {
        let cwd = meta
            .cwd
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.home.clone());
        let shell = self.shell(cwd.clone());
        let note = |s: &str| format!("\x1b[2m[{s}]\x1b[0m\r\n");
        match (&meta.policy, &meta.command) {
            (Policy::None, _) => Start::Wait {
                banner: note("press Enter for a shell"),
                enter: shell,
                escape: None,
            },
            (Policy::Rerun { confirm: true }, Some(cmd)) => Start::Wait {
                banner: note(&format!("press Enter to re-run: {cmd}  ·  Esc for a shell")),
                enter: self.run(cwd, cmd),
                escape: Some(shell),
            },
            (Policy::Rerun { confirm: false }, Some(cmd)) => Start::Now(self.run(cwd, cmd)),
            (Policy::Hook { command }, _) => Start::Now(self.run(cwd, command)),
            (Policy::Shell | Policy::Rerun { .. }, _) => Start::Now(shell),
        }
    }
}

struct Daemon {
    mux: Mux,
    panes: HashMap<PaneId, PaneHandle>,
    meta: HashMap<PaneId, PaneMeta>,
    clients: HashMap<ClientId, Subscriber>,
    /// Size each pane was last given.
    sizes: BTreeMap<PaneId, (u16, u16)>,
    config: Config,
    store: StateDir,
    exits: ExitSink,
    save_due: Option<Instant>,
    last_saved: Option<(Mux, BTreeMap<PaneId, PaneMeta>)>,
    shutting_down: bool,
}

pub fn start(config: Config, store: StateDir) -> MuxHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (exits, exits_rx) = mpsc::unbounded_channel();
    let mut d = Daemon {
        mux: Mux::new(),
        panes: HashMap::new(),
        meta: HashMap::new(),
        clients: HashMap::new(),
        sizes: BTreeMap::new(),
        config,
        store,
        exits,
        save_due: None,
        last_saved: None,
        shutting_down: false,
    };
    if !d.restore() {
        // Something to attach to on first start.
        if let Err(e) = d.intent(
            None,
            Intent::NewSession {
                name: None,
                from_pane: None,
            },
        ) {
            warn!(error = %e, "could not create the first session");
        }
    }
    tokio::spawn(d.run(rx, exits_rx));
    MuxHandle { tx }
}

impl Daemon {
    /// Bring back the saved layout and every pane in it. False if there was
    /// nothing (usable) to restore.
    fn restore(&mut self) -> bool {
        let saved = match self.store.load_layout() {
            Ok(Some(saved)) => saved,
            Ok(None) => return false,
            Err(e) => {
                let aside = self
                    .store
                    .root()
                    .join(format!("layout.json.unreadable-{}", now_ms()));
                warn!(error = %e, aside = %aside.display(), "can't read the saved layout; starting fresh");
                let _ = std::fs::rename(self.store.root().join("layout.json"), aside);
                return false;
            }
        };
        let Saved {
            mux, panes: meta, ..
        } = saved;
        self.mux = mux;
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
            let cwd = meta
                .cwd
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| self.config.home.clone());
            match self.open_pane(id, cols, rows, true, self.config.restore(&meta), cwd) {
                Ok(()) => {
                    self.meta.insert(id, meta);
                }
                Err(e) => {
                    warn!(pane = id, error = %e, "can't restore pane; dropping it");
                    let _ = self.mux.apply(Intent::ClosePane { pane: id });
                }
            }
        }
        info!(
            sessions = self.mux.sessions.len(),
            panes = self.panes.len(),
            "restored"
        );
        if self.mux.sessions.is_empty() {
            return false;
        }
        self.last_saved = Some((self.mux.clone(), self.meta.clone().into_iter().collect()));
        true
    }

    fn open_pane(
        &mut self,
        id: PaneId,
        cols: u16,
        rows: u16,
        restore: bool,
        start: Start,
        cwd: PathBuf,
    ) -> std::io::Result<()> {
        let log = PaneLog::open(self.store.pane_dir(id))?;
        let h = pane::spawn_pane(Setup {
            id,
            cols,
            rows,
            log,
            restore,
            start,
            shell: self.config.shell(cwd),
            on_exit: self.exits.clone(),
        })?;
        self.panes.insert(id, h);
        self.sizes.insert(id, (cols, rows));
        Ok(())
    }

    async fn run(
        mut self,
        mut rx: mpsc::UnboundedReceiver<Cmd>,
        mut exits: mpsc::UnboundedReceiver<Exit>,
    ) {
        let mut refresh = tokio::time::interval(REFRESH);
        loop {
            let due = self
                .save_due
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Shutdown(done)) => {
                        self.shutdown();
                        let _ = done.send(());
                    }
                    Some(cmd) => self.handle(cmd),
                    None => return,
                },
                Some(exit) = exits.recv() => self.exited(exit),
                _ = sleep_until(due), if self.save_due.is_some() => {
                    self.save_due = None;
                    self.save();
                }
                _ = refresh.tick() => {
                    if self.refresh_meta() {
                        // Clients show working directories and commands too.
                        self.broadcast();
                    }
                    self.save();
                }
            }
        }
    }

    fn exited(&mut self, exit: Exit) {
        if self.shutting_down {
            return;
        }
        if exit.close {
            info!(pane = exit.pane, code = ?exit.code, "pane exited; closing it");
            let _ = self.intent(None, Intent::ClosePane { pane: exit.pane });
        } else {
            // Started or stopped without going away.
            self.changed();
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
                self.clients.insert(sub.client, sub);
            }
            Cmd::Disconnect { client } => {
                self.clients.remove(&client);
                for p in self.panes.values() {
                    p.detach(client);
                }
                if self.mux.release(client) {
                    self.changed();
                }
            }
            Cmd::Input { pane, data } => {
                if let Some(p) = self.panes.get(&pane) {
                    p.input(data);
                }
            }
            Cmd::Msg { client, msg } => self.message(client, msg),
            Cmd::Shutdown(_) => unreachable!("handled in run"),
        }
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
            ClientMsg::View {
                tab,
                cols,
                rows,
                zoom,
                claim,
            } => {
                if let Ok(true) = self.mux.view(client, tab, cols, rows, zoom, claim) {
                    self.changed();
                }
            }
            ClientMsg::Intent { id, intent } => {
                if let Err(message) = self.intent(Some(client), intent) {
                    let _ = sub
                        .ctrl
                        .send(ToClient::Msg(ServerMsg::Error { id, message }));
                }
            }
            ClientMsg::Pane { pane, op } => {
                let Some(handle) = self.panes.get(&pane) else {
                    let message = format!("no pane %{pane}");
                    let _ = sub
                        .ctrl
                        .send(ToClient::Msg(ServerMsg::Error { id: None, message }));
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
                }
                self.changed();
            }
        }
    }

    fn intent(&mut self, client: Option<ClientId>, intent: Intent) -> Result<(), String> {
        let effects = self.mux.apply(intent.clone()).map_err(|e| e.to_string())?;
        info!(?client, ?intent, "intent");
        let rects = self.mux.pane_rects();
        for e in effects {
            match e {
                Effect::Spawn { pane, cwd_from } => {
                    let cwd = cwd_from
                        .and_then(|p| self.panes.get(&p)?.cwd())
                        .unwrap_or_else(|| self.config.home.clone());
                    let (cols, rows) = rects
                        .get(&pane)
                        .map(|r| (r.cols, r.rows))
                        .unwrap_or((80, 24));
                    let start = Start::Now(self.config.shell(cwd.clone()));
                    match self.open_pane(pane, cols, rows, false, start, cwd) {
                        Ok(()) => {
                            self.meta.insert(pane, PaneMeta::default());
                        }
                        Err(e) => {
                            warn!(pane, error = %e, "could not start pane");
                            let _ = self.mux.apply(Intent::ClosePane { pane });
                        }
                    }
                }
                Effect::Kill { pane } => {
                    if let Some(p) = self.panes.remove(&pane) {
                        p.close();
                    }
                    self.sizes.remove(&pane);
                    self.meta.remove(&pane);
                }
            }
        }
        self.changed();
        Ok(())
    }

    /// Resize panes whose cells changed, tell every client, and save soon.
    fn changed(&mut self) {
        for (pane, r) in self.mux.pane_rects() {
            let size = (r.cols, r.rows);
            if self.sizes.get(&pane) != Some(&size)
                && let Some(p) = self.panes.get(&pane)
            {
                p.resize(r.cols, r.rows);
                self.sizes.insert(pane, size);
            }
        }
        self.broadcast();
        self.save_due
            .get_or_insert_with(|| Instant::now() + SAVE_DEBOUNCE);
    }

    fn broadcast(&self) {
        let state = self.state();
        for sub in self.clients.values() {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::State {
                state: state.clone(),
            }));
        }
    }

    /// Note each running pane's directory and foreground command. True if
    /// any changed.
    fn refresh_meta(&mut self) -> bool {
        let mut changed = false;
        for (id, h) in &self.panes {
            if !h.running() {
                continue;
            }
            let m = self.meta.entry(*id).or_default();
            let cwd = h.cwd().map(|c| c.display().to_string()).or(m.cwd.clone());
            let command = h.command();
            changed |= m.cwd != cwd || m.command != command;
            (m.cwd, m.command) = (cwd, command);
        }
        changed
    }

    /// Write the layout and pane details if anything changed since the last
    /// write.
    fn save(&mut self) {
        self.refresh_meta();
        let panes: BTreeMap<PaneId, PaneMeta> =
            self.meta.iter().map(|(k, v)| (*k, v.clone())).collect();
        if self
            .last_saved
            .as_ref()
            .is_some_and(|(m, p)| *m == self.mux && *p == panes)
        {
            return;
        }
        let saved = Saved {
            version: LAYOUT_VERSION,
            saved_at_ms: now_ms(),
            mux: self.mux.clone(),
            panes,
        };
        match self.store.save_layout(&saved) {
            Ok(()) => self.last_saved = Some((saved.mux, saved.panes)),
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
        let mut panes: Vec<PaneInfo> = self
            .panes
            .values()
            .map(|p| {
                let meta = self.meta.get(&p.id).cloned().unwrap_or_default();
                let running = p.running();
                PaneInfo {
                    id: p.id,
                    epoch: p.epoch,
                    cwd: p.cwd().map(|c| c.display().to_string()).or(meta.cwd),
                    command: if running { p.command() } else { meta.command },
                    running,
                    policy: meta.policy,
                }
            })
            .collect();
        panes.sort_by_key(|p| p.id);
        State {
            rev: self.mux.rev,
            sessions: self.mux.sessions.clone(),
            tabs,
            panes,
        }
    }
}
