//! The relay, as control would run it: daemons dial in and keep one mux
//! socket open; clients connect by daemon name and are spliced onto a new
//! stream. It forwards opaque Noise messages and counts bytes.
//!
//! For the phone tests it also serves the test pages, a plain WebSocket
//! echo (to time the phone-to-relay leg alone), `/info` (connected daemons
//! and the public keys they announced), and `/report`, which logs what a
//! test page posts so results show up in `fly logs`. `/report` also logs
//! every request's User-Agent, for the link-preview check.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
    serve::ListenerExt,
};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncWriteExt, DuplexStream};

use crate::{mux, read_frame, write_frame};

#[derive(Default)]
struct Relay {
    daemons: Mutex<HashMap<String, (mux::Mux, String)>>,
    bytes: AtomicU64,
    streams: AtomicU64,
}

type S = State<Arc<Relay>>;

pub async fn run(listen: String) -> anyhow::Result<()> {
    let state = Arc::new(Relay::default());
    let s2 = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            println!(
                "relay: {} daemons, {} streams open, {} bytes relayed",
                s2.daemons.lock().unwrap().len(),
                s2.streams.load(Ordering::Relaxed),
                s2.bytes.load(Ordering::Relaxed)
            );
        }
    });
    let app = Router::new()
        .route("/", get(|| async { page("keys.html") }))
        .route("/keys.html", get(|| async { page("keys.html") }))
        .route("/rtt.html", get(|| async { page("rtt.html") }))
        .route("/share.html", get(share))
        .route("/dist/noise.js", get(|| async { page("noise.js") }))
        .route("/info", get(info))
        .route("/report", post(report))
        .route("/echo", get(|u: WebSocketUpgrade| async { u.on_upgrade(echo) }))
        .route("/dial/{name}", get(dial))
        .route("/c/{name}", get(client))
        .with_state(state);
    // Nagle off: see the daemon's main.rs (S15's first finding).
    let l = tokio::net::TcpListener::bind(&listen).await?.tap_io(|t| {
        let _ = t.set_nodelay(true);
    });
    println!("relay on {listen}");
    axum::serve(l, app).await?;
    Ok(())
}

fn page(name: &str) -> Response {
    let (body, ty) = match name {
        "keys.html" => (include_str!("../web/keys.html"), "text/html; charset=utf-8"),
        "rtt.html" => (include_str!("../web/rtt.html"), "text/html; charset=utf-8"),
        "share.html" => (include_str!("../web/share.html"), "text/html; charset=utf-8"),
        "noise.js" => (include_str!("../web/dist/noise.js"), "text/javascript"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, ty), (header::CACHE_CONTROL, "no-store")], body).into_response()
}

async fn share(headers: HeaderMap) -> Response {
    // What the server sees of a share link: never the fragment.
    println!("share.html fetched by {:?}", headers.get(header::USER_AGENT));
    page("share.html")
}

async fn info(State(s): S) -> Json<serde_json::Value> {
    let d: HashMap<String, String> =
        s.daemons.lock().unwrap().iter().map(|(k, (_, p))| (k.clone(), p.clone())).collect();
    Json(serde_json::json!({ "daemons": d }))
}

async fn report(headers: HeaderMap, body: Bytes) -> StatusCode {
    println!("REPORT ua={:?} {}", headers.get(header::USER_AGENT), String::from_utf8_lossy(&body));
    StatusCode::NO_CONTENT
}

async fn echo(mut ws: WebSocket) {
    while let Some(Ok(m)) = ws.next().await {
        if matches!(m, Message::Close(_)) || ws.send(m).await.is_err() {
            break;
        }
    }
}

async fn dial(
    State(s): S,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    u: WebSocketUpgrade,
) -> Response {
    // The real control authenticates the daemon by its enrolled key here.
    let pub_key = q.get("pub").cloned().unwrap_or_default();
    u.on_upgrade(move |ws| async move {
        let (m, mut out) = mux::Mux::new(None);
        s.daemons.lock().unwrap().insert(name.clone(), (m.clone(), pub_key));
        println!("daemon {name} connected");
        let (mut tx, mut rx) = ws.split();
        let writer = tokio::spawn(async move {
            while let Some(f) = out.recv().await {
                if tx.send(Message::Binary(f.into())).await.is_err() {
                    break;
                }
            }
        });
        while let Some(Ok(msg)) = rx.next().await {
            if let Message::Binary(b) = msg
                && let Err(e) = m.handle(&b)
            {
                println!("daemon {name}: {e}");
                break;
            }
        }
        m.close();
        writer.abort();
        s.daemons.lock().unwrap().remove(&name);
        println!("daemon {name} gone");
    })
}

async fn client(State(s): S, Path(name): Path<String>, u: WebSocketUpgrade) -> Response {
    let Some(m) = s.daemons.lock().unwrap().get(&name).map(|(m, _)| m.clone()) else {
        return (StatusCode::NOT_FOUND, "no such daemon").into_response();
    };
    let Ok(stream) = m.open() else {
        return (StatusCode::BAD_GATEWAY, "daemon tunnel closed").into_response();
    };
    u.on_upgrade(move |ws| async move {
        s.streams.fetch_add(1, Ordering::Relaxed);
        let (up, down) = splice(ws, stream).await;
        s.streams.fetch_sub(1, Ordering::Relaxed);
        s.bytes.fetch_add(up + down, Ordering::Relaxed);
    })
}

async fn splice(ws: WebSocket, stream: DuplexStream) -> (u64, u64) {
    let (mut wtx, mut wrx) = ws.split();
    let (mut rd, mut wr) = tokio::io::split(stream);
    let up = async {
        let mut n = 0u64;
        while let Some(Ok(m)) = wrx.next().await {
            match m {
                Message::Binary(b) => {
                    n += b.len() as u64;
                    if write_frame(&mut wr, &b).await.is_err() {
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
    let down = async {
        let mut n = 0u64;
        while let Ok(Some(b)) = read_frame(&mut rd).await {
            n += b.len() as u64;
            if wtx.send(Message::Binary(b.into())).await.is_err() {
                break;
            }
        }
        let _ = wtx.close().await;
        n
    };
    tokio::join!(up, down)
}
