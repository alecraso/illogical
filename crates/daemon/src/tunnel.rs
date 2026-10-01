//! The provider tunnel (M4b): how a client reaches a resident daemon in a
//! sandbox that has no tailnet route, through the home daemon.
//!
//! `/tunnel/<host>/<path>` on the home daemon is `<path>` on that host's
//! daemon. Each request (a WebSocket upgrade included) gets a connection of
//! its own through the provider (for sprites, the Sprites proxy to the
//! daemon's port), which also wakes the sandbox: so connecting is the wake,
//! and an open WebSocket keeps it awake until the client lets go.
//!
//! **A transport, not a relay of anything else.** The bytes are passed
//! through untouched; the layout, terminals and scrollback are the host
//! daemon's own, and nothing here holds state about them.
//!
//! **Identity, end to end.** The caller has passed this daemon's checks:
//! the owner's tailnet login, or the Unix socket, and for a browser one of
//! our own origins. The request then goes on with the token this daemon
//! minted for that host when it made it resident, which that daemon
//! requires of everything it doesn't learn about from tailscaled
//! (`access.rs`). What the caller sent that says who it is, or where it
//! came from, is dropped: cookies, `Authorization`, `Origin`,
//! `Tailscale-*`, `X-Forwarded-*`.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::any,
};
use hyper_util::rt::TokioIo;
use tracing::{debug, info, warn};

use crate::{provider::Conn, server::App};

/// A sandbox that is booting cold (wisp: a real boot, then the daemon
/// starting as a service) may refuse its port for a moment.
const PATIENCE: Duration = Duration::from_secs(45);

pub fn routes() -> axum::Router<Arc<App>> {
    axum::Router::new().route("/tunnel/{host}/{*rest}", any(tunnel))
}

fn text<'h>(h: &'h HeaderMap, name: &str) -> Option<&'h str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn is_upgrade(h: &HeaderMap) -> bool {
    text(h, header::CONNECTION.as_str()).is_some_and(|c| c.to_ascii_lowercase().contains("upgrade"))
        && h.contains_key(header::UPGRADE)
}

/// Hop-by-hop headers: for one connection, never forwarded.
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
];

/// The request as the host's daemon sees it: from its own loopback, with
/// our token and nothing of the caller's identity.
fn rewrite_request(h: &mut HeaderMap, port: u16, token: &str) {
    let upgrade = is_upgrade(h);
    for name in HOP {
        if !(upgrade && (*name == "connection" || *name == "upgrade")) {
            h.remove(*name);
        }
    }
    let identity: Vec<HeaderName> = h
        .keys()
        .filter(|k| {
            let k = k.as_str();
            k.starts_with("tailscale-")
                || k.starts_with("x-forwarded-")
                || matches!(k, "forwarded" | "x-real-ip" | "cookie" | "authorization" | "origin" | "referer")
        })
        .cloned()
        .collect();
    for k in identity {
        h.remove(k);
    }
    if upgrade {
        h.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    }
    h.insert(header::HOST, HeaderValue::from_str(&format!("127.0.0.1:{port}")).expect("host"));
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {token}")) {
        h.insert(header::AUTHORIZATION, v);
    }
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, axum::Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// A connection to the host's daemon, waiting out a cold boot.
async fn dial(app: &App, host: &str) -> Result<(Conn, u16, String), Response> {
    let Some((at, token)) = app.hosts.tunnel(host) else {
        return Err(error(StatusCode::NOT_FOUND, format!("no provider host {host}")));
    };
    let Some(provider) = app.mux.provider.clone().filter(|p| p.name() == at.provider) else {
        return Err(error(StatusCode::SERVICE_UNAVAILABLE, format!("provider {} isn't set up here", at.provider)));
    };
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut woken = false;
    loop {
        match provider.dial(&at.sandbox, at.port).await {
            Ok(c) => return Ok((c, at.port, token)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                app.hosts.note_status(host, "gone");
                return Err(error(StatusCode::BAD_GATEWAY, format!("{host}'s sandbox is gone")));
            }
            Err(e) if tokio::time::Instant::now() >= deadline => {
                return Err(error(StatusCode::BAD_GATEWAY, format!("{host} isn't answering: {e}")));
            }
            Err(e) => {
                debug!(host, error = %e, "tunnel: not answering yet");
                // Dialing wakes a sprite; a provider whose proxy doesn't
                // gets asked to.
                if !woken {
                    woken = true;
                    if let Err(e) = provider.wake(&at.sandbox).await {
                        debug!(host, error = %e, "tunnel: wake");
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

async fn tunnel(State(app): State<Arc<App>>, Path((host, rest)): Path<(String, String)>, mut req: Request) -> Response {
    let (conn, port, token) = match dial(&app, &host).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    // Reached: it's running now, whatever the provider said last.
    app.hosts.note_status(&host, "running");
    let upgrade = is_upgrade(req.headers());
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
    let (mut parts, body) = req.into_parts();
    rewrite_request(&mut parts.headers, port, &token);
    let query = parts.uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    parts.uri = match format!("/{rest}{query}").parse::<Uri>() {
        Ok(u) => u,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("bad path: {e}")),
    };
    let out = axum::http::Request::from_parts(parts, body);
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(conn)).await {
        Ok(x) => x,
        Err(e) => return error(StatusCode::BAD_GATEWAY, format!("{host}: {e}")),
    };
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });
    let mut res = match sender.send_request(out).await {
        Ok(r) => r,
        Err(e) => return error(StatusCode::BAD_GATEWAY, format!("{host}: {e}")),
    };
    let upgraded = res.status() == StatusCode::SWITCHING_PROTOCOLS;
    for name in HOP {
        if !(upgraded && (*name == "connection" || *name == "upgrade")) {
            res.headers_mut().remove(*name);
        }
    }
    if upgraded && let Some(client) = client_upgrade {
        let server = hyper::upgrade::on(&mut res);
        let host = host.clone();
        tokio::spawn(async move {
            let (client, server) = tokio::join!(client, server);
            let (client, server) = match (client, server) {
                (Ok(c), Ok(s)) => (c, s),
                (c, s) => {
                    return warn!(host, client = ?c.err(), server = ?s.err(), "tunnel: upgrade failed");
                }
            };
            info!(host, "tunnel open");
            let (mut a, mut b) = (TokioIo::new(client), TokioIo::new(server));
            let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
            info!(host, "tunnel closed");
        });
    }
    res.map(Body::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn requests_carry_our_token_and_nothing_of_the_callers() {
        let mut h = headers(&[
            ("host", "geek.example.ts.net"),
            ("origin", "https://geek.example.ts.net"),
            ("referer", "https://geek.example.ts.net/"),
            ("cookie", "a=b"),
            ("authorization", "Bearer theirs"),
            ("tailscale-user-login", "me@x.com"),
            ("x-forwarded-for", "100.1.2.3"),
            ("content-type", "application/json"),
            ("keep-alive", "timeout=5"),
        ]);
        rewrite_request(&mut h, 7681, "ilt_ours");
        assert_eq!(h["host"], "127.0.0.1:7681");
        assert_eq!(h["authorization"], "Bearer ilt_ours");
        assert_eq!(h["content-type"], "application/json");
        for gone in ["origin", "referer", "cookie", "tailscale-user-login", "x-forwarded-for", "keep-alive"] {
            assert!(!h.contains_key(gone), "{gone} should be dropped");
        }
        // WebSocket upgrades keep what makes them upgrades.
        let mut h = headers(&[
            ("connection", "keep-alive, Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-key", "k"),
            ("origin", "https://geek.example.ts.net"),
        ]);
        rewrite_request(&mut h, 7681, "t");
        assert_eq!(h["connection"], "upgrade");
        assert_eq!(h["upgrade"], "websocket");
        assert_eq!(h["sec-websocket-key"], "k");
        assert!(!h.contains_key("origin"));
    }
}
