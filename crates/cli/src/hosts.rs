//! `--host` and `illogical hosts`: other daemons, from the home daemon's
//! list. A name is looked up in that list (over the local socket); a URL is
//! used as it is. Either way the commands then talk to that daemon
//! directly, over HTTP(S), where its usual access checks apply. A resident
//! daemon in a sandbox (a provider host) is reached through the home
//! daemon's provider tunnel instead, which also wakes it.
//!
//! `illogical sandboxes`: the home daemon's provider's sandboxes, a shell
//! on one with no daemon there (`run --sandbox`), and making a daemon
//! resident in one.

use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::http::{Target, Url, request};

#[derive(Subcommand)]
pub enum HostsCmd {
    /// Add a daemon (or replace the one with this name).
    Add {
        name: String,
        /// Its URL(s), best first: `https://box.tailnet.ts.net`. Or one
        /// `ssh://[user@]box[:port]` (M51): reached over ssh from each client,
        /// with your own ssh and its ~/.ssh/config; only that is kept.
        #[arg(required = true)]
        urls: Vec<String>,
    },
    /// Remove a daemon from the list.
    Rm { name: String },
    /// A one-time token that lets a sandbox add itself
    /// (`illogicald install --tailnet … --join TOKEN`).
    Invite {
        /// How long it's good for (e.g. 30m, 2h).
        #[arg(long, default_value = "1h")]
        ttl: String,
    },
    /// Mint a per-host token for a host without tailnet identity: one that
    /// dials out (`illogicald --peer wss://this-daemon --token FILE`) or
    /// pushes its history (`--sync`). Adds it as a dial-out host if it isn't
    /// listed; replaces any token it had. Printed once; only its hash is
    /// kept.
    Token { name: String },
    /// Revoke a host's token, and drop its dial-out connection.
    Revoke { name: String },
}

#[derive(Subcommand)]
pub enum SandboxesCmd {
    /// Copy the static daemon into a sandbox and keep it running there as
    /// a provider service; it joins the host list, reached through this
    /// daemon's tunnel (`--host NAME`).
    Promote {
        sandbox: String,
        /// Its name in the host list [default: the sandbox's].
        #[arg(long = "as", value_name = "HOST")]
        name: Option<String>,
        /// Its port inside the sandbox [default: 7681].
        #[arg(long)]
        port: Option<u16>,
    },
    /// Stop a sandbox's resident daemon and take it off the host list.
    Demote { sandbox: String },
}

pub fn sandboxes(target: &Target, cmd: Option<SandboxesCmd>, json_out: bool) -> anyhow::Result<()> {
    let v = match cmd {
        None => {
            let v = request(target, "GET", "/api/sandboxes", None)?.json()?;
            if !json_out {
                let p = &v["provider"];
                let replay = p["exec_replay"].as_u64().unwrap_or(0);
                println!(
                    "{}: a shell with no daemon (`run --sandbox NAME`) keeps {} KB while detached",
                    p["name"].as_str().unwrap_or("?"),
                    replay / 1024
                );
                for s in v["sandboxes"].as_array().into_iter().flatten() {
                    let host = s["host"].as_str().map(|h| format!("resident: --host {h}")).unwrap_or_default();
                    println!(
                        "{:<34} {:<8} {host}",
                        s["name"].as_str().unwrap_or("?"),
                        s["status"].as_str().unwrap_or("?")
                    );
                }
                return Ok(());
            }
            v
        }
        Some(SandboxesCmd::Promote { sandbox, name, port }) => {
            let body = json!({"host": name, "port": port});
            let v = request(
                target,
                "POST",
                &format!("/api/sandboxes/{}/promote", crate::http::enc(&sandbox)),
                Some(&body),
            )?
            .json()?;
            if !json_out {
                println!("{} is resident in {sandbox}: `illogical --host {0} …`", v["name"].as_str().unwrap_or("?"));
                return Ok(());
            }
            v
        }
        Some(SandboxesCmd::Demote { sandbox }) => {
            request(target, "DELETE", &format!("/api/sandboxes/{}/resident", crate::http::enc(&sandbox)), None)?
                .json()?
        }
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    }
    Ok(())
}

/// Where commands go: the local socket, or the daemon `--host` names.
pub fn target(socket: PathBuf, host: Option<&str>) -> anyhow::Result<Target> {
    let local = Target::Socket(socket);
    let Some(host) = host else { return Ok(local) };
    if host.contains("://") {
        return Ok(Target::Url(Url::parse(host)?));
    }
    // The local daemon's list first; then control's directory (M49), which
    // needs no local daemon at all.
    let list = request(&local, "GET", "/api/hosts", None).and_then(|r| r.json());
    if let Ok(l) = &list
        && l["this"].as_str() == Some(host)
    {
        return Ok(local);
    }
    let entry = list
        .as_ref()
        .ok()
        .and_then(|l| l["hosts"].as_array().into_iter().flatten().find(|h| h["name"].as_str() == Some(host)).cloned());
    let Some(entry) = entry else {
        if let Some(t) = crate::control::target(host)? {
            return Ok(t);
        }
        let hint = if crate::control::logged_in() {
            ""
        } else {
            "; `illogical login` reaches the machines on your illogical control account"
        };
        return match list {
            Err(e) => Err(e.context(format!("looking up --host {host} in the local daemon's host list{hint}"))),
            Ok(_) => bail!("no host {host} (see `illogical hosts`){hint}"),
        };
    };
    // A host reached through the home daemon: one that dials out to it
    // (M4c), or a resident daemon in a sandbox, through the provider tunnel
    // (M4b; that also wakes it).
    // Over ssh, from this client (M51).
    if entry["transport"].as_str() == Some("ssh") {
        let dest = entry["ssh"].as_str().with_context(|| format!("host {host} has no ssh destination"))?;
        return Ok(Target::Ssh(crate::ssh::Remote::parse(dest)?));
    }
    let via = match entry["transport"].as_str() {
        Some("dial_out") => Some("h"),
        Some("provider") => Some("tunnel"),
        _ => None,
    };
    if let Some(via) = via {
        return Ok(Target::Via(socket_of(&local), format!("/{via}/{}", crate::http::enc(host))));
    }
    let urls: Vec<&str> = entry["urls"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    // The first URL that answers.
    let mut last = None;
    for u in &urls {
        let t = Target::Url(Url::parse(u)?);
        match t.connect() {
            Ok(_) => return Ok(t),
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) => Err(e.context(format!("can't reach {host}"))),
        None => bail!("host {host} has no URL"),
    }
}

fn socket_of(t: &Target) -> PathBuf {
    match t {
        Target::Socket(p) | Target::Via(p, _) => p.clone(),
        Target::Url(_) | Target::Ssh(_) | Target::Control(_) => unreachable!("the local daemon is a socket"),
    }
}

fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64);
    let secs = now.unwrap_or(0).saturating_sub(ms) / 1000;
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

pub fn run(
    target: &Target,
    cmd: Option<HostsCmd>,
    json_out: bool,
    secs: impl Fn(&str) -> anyhow::Result<u64>,
) -> anyhow::Result<()> {
    let v = match cmd {
        None => {
            // This daemon's list (if one runs here), and control's
            // directory (if this CLI is logged in), marked apart.
            let local = request(target, "GET", "/api/hosts", None).and_then(|r| r.json());
            let control = crate::control::listing();
            let local = match (local, &control) {
                (Ok(v), _) => v,
                (Err(_), Ok(Some(_))) => Value::Null,
                (Err(e), _) => return Err(e),
            };
            let control = control.unwrap_or_else(|e| {
                eprintln!("illogical: control's directory: {e:#}");
                None
            });
            if json_out {
                let c = control.as_ref().map(|(url, list)| {
                    json!({"url": url, "machines": list.iter().map(|m| json!({"id": m.id, "name": m.name, "urls": m.urls, "online": m.online})).collect::<Vec<_>>()})
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"this": local["this"], "hosts": local["hosts"], "control": c})
                    )
                    .unwrap_or_default()
                );
                return Ok(());
            }
            if let Some((url, list)) = &control {
                for m in list {
                    let how = match (m.online, m.urls.is_empty()) {
                        (false, _) => "offline".to_owned(),
                        (true, true) => "online, relayed".to_owned(),
                        (true, false) => format!("online, direct {} or relayed", m.urls.join(" ")),
                    };
                    println!("{:<20} {:<44} (control: {url})", m.name, how);
                }
            }
            if local.is_null() {
                return Ok(());
            }
            let v = local;
            if !json_out {
                println!("{:<20} {:<44} (this daemon)", v["this"].as_str().unwrap_or("?"), "");
                for h in v["hosts"].as_array().into_iter().flatten() {
                    let mut urls: Vec<&str> =
                        h["urls"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                    if h["transport"].as_str() == Some("dial_out") {
                        urls.push("(dials out, reached through here)");
                    }
                    let ssh;
                    if h["transport"].as_str() == Some("ssh") {
                        ssh = format!("ssh {}", h["ssh"].as_str().unwrap_or("?"));
                        urls.push(&ssh);
                    }
                    let seen =
                        h["last_seen_ms"].as_u64().map(|t| format!("seen {}", ago(t))).unwrap_or("never seen".into());
                    let place = match h["provider"].as_object() {
                        Some(p) => format!(
                            "{} {} (tunnel){}",
                            p["provider"].as_str().unwrap_or("?"),
                            p["sandbox"].as_str().unwrap_or("?"),
                            if urls.is_empty() { String::new() } else { format!(" {}", urls.join(" ")) }
                        ),
                        None => urls.join(" "),
                    };
                    let seen = match h["status"].as_str() {
                        Some(st) => format!("{st}, {seen}"),
                        None => seen,
                    };
                    println!("{:<20} {place:<44} {seen}", h["name"].as_str().unwrap_or("?"));
                }
                return Ok(());
            }
            v
        }
        Some(HostsCmd::Add { name, urls }) if urls.iter().any(|u| u.starts_with("ssh://")) => {
            let [dest] = urls.as_slice() else { bail!("an ssh host has one ssh:// destination and no other URL") };
            let dest = crate::ssh::Remote::parse(dest)?.dest;
            let body = json!({"name": name, "urls": [], "transport": "ssh", "ssh": dest});
            request(target, "POST", "/api/hosts", Some(&body))?.json()?
        }
        Some(HostsCmd::Add { name, urls }) => {
            request(target, "POST", "/api/hosts", Some(&json!({"name": name, "urls": urls, "transport": "tailnet"})))?
                .json()?
        }
        Some(HostsCmd::Rm { name }) => {
            request(target, "DELETE", &format!("/api/hosts/{}", crate::http::enc(&name)), None)?.json()?
        }
        Some(HostsCmd::Invite { ttl }) => {
            let v = request(target, "POST", &format!("/api/hosts/invite?ttl={}", secs(&ttl)?), None)?.json()?;
            if !json_out {
                println!("{}", v["token"].as_str().unwrap_or_default());
                return Ok(());
            }
            v
        }
        Some(HostsCmd::Token { name }) => {
            let path = format!("/api/hosts/{}/token", crate::http::enc(&name));
            let v = request(target, "POST", &path, None)?.json()?;
            if !json_out {
                println!("{}", v["token"].as_str().unwrap_or_default());
                return Ok(());
            }
            v
        }
        Some(HostsCmd::Revoke { name }) => {
            request(target, "DELETE", &format!("/api/hosts/{}/token", crate::http::enc(&name)), None)?.json()?
        }
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    }
    Ok(())
}

/// What one host said to `--host all`.
pub enum Answer {
    Json(Value),
    /// A sandbox that isn't running: not woken just to be asked.
    Asleep,
    Failed(String),
    /// Nothing by the deadline.
    Silent,
}

/// `--host all` (#78): GET `path` on this daemon and every host on its list
/// at once, each answer handed to `each` as it comes. A host still silent
/// at `wait` (asleep without saying so, or unreachable) is reported as
/// such and not waited for: its thread is left behind.
pub fn each(
    socket: PathBuf,
    path: &str,
    wait: std::time::Duration,
    mut each: impl FnMut(&str, Answer),
) -> anyhow::Result<()> {
    let local = Target::Socket(socket.clone());
    let list = request(&local, "GET", "/api/hosts", None)?.json()?;
    let mut names = vec![list["this"].as_str().unwrap_or("this").to_owned()];
    let (tx, rx) = std::sync::mpsc::channel();
    for h in list["hosts"].as_array().into_iter().flatten() {
        let Some(name) = h["name"].as_str() else { continue };
        if h["transport"].as_str() == Some("provider") && h["status"].as_str().is_some_and(|s| s != "running") {
            each(name, Answer::Asleep);
            continue;
        }
        names.push(name.to_owned());
    }
    for (i, name) in names.iter().enumerate() {
        let (tx, socket, name, path) = (tx.clone(), socket.clone(), name.clone(), path.to_owned());
        std::thread::spawn(move || {
            let t = if i == 0 { Ok(Target::Socket(socket)) } else { target(socket, Some(&name)) };
            let v = t.and_then(|t| request(&t, "GET", &path, None)?.json());
            let _ = tx.send((name, v));
        });
    }
    drop(tx);
    let until = std::time::Instant::now() + wait;
    let mut left: Vec<String> = names;
    while !left.is_empty() {
        let Ok((name, v)) = rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) else { break };
        left.retain(|n| *n != name);
        each(&name, v.map(Answer::Json).unwrap_or_else(|e| Answer::Failed(format!("{e:#}"))));
    }
    for name in left {
        each(&name, Answer::Silent);
    }
    Ok(())
}
