//! `arugula ide`: arugulad as Claude Code's IDE.

use super::Ctx;
use crate::http::request;
use crate::util::print_json;
use serde_json::{Value, json};

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    diffs: Option<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { diffs } = args;
    let v = match diffs {
        Some(d) => {
            request(&sock, "PUT", "/api/ide", Some(&json!({ "diffs": d })))?.json()?;
            request(&sock, "GET", "/api/ide", None)?.json()?
        }
        None => request(&sock, "GET", "/api/ide", None)?.json()?,
    };
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    if v["on"] != true {
        println!("arugulad isn't Claude Code's IDE (--no-claude-ide)");
        return Ok(0);
    }
    println!("Claude Code's IDE on port {} ({})", v["port"], v["lock_dir"].as_str().unwrap_or(""));
    println!("diffs go to: {}", v["diffs"].as_str().unwrap_or(""));
    for o in v["others"].as_array().into_iter().flatten() {
        let alive = if o["alive"] == true { "" } else { "  (gone)" };
        let folders: Vec<&str> = o["folders"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        println!("  also: {:<24} port {}  {}{alive}", o["name"].as_str().unwrap_or("?"), o["port"], folders.join(", "));
    }
    Ok(0)
}
