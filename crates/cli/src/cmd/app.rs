//! `arugula app`: a studio app's box as a block, or the list of apps.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, env_pane, here, print_json};
use serde_json::json;

#[derive(clap::Args)]
pub struct Args {
    /// The app's name in studio.
    name: Option<String>,
    /// Split a block instead of opening a tab: `right` for the one this
    /// runs in, or `%N`.
    #[arg(long)]
    split: Option<String>,
    #[arg(long)]
    session: Option<String>,
    /// Follow the box with the app's follower link (`arugula studio
    /// follower`), so hud is told who answered. The default whenever a
    /// link is kept for the app.
    #[arg(long)]
    follower: bool,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    match args {
        Args { name: None, .. } => {
            let v = request(&sock, "GET", "/api/studio/apps", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for a in v["apps"].as_array().into_iter().flatten() {
                let s = |k: &str| a[k].as_str().unwrap_or("");
                let blocks: Vec<String> = a["blocks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b.as_u64())
                    .map(|b| format!("%{b}"))
                    .collect();
                let open = if blocks.is_empty() { String::new() } else { format!("  [{}]", blocks.join(", ")) };
                let status = if s("status").is_empty() { String::new() } else { format!(" ({})", s("status")) };
                println!("{:<24} {}{status}{open}", s("name"), s("url"));
            }
        }
        Args { name: Some(name), split, session, follower } => {
            let split = match split.as_deref() {
                None => None,
                Some("right") => Some(here(None)?),
                Some(p) => Some(p.parse::<Pane>().map_err(anyhow::Error::msg)?.0),
            };
            let body = json!({
                "type": "app",
                "config": if follower { serde_json::json!({ "app": name, "follower": true }) } else { serde_json::json!({ "app": name }) },
                "split": split,
                "session": session,
                "from_pane": env_pane(),
            });
            let v = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("%{}", v["block"]);
            }
        }
    }
    Ok(0)
}
