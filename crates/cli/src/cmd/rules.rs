//! `arugula rules`: the standing permission rules for agent blocks.

use super::Ctx;
use crate::http::request;
use crate::util::print_json;
use serde_json::Value;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long, conflicts_with = "forget_all")]
    forget: Option<usize>,
    #[arg(long)]
    forget_all: bool,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { forget, forget_all } = args;
    if forget_all {
        request(&sock, "DELETE", "/api/rules", None)?.json()?;
    } else if let Some(i) = forget {
        request(&sock, "DELETE", &format!("/api/rules/{i}"), None)?.json()?;
    }
    let v: Value = request(&sock, "GET", "/api/rules", None)?.json()?;
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    let rules = v["rules"].as_array().cloned().unwrap_or_default();
    if rules.is_empty() {
        println!("No standing rules: agent blocks ask (\"Always\" for a directory or everywhere makes one)");
    }
    for r in rules {
        println!("{:>3}  {}", r["index"], r["text"].as_str().unwrap_or(""));
    }
    Ok(0)
}
