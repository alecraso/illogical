//! M6's block contract, through the first non-terminal type (a browser
//! block on ordinary pages): open, describe, call, capture, restore after a
//! restart, close.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    io::{Read, Write},
    net::TcpListener,
};

use arugula_testkit::{Daemon, arugulad};
use serde_json::json;

fn start() -> Daemon {
    arugulad!("blk").no_wisp().wait_secs(10).start()
}

/// A page that allows framing, with a title, on a port of its own.
fn serve_page() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let body = "<html><head><title>Plain page</title></head><body>hi</body></html>";
            let _ = s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    port
}

#[test]
fn a_browser_block_opens_describes_calls_restores_and_closes() {
    let mut d = start();
    let first = d.get("/api/panes")[0]["id"].as_u64().unwrap();
    let page = serve_page();

    // The daemon's own page refuses to be framed; it's shown as a card.
    let app = format!("http://127.0.0.1:{}/", d.port);
    let id = d.post("/api/blocks", json!({"type": "browser", "config": {"url": app}, "split": first}))["block"]
        .as_u64()
        .unwrap();
    let state = |d: &Daemon| d.get(&format!("/api/blocks/{id}"))["state"].clone();
    d.wait_for("the probe", || state(&d)["framable"].is_boolean());
    assert_eq!(state(&d)["framable"], false);
    let described = d.get(&format!("/api/blocks/{id}"));
    assert_eq!(described["info"]["type"], "browser");
    assert_eq!(described["info"]["tab"], d.get("/api/panes")[0]["tab"], "split beside the first pane");

    // Methods: navigate, then back.
    let plain = format!("http://127.0.0.1:{page}/x");
    d.post(&format!("/api/blocks/{id}/call/navigate"), json!({"url": plain}));
    d.wait_for("the new page", || state(&d)["title"] == "Plain page");
    assert_eq!(state(&d)["framable"], true);
    assert_eq!(state(&d)["back"], json!([app]));
    let (status, err) = d.raw("POST", &format!("/api/blocks/{id}/call/fly"), Some(json!({})));
    assert_eq!(status, 400, "{err}");
    let text = d.raw("GET", &format!("/api/panes/{id}/capture"), None).1;
    assert_eq!(text, format!("Plain page\n{plain}\n"));
    // Listed with the terminals, in the one id space.
    let ls = d.get("/api/panes");
    assert!(ls.as_array().unwrap().iter().any(|p| p["id"] == id && p["type"] == "browser"), "{ls}");

    // Terminals answer the same call route.
    d.post(&format!("/api/blocks/{first}/call/send"), json!({"text": "echo via-call-$((1+1))", "enter": true}));
    d.wait_for("the terminal", || {
        d.post(&format!("/api/blocks/{first}/call/capture"), json!({}))["text"].as_str().unwrap().contains("via-call-2")
    });

    // A restart brings it back from its config, where it was.
    d.stop();
    d.start();
    d.wait_for("the restored block", || {
        d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 200 && state(&d)["title"] == "Plain page"
    });

    // Closing it keeps its history like a pane's.
    d.post(&format!("/api/panes/{id}/close"), json!({}));
    d.wait_for("it to go", || d.raw("GET", &format!("/api/blocks/{id}"), None).0 == 404);
    let closed: Vec<_> = std::fs::read_dir(d.state.join("closed")).unwrap().flatten().collect();
    assert!(closed.iter().any(|e| e.file_name().to_string_lossy().starts_with(&format!("{id}-"))));
}
