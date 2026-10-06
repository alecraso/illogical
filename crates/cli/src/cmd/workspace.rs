//! `illogical workspace`: a chant workspace as a block.

use super::Ctx;
use crate::http::request;
use crate::util::{absolute, env_pane, loaded, print_json, split_of};
use anyhow::bail;
use serde_json::json;

#[derive(clap::Args)]
pub struct Args {
    /// The workspace root, holding chant.workspace.json [default: here].
    dir: Option<String>,
    /// The environment whose gates and releases to read.
    #[arg(long, default_value = "local")]
    env: String,
    /// Split a block instead of opening a tab: `right` for the one this
    /// runs in, or `%N`.
    #[arg(long)]
    split: Option<String>,
    #[arg(long)]
    session: Option<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { dir, env, split, session } = args;
    let root = absolute(dir.as_deref().unwrap_or("."))?;
    let body = json!({
        "type": "workspace",
        "config": { "root": root, "env": env },
        "split": split_of(split.as_deref())?,
        "local": true,
        "session": session,
        "from_pane": env_pane(),
    });
    let block = request(&sock, "POST", "/api/blocks", Some(&body))?.json()?["block"].as_u64().unwrap_or(0);
    // Its first read: four chant processes, a second or two.
    let v = loaded(&sock, block)?;
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    println!("%{block}");
    let st = &v["state"];
    if let Some(e) = st["error"].as_str() {
        bail!("{e}");
    }
    let members = st["members"].as_array().map_or(0, Vec::len);
    let gates = st["gates"].as_array().cloned().unwrap_or_default();
    println!(
        "{}: {members} members, {} records, {} waiting at a gate",
        st["name"].as_str().unwrap_or("workspace"),
        st["records"].as_array().map_or(0, Vec::len),
        gates.len()
    );
    for g in gates {
        let s = |k: &str| g[k].as_str().unwrap_or("").to_owned();
        println!("  {}: {} waits at gate {}", s("member"), s("op"), s("gate"));
    }
    Ok(0)
}
