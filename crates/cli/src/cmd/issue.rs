//! `arugula issue`: an issue as a block, and the writes to it.

use super::Ctx;
use crate::http::{self, request};
use crate::util::{Pane, absolute, env_pane, loaded, print_json, split_of};
use anyhow::{Context, bail};
use serde_json::json;

/// Issues.
#[derive(clap::Subcommand)]
pub enum IssueCmd {
    /// Open a new issue in this directory's repository (or --repo).
    New {
        #[arg(long, short)]
        title: String,
        #[arg(long, short)]
        body: Option<String>,
        /// OWNER/REPO.
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        split: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Comment on an issue block's issue.
    Comment { block: Pane, body: String },
    /// Start an agent on it.
    ///
    /// A worktree and branch `iN-<slug>`, the agent there with the issue as
    /// its prompt, and the two in a tab.
    Agent {
        block: Pane,
        /// claude (the default), codex, fountain or acp.
        #[arg(long)]
        agent: Option<String>,
        /// More for its prompt.
        #[arg(long)]
        prompt: Option<String>,
        /// The clone to work in (default: where the issue was opened).
        #[arg(long)]
        dir: Option<String>,
    },
}

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    cmd: Option<IssueCmd>,
    /// The issue.
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
        Args { cmd: Some(IssueCmd::Comment { block, body }), .. } => {
            let mut args = json!({ "body": body });
            if http::agent() {
                args["agent"] = json!(true);
            }
            let v = request(&sock, "POST", &format!("/api/blocks/{}/call/comment", block.0), Some(&args))?.json()?;
            if json_out {
                print_json(&v);
            } else if let Some(d) = v["draft"].as_str() {
                println!("draft {d}: waits for a person to send it on %{}", block.0);
            } else {
                println!("{}", v["url"].as_str().or(v["said"].as_str()).unwrap_or("sent"));
            }
        }
        Args { cmd: Some(IssueCmd::Agent { block, agent, prompt, dir }), .. } => {
            let dir = dir.map(|d| absolute(&d)).transpose()?;
            let args = json!({ "agent": agent, "prompt_extra": prompt, "dir": dir });
            let v = request(&sock, "POST", &format!("/api/blocks/{}/call/agent", block.0), Some(&args))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!(
                    "%{} on branch {} in {}",
                    v["agent"],
                    v["branch"].as_str().unwrap_or("?"),
                    v["worktree"].as_str().unwrap_or("?")
                );
            }
        }
        Args { cmd: Some(IssueCmd::New { title, body, repo, split, session }), .. } => {
            let body = json!({
                "type": "forge",
                "config": { "issue": "new", "title": title, "body": body.unwrap_or_default(), "repo": repo, "dir": absolute(".")? },
                "split": split_of(split.as_deref())?,
                "session": session,
                "from_pane": env_pane(),
            });
            let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
            // A person's goes out now; an agent's waits as a draft.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let v = loop {
                let v = request(&sock, "GET", &format!("/api/blocks/{block}"), None)?.json()?;
                let st = &v["state"];
                let settled = st["number"].as_u64().is_some_and(|n| n > 0)
                    || st["new"]["agent"] == true
                    || !st["error"].is_null()
                    || !st["new"]["error"].is_null();
                if settled || std::time::Instant::now() > deadline {
                    break v;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            };
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let st = &v["state"];
            if let Some(e) = st["error"].as_str().or(st["new"]["error"].as_str()) {
                bail!("{e}");
            }
            match st["number"].as_u64().filter(|n| *n > 0) {
                Some(n) => println!(
                    "%{block} {}#{n} {}",
                    st["repo"].as_str().unwrap_or(""),
                    st["new"]["url"].as_str().unwrap_or("")
                ),
                None => println!("%{block}: a draft that waits for a person to send it"),
            }
        }
        Args { cmd: None, target, split, session } => {
            let target = target.context("which issue? a URL, OWNER/REPO#N, or N in this repository")?;
            let body = json!({
                "type": "forge",
                "config": { "issue": target, "dir": absolute(".")? },
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
