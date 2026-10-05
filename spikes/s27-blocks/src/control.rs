//! Control's half: its page (a stand-in for the real one), the block
//! origins `https://b-<key>.blocks.test`, and the relay.
//!
//! On a block origin control serves only its own static files: a bootstrap
//! page for any navigation the worker didn't answer, the worker, and the
//! WebSocket shim. Everything else the block's page loads comes through the
//! worker, over Noise, from the daemon. `/.s27/relay?d=<daemon>` on the
//! block's own origin splices the browser's socket to a stream on that
//! daemon's dial-out socket, as `/api/relay/c` does.
//!
//! For the spike's checks control counts what it relays and looks for a
//! marker string in it: the test serves a page full of the marker, and a
//! hit would mean control could read a block.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    Json, Router,
    body::Body,
    extract::{
        FromRequest, Query, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use illogical_e2e::{channel::MAX_WIRE, mux::Mux};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct Control {
    /// `https://control.test:<port>`.
    pub origin: String,
    pub block_domain: String,
    pub web: PathBuf,
    pub marker: Vec<u8>,
    daemons: Mutex<HashMap<String, (u64, Mux)>>,
    next: AtomicU64,
    stats: Stats,
    log: Mutex<Vec<serde_json::Value>>,
}

#[derive(Default)]
struct Stats {
    relayed_bytes: AtomicU64,
    relay_sockets: AtomicU64,
    marker_hits: AtomicU64,
    boots: AtomicU64,
    /// Requests for a block's content that reached control: the worker
    /// wasn't in the way (a hard reload, an app's own worker script).
    misses: AtomicU64,
    /// An app's own worker scripts, answered with ours (web/sw.ts).
    app_workers: AtomicU64,
}

impl Control {
    pub fn new(origin: String, block_domain: String, web: PathBuf, marker: String) -> Arc<Self> {
        Arc::new(Self {
            origin,
            block_domain,
            web,
            marker: marker.into_bytes(),
            daemons: Mutex::default(),
            next: AtomicU64::new(1),
            stats: Stats::default(),
            log: Mutex::default(),
        })
    }

    fn note(&self, v: serde_json::Value) {
        let mut log = self.log.lock().unwrap();
        log.push(v);
        let n = log.len();
        if n > 500 {
            log.drain(..n - 500);
        }
    }

    fn scan(&self, b: &[u8]) {
        self.stats.relayed_bytes.fetch_add(b.len() as u64, Ordering::Relaxed);
        if !self.marker.is_empty() && b.windows(self.marker.len()).any(|w| w == self.marker.as_slice()) {
            self.stats.marker_hits.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// What browsers reach (over TLS).
pub fn public(c: Arc<Control>) -> Router {
    Router::new().fallback(by_host).with_state(c)
}

/// Where daemons dial in (plain, loopback: the spike's daemon side of the
/// relay isn't what it measures).
pub fn dial(c: Arc<Control>) -> Router {
    Router::new().route("/relay/dial", get(daemon_dial)).with_state(c)
}

fn file(c: &Control, name: &str, ct: &'static str) -> Response {
    match std::fs::read(c.web.join(name)) {
        Ok(b) => {
            let mut r = Response::new(Body::from(b));
            r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            r
        }
        Err(_) => (StatusCode::NOT_FOUND, format!("{name} isn't built")).into_response(),
    }
}

const JS: &str = "text/javascript; charset=utf-8";
const HTML: &str = "text/html; charset=utf-8";

async fn by_host(State(c): State<Arc<Control>>, req: Request) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let name = host.split(':').next().unwrap_or("").to_owned();
    let path = req.uri().path().to_owned();
    if name == "control.test" {
        return match path.as_str() {
            "/" => file(&c, "parent.html", HTML),
            "/parent.js" => file(&c, "parent.js", JS),
            "/.s27/stats" => stats(&c),
            _ => StatusCode::NOT_FOUND.into_response(),
        };
    }
    let Some(label) = name.strip_suffix(&format!(".{}", c.block_domain)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !label.starts_with("b-") || label.contains('.') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let h = |n: &str| req.headers().get(n).and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    match path.as_str() {
        "/.s27/sw.js" => {
            let mut r = file(&c, "sw.js", JS);
            r.headers_mut().insert("service-worker-allowed", HeaderValue::from_static("/"));
            r
        }
        "/.s27/boot.js" => file(&c, "boot.js", JS),
        "/.s27/shim.js" => file(&c, "shim.js", JS),
        "/.s27/relay" => {
            #[derive(Deserialize)]
            struct Q {
                d: String,
            }
            let Ok(Query(q)) = Query::<Q>::try_from_uri(req.uri()) else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            match WebSocketUpgrade::from_request(req, &()).await {
                Ok(up) => relay(c, q.d, up),
                Err(e) => e.into_response(),
            }
        }
        _ => {
            let (mode, dest, site, sw) =
                (h("sec-fetch-mode"), h("sec-fetch-dest"), h("sec-fetch-site"), h("service-worker"));
            if sw == "script" {
                c.stats.app_workers.fetch_add(1, Ordering::Relaxed);
                // The block's app registering a worker of its own: control
                // can't serve its code (it doesn't have it), so it serves
                // its own worker, which fetches the app's through the
                // channel and runs it inside (see web/sw.ts).
                let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
                let body = format!(
                    "self.S27_APP_SW={};importScripts(\"/.s27/sw.js\");\n",
                    serde_json::to_string(path).unwrap()
                );
                let mut r = Response::new(Body::from(body));
                r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(JS));
                r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                return r;
            }
            let navigating = mode == "navigate";
            c.note(json!({ "host": name, "path": req.uri().to_string(), "mode": mode, "dest": dest, "site": site, "sw": sw }));
            if !navigating {
                // Content control doesn't have: the worker wasn't in the way.
                c.stats.misses.fetch_add(1, Ordering::Relaxed);
                return (StatusCode::SERVICE_UNAVAILABLE, "s27: this request didn't go through the block's worker")
                    .into_response();
            }
            c.stats.boots.fetch_add(1, Ordering::Relaxed);
            boot(&c)
        }
    }
}

/// The bootstrap page: control's code only. It registers the worker, gets
/// the block's key from the parent page, and loads the page again through
/// the worker.
fn boot(c: &Control) -> Response {
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Loading block</title>\
         <style>body{{font:13px system-ui;color:#888;margin:16px}}</style>\
         <p id=s27-state>Connecting…</p>\
         <script>self.S27_CONTROL={control:?}</script><script src=\"/.s27/boot.js\"></script>",
        control = c.origin
    );
    let mut r = Response::new(Body::from(page));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(HTML));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_str(&format!("frame-ancestors {}", c.origin)).unwrap());
    r
}

fn stats(c: &Control) -> Response {
    let s = &c.stats;
    let n = |a: &AtomicU64| a.load(Ordering::Relaxed);
    Json(json!({
        "relayed_bytes": n(&s.relayed_bytes),
        "relay_sockets": n(&s.relay_sockets),
        "marker_hits": n(&s.marker_hits),
        "boots": n(&s.boots),
        "misses": n(&s.misses),
        "app_workers": n(&s.app_workers),
        "daemons": c.daemons.lock().unwrap().keys().cloned().collect::<Vec<_>>(),
        "log": *c.log.lock().unwrap(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct DialQ {
    id: String,
}

async fn daemon_dial(State(c): State<Arc<Control>>, Query(q): Query<DialQ>, up: WebSocketUpgrade) -> Response {
    up.max_message_size(1 << 20).on_upgrade(move |ws| async move {
        let (mux, mut out) = Mux::new(None);
        let generation = c.next.fetch_add(1, Ordering::Relaxed);
        c.daemons.lock().unwrap().insert(q.id.clone(), (generation, mux.clone()));
        let (mut tx, mut rx) = ws.split();
        let writer = tokio::spawn(async move {
            while let Some(f) = out.recv().await {
                if tx.send(Message::Binary(f.into())).await.is_err() {
                    break;
                }
            }
        });
        while let Some(Ok(m)) = rx.next().await {
            if let Message::Binary(b) = m
                && mux.handle(&b).is_err()
            {
                break;
            }
        }
        mux.close();
        writer.abort();
        let mut d = c.daemons.lock().unwrap();
        // A daemon that redialed meanwhile keeps its new socket.
        if d.get(&q.id).is_some_and(|(g, _)| *g == generation) {
            d.remove(&q.id);
        }
    })
}

/// A browser's socket to a daemon: each WebSocket message is one Noise
/// message, `len ‖ bytes` on the stream. Control counts them and can't
/// read them.
fn relay(c: Arc<Control>, daemon: String, up: WebSocketUpgrade) -> Response {
    let Some(mux) = c.daemons.lock().unwrap().get(&daemon).map(|(_, m)| m.clone()) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "that daemon isn't connected to the relay").into_response();
    };
    let Ok(stream) = mux.open() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "that daemon just went away").into_response();
    };
    c.stats.relay_sockets.fetch_add(1, Ordering::Relaxed);
    up.max_message_size(MAX_WIRE).on_upgrade(move |ws: WebSocket| async move {
        let (mut wtx, mut wrx) = ws.split();
        let (mut srd, mut swr) = tokio::io::split(stream);
        let c2 = c.clone();
        let up = async move {
            while let Some(Ok(m)) = wrx.next().await {
                let Message::Binary(b) = m else {
                    if matches!(m, Message::Close(_)) {
                        break;
                    }
                    continue;
                };
                c2.scan(&b);
                let mut f = Vec::with_capacity(4 + b.len());
                f.extend_from_slice(&(b.len() as u32).to_be_bytes());
                f.extend_from_slice(&b);
                if swr.write_all(&f).await.is_err() {
                    break;
                }
            }
            let _ = swr.shutdown().await;
        };
        let down = async move {
            let mut len = [0u8; 4];
            while srd.read_exact(&mut len).await.is_ok() {
                let n = u32::from_be_bytes(len) as usize;
                if n > MAX_WIRE {
                    break;
                }
                let mut b = vec![0u8; n];
                if srd.read_exact(&mut b).await.is_err() {
                    break;
                }
                c.scan(&b);
                if wtx.send(Message::Binary(b.into())).await.is_err() {
                    break;
                }
            }
            let _ = wtx.close().await;
        };
        tokio::select! {
            _ = up => {}
            _ = down => {}
        }
    })
}
