//! `arugula invite`: bring someone into a session.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, env_pane, print_json};
use anyhow::{Context, bail};
use serde_json::json;
use std::io::Write;

#[derive(clap::Args)]
pub struct Args {
    who: String,
    /// The session (name or $N) [default: this pane's].
    #[arg(long)]
    session: Option<String>,
    /// Where it opens [default: this pane, else the session's first].
    #[arg(long)]
    pane: Option<Pane>,
    /// viewer or editor.
    #[arg(long, default_value = "viewer")]
    role: String,
    /// Share what's already there too (default: from now on).
    #[arg(long)]
    history: bool,
    /// An editor may also type in the pane on this machine for so many
    /// minutes (1 to 1440).
    #[arg(long, value_name = "MINUTES")]
    drive: Option<u32>,
    /// What the notification says.
    #[arg(long)]
    note: Option<String>,
    /// For account:ID that this machine can't vouch for: their root
    /// device. Its fingerprint is printed for you to check with them.
    #[arg(long, value_name = "DEVICE")]
    root: Option<String>,
    /// With --root: you've checked the fingerprint.
    #[arg(long)]
    yes: bool,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { who, session, pane, role, history, drive, note, root, yes } = args;
    if !matches!(role.as_str(), "viewer" | "editor") {
        bail!("an invite makes a viewer or an editor");
    }
    // Someone's root as typed is only as good as its fingerprint,
    // checked with them: before anything is sent.
    if let Some(r) = &root
        && !yes
        && !confirm_root(r)?
    {
        return Ok(1);
    }
    let panes = request(&sock, "GET", "/api/panes", None)?.json()?;
    let pane = pane.map(|p| p.0).or_else(|| if session.is_none() { env_pane() } else { None });
    let session =
        match (&session, pane) {
            (Some(want), _) => {
                let id = want.strip_prefix('$').and_then(|n| n.parse::<u64>().ok());
                panes.as_array().into_iter().flatten().find(|p| {
                    Some(p["session"].as_u64().unwrap_or(0)) == id || p["session_name"].as_str() == Some(want)
                })
            }
            (None, Some(n)) => panes.as_array().into_iter().flatten().find(|p| p["id"] == n),
            (None, None) => bail!("which session? (give --session, or run this inside an Arugula pane)"),
        }
        .and_then(|p| p["session"].as_u64())
        .with_context(|| format!("no session {}", session.as_deref().unwrap_or("for that pane")))?;
    let body = json!({
        "session": session, "who": who, "role": role, "pane": pane, "history": history,
        "drive_minutes": drive, "note": note, "root": root,
    });
    let v = request(&sock, "POST", "/api/invite", Some(&body))?.json()?;
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    let g = &v["grant"];
    let name = g["name"].as_str().unwrap_or(&who);
    let what = if g["granted"] == true { "shared with" } else { "already shared with" };
    println!("{what} {name} as {} (opens at %{})", g["role"].as_str().unwrap_or(""), v["pane"]);
    match (v["drive"].as_bool(), drive) {
        (Some(true), Some(m)) => println!("they may type in %{} for {m} minutes", v["pane"]),
        (Some(false), _) => {
            println!("--drive changed nothing: they drive by their role here, or the pane isn't on this machine")
        }
        _ => {}
    }
    match (v["delivery"].as_str().unwrap_or(""), v["reason"].as_str()) {
        ("sent", _) => println!("notified"),
        (d, Some(why)) => println!("{d}: {why}"),
        (d, None) => println!("{d}"),
    }
    Ok(0)
}

/// What a device id looks like to people (as the web shows it).
fn fingerprint(id: &str) -> String {
    id.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("-")
}

/// `invite --root`: print the fingerprint, and on a terminal ask whether
/// it's theirs; anywhere else stop, for `--yes` once checked.
fn confirm_root(root: &str) -> anyhow::Result<bool> {
    use std::io::IsTerminal;
    eprintln!("Their first device's fingerprint: {}", fingerprint(root));
    eprintln!("Check it with them before going on: control could hand over a device of its own.");
    if !std::io::stdin().is_terminal() {
        eprintln!("Not inviting yet: run again with --yes once you've checked it.");
        return Ok(false);
    }
    eprint!("Is it theirs? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    if matches!(line.trim(), "y" | "Y" | "yes") {
        return Ok(true);
    }
    eprintln!("Not inviting.");
    Ok(false)
}
