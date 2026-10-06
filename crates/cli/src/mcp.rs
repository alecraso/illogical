//! `illogical mcp` (M16): a stdio MCP server for clients that start one as a
//! command (`claude mcp add illogical -- illogical mcp`), bridged to the
//! daemon's own at `/mcp`, over its Unix socket (or another daemon's, with
//! `--host`).
//!
//! It relays, without understanding the tools: each JSON-RPC line from
//! stdin is POSTed to `/mcp` (on a thread of its own, so a long `wait`
//! doesn't hold up the rest), and what comes back, a JSON body or an SSE
//! stream of progress and then the result, is written to stdout a line per
//! message. It adds what Streamable HTTP wants that stdio doesn't have: the
//! session id an `initialize` got, the protocol version, and the
//! `Mcp-Method`/`Mcp-Name` headers. If the daemon restarted (its sessions
//! are gone: 404), it opens a new session with the client's own
//! `initialize` and sends the request again, so the client never notices.
//!
//! In a pane, it says which (`$ILLOGICAL_PANE`, as `X-Illogical-Pane`): a
//! default for tools that act where the client works (#234's
//! `invite_person`), not a credential.

use std::{
    io::{BufRead, BufReader, Write},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use anyhow::Context;
use serde_json::{Value, json};

use crate::http::{self, Target};

const PATH: &str = "/mcp";

#[derive(Default)]
struct Session {
    /// `Mcp-Session-Id`, for clients that use sessions (`initialize`).
    id: Option<String>,
    /// The version `initialize` agreed on.
    version: Option<String>,
    /// The client's `initialize` and `notifications/initialized`, to open a
    /// new session with after the daemon restarts.
    init: Option<String>,
    initialized: Option<String>,
}

struct Bridge {
    target: Target,
    token: Option<String>,
    /// The pane this runs in (`$ILLOGICAL_PANE`), sent on every request so
    /// the tools can default to it.
    pane: Option<String>,
    session: Mutex<Session>,
    out: Mutex<std::io::Stdout>,
}

pub fn run(target: Target, token: Option<String>) -> anyhow::Result<i32> {
    // Another daemon's panes aren't this shell's.
    let pane = std::env::var("ILLOGICAL_PANE")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|_| matches!(target, Target::Socket(_)))
        .map(|p| p.to_string());
    let bridge = Arc::new(Bridge {
        target,
        token,
        pane,
        session: Mutex::new(Session::default()),
        out: Mutex::new(std::io::stdout()),
    });
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.context("reading stdin")?;
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                bridge.write(&json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } }));
                continue;
            }
        };
        match msg["method"].as_str() {
            Some("initialize") => bridge.session.lock().unwrap().init = Some(line.clone()),
            Some("notifications/initialized") => bridge.session.lock().unwrap().initialized = Some(line.clone()),
            _ => {}
        }
        // In the order the client sent them: each goes once the one before
        // it is under way (the daemon answered its headers), and anything
        // after `initialize` waits for its session.
        let wait = if msg["method"] == "initialize" { Duration::from_secs(30) } else { Duration::from_secs(2) };
        let (sent, under_way) = mpsc::channel();
        let b = bridge.clone();
        std::thread::spawn(move || b.forward(&line, &msg, sent));
        let _ = under_way.recv_timeout(wait);
    }
    // The client went away: end its session, if it had one.
    let id = bridge.session.lock().unwrap().id.clone();
    if let Some(id) = id {
        let _ = http::send(&bridge.target, "DELETE", PATH, &[("Mcp-Session-Id", &id)], b"");
    }
    Ok(0)
}

/// `Mcp-Name`'s value for a request, if its method has one.
fn mcp_name(msg: &Value) -> Option<String> {
    let key = match msg["method"].as_str()? {
        "tools/call" | "prompts/get" => "name",
        "resources/read" | "resources/subscribe" | "resources/unsubscribe" => "uri",
        "tasks/get" | "tasks/update" | "tasks/cancel" => "taskId",
        _ => return None,
    };
    let v = msg["params"][key].as_str()?;
    let plain = !v.starts_with([' ', '\t'])
        && !v.ends_with([' ', '\t'])
        && v.chars().all(|c| (' '..='~').contains(&c))
        && !(v.starts_with("=?base64?") && v.ends_with("?="));
    Some(if plain { v.to_owned() } else { format!("=?base64?{}?=", base64(v.as_bytes())) })
}

fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl Bridge {
    fn write(&self, v: &Value) {
        let mut out = self.out.lock().unwrap();
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    }

    /// Send one message on, and its answers back.
    fn forward(&self, line: &str, msg: &Value, sent: mpsc::Sender<()>) {
        let id = msg.get("id").cloned().filter(|_| msg.get("method").is_some());
        let mut used = None;
        let result = self.post(line, msg, &mut used, &sent).and_then(|status| match status {
            404 if used.is_some() => {
                // The daemon restarted: a new session, then this again.
                self.reopen(used.as_deref())?;
                self.post(line, msg, &mut None, &sent).and_then(|s| match s {
                    200..300 => Ok(()),
                    s => anyhow::bail!("the daemon answered HTTP {s}"),
                })
            }
            200..300 => Ok(()),
            s => anyhow::bail!("the daemon answered HTTP {s}"),
        });
        let _ = sent.send(());
        if let (Err(e), Some(id)) = (result, id) {
            self.write(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32603, "message": format!("illogical: {e:#}") } }));
        }
    }

    /// POST it; relay what comes back unless it's a 404 for a session (the
    /// caller reopens it). `used`: the session id it went with.
    fn post(&self, line: &str, msg: &Value, used: &mut Option<String>, sent: &mpsc::Sender<()>) -> anyhow::Result<u16> {
        let (session, version) = {
            let s = self.session.lock().unwrap();
            (s.id.clone(), s.version.clone())
        };
        *used = session.clone();
        // A stateless request says its version in `_meta`; a session's is
        // what `initialize` agreed on.
        let version = msg["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
            .as_str()
            .map(str::to_owned)
            .or(version)
            .filter(|_| msg["method"] != "initialize");
        let auth = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let name = mcp_name(msg);
        let mut headers: Vec<(&str, &str)> =
            vec![("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream")];
        if let Some(s) = &session {
            headers.push(("Mcp-Session-Id", s));
        }
        if let Some(v) = &version {
            headers.push(("MCP-Protocol-Version", v));
        }
        if let Some(m) = msg["method"].as_str() {
            headers.push(("Mcp-Method", m));
        }
        if let Some(n) = &name {
            headers.push(("Mcp-Name", n));
        }
        if let Some(a) = &auth {
            headers.push(("Authorization", a));
        }
        if let Some(p) = &self.pane {
            headers.push(("X-Illogical-Pane", p));
        }
        let res = http::send(&self.target, "POST", PATH, &headers, line.as_bytes())?;
        let status = res.status;
        if status == 404 && session.is_some() {
            return Ok(status);
        }
        if msg["method"] == "initialize" {
            let mut s = self.session.lock().unwrap();
            if let Some(id) = res.header("mcp-session-id") {
                s.id = Some(id.to_owned());
            }
        } else {
            let _ = sent.send(());
        }
        if !(200..300).contains(&status) {
            let text = res.text().unwrap_or_default();
            let why = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().or(v["error"]["message"].as_str()).map(str::to_owned))
                .unwrap_or(text);
            anyhow::bail!("{} (HTTP {status})", why.trim());
        }
        let sse = res.header("content-type").is_some_and(|c| c.starts_with("text/event-stream"));
        if !sse {
            let text = res.text()?;
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                self.answered(msg, &v);
                self.write(&v);
            }
            return Ok(status);
        }
        // Server-sent events: each `data` is a message.
        let mut data = String::new();
        for l in BufReader::new(res).lines() {
            let l = l?;
            if let Some(d) = l.strip_prefix("data:") {
                data.push_str(d.strip_prefix(' ').unwrap_or(d));
            } else if l.is_empty() && !data.is_empty() {
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    self.answered(msg, &v);
                    self.write(&v);
                }
                data.clear();
            }
        }
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            self.write(&v);
        }
        Ok(status)
    }

    /// Note the version an `initialize` agreed on.
    fn answered(&self, msg: &Value, v: &Value) {
        if msg["method"] == "initialize"
            && let Some(ver) = v["result"]["protocolVersion"].as_str()
        {
            self.session.lock().unwrap().version = Some(ver.to_owned());
        }
    }

    /// The session `old` is gone: open another with the client's own
    /// `initialize` (its answer isn't the client's to see again).
    fn reopen(&self, old: Option<&str>) -> anyhow::Result<()> {
        let (init, initialized) = {
            let mut s = self.session.lock().unwrap();
            if s.id.as_deref() != old {
                // Another request already did.
                return Ok(());
            }
            s.id = None;
            (s.init.clone(), s.initialized.clone())
        };
        let init = init.context("the daemon forgot this session, and it never began with initialize")?;
        let headers = [("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream")];
        let mut headers = headers.to_vec();
        let auth = self.token.as_ref().map(|t| format!("Bearer {t}"));
        if let Some(a) = &auth {
            headers.push(("Authorization", a));
        }
        if let Some(p) = &self.pane {
            headers.push(("X-Illogical-Pane", p));
        }
        let res = http::send(&self.target, "POST", PATH, &headers, init.as_bytes())?;
        let id = res.header("mcp-session-id").map(str::to_owned);
        let _ = res.bytes();
        let id = id.context("the daemon gave the new session no id")?;
        {
            let mut s = self.session.lock().unwrap();
            s.id = Some(id.clone());
        }
        if let Some(n) = initialized {
            let version = self.session.lock().unwrap().version.clone();
            let mut h = headers.clone();
            h.push(("Mcp-Session-Id", &id));
            if let Some(v) = &version {
                h.push(("MCP-Protocol-Version", v));
            }
            h.push(("Mcp-Method", "notifications/initialized"));
            let _ = http::send(&self.target, "POST", PATH, &h, n.as_bytes()).and_then(|r| r.bytes());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_base64() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi"), "aGk=");
        assert_eq!(base64(b"abc"), "YWJj");
        let call = json!({ "method": "tools/call", "params": { "name": "run" } });
        assert_eq!(mcp_name(&call).as_deref(), Some("run"));
        let read = json!({ "method": "resources/read", "params": { "uri": "illogical://pane/7/output" } });
        assert_eq!(mcp_name(&read).as_deref(), Some("illogical://pane/7/output"));
        let odd = json!({ "method": "tools/call", "params": { "name": " é" } });
        assert_eq!(mcp_name(&odd).as_deref(), Some("=?base64?IMOp?="));
        assert_eq!(mcp_name(&json!({ "method": "tools/list" })), None);
    }
}
