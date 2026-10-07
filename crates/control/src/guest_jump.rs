//! The ssh jump host for guests of daemons behind NAT (M65's relay step).
//!
//! A guest with only OpenSSH reaches a daemon that can't be dialled through
//! here: the invite's command makes this the first hop (`ssh -W` to the
//! daemon's id, which is what `-J` turns into) and the daemon the second.
//! The hop's username is a route: a token the daemon registered over its
//! relay socket while the invite lives (`{"t":"guest.routes"}`), kept here
//! as hashes. It's accepted with ssh's `none` method and opens one thing,
//! a `direct-tcpip` channel to that route's daemon, which is spliced onto a
//! raw stream (`ssh-guest`) over the daemon's dial-out mux. Sessions,
//! commands and every other forward are refused.
//!
//! The guest's ssh session runs inside that channel and ends at the
//! daemon, with the daemon's host key pinned in the command, so what
//! crosses here is ssh ciphertext. The secret token is the inner session's
//! username: control never sees it. A route only opens the hop to one
//! daemon, which still wants the token.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use arugula_e2e::now_ms;
use russh::{
    Channel, MethodKind, MethodSet,
    keys::{HashAlg, PrivateKey, ssh_key},
    server::{Auth, ChannelOpenHandle, Handler, Msg, Session},
};
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, info, warn};

use crate::App;

/// The raw stream kind a guest's connection takes to the daemon.
pub const STREAM_KIND: &[u8] = b"ssh-guest";
/// Routes one daemon may have at once (one per live invite).
const MAX_ROUTES: usize = 64;
/// Connections open at once, logged in or not.
const MAX_CONNECTIONS: usize = 256;
/// Hops one connection may open (a guest needs one).
const MAX_HOPS: usize = 2;
/// A connection that hasn't logged in by now is dropped.
const LOGIN_WITHIN: Duration = Duration::from_secs(20);

/// The jump host's address and key, as invites name them.
pub struct Jump {
    /// What guests dial: a host name or address...
    pub host: String,
    /// ...and port.
    pub port: u16,
    key: PrivateKey,
    connections: Arc<AtomicUsize>,
}

impl Jump {
    /// The key kept in `key_path`, made the first time.
    pub fn new(key_path: &Path, host: String, port: u16) -> anyhow::Result<Self> {
        Ok(Self { host, port, key: host_key(key_path)?, connections: Arc::default() })
    }

    /// How a known-hosts file names this hop.
    pub fn known_hosts(&self) -> String {
        let name = if self.port == 22 { self.host.clone() } else { format!("[{}]:{}", self.host, self.port) };
        format!("{name} {}", self.key.public_key().to_openssh().unwrap_or_default())
    }

    /// What `/control.json` says about it, for daemons making invites.
    pub fn describe(&self) -> arugula_control_wire::GuestJump {
        arugula_control_wire::GuestJump {
            host: self.host.clone(),
            port: self.port,
            known_hosts: self.known_hosts(),
            fingerprint: self.key.public_key().fingerprint(HashAlg::Sha256).to_string(),
        }
    }
}

fn host_key(path: &Path) -> anyhow::Result<PrivateKey> {
    match std::fs::read_to_string(path) {
        Ok(pem) => Ok(PrivateKey::from_openssh(pem)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let pair = ssh_key::private::Ed25519Keypair::from_seed(&arugula_e2e::random::<32>());
            let key = PrivateKey::from(pair);
            let pem = key.to_openssh(ssh_key::LineEnding::LF)?;
            let tmp = path.with_extension("tmp");
            {
                use std::os::unix::fs::OpenOptionsExt;
                let mut f =
                    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
                std::io::Write::write_all(&mut f, pem.as_bytes())?;
            }
            std::fs::rename(&tmp, path)?;
            info!(path = %path.display(), "made the guest ssh jump host's key");
            Ok(key)
        }
        Err(e) => Err(e.into()),
    }
}

/// Where the key lives: beside the database.
pub fn key_path(db: &Path) -> PathBuf {
    db.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new(".")).join("guest_ssh_host_key")
}

/// Routes daemons have registered: a route's hash to its daemon, for as
/// long as the daemon's relay socket that registered it is up.
#[derive(Default)]
pub struct Routes {
    map: Mutex<HashMap<String, (String, u64)>>,
}

impl Routes {
    /// The daemon's routes are now `digests` (replacing what it had).
    pub fn set(&self, daemon: &str, generation: u64, digests: &[String]) {
        let mut m = self.map.lock().unwrap();
        m.retain(|_, (d, _)| d != daemon);
        for h in digests.iter().take(MAX_ROUTES) {
            // A hash nobody else holds: two daemons can't share a route.
            m.entry(h.clone()).or_insert_with(|| (daemon.to_owned(), generation));
        }
    }

    /// The daemon's relay socket closed: its routes go with it.
    pub fn drop_daemon(&self, daemon: &str, generation: u64) {
        self.map.lock().unwrap().retain(|_, (d, g)| !(d == daemon && *g == generation));
    }

    /// The daemon a route opens the hop to.
    pub fn lookup(&self, route: &str) -> Option<String> {
        self.map.lock().unwrap().get(&crate::auth::hash(route)).map(|(d, _)| d.clone())
    }
}

fn good_digest(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A text message from a daemon's relay socket, if it's about routes:
/// whether it was. Control answers `guest.routes.ok` with its `seq`, so
/// the daemon knows its invite works before it hands it out.
pub fn from_daemon(app: &App, daemon: &str, generation: u64, text: &str) -> bool {
    #[derive(Deserialize)]
    struct Msg {
        t: String,
        #[serde(default)]
        seq: u64,
        #[serde(default)]
        routes: Vec<String>,
    }
    let Ok(m) = serde_json::from_str::<Msg>(text) else { return false };
    if m.t != "guest.routes" {
        return false;
    }
    let routes: Vec<String> = m.routes.into_iter().filter(|h| good_digest(h)).collect();
    if app.jump.is_some() {
        app.guest_routes.set(daemon, generation, &routes);
        debug!(%daemon, routes = routes.len(), "guest routes");
    }
    let mut ok = json!({ "t": "guest.routes.ok", "seq": m.seq });
    if app.jump.is_none() {
        ok["off"] = json!("this control has no ssh jump host for guests");
    }
    app.relay.text(daemon, &ok.to_string());
    true
}

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

/// Serve the jump host on `listener`, for good.
pub async fn serve(app: Arc<App>, listener: tokio::net::TcpListener) {
    let Some(jump) = &app.jump else { return };
    let config = Arc::new(config(jump.key.clone()));
    let connections = jump.connections.clone();
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                warn!(error = %e, "guest ssh jump host: accept");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if connections.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
            connections.fetch_sub(1, Ordering::Relaxed);
            debug!(%peer, "guest ssh jump host: too many connections");
            continue;
        }
        let _ = tcp.set_nodelay(true);
        let authed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hop = Hop {
            app: app.clone(),
            peer,
            route: None,
            hops: 0,
            authed: authed.clone(),
            _slot: Slot(connections.clone()),
        };
        let config = config.clone();
        tokio::spawn(async move {
            let s = match tokio::time::timeout(LOGIN_WITHIN, russh::server::run_stream(config, tcp, hop)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => return debug!(%peer, error = %e, "guest ssh jump host: handshake"),
                Err(_) => return debug!(%peer, "guest ssh jump host: no handshake in time"),
            };
            let handle = s.handle();
            tokio::spawn(async move {
                tokio::time::sleep(LOGIN_WITHIN).await;
                if !authed.load(Ordering::Relaxed) {
                    let _ = handle
                        .disconnect(russh::Disconnect::ByApplication, "no login in time".into(), String::new())
                        .await;
                }
            });
            if let Err(e) = s.await {
                debug!(%peer, error = %e, "guest ssh jump host: session ended");
            }
        });
    }
}

/// One connection's place under [`MAX_CONNECTIONS`].
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One guest's connection to the hop.
struct Hop {
    app: Arc<App>,
    peer: SocketAddr,
    /// The route it logged in with.
    route: Option<String>,
    hops: usize,
    authed: Arc<std::sync::atomic::AtomicBool>,
    _slot: Slot,
}

impl Handler for Hop {
    type Error = russh::Error;

    async fn auth_none(&mut self, user: &str) -> Result<Auth, Self::Error> {
        if self.app.guest_routes.lookup(user).is_none() {
            info!(peer = %self.peer, "guest ssh jump host: refused a route");
            return Ok(Auth::reject());
        }
        self.route = Some(user.to_owned());
        self.authed.store(true, Ordering::Relaxed);
        Ok(Auth::Accept)
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        _port: u32,
        _from: &str,
        _from_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // Looked up again: the invite may have ended since the login.
        let daemon = self.route.as_deref().and_then(|r| self.app.guest_routes.lookup(r));
        let Some(daemon) = daemon.filter(|d| d.eq_ignore_ascii_case(host) && self.hops < MAX_HOPS) else {
            debug!(peer = %self.peer, %host, "guest ssh jump host: refused a hop");
            reply.reject(russh::ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        };
        let account = self.app.db.daemon_account(&daemon).ok().flatten();
        let ticket = account
            .as_deref()
            .and_then(|a| self.app.relay.admit(&self.app.db, a, false, self.app.stripe.is_some()).ok());
        let stream = self.app.relay.daemon_mux(&daemon).and_then(|m| m.open_raw(STREAM_KIND).ok());
        let (Some(account), Some(ticket), Some(stream)) = (account, ticket, stream) else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        self.hops += 1;
        reply.accept().await;
        info!(peer = %self.peer, %daemon, "guest ssh: a hop to a daemon");
        let app = self.app.clone();
        tokio::spawn(async move {
            let mut guest = channel.into_stream();
            let mut stream = stream;
            let moved = tokio::select! {
                r = tokio::io::copy_bidirectional(&mut guest, &mut stream) => r.map(|(a, b)| a + b).unwrap_or(0),
                _ = ticket.gone() => 0,
            };
            ticket.count(moved);
            if let Err(e) = app.db.add_relay_bytes(&account, &crate::day(now_ms()), moved) {
                warn!(error = %e, "metering");
            }
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_belong_to_one_daemon_and_go_with_its_socket() {
        let r = Routes::default();
        let h = crate::auth::hash("rsecret");
        r.set("d1", 1, std::slice::from_ref(&h));
        assert_eq!(r.lookup("rsecret").as_deref(), Some("d1"));
        assert_eq!(r.lookup("rother"), None);
        // Another daemon can't take it over.
        r.set("d2", 5, std::slice::from_ref(&h));
        assert_eq!(r.lookup("rsecret").as_deref(), Some("d1"));
        // An older socket closing leaves the newer one's routes.
        r.set("d1", 2, std::slice::from_ref(&h));
        r.drop_daemon("d1", 1);
        assert_eq!(r.lookup("rsecret").as_deref(), Some("d1"));
        r.drop_daemon("d1", 2);
        assert_eq!(r.lookup("rsecret"), None);
        // Withdrawn by setting none.
        r.set("d1", 3, std::slice::from_ref(&h));
        r.set("d1", 3, &[]);
        assert_eq!(r.lookup("rsecret"), None);
        assert!(good_digest(&h) && !good_digest("xyz") && !good_digest(&h.to_uppercase()));
    }

    #[test]
    fn the_key_is_made_once_and_kept_private() {
        let d = std::env::temp_dir().join(format!("ilc-jump-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = key_path(&d.join("control.db"));
        let a = Jump::new(&p, "control".into(), 2222).unwrap();
        let b = Jump::new(&p, "control".into(), 2222).unwrap();
        assert_eq!(a.known_hosts(), b.known_hosts());
        assert!(a.known_hosts().starts_with("[control]:2222 ssh-ed25519 "), "{}", a.known_hosts());
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(d).unwrap();
    }
}
