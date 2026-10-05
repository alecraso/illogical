//! End-to-end channels from client devices (M17, M18): the responder side
//! of `illogical_e2e::channel`, over a WebSocket at `/e2e` (the direct
//! path) or a stream from control's relay.
//!
//! The client's Noise key must belong to a device this daemon trusts (its
//! certificate chains back to the account root pinned at join; see
//! `control.rs`). Then the channel is what `/ws` and the API are for a
//! tailnet client: the protocol's messages go to the mux as a client of
//! their own, and requests are answered by the daemon's own API router.
//! If the device stops being trusted (revoked), its channels close.

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, HttpBody},
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{Request, header},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use illogical_e2e::channel::{Channel, MAX_MSG, MAX_WIRE, Msg, RequestHead, Responder, ResponseHead, prologue};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    sync::mpsc,
};
use tower::ServiceExt;
use tracing::{debug, info, warn};

use crate::{
    mux::Cmd,
    pane::{Subscriber, ToClient, client_queue},
    server::App,
};

pub const PATH: &str = "/e2e";

/// Wire messages queued for the socket, per channel.
const OUT_QUEUE: usize = 256;

/// `/e2e`: the direct path. The handshake is the authentication, so this
/// needs no tailnet identity or origin.
pub async fn ws(State(app): State<Arc<App>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.max_message_size(MAX_WIRE).on_upgrade(move |socket| serve_ws(app, socket))
}

async fn serve_ws(app: Arc<App>, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(OUT_QUEUE);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(OUT_QUEUE);
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
    let writer = async move {
        while let Some(w) = out_rx.recv().await {
            if tx.send(Message::Binary(w.into())).await.is_err() {
                break;
            }
        }
        let _ = tx.close().await;
    };
    let w = tokio::spawn(writer);
    tokio::select! {
        r = serve(app, in_rx, out_tx) => log_end(r),
        _ = reader => {}
    }
    // Whatever was queued still goes out.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), w).await;
}

/// A stream from control's relay: `len (u32 BE) ‖ Noise message` frames.
pub async fn serve_stream(app: Arc<App>, stream: DuplexStream) {
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(OUT_QUEUE);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(OUT_QUEUE);
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
    let writer = async move {
        while let Some(w) = out_rx.recv().await {
            let mut f = Vec::with_capacity(4 + w.len());
            f.extend_from_slice(&(w.len() as u32).to_be_bytes());
            f.extend_from_slice(&w);
            if wr.write_all(&f).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    };
    let w = tokio::spawn(writer);
    tokio::select! {
        r = serve(app, in_rx, out_tx) => log_end(r),
        _ = reader => {}
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), w).await;
}

fn log_end(r: anyhow::Result<()>) {
    if let Err(e) = r {
        debug!(error = %e, "channel ended");
    }
}

/// Seals and queues messages, one whole message at a time: the nonces
/// (and a message's chunks) must reach the socket in order.
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
}

async fn serve(app: Arc<App>, mut inbound: mpsc::Receiver<Vec<u8>>, out: mpsc::Sender<Vec<u8>>) -> anyhow::Result<()> {
    let m1 = tokio::time::timeout(std::time::Duration::from_secs(15), inbound.recv())
        .await
        .map_err(|_| anyhow::anyhow!("no handshake"))?
        .ok_or_else(|| anyhow::anyhow!("closed before the handshake"))?;
    let enrolled = app.control.enrolled().ok_or_else(|| anyhow::anyhow!("not enrolled in control"))?;
    let (responder, who) = Responder::read(&enrolled.keys, &prologue(&enrolled.keys.id()), &m1)?;
    let Some((device, principal)) = app.control.device(&who) else {
        warn!(key = hex::encode(&who[..8]), "refused a channel from a device this daemon doesn't trust");
        anyhow::bail!("unknown device");
    };
    let (m2, ch) = responder.finish(&[])?;
    out.send(m2).await?;
    let out = Arc::new(Out { ch: Arc::new(ch), q: tokio::sync::Mutex::new(out) });
    let ch = out.ch.clone();
    info!(device = device.device, name = device.name, who = principal.id(), "channel open");

    // An owner here through control has a name of their own (M30).
    let name = principal.is_owner().then(|| app.control.name_of_account(&device.account)).flatten();
    let client = app.new_client_id();
    let (data_tx, mut data_rx) = client_queue();
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    app.hands.connect(
        client,
        ctrl_tx.clone(),
        principal.is_owner(),
        Some((device.device.clone(), device.name.clone())),
    );
    app.mux.send(Cmd::Connect {
        sub: Subscriber {
            client,
            data: data_tx,
            ctrl: ctrl_tx,
            principal: principal.clone(),
            name,
            // A read-only link's stand-in has no signing key.
            device: Some(device.clone()).filter(|d| !d.sign.is_empty()),
        },
    });
    let router = crate::server::channel_router(app.clone());
    let mut changed = app.control.changed.subscribe();
    let result = loop {
        tokio::select! {
            w = inbound.recv() => {
                let Some(w) = w else { break Ok(()) };
                match ch.open(&w) {
                    Ok(None) => {}
                    Ok(Some(Msg::Text(t))) => {
                        if let Err(e) = crate::server::handle(&app, client, Message::Text(t.into())) {
                            debug!(client, error = %e, "bad client message");
                        }
                    }
                    Ok(Some(Msg::Binary(b))) => {
                        if let Err(e) = crate::server::handle(&app, client, Message::Binary(b.into())) {
                            debug!(client, error = %e, "bad client message");
                        }
                    }
                    Ok(Some(Msg::Request { id, head, body })) => {
                        tokio::spawn(answer(router.clone(), out.clone(), id, head, body, principal.clone()));
                    }
                    Ok(Some(Msg::Response { .. })) => break Err(anyhow::anyhow!("a client doesn't answer requests")),
                    Err(e) => break Err(e),
                }
            }
            Some(o) = ctrl_rx.recv() => {
                let Some(m) = to_msg(o) else { break Ok(()) };
                if let Err(e) = out.put(&m).await { break Err(e) }
            }
            Some(o) = data_rx.recv() => {
                let Some(m) = to_msg(o) else { break Ok(()) };
                if let Err(e) = out.put(&m).await { break Err(e) }
            }
            Ok(()) = changed.changed() => {
                if app.control.device(&who).is_none() {
                    info!(device = device.device, "device no longer trusted: closing its channel");
                    // Say why, as the mux does when a grant goes (M30): the
                    // page drops what it showed instead of keeping it greyed.
                    let message = "your access was removed".to_owned();
                    let m = Msg::Text(serde_json::to_string(&illogical_proto::ServerMsg::Error { id: None, message }).expect("serialize"));
                    let _ = out.put(&m).await;
                    break Ok(());
                }
            }
        }
    };
    app.mux.send(Cmd::Disconnect { client });
    app.hands.disconnect(client);
    info!(device = device.device, "channel closed");
    result
}

/// `None`: hang up.
fn to_msg(o: ToClient) -> Option<Msg> {
    match o {
        ToClient::Frame(bytes) => Some(Msg::Binary(bytes)),
        ToClient::Msg(m) => Some(Msg::Text(serde_json::to_string(&m).expect("serialize"))),
        ToClient::Json(t) => Some(Msg::Text(t)),
        ToClient::Close => None,
    }
}

async fn answer(router: Router, out: Arc<Out>, id: u32, head: RequestHead, body: Vec<u8>, who: crate::acl::Principal) {
    let say = |status, content_type, more, body| Msg::Response {
        id,
        head: ResponseHead { status, content_type, more },
        body,
    };
    let stream = head.stream;
    let (status, content_type, body) = match call(router, head, body, who).await {
        Ok(r) => r,
        Err(e) => {
            let body = serde_json::to_vec(&serde_json::json!({ "error": e.to_string() })).unwrap();
            let _ = out.put(&say(400, Some("application/json".to_owned()), false, body)).await;
            return;
        }
    };
    // A body of no fixed length (a follow) goes in parts when the client
    // can take them; otherwise whole, as before.
    if !stream || body.size_hint().exact().is_some() {
        let body = match axum::body::to_bytes(body, MAX_MSG - 1024).await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                let body = serde_json::to_vec(&serde_json::json!({ "error": e.to_string() })).unwrap();
                let _ = out.put(&say(400, Some("application/json".to_owned()), false, body)).await;
                return;
            }
        };
        let _ = out.put(&say(status, content_type, false, body)).await;
        return;
    }
    if out.put(&say(status, content_type.clone(), true, Vec::new())).await.is_err() {
        return;
    }
    let mut parts = body.into_data_stream();
    while let Some(Ok(b)) = parts.next().await {
        // The channel went: nobody to tell.
        if out.put(&say(status, content_type.clone(), true, b.to_vec())).await.is_err() {
            return;
        }
    }
    let _ = out.put(&say(status, content_type, false, Vec::new())).await;
}

async fn call(
    router: Router,
    head: RequestHead,
    body: Vec<u8>,
    who: crate::acl::Principal,
) -> anyhow::Result<(u16, Option<String>, Body)> {
    anyhow::ensure!(head.path.starts_with("/api/"), "only the API is reachable this way");
    let mut req = Request::builder().method(head.method.as_str()).uri(head.path.as_str());
    if let Some(ct) = &head.content_type {
        req = req.header(header::CONTENT_TYPE, ct);
    }
    let mut req = req.body(Body::from(body))?;
    // Who's asking: the API's checks (authz.rs) go by it.
    req.extensions_mut().insert(who);
    let res = router.oneshot(req).await?;
    let status = res.status().as_u16();
    let ct = res.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_owned);
    Ok((status, ct, res.into_body()))
}
