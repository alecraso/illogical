//! `--host` and `illogical hosts`: other daemons, from the home daemon's
//! list. A name is looked up in that list (over the local socket); a URL is
//! used as it is. Either way the commands then talk to that daemon
//! directly, over HTTP(S), where its usual access checks apply.

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
        /// Its URL(s), best first: `https://box.tailnet.ts.net`.
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
}

/// Where commands go: the local socket, or the daemon `--host` names.
pub fn target(socket: PathBuf, host: Option<&str>) -> anyhow::Result<Target> {
    let local = Target::Socket(socket);
    let Some(host) = host else { return Ok(local) };
    if host.contains("://") {
        return Ok(Target::Url(Url::parse(host)?));
    }
    let list = request(&local, "GET", "/api/hosts", None)
        .and_then(|r| r.json())
        .context("looking up --host in the local daemon's host list")?;
    if list["this"].as_str() == Some(host) {
        return Ok(local);
    }
    let entry = list["hosts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|h| h["name"].as_str() == Some(host))
        .with_context(|| format!("no host {host} (see `illogical hosts`)"))?;
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
            let v = request(target, "GET", "/api/hosts", None)?.json()?;
            if !json_out {
                println!("{:<20} {:<44} (this daemon)", v["this"].as_str().unwrap_or("?"), "");
                for h in v["hosts"].as_array().into_iter().flatten() {
                    let urls: Vec<&str> =
                        h["urls"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                    let seen =
                        h["last_seen_ms"].as_u64().map(|t| format!("seen {}", ago(t))).unwrap_or("never seen".into());
                    println!("{:<20} {:<44} {seen}", h["name"].as_str().unwrap_or("?"), urls.join(" "));
                }
                return Ok(());
            }
            v
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
    };
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    }
    Ok(())
}
