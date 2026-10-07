//! `arugula keys`: press named keys in a pane.

use super::Ctx;
use crate::http::request;
use crate::util::Pane;
use serde_json::json;

#[derive(clap::Args)]
pub struct Args {
    pane: Pane,
    #[arg(required = true)]
    keys: Vec<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, .. } = ctx;
    let Args { pane, keys } = args;
    request(&sock, "POST", &format!("/api/panes/{}/keys", pane.0), Some(&json!({"keys": keys})))?.json()?;
    Ok(0)
}
