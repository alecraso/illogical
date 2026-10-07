//! `arugula claude`: Claude Code conversations on this machine.

use super::Ctx;
use crate::http::{enc, request};
use crate::{
    hosts,
    util::{Pane, env_pane, print_json, time},
};
use anyhow::Context;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Subcommand)]
pub enum ClaudeCmd {
    /// List them, newest first.
    Ls {
        /// Everything: `claude -p` and SDK runs, archived ones, ones whose
        /// folder is gone.
        #[arg(long)]
        all: bool,
        /// Only ones open in a terminal, the desktop app or a pane now.
        #[arg(long)]
        live: bool,
        /// Only ones under this folder.
        #[arg(long)]
        cwd: Option<String>,
        /// How many [default: 30].
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
        /// Words in the title, prompts or folder.
        words: Vec<String>,
    },
    /// Show one as an agent block; prints the block.
    ///
    /// Stopped, its transcript as it grows. The block that has it already, if
    /// one does.
    Open {
        /// Its id, or the start of it.
        id: String,
        #[arg(long)]
        session: Option<String>,
        /// Split this block instead of opening a tab.
        #[arg(long)]
        split: Option<Pane>,
    },
}

pub fn run(cmd: ClaudeCmd, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    match cmd {
        ClaudeCmd::Ls { all, live, cwd, limit, words } => {
            let path = conversations_path(all, live, cwd, limit, &words);
            let v = request(&sock, "GET", &path, None)?.json()?;
            if json_out {
                print_json(&v);
                return Ok(0);
            }
            let home = std::env::var("HOME").ok();
            for c in v["conversations"].as_array().into_iter().flatten() {
                print_conversation(c, home.as_deref());
            }
        }
        ClaudeCmd::Open { id, session, split } => {
            let body = json!({ "id": id, "session": session, "split": split.map(|p| p.0), "from_pane": env_pane() });
            let v = request(&sock, "POST", "/api/conversations/open", Some(&body))?.json()?;
            if json_out {
                print_json(&v);
            } else {
                println!("%{}", v["block"].as_u64().context("no block in the answer")?);
            }
        }
    }
    Ok(0)
}

/// `GET /api/conversations` with `claude ls`'s filters.
pub fn conversations_path(all: bool, live: bool, cwd: Option<String>, limit: usize, words: &[String]) -> String {
    let mut q = vec![format!("limit={limit}")];
    if all {
        q.push("all=1".into());
    }
    if live {
        q.push("live=1".into());
    }
    if let Some(d) = cwd {
        let d = std::fs::canonicalize(&d)
            .or_else(|_| std::env::current_dir().map(|h| h.join(&d)))
            .map(|p| p.display().to_string())
            .unwrap_or(d);
        q.push(format!("cwd={}", enc(&d)));
    }
    if !words.is_empty() {
        q.push(format!("q={}", enc(&words.join(" "))));
    }
    format!("/api/conversations?{}", q.join("&"))
}

/// One line of `claude ls`; `~` for `home`.
fn print_conversation(c: &Value, home: Option<&str>) {
    let s = |k: &str| c[k].as_str().unwrap_or("");
    let mut cwd = s("cwd").to_owned();
    if let Some(home) = home.filter(|h| !h.is_empty())
        && cwd.starts_with(home)
    {
        cwd = format!("~{}", &cwd[home.len()..]);
    }
    let src = match s("source") {
        "terminal" => "term",
        "desktop" => "desk",
        _ => "other",
    };
    let mut tags = vec![];
    if let Some(p) = c["live"]["place"].as_str() {
        tags.push(p.to_owned());
    }
    if let Some(b) = c["block"].as_u64() {
        tags.push(format!("block %{b}"));
    }
    let tags = if tags.is_empty() { String::new() } else { format!("  [{}]", tags.join(", ")) };
    let id: String = s("id").chars().take(8).collect();
    let title: String = s("title").chars().take(60).collect();
    let when = time(c["updated_ms"].as_u64().unwrap_or(0));
    println!("{id}  {src:<5} {when:>8}  {title}  {cwd}{tags}");
}

/// `claude ls --host all` (#78): each host's conversations under its name,
/// as it answers; one that's asleep or doesn't answer within 5 s says so.
/// `--json`: `{hosts: [{host, conversations | error | asleep}]}` once all
/// are in.
pub fn claude_ls_all(socket: PathBuf, path: String, json_out: bool) -> anyhow::Result<i32> {
    let mut out = vec![];
    let mut first = true;
    hosts::each(socket, &path, std::time::Duration::from_secs(5), |host, a| {
        if json_out {
            out.push(match a {
                hosts::Answer::Json(v) => json!({ "host": host, "conversations": v["conversations"] }),
                hosts::Answer::Asleep => json!({ "host": host, "asleep": true }),
                hosts::Answer::Failed(e) => json!({ "host": host, "error": e }),
                hosts::Answer::Silent => json!({ "host": host, "error": "no answer in 5 s" }),
            });
            return;
        }
        if !first {
            println!();
        }
        first = false;
        match a {
            hosts::Answer::Json(v) => {
                let list = v["conversations"].as_array().cloned().unwrap_or_default();
                println!("{host} ({})", list.len());
                // Its home, from its paths: another machine's isn't ours.
                let home = list.iter().find_map(|c| home_of(c["cwd"].as_str()?));
                for c in &list {
                    print_conversation(c, home);
                }
            }
            hosts::Answer::Asleep => println!("{host}: asleep (not woken to ask)"),
            hosts::Answer::Failed(e) => println!("{host}: {e}"),
            hosts::Answer::Silent => println!("{host}: no answer in 5 s"),
        }
    })?;
    if json_out {
        print_json(&json!({ "hosts": out }));
    }
    Ok(0)
}

/// `/home/me` or `/Users/me` at the start of a path.
fn home_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/home/").or_else(|| path.strip_prefix("/Users/"))?;
    let end = path.len() - rest.len() + rest.find('/').unwrap_or(rest.len());
    Some(&path[..end])
}
