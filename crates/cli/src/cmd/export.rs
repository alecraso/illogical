//! `arugula export`: a pane's history as an asciicast.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, here};
use anyhow::Context;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    pane: Option<Pane>,
    #[arg(long, default_value_t = true)]
    cast: bool,
    #[arg(short, long)]
    output: Option<PathBuf>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, .. } = ctx;
    let Args { pane, cast: _, output } = args;
    let pane = here(pane)?;
    let text = request(&sock, "GET", &format!("/api/panes/{pane}/export.cast"), None)?.ok()?.text()?;
    match output {
        Some(path) => std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?,
        None => print!("{text}"),
    }
    Ok(0)
}
