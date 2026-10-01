//! The multiplexer task: owns the layout (`illogical_core::Mux`), the panes
//! and the connected clients. Every client message goes through here, so
//! layout changes, pane starts and stops, and resizes happen in one order.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use illogical_core::{Effect, Intent, Mux};
use illogical_proto::{ClientId, ClientMsg, PaneId, PaneInfo, ServerMsg, State, TabView};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::pane::{self, ExitSink, PaneHandle, Spawn, Subscriber, ToClient};

pub enum Cmd {
    Connect { sub: Subscriber },
    Disconnect { client: ClientId },
    Msg { client: ClientId, msg: ClientMsg },
    Input { pane: PaneId, data: Vec<u8> },
}

#[derive(Clone)]
pub struct MuxHandle {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl MuxHandle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

struct Daemon {
    mux: Mux,
    panes: HashMap<PaneId, PaneHandle>,
    clients: HashMap<ClientId, Subscriber>,
    /// Size each pane was last given.
    sizes: BTreeMap<PaneId, (u16, u16)>,
    spawn: Spawn,
    home: PathBuf,
    exits: ExitSink,
}

pub fn start(spawn: Spawn) -> MuxHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (exits, exits_rx) = mpsc::unbounded_channel();
    let home = spawn.cwd.clone();
    let mut d = Daemon {
        mux: Mux::new(),
        panes: HashMap::new(),
        clients: HashMap::new(),
        sizes: BTreeMap::new(),
        spawn,
        home,
        exits,
    };
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
    tokio::spawn(d.run(rx, exits_rx));
    MuxHandle { tx }
}

impl Daemon {
    async fn run(
        mut self,
        mut rx: mpsc::UnboundedReceiver<Cmd>,
        mut exits: mpsc::UnboundedReceiver<(PaneId, Option<i32>)>,
    ) {
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => self.handle(cmd),
                    None => return,
                },
                Some((pane, code)) = exits.recv() => {
                    info!(pane, ?code, "pane exited; closing it");
                    // Not in `panes` means the close came from a client.
                    if self.panes.remove(&pane).is_some() {
                        let _ = self.intent(None, Intent::ClosePane { pane });
                    }
                }
            }
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
                        .unwrap_or_else(|| self.home.clone());
                    let spawn = Spawn {
                        cwd,
                        ..self.spawn.clone()
                    };
                    let (cols, rows) = rects
                        .get(&pane)
                        .map(|r| (r.cols, r.rows))
                        .unwrap_or((80, 24));
                    match pane::spawn_pane(pane, &spawn, cols, rows, self.exits.clone()) {
                        Ok(h) => {
                            self.panes.insert(pane, h);
                            self.sizes.insert(pane, (cols, rows));
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
                }
            }
        }
        self.changed();
        Ok(())
    }

    /// Resize panes whose cells changed and tell every client.
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
        let state = self.state();
        for sub in self.clients.values() {
            let _ = sub.ctrl.send(ToClient::Msg(ServerMsg::State {
                state: state.clone(),
            }));
        }
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
            .map(|p| PaneInfo {
                id: p.id,
                epoch: p.epoch,
                cwd: p.cwd().map(|c| c.display().to_string()),
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
