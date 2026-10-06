//! `illogical capture`: what a pane shows.

use super::Ctx;
use crate::http::request;
use crate::util::{Pane, here};

#[derive(clap::Args)]
pub struct Args {
    pane: Option<Pane>,
    #[arg(long, group = "format")]
    ansi: bool,
    #[arg(long, group = "format")]
    html: bool,
    #[arg(long, group = "scope")]
    scrollback: bool,
    #[arg(long, group = "scope")]
    last_command: bool,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, .. } = ctx;
    let Args { pane, ansi, html, scrollback, last_command } = args;
    let format = if ansi {
        "ansi"
    } else if html {
        "html"
    } else {
        "text"
    };
    let scope = if scrollback {
        "scrollback"
    } else if last_command {
        "last-command"
    } else {
        "screen"
    };
    let path = format!("/api/panes/{}/capture?format={format}&scope={scope}", here(pane)?);
    let text = request(&sock, "GET", &path, None)?.ok()?.text()?;
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    Ok(0)
}
