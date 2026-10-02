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
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use illogical_e2e::{channel::MAX_WIRE, mux::Mux, now_ms};
use serde::Deserialize;
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

#[derive(Default)]
pub struct Relay {
    live: Mutex<HashMap<String, Live>>,
    next: AtomicU64,
}

struct Live {
    generation: u64,
    mux: Mux,
    stop: Arc<tokio::sync::Notify>,
    /// Tells the daemon to fetch its account's certificates now.
    nudge: Arc<tokio::sync::Notify>,
}

impl Relay {
    pub fn online(&self, id: &str) -> bool {
        self.live.lock().unwrap().contains_key(id)
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

    /// A daemon left or was revoked: hang up on it.
    pub fn drop_daemon(&self, id: &str) {
        if let Some(l) = self.live.lock().unwrap().remove(id) {
            l.stop.notify_one();
            l.mux.close();
        }
    }
}

#[derive(Deserialize)]
pub struct DialQuery {
    /// JSON list of the daemon's direct URLs, for the directory.
    urls: Option<String>,
}

pub async fn dial(
    State(app): State<Arc<App>>,
    d: DaemonAuth,
    Query(q): Query<DialQuery>,
    up: WebSocketUpgrade,
) -> Response {
    let urls: Option<Vec<String>> = q.urls.and_then(|u| serde_json::from_str(&u).ok());
    let id = d.cert.device.clone();
    if let Err(e) = app.db.seen(&id, urls.as_deref(), now_ms()) {
        warn!(error = %e, "recording a daemon");
    }
    up.on_upgrade(move |ws| daemon_socket(app, id, ws))
}

async fn daemon_socket(app: Arc<App>, id: String, ws: WebSocket) {
    let (mux, mut out) = Mux::new(None);
    let stop = Arc::new(tokio::sync::Notify::new());
    let nudge = Arc::new(tokio::sync::Notify::new());
    let generation = app.relay.next.fetch_add(1, Ordering::Relaxed);
    if let Some(old) = app
        .relay
        .live
        .lock()
        .unwrap()
        .insert(id.clone(), Live { generation, mux: mux.clone(), stop: stop.clone(), nudge: nudge.clone() })
    {
        // A daemon that reconnected: the old socket is dead or about to be.
        old.stop.notify_one();
        old.mux.close();
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
                Some(Ok(_)) => heard = Instant::now(),
            },
            _ = ping.tick() => {
                if heard.elapsed() > DEAD_AFTER || tx.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
                let _ = app.db.seen(&id, None, now_ms());
            }
            _ = stop.notified() => break,
            _ = nudge.notified() => if tx.send(Message::Text(NUDGE.into())).await.is_err() { break },
        }
    }
    mux.close();
    let mut live = app.relay.live.lock().unwrap();
    if live.get(&id).is_some_and(|l| l.generation == generation) {
        live.remove(&id);
    }
    drop(live);
    let _ = app.db.seen(&id, None, now_ms());
    info!(daemon = %id, "daemon left the relay");
}

pub async fn client(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>, up: WebSocketUpgrade) -> Response {
    // Its owner's, a team's member, or someone it was shared with (the
    // daemon checks for itself; this only routes).
    match crate::teams::may_reach(&app, &s.account, &id) {
        Ok(true) => {}
        Ok(false) => return err(StatusCode::NOT_FOUND, "no such daemon").into_response(),
        Err(e) => return crate::ApiError::from(e).into_response(),
    }
    splice_to(app, id, Some(s.account), up)
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
        Ok(true) => splice_to(app, id, None, up),
        Ok(false) => err(StatusCode::NOT_FOUND, "that link has expired").into_response(),
        Err(e) => crate::ApiError::from(e).into_response(),
    }
}

fn splice_to(app: Arc<App>, id: String, account: Option<String>, up: WebSocketUpgrade) -> Response {
    // A hosted sandbox (M20) doesn't dial in: it's reached through the
    // provider's proxy, which wakes it.
    if let Ok(Some(sandbox)) = app.db.sandbox_of_daemon(&id)
        && app.hosted.is_some()
    {
        return up.max_message_size(MAX_WIRE).on_upgrade(move |ws| async move {
            let (up, down) = match sandbox_splice(&app, &sandbox, ws).await {
                Ok(n) => n,
                Err(e) => {
                    warn!(%sandbox, error = %e, "can't reach the sandbox");
                    return;
                }
            };
            let who = account.or_else(|| app.db.daemon_account(&id).ok().flatten());
            if let Some(a) = who {
                let _ = app.db.add_relay_bytes(&a, &crate::day(now_ms()), up + down);
            }
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
    up.max_message_size(MAX_WIRE).on_upgrade(move |ws| async move {
        let (up, down) = splice(ws, stream, slow).await;
        let day = crate::day(now_ms());
        // Links count against the daemon's owner.
        let who = account.or_else(|| app.db.daemon_account(&id).ok().flatten());
        if let Some(a) = who
            && let Err(e) = app.db.add_relay_bytes(&a, &day, up + down)
        {
            warn!(error = %e, "metering");
        }
    })
}

/// Client WebSocket messages to length-prefixed frames on the stream, and
/// back. Returns the bytes moved each way.
/// `slow`: at most about 64 KB/s down (over the free relay allowance).
async fn splice(ws: WebSocket, stream: DuplexStream, slow: bool) -> (u64, u64) {
    let (mut wtx, mut wrx) = ws.split();
    let (mut rd, mut wr) = tokio::io::split(stream);
    let up = async {
        let mut n = 0u64;
        while let Some(Ok(m)) = wrx.next().await {
            match m {
                Message::Binary(b) => {
                    n += b.len() as u64;
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
                    if slow {
                        tokio::time::sleep(Duration::from_micros(b.len() as u64 * 1_000_000 / 65_536)).await;
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
async fn sandbox_splice(app: &App, sandbox: &str, ws: WebSocket) -> anyhow::Result<(u64, u64)> {
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

pub async fn many(State(app): State<Arc<App>>, s: Session, up: WebSocketUpgrade) -> Response {
    up.max_message_size(MAX_WIRE + 5).on_upgrade(move |ws| many_socket(app, s.account, ws))
}

async fn many_socket(app: Arc<App>, account: String, ws: WebSocket) {
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
                        tokio::spawn(async move {
                            let opened = channel(app, account, id, chan, rx, out.clone()).await;
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
            if slow {
                tokio::time::sleep(Duration::from_micros(l as u64 * 1_000_000 / 65_536)).await;
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
