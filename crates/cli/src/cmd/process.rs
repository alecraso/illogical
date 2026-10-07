//! `arugula process`: a pane's foreground process.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, here, print_json};
use serde_json::Value;

#[derive(clap::Args)]
pub struct Args {
    pub pane: Option<Pane>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { pane } = args;
    let v = request(&sock, "GET", &format!("/api/panes/{}/process", here(pane)?), None)?.json()?;
    if json_out {
        print_json(&v);
    } else {
        let argv: Vec<&str> = v["argv"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        println!("{} {}  (cwd {})", v["foreground"], argv.join(" "), v["cwd"].as_str().unwrap_or("?"));
    }
    Ok(0)
}
