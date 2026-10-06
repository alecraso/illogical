//! `illogical studio`: your studio's token and follower links.

use super::Ctx;
use crate::http::{enc, request};
use crate::{term, util::print_json};
use anyhow::bail;
use serde_json::json;
use std::io::Write;

#[derive(clap::Subcommand)]
pub enum StudioCmd {
    /// Keep a studio token in the daemon.
    ///
    /// Mode 0600, never sent to a client. The token is read from stdin, or
    /// asked for.
    Login {
        /// The studio, e.g. `https://studio.example`.
        url: String,
    },
    /// Forget the token, and every follower link.
    Logout,
    /// Keep an app's follower link, or drop it.
    ///
    /// The link the box's owner made with `hud share --role follower` (read
    /// from stdin); with `--forget`, drop it.
    Follower {
        app: String,
        #[arg(long)]
        forget: bool,
    },
}

pub fn run(cmd: Option<StudioCmd>, ctx: Ctx) -> anyhow::Result<i32> {
    let Ctx { sock, json_out, .. } = ctx;
    let v = match cmd {
        None => request(&sock, "GET", "/api/studio", None)?.json()?,
        Some(StudioCmd::Login { url }) => {
            let token = secret_input("Studio token: ")?;
            request(&sock, "POST", "/api/studio", Some(&json!({ "url": url, "token": token })))?.json()?
        }
        Some(StudioCmd::Logout) => request(&sock, "DELETE", "/api/studio", None)?.json()?,
        Some(StudioCmd::Follower { app, forget: true }) => {
            request(&sock, "DELETE", &format!("/api/studio/followers/{}", enc(&app)), None)?.json()?
        }
        Some(StudioCmd::Follower { app, forget: false }) => {
            let link = secret_input("Follower link: ")?;
            let path = format!("/api/studio/followers/{}", enc(&app));
            request(&sock, "PUT", &path, Some(&json!({ "link": link })))?.json()?
        }
    };
    if json_out {
        print_json(&v);
    } else if let Some(apps) = v["apps"].as_array() {
        println!("logged in; {} app{}", apps.len(), if apps.len() == 1 { "" } else { "s" });
    } else if let Some(url) = v["url"].as_str() {
        let state = if v["logged_in"] == true { "logged in" } else { "logged out" };
        println!("{url}: {state}");
        for f in v["followers"].as_array().into_iter().flatten() {
            println!("  follower link for {}", f.as_str().unwrap_or("?"));
        }
    } else if v.get("logged_in").is_some() {
        println!("no studio: `illogical studio login <url>`");
    }
    Ok(0)
}

/// A secret from stdin: piped, the first line; on a terminal, asked for
/// without echo.
fn secret_input(prompt: &str) -> anyhow::Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    let tty = stdin.is_terminal();
    let mut line = String::new();
    if tty {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let read = term::read_hidden(&mut line);
        eprintln!();
        read?;
    } else {
        stdin.read_line(&mut line)?;
    }
    let v = line.trim().to_owned();
    if v.is_empty() {
        bail!("nothing given on stdin");
    }
    Ok(v)
}
