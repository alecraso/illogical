//! `arugula history`: commands run in any pane, and what else happened there.

use super::Ctx;
use crate::http::{enc, request};
use crate::util::{Pane, duration, print_json, time};

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    pane: Option<Pane>,
    /// Only commands that failed.
    #[arg(long)]
    failed: bool,
    /// Only this kind: command (ran in a shell), answer (an answer or
    /// approval) or agent (an agent block's steps that aren't commands).
    #[arg(long, value_parser = ["command", "answer", "agent"])]
    kind: Option<String>,
    /// e.g. 30m, 2h, 7d.
    #[arg(long)]
    since: Option<String>,
    /// Only commands run in this directory (or below).
    #[arg(long)]
    cwd: Option<String>,
    /// Only commands matching this regular expression.
    #[arg(long = "match")]
    matching: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
    /// Another host's history, as synced here (`all`: every host's).
    #[arg(long, value_name = "HOST")]
    synced: Option<String>,
}

pub fn run(args: Args, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, gone, .. } = ctx;
    let synced_q = |flag| super::synced_q(flag, &gone);
    let Args { pane, failed, kind, since, cwd, matching, limit, synced } = args;
    let mut q = vec![format!("limit={limit}")];
    q.extend(synced_q(synced));
    if let Some(p) = pane {
        q.push(format!("pane={}", p.0));
    }
    if failed {
        q.push("failed=1".into());
    }
    if let Some(k) = kind {
        q.push(format!("kind={k}"));
    }
    if let Some(s) = since {
        q.push(format!("since={}", duration(&s)?));
    }
    if let Some(c) = cwd {
        q.push(format!("cwd={}", enc(&c)));
    }
    if let Some(m) = matching {
        q.push(format!("match={}", enc(&m)));
    }
    let v = request(&sock, "GET", &format!("/api/history?{}", q.join("&")), None)?.json()?;
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    for c in v.as_array().into_iter().flatten() {
        let exit = match c["exit"].as_i64() {
            Some(0) => "  ".to_owned(),
            Some(e) => format!("{e:>2}"),
            None => " …".to_owned(),
        };
        let closed = if c["open"].as_bool() == Some(false) { " (closed)" } else { "" };
        let host = c["host"].as_str().map(|h| format!("{h}:")).unwrap_or_default();
        let by = c["by"].as_str().map(|b| format!("  by {b}")).unwrap_or_default();
        let by = match c["kind"].as_str() {
            Some(k @ ("answer" | "agent")) => format!("  ({k}){by}"),
            _ => by,
        };
        println!(
            "{exit}  {host}%{:<4} {:>8}  {}{closed}   [{}]{by}",
            c["pane"],
            time(c["started_ms"].as_u64().unwrap_or(0)),
            c["text"].as_str().unwrap_or("?"),
            c["cwd"].as_str().unwrap_or("")
        );
    }
    Ok(0)
}
