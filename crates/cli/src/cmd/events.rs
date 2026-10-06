//! `illogical events`: events as they happen, as NDJSON.

use super::Ctx;
use crate::http::{enc, request};
use crate::util::{Pane, duration};
use std::io::{Read, Write};

#[derive(clap::Args)]
pub struct Args {
    #[arg(short, long)]
    follow: bool,
    #[arg(long)]
    pane: Option<Pane>,
    /// Comma-separated: command_start, command_end, notify, attention, ...
    #[arg(long = "type")]
    types: Option<String>,
    /// Without --follow: how far back (e.g. 30m, 2h).
    #[arg(long)]
    since: Option<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, .. } = ctx;
    let Args { follow, pane, types, since } = args;
    let mut q = vec![];
    if follow {
        q.push("follow=1".to_owned());
    }
    if let Some(p) = pane {
        q.push(format!("pane={}", p.0));
    }
    if let Some(t) = types {
        q.push(format!("type={}", enc(&t)));
    }
    if let Some(s) = since {
        q.push(format!("since={}", duration(&s)?));
    }
    let mut res = request(&sock, "GET", &format!("/api/events?{}", q.join("&")), None)?.ok()?;
    let mut out = std::io::stdout().lock();
    let mut buf = [0u8; 16384];
    loop {
        let n = res.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        out.flush()?;
    }
    Ok(0)
}
