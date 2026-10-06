//! `illogical close`: panes, and what runs in them.

use super::Ctx;
use crate::http::request;
use crate::{
    hosts,
    util::{Pane, REMOTE},
};

#[derive(clap::Args)]
pub struct Args {
    #[arg(required = true)]
    panes: Vec<Pane>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, local_sock, .. } = ctx;
    let Args { panes } = args;
    for p in panes {
        // A remote pane (#17): close it on its host too. If that
        // can't be reached, it stays in the host's own layout.
        let remote = match request(&sock, "GET", &format!("/api/blocks/{}", p.0), None).and_then(|r| r.json()) {
            Ok(d) if d["info"]["type"] == "remote" && !REMOTE.load(std::sync::atomic::Ordering::Relaxed) => {
                Some((d["state"]["host"].as_str().unwrap_or_default().to_owned(), d["state"]["pane"].clone()))
            }
            _ => None,
        };
        if let Some((host, pane)) = remote {
            let closed = hosts::target(local_sock.clone(), Some(&host))
                .and_then(|t| request(&t, "POST", &format!("/api/panes/{pane}/close"), None)?.json());
            if let Err(e) = closed {
                eprintln!("illogical: %{pane} on {host} stays open there: {e:#}");
            }
        }
        request(&sock, "POST", &format!("/api/panes/{}/close", p.0), None)?.json()?;
    }
    Ok(0)
}
