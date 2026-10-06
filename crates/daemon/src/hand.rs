//! Hands (S33): a client device as tools agents may call. A connected
//! client (the phone's page, say) offers tools with `ClientMsg::Hand`; an
//! agent calls one through MCP (`device_call`), and the call goes to that
//! client as `ServerMsg::HandCall`, which answers with `HandReply`. The
//! person holding the device decides each call there.
//!
//! A hand that isn't connected can be woken: devices offering tools over
//! control's channel are remembered (`<state>/hands.json`), and a call to
//! one pushes a notification to that device (through control, encrypted
//! here). Opening it brings the page back, which offers its tools again,
//! and the call goes through.
//!
//! Only the owner's devices are hands, and only the owner's agents call
//! them. A photo or a recording comes back as base64 and is written to
//! `<state>/hand/`, so the agent gets a path on this machine.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use illogical_proto::{ClientId, HandTool, ServerMsg};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::pane::ToClient;

/// How long a woken hand has to come back.
pub const WAKE_WAIT: Duration = Duration::from_secs(120);

pub struct Hands {
    dir: PathBuf,
    inner: Mutex<Inner>,
    /// Something offered tools (or left): wakers wait on it.
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct Inner {
    conns: HashMap<ClientId, Conn>,
    pending: HashMap<u64, Pending>,
    next: u64,
    /// Devices that have been hands, by device id.
    known: BTreeMap<String, Known>,
}

struct Conn {
    ctrl: mpsc::UnboundedSender<ToClient>,
    owner: bool,
    /// The device's id and name, for a client over control's channel.
    device: Option<(String, String)>,
    offer: Option<Offer>,
}

struct Offer {
    tools: Vec<HandTool>,
    name: String,
}

struct Pending {
    client: ClientId,
    tx: oneshot::Sender<Result<Value, String>>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Known {
    name: String,
    tools: Vec<HandTool>,
    /// Unix ms it last offered tools.
    seen: u64,
}

/// Where a call went, and how long each part took.
pub struct Called {
    pub device: String,
    pub name: String,
    pub result: Value,
    pub woke_ms: Option<u64>,
    pub answer_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl Hands {
    pub fn open(state_dir: &std::path::Path) -> Arc<Self> {
        let known = std::fs::read(state_dir.join("hands.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Arc::new(Self {
            dir: state_dir.to_owned(),
            inner: Mutex::new(Inner { known, next: 1, ..Default::default() }),
            changed: Default::default(),
        })
    }

    /// A client connected. `device`: its id and name, over control's channel.
    pub fn connect(
        &self,
        client: ClientId,
        ctrl: mpsc::UnboundedSender<ToClient>,
        owner: bool,
        device: Option<(String, String)>,
    ) {
        self.inner.lock().unwrap().conns.insert(client, Conn { ctrl, owner, device, offer: None });
    }

    /// It left: its calls fail.
    pub fn disconnect(&self, client: ClientId) {
        let mut i = self.inner.lock().unwrap();
        let was = i.conns.remove(&client).is_some_and(|c| c.offer.is_some());
        let ids: Vec<u64> = i.pending.iter().filter(|(_, p)| p.client == client).map(|(id, _)| *id).collect();
        for id in ids {
            if let Some(p) = i.pending.remove(&id) {
                let _ = p.tx.send(Err("the device went away before answering".into()));
            }
        }
        drop(i);
        if was {
            self.changed.notify_waiters();
        }
    }

    /// `ClientMsg::Hand`: what it offers now (nothing: it stops).
    pub fn offer(&self, client: ClientId, tools: Vec<HandTool>, name: Option<String>) {
        let mut i = self.inner.lock().unwrap();
        let Some(c) = i.conns.get_mut(&client) else { return };
        if !c.owner {
            warn!(client, "refused a hand from someone who isn't an owner here");
            return;
        }
        if tools.is_empty() {
            c.offer = None;
        } else {
            let name =
                name.or_else(|| c.device.as_ref().map(|d| d.1.clone())).unwrap_or_else(|| format!("client {client}"));
            info!(client, %name, tools = tools.len(), "a hand offers tools");
            if let Some((id, _)) = &c.device {
                let id = id.clone();
                let k = Known { name: name.clone(), tools: tools.clone(), seen: now_ms() };
                c.offer = Some(Offer { tools, name });
                i.known.insert(id, k);
                self.save(&i);
            } else {
                c.offer = Some(Offer { tools, name });
            }
        }
        drop(i);
        self.changed.notify_waiters();
    }

    /// `ClientMsg::HandReply`, from the client that was asked.
    pub fn reply(&self, client: ClientId, id: u64, result: Option<Value>, error: Option<String>) {
        let mut i = self.inner.lock().unwrap();
        if i.pending.get(&id).is_none_or(|p| p.client != client) {
            return;
        }
        let p = i.pending.remove(&id).unwrap();
        let _ = p.tx.send(match (result, error) {
            (_, Some(e)) => Err(e),
            (Some(r), None) => Ok(r),
            (None, None) => Ok(Value::Null),
        });
    }

    fn save(&self, i: &Inner) {
        let body = serde_json::to_vec_pretty(&i.known).unwrap_or_default();
        if let Err(e) = crate::store::write_atomic(&self.dir.join("hands.json"), &body) {
            warn!(error = %e, "can't save hands.json");
        }
    }

    /// Every hand: connected ones, and devices that were (which a call
    /// wakes).
    pub fn list(&self) -> Vec<Value> {
        let i = self.inner.lock().unwrap();
        let mut out = Vec::new();
        let mut live = std::collections::HashSet::new();
        for (client, c) in &i.conns {
            let Some(o) = &c.offer else { continue };
            let id = c.device.as_ref().map_or_else(|| format!("client-{client}"), |d| d.0.clone());
            // A device with two connections here is one hand.
            if !live.insert(id.clone()) {
                continue;
            }
            out.push(json!({ "device": id, "name": o.name, "connected": true, "tools": o.tools }));
        }
        for (id, k) in &i.known {
            if live.contains(id) {
                continue;
            }
            out.push(json!({ "device": id, "name": k.name, "connected": false, "wakeable": true, "last_seen_ms": k.seen, "tools": k.tools }));
        }
        out
    }

    /// Which device `sel` names (its id, or part of its name); with none,
    /// the only one there is.
    fn resolve(&self, sel: Option<&str>) -> Result<(String, String), String> {
        let all = self.list();
        let pick =
            |d: &Value| (d["device"].as_str().unwrap_or("").to_owned(), d["name"].as_str().unwrap_or("").to_owned());
        let found: Vec<(String, String)> = match sel {
            None => all.iter().map(pick).collect(),
            Some(s) => {
                let s = s.to_lowercase();
                let exact: Vec<_> = all.iter().map(pick).filter(|(id, _)| id.to_lowercase() == s).collect();
                if exact.is_empty() {
                    all.iter().map(pick).filter(|(_, n)| n.to_lowercase().contains(&s)).collect()
                } else {
                    exact
                }
            }
        };
        match found.as_slice() {
            [one] => Ok(one.clone()),
            [] if all.is_empty() => Err("no device has offered tools here yet: open illogical on the phone and turn on \"Lend this device to agents\"".into()),
            [] => Err(format!("no device matches {:?}: see list (kind devices)", sel.unwrap_or(""))),
            more => Err(format!(
                "{} devices match; say which: {}",
                more.len(),
                more.iter().map(|(id, n)| format!("{n} ({id})")).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    /// The connected client offering `tool` as `device`.
    fn live(&self, device: &str, tool: &str) -> Option<Result<(ClientId, String), String>> {
        let i = self.inner.lock().unwrap();
        i.conns.iter().find_map(|(client, c)| {
            let o = c.offer.as_ref()?;
            let id = c.device.as_ref().map_or_else(|| format!("client-{client}"), |d| d.0.clone());
            (id == device).then(|| {
                if o.tools.iter().any(|t| t.name == tool) {
                    Ok((*client, o.name.clone()))
                } else {
                    Err(format!(
                        "{} has no tool {tool:?}; it offers {}",
                        o.name,
                        o.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
                    ))
                }
            })
        })
    }

    /// Call `tool` on a device, waking it first if it isn't connected.
    /// The caller bounds how long this may take.
    pub async fn call(
        &self,
        control: &Arc<crate::control::Control>,
        sel: Option<&str>,
        tool: &str,
        args: Value,
        from: &str,
    ) -> Result<Called, String> {
        let (device, name) = self.resolve(sel)?;
        let mut woke_ms = None;
        let (client, name) = match self.live(&device, tool) {
            Some(r) => r?,
            None => {
                let started = Instant::now();
                let payload = json!({
                    "title": format!("{from} wants your {name}"),
                    "body": format!("To use {tool}. Open to answer."),
                    "tag": "hand",
                    "hand": true,
                    "pane": 0,
                });
                if !control.push_device(&device, payload) {
                    return Err(format!(
                        "{name} isn't connected, and can't be woken: it has no push subscription this machine trusts (turn on notifications on it)"
                    ));
                }
                info!(%device, %name, tool, "woke a hand");
                let deadline = started + WAKE_WAIT;
                loop {
                    let notified = self.changed.notified();
                    if let Some(r) = self.live(&device, tool) {
                        woke_ms = Some(started.elapsed().as_millis() as u64);
                        break r?;
                    }
                    if tokio::time::timeout_at(deadline.into(), notified).await.is_err() {
                        return Err(format!(
                            "{name} didn't come back within {}s of the notification",
                            WAKE_WAIT.as_secs()
                        ));
                    }
                }
            }
        };

        let (tx, rx) = oneshot::channel();
        let id = {
            let mut i = self.inner.lock().unwrap();
            let id = i.next;
            i.next += 1;
            let ctrl = i.conns.get(&client).map(|c| c.ctrl.clone()).ok_or("the device went away")?;
            i.pending.insert(id, Pending { client, tx });
            let _ = ctrl.send(ToClient::Msg(ServerMsg::HandCall {
                id,
                tool: tool.to_owned(),
                args,
                from: from.to_owned(),
            }));
            id
        };
        // A caller that gives up takes its call back.
        struct Forget<'a>(&'a Hands, u64);
        impl Drop for Forget<'_> {
            fn drop(&mut self) {
                self.0.inner.lock().unwrap().pending.remove(&self.1);
            }
        }
        let _forget = Forget(self, id);
        let asked = Instant::now();
        let result = rx.await.map_err(|_| "the device went away".to_owned())??;
        let answer_ms = asked.elapsed().as_millis() as u64;
        let result = self.keep_files(&device, tool, result)?;
        Ok(Called { device, name, result, woke_ms, answer_ms })
    }

    /// `{"image" | "audio": {"data": base64, "mime"}}` in a result becomes
    /// a file here: `{"path", "bytes", "mime"}`.
    fn keep_files(&self, device: &str, tool: &str, mut result: Value) -> Result<Value, String> {
        use base64::Engine;
        for key in ["image", "audio"] {
            let Some(f) = result.get_mut(key) else { continue };
            let Some(data) = f.get("data").and_then(Value::as_str) else { continue };
            // Plain base64, or a data: URL.
            let data = if data.starts_with("data:") { data.split_once(',').map_or("", |(_, d)| d) } else { data };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| format!("the device sent a {key} that isn't base64: {e}"))?;
            let mime = f.get("mime").and_then(Value::as_str).unwrap_or("application/octet-stream").to_owned();
            let ext = match mime.split(';').next().unwrap_or("") {
                "image/jpeg" => "jpg",
                "image/png" => "png",
                "image/heic" => "heic",
                "image/webp" => "webp",
                "audio/mp4" => "m4a",
                "audio/webm" => "webm",
                "audio/ogg" => "ogg",
                _ => "bin",
            };
            let dir = self.dir.join("hand");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let short: String = device.chars().take(8).collect();
            let path = dir.join(format!("{}-{short}-{tool}.{ext}", now_ms()));
            std::fs::write(&path, &bytes).map_err(|e| format!("can't save the {key}: {e}"))?;
            *f = json!({ "path": path, "bytes": bytes.len(), "mime": mime });
        }
        Ok(result)
    }
}
