//! `arugula diff`: what changed in a git repository, as a diff block.

use super::Ctx;
use crate::http::request;
use crate::util::{REMOTE, absolute, env_pane, loaded, print_json, split_of};
use anyhow::{Context, bail};
use serde_json::json;

#[derive(clap::Args)]
pub struct Args {
    /// `[%N] [REV_A [REV_B]]`.
    args: Vec<String>,
    /// The repository (any directory in it) [default: %N's directory,
    /// or this one].
    #[arg(long)]
    repo: Option<String>,
    /// Split a block instead of opening a tab: `right` for the one this
    /// runs in, or `%N`.
    #[arg(long)]
    split: Option<String>,
    #[arg(long)]
    session: Option<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { args, repo, split, session } = args;
    let (from, revs) = match args.first().and_then(|a| a.strip_prefix('%')) {
        Some(n) => (Some(n.parse::<u32>().with_context(|| format!("not a pane: %{n}"))?), &args[1..]),
        None => (None, &args[..]),
    };
    if revs.len() > 2 {
        bail!("at most two revisions: diff [%N] [REV_A [REV_B]]");
    }
    let remote = REMOTE.load(std::sync::atomic::Ordering::Relaxed);
    let repo = match repo {
        Some(r) if !remote && from.is_none() => Some(absolute(&r)?),
        Some(r) => Some(r),
        None if from.is_none() && !remote => Some(std::env::current_dir()?.display().to_string()),
        None => None,
    };
    let body = json!({
        "type": "diff",
        "config": { "repo": repo, "rev_a": revs.first(), "rev_b": revs.get(1) },
        "split": split_of(split.as_deref())?,
        "session": session,
        "from_pane": from.or_else(env_pane),
    });
    let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
    let v = loaded(&sock, block)?;
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    println!("%{block}");
    let st = &v["state"];
    if let Some(e) = st["error"].as_str() {
        bail!("{e}");
    }
    for f in st["files"].as_array().into_iter().flatten() {
        let counts = match (f["binary"].as_bool(), f["big"].as_bool()) {
            (Some(true), _) => "binary".to_owned(),
            (_, Some(true)) => "too big".to_owned(),
            _ => format!("+{} -{}", f["add"], f["del"]),
        };
        let path = match f["old"].as_str() {
            Some(old) => format!("{old} -> {}", f["path"].as_str().unwrap_or("")),
            None => f["path"].as_str().unwrap_or("").to_owned(),
        };
        println!("{:<10} {path}  {counts}", f["status"].as_str().unwrap_or(""));
    }
    let n = st["files"].as_array().map_or(0, Vec::len);
    println!(
        "{n} file{} changed, +{} -{} ({})",
        if n == 1 { "" } else { "s" },
        st["add"],
        st["del"],
        st["against"].as_str().unwrap_or("")
    );
    Ok(0)
}
