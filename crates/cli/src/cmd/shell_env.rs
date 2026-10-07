//! `arugula shell-env`: the shell environment blocks that run your tools get.

use super::Ctx;
use crate::http::request;
use crate::util::print_json;
use serde_json::Value;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    refresh: bool,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let Args { refresh } = args;
    let v = match refresh {
        true => request(&sock, "POST", "/api/hosts/self/shell-env/refresh", None)?.json()?,
        false => request(&sock, "GET", "/api/hosts/self/shell-env", None)?.json()?,
    };
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    let shell = v["shell"].as_str().unwrap_or("?");
    match v["error"].as_str() {
        Some(e) => println!("{shell}: {e}; blocks get the daemon's environment"),
        None => {
            println!("{shell} ({} ms)", v["ms"]);
            match v["path"].as_str() {
                Some(p) => println!("PATH={p}"),
                None => println!("PATH is the daemon's"),
            }
            let vars: Vec<&str> = v["vars"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            println!("also sets: {}", vars.into_iter().filter(|k| *k != "PATH").collect::<Vec<_>>().join(" "));
        }
    }
    Ok(0)
}
