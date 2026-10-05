//! M6a: a browser block on a port of this host, in the dev scheme
//! (`b-<id>-<key>.localhost` on a loopback listener). Through the block's
//! site the port sees plain local requests with nobody's identity, hot
//! reload's WebSocket passes, other origins are refused both by the site and
//! by the app, and a server that dies and comes back is noticed both ways.

mod listen;
mod strays;

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::ws::{Message as AxMessage, WebSocketUpgrade},
    http::HeaderMap,
    response::Html,
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Daemon {
    child: Option<Child>,
    state: PathBuf,
    port: u16,
    blocks: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        strays::remove(&self.state);
    }
}

impl Daemon {
    fn new() -> Self {
        let state = std::env::temp_dir().join(format!("ilg-sites-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
            .args(["--listen", listen::ANY, "--block-listen", listen::ANY])
            .args(["--shell", "bash --norc --noprofile", "--no-manager-env", "--wisp-token-file", "/nonexistent"])
            .arg("--state-dir")
            .arg(&state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut d = Self { child: Some(child), state, port: 0, blocks: 0 };
        d.port = listen::wait_port(&d.state);
        d.blocks = listen::wait_block_port(&d.state);
        let blocks = d.blocks;
        d.wait_for("daemon", || UnixStream::connect(d.sock()).is_ok());
        d.wait_for("block listener", || std::net::TcpStream::connect(("127.0.0.1", blocks)).is_ok());
        d
    }

    /// `Authorization` with the local token loopback callers show.
    fn bearer(&self) -> String {
        format!("Bearer {}", std::fs::read_to_string(self.state.join("local-token")).unwrap().trim())
    }

    fn sock(&self) -> PathBuf {
        self.state.join("sock")
    }

    fn raw(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String) {
        let mut s = UnixStream::connect(self.sock()).unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        s.write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        while {
            line.clear();
            r.read_line(&mut line).unwrap();
            !line.trim().is_empty()
        } {}
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        (status, out)
    }

    fn get(&self, path: &str) -> Value {
        let (status, body) = self.raw("GET", path, None);
        assert_eq!(status, 200, "{path}: {body}");
        serde_json::from_str(&body).unwrap()
    }

    fn wait_for(&self, what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The event stream, collected as it comes.
    fn events(&self) -> Arc<Mutex<Vec<Value>>> {
        let got = Arc::new(Mutex::new(vec![]));
        let mut s = UnixStream::connect(self.sock()).unwrap();
        write!(s, "GET /api/events?follow=1 HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let g = got.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(s).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(line.trim()) {
                    g.lock().unwrap().push(v);
                }
            }
        });
        got
    }
}

/// A stand-in dev server: a page, an echo of the request's headers, and a
/// WebSocket that reports its own upgrade's headers and then echoes.
async fn dev_server(port: u16) -> tokio::task::JoinHandle<()> {
    fn heads(h: &HeaderMap) -> HashMap<String, String> {
        h.iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned())).collect()
    }
    let app = Router::new()
        .route("/", get(|| async { Html("<html><head><title>Dev app</title></head><body>hello</body></html>") }))
        .route("/headers", get(|h: HeaderMap| async move { Json(heads(&h)) }))
        .route(
            "/hmr",
            get(|h: HeaderMap, ws: WebSocketUpgrade| async move {
                let first = serde_json::to_string(&heads(&h)).unwrap();
                ws.on_upgrade(|mut s| async move {
                    let _ = s.send(AxMessage::Text(first.into())).await;
                    while let Some(Ok(m)) = s.recv().await {
                        if s.send(m).await.is_err() {
                            break;
                        }
                    }
                })
            }),
        );
    let l = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(async move {
        axum::serve(l, app).await.unwrap();
    })
}

/// A client that resolves the block's name to the block listener, as a
/// browser does for `*.localhost`.
fn client(host: &str, listener: u16) -> reqwest::Client {
    let addr: SocketAddr = format!("127.0.0.1:{listener}").parse().unwrap();
    reqwest::Client::builder().resolve(host, addr).redirect(reqwest::redirect::Policy::none()).build().unwrap()
}

async fn websocket(
    listener: u16,
    url: &str,
    origin: &str,
) -> Result<impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + SinkExt<Message> + Unpin, u16>
{
    let mut req = url.into_client_request().unwrap();
    req.headers_mut().insert("origin", origin.parse().unwrap());
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", listener)).await.unwrap();
    match tokio_tungstenite::client_async(req, tcp).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
        Err(e) => panic!("websocket: {e}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_port_through_its_own_site() {
    let d = Daemon::new();
    let dev_port = free_port();
    let server = dev_server(dev_port).await;
    let events = d.events();

    // Opening the daemon's own ports is refused; a dev server's isn't.
    let (status, body) = d.raw("POST", "/api/blocks", Some(json!({"type": "browser", "config": {"port": d.port}})));
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("illogical's own"), "{body}");
    let (status, body) = d.raw("POST", "/api/blocks", Some(json!({"type": "browser", "config": {"port": d.blocks}})));
    assert_eq!(status, 400, "{body}");
    let (status, body) =
        d.raw("POST", "/api/blocks", Some(json!({"type": "browser", "config": {"port": dev_port, "path": "/"}})));
    assert_eq!(status, 200, "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["block"].as_u64().unwrap();
    d.wait_for("the title", || d.get(&format!("/api/blocks/{id}"))["state"]["title"] == "Dev app");
    let state = d.get(&format!("/api/blocks/{id}"))["state"].clone();
    assert_eq!(state["port"], dev_port);
    assert_eq!(state["machine"], Value::Null);
    let url = state["url"].as_str().unwrap().to_owned();
    let origin = url.trim_end_matches('/').to_owned();
    let host = origin.trim_start_matches("http://").split(':').next().unwrap().to_owned();
    assert!(host.starts_with(&format!("b-{id}-")) && host.ends_with(".localhost"), "{host}");
    assert_eq!(origin, format!("http://{host}:{}", d.blocks));
    // Its config keeps the name's key, so the origin (and its storage)
    // survives restarts.
    let (_, saved) = d.raw("GET", "/api/panes", None);
    assert!(saved.contains("browser"), "{saved}");

    let http = client(&host, d.blocks);
    let page = http.get(&url).send().await.unwrap();
    assert_eq!(page.status(), 200);
    let csp: Vec<String> =
        page.headers().get_all("content-security-policy").iter().map(|v| v.to_str().unwrap().to_owned()).collect();
    assert!(
        csp.iter().any(|c| c.starts_with("frame-ancestors ") && c.contains(&format!("http://127.0.0.1:{}", d.port))),
        "only the app may frame it: {csp:?}"
    );
    assert!(page.text().await.unwrap().contains("hello"));

    // What the port sees: a local request, with nobody's identity.
    let seen: HashMap<String, String> = http
        .get(format!("{origin}/headers"))
        .header("origin", &origin)
        .header("referer", format!("{origin}/somewhere"))
        .header("tailscale-user-login", "me@example.com")
        .header("tailscale-user-name", "Me")
        .header("x-forwarded-for", "100.64.0.1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(seen["host"], format!("localhost:{dev_port}"));
    assert_eq!(seen["origin"], format!("http://localhost:{dev_port}"));
    assert_eq!(seen["referer"], format!("http://localhost:{dev_port}/somewhere"));
    for k in seen.keys() {
        assert!(!k.starts_with("tailscale-") && !k.starts_with("x-forwarded-"), "{k} reached the port");
    }

    // Hot reload's socket passes, from the block's own origin.
    let ws_url = format!("ws://{host}:{}/hmr", d.blocks);
    let mut ws = websocket(d.blocks, &ws_url, &origin).await.unwrap_or_else(|s| panic!("hmr socket: {s}"));
    let Some(Ok(Message::Text(first))) = ws.next().await else { panic!("no hello") };
    let upgrade: HashMap<String, String> = serde_json::from_str(&first).unwrap();
    assert_eq!(upgrade["host"], format!("localhost:{dev_port}"));
    assert_eq!(upgrade["origin"], format!("http://localhost:{dev_port}"));
    let _ = ws.send(Message::Text("update".into())).await;
    let Some(Ok(Message::Text(echo))) = ws.next().await else { panic!("no echo") };
    assert_eq!(echo.as_str(), "update");

    // Anyone else is refused: other origins (exactly: scheme and port
    // count), the app's own origin, cross-site fetches, a wrong key.
    let app_origin = format!("http://127.0.0.1:{}", d.port);
    for bad in [
        "https://evil.example",
        "null",
        &app_origin,
        &format!("https://{host}:{}", d.blocks),
        &format!("http://{host}:{}", d.blocks + 1),
    ] {
        assert_eq!(websocket(d.blocks, &ws_url, bad).await.err(), Some(403), "websocket from {bad}");
        let r = http.post(format!("{origin}/headers")).header("origin", bad).send().await.unwrap();
        assert_eq!(r.status(), 403, "POST from {bad}");
    }
    let r =
        http.get(&url).header("sec-fetch-site", "cross-site").header("sec-fetch-mode", "cors").send().await.unwrap();
    assert_eq!(r.status(), 403, "cross-site fetch");
    let r =
        http.get(&url).header("sec-fetch-site", "same-site").header("sec-fetch-mode", "no-cors").send().await.unwrap();
    assert_eq!(r.status(), 403, "same-site fetch (another block)");
    let nav = http.get(&url).header("sec-fetch-site", "cross-site").header("sec-fetch-mode", "navigate");
    assert_eq!(nav.send().await.unwrap().status(), 200, "a page navigation (the app's frame) is fine");
    let wrong = format!("b-{id}-wrongkeywrongkeywrong.localhost");
    let r = client(&wrong, d.blocks).get(format!("http://{wrong}:{}/", d.blocks)).send().await.unwrap();
    assert_eq!(r.status(), 404, "wrong key");
    let r = reqwest::get(format!("http://127.0.0.1:{}/", d.blocks)).await.unwrap();
    assert_eq!(r.status(), 404, "no name at all");

    // And the app refuses the block's origin: its socket and its API.
    let app_ws = format!("ws://127.0.0.1:{}/ws", d.port);
    let mut req = app_ws.as_str().into_client_request().unwrap();
    req.headers_mut().insert("origin", origin.parse().unwrap());
    req.headers_mut().insert("authorization", d.bearer().parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => assert_eq!(r.status(), 403),
        other => panic!("the app's socket took the block's origin: {:?}", other.map(|_| ())),
    }
    let r = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{}/api/run", d.port))
        .header("authorization", d.bearer())
        .header("origin", &origin)
        .json(&json!({"command": "touch /tmp/ilg-should-not-exist"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "the app's API took the block's origin");

    // The frame navigating is reported, with the new page's title.
    let r = http
        .get(format!("{origin}/headers?x=1"))
        .header("sec-fetch-mode", "navigate")
        .header("sec-fetch-dest", "iframe");
    assert_eq!(r.send().await.unwrap().status(), 200);
    d.wait_for("navigated", || d.get(&format!("/api/blocks/{id}"))["state"]["path"] == "/headers?x=1");

    // The server dies: the block asks for you, and says why.
    server.abort();
    let _ = server.await;
    // (A new connection: an old one may still reach the dying server.)
    let r = client(&host, d.blocks).get(&url).send().await.unwrap();
    assert_eq!(r.status(), 502);
    d.wait_for("needs input", || d.get(&format!("/api/blocks/{id}"))["info"]["attention"] == "needs_input");
    assert!(d.get(&format!("/api/blocks/{id}"))["state"]["error"].as_str().unwrap().contains("nothing is answering"));
    // ...and lets go when it's back.
    let _server = dev_server(dev_port).await;
    d.wait_for("back", || d.get(&format!("/api/blocks/{id}"))["info"]["attention"] == "idle");
    assert_eq!(d.get(&format!("/api/blocks/{id}"))["state"]["error"], Value::Null);

    let kinds = |t: &str| {
        events.lock().unwrap().iter().filter(|e| e["pane"] == id && e["type"] == t).cloned().collect::<Vec<_>>()
    };
    let nav = kinds("navigated");
    assert!(nav.iter().any(|e| e["title"] == "Dev app" && e["url"] == url), "{nav:?}");
    assert!(nav.iter().any(|e| e["url"] == format!("{origin}/headers?x=1")), "{nav:?}");
    let errors = kinds("load_error");
    assert_eq!(errors.len(), 1, "{errors:?}");

    // Closed: its name stops working.
    assert_eq!(d.raw("POST", &format!("/api/panes/{id}/close"), Some(json!({}))).0, 200);
    d.wait_for("site gone", || {
        std::process::Command::new("curl")
            .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "--resolve"])
            .arg(format!("{host}:{}:127.0.0.1", d.blocks))
            .arg(&url)
            .output()
            .map(|o| o.stdout == b"404")
            .unwrap_or(false)
    });
}
