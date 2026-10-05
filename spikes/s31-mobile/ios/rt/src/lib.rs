//! S31: daemon-shaped Rust inside an iOS app process. A tokio runtime, an axum
//! server on loopback (HTTP and a websocket), outbound HTTPS through reqwest
//! and outbound wss through tokio-tungstenite, both verified by the platform
//! verifier (Security.framework on iOS), and Ghostty's lib-vt fed some output.

use std::ffi::{CString, c_char};
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::ws::{Message as AxMsg, WebSocketUpgrade};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use illogical_vt::{GhosttyEngine, VtEngine};

fn report(lines: &mut Vec<String>, name: &str, r: Result<String, String>, t: Instant) {
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    match r {
        Ok(s) => lines.push(format!("ok   {name} ({ms:.1} ms): {s}")),
        Err(e) => lines.push(format!("FAIL {name} ({ms:.1} ms): {e}")),
    }
}

async fn selftest() -> Vec<String> {
    let mut out = Vec::new();

    // 1. axum on loopback, HTTP and websocket.
    let t = Instant::now();
    let app = Router::new().route("/", get(|| async { "illogical-ish" })).route(
        "/ws",
        get(|ws: WebSocketUpgrade| async {
            ws.on_upgrade(|mut s| async move {
                while let Some(Ok(m)) = s.recv().await {
                    if let AxMsg::Text(t) = m {
                        let _ = s.send(AxMsg::Text(format!("echo:{t}").into())).await;
                    }
                }
            })
        }),
    );
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => {
            out.push(format!("FAIL bind loopback: {e}"));
            return out;
        }
    };
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await });
    report(&mut out, "axum bind 127.0.0.1", Ok(format!("port {port}")), t);

    let client = reqwest::Client::builder().build();
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            out.push(format!("FAIL reqwest client: {e}"));
            return out;
        }
    };
    let t = Instant::now();
    let r = async { client.get(format!("http://127.0.0.1:{port}/")).send().await?.text().await }.await;
    report(&mut out, "GET loopback", r.map_err(|e| e.to_string()), t);

    let t = Instant::now();
    let r = async {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws")).await.map_err(|e| e.to_string())?;
        ws.send(tokio_tungstenite::tungstenite::Message::Text("hi".into())).await.map_err(|e| e.to_string())?;
        match ws.next().await {
            Some(Ok(m)) => Ok(m.to_string()),
            other => Err(format!("{other:?}")),
        }
    }
    .await;
    report(&mut out, "ws loopback", r, t);

    // 2. Outbound HTTPS (reqwest's rustls uses the platform verifier).
    for url in ["https://control.illogical.widgets.wtf/", "https://github.com/"] {
        let t = Instant::now();
        let r = async { client.get(url).send().await.map(|r| format!("HTTP {}", r.status())) }.await;
        report(&mut out, &format!("GET {url}"), r.map_err(|e| e.to_string()), t);
    }

    // 3. Outbound wss with rustls + the platform verifier, as the daemon dials.
    let t = Instant::now();
    let r = async {
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?;
        let cfg = rustls_platform_verifier::BuilderVerifierExt::with_platform_verifier(cfg)
            .map_err(|e| e.to_string())?
            .with_no_client_auth();
        let conn = tokio_tungstenite::Connector::Rustls(Arc::new(cfg));
        let (mut ws, _) =
            tokio_tungstenite::connect_async_tls_with_config("wss://echo.websocket.org/", None, false, Some(conn))
                .await
                .map_err(|e| e.to_string())?;
        // echo.websocket.org greets first, then echoes.
        let _greeting = ws.next().await;
        ws.send(tokio_tungstenite::tungstenite::Message::Text("s31".into())).await.map_err(|e| e.to_string())?;
        match ws.next().await {
            Some(Ok(m)) => Ok(format!("echoed {m}")),
            other => Err(format!("{other:?}")),
        }
    }
    .await;
    report(&mut out, "wss echo.websocket.org (platform verifier)", r, t);

    // 4. Ghostty's lib-vt.
    let t = Instant::now();
    let mut vt = GhosttyEngine::new(80, 24);
    for i in 0..2000 {
        vt.feed(format!("\x1b[1;3{}mline {i}\x1b[0m hello from the phone\r\n", i % 8).as_bytes());
    }
    let snap = vt.snapshot();
    let text = vt.plain_text();
    let last = text.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("").to_string();
    report(&mut out, "lib-vt feed 2000 lines + snapshot", Ok(format!("snapshot {} bytes, last line {last:?}", snap.len())), t);

    out
}

/// Runs the self test on a fresh tokio runtime and returns the report as a C
/// string (free with `s31_free`).
#[unsafe(no_mangle)]
pub extern "C" fn s31_rt_selftest() -> *mut c_char {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build();
    let lines = match rt {
        Ok(rt) => rt.block_on(selftest()),
        Err(e) => vec![format!("FAIL tokio runtime: {e}")],
    };
    CString::new(lines.join("\n")).unwrap_or_default().into_raw()
}

/// A long-lived server for the background test: axum on loopback, and a
/// once-a-second request to itself, logged through `log`.
#[unsafe(no_mangle)]
pub extern "C" fn s31_rt_heartbeat(log: extern "C" fn(*const c_char)) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let app = Router::new().route("/", get(|| async { "beat" }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move { axum::serve(listener, app).await });
            let client = reqwest::Client::new();
            let mut n = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                n += 1;
                let loop_ok = client.get(format!("http://127.0.0.1:{port}/")).send().await.is_ok();
                let net = client
                    .get("https://control.illogical.widgets.wtf/")
                    .timeout(std::time::Duration::from_secs(4))
                    .send()
                    .await
                    .map(|r| r.status().as_u16().to_string())
                    .unwrap_or_else(|e| format!("err:{e}"));
                let line = CString::new(format!("rt beat {n} loopback={loop_ok} control={net}")).unwrap();
                log(line.as_ptr());
            }
        });
    });
}

/// Frees a string returned by this library.
///
/// # Safety
/// `p` must come from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn s31_free(p: *mut c_char) {
    if !p.is_null() {
        drop(unsafe { CString::from_raw(p) });
    }
}
