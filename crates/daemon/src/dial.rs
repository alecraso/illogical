//! The dial-out transport (M4c), both ends.
//!
//! **A host that can only dial out** (`--peer wss://home… --peer-token-file
//! F`) keeps one WebSocket open to the home daemon's `/api/dial`, with its
//! per-host token, and redials with backoff whenever it drops. It serves the
//! same WebSocket protocol and HTTP API it serves on its own port, over
//! streams the home daemon opens in that socket (`arugula_e2e::mux`). It works
//! standalone all along: the tunnel is just one more way in.
//!
//! **The home daemon** treats it as one more host (`transport: dial_out`)
//! and answers for it at `/h/<name>/ws` and `/h/<name>/api/...`, behind its
//! own access checks: a client or the CLI uses those exactly as it would the
//! host's own URL. It holds no layout of the host and keeps nothing; it
//! forwards each request over a fresh stream. It is not a hub: a host can't
//! open streams (only answer), and what a host serves over its tunnel has
//! no tunnels of its own in it.
//!
//! What comes back from a host is served on the home daemon's origin, so it
//! is defanged: no cookies, no CORS, `nosniff`, and a sandboxing CSP, so a
//! hostile host can't put a page on our origin.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    extract::{
        Path, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use tokio::{
    io::DuplexStream,
    sync::{Notify, mpsc},
};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use tracing::{info, warn};

use arugula_e2e::mux::Mux;

use crate::server::App;

/// How often each end pings, and how long silence lasts before the tunnel
/// is given up as dead.
const PING_EVERY: Duration = Duration::from_secs(15);
const DEAD_AFTER: Duration = Duration::from_secs(45);
/// Redial backoff.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

pub const DIAL_PATH: &str = "/api/dial";

// ---------------------------------------------------------------- home end

/// The dial-out hosts connected now.
#[derive(Default)]
pub struct DialOuts {
    live: Mutex<HashMap<String, Live>>,
    next: std::sync::atomic::AtomicU64,
}

struct Live {
    generation: u64,
    mux: Mux,
    stop: Arc<Notify>,
}

impl DialOuts {
    /// A host connected; an older connection of the same host is dropped.
    fn insert(&self, name: &str, mux: Mux, stop: Arc<Notify>) -> u64 {
        let generation = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(old) = self.live.lock().unwrap().insert(name.to_owned(), Live { generation, mux, stop }) {
            old.stop.notify_one();
        }
        generation
    }

    fn remove(&self, name: &str, generation: u64) {
        let mut live = self.live.lock().unwrap();
        if live.get(name).is_some_and(|l| l.generation == generation) {
            live.remove(name);
        }
    }

    pub fn get(&self, name: &str) -> Option<Mux> {
        self.live.lock().unwrap().get(name).map(|l| l.mux.clone())
    }

    /// Cut a host off (its token was revoked or replaced); whether it was
    /// connected.
    pub fn drop_host(&self, name: &str) -> bool {
        match self.live.lock().unwrap().remove(name) {
            Some(l) => {
                l.stop.notify_one();
                true
            }
            None => false,
        }
    }
}

/// `Authorization: Bearer <token>`.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim)
}

fn refuse(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": msg }))).into_response()
}

/// Routes on the home daemon: hosts dialing in, and clients reaching them.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route(DIAL_PATH, get(dial))
        .route("/h/{name}/ws", any(via_ws))
        .route("/h/{name}/api/{*rest}", any(via_api))
}

async fn dial(State(app): State<Arc<App>>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let Some(token) = bearer(&headers) else {
        return refuse(StatusCode::UNAUTHORIZED, "a host token is needed (Authorization: Bearer)");
    };
    let Some(name) = app.hosts.host_for_token(token) else {
        warn!("refused a dial-out host: unknown or revoked token");
        return refuse(StatusCode::FORBIDDEN, "invalid or revoked host token");
    };
    upgrade.max_message_size(1 << 20).on_upgrade(move |socket| home_end(app, name, socket))
}

async fn home_end(app: Arc<App>, name: String, socket: WebSocket) {
    let (mux, mut out) = Mux::new(None);
    let stop = Arc::new(Notify::new());
    let generation = app.dial_outs.insert(&name, mux.clone(), stop.clone());
    app.hosts.seen(&name);
    info!(host = name, "dial-out host connected");
    let (mut tx, mut rx) = socket.split();
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            m = rx.next() => match m {
                Some(Ok(Message::Binary(b))) => {
                    heard = Instant::now();
                    if let Err(e) = mux.handle(&b) {
                        warn!(host = name, error = e, "dial-out host broke the protocol");
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => heard = Instant::now(),
            },
            Some(f) = out.recv() => if tx.send(Message::Binary(f.into())).await.is_err() { break },
            _ = ping.tick() => {
                if heard.elapsed() > DEAD_AFTER {
                    warn!(host = name, "dial-out host went quiet");
                    break;
                }
                if tx.send(Message::Ping(Default::default())).await.is_err() { break }
                app.hosts.seen(&name);
            }
            _ = stop.notified() => {
                let _ = tx.send(Message::Close(None)).await;
                break;
            }
        }
    }
    mux.close();
    app.dial_outs.remove(&name, generation);
    info!(host = name, "dial-out host disconnected");
}

async fn via_ws(State(app): State<Arc<App>>, Path(name): Path<String>, req: Request) -> Response {
    forward(app, name, "/ws".into(), req).await
}

async fn via_api(State(app): State<Arc<App>>, Path((name, rest)): Path<(String, String)>, req: Request) -> Response {
    forward(app, name, format!("/api/{rest}"), req).await
}

/// Request headers that mean something only here: who the caller is to us,
/// and credentials for us.
const DROP_REQUEST: [&str; 7] = [
    "authorization",
    "cookie",
    "origin",
    "tailscale-user-login",
    "tailscale-user-name",
    "tailscale-user-profile-pic",
    "tailscale-headers-info",
];

/// One request to a dial-out host, over a stream of its own.
async fn forward(app: Arc<App>, name: String, path: String, mut req: Request) -> Response {
    let Some(mux) = app.dial_outs.get(&name) else {
        return refuse(StatusCode::BAD_GATEWAY, &format!("{name} is not connected"));
    };
    let upgrade = req.headers().get(header::UPGRADE).is_some();
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
    let (mut parts, body) = req.into_parts();
    let query = parts.uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    parts.uri = match format!("{path}{query}").parse() {
        Ok(u) => u,
        Err(_) => return refuse(StatusCode::BAD_REQUEST, "bad path"),
    };
    for h in DROP_REQUEST {
        parts.headers.remove(h);
    }
    parts.headers.insert(header::HOST, HeaderValue::from_static("localhost"));
    let stream = match mux.open() {
        Ok(s) => s,
        Err(e) => return refuse(StatusCode::BAD_GATEWAY, &format!("{name}: {e}")),
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
        Ok(x) => x,
        Err(e) => return refuse(StatusCode::BAD_GATEWAY, &format!("{name}: {e}")),
    };
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });
    let mut res = match sender.send_request(axum::http::Request::from_parts(parts, body)).await {
        Ok(r) => r,
        Err(e) => return refuse(StatusCode::BAD_GATEWAY, &format!("{name}: {e}")),
    };
    defang(res.headers_mut());
    if res.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(client) = client_upgrade
    {
        let server = hyper::upgrade::on(&mut res);
        tokio::spawn(async move {
            let (Ok(client), Ok(server)) = tokio::join!(client, server) else { return };
            let (mut a, mut b) = (TokioIo::new(client), TokioIo::new(server));
            let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
        });
    }
    res.map(Body::new)
}

/// A host's answer, made safe to serve on our origin.
pub(crate) fn defang(h: &mut HeaderMap) {
    h.remove(header::SET_COOKIE);
    let cors: Vec<_> = h.keys().filter(|k| k.as_str().starts_with("access-control-")).cloned().collect();
    for k in cors {
        h.remove(k);
    }
    let renders = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_none_or(|t| {
        let t = t.to_ascii_lowercase();
        t.contains("html") || t.contains("xml") || t.contains("svg")
    });
    if renders && h.contains_key(header::CONTENT_TYPE) {
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'; frame-ancestors 'none'"),
    );
}

// ---------------------------------------------------------------- host end

/// `--peer` and friends.
#[derive(Debug, Clone)]
pub struct PeerOpts {
    /// `wss://home…/api/dial` (a bare origin gets the path).
    pub url: String,
    /// The per-host token, in a file (0600).
    pub token_file: PathBuf,
    /// An invite to trade for a token when the file doesn't exist yet.
    pub join: Option<String>,
    /// This host's name, for joining.
    pub name: String,
}

/// `ws(s)://host[:port]` plus the dial path, unless one is given.
pub fn dial_url(peer: &str) -> anyhow::Result<reqwest::Url> {
    let mut u = reqwest::Url::parse(peer.trim())?;
    match u.scheme() {
        "ws" | "wss" => {}
        "http" => u.set_scheme("ws").map_err(|_| anyhow::anyhow!("bad --peer"))?,
        "https" => u.set_scheme("wss").map_err(|_| anyhow::anyhow!("bad --peer"))?,
        s => anyhow::bail!("--peer {peer}: want wss:// (not {s}://)"),
    }
    if u.path() == "/" || u.path().is_empty() {
        u.set_path(DIAL_PATH);
    }
    Ok(u)
}

/// The home daemon's HTTP(S) origin, for joining and for pushing history.
pub fn home_origin(peer: &str) -> anyhow::Result<String> {
    let mut u = dial_url(peer)?;
    let scheme = if u.scheme() == "wss" { "https" } else { "http" };
    u.set_scheme(scheme).map_err(|_| anyhow::anyhow!("bad --peer"))?;
    Ok(u.origin().ascii_serialization())
}

/// The token, from its file; trading the invite for one first if there's
/// no file yet.
pub async fn token(opts: &PeerOpts) -> anyhow::Result<String> {
    if let Ok(t) = std::fs::read_to_string(&opts.token_file)
        && !t.trim().is_empty()
    {
        return Ok(t.trim().to_owned());
    }
    let Some(invite) = &opts.join else {
        anyhow::bail!("no host token in {} (mint one with `arugula hosts token NAME`)", opts.token_file.display());
    };
    let body = arugula_proto::hosts::JoinRequest {
        token: invite.clone(),
        host: arugula_proto::hosts::AddHost {
            name: opts.name.clone(),
            urls: vec![],
            transport: arugula_proto::hosts::Transport::DialOut,
            ssh: None,
        },
    };
    let res = crate::roots::client()
        .post(format!("{}/api/hosts/join", home_origin(&opts.url)?))
        .json(&body)
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    if !res.status().is_success() {
        anyhow::bail!("joining {}: HTTP {}: {}", opts.url, res.status(), res.text().await.unwrap_or_default());
    }
    let joined: arugula_proto::hosts::Joined = res.json().await?;
    let token = joined.token.ok_or_else(|| anyhow::anyhow!("the home daemon sent no token"))?;
    if let Some(dir) = opts.token_file.parent() {
        crate::store::private_dir(dir)?;
    }
    crate::store::write_atomic(&opts.token_file, token.as_bytes())?;
    info!(file = %opts.token_file.display(), "joined; host token saved");
    Ok(token)
}

/// A listener for axum whose connections are the streams the home daemon
/// opens, over whichever tunnel is up.
pub struct Streams(pub mpsc::UnboundedReceiver<DuplexStream>);

impl axum::serve::Listener for Streams {
    type Io = DuplexStream;
    type Addr = &'static str;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.recv().await {
            Some(s) => (s, "tunnel"),
            // Nobody will dial again: no more connections, ever.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok("tunnel")
    }
}

/// Keep a tunnel to the home daemon up, for good: dial, serve streams into
/// `accept` while it lasts, back off, dial again.
pub async fn keep_dialing(opts: PeerOpts, accept: mpsc::UnboundedSender<DuplexStream>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        let started = Instant::now();
        match connect_once(&opts, &accept).await {
            Ok(()) => info!(peer = opts.url, "tunnel to the home daemon closed"),
            Err(e) => warn!(peer = opts.url, error = %e, "can't reach the home daemon"),
        }
        if started.elapsed() > Duration::from_secs(30) {
            backoff = BACKOFF_MIN;
        }
        // Jitter, so a fleet of sandboxes doesn't redial in step.
        let jitter = Duration::from_millis(u64::from(std::process::id() % 500));
        tokio::time::sleep(backoff + jitter).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

pub type Ws = tokio_tungstenite::WebSocketStream<Box<dyn Io>>;
pub trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Io for T {}

async fn open_socket(opts: &PeerOpts) -> anyhow::Result<Ws> {
    let token = token(opts).await?;
    let url = dial_url(&opts.url)?;
    open_ws(&url, &[(header::AUTHORIZATION.as_str(), &format!("Bearer {token}"))]).await
}

/// A WebSocket to `url` (ws or wss) with extra request headers: the tunnel
/// to a home daemon, or (M18) to control's relay.
pub async fn open_ws(url: &reqwest::Url, headers: &[(&str, &str)]) -> anyhow::Result<Ws> {
    let mut req = url.as_str().into_client_request()?;
    for (k, v) in headers {
        req.headers_mut().insert(axum::http::HeaderName::from_bytes(k.as_bytes())?, HeaderValue::from_str(v)?);
    }
    let host = url.host_str().ok_or_else(|| anyhow::anyhow!("{url} has no host"))?.to_owned();
    let port = url.port_or_known_default().unwrap_or(443);
    let tcp = tokio::time::timeout(Duration::from_secs(15), tokio::net::TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| anyhow::anyhow!("connecting timed out"))??;
    tcp.set_nodelay(true)?;
    let io: Box<dyn Io> = if url.scheme() == "wss" {
        let name = tokio_rustls::rustls::pki_types::ServerName::try_from(host)?;
        Box::new(tls_connector()?.connect(name, tcp).await?)
    } else {
        Box::new(tcp)
    };
    let mut config = tungstenite::protocol::WebSocketConfig::default();
    config.max_message_size = Some(1 << 20);
    let (ws, _) = tokio_tungstenite::client_async_with_config(req, io, Some(config)).await.map_err(|e| match e {
        tungstenite::Error::Http(r) => {
            let said = format!(
                "{} said {}: {}",
                url.host_str().unwrap_or("it"),
                r.status(),
                r.body().as_deref().map(String::from_utf8_lossy).unwrap_or_default()
            );
            let wait = r.headers().get("retry-after").and_then(|v| v.to_str().ok()?.trim().parse().ok());
            match wait {
                Some(secs) if r.status() == 503 => {
                    anyhow::Error::new(Busy { wait: Duration::from_secs(secs).min(MOST_WAIT), said })
                }
                _ => anyhow::anyhow!(said),
            }
        }
        e => e.into(),
    })?;
    Ok(ws)
}

/// The other end is full for now and said how long to wait (`503` with
/// `Retry-After`: control's relay at its ceiling, #344), up to
/// [`MOST_WAIT`].
#[derive(Debug)]
pub struct Busy {
    pub wait: Duration,
    said: String,
}

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.said)
    }
}

impl std::error::Error for Busy {}

const MOST_WAIT: Duration = Duration::from_secs(600);

fn tls_connector() -> anyhow::Result<tokio_rustls::TlsConnector> {
    use tokio_rustls::rustls;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = crate::roots::verifier(provider.clone())?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

async fn connect_once(opts: &PeerOpts, accept: &mpsc::UnboundedSender<DuplexStream>) -> anyhow::Result<()> {
    let ws = open_socket(opts).await?;
    info!(peer = opts.url, "tunnel to the home daemon up");
    serve_mux(ws, accept, None, None, None).await
}

/// Text messages on control's relay socket besides `trust` (M40, M65):
/// what to do with those that come, and messages to send: the latest of
/// each `out` (sent on connect and whenever it changes).
pub struct Texts<'a> {
    pub on_text: &'a (dyn Fn(&str) + Send + Sync),
    pub out: Vec<tokio::sync::watch::Receiver<Option<String>>>,
}

/// The host end of a mux over `ws`: streams the other end opens go to
/// `accept` (raw streams to `raw`, with their kind, when given), until the
/// socket closes or goes quiet.
/// A text message `trust` (control's relay) wakes `nudge`; others go to
/// `texts`.
pub async fn serve_mux(
    ws: Ws,
    accept: &mpsc::UnboundedSender<DuplexStream>,
    raw: Option<&mpsc::UnboundedSender<(Vec<u8>, DuplexStream)>>,
    nudge: Option<&Notify>,
    texts: Option<Texts<'_>>,
) -> anyhow::Result<()> {
    let (mux, mut out) = Mux::with_raw(Some(accept.clone()), raw.cloned());
    let (mut tx, mut rx) = ws.split();
    // Each `out`'s latest message, now and as it changes, in one queue.
    let (say_tx, mut say) = mpsc::unbounded_channel::<String>();
    let mut sayers = Vec::new();
    let on_text = match texts {
        Some(t) => {
            for mut w in t.out {
                let say_tx = say_tx.clone();
                sayers.push(tokio::spawn(async move {
                    loop {
                        if let Some(m) = w.borrow_and_update().clone()
                            && say_tx.send(m).is_err()
                        {
                            return;
                        }
                        if w.changed().await.is_err() {
                            return;
                        }
                    }
                }));
            }
            Some(t.on_text)
        }
        None => None,
    };
    drop(say_tx);
    struct Abort(Vec<tokio::task::JoinHandle<()>>);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.iter().for_each(|h| h.abort());
        }
    }
    let _sayers = Abort(sayers);
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut heard = Instant::now();
    let result = loop {
        tokio::select! {
            m = rx.next() => match m {
                Some(Ok(tungstenite::Message::Binary(b))) => {
                    heard = Instant::now();
                    if let Err(e) = mux.handle(&b) {
                        break Err(anyhow::anyhow!("the other end broke the protocol: {e}"));
                    }
                }
                Some(Ok(tungstenite::Message::Close(_))) | None => break Ok(()),
                Some(Err(e)) => break Err(e.into()),
                Some(Ok(tungstenite::Message::Text(t))) => {
                    heard = Instant::now();
                    if t.as_str() == "trust" {
                        if let Some(n) = nudge {
                            n.notify_one();
                        }
                    } else if let Some(f) = on_text {
                        f(t.as_str());
                    }
                }
                Some(Ok(_)) => heard = Instant::now(),
            },
            Some(f) = out.recv() => {
                if let Err(e) = tx.send(tungstenite::Message::Binary(f.into())).await { break Err(e.into()) }
            }
            Some(m) = say.recv() => {
                if let Err(e) = tx.send(tungstenite::Message::Text(m.into())).await { break Err(e.into()) }
            }
            _ = ping.tick() => {
                if heard.elapsed() > DEAD_AFTER {
                    break Err(anyhow::anyhow!("the other end went quiet"));
                }
                if let Err(e) = tx.send(tungstenite::Message::Ping(Default::default())).await { break Err(e.into()) }
            }
        }
    };
    mux.close();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_urls() {
        assert_eq!(dial_url("wss://geek.example.ts.net").unwrap().as_str(), "wss://geek.example.ts.net/api/dial");
        assert_eq!(dial_url("https://geek.example.ts.net/").unwrap().as_str(), "wss://geek.example.ts.net/api/dial");
        assert_eq!(dial_url("ws://127.0.0.1:7730/x/dial").unwrap().as_str(), "ws://127.0.0.1:7730/x/dial");
        assert!(dial_url("ftp://x").is_err());
        assert_eq!(home_origin("wss://geek.example.ts.net").unwrap(), "https://geek.example.ts.net");
        assert_eq!(home_origin("ws://127.0.0.1:7730").unwrap(), "http://127.0.0.1:7730");
    }

    #[test]
    fn answers_from_a_host_are_defanged() {
        let mut h = HeaderMap::new();
        h.insert(header::CONTENT_TYPE, "text/html".parse().unwrap());
        h.insert(header::SET_COOKIE, "a=b".parse().unwrap());
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
        defang(&mut h);
        assert_eq!(h[header::CONTENT_TYPE], "text/plain; charset=utf-8");
        assert!(!h.contains_key(header::SET_COOKIE));
        assert!(!h.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert!(h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().starts_with("sandbox"));
        let mut json = HeaderMap::new();
        json.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        defang(&mut json);
        assert_eq!(json[header::CONTENT_TYPE], "application/json");
    }

    #[tokio::test]
    async fn a_full_relay_says_how_long_to_wait() {
        // Control's relay at its ceiling (#344); and a plain refusal.
        let full = || async {
            let mut r =
                (StatusCode::SERVICE_UNAVAILABLE, r#"{"error":"control is full: try again shortly"}"#).into_response();
            r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
            r
        };
        let app = Router::new()
            .route("/full", get(full))
            .route("/no", get(|| async { (StatusCode::TOO_MANY_REQUESTS, "slow down") }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let url = |p: &str| reqwest::Url::parse(&format!("ws://{at}{p}")).unwrap();
        let e = open_ws(&url("/full"), &[]).await.err().unwrap();
        assert_eq!(e.downcast_ref::<Busy>().unwrap().wait, Duration::from_secs(30));
        assert!(e.to_string().contains("503 Service Unavailable: {\"error\":\"control is full"), "{e}");
        let e = open_ws(&url("/no"), &[]).await.err().unwrap();
        assert!(e.downcast_ref::<Busy>().is_none(), "{e}");
    }

    #[test]
    fn bearer_tokens() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Bearer ilh_abc".parse().unwrap());
        assert_eq!(bearer(&h), Some("ilh_abc"));
        h.insert(header::AUTHORIZATION, "Basic xyz".parse().unwrap());
        assert_eq!(bearer(&h), None);
    }
}
