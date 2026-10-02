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
    match app.db.daemon_account(&id) {
        Ok(Some(a)) if a == s.account => {}
        Ok(_) => return err(StatusCode::NOT_FOUND, "no such daemon").into_response(),
        Err(e) => return crate::ApiError::from(e).into_response(),
    }
    let Some(mux) = app.relay.mux(&id) else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "that daemon isn't connected to the relay").into_response();
    };
    let Ok(stream) = mux.open() else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "that daemon just went away").into_response();
    };
    up.max_message_size(MAX_WIRE).on_upgrade(move |ws| async move {
        let (up, down) = splice(ws, stream).await;
        let day = crate::day(now_ms());
        if let Err(e) = app.db.add_relay_bytes(&s.account, &day, up + down) {
            warn!(error = %e, "metering");
        }
    })
}

/// Client WebSocket messages to length-prefixed frames on the stream, and
/// back. Returns the bytes moved each way.
async fn splice(ws: WebSocket, stream: DuplexStream) -> (u64, u64) {
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
