//! Loopback callers show the local token: programs as a bearer, browsers as
//! the cookie a sign-in link sets. The Unix socket needs none.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
};

use illogical_testkit::{Daemon, illogicald};
use serde_json::Value;
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest};

fn start() -> Daemon {
    illogicald!("lauth").no_wisp().no_tailscale().env_remove("ILLOGICAL_LOCAL_TOKEN_FILE").start()
}

/// One request over TCP with these headers: the response's head
/// (lowercase) and body.
fn tcp(d: &Daemon, method: &str, path: &str, headers: &[(&str, &str)]) -> (u16, String, String) {
    d.tcp(method, path, headers, None)
}

/// The same over the Unix socket, with no credential.
fn socket(d: &Daemon, path: &str) -> (u16, String, String) {
    let mut s = UnixStream::connect(d.sock()).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    response(s)
}

fn response(mut s: impl Read) -> (u16, String, String) {
    let mut res = Vec::new();
    s.read_to_end(&mut res).unwrap();
    let res = String::from_utf8_lossy(&res).into_owned();
    let (head, body) = res.split_once("\r\n\r\n").unwrap_or((&res, ""));
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, head.to_ascii_lowercase(), body.to_owned())
}

#[test]
fn loopback_programs_and_browsers_show_the_local_token() {
    let d = start();
    let token = d.token();
    assert!(token.starts_with("ilt_"), "{token}");
    let mode = std::fs::metadata(d.state.join("local-token")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "the token file is the owner's alone");

    // Loopback alone is not enough.
    assert_eq!(tcp(&d, "GET", "/api/panes", &[]).0, 401);
    let (status, _, page) = tcp(&d, "GET", "/", &[("accept", "text/html")]);
    assert_eq!(status, 401);
    assert!(page.contains("illogical web"), "the page says how to sign in: {page}");
    // A program: the bearer.
    let bearer = format!("Bearer {token}");
    assert_eq!(tcp(&d, "GET", "/api/panes", &[("authorization", &bearer)]).0, 200);
    assert_eq!(tcp(&d, "GET", "/api/panes", &[("authorization", "Bearer ilt_wrong")]).0, 401);
    // A browser: the cookie.
    let cookie = format!("illogical_{}={token}", d.port);
    assert_eq!(tcp(&d, "GET", "/api/panes", &[("cookie", &cookie), ("sec-fetch-site", "same-origin")]).0, 200);
    // The socket is the owner's already.
    assert_eq!(socket(&d, "/api/panes").0, 200);
}

#[test]
fn a_signin_link_sets_the_cookie_and_goes_to_the_page() {
    let d = start();
    let token = d.token();
    assert_eq!(tcp(&d, "GET", "/auth?token=ilt_wrong", &[]).0, 401);
    let (status, head, _) = tcp(&d, "GET", &format!("/auth?token={token}&next=/x"), &[]);
    assert_eq!(status, 303, "{head}");
    assert!(head.contains("\r\nlocation: /x\r\n"), "{head}");
    let set =
        format!("set-cookie: illogical_{}={}; path=/; httponly; samesite=lax", d.port, token.to_ascii_lowercase());
    assert!(head.contains(&set), "{head}");
    // Only a path of ours.
    for next in ["//evil.example/", "https://evil.example/", "/%5Cevil.example"] {
        let (status, head, _) = tcp(&d, "GET", &format!("/auth?token={token}&next={next}"), &[]);
        assert_eq!(status, 303);
        assert!(head.contains("\r\nlocation: /\r\n"), "{next}: {head}");
    }

    // `illogical web`'s link: over the socket only.
    let (status, _, body) = socket(&d, "/api/signin-link");
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["url"], format!("http://127.0.0.1:{}/auth?token={token}", d.port));
    let bearer = format!("Bearer {token}");
    assert_eq!(tcp(&d, "GET", "/api/signin-link", &[("authorization", &bearer)]).0, 404);
}

#[tokio::test]
async fn the_socket_and_mcp_over_loopback_need_it_too() {
    let d = start();
    let token = d.token();
    let ws = |auth: Option<String>| {
        let mut req = format!("ws://127.0.0.1:{}/ws", d.port).into_client_request().unwrap();
        if let Some(a) = auth {
            req.headers_mut().insert("authorization", a.parse().unwrap());
        }
        connect_async(req)
    };
    match ws(None).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 401),
        other => panic!("a WebSocket without the token: {:?}", other.map(|_| ())),
    }
    assert!(ws(Some(format!("Bearer {token}"))).await.is_ok());

    // MCP: refused without it; the local token works as its bearer.
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}"#;
    let mcp = |auth: Option<String>| {
        let mut r = reqwest::Client::new()
            .post(format!("http://127.0.0.1:{}/mcp", d.port))
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .body(body);
        if let Some(a) = auth {
            r = r.header("Authorization", a);
        }
        r.send()
    };
    assert_eq!(mcp(None).await.unwrap().status(), 401);
    assert!([401, 403].contains(&mcp(Some("Bearer ilt_wrong".into())).await.unwrap().status().as_u16()));
    let ok = mcp(Some(format!("Bearer {token}"))).await.unwrap().status().as_u16();
    assert!(ok < 300, "the local token as MCP's bearer: {ok}");
}
