//! `arugula attention`: what wants you, or tell Arugula a pane needs you.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, here, print_json};
use serde_json::{Value, json};

/// A hook's `message` (Claude Code's Notification: "Claude needs your
/// permission to use Bash"), when its JSON is on stdin.
fn hook_message() -> Option<String> {
    use std::io::{IsTerminal, Read};
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut input = String::new();
    std::io::stdin().take(1 << 20).read_to_string(&mut input).ok()?;
    let v: Value = serde_json::from_str(&input).ok()?;
    v["message"].as_str().map(str::to_owned).filter(|m| !m.is_empty())
}

#[derive(clap::Args)]
pub struct Args {
    /// needs-input, done, working or idle; none lists what wants you.
    state: Option<String>,
    #[arg(long)]
    pane: Option<Pane>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    match args {
        Args { state: None, .. } => {
            let v = request(&sock, "GET", "/api/attention", None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            for i in v.as_array().into_iter().flatten() {
                let r = &i["reason"];
                let bundle = r["bundle"].as_str().map(|b| format!("  [{b}]")).unwrap_or_default();
                println!(
                    "%{:<4} {:<7} {}{bundle}",
                    i["pane"],
                    r["kind"].as_str().unwrap_or(""),
                    r["headline"].as_str().unwrap_or("")
                );
            }
        }
        Args { state: Some(state), pane } => {
            let state = state.replace('-', "_");
            // Hooks (Claude Code's, say) run this in every terminal; outside an
            // Arugula pane there's nobody to tell, and that's fine.
            let Ok(pane) = here(pane) else { return Ok(0) };
            let path = format!("/api/panes/{pane}/attention");
            request(&sock, "POST", &path, Some(&json!({"state": state, "why": hook_message()})))?.json()?;
        }
    }
    Ok(0)
}
