//! `illogical send`: type text into a pane, or prompt the agent there and wait.

use super::Ctx;
use crate::http::request;
use crate::util::Pane;
use serde_json::{Value, json};
use std::io::Read;

/// What `send --wait` came to, for people, and its exit code.
fn prompted(pane: u32, r: &Value) -> (String, i32) {
    let q = |r: &Value| r["question"].as_str().unwrap_or("a question").to_owned();
    match r["result"].as_str().unwrap_or_default() {
        "done" => (format!("%{pane} finished its turn"), 0),
        "needs_input" => (format!("%{pane} asks: {}", q(r)), 2),
        "blocked" => (
            format!("%{pane} was already waiting on someone ({}); nothing was typed (--answering to answer it)", q(r)),
            2,
        ),
        "stalled" => {
            let screen = r["screen"].as_str().unwrap_or_default();
            (format!("%{pane} stalled: {}\n{screen}", r["why"].as_str().unwrap_or_default()), 3)
        }
        "still_running" => (format!("%{pane} is still working (illogical wait %{pane} --idle)"), 4),
        other => (format!("%{pane}: {other}"), 1),
    }
}

#[derive(clap::Args)]
pub struct Args {
    pane: Pane,
    #[arg(required = true)]
    text: Vec<String>,
    /// Press Enter afterwards.
    #[arg(short, long)]
    enter: bool,
    /// Prompt the agent there and wait for its turn.
    #[arg(long)]
    wait: bool,
    /// With --wait: it's waiting on a question and this answers it.
    #[arg(long, requires = "wait")]
    answering: bool,
    /// With --wait: seconds before giving up waiting (default 100).
    #[arg(long, requires = "wait")]
    timeout: Option<f64>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, .. } = ctx;
    let Args { pane, text, enter, wait, answering, timeout } = args;
    let mut text = text.join(" ");
    if text == "-" {
        text.clear();
        std::io::stdin().read_to_string(&mut text)?;
    }
    if wait {
        let body = json!({"text": text, "answering": answering, "timeout": timeout});
        let r = request(&sock, "POST", &format!("/api/panes/{}/prompt", pane.0), Some(&body))?.json()?;
        let (line, code) = prompted(pane.0, &r);
        println!("{line}");
        return Ok(code);
    }
    request(&sock, "POST", &format!("/api/panes/{}/send", pane.0), Some(&json!({"text": text, "enter": enter})))?
        .json()?;
    Ok(0)
}
