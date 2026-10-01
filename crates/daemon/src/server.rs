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
    http::{HeaderMap, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use illogical_proto::{ClientId, ClientMsg, Frame, FrameKind, ServerMsg};
use rust_embed::Embed;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{
    access::Access,
    pane::{CLIENT_QUEUE, PaneHandle, Subscriber, ToClient},
};

#[derive(Embed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

pub struct App {
    pub access: Access,
    pub panes: Vec<PaneHandle>,
    next_client: AtomicU64,
}

impl App {
    pub fn new(access: Access, panes: Vec<PaneHandle>) -> Arc<Self> {
        Arc::new(Self {
            access,
            panes,
            next_client: AtomicU64::new(1),
        })
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
    match app.access.check(req.headers()) {
        Ok(()) => next.run(req).await,
        Err((status, why)) => {
            warn!(%why, uri = %req.uri(), "rejected request");
            (status, why).into_response()
        }
    }
}

async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => {
            // Vite fingerprints everything under assets/; the rest must revalidate.
            let cache = if path.starts_with("assets/") {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            };
            Response::builder()
                .header(header::CONTENT_TYPE, file.metadata.mimetype())
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(file.data))
                .unwrap()
        }
        None if path == "index.html" => (
            StatusCode::NOT_FOUND,
            "web client not built: run `just web`",
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn ws(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
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
    let sub = Subscriber {
        client,
        data: data_tx,
        ctrl: ctrl_tx,
    };

    let hello = ServerMsg::Hello {
        version: env!("CARGO_PKG_VERSION").into(),
        client,
        panes: app.panes.iter().map(|p| p.info.clone()).collect(),
    };
    if send(&mut socket, ToClient::Msg(hello)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(msg)) => {
                    if let Err(e) = handle(&app, &sub, msg) {
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
    for pane in &app.panes {
        pane.detach(client);
    }
    info!(client, "client disconnected");
}

fn pane(app: &App, id: u32) -> anyhow::Result<&PaneHandle> {
    app.panes
        .iter()
        .find(|p| p.info.id == id)
        .ok_or_else(|| anyhow::anyhow!("no pane {id}"))
}

fn handle(app: &App, sub: &Subscriber, msg: Message) -> anyhow::Result<()> {
    match msg {
        Message::Text(text) => match serde_json::from_str::<ClientMsg>(&text)? {
            ClientMsg::Attach { panes } => {
                for a in panes {
                    pane(app, a.pane)?.attach(sub.clone(), a.offset);
                }
            }
            ClientMsg::Resize {
                pane: id,
                cols,
                rows,
            } => pane(app, id)?.resize(sub.client, cols, rows),
        },
        Message::Binary(bytes) => {
            let frame = Frame::decode(&bytes)?;
            match frame.kind {
                FrameKind::Input => pane(app, frame.pane)?.input(frame.data),
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
