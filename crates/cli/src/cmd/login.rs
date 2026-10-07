//! `arugula login`: this CLI as one of your devices on Arugula control.

use crate::http::{self};
use crate::{Cli, control, fountain_runner, socket, ssh};

#[derive(clap::Args)]
pub struct Args {
    /// The control [default: the one this machine's daemon joined, else
    /// https://control.arugula.io].
    url: Option<String>,
    /// What the account calls this terminal [default: Arugula CLI on
    /// <hostname>].
    #[arg(long)]
    name: Option<String>,
    /// The account's fingerprint, as the approving device shows it:
    /// checked instead of asking.
    #[arg(long, value_name = "FINGERPRINT")]
    account: Option<String>,
}

pub fn run(args: &Args, cli: &Cli) -> anyhow::Result<i32> {
    let Args { url, name, account } = args;
    let url = match url {
        Some(u) => u.clone(),
        None => http::request(&http::Target::Socket(socket(cli)), "GET", "/api/host", None)
            .and_then(|r| r.json())
            .ok()
            .and_then(|v| v["control"].as_str().map(String::from))
            .unwrap_or_else(|| ssh::CONTROL.to_owned()),
    };
    let name = name.clone().unwrap_or_else(|| {
        let h = fountain_runner::hostname().unwrap_or_default();
        if h.is_empty() { "Arugula CLI".into() } else { format!("Arugula CLI on {h}") }
    });
    control::login(&url, &name, account.as_deref())?;
    Ok(0)
}
