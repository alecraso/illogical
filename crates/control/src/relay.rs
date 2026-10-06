//! The relay (M18): M4c's dial-out transport with control at the home end.
//!
//! An enrolled daemon keeps one WebSocket open to `/api/relay/dial`
//! (signed with its key) and serves streams over it with the mux. A
//! client that can't reach the daemon directly connects to
//! `/api/relay/c/<daemon id>`; control opens a stream and splices the two.
//! What crosses is Noise messages (`illogical_e2e::channel`): control
//! counts them and can't read them. Each is a WebSocket message on the
//! client side and `len (u32 BE) ‖ bytes` on the stream.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    extract::{
        Path, Query, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use illogical_e2e::{channel::MAX_WIRE, mux::Mux, now_ms};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tracing::{info, warn};

use crate::{
    App,
    auth::{DaemonAuth, Session},
    err,
};

const PING_EVERY: Duration = Duration::from_secs(15);
/// A text message on a daemon's socket: "fetch your certificates now".
pub const NUDGE: &str = "trust";
const DEAD_AFTER: Duration = Duration::from_secs(45);
const CLIENT_PING: Duration = Duration::from_secs(30);
/// Each socket's read buffer (#174): the default, 128 KiB, made an idle
/// relay connection cost about 150 KB; terminal traffic is small messages.
/// Bigger messages still arrive whole, read in more than one go.
const CLIENT_READ_BUFFER: usize = 8 * 1024;
/// A daemon's socket carries all its streams.
const DAEMON_READ_BUFFER: usize = 32 * 1024;
/// The largest message a daemon's socket takes: a mux frame's most data
/// (its window) and header, with room to spare.
const DAEMON_MAX_MESSAGE: usize = 512 * 1024;
/// What a socket refused over the relay's ceiling (#344) hears, and how
/// long it's asked to wait.
pub const FULL: &str = "control is full: try again shortly";
const FULL_RETRY: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Relay {
    live: Mutex<HashMap<String, Live>>,
    next: AtomicU64,
    /// What each account has open through the relay (#174).
    accounts: Arc<Mutex<HashMap<String, Arc<Use>>>>,
    /// Daemons hung up on by [`Relay::redial`], to nudge when they're back.
    renudge: Mutex<std::collections::HashSet<String>>,
    /// Every relay socket open, and how many are daemons (#344).
    open: Arc<Open>,
    pub caps: Caps,
}

/// The relay's sockets in all, against [`Caps::total`].
#[derive(Default)]
struct Open {
    all: AtomicUsize,
    daemons: AtomicUsize,
    /// Sockets refused for the ceiling, ever, and whether the last one
    /// asked was (so the log says when it fills and when it has room).
    refused: AtomicU64,
    full: AtomicBool,
}

/// Per-account relay limits (#174), so one account can't take the whole
/// machine: sockets at once, and (while billing is off) bytes a day,
/// past which its relayed traffic slows down as billing's does. 0 is no
/// limit.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// Client sockets (`/api/relay/c`, `/api/relay/m`, read-only links).
    pub sockets: usize,
    /// Daemons dialed in.
    pub daemons: usize,
    /// Relayed bytes a day, both ways, while billing is off.
    pub daily_bytes: u64,
    /// Relay sockets of every kind, all accounts together (#344): below
    /// the proxy's connection limit, so a full relay leaves room for
    /// control's pages and sign-ins.
    pub total: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self { sockets: 32, daemons: 50, daily_bytes: 2_000_000_000, total: 5_000 }
    }
}

/// Why a socket wasn't let onto the relay.
#[derive(Debug, PartialEq)]
pub enum Refused {
    /// Its account is at one of its own limits.
    Account(String),
    /// The relay is at its ceiling (#344).
    Full,
}

/// One account's open relay sockets.
struct Use {
    /// (client sockets, daemon sockets), changed under the map's lock.
    open: Mutex<(usize, usize)>,
    /// Today (`YYYY-MM-DD`) and its relayed bytes: the database's count
    /// when the first socket opened, plus what's moved since.
    today: Mutex<(String, u64)>,
    /// Set when the account is deleted: its sockets hang up.
    gone: tokio::sync::watch::Sender<bool>,
}

/// An open relay socket, counted against its account until dropped.
pub struct Ticket {
    map: Arc<Mutex<HashMap<String, Arc<Use>>>>,
    account: String,
    u: Arc<Use>,
    open: Arc<Open>,
    daemon: bool,
    /// The byte budget to slow down past (0: none).
    budget: u64,
}

impl Ticket {
    /// Count bytes moved; whether the account is now past today's budget.
    pub fn count(&self, n: u64) -> bool {
        let mut t = self.u.today.lock().unwrap();
        let day = crate::day(now_ms());
        if t.0 != day {
            *t = (day, 0);
        }
        t.1 += n;
        self.budget > 0 && t.1 > self.budget
    }

    /// Resolves when the account is deleted.
    pub async fn gone(&self) {
        let mut rx = self.u.gone.subscribe();
        let _ = rx.wait_for(|g| *g).await;
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.open.all.fetch_sub(1, Ordering::Relaxed);
        if self.daemon {
            self.open.daemons.fetch_sub(1, Ordering::Relaxed);
        }
        let mut map = self.map.lock().unwrap();
        let mut open = self.u.open.lock().unwrap();
        if self.daemon {
            open.1 = open.1.saturating_sub(1);
        } else {
            open.0 = open.0.saturating_sub(1);
        }
        if *open == (0, 0) && map.get(&self.account).is_some_and(|u| Arc::ptr_eq(u, &self.u)) {
            drop(open);
            map.remove(&self.account);
        }
    }
}

struct Live {
    generation: u64,
    mux: Mux,
    stop: Arc<tokio::sync::Notify>,
    /// Tells the daemon to fetch its account's certificates now.
    nudge: Arc<tokio::sync::Notify>,
    /// Text messages for the daemon (M40: forge pokes and heartbeats).
    text: tokio::sync::mpsc::UnboundedSender<String>,
}

impl Relay {
    pub fn new(caps: Caps) -> Self {
        Self { caps, ..Default::default() }
    }

    /// Count a socket against `account` and the relay's ceiling, or say
    /// why not (#174, #344). An account at its own limit is refused for
    /// that and takes nothing from the ceiling; sockets already open stay.
    /// `billing`: control bills (its own allowance applies, not the
    /// daily budget).
    pub fn admit(&self, db: &crate::db::Db, account: &str, daemon: bool, billing: bool) -> Result<Ticket, Refused> {
        let mut map = self.accounts.lock().unwrap();
        let u = match map.get(account) {
            Some(u) => u.clone(),
            None => {
                let day = crate::day(now_ms());
                let used = db.relay_bytes(account, &day).unwrap_or(0);
                let u = Arc::new(Use {
                    open: Mutex::new((0, 0)),
                    today: Mutex::new((day, used)),
                    gone: tokio::sync::watch::channel(false).0,
                });
                map.insert(account.to_owned(), u.clone());
                u
            }
        };
        let mut open = u.open.lock().unwrap();
        let (n, cap, what) = if daemon {
            (&mut open.1, self.caps.daemons, "machines connected to the relay")
        } else {
            (&mut open.0, self.caps.sockets, "connections through the relay")
        };
        let all = &self.open.all;
        let why = if cap > 0 && *n >= cap {
            Some(Refused::Account(format!("this account has {n} {what}, the most at once; close some first")))
        } else if self.caps.total > 0 && all.load(Ordering::Relaxed) >= self.caps.total {
            Some(Refused::Full)
        } else {
            None
        };
        if let Some(why) = why {
            let empty = *open == (0, 0);
            drop(open);
            if empty {
                map.remove(account);
            }
            if why == Refused::Full {
                self.open.refused.fetch_add(1, Ordering::Relaxed);
                if !self.open.full.swap(true, Ordering::Relaxed) {
                    warn!(sockets = self.caps.total, "the relay is full: refusing new sockets");
                }
            }
            return Err(why);
        }
        *n += 1;
        drop(open);
        // Under the map's lock, so the ceiling holds.
        all.fetch_add(1, Ordering::Relaxed);
        if daemon {
            self.open.daemons.fetch_add(1, Ordering::Relaxed);
        }
        if self.open.full.swap(false, Ordering::Relaxed) {
            info!(refused = self.open.refused.load(Ordering::Relaxed), "the relay has room again");
        }
        Ok(Ticket {
            map: self.accounts.clone(),
            account: account.to_owned(),
            u,
            open: self.open.clone(),
            daemon,
            budget: if billing { 0 } else { self.caps.daily_bytes },
        })
    }

    /// Relay sockets open (all, daemons), the ceiling, and how many were
    /// refused for it since control started (#344).
    pub fn counts(&self) -> (usize, usize, usize, u64) {
        let o = &self.open;
        (
            o.all.load(Ordering::Relaxed),
            o.daemons.load(Ordering::Relaxed),
            self.caps.total,
            o.refused.load(Ordering::Relaxed),
        )
    }

    /// Log the relay's sockets each minute they've changed (#344), for
    /// watching `fly logs`.
    pub async fn log_counts(app: Arc<App>) {
        let mut last = None;
        let mut every = tokio::time::interval(Duration::from_secs(60));
        loop {
            every.tick().await;
            let now = app.relay.counts();
            if last != Some(now) {
                let (sockets, daemons, max, refused) = now;
                info!(sockets, daemons, clients = sockets.saturating_sub(daemons), max, refused, "relay sockets");
                last = Some(now);
            }
        }
    }

    /// The account was deleted (#173): hang up its open sockets.
    pub fn account_gone(&self, account: &str) {
        if let Some(u) = self.accounts.lock().unwrap().get(account) {
            u.gone.send_replace(true);
        }
    }

    pub fn online(&self, id: &str) -> bool {
        self.live.lock().unwrap().contains_key(id)
    }

    /// The daemon's dial-out mux, if it's connected.
    pub fn daemon_mux(&self, id: &str) -> Option<Mux> {
        self.mux(id)
    }

    fn mux(&self, id: &str) -> Option<Mux> {
        self.live.lock().unwrap().get(id).map(|l| l.mux.clone())
    }

    /// The account's devices changed (an approval, a revocation): its
    /// daemons fetch certificates now rather than within the minute, so a
    /// new device gets in and a removed one is cut off at once.
    pub fn nudge(&self, daemons: &[String]) {
        let live = self.live.lock().unwrap();
        for id in daemons {
            if let Some(l) = live.get(id) {
                l.nudge.notify_one();
            }
        }
    }

    /// A text message to a connected daemon (M40); dropped if it isn't.
    pub fn text(&self, id: &str, msg: &str) {
        if let Some(l) = self.live.lock().unwrap().get(id) {
            let _ = l.text.send(msg.to_owned());
        }
    }

    /// Hang up on these daemons now, and nudge each as it dials back in
    /// (#206: a team they were in was deleted). Whatever they relayed ends
    /// at once, rather than at their next check of certificates and teams,
    /// and that check comes as soon as they're back.
    pub fn redial(&self, ids: &[String]) {
        self.renudge.lock().unwrap().extend(ids.iter().cloned());
        for id in ids {
            self.drop_daemon(id);
        }
    }

    /// A daemon left or was revoked: hang up on it.
    pub fn drop_daemon(&self, id: &str) {
        if let Some(l) = self.live.lock().unwrap().remove(id) {
            l.stop.notify_one();
            l.mux.close();
        }
    }
}

pub async fn dial(
    State(app): State<Arc<App>>,
    d: DaemonAuth,
    Query(q): Query<illogical_control_wire::DialQuery>,
    up: WebSocketUpgrade,
) -> Response {
    let urls = q.direct_urls();
    let id = d.cert.device.clone();
    let ticket = match app.relay.admit(&app.db, &d.cert.account, true, app.stripe.is_some()) {
        Ok(t) => t,
        Err(why) => return refuse(why, false, up),
    };
    if let Err(e) = app.db.seen(&id, urls.as_deref(), now_ms()) {
        warn!(error = %e, "recording a daemon");
    }
    // Mux frames (at most a window of data and a header) and forge text.
    up.max_message_size(DAEMON_MAX_MESSAGE)
        .max_frame_size(DAEMON_MAX_MESSAGE)
        .read_buffer_size(DAEMON_READ_BUFFER)
        .on_upgrade(move |ws| daemon_socket(app, id, ws, ticket))
}

async fn daemon_socket(app: Arc<App>, id: String, ws: WebSocket, ticket: Ticket) {
    let (mux, mut out) = Mux::new(None);
    let stop = Arc::new(tokio::sync::Notify::new());
    let nudge = Arc::new(tokio::sync::Notify::new());
    let (text, mut texts) = tokio::sync::mpsc::unbounded_channel::<String>();
    let generation = app.relay.next.fetch_add(1, Ordering::Relaxed);
    if let Some(old) = app
        .relay
        .live
        .lock()
        .unwrap()
        .insert(id.clone(), Live { generation, mux: mux.clone(), stop: stop.clone(), nudge: nudge.clone(), text })
    {
        // A daemon that reconnected: the old socket is dead or about to be.
        old.stop.notify_one();
        old.mux.close();
    }
    if app.relay.renudge.lock().unwrap().remove(&id) {
        nudge.notify_one();
    }
    info!(daemon = %id, "daemon connected to the relay");
    let (mut tx, mut rx) = ws.split();
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            f = out.recv() => match f {
                Some(f) => if tx.send(Message::Binary(f.into())).await.is_err() { break },
                None => break,
            },
            m = rx.next() => match m {
                Some(Ok(Message::Binary(b))) => {
                    heard = Instant::now();
                    if let Err(e) = mux.handle(&b) {
                        warn!(daemon = %id, error = %e, "daemon broke the mux protocol");
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(Message::Text(t))) => {
                    heard = Instant::now();
                    if !crate::guest_jump::from_daemon(&app, &id, generation, t.as_str()) {
                        crate::forge::from_daemon(&app, &id, generation, t.as_str());
                    }
                }
                Some(Ok(_)) => heard = Instant::now(),
            },
            Some(t) = texts.recv() => if tx.send(Message::Text(t.into())).await.is_err() { break },
            _ = ping.tick() => {
                if heard.elapsed() > DEAD_AFTER || tx.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
                let _ = app.db.seen(&id, None, now_ms());
            }
            _ = stop.notified() => break,
            _ = ticket.gone() => break,
            _ = nudge.notified() => if tx.send(Message::Text(NUDGE.into())).await.is_err() { break },
        }
    }
    mux.close();
    let mut live = app.relay.live.lock().unwrap();
    if live.get(&id).is_some_and(|l| l.generation == generation) {
        live.remove(&id);
    }
    drop(live);
    app.forge.drop_daemon(&id, generation);
    app.guest_routes.drop_daemon(&id, generation);
    let _ = app.db.seen(&id, None, now_ms());
    info!(daemon = %id, "daemon left the relay");
}

/// A refused socket's answer. Over the ceiling (#344), a browser can't
/// read a refused upgrade's status, so it gets the socket and at once a
/// close that says why (1013, try again later). Daemons and the CLI,
/// which sign their requests (a browser can't), get 503 with
/// `Retry-After`.
fn refuse(why: Refused, browser: bool, up: WebSocketUpgrade) -> Response {
    match why {
        Refused::Account(why) => err(StatusCode::TOO_MANY_REQUESTS, &why).into_response(),
        Refused::Full if browser => up.on_upgrade(|mut ws| async move {
            let _ = ws.send(Message::Close(Some(CloseFrame { code: 1013, reason: FULL.into() }))).await;
        }),
        Refused::Full => {
            let mut r = err(StatusCode::SERVICE_UNAVAILABLE, FULL).into_response();
            r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(FULL_RETRY.as_secs()));
            r
        }
    }
}

/// Not signed, so a browser: daemons and the CLI sign their requests.
fn browser(headers: &HeaderMap) -> bool {
    !headers.contains_key(crate::auth::AUTH_HEADER)
}

pub async fn client(
    State(app): State<Arc<App>>,
    s: Session,
    Path(id): Path<String>,
    headers: HeaderMap,
    up: WebSocketUpgrade,
) -> Response {
    // Its owner's, a team's member, or someone it was shared with (the
    // daemon checks for itself; this only routes).
    match crate::teams::may_reach(&app, &s.account, &id) {
        Ok(true) => {}
        Ok(false) => return err(StatusCode::NOT_FOUND, "no such daemon").into_response(),
        Err(e) => return crate::ApiError::from(e).into_response(),
    }
    splice_to(app, id, Some(s.account), &headers, up)
}

/// A read-only link's viewer (M19): no account. Only to a daemon that has
/// live links, and rate-limited; the daemon checks the link's key.
pub async fn link(
    State(app): State<Arc<App>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    up: WebSocketUpgrade,
) -> Response {
    if let Err(e) = app.limits.check(crate::limit::LINKS, app.limits.client_ip(peer, &headers)) {
        return e.into_response();
    }
    match app.db.daemon_has_links(&id, now_ms()) {
        Ok(true) => splice_to(app, id, None, &headers, up),
        Ok(false) => err(StatusCode::NOT_FOUND, "that link has expired").into_response(),
        Err(e) => crate::ApiError::from(e).into_response(),
    }
}

fn splice_to(
    app: Arc<App>,
    id: String,
    account: Option<String>,
    headers: &HeaderMap,
    up: WebSocketUpgrade,
) -> Response {
    // Links count against the daemon's owner (#174: sockets too).
    let Some(who) = account.clone().or_else(|| app.db.daemon_account(&id).ok().flatten()) else {
        return err(StatusCode::NOT_FOUND, "no such daemon").into_response();
    };
    let ticket = match app.relay.admit(&app.db, &who, false, app.stripe.is_some()) {
        Ok(t) => t,
        Err(why) => return refuse(why, browser(headers), up),
    };
    // A hosted sandbox (M20) doesn't dial in: it's reached through the
    // provider's proxy, which wakes it.
    if let Ok(Some(sandbox)) = app.db.sandbox_of_daemon(&id)
        && app.hosted.is_some()
    {
        return up.max_message_size(MAX_WIRE).read_buffer_size(CLIENT_READ_BUFFER).on_upgrade(move |ws| async move {
            let (up, down) = tokio::select! {
                r = sandbox_splice(&app, &sandbox, ws, &ticket) => match r {
                    Ok(n) => n,
                    Err(e) => {
                        warn!(%sandbox, error = %e, "can't reach the sandbox");
                        return;
                    }
                },
                _ = ticket.gone() => return,
            };
            let _ = app.db.add_relay_bytes(&who, &crate::day(now_ms()), up + down);
        });
    }
    // Past twice the free allowance, a free account's relayed traffic
    // slows down (M22); it's warned before that.
    let slow =
        account.as_deref().is_some_and(|a| crate::billing::relay_standing(&app, a).is_ok_and(|(_, _, slow)| slow));
    let Some(mux) = app.relay.mux(&id) else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "that daemon isn't connected to the relay").into_response();
    };
    let Ok(stream) = mux.open() else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "that daemon just went away").into_response();
    };
    up.max_message_size(MAX_WIRE).read_buffer_size(CLIENT_READ_BUFFER).on_upgrade(move |ws| async move {
        let done = tokio::select! {
            n = splice(ws, stream, slow, &ticket) => Some(n),
            _ = ticket.gone() => None,
        };
        let Some((up, down)) = done else { return };
        let day = crate::day(now_ms());
        if let Err(e) = app.db.add_relay_bytes(&who, &day, up + down) {
            warn!(error = %e, "metering");
        }
    })
}

/// Client WebSocket messages to length-prefixed frames on the stream, and
/// back. Returns the bytes moved each way.
/// `slow`: at most about 64 KB/s down (over the free relay allowance);
/// so too past the account's daily budget (#174).
async fn splice(ws: WebSocket, stream: DuplexStream, slow: bool, ticket: &Ticket) -> (u64, u64) {
    let (mut wtx, mut wrx) = ws.split();
    let (mut rd, mut wr) = tokio::io::split(stream);
    let up = async {
        let mut n = 0u64;
        while let Some(Ok(m)) = wrx.next().await {
            match m {
                Message::Binary(b) => {
                    n += b.len() as u64;
                    if ticket.count(b.len() as u64) {
                        tokio::time::sleep(throttle(b.len())).await;
                    }
                    let mut f = Vec::with_capacity(4 + b.len());
                    f.extend_from_slice(&(b.len() as u32).to_be_bytes());
                    f.extend_from_slice(&b);
                    if wr.write_all(&f).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = wr.shutdown().await;
        n
    };
    // Frames from the stream, read in a task of their own: read_exact
    // can't be raced against the ping timer without losing bytes.
    let (frames_tx, mut frames) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let reader = tokio::spawn(async move {
        let mut len = [0u8; 4];
        while rd.read_exact(&mut len).await.is_ok() {
            let l = u32::from_be_bytes(len) as usize;
            if l > MAX_WIRE {
                break;
            }
            let mut b = vec![0u8; l];
            if rd.read_exact(&mut b).await.is_err() || frames_tx.send(b).await.is_err() {
                break;
            }
        }
    });
    let down = async {
        let mut n = 0u64;
        // Pings keep an idle channel open through proxies (Fly's among them).
        let mut ping = tokio::time::interval(CLIENT_PING);
        ping.tick().await;
        loop {
            tokio::select! {
                f = frames.recv() => {
                    let Some(b) = f else { break };
                    n += b.len() as u64;
                    if ticket.count(b.len() as u64) || slow {
                        tokio::time::sleep(throttle(b.len())).await;
                    }
                    if wtx.send(Message::Binary(b.into())).await.is_err() {
                        break;
                    }
                }
                _ = ping.tick() => if wtx.send(Message::Ping(Default::default())).await.is_err() { break },
            }
        }
        let _ = wtx.close().await;
        n
    };
    let r = tokio::join!(up, down);
    reader.abort();
    r
}

/// A client's channel onto a hosted sandbox's daemon (`/e2e`), through the
/// provider's proxy: WebSocket messages both ways, one for one.
async fn sandbox_splice(app: &App, sandbox: &str, ws: WebSocket, ticket: &Ticket) -> anyhow::Result<(u64, u64)> {
    use tokio_tungstenite::tungstenite::Message as T;
    let h = app.hosted.as_ref().ok_or_else(|| anyhow::anyhow!("no hosted sandboxes"))?;
    let stream = h.sprites.dial(sandbox, crate::sandboxes::PORT).await?;
    let url = format!("ws://localhost:{}/e2e", crate::sandboxes::PORT);
    let (daemon, _) = tokio_tungstenite::client_async(url, stream).await?;
    let (mut dtx, mut drx) = daemon.split();
    let (mut ctx, mut crx) = ws.split();
    let up = async {
        let mut n = 0u64;
        while let Some(Ok(m)) = crx.next().await {
            match m {
                Message::Binary(b) => {
                    n += b.len() as u64;
                    if ticket.count(b.len() as u64) {
                        tokio::time::sleep(throttle(b.len())).await;
                    }
                    if dtx.send(T::Binary(b.to_vec().into())).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = dtx.close().await;
        n
    };
    let down = async {
        let mut n = 0u64;
        while let Some(Ok(m)) = drx.next().await {
            match m {
                T::Binary(b) => {
                    n += b.len() as u64;
                    if ticket.count(b.len() as u64) {
                        tokio::time::sleep(throttle(b.len())).await;
                    }
                    if ctx.send(Message::Binary(b.to_vec().into())).await.is_err() {
                        break;
                    }
                }
                T::Close(_) => break,
                _ => {}
            }
        }
        let _ = ctx.close().await;
        n
    };
    Ok(tokio::join!(up, down))
}

// ---- many daemons over one socket (M25)
//
// A page that shows every machine at once would otherwise hold a relay
// socket per daemon, and browsers space out WebSocket connections to one
// address past about eight (S16: 20 daemons took 2–5 s to come back after
// a wake). Instead it opens `/api/relay/m` once and carries each daemon's
// Noise channel inside it, as numbered channels. Each binary message is
// `kind (1) ‖ channel (4, BE) ‖ payload`:
//
// - `OPEN` (page to control): payload is the daemon's id;
// - `OPENED` (control to page): the daemon's stream is up;
// - `DATA` (both ways): one Noise message;
// - `CLOSE` (both ways): the channel is over; from control, payload is why.
//
// Control routes and counts, as for `/api/relay/c/<id>`, and can't read
// what crosses.

/// How long `n` bytes take at about 64 KB/s: a slowed account's pace.
fn throttle(n: usize) -> Duration {
    Duration::from_micros(n as u64 * 1_000_000 / 65_536)
}

const M_OPEN: u8 = 1;
const M_DATA: u8 = 2;
const M_CLOSE: u8 = 3;
const M_OPENED: u8 = 4;
/// Channels one page may hold at once.
const MAX_CHANNELS: usize = 64;

fn mframe(kind: u8, chan: u32, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(5 + payload.len());
    f.push(kind);
    f.extend_from_slice(&chan.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

pub async fn many(State(app): State<Arc<App>>, s: Session, headers: HeaderMap, up: WebSocketUpgrade) -> Response {
    let ticket = match app.relay.admit(&app.db, &s.account, false, app.stripe.is_some()) {
        Ok(t) => Arc::new(t),
        Err(why) => return refuse(why, browser(&headers), up),
    };
    up.max_message_size(MAX_WIRE + 5)
        .read_buffer_size(CLIENT_READ_BUFFER)
        .on_upgrade(move |ws| many_socket(app, s.account, ws, ticket))
}

async fn many_socket(app: Arc<App>, account: String, ws: WebSocket, ticket: Arc<Ticket>) {
    let (mut wtx, mut wrx) = ws.split();
    let (out_tx, mut out) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
    let mut chans: HashMap<u32, tokio::sync::mpsc::Sender<Vec<u8>>> = HashMap::new();
    let (done_tx, mut done) = tokio::sync::mpsc::unbounded_channel::<u32>();
    let mut ping = tokio::time::interval(CLIENT_PING);
    ping.tick().await;
    loop {
        tokio::select! {
            m = wrx.next() => {
                let b = match m {
                    Some(Ok(Message::Binary(b))) => b,
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => continue,
                };
                if b.len() < 5 {
                    break;
                }
                let chan = u32::from_be_bytes(b[1..5].try_into().unwrap());
                let payload = &b[5..];
                match b[0] {
                    M_OPEN if !chans.contains_key(&chan) => {
                        if chans.len() >= MAX_CHANNELS {
                            let _ = out_tx.send(mframe(M_CLOSE, chan, b"too many channels on one socket")).await;
                            continue;
                        }
                        let id = String::from_utf8_lossy(payload).into_owned();
                        let (tx, rx) = tokio::sync::mpsc::channel(64);
                        chans.insert(chan, tx);
                        let (app, account, out, done) = (app.clone(), account.clone(), out_tx.clone(), done_tx.clone());
                        let ticket = ticket.clone();
                        tokio::spawn(async move {
                            let opened = channel(app, account, id, chan, rx, out.clone(), &ticket).await;
                            // After everything it sent, in the same queue: a
                            // daemon's last words (why it hung up) arrive.
                            if opened {
                                let _ = out.send(mframe(M_CLOSE, chan, b"")).await;
                            }
                            let _ = done.send(chan);
                        });
                    }
                    M_DATA => {
                        if let Some(tx) = chans.get(&chan) {
                            // A full channel (a daemon not reading) holds up
                            // only this socket's reading, briefly; drop the
                            // channel rather than everyone.
                            if tx.try_send(payload.to_vec()).is_err() {
                                chans.remove(&chan);
                                let _ = out_tx.send(mframe(M_CLOSE, chan, b"the daemon isn't keeping up")).await;
                            }
                        }
                    }
                    M_CLOSE => {
                        chans.remove(&chan);
                    }
                    _ => {}
                }
            }
            f = out.recv() => match f {
                Some(f) => if wtx.send(Message::Binary(f.into())).await.is_err() { break },
                None => break,
            },
            Some(chan) = done.recv() => {
                chans.remove(&chan);
            }
            _ = ping.tick() => if wtx.send(Message::Ping(Default::default())).await.is_err() { break },
            _ = ticket.gone() => break,
        }
    }
    // Dropping the senders ends every channel.
}

/// One daemon's channel inside a page's socket: its relay stream, framed as
/// `/api/relay/c/<id>` frames it.
async fn channel(
    app: Arc<App>,
    account: String,
    id: String,
    chan: u32,
    mut from_page: tokio::sync::mpsc::Receiver<Vec<u8>>,
    out: tokio::sync::mpsc::Sender<Vec<u8>>,
    ticket: &Ticket,
) -> bool {
    let refuse = |why: &str| mframe(M_CLOSE, chan, why.as_bytes());
    match crate::teams::may_reach(&app, &account, &id) {
        Ok(true) => {}
        _ => {
            let _ = out.send(refuse("no such daemon")).await;
            return false;
        }
    }
    // Hosted sandboxes are reached through their provider, one socket each.
    if app.hosted.is_some() && matches!(app.db.sandbox_of_daemon(&id), Ok(Some(_))) {
        let _ = out.send(refuse("a hosted sandbox: use /api/relay/c")).await;
        return false;
    }
    let slow = crate::billing::relay_standing(&app, &account).is_ok_and(|(_, _, slow)| slow);
    let Some(stream) = app.relay.mux(&id).and_then(|m| m.open().ok()) else {
        let _ = out.send(refuse("that daemon isn't connected to the relay")).await;
        return false;
    };
    if out.send(mframe(M_OPENED, chan, b"")).await.is_err() {
        return false;
    }
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (sent, got) = (AtomicU64::new(0), AtomicU64::new(0));
    let up = async {
        while let Some(b) = from_page.recv().await {
            sent.fetch_add(b.len() as u64, Ordering::Relaxed);
            if ticket.count(b.len() as u64) {
                tokio::time::sleep(throttle(b.len())).await;
            }
            let mut f = Vec::with_capacity(4 + b.len());
            f.extend_from_slice(&(b.len() as u32).to_be_bytes());
            f.extend_from_slice(&b);
            if wr.write_all(&f).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    };
    let down = async {
        let mut len = [0u8; 4];
        while rd.read_exact(&mut len).await.is_ok() {
            let l = u32::from_be_bytes(len) as usize;
            if l > MAX_WIRE {
                break;
            }
            let mut b = vec![0u8; l];
            if rd.read_exact(&mut b).await.is_err() {
                break;
            }
            got.fetch_add(l as u64, Ordering::Relaxed);
            if ticket.count(l as u64) || slow {
                tokio::time::sleep(throttle(l)).await;
            }
            if out.send(mframe(M_DATA, chan, &b)).await.is_err() {
                break;
            }
        }
    };
    // Either side ending ends the channel.
    tokio::select! {
        _ = up => {},
        _ = down => {},
    }
    let bytes = sent.load(Ordering::Relaxed) + got.load(Ordering::Relaxed);
    let _ = app.db.add_relay_bytes(&account, &crate::day(now_ms()), bytes);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn caps_per_account() {
        let db = crate::db::Db::memory();
        let r = Relay::new(Caps { sockets: 2, daemons: 1, daily_bytes: 100, total: 0 });
        let a1 = r.admit(&db, "a", false, false).unwrap();
        let a2 = r.admit(&db, "a", false, false).unwrap();
        let Err(Refused::Account(why)) = r.admit(&db, "a", false, false) else { panic!("over the account's limit") };
        assert!(why.contains("the most at once"));
        // Someone else's count is their own; machines count apart.
        let _b = r.admit(&db, "b", false, false).unwrap();
        let d = r.admit(&db, "a", true, false).unwrap();
        assert!(r.admit(&db, "a", true, false).is_err());
        drop(a1);
        let a3 = r.admit(&db, "a", false, false).unwrap();
        // The budget is the account's, across its sockets, and counts
        // what's in the database from earlier today.
        assert!(!a2.count(60));
        assert!(a3.count(60));
        drop((a2, a3, d));
        assert!(r.accounts.lock().unwrap().get("a").is_none(), "forgotten once nothing's open");
        db.add_relay_bytes("a", &crate::day(now_ms()), 150).unwrap();
        assert!(r.admit(&db, "a", false, false).unwrap().count(1));
        // With billing on, billing's allowance applies instead.
        assert!(!r.admit(&db, "a", false, true).unwrap().count(1));
        // A deleted account's sockets hear it.
        let t = r.admit(&db, "c", false, false).unwrap();
        r.account_gone("c");
        tokio::time::timeout(Duration::from_secs(1), t.gone()).await.unwrap();
    }

    #[test]
    fn a_ceiling_over_every_account() {
        let db = crate::db::Db::memory();
        let r = Relay::new(Caps { sockets: 2, daemons: 1, daily_bytes: 0, total: 4 });
        // A is at its own limit: refused for that, which takes nothing from
        // the ceiling, so B still gets in.
        let a = [r.admit(&db, "a", false, false).unwrap(), r.admit(&db, "a", false, false).unwrap()];
        for _ in 0..10 {
            assert!(matches!(r.admit(&db, "a", false, false), Err(Refused::Account(_))));
        }
        let b = r.admit(&db, "b", false, false).unwrap();
        let bd = r.admit(&db, "b", true, false).unwrap();
        assert_eq!(r.counts(), (4, 1, 4, 0));
        // Full: anyone's next socket is refused, a daemon's too, and what's
        // open stays open.
        assert_eq!(r.admit(&db, "c", false, false).err(), Some(Refused::Full));
        assert_eq!(r.admit(&db, "c", true, false).err(), Some(Refused::Full));
        assert!(r.accounts.lock().unwrap().get("c").is_none(), "a refused account isn't kept");
        // An account at its own limit still hears that, not that it's full.
        assert!(matches!(r.admit(&db, "a", false, false), Err(Refused::Account(_))));
        assert_eq!(r.counts(), (4, 1, 4, 2));
        // A socket closing makes room.
        drop(b);
        let c = r.admit(&db, "c", false, false).unwrap();
        assert_eq!(r.counts(), (4, 1, 4, 2));
        drop((a, bd, c));
        assert_eq!(r.counts(), (0, 0, 4, 2));
        assert!(r.accounts.lock().unwrap().is_empty());
    }
}
