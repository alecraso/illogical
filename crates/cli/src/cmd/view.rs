//! `illogical view`: a file in a file block, read-only and followed live.

use super::Ctx;
use crate::http::request;
use crate::util::{REMOTE, absolute, env_pane, file_line, print_json, split_of};
use anyhow::Context;
use serde_json::json;

#[derive(clap::Args)]
pub struct Args {
    spec: String,
    /// The line to mark and show.
    #[arg(long)]
    line: Option<u32>,
    /// Split a block instead of opening a tab: `right` for the one this
    /// runs in, or `%N`.
    #[arg(long)]
    split: Option<String>,
    #[arg(long)]
    session: Option<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { spec, line, split, session } = args;
    let remote = REMOTE.load(std::sync::atomic::Ordering::Relaxed);
    let (on, path) = match spec.split_once(':') {
        Some((on, p)) if on.starts_with('%') || (on.starts_with('m') && on[1..].parse::<u32>().is_ok()) => {
            (Some(on.to_owned()), p.to_owned())
        }
        _ => (None, spec.clone()),
    };
    let (path, at) = file_line(&path, on.is_none() && !remote);
    let (from, host) = match on.as_deref() {
        Some(p) if p.starts_with('%') => {
            (Some(p[1..].parse::<u32>().with_context(|| format!("not a pane: {p}"))?), None)
        }
        Some(m) => (None, Some(m[1..].parse::<u32>()?)),
        None => (env_pane(), None),
    };
    let path = if on.is_none() && !remote { absolute(&path)? } else { path };
    let body = json!({
        "type": "file",
        "config": { "path": path, "line": line.or(at) },
        "split": split_of(split.as_deref())?,
        "host": host,
        "local": on.is_none() && !remote,
        "session": session,
        "from_pane": from,
    });
    let v = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?;
    if json_out {
        print_json(&v);
    } else {
        println!("%{}", v["block"]);
    }
    Ok(0)
}
