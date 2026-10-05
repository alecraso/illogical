//! A pane for a guest who has only OpenSSH (M65): `illogical share --guest
//! %N` makes an invite, and the guest pastes the `ssh` command it prints.
//!
//! The daemon runs its own ssh server (russh) on `--guest-ssh` (default
//! `0.0.0.0:7684`), only while at least one invite exists. It isn't the
//! box's `sshd`: there are no accounts and no shell. The username is the
//! invite's token, accepted with ssh's `none` method, and the session's
//! shell request attaches to the invite's pane, nothing else. `exec`,
//! subsystems, forwarding of any kind and X11 are refused.
//!
//! The command pins the daemon's host key (`KnownHostsCommand`, with
//! `StrictHostKeyChecking=yes` and no known-hosts file), so the token is
//! only ever sent to this daemon. The key is ed25519, made once and kept in
//! the state directory.
//!
//! Read-only is the default. A read-write guest types under one-driver
//! rules, labeled with the invite's name, and sizes the pane while they
//! drive it. Revoking an invite drops its sessions at once; expiry ends
//! them at the deadline; closing the pane ends them and the invite. The
//! daemon keeps only each token's hash, in `guests.json`.
//!
//! A daemon behind NAT is reached through control (the relay step): when
//! it's joined to a control that runs a jump host and no address for
//! guests is set, the command hops through control first (`ssh -W` to the
//! daemon's id, as `-J` does, with control's key pinned too). The hop's
//! username is a route, a second token whose hash the daemon registers
//! with control over its relay socket while the invite lives. Control
//! splices the hop onto a raw `ssh-guest` stream over that socket, and the
//! stream comes here, to the same ssh server: the session is end to end,
//! and control sees ciphertext.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use illogical_proto::{
    BlockType, ClientId, Driver, EventKind, Frame, FrameKind, PaneId,
    api::{GuestInvite, GuestInviteRequest},
};
use russh::{
    Channel, ChannelId, MethodKind, MethodSet,
    keys::{HashAlg, PrivateKey, ssh_key},
    server::{Auth, ChannelOpenHandle, Handler, Msg, Session},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info, warn};

use crate::{
    hosts::digest,
    mux::Api,
    pane::{Subscriber, ToClient, Want, client_queue},
    server::App,
    store::{now_ms, write_atomic},
};

/// Where it listens unless `--guest-ssh` says otherwise.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:7684";
/// The raw stream kind control opens for a guest through its jump host.
pub const STREAM_KIND: &[u8] = b"ssh-guest";
/// How long making a relayed invite waits for control to take its route.
const ROUTE_WITHIN: Duration = Duration::from_secs(10);
const DEFAULT_TTL_SECS: u64 = 3600;
/// A read-only invite lasts at most a day...
const MAX_TTL_SECS: u64 = 24 * 3600;
/// ...and a read-write one at most two hours, as an M14 trust grant does.
const MAX_RW_TTL_SECS: u64 = 2 * 3600;
/// Connections open at once, authenticated or not.
const MAX_CONNECTIONS: usize = 32;
/// Rows of scrollback a guest gets with the screen.
const HISTORY: u32 = 1000;
/// Ctrl-]: leave, as `illogical attach` does.
const DETACH: u8 = 0x1d;
/// A connection that hasn't logged in by now is dropped, so slow ones can't
/// hold every place.
const LOGIN_WITHIN: Duration = Duration::from_secs(20);

#[derive(Clone, Serialize, Deserialize)]
struct Saved {
    id: u32,
    digest: String,
    pane: PaneId,
    rw: bool,
    reusable: bool,
    label: String,
    created_ms: u64,
    expires_ms: u64,
    #[serde(default)]
    used: bool,
    /// Through control: the hash of the route registered for it there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct SavedInvites {
    next: u32,
    invites: Vec<Saved>,
}

/// What a login got: the invite it used.
#[derive(Clone, Debug)]
struct Granted {
    id: u32,
    pane: PaneId,
    rw: bool,
    label: String,
    expires_ms: u64,
}

struct Listening {
    addr: SocketAddr,
    stop: oneshot::Sender<()>,
}

pub struct Guests {
    path: PathBuf,
    key_path: PathBuf,
    /// `None`: `--guest-ssh off`.
    listen: Option<SocketAddr>,
    /// The address to put in commands, when the request doesn't name one.
    host: Option<String>,
    inner: Mutex<SavedInvites>,
    /// Bumped whenever an invite ends early (revoked, its pane closed).
    ended: watch::Sender<u64>,
    sessions: Mutex<HashMap<u32, u32>>,
    listening: tokio::sync::Mutex<Option<Listening>>,
    key: Mutex<Option<PrivateKey>>,
    connections: Arc<AtomicUsize>,
    /// The latest `guest.routes` message for control's relay socket.
    routes_out: watch::Sender<Option<String>>,
    route_seq: AtomicU64,
    /// The highest `seq` control has taken, and what it said if it has no
    /// jump host.
    routes_taken: watch::Sender<(u64, Option<String>)>,
}

fn random_bytes<const N: usize>() -> [u8; N] {
    crate::push::random()
}

/// A token is a valid ssh username: `g` and 32 hex digits (128 bits).
fn random_token() -> String {
    format!("g{}", random_bytes::<16>().iter().map(|x| format!("{x:02x}")).collect::<String>())
}

/// A host name or address that's safe to paste into a shell command.
fn plain_host(h: &str) -> bool {
    !h.is_empty() && h.len() <= 253 && h.chars().all(|c| c.is_ascii_alphanumeric() || ".-:".contains(c))
}

/// How known-hosts files name `host` on `port`.
fn known_name(host: &str, port: u16) -> String {
    if port == 22 { host.to_owned() } else { format!("[{host}]:{port}") }
}

/// A route: the jump host's username for one invite (`r` and 32 hex
/// digits).
fn random_route() -> String {
    format!("r{}", random_bytes::<16>().iter().map(|x| format!("{x:02x}")).collect::<String>())
}

/// The command for a guest of a daemon behind NAT: through control's jump
/// host to the daemon, by its id. It's what `ssh -J route@jump token@id`
/// does (OpenSSH turns `-J` into this `ProxyCommand`), spelled out because
/// ssh doesn't pass `-o` options to a `-J` hop: written this way the hop
/// pins control's key too, rather than asking the guest about it and
/// saving it in their known-hosts file. The hop never prompts
/// (`BatchMode`): with a route that has ended, OpenSSH would otherwise ask
/// for a password the jump host doesn't take.
pub fn relay_command(token: &str, id: &str, known_hosts: &str, route: &str, jump: &Jump) -> String {
    let p = if jump.port == 22 { String::new() } else { format!("-p {} ", jump.port) };
    format!(
        "ssh -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes \
         -o 'KnownHostsCommand=/bin/echo {known_hosts}' \
         -o 'ProxyCommand=ssh {p}-o BatchMode=yes -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes \
         -o \"KnownHostsCommand=/bin/echo {jk}\" -W %h:%p {route}@{jh}' {token}@{id}",
        jk = jump.known_hosts,
        jh = jump.host,
    )
}

/// Control's jump host, as its `/control.json` describes it.
#[derive(Clone, Debug, Deserialize)]
pub struct Jump {
    pub host: String,
    pub port: u16,
    pub known_hosts: String,
}

impl Jump {
    /// Nothing in it a shell would read as more than words.
    fn plain(&self) -> bool {
        plain_host(&self.host)
            && self.known_hosts.split(' ').count() == 3
            && self.known_hosts.chars().all(|c| c.is_ascii_alphanumeric() || " []:.-+/=@".contains(c))
    }
}

/// The command a guest pastes: the host key pinned, nothing written to
/// their known-hosts file.
pub fn command(token: &str, host: &str, port: u16, known_hosts: &str) -> String {
    let p = if port == 22 { String::new() } else { format!("-p {port} ") };
    format!(
        "ssh {p}-o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes \
         -o 'KnownHostsCommand=/bin/echo {known_hosts}' {token}@{host}"
    )
}

impl Guests {
    pub fn open(state_dir: &FsPath, listen: Option<SocketAddr>, host: Option<String>) -> Arc<Self> {
        let path = state_dir.join("guests.json");
        let saved = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Arc::new(Self {
            path,
            key_path: state_dir.join("guest_ssh_host_key"),
            listen,
            host,
            inner: Mutex::new(saved),
            ended: watch::channel(0).0,
            sessions: Mutex::default(),
            listening: tokio::sync::Mutex::new(None),
            key: Mutex::new(None),
            connections: Arc::default(),
            routes_out: watch::channel(None).0,
            route_seq: AtomicU64::new(0),
            routes_taken: watch::channel((0, None)).0,
        })
        .with_routes()
    }

    fn with_routes(self: Arc<Self>) -> Arc<Self> {
        self.publish_routes();
        self
    }

    /// Tell control (over the relay socket, now or when it's next up) the
    /// routes of the invites that live; the `seq` it will answer with.
    fn publish_routes(&self) -> u64 {
        let routes: Vec<String> = {
            let now = now_ms();
            let s = self.inner.lock().unwrap();
            s.invites.iter().filter(|x| x.expires_ms > now).filter_map(|x| x.route.clone()).collect()
        };
        let seq = self.route_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let msg = serde_json::json!({ "t": "guest.routes", "seq": seq, "routes": routes }).to_string();
        self.routes_out.send_replace(Some(msg));
        seq
    }

    /// The messages for control's relay socket about routes.
    pub fn routes_messages(&self) -> watch::Receiver<Option<String>> {
        self.routes_out.subscribe()
    }

    /// A text message from control's relay socket: whether it was ours.
    pub fn heard_from_control(&self, text: &str) -> bool {
        #[derive(Deserialize)]
        struct Msg {
            t: String,
            #[serde(default)]
            seq: u64,
            #[serde(default)]
            off: Option<String>,
        }
        let Ok(m) = serde_json::from_str::<Msg>(text) else { return false };
        if m.t != "guest.routes.ok" {
            return false;
        }
        self.routes_taken.send_modify(|(seq, off)| {
            *seq = (*seq).max(m.seq);
            *off = m.off;
        });
        true
    }

    /// Wait until control has taken the routes as of `seq`.
    async fn route_taken(&self, seq: u64) -> Result<(), String> {
        let mut rx = self.routes_taken.subscribe();
        let got = tokio::time::timeout(ROUTE_WITHIN, rx.wait_for(|(s, _)| *s >= seq)).await;
        match got {
            Ok(Ok(r)) => match &r.1 {
                Some(off) => Err(format!("control says: {off}")),
                None => Ok(()),
            },
            _ => Err("control's relay didn't take the invite's route; is this machine connected to control?".into()),
        }
    }

    fn save(&self, s: &SavedInvites) {
        let bytes = serde_json::to_vec_pretty(s).expect("serialize invites");
        if let Err(e) = write_atomic(&self.path, &bytes) {
            warn!(error = %e, "can't save ssh invites");
        }
    }

    /// Drop expired invites; whether any went.
    fn prune(&self) -> bool {
        let now = now_ms();
        let mut s = self.inner.lock().unwrap();
        let before = s.invites.len();
        s.invites.retain(|x| x.expires_ms > now);
        let gone = s.invites.len() != before;
        if gone {
            self.save(&s);
            drop(s);
            self.publish_routes();
        }
        gone
    }

    fn any(&self) -> bool {
        !self.inner.lock().unwrap().invites.is_empty()
    }

    /// The host key, made the first time it's needed.
    fn host_key(&self) -> anyhow::Result<PrivateKey> {
        let mut k = self.key.lock().unwrap();
        if let Some(k) = &*k {
            return Ok(k.clone());
        }
        let key = match std::fs::read_to_string(&self.key_path) {
            Ok(pem) => PrivateKey::from_openssh(pem)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let pair = ssh_key::private::Ed25519Keypair::from_seed(&random_bytes::<32>());
                let key = PrivateKey::from(pair);
                let pem = key.to_openssh(ssh_key::LineEnding::LF)?;
                write_atomic(&self.key_path, pem.as_bytes())?;
                crate::perm::set(&self.key_path, 0o600)?;
                info!("made the guest ssh host key");
                key
            }
            Err(e) => return Err(e.into()),
        };
        *k = Some(key.clone());
        Ok(key)
    }

    /// A new invite, and its token. `route`: through control, with this
    /// route's hash.
    fn mint(&self, req: &GuestInviteRequest, route: Option<String>) -> (GuestInvite, String) {
        let token = random_token();
        let created_ms = now_ms();
        let max = if req.rw { MAX_RW_TTL_SECS } else { MAX_TTL_SECS };
        let ttl = req.ttl_secs.unwrap_or(DEFAULT_TTL_SECS).clamp(1, max);
        let label = req.label.clone().filter(|l| !l.trim().is_empty()).unwrap_or_else(|| "guest".into());
        let mut s = self.inner.lock().unwrap();
        s.next += 1;
        let saved = Saved {
            id: s.next,
            digest: digest(&token),
            pane: req.pane,
            rw: req.rw,
            reusable: req.reusable,
            label: label.chars().take(64).collect(),
            created_ms,
            expires_ms: created_ms + ttl * 1000,
            used: false,
            route,
        };
        s.invites.push(saved.clone());
        self.save(&s);
        info!(id = saved.id, pane = saved.pane, rw = saved.rw, "ssh invite made");
        (self.public(&saved), token)
    }

    fn public(&self, x: &Saved) -> GuestInvite {
        GuestInvite {
            id: x.id,
            pane: x.pane,
            rw: x.rw,
            reusable: x.reusable,
            label: x.label.clone(),
            created_ms: x.created_ms,
            expires_ms: x.expires_ms,
            used: x.used,
            sessions: self.sessions.lock().unwrap().get(&x.id).copied().unwrap_or(0),
            token: None,
            command: None,
            known_hosts: None,
            fingerprint: None,
            host: None,
            port: None,
            relay: x.route.is_some(),
            jump: None,
        }
    }

    pub fn list(&self) -> Vec<GuestInvite> {
        self.prune();
        let s = self.inner.lock().unwrap();
        s.invites.iter().map(|x| self.public(x)).collect()
    }

    /// End invite `id` now; its guests are cut off.
    pub fn revoke(&self, id: u32) -> bool {
        let gone = {
            let mut s = self.inner.lock().unwrap();
            let before = s.invites.len();
            s.invites.retain(|x| x.id != id);
            let gone = s.invites.len() != before;
            if gone {
                self.save(&s);
            }
            gone
        };
        if gone {
            info!(id, "ssh invite revoked");
            self.publish_routes();
            self.ended.send_modify(|n| *n += 1);
        }
        gone
    }

    /// A pane closed: its invites end with it.
    fn pane_closed(&self, pane: PaneId) {
        let gone = {
            let mut s = self.inner.lock().unwrap();
            let before = s.invites.len();
            s.invites.retain(|x| x.pane != pane);
            let gone = s.invites.len() != before;
            if gone {
                self.save(&s);
            }
            gone
        };
        if gone {
            self.publish_routes();
            self.ended.send_modify(|n| *n += 1);
        }
    }

    /// A login with `token`: the invite, if it's current and not spent. A
    /// single-use invite is spent by it.
    fn claim(&self, token: &str) -> Option<Granted> {
        let want = digest(token);
        let now = now_ms();
        let mut s = self.inner.lock().unwrap();
        let x = s.invites.iter_mut().find(|x| x.digest == want && x.expires_ms > now)?;
        if x.used && !x.reusable {
            return None;
        }
        x.used = true;
        let g = Granted { id: x.id, pane: x.pane, rw: x.rw, label: x.label.clone(), expires_ms: x.expires_ms };
        self.save(&s);
        Some(g)
    }

    fn live(&self, id: u32) -> bool {
        let now = now_ms();
        self.inner.lock().unwrap().invites.iter().any(|x| x.id == id && x.expires_ms > now)
    }

    fn count(&self, id: u32, delta: i32) {
        let mut s = self.sessions.lock().unwrap();
        let n = s.entry(id).or_insert(0);
        *n = n.saturating_add_signed(delta);
        if *n == 0 {
            s.remove(&id);
        }
    }

    /// Listen, if we aren't: the address we're on.
    async fn ensure_listening(self: &Arc<Self>, app: &Arc<App>) -> anyhow::Result<SocketAddr> {
        let Some(want) = self.listen else { anyhow::bail!("guest ssh is off on this machine (--guest-ssh off)") };
        let mut l = self.listening.lock().await;
        if let Some(l) = &*l {
            return Ok(l.addr);
        }
        let key = self.host_key()?;
        let listener =
            tokio::net::TcpListener::bind(want).await.map_err(|e| anyhow::anyhow!("can't listen on {want}: {e}"))?;
        let addr = listener.local_addr()?;
        let (stop, stopped) = oneshot::channel();
        tokio::spawn(serve(self.clone(), app.clone(), listener, stopped, key));
        info!(%addr, "guest ssh listening");
        *l = Some(Listening { addr, stop });
        Ok(addr)
    }

    /// Stop listening once no invite is left.
    async fn stop_if_idle(&self) {
        if self.any() {
            return;
        }
        if let Some(l) = self.listening.lock().await.take() {
            let _ = l.stop.send(());
            info!("guest ssh stopped listening: no invites left");
        }
    }

    /// Keep invites and the listener in step: expiry, closed panes, and
    /// listening again after a restart while invites are still good.
    pub fn run(self: &Arc<Self>, app: &Arc<App>) {
        let (me, app) = (self.clone(), app.clone());
        tokio::spawn(async move {
            me.prune();
            if me.any()
                && let Err(e) = me.ensure_listening(&app).await
            {
                warn!(error = %e, "guest ssh can't listen");
            }
            let mut events = app.mux.events();
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    e = events.recv() => match e {
                        Ok(e) if matches!(e.kind, EventKind::Closed) => {
                            if let Some(p) = e.pane { me.pane_closed(p) }
                        }
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => return,
                    },
                    _ = tick.tick() => { me.prune(); }
                }
                me.stop_if_idle().await;
            }
        });
    }
}

// ---------------------------------------------------------------- server

fn config(key: PrivateKey) -> russh::server::Config {
    russh::server::Config {
        methods: MethodSet::from(&[MethodKind::None][..]),
        auth_rejection_time: Duration::from_millis(300),
        auth_rejection_time_initial: Some(Duration::from_millis(300)),
        max_auth_attempts: 3,
        keys: vec![key],
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    }
}

async fn serve(
    guests: Arc<Guests>,
    app: Arc<App>,
    listener: tokio::net::TcpListener,
    mut stop: oneshot::Receiver<()>,
    key: PrivateKey,
) {
    let config = Arc::new(config(key));
    loop {
        let (tcp, peer) = tokio::select! {
            _ = &mut stop => return,
            r = listener.accept() => match r {
                Ok(x) => x,
                Err(e) => {
                    warn!(error = %e, "guest ssh accept");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        let _ = tcp.set_nodelay(true);
        connection(&guests, &app, config.clone(), tcp, peer.to_string());
    }
}

/// One guest's connection, on whatever carries it: a TCP connection, or a
/// raw stream from control's jump host.
fn connection<S>(guests: &Arc<Guests>, app: &Arc<App>, config: Arc<russh::server::Config>, io: S, peer: String)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    if guests.connections.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
        guests.connections.fetch_sub(1, Ordering::Relaxed);
        debug!(%peer, "guest ssh: too many connections");
        return;
    }
    let authed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let conn = Conn {
        guests: guests.clone(),
        app: app.clone(),
        peer: peer.clone(),
        granted: None,
        channel: None,
        size: (80, 24),
        term: None,
        tx: None,
        authed: authed.clone(),
        _slot: Slot(guests.connections.clone()),
    };
    tokio::spawn(async move {
        let s = match tokio::time::timeout(LOGIN_WITHIN, russh::server::run_stream(config, io, conn)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return debug!(%peer, error = %e, "guest ssh handshake"),
            Err(_) => return debug!(%peer, "guest ssh: no handshake in time"),
        };
        let handle = s.handle();
        let p = peer.clone();
        tokio::spawn(async move {
            tokio::time::sleep(LOGIN_WITHIN).await;
            if !authed.load(Ordering::Relaxed) {
                debug!(peer = %p, "guest ssh: no login in time");
                let _ =
                    handle.disconnect(russh::Disconnect::ByApplication, "no login in time".into(), String::new()).await;
            }
        });
        if let Err(e) = s.await {
            debug!(%peer, error = %e, "guest ssh session ended");
        }
    });
}

impl Guests {
    /// A guest's connection through control's jump host: the same server
    /// as on the port, so the session ends here.
    pub fn serve_relayed(self: &Arc<Self>, app: &Arc<App>, stream: tokio::io::DuplexStream) {
        if self.listen.is_none() || !self.any() {
            return debug!("guest ssh: a relayed connection with guest ssh off or no invites");
        }
        let key = match self.host_key() {
            Ok(k) => k,
            Err(e) => return warn!(error = %e, "guest ssh: no host key"),
        };
        connection(self, app, Arc::new(config(key)), stream, "control's relay".into());
    }
}

/// One connection's place under [`MAX_CONNECTIONS`].
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

enum FromGuest {
    Input(Vec<u8>),
    Size(u16, u16),
}

/// One guest's connection.
struct Conn {
    guests: Arc<Guests>,
    app: Arc<App>,
    peer: String,
    granted: Option<Granted>,
    /// The one session channel it may open.
    channel: Option<ChannelId>,
    size: (u16, u16),
    term: Option<String>,
    tx: Option<mpsc::UnboundedSender<FromGuest>>,
    authed: Arc<std::sync::atomic::AtomicBool>,
    _slot: Slot,
}

fn clamp_size(cols: u32, rows: u32) -> (u16, u16) {
    (cols.clamp(2, 1000) as u16, rows.clamp(1, 1000) as u16)
}

impl Handler for Conn {
    type Error = russh::Error;

    async fn auth_none(&mut self, user: &str) -> Result<Auth, Self::Error> {
        let Some(g) = self.guests.claim(user) else {
            info!(peer = %self.peer, "guest ssh: refused a login");
            return Ok(Auth::reject());
        };
        // The pane must still be there.
        if self.app.mux.api(|r| Api::Pane(g.pane, r)).await.flatten().is_none() {
            return Ok(Auth::reject());
        }
        info!(peer = %self.peer, invite = g.id, pane = g.pane, "guest ssh: logged in");
        self.granted = Some(g);
        self.authed.store(true, Ordering::Relaxed);
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let ok = self.channel.is_none() && self.granted.as_ref().is_some_and(|g| self.guests.live(g.id));
        if ok {
            self.channel = Some(channel.id());
            reply.accept().await;
        } else {
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
        }
        // Everything arrives through `data`; nobody reads the channel.
        drop(channel);
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.term = Some(term.chars().take(64).collect());
        if cols > 0 && rows > 0 {
            self.size = clamp_size(cols, rows);
        }
        session.channel_success(channel)
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        let (Some(g), Some(ch)) = (self.granted.clone(), self.channel) else {
            return session.channel_failure(channel);
        };
        if ch != channel || self.tx.is_some() {
            return session.channel_failure(channel);
        }
        let (tx, rx) = mpsc::unbounded_channel();
        self.tx = Some(tx);
        session.channel_success(channel)?;
        debug!(invite = g.id, term = ?self.term, size = ?self.size, "guest ssh: shell");
        let (app, guests, handle, size) = (self.app.clone(), self.guests.clone(), session.handle(), self.size);
        tokio::spawn(attach(app, guests, g, channel, handle, size, rx));
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, _: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        session.channel_failure(channel)
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)
    }

    async fn data(&mut self, _: ChannelId, data: &[u8], _: &mut Session) -> Result<(), Self::Error> {
        if let Some(tx) = &self.tx {
            let _ = tx.send(FromGuest::Input(data.to_vec()));
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _: ChannelId,
        cols: u32,
        rows: u32,
        _pw: u32,
        _ph: u32,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        if cols > 0 && rows > 0 {
            self.size = clamp_size(cols, rows);
            if let Some(tx) = &self.tx {
                let _ = tx.send(FromGuest::Size(self.size.0, self.size.1));
            }
        }
        Ok(())
    }

    async fn channel_eof(&mut self, _: ChannelId, _: &mut Session) -> Result<(), Self::Error> {
        self.tx = None;
        Ok(())
    }

    async fn channel_close(&mut self, _: ChannelId, _: &mut Session) -> Result<(), Self::Error> {
        self.tx = None;
        Ok(())
    }
}

static NEXT_GUEST: AtomicU64 = AtomicU64::new(1 << 60);

fn title(text: &str) -> Vec<u8> {
    format!("\x1b]2;{text}\x07").into_bytes()
}

/// One guest on their pane, until they leave, the pane closes or the
/// invite ends.
async fn attach(
    app: Arc<App>,
    guests: Arc<Guests>,
    g: Granted,
    chan: ChannelId,
    out: russh::server::Handle,
    mut size: (u16, u16),
    mut rx: mpsc::UnboundedReceiver<FromGuest>,
) {
    let send = |b: Vec<u8>| {
        let out = out.clone();
        async move { out.data(chan, b).await.is_ok() }
    };
    let Some(handle) = app.mux.api(|r| Api::Pane(g.pane, r)).await.flatten() else {
        let _ = out.extended_data(chan, 1, b"[illogical: the pane has closed]\r\n".to_vec()).await;
        let _ = out.exit_status_request(chan, 1).await;
        let _ = out.close(chan).await;
        return;
    };
    let client: ClientId = NEXT_GUEST.fetch_add(1, Ordering::Relaxed);
    // Each connection drives on its own: two guests on one reusable invite
    // are still two people.
    let who = format!("guest-ssh:{}:{client}", g.id);
    let by = Driver { who: who.clone(), name: g.label.clone() };
    let (data, mut data_rx) = client_queue();
    let (ctrl, mut ctrl_rx) = mpsc::unbounded_channel();
    let sub = Subscriber {
        client,
        data,
        ctrl,
        principal: crate::acl::Principal::Owner,
        name: Some(g.label.clone()),
        device: None,
    };
    let want = Want { history: Some(HISTORY), ..Default::default() };
    handle.attach_with(sub.clone(), want);
    guests.count(g.id, 1);
    let mode = if g.rw { "read-write" } else { "read-only" };
    let heading = format!("illogical %{} ({mode}) · Ctrl-] leaves", g.pane);
    let mut ended = guests.ended.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(g.expires_ms.saturating_sub(now_ms()));
    let mut events = app.mux.events();
    let mut told: Option<String> = None;
    let (why, status) = loop {
        tokio::select! {
            item = data_rx.recv() => match item {
                Some(ToClient::Frame(bytes)) => {
                    let Ok(f) = Frame::decode(&bytes) else { continue };
                    if f.pane != g.pane { continue }
                    let ok = match f.kind {
                        FrameKind::Snapshot => {
                            let mut b = b"\x1bc".to_vec();
                            b.extend_from_slice(&f.data);
                            b.extend(title(&heading));
                            send(b).await
                        }
                        FrameKind::Output => send(f.data).await,
                        _ => true,
                    };
                    if !ok { break (None, 0) }
                }
                Some(ToClient::Close) | None => break (Some("the pane closed"), 0),
                Some(_) => {}
            },
            Some(item) = ctrl_rx.recv() => {
                // Fell behind and was dropped: start over from a snapshot.
                if let ToClient::Msg(illogical_proto::ServerMsg::Resync { .. }) = item {
                    handle.attach_with(sub.clone(), want);
                }
            }
            e = events.recv() => {
                if let Ok(e) = e && e.pane == Some(g.pane) && matches!(e.kind, EventKind::Closed) {
                    break (Some("the pane closed"), 0);
                }
            }
            _ = ended.changed() => {
                if !guests.live(g.id) { break (Some("the invite was revoked"), 1) }
            }
            _ = tokio::time::sleep_until(deadline) => break (Some("the invite expired"), 1),
            m = rx.recv() => match m {
                None => break (None, 0),
                Some(FromGuest::Size(c, r)) => {
                    size = (c, r);
                    if g.rw {
                        app.mux.send(crate::mux::Cmd::Api(Api::GuestSize { pane: g.pane, client, who: who.clone(), size }));
                    }
                }
                Some(FromGuest::Input(bytes)) => {
                    let (bytes, leave) = match bytes.iter().position(|b| *b == DETACH) {
                        Some(i) => (bytes[..i].to_vec(), true),
                        None => (bytes, false),
                    };
                    if !bytes.is_empty() {
                        let refused = if g.rw {
                            let r = app.mux.api(|reply| Api::GuestInput {
                                pane: g.pane, client, by: by.clone(), data: bytes, size, reply,
                            }).await;
                            match r {
                                Some(Ok(())) => None,
                                Some(Err(why)) => Some(why),
                                None => Some("the daemon is stopping".into()),
                            }
                        } else {
                            Some("read-only: your keys aren't sent".into())
                        };
                        match refused {
                            Some(why) if told.as_deref() != Some(why.as_str()) => {
                                let _ = send(title(&format!("{heading} · {why}"))).await;
                                told = Some(why);
                            }
                            Some(_) => {}
                            None if told.is_some() => {
                                told = None;
                                let _ = send(title(&heading)).await;
                            }
                            None => {}
                        }
                    }
                    if leave { break (Some("you left"), 0) }
                }
            },
        }
    };
    handle.detach(client);
    app.mux.send(crate::mux::Cmd::Api(Api::GuestLeft { client, who }));
    guests.count(g.id, -1);
    info!(invite = g.id, pane = g.pane, why = why.unwrap_or("hung up"), "guest ssh: left");
    if let Some(why) = why {
        // Leave the screen as a shell would find it, then say why.
        let reset = b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[0m\x1b[?25h\r\n";
        let _ = out.data(chan, reset.to_vec()).await;
        let _ = out.extended_data(chan, 1, format!("[illogical: {why}]\r\n").into_bytes()).await;
        let _ = out.exit_status_request(chan, status).await;
        let _ = out.eof(chan).await;
        let _ = out.close(chan).await;
        // A client that doesn't hang up when its only channel closes.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let _ = out.disconnect(russh::Disconnect::ByApplication, why.into(), String::new()).await;
        });
    }
}

// ---------------------------------------------------------------- routes

type AppState = State<Arc<App>>;

/// The owner's: making, listing and revoking invites.
pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/guests", get(list).post(mint)).route("/api/guests/{id}", delete(revoke))
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

fn hostname() -> String {
    crate::hostname().unwrap_or_else(|| "localhost".into())
}

async fn mint(State(app): AppState, Json(req): Json<GuestInviteRequest>) -> Response {
    let summaries = app.mux.api(Api::Panes).await.unwrap_or_default();
    match summaries.iter().find(|p| p.info.id == req.pane) {
        None => return error(StatusCode::NOT_FOUND, format!("no pane %{}", req.pane)),
        Some(p) if p.info.kind != BlockType::Terminal => {
            return error(
                StatusCode::BAD_REQUEST,
                format!("%{} isn't a terminal; only terminals can be shared", req.pane),
            );
        }
        Some(_) => {}
    }
    let guests = &app.guests;
    let named = req.host.clone().or_else(|| guests.host.clone());
    // Through control when asked, or when this machine is joined to one
    // and no address for guests is set: it may well be behind NAT.
    let relay = req.relay.unwrap_or(named.is_none() && app.control.enrolled().is_some() && !app.control.no_relay);
    if relay {
        match relayed(&app, &req).await {
            Ok(invite) => return Json(invite).into_response(),
            // Asked for nothing in particular: the direct path, then.
            Err((false, why)) if req.relay.is_none() => info!(%why, "guest ssh: not through control"),
            Err((_, why)) => return error(StatusCode::SERVICE_UNAVAILABLE, why),
        }
    }
    let host = named.unwrap_or_else(hostname);
    if !plain_host(&host) {
        return error(StatusCode::BAD_REQUEST, format!("{host:?} isn't a host name or address"));
    }
    let addr = match guests.ensure_listening(&app).await {
        Ok(a) => a,
        Err(e) => return error(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
    };
    let key = match guests.host_key() {
        Ok(k) => k,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, format!("no host key: {e}")),
    };
    let public = key.public_key().to_openssh().unwrap_or_default();
    let (mut invite, token) = guests.mint(&req, None);
    let known = format!("{} {public}", known_name(&host, addr.port()));
    invite.command = Some(command(&token, &host, addr.port(), &known));
    invite.known_hosts = Some(known);
    invite.fingerprint = Some(key.public_key().fingerprint(HashAlg::Sha256).to_string());
    invite.host = Some(host);
    invite.port = Some(addr.port());
    invite.token = Some(token);
    Json(invite).into_response()
}

/// An invite through control's jump host, or why not (`true`: control
/// has a jump host, and something else went wrong).
async fn relayed(app: &Arc<App>, req: &GuestInviteRequest) -> Result<GuestInvite, (bool, String)> {
    let guests = &app.guests;
    if guests.listen.is_none() {
        return Err((true, "guest ssh is off on this machine (--guest-ssh off)".into()));
    }
    let Some(e) = app.control.enrolled() else {
        return Err((false, "this machine isn't joined to control".into()));
    };
    let jump = match app.control.guest_jump().await {
        Ok(Some(j)) if j.plain() => j,
        Ok(Some(j)) => return Err((true, format!("control's jump host {:?} isn't a plain host and key", j.host))),
        Ok(None) => return Err((false, format!("control at {} has no ssh jump host for guests", e.saved.url))),
        Err(err) => return Err((false, format!("can't ask control about its jump host: {err}"))),
    };
    let key = guests.host_key().map_err(|err| (true, format!("no host key: {err}")))?;
    let id = e.saved.cert.device.clone();
    let route = random_route();
    let (mut invite, token) = guests.mint(req, Some(digest(&route)));
    let seq = guests.publish_routes();
    if let Err(why) = guests.route_taken(seq).await {
        guests.revoke(invite.id);
        return Err((true, why));
    }
    let known = format!("{id} {}", key.public_key().to_openssh().unwrap_or_default());
    invite.command = Some(relay_command(&token, &id, &known, &route, &jump));
    invite.known_hosts = Some(format!("{known}\n{}", jump.known_hosts));
    invite.fingerprint = Some(key.public_key().fingerprint(HashAlg::Sha256).to_string());
    invite.host = Some(id);
    invite.jump = Some(if jump.port == 22 { jump.host } else { format!("{}:{}", jump.host, jump.port) });
    invite.token = Some(token);
    info!(id = invite.id, "ssh invite through control's jump host");
    Ok(invite)
}

async fn list(State(app): AppState) -> Json<Vec<GuestInvite>> {
    Json(app.guests.list())
}

async fn revoke(State(app): AppState, Path(id): Path<u32>) -> Response {
    if app.guests.revoke(id) {
        app.guests.stop_if_idle().await;
        Json(serde_json::json!({})).into_response()
    } else {
        error(StatusCode::NOT_FOUND, format!("no invite {id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ilg-guests-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn invites_are_spent_capped_revoked_and_kept_as_hashes() {
        let d = dir("store");
        let g = Guests::open(&d, None, None);
        let (once, token) = g.mint(&GuestInviteRequest { pane: 3, ..Default::default() }, None);
        assert!(token.starts_with('g') && token.len() == 33, "{token}");
        assert_eq!(once.label, "guest");
        assert!(g.claim("gwrong").is_none());
        let got = g.claim(&token).expect("first login");
        assert_eq!((got.pane, got.rw), (3, false));
        assert!(g.claim(&token).is_none(), "single use");
        assert!(g.list()[0].used);
        let (many, t2) = g.mint(&GuestInviteRequest { pane: 4, reusable: true, rw: true, ..Default::default() }, None);
        assert!(g.claim(&t2).is_some() && g.claim(&t2).is_some(), "reusable");
        let saved = std::fs::read_to_string(d.join("guests.json")).unwrap();
        assert!(!saved.contains(&token) && !saved.contains(&t2));
        // Survives a restart; revoking ends it.
        let g = Guests::open(&d, None, None);
        assert!(g.live(many.id));
        assert!(g.revoke(many.id) && !g.revoke(many.id));
        assert!(g.claim(&t2).is_none());
        // Closing the pane ends its invites.
        let (p, t3) = g.mint(&GuestInviteRequest { pane: 9, reusable: true, ..Default::default() }, None);
        g.pane_closed(9);
        assert!(!g.live(p.id) && g.claim(&t3).is_none());
        // Caps: a day read-only, two hours read-write.
        let (ro, _) =
            g.mint(&GuestInviteRequest { pane: 3, ttl_secs: Some(10 * MAX_TTL_SECS), ..Default::default() }, None);
        assert_eq!(ro.expires_ms - ro.created_ms, MAX_TTL_SECS * 1000);
        let (rw, _) =
            g.mint(&GuestInviteRequest { pane: 3, rw: true, ttl_secs: Some(MAX_TTL_SECS), ..Default::default() }, None);
        assert_eq!(rw.expires_ms - rw.created_ms, MAX_RW_TTL_SECS * 1000);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[tokio::test]
    async fn routes_are_told_to_control_while_their_invites_live() {
        let d = dir("routes");
        let g = Guests::open(&d, None, None);
        let routes = |g: &Guests| -> serde_json::Value {
            serde_json::from_str(g.routes_out.borrow().as_deref().unwrap()).unwrap()
        };
        assert_eq!(routes(&g)["routes"], serde_json::json!([]));
        let route = random_route();
        let (x, _) = g.mint(&GuestInviteRequest { pane: 3, ..Default::default() }, Some(digest(&route)));
        let seq = g.publish_routes();
        let m = routes(&g);
        assert_eq!((m["t"].as_str(), m["seq"].as_u64()), (Some("guest.routes"), Some(seq)));
        assert_eq!(m["routes"], serde_json::json!([digest(&route)]), "the hash only");
        assert!(g.list()[0].relay);
        // Control takes it, or says it has no jump host.
        assert!(!g.heard_from_control(r#"{"t":"forge.poke"}"#));
        assert!(g.heard_from_control(&format!(r#"{{"t":"guest.routes.ok","seq":{seq}}}"#)));
        g.route_taken(seq).await.unwrap();
        g.heard_from_control(&format!(r#"{{"t":"guest.routes.ok","seq":{seq},"off":"no jump host"}}"#));
        assert!(g.route_taken(seq).await.unwrap_err().contains("no jump host"));
        // A restart tells control again; revoking withdraws it.
        let g = Guests::open(&d, None, None);
        assert_eq!(routes(&g)["routes"], serde_json::json!([digest(&route)]));
        g.revoke(x.id);
        assert_eq!(routes(&g)["routes"], serde_json::json!([]));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn the_relayed_command_pins_both_hops() {
        let jump = Jump { host: "control".into(), port: 2222, known_hosts: "[control]:2222 ssh-ed25519 BBBB".into() };
        assert!(jump.plain());
        assert!(!Jump { known_hosts: "x' ; rm -rf ~".into(), ..jump.clone() }.plain());
        let c = relay_command("gabc", "0123abcd", "0123abcd ssh-ed25519 AAAA", "rdef", &jump);
        assert_eq!(
            c,
            "ssh -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes \
             -o 'KnownHostsCommand=/bin/echo 0123abcd ssh-ed25519 AAAA' \
             -o 'ProxyCommand=ssh -p 2222 -o BatchMode=yes -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes \
             -o \"KnownHostsCommand=/bin/echo [control]:2222 ssh-ed25519 BBBB\" -W %h:%p rdef@control' gabc@0123abcd"
        );
    }

    #[test]
    fn the_host_key_is_made_once_and_kept() {
        let d = dir("key");
        let a = Guests::open(&d, None, None).host_key().unwrap();
        let b = Guests::open(&d, None, None).host_key().unwrap();
        assert_eq!(a.public_key(), b.public_key());
        let mode = crate::perm::mode(&std::fs::metadata(d.join("guest_ssh_host_key")).unwrap());
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn commands_pin_the_key_and_quote_nothing_unsafe() {
        assert!(plain_host("box.example.ts.net") && plain_host("10.0.0.2") && plain_host("::1"));
        assert!(!plain_host("box;rm -rf ~") && !plain_host("a b") && !plain_host("$(x)") && !plain_host(""));
        assert_eq!(known_name("box", 22), "box");
        assert_eq!(known_name("box", 7684), "[box]:7684");
        let c = command("gabc", "box", 7684, "[box]:7684 ssh-ed25519 AAAA");
        assert_eq!(
            c,
            "ssh -p 7684 -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes \
             -o 'KnownHostsCommand=/bin/echo [box]:7684 ssh-ed25519 AAAA' gabc@box"
        );
    }
}
