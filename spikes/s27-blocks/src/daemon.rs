//! The daemon's half: the responder of each block's Noise channel, and the
//! proxy from it to the block's port.
//!
//! Channels come two ways, as a real daemon's do: streams over its one
//! dial-out WebSocket to control's relay (`illogical_e2e::mux`, frames
//! `len (u32 BE) ‖ Noise message`), or a WebSocket at
//! `wss://daemon.test/e2e` (the direct path).
//!
//! For the latency baseline it also serves today's block sites itself,
//! `https://b-<block>.direct.test`: the same proxy, reached over HTTP with no
//! worker, no Noise and no relay (what `sites.rs` does on the tailnet).
//!
//! An admin API on loopback stands in for what the daemon learns over its
//! device channel and from control: which devices it trusts, which blocks
//! it has.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, bail};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use illogical_e2e::{
    DeviceKeys,
    channel::{Channel, MAX_WIRE, Msg, Responder, prologue},
    mux::Mux,
    now_ms,
};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    sync::{mpsc, watch},
};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

use crate::wire::{self, Grant, ReqHead, ResHead, WsClose, WsOpen};

const QUEUE: usize = 256;

#[derive(Default)]
pub struct Stats {
    pub channels: AtomicU64,
    pub refused: AtomicU64,
    pub requests: AtomicU64,
    pub direct_requests: AtomicU64,
    pub sockets: AtomicU64,
    pub dials: AtomicU64,
}

pub struct Daemon {
    keys: DeviceKeys,
    trusted: Mutex<Vec<String>>,
    /// Block id to its port.
    blocks: Mutex<HashMap<String, u16>>,
    stats: Stats,
    refusals: Mutex<Vec<String>>,
    /// Bumped to drop every open channel (a daemon restart, as far as the
    /// browser can tell).
    drop: watch::Sender<u64>,
    client: Client<HttpConnector, Body>,
}

impl Daemon {
    pub fn new() -> Arc<Self> {
        let mut http = HttpConnector::new();
        http.set_nodelay(true);
        Arc::new(Self {
            keys: DeviceKeys::generate(),
            trusted: Mutex::default(),
            blocks: Mutex::default(),
            stats: Stats::default(),
            refusals: Mutex::default(),
            drop: watch::channel(0).0,
            client: Client::builder(TokioExecutor::new()).pool_idle_timeout(Duration::from_secs(30)).build(http),
        })
    }

    pub fn id(&self) -> String {
        self.keys.id()
    }

    pub fn noise(&self) -> String {
        hex::encode(self.keys.noise_public)
    }
}

// ---- admin --------------------------------------------------------------

pub fn admin(d: Arc<Daemon>) -> Router {
    #[derive(serde::Deserialize)]
    struct Trust {
        sign: String,
    }
    #[derive(serde::Deserialize)]
    struct NewBlock {
        id: String,
        port: u16,
    }
    Router::new()
        .route(
            "/info",
            get(|State(d): State<Arc<Daemon>>| async move { Json(json!({ "id": d.id(), "noise": d.noise() })) }),
        )
        .route(
            "/trust",
            post(|State(d): State<Arc<Daemon>>, Json(t): Json<Trust>| async move {
                d.trusted.lock().unwrap().push(t.sign.to_ascii_lowercase());
                StatusCode::NO_CONTENT
            }),
        )
        .route(
            "/block",
            post(|State(d): State<Arc<Daemon>>, Json(b): Json<NewBlock>| async move {
                d.blocks.lock().unwrap().insert(b.id, b.port);
                StatusCode::NO_CONTENT
            }),
        )
        .route(
            "/drop",
            post(|State(d): State<Arc<Daemon>>| async move {
                d.drop.send_modify(|g| *g += 1);
                StatusCode::NO_CONTENT
            }),
        )
        .route(
            "/stats",
            get(|State(d): State<Arc<Daemon>>| async move {
                let s = &d.stats;
                let n = |a: &AtomicU64| a.load(Ordering::Relaxed);
                Json(json!({
                    "channels": n(&s.channels),
                    "refused": n(&s.refused),
                    "requests": n(&s.requests),
                    "direct_requests": n(&s.direct_requests),
                    "sockets": n(&s.sockets),
                    "dials": n(&s.dials),
                    "refusals": *d.refusals.lock().unwrap(),
                }))
            }),
        )
        .with_state(d)
}

// ---- the relay: one dial-out socket, streams over it --------------------

/// Keep a socket open to control's relay, redialing when it drops.
pub async fn dial(d: Arc<Daemon>, url: String) {
    loop {
        if let Err(e) = dial_once(&d, &url).await {
            eprintln!("s27 daemon: relay: {e}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn dial_once(d: &Arc<Daemon>, url: &str) -> anyhow::Result<()> {
    // Without TCP_NODELAY, small frames wait on the peer's delayed ACK
    // (40 ms on Linux) behind any unacknowledged one.
    let (ws, _) = tokio_tungstenite::connect_async_with_config(format!("{url}?id={}", d.id()), None, true).await?;
    d.stats.dials.fetch_add(1, Ordering::Relaxed);
    let (mut tx, mut rx) = ws.split();
    let (accept_tx, mut accept_rx) = mpsc::unbounded_channel::<DuplexStream>();
    let (mux, mut out) = Mux::new(Some(accept_tx));
    let writer = tokio::spawn(async move {
        while let Some(f) = out.recv().await {
            if tx.send(tungstenite::Message::Binary(f.into())).await.is_err() {
                break;
            }
        }
    });
    let d2 = d.clone();
    let acceptor = tokio::spawn(async move {
        while let Some(s) = accept_rx.recv().await {
            tokio::spawn(serve_stream(d2.clone(), s));
        }
    });
    while let Some(m) = rx.next().await {
        match m? {
            tungstenite::Message::Binary(b) => {
                if let Err(e) = mux.handle(&b) {
                    bail!("relay broke the protocol: {e}");
                }
            }
            tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }
    mux.close();
    writer.abort();
    acceptor.abort();
    Ok(())
}

/// One relayed channel: `len ‖ Noise message` frames on a mux stream.
async fn serve_stream(d: Arc<Daemon>, stream: DuplexStream) {
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(QUEUE);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(QUEUE);
    let reader = async move {
        let mut len = [0u8; 4];
        while rd.read_exact(&mut len).await.is_ok() {
            let n = u32::from_be_bytes(len) as usize;
            if n > MAX_WIRE {
                break;
            }
            let mut b = vec![0u8; n];
            if rd.read_exact(&mut b).await.is_err() || in_tx.send(b).await.is_err() {
                break;
            }
        }
    };
    let writer = tokio::spawn(async move {
        while let Some(w) = out_rx.recv().await {
            let mut f = Vec::with_capacity(4 + w.len());
            f.extend_from_slice(&(w.len() as u32).to_be_bytes());
            f.extend_from_slice(&w);
            if wr.write_all(&f).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    });
    tokio::select! {
        r = serve(d, in_rx, out_tx) => end(r),
        _ = reader => {}
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
}

/// The direct path: a WebSocket at `wss://daemon.test/e2e`.
async fn serve_ws(d: Arc<Daemon>, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(QUEUE);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(QUEUE);
    let reader = async move {
        while let Some(Ok(m)) = rx.next().await {
            match m {
                Message::Binary(b) => {
                    if in_tx.send(b.to_vec()).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    };
    let writer = tokio::spawn(async move {
        while let Some(w) = out_rx.recv().await {
            if tx.send(Message::Binary(w.into())).await.is_err() {
                break;
            }
        }
        let _ = tx.close().await;
    });
    tokio::select! {
        r = serve(d, in_rx, out_tx) => end(r),
        _ = reader => {}
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
}

fn end(r: anyhow::Result<()>) {
    if let Err(e) = r {
        eprintln!("s27 daemon: channel ended: {e:#}");
    }
}

// ---- a channel ----------------------------------------------------------

/// Seals and queues one whole message at a time (nonces go out in order).
struct Out {
    ch: Arc<Channel>,
    q: tokio::sync::Mutex<mpsc::Sender<Vec<u8>>>,
}

impl Out {
    async fn put(&self, m: &Msg) -> anyhow::Result<()> {
        let q = self.q.lock().await;
        for w in self.ch.seal(m)? {
            q.send(w).await.map_err(|_| anyhow::anyhow!("socket gone"))?;
        }
        Ok(())
    }

    async fn frame(&self, f: Vec<u8>) -> anyhow::Result<()> {
        self.put(&Msg::Binary(f)).await
    }
}

enum Stream {
    /// A request whose body is still coming.
    Http(ReqHead, Vec<u8>),
    /// A request on its way, or a WebSocket.
    Running(tokio::task::JoinHandle<()>, Option<mpsc::UnboundedSender<WsCmd>>),
}

enum WsCmd {
    Msg(tungstenite::Message),
    Close(WsClose),
}

async fn first(inbound: &mut mpsc::Receiver<Vec<u8>>, what: &str) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(15), inbound.recv())
        .await
        .map_err(|_| anyhow::anyhow!("no {what}"))?
        .ok_or_else(|| anyhow::anyhow!("closed before the {what}"))
}

async fn serve(d: Arc<Daemon>, mut inbound: mpsc::Receiver<Vec<u8>>, out: mpsc::Sender<Vec<u8>>) -> anyhow::Result<()> {
    let mut dropped = d.drop.subscribe();
    let m1 = first(&mut inbound, "handshake").await?;
    let (responder, who) = Responder::read(&d.keys, &prologue(&d.keys.id()), &m1)?;
    let (m2, ch) = responder.finish(&[])?;
    out.send(m2).await?;
    let out = Arc::new(Out { ch: Arc::new(ch), q: tokio::sync::Mutex::new(out) });
    let ch = out.ch.clone();

    // The grant comes first, before anything acts.
    let w = first(&mut inbound, "grant").await?;
    let Some(Msg::Text(t)) = ch.open(&w)? else { bail!("expected the grant") };
    let checked = serde_json::from_str::<Grant>(&t).context("a grant").and_then(|g| {
        let trusted = d.trusted.lock().unwrap().clone();
        g.check(&d.id(), &who, now_ms(), &trusted)?;
        let port = d.blocks.lock().unwrap().get(&g.block).copied().context("no such block")?;
        Ok((g, port))
    });
    let (grant, port) = match checked {
        Ok(ok) => ok,
        Err(e) => {
            d.stats.refused.fetch_add(1, Ordering::Relaxed);
            d.refusals.lock().unwrap().push(format!("{e:#}"));
            out.put(&Msg::Text(json!({ "refused": format!("{e:#}") }).to_string())).await?;
            return Ok(());
        }
    };
    d.stats.channels.fetch_add(1, Ordering::Relaxed);
    out.put(&Msg::Text(json!({ "ok": true, "block": grant.block, "now": now_ms() }).to_string())).await?;

    let mut streams: HashMap<u32, Stream> = HashMap::new();
    let result = loop {
        let w = tokio::select! {
            w = inbound.recv() => w,
            _ = dropped.changed() => break Ok(()),
        };
        let Some(w) = w else { break Ok(()) };
        let m = match ch.open(&w) {
            Ok(None) => continue,
            Ok(Some(m)) => m,
            Err(e) => break Err(e),
        };
        let Msg::Binary(f) = m else { continue };
        let (kind, sid, payload) = wire::parse(&f)?;
        streams.retain(|_, s| !matches!(s, Stream::Running(h, _) if h.is_finished()));
        match kind {
            b'H' => {
                let head: ReqHead = serde_json::from_slice(payload)?;
                streams.insert(sid, Stream::Http(head, Vec::new()));
            }
            b'D' => {
                if let Some(Stream::Http(_, body)) = streams.get_mut(&sid) {
                    body.extend_from_slice(payload);
                }
            }
            b'E' => {
                if let Some(Stream::Http(head, body)) = streams.remove(&sid) {
                    d.stats.requests.fetch_add(1, Ordering::Relaxed);
                    let (d, out) = (d.clone(), out.clone());
                    let h = tokio::spawn(async move {
                        if let Err(e) = proxy(&d, &out, sid, port, head, body).await {
                            let _ =
                                out.frame(wire::json_frame(b'X', sid, &json!({ "message": format!("{e:#}") }))).await;
                        }
                    });
                    streams.insert(sid, Stream::Running(h, None));
                }
            }
            b'O' => {
                let open: WsOpen = serde_json::from_slice(payload)?;
                d.stats.sockets.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = mpsc::unbounded_channel();
                let out = out.clone();
                let h = tokio::spawn(async move {
                    if let Err(e) = websocket(&out, sid, port, open, rx).await {
                        let _ = out.frame(wire::json_frame(b'X', sid, &json!({ "message": format!("{e:#}") }))).await;
                    }
                });
                streams.insert(sid, Stream::Running(h, Some(tx)));
            }
            b'M' => {
                if let Some(Stream::Running(_, Some(tx))) = streams.get(&sid)
                    && let Some((&k, data)) = payload.split_first()
                {
                    let m = if k == 0 {
                        tungstenite::Message::Text(String::from_utf8_lossy(data).into_owned().into())
                    } else {
                        tungstenite::Message::Binary(data.to_vec().into())
                    };
                    let _ = tx.send(WsCmd::Msg(m));
                }
            }
            b'C' => {
                if let Some(Stream::Running(_, Some(tx))) = streams.get(&sid) {
                    let _ = tx.send(WsCmd::Close(serde_json::from_slice(payload).unwrap_or_default()));
                }
            }
            b'K' => {
                if let Some(Stream::Running(h, _)) = streams.remove(&sid) {
                    h.abort();
                }
            }
            k => break Err(anyhow::anyhow!("unknown frame kind {k}")),
        }
    };
    for s in streams.into_values() {
        if let Stream::Running(h, _) = s {
            h.abort();
        }
    }
    result
}

// ---- the proxy (as sites.rs: the port sees a request to itself) --------

const HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
];

fn local(port: u16) -> String {
    format!("localhost:{port}")
}

/// The request's headers as the port sees them.
fn request_headers(pairs: &[(String, String)], port: u16) -> HeaderMap {
    let local_origin = format!("http://{}", local(port));
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        let k = k.to_ascii_lowercase();
        if HOP.contains(&k.as_str()) || k.starts_with("x-forwarded-") || k == "forwarded" {
            continue;
        }
        let v = match k.as_str() {
            "origin" => local_origin.clone(),
            // The page's URL, on the block's origin: the same path locally.
            "referer" => match v.find("://").and_then(|i| v[i + 3..].find('/').map(|j| i + 3 + j)) {
                Some(p) => format!("{local_origin}{}", &v[p..]),
                None => local_origin.clone(),
            },
            _ => v.clone(),
        };
        if let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::from_str(&v)) {
            h.append(k, v);
        }
    }
    h.insert(header::HOST, HeaderValue::from_str(&local(port)).unwrap());
    h
}

/// The response's headers as the block's page sees them.
fn response_headers(h: &HeaderMap, port: u16) -> Vec<(String, String)> {
    let locals = [format!("http://localhost:{port}"), format!("http://127.0.0.1:{port}")];
    h.iter()
        .filter(|(k, _)| !HOP.contains(&k.as_str()) && *k != header::X_FRAME_OPTIONS)
        .filter_map(|(k, v)| {
            let mut v = v.to_str().ok()?.to_owned();
            if k == header::LOCATION
                && let Some(rest) = locals.iter().find_map(|l| v.strip_prefix(l.as_str()))
            {
                v = if rest.is_empty() { "/".into() } else { rest.to_owned() };
            }
            Some((k.as_str().to_owned(), v))
        })
        .collect()
}

async fn proxy(d: &Daemon, out: &Out, sid: u32, port: u16, head: ReqHead, body: Vec<u8>) -> anyhow::Result<()> {
    let uri: Uri = format!("http://{}{}", local(port), head.path).parse()?;
    let mut req = Request::builder().method(head.method.as_str()).uri(uri).body(Body::from(body))?;
    *req.headers_mut() = request_headers(&head.headers, port);
    let res = d.client.request(req).await.map_err(|e| anyhow::anyhow!("nothing is answering on port {port}: {e}"))?;
    let rh = ResHead { status: res.status().as_u16(), headers: response_headers(res.headers(), port) };
    out.frame(wire::json_frame(b'R', sid, &rh)).await?;
    let mut body = res.into_body();
    while let Some(f) = body.frame().await {
        if let Ok(data) = f?.into_data()
            && !data.is_empty()
        {
            out.frame(wire::frame(b'D', sid, &data)).await?;
        }
    }
    out.frame(wire::frame(b'E', sid, &[])).await?;
    Ok(())
}

async fn websocket(
    out: &Out,
    sid: u32,
    port: u16,
    open: WsOpen,
    mut cmds: mpsc::UnboundedReceiver<WsCmd>,
) -> anyhow::Result<()> {
    let mut req = format!("ws://{}{}", local(port), open.path).into_client_request()?;
    let h = req.headers_mut();
    h.insert(header::ORIGIN, HeaderValue::from_str(&format!("http://{}", local(port)))?);
    if !open.protocols.is_empty() {
        h.insert(header::SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_str(&open.protocols.join(", "))?);
    }
    let (ws, res) = tokio_tungstenite::connect_async_with_config(req, None, true).await?;
    let protocol = res.headers().get(header::SEC_WEBSOCKET_PROTOCOL).and_then(|v| v.to_str().ok()).unwrap_or("");
    out.frame(wire::json_frame(b'A', sid, &json!({ "protocol": protocol }))).await?;
    let (mut tx, mut rx) = ws.split();
    loop {
        tokio::select! {
            m = rx.next() => {
                let close = |c: Option<tungstenite::protocol::CloseFrame>| WsClose {
                    code: c.as_ref().map(|c| u16::from(c.code)).unwrap_or(1005),
                    reason: c.map(|c| c.reason.to_string()).unwrap_or_default(),
                };
                match m {
                    Some(Ok(tungstenite::Message::Text(t))) => out.frame(wire::frame(b'M', sid, &[&[0u8][..], t.as_bytes()].concat())).await?,
                    Some(Ok(tungstenite::Message::Binary(b))) => out.frame(wire::frame(b'M', sid, &[&[1u8][..], &b[..]].concat())).await?,
                    Some(Ok(tungstenite::Message::Close(c))) => {
                        out.frame(wire::json_frame(b'C', sid, &close(c))).await?;
                        return Ok(());
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => {
                        out.frame(wire::json_frame(b'C', sid, &close(None))).await?;
                        return Ok(());
                    }
                }
            }
            c = cmds.recv() => match c {
                Some(WsCmd::Msg(m)) => tx.send(m).await?,
                Some(WsCmd::Close(c)) => {
                    let code = if c.code == 1000 || (3000..5000).contains(&c.code) { c.code } else { 1000 };
                    let _ = tx.send(tungstenite::Message::Close(Some(tungstenite::protocol::CloseFrame {
                        code: code.into(),
                        reason: c.reason.into(),
                    }))).await;
                    return Ok(());
                }
                None => return Ok(()),
            }
        }
    }
}

// ---- the direct listener: daemon.test/e2e, and today's block sites -----

pub fn direct(d: Arc<Daemon>) -> Router {
    Router::new().fallback(direct_handler).with_state(d)
}

async fn direct_handler(State(d): State<Arc<Daemon>>, req: Request) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let name = host.split(':').next().unwrap_or("");
    if name == "daemon.test" {
        if req.uri().path() != "/e2e" {
            return StatusCode::NOT_FOUND.into_response();
        }
        use axum::extract::FromRequest;
        return match WebSocketUpgrade::from_request(req, &()).await {
            Ok(up) => up.max_message_size(MAX_WIRE).on_upgrade(move |s| serve_ws(d, s)),
            Err(e) => e.into_response(),
        };
    }
    // b-<block>.direct.test: today's path, the daemon's own block site.
    let Some(block) = name.strip_suffix(".direct.test").and_then(|n| n.strip_prefix("b-")) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(port) = d.blocks.lock().unwrap().get(block).copied() else {
        return (StatusCode::NOT_FOUND, "no such block").into_response();
    };
    d.stats.direct_requests.fetch_add(1, Ordering::Relaxed);
    match direct_proxy(&d, port, req).await {
        Ok(r) => r,
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn direct_proxy(d: &Daemon, port: u16, mut req: Request) -> anyhow::Result<Response> {
    let pairs: Vec<(String, String)> =
        req.headers().iter().filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned()))).collect();
    let upgrade = req.headers().get(header::UPGRADE).cloned();
    let path = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_owned();
    let mut headers = request_headers(&pairs, port);
    if let Some(u) = &upgrade {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, u.clone());
        // An upgraded connection is used up: one of its own.
        let client_up = hyper::upgrade::on(&mut req);
        let tcp = tokio::net::TcpStream::connect(local(port)).await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp)).await?;
        tokio::spawn(async move {
            let _ = conn.with_upgrades().await;
        });
        let mut out = Request::builder().method(req.method()).uri(path).body(Body::empty())?;
        *out.headers_mut() = headers;
        let mut res = sender.send_request(out).await?;
        let server_up = hyper::upgrade::on(&mut res);
        tokio::spawn(async move {
            let (Ok(c), Ok(s)) = tokio::join!(client_up, server_up) else { return };
            let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(c), &mut TokioIo::new(s)).await;
        });
        let mut r = Response::builder().status(res.status());
        for (k, v) in res.headers() {
            r = r.header(k, v);
        }
        return Ok(r.body(Body::empty())?);
    }
    let uri: Uri = format!("http://{}{path}", local(port)).parse()?;
    let (parts, body) = req.into_parts();
    let mut out = Request::builder().method(parts.method).uri(uri).body(body)?;
    *out.headers_mut() = headers;
    let res = d.client.request(out).await?;
    let mut r = Response::builder().status(res.status());
    for (k, v) in response_headers(res.headers(), port) {
        r = r.header(k, v);
    }
    Ok(r.body(Body::new(res.into_body()))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_look_local() {
        let pairs = [
            ("Host", "b-x.blocks.test:7753"),
            ("Origin", "https://b-x.blocks.test:7753"),
            ("Referer", "https://b-x.blocks.test:7753/a/b?c"),
            ("X-Forwarded-For", "1.2.3.4"),
            ("Connection", "keep-alive"),
            ("Accept", "text/html"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()));
        let h = request_headers(&pairs, 5173);
        assert_eq!(h["host"], "localhost:5173");
        assert_eq!(h["origin"], "http://localhost:5173");
        assert_eq!(h["referer"], "http://localhost:5173/a/b?c");
        assert_eq!(h["accept"], "text/html");
        assert!(!h.contains_key("x-forwarded-for") && !h.contains_key("connection"));
    }

    #[test]
    fn redirects_stay_on_the_block() {
        let mut h = HeaderMap::new();
        h.insert(header::LOCATION, "http://localhost:5173/next".parse().unwrap());
        h.insert(header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
        let out = response_headers(&h, 5173);
        assert_eq!(out, [("location".to_owned(), "/next".to_owned())]);
    }
}
