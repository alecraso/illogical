//! `arugula setup [claude|codex]` (#335): Getting started's "Use Claude
//! Code with Arugula". The daemon installs the agent's ACP adapter at
//! the version it pins (or updates an older one) and, for Claude Code,
//! adds Arugula's MCP server; this prints what changed, or why not and
//! what fixes it.

use serde_json::json;

use super::Ctx;
use crate::{http::request, util::print_json};

#[derive(clap::Args)]
pub struct Args {
    /// `claude` or `codex`.
    #[arg(default_value = "claude")]
    agent: String,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let agent = args.agent;
    if !json_out {
        eprintln!("Setting up {agent} (installing its adapter can take a minute)…");
    }
    let v = request(&sock, "POST", &format!("/api/setup/agents/{agent}"), Some(&json!({})))?.json()?;
    if json_out {
        print_json(&v);
    } else {
        for d in v["done"].as_array().into_iter().flatten() {
            println!("{}", d.as_str().unwrap_or_default());
        }
        if let Some(e) = v["error"].as_str() {
            eprintln!("{e}");
            if let Some(fix) = v["fix"].as_str() {
                eprintln!("  {fix}");
            }
            if let Some(url) = v["link"]["url"].as_str() {
                eprintln!("{}: {url}", v["link"]["label"].as_str().unwrap_or("See"));
            }
        }
    }
    Ok(if v["ok"] == true { 0 } else { 1 })
}
