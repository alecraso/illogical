//! `arugula pr`: a pull request as a block, and the writes to it.

use super::Ctx;
use crate::http::{self, request};
use crate::util::{Pane, absolute, env_pane, loaded, print_json, split_of};
use anyhow::{Context, bail};
use serde_json::json;

/// Writes to a PR block.
#[derive(clap::Subcommand)]
pub enum PrCmd {
    /// Comment on it.
    Comment { block: Pane, body: String },
    /// Review it: approve, request_changes or comment.
    Review { block: Pane, event: String, body: Option<String> },
    /// Merge it.
    ///
    /// merge, rebase, rebase-merge, squash, fast-forward-only; GitHub: merge,
    /// squash or rebase.
    Merge {
        block: Pane,
        #[arg(long)]
        style: Option<String>,
    },
    /// Rerun its failed checks.
    ///
    /// GitHub: each red workflow run's failed jobs; Forgejo has no API for it.
    Rerun { block: Pane },
}

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    cmd: Option<PrCmd>,
    /// The pull request.
    target: Option<String>,
    /// Split a block instead of opening a tab: `right` for the one this
    /// runs in, or `%N`.
    #[arg(long)]
    split: Option<String>,
    #[arg(long)]
    session: Option<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    match args {
        Args { cmd: Some(cmd), .. } => {
            let (block, method, mut args) = match cmd {
                PrCmd::Comment { block, body } => (block, "comment", json!({ "body": body })),
                PrCmd::Review { block, event, body } => (block, "review", json!({ "event": event, "body": body })),
                PrCmd::Merge { block, style } => (block, "merge", json!({ "style": style })),
                PrCmd::Rerun { block } => (block, "rerun_checks", json!({})),
            };
            if http::agent() {
                args["agent"] = json!(true);
            }
            let v = request(&sock, "POST", &format!("/api/blocks/{}/call/{method}", block.0), Some(&args))?.json()?;
            if json_out {
                print_json(&v);
            } else if let Some(d) = v["draft"].as_str() {
                println!("draft {d}: waits for a person to send it on %{}", block.0);
            } else {
                println!("{}", v["url"].as_str().or(v["said"].as_str()).unwrap_or("sent"));
            }
        }
        Args { cmd: None, target, split, session } => {
            let target = target.context("which pull request? a URL, OWNER/REPO#N, or N in this repository")?;
            let body = json!({
                "type": "forge",
                "config": { "pr": target, "dir": absolute(".")? },
                "split": split_of(split.as_deref())?,
                "session": session,
                "from_pane": env_pane(),
            });
            let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
            let v = loaded(&sock, block)?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            println!("%{block}");
            if let Some(e) = v["state"]["error"].as_str() {
                bail!("{e}");
            }
            print!("{}", request(&sock, "GET", &format!("/api/panes/{block}/capture"), None)?.text()?);
        }
    }
    Ok(0)
}
