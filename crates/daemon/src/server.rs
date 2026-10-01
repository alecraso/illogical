//! HTTP: the embedded web client, and the WebSocket protocol at `/ws`.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::{
    Router,
    body::Body,
    extract::{
        Request, State,
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
    mux::{Cmd, MuxHandle},
    pane::{CLIENT_QUEUE, Subscriber, ToClient},
};

#[derive(Embed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

pub struct App {
    pub access: Access,
    pub mux: MuxHandle,
    next_client: AtomicU64,
}

impl App {
    pub fn new(access: Access, mux: MuxHandle) -> Arc<Self> {
        Arc::new(Self { access, mux, next_client: AtomicU64::new(1) })
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/ws", get(ws))
        .fallback(asset)
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

async fn guard(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let mut res = match app.access.check(req.headers()) {
        Ok(()) => next.run(req).await,
        Err((status, why)) => {
            warn!(%why, uri = %req.uri(), "rejected request");
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
