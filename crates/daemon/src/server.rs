//! HTTP: the embedded web client, and the WebSocket protocol at `/ws`.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    extract::{
        ConnectInfo, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use illogical_proto::{ClientId, ClientMsg, Frame, FrameKind};
use rust_embed::Embed;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{
    access::Access,
    hosts::Hosts,
    mux::{Cmd, MuxHandle},
    pane::{CLIENT_QUEUE, Subscriber, ToClient},
    tailscale::Identify,
};

#[derive(Embed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

pub struct App {
    pub access: Access,
    /// Who is on the other end of a TCP connection (tailscaled's WhoIs).
    pub identify: Identify,
    pub mux: MuxHandle,
    pub push: Option<crate::push::Push>,
    pub hosts: Arc<Hosts>,
    next_client: AtomicU64,
}

impl App {
    pub fn new(
        access: Access,
        identify: Identify,
        mux: MuxHandle,
        push: Option<crate::push::Push>,
        hosts: Arc<Hosts>,
    ) -> Arc<Self> {
        Arc::new(Self { access, identify, mux, push, hosts, next_client: AtomicU64::new(1) })
    }
}

fn api_routes() -> Router<Arc<App>> {
    crate::api::routes().merge(crate::hosts::routes())
}

/// Over TCP (loopback, behind `tailscale serve`, or a tailnet address):
/// every request passes the access checks, and API calls from a browser
/// must come from one of our origins. Serve it with
/// `into_make_service_with_connect_info::<SocketAddr>()`.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/ws", get(ws))
        .merge(api_routes().layer(middleware::from_fn_with_state(app.clone(), api_origin)))
        .fallback(asset)
        .layer(middleware::from_fn_with_state(app.clone(), cors))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Over the Unix socket (the CLI, programs in panes): the socket lives in
/// the user's private state directory, so reaching it is the check.
pub fn local_router(app: Arc<App>) -> Router {
    Router::new().route("/ws", get(local_ws)).merge(api_routes()).with_state(app)
}

/// Cross-site requests can't read our answers, but a POST still lands: so a
/// browser's API call must come from one of our own origins. Programs (no
/// Origin header) are fine.
async fn api_origin(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    match app.access.check_origin(req.headers()) {
        Ok(()) => next.run(req).await,
        Err((status, why)) => {
            warn!(%why, uri = %req.uri(), "rejected API request");
            (status, why).into_response()
        }
    }
}

/// The home daemon's page talks to other daemons' APIs (another origin):
/// browsers allow that only if we say so, and we say so only to origins the
/// access checks accept, exactly. Nothing needs cookies (identity is the
/// tailnet's), so credentials stay off.
async fn cors(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .filter(|o| app.access.origin_allowed(o))
        .map(str::to_owned);
    let Some(origin) = origin else { return next.run(req).await };
    let preflight = req.method() == axum::http::Method::OPTIONS
        && req.headers().contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    let mut res = if preflight { StatusCode::NO_CONTENT.into_response() } else { next.run(req).await };
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&origin) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
    }
    h.append(header::VARY, HeaderValue::from_static("origin"));
    if preflight {
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, DELETE"));
        h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    }
    res
}

async fn guard(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let peer = app.identify.peer(addr).await;
    // Joining the host list needs an invite, not an identity: it's how a
    // tagged sandbox node adds itself. The Host check still applies.
    let joining = req.method() == axum::http::Method::POST && req.uri().path() == crate::hosts::JOIN_PATH;
    let checked = app
        .access
        .check_host(req.headers())
        .and_then(|()| if joining { Ok(()) } else { app.access.check_identity(req.headers(), &peer) });
    let mut res = match checked {
        Ok(()) => next.run(req).await,
        Err((status, why)) => {
            warn!(%why, ?peer, %addr, uri = %req.uri(), "rejected request");
            (status, why).into_response()
        }
    };
    // serve authenticates by source, so any page the owner visits could frame
    // the logged-in app (clickjacking). Nothing frames us legitimately.
    let h = res.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("frame-ancestors 'none'"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    res
}

async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => {
            // Vite fingerprints everything under assets/; the rest must revalidate.
            let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
            Response::builder()
                .header(header::CONTENT_TYPE, file.metadata.mimetype())
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(file.data))
                .unwrap()
        }
        None if path == "index.html" => (StatusCode::NOT_FOUND, "web client not built: run `just web`").into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn ws(State(app): State<Arc<App>>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    if let Err((status, why)) = app.access.check_origin(&headers) {
        warn!(%why, "rejected websocket");
        return (status, why).into_response();
    }
    upgrade.on_upgrade(move |socket| connection(app, socket))
}

async fn local_ws(State(app): State<Arc<App>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| connection(app, socket))
}

async fn connection(app: Arc<App>, mut socket: WebSocket) {
    let client: ClientId = app.next_client.fetch_add(1, Ordering::Relaxed);
    info!(client, "client connected");
    let (data_tx, mut data_rx) = mpsc::channel(CLIENT_QUEUE);
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    app.mux.send(Cmd::Connect { sub: Subscriber { client, data: data_tx, ctrl: ctrl_tx } });

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(msg)) => {
                    if let Err(e) = handle(&app, client, msg) {
                        debug!(client, error = %e, "bad client message");
                    }
                }
                Some(Err(e)) => {
                    debug!(client, error = %e, "websocket error");
                    break;
                }
                None => break,
            },
            Some(out) = ctrl_rx.recv() => if send(&mut socket, out).await.is_err() { break },
            Some(out) = data_rx.recv() => if send(&mut socket, out).await.is_err() { break },
        }
    }
    app.mux.send(Cmd::Disconnect { client });
    info!(client, "client disconnected");
}

fn handle(app: &App, client: ClientId, msg: Message) -> anyhow::Result<()> {
    match msg {
        Message::Text(text) => {
            let msg = serde_json::from_str::<ClientMsg>(&text)?;
            app.mux.send(Cmd::Msg { client, msg });
        }
        Message::Binary(bytes) => {
            let frame = Frame::decode(&bytes)?;
            match frame.kind {
                FrameKind::Input => app.mux.send(Cmd::Input { pane: frame.pane, data: frame.data }),
                k => anyhow::bail!("unexpected frame kind {k:?} from client"),
            }
        }
        _ => {}
    }
    Ok(())
}

async fn send(socket: &mut WebSocket, out: ToClient) -> Result<(), axum::Error> {
    let msg = match out {
        ToClient::Frame(bytes) => Message::Binary(bytes.into()),
        ToClient::Msg(m) => Message::Text(serde_json::to_string(&m).expect("serialize").into()),
    };
    socket.send(msg).await
}
