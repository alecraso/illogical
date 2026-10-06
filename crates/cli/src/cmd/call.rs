//! `illogical call`: one of a block's methods.

use super::Ctx;
use crate::http::{enc, request};
use crate::util::{Pane, print_json};
use anyhow::Context;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub struct Args {
    block: Pane,
    method: String,
    /// Arguments as JSON.
    args: Option<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, .. } = ctx;
    let Args { block, method, args } = args;
    let args: Value = match args {
        Some(a) => serde_json::from_str(&a).context("args must be JSON")?,
        None => json!({}),
    };
    let path = format!("/api/blocks/{}/call/{}", block.0, enc(&method));
    print_json(&request(&sock, "POST", &path, Some(&args))?.json()?);
    Ok(0)
}
