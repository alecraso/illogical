//! `arugula fountain`: Fountain agents as a catalog block, and this machine as the runner.

use super::Ctx;
use crate::http::{enc, request};
use crate::util::{env_pane, loaded, print_json, split_of};
use anyhow::bail;
use serde_json::json;

/// Fountain: the catalog and this machine as the runner.
#[derive(clap::Subcommand)]
pub enum FountainCmd {
    /// This machine as the account's Fountain runner.
    ///
    /// `install`, `status`, `adopt` (the root half is
    /// `scripts/fountain-runner-setup.sh`).
    Runner {
        #[command(subcommand)]
        cmd: crate::fountain_runner::RunnerCmd,
    },
    /// List your agents here, one line each, without opening a block.
    Agents {
        /// Words to look for (names, descriptions, skills, MCP servers).
        query: Option<String>,
        /// agent-specs, hand or app.
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
}

#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Option<FountainCmd>,
    /// What the block shows: catalog (the agents) or runner (this host
    /// as the Fountain runner, its status and its sandboxes).
    #[arg(long, value_parser = ["catalog", "runner"], default_value = "catalog")]
    view: String,
    /// Start with this search (names, descriptions, skills, MCP servers).
    #[arg(long, short)]
    query: Option<String>,
    /// Start with this source: agent-specs, hand or app.
    #[arg(long)]
    source: Option<String>,
    /// The credentials profile (default: FOUNTAIN_PROFILE, else default).
    #[arg(long)]
    profile: Option<String>,
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
        Args { cmd: Some(FountainCmd::Agents { query, source, profile }), .. } => {
            let mut q = vec![];
            for (k, v) in [("query", query), ("source", source), ("profile", profile)] {
                if let Some(v) = v {
                    q.push(format!("{k}={}", enc(&v)));
                }
            }
            let path = if q.is_empty() {
                "/api/fountain/agents".into()
            } else {
                format!("/api/fountain/agents?{}", q.join("&"))
            };
            let v = request(&sock, "GET", &path, None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let rows = v["agents"].as_array().cloned().unwrap_or_default();
            println!("{} of {} agents on {}", rows.len(), v["total"], v["base_url"].as_str().unwrap_or("Fountain"));
            if let Some(n) = v["unreadable"].as_u64().filter(|n| *n > 0) {
                println!("({n} couldn't be read: Fountain sent something this Arugula doesn't understand)");
            }
            for r in rows {
                let s = |k: &str| r[k].as_str().unwrap_or("").to_owned();
                let list = |k: &str| {
                    r[k].as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", "))
                        .unwrap_or_default()
                };
                let mut line = format!("{}  [{}] {}", s("name"), s("runtime"), s("source"));
                if !list("skills").is_empty() {
                    line.push_str(&format!("  skills: {}", list("skills")));
                }
                if !list("mcp").is_empty() {
                    line.push_str(&format!("  mcp: {}", list("mcp")));
                }
                println!("{line}");
            }
        }
        Args { cmd: None, view, query, source, profile, split, session } => {
            if view == "runner" && (query.is_some() || source.is_some()) {
                bail!("--query and --source filter the catalog, not the runner view");
            }
            let mut filter = json!({});
            if let Some(q) = query {
                filter["query"] = json!(q);
            }
            if let Some(s) = source {
                filter["sources"] = json!(s.split(',').map(str::trim).collect::<Vec<_>>());
            }
            let body = json!({
                "type": "fountain",
                "config": { "profile": profile, "view": view, "filter": filter },
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
        Args { cmd: Some(FountainCmd::Runner { .. }), .. } => unreachable!("handled first"),
    }
    Ok(0)
}
