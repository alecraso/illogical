//! `arugula open`: a browser block on a port or a web page.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, env_pane, here, print_json};
use anyhow::Context;
use serde_json::json;

#[derive(clap::Args)]
pub struct Args {
    /// `:PORT[/path]`, or a URL.
    target: String,
    /// Split a block instead of opening a tab: `right` for the one this
    /// runs in, or `%N`.
    #[arg(long)]
    split: Option<String>,
    /// The machine whose port it is: `mN`, or `local` for this host
    /// [default: this host]. (`--host` is another daemon.)
    #[arg(long)]
    pub machine: Option<String>,
    #[arg(long)]
    session: Option<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { target, split, machine, session } = args;
    let config = match target.strip_prefix(':') {
        Some(rest) => {
            let (port, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            let port: u16 = port.parse().with_context(|| format!("not a port: {target}"))?;
            json!({ "port": port, "path": if path.is_empty() { "/" } else { path } })
        }
        None => json!({ "url": target }),
    };
    let split = match split.as_deref() {
        None => None,
        Some("right") => Some(here(None)?),
        Some(p) => Some(p.parse::<Pane>().map_err(anyhow::Error::msg)?.0),
    };
    let local = machine.as_deref() == Some("local");
    let host = match machine.filter(|_| !local) {
        Some(m) => Some(m.trim_start_matches('m').parse::<u32>().with_context(|| format!("not a machine: {m}"))?),
        None => None,
    };
    let body = json!({
        "type": "browser",
        "config": config,
        "split": split,
        "host": host,
        "local": local,
        "session": session,
        "from_pane": env_pane(),
    });
    let v = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?;
    if json_out {
        print_json(&v);
    } else {
        println!("%{}", v["block"]);
    }
    Ok(0)
}
