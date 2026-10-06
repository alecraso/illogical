//! `illogical log`: a pane's commands and who ran each.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, here, print_json, time};

#[derive(clap::Args)]
pub struct Args {
    pane: Option<Pane>,
    #[arg(long)]
    who: bool,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { pane, who } = args;
    let id = here(pane)?;
    if !who {
        let v = request(&sock, "GET", &format!("/api/history?pane={id}&limit=1000"), None)?.json()?;
        if json_out {
            print_json(&v);
            return Ok(0);
        }
        for c in v.as_array().into_iter().flatten() {
            println!(
                "{:>8}  {:<16} {}",
                time(c["started_ms"].as_u64().unwrap_or(0)),
                c["by"].as_str().unwrap_or("-"),
                c["text"].as_str().unwrap_or("?")
            );
        }
        return Ok(0);
    }
    let v = request(&sock, "GET", &format!("/api/panes/{id}/drivers"), None)?.json()?;
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    for d in v.as_array().into_iter().flatten() {
        println!(
            "{:>8}  @{:<10} {}",
            time(d["at_ms"].as_u64().unwrap_or(0)),
            d["offset"],
            d["who"].as_str().unwrap_or("")
        );
    }
    Ok(0)
}
