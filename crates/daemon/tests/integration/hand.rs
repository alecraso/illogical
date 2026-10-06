//! S33: a client as a hand. A fake phone on `/ws` offers tools; an agent
//! through `illogical mcp` lists it and calls them: a location, a photo
//! that lands as a file here, a denied call, and a call that fails because
//! the phone went away.

use crate::agentd;

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use agentd::*;
use futures_util::{SinkExt, StreamExt};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
    service::RunningService,
    transport::TokioChildProcess,
};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

async fn agent(d: &Daemon) -> RunningService<rmcp::RoleClient, ()> {
    let mut cmd = tokio::process::Command::new(cli_bin());
    cmd.arg("--socket").arg(d.sock()).arg("mcp");
    ().serve(TokioChildProcess::new(cmd).unwrap()).await.expect("connecting through illogical mcp")
}

async fn call(s: &RunningService<rmcp::RoleClient, ()>, tool: &str, args: Value) -> CallToolResult {
    let params =
        CallToolRequestParams::new(tool.to_owned()).with_arguments(args.as_object().cloned().unwrap_or_default());
    s.call_tool(params).await.unwrap_or_else(|e| panic!("{tool}: {e}"))
}

fn text(r: &CallToolResult) -> String {
    r.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect()
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The next control message of `kind`, skipping the rest.
async fn next_of(ws: &mut Ws, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match ws.next().await.expect("socket open").expect("socket ok") {
                Message::Text(t) => {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v["type"] == kind {
                        return v;
                    }
                }
                _ => continue,
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} message"))
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_uses_a_phone() {
    let d = Daemon::child_with(&["--wisp-token-file", "/nonexistent"]);
    let s = agent(&d).await;

    // Nothing lends tools yet.
    let r = call(&s, "device_call", json!({ "tool": "location" })).await;
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("no device has offered tools"), "{}", text(&r));

    let (mut ws, _) = tokio_tungstenite::connect_async(d.ws("/ws")).await.unwrap();
    next_of(&mut ws, "hello").await;
    let schema = json!({ "type": "object", "properties": {} });
    send(
        &mut ws,
        json!({ "type": "hand", "name": "Test iPhone", "tools": [
            { "name": "location", "description": "where", "schema": schema },
            { "name": "take_photo", "description": "a photo", "schema": schema },
        ]}),
    )
    .await;

    let listed = loop {
        let r = call(&s, "list", json!({ "kind": "devices" })).await;
        let v = r.structured_content.unwrap();
        if v["devices"].as_array().is_some_and(|a| !a.is_empty()) {
            break v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let dev = &listed["devices"][0];
    assert_eq!((dev["name"].as_str(), dev["connected"].as_bool()), (Some("Test iPhone"), Some(true)));
    assert_eq!(dev["tools"].as_array().unwrap().len(), 2);

    // A location: the call reaches the phone with who asks, and its answer
    // comes back.
    let (r, _) =
        tokio::join!(call(&s, "device_call", json!({ "tool": "location", "args": { "precise": true } })), async {
            let c = next_of(&mut ws, "hand_call").await;
            assert_eq!(c["tool"], "location");
            assert_eq!(c["args"]["precise"], true);
            assert!(c["from"].as_str().unwrap().contains(" on "), "{c}");
            send(&mut ws, json!({ "type": "hand_reply", "id": c["id"], "result": { "lat": 36.1, "lon": -86.8 } }))
                .await;
        });
    assert_ne!(r.is_error, Some(true), "{}", text(&r));
    let v = r.structured_content.unwrap();
    assert_eq!(v["result"]["lat"], 36.1);
    assert_eq!(v["name"], "Test iPhone");

    // A photo becomes a file on this machine.
    let jpeg = [0xffu8, 0xd8, 0xff, 0xe0, 1, 2, 3];
    let (r, _) = tokio::join!(call(&s, "device_call", json!({ "tool": "take_photo", "device": "iphone" })), async {
        use base64::Engine;
        let c = next_of(&mut ws, "hand_call").await;
        let data = base64::engine::general_purpose::STANDARD.encode(jpeg);
        let result = json!({ "image": { "data": data, "mime": "image/jpeg" }, "width": 1 });
        send(&mut ws, json!({ "type": "hand_reply", "id": c["id"], "result": result })).await;
    });
    let v = r.structured_content.unwrap();
    let path = v["result"]["image"]["path"].as_str().unwrap();
    assert!(path.ends_with("-take_photo.jpg"), "{path}");
    assert_eq!(std::fs::read(path).unwrap(), jpeg);
    assert_eq!(v["result"]["image"]["bytes"], jpeg.len());

    // A tool it doesn't have, and one the person denies.
    let r = call(&s, "device_call", json!({ "tool": "nfc" })).await;
    assert!(text(&r).contains("has no tool \"nfc\""), "{}", text(&r));
    let (r, _) = tokio::join!(call(&s, "device_call", json!({ "tool": "location" })), async {
        let c = next_of(&mut ws, "hand_call").await;
        send(&mut ws, json!({ "type": "hand_reply", "id": c["id"], "error": "the person denied it" })).await;
    });
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("denied"), "{}", text(&r));

    // The phone leaves while a call waits: the call fails at once, and
    // without control to wake it through, it can't be called again.
    let (r, _) = tokio::join!(call(&s, "device_call", json!({ "tool": "location" })), async {
        next_of(&mut ws, "hand_call").await;
        ws.close(None).await.unwrap();
    });
    assert_eq!(r.is_error, Some(true));
    assert!(text(&r).contains("went away"), "{}", text(&r));
    let r = call(&s, "list", json!({ "kind": "devices" })).await;
    assert_eq!(r.structured_content.unwrap()["devices"], json!([]));
}
