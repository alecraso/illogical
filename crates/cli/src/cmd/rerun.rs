//! `illogical rerun`: type a pane's failed command again.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, here};
use anyhow::bail;
use serde_json::json;

#[derive(clap::Args)]
pub struct Args {
    pub pane: Option<Pane>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, .. } = ctx;
    let Args { pane } = args;
    let pane = here(pane)?;
    let v = request(&sock, "POST", "/api/attention/act", Some(&json!({ "action": "rerun", "pane": pane })))?;
    let v = v.json()?;
    if let Some(e) = v["results"][0]["error"].as_str() {
        bail!("{e}");
    }
    Ok(0)
}
