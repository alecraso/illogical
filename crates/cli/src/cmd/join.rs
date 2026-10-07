//! `arugula join`: this machine, or a box over ssh, on your account on Arugula control.

use crate::{Cli, ssh};
use anyhow::bail;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// The control [default: https://control.arugula.io].
    url: Option<String>,
    /// Its name in the directory [default: its hostname].
    #[arg(long)]
    name: Option<String>,
    /// Join it to a team (its id), not your account alone.
    #[arg(long)]
    team: Option<String>,
    /// The account's fingerprint, as the approving device shows it:
    /// checked instead of asking.
    #[arg(long, value_name = "FINGERPRINT")]
    account: Option<String>,
}

pub fn run(args: &Args, cli: &Cli) -> anyhow::Result<i32> {
    let Args { url, name, team, account } = args;
    let control = url.clone().unwrap_or_else(|| ssh::CONTROL.to_owned());
    let mut args = vec!["join".to_owned(), control.clone()];
    for (flag, v) in [("--name", name), ("--team", team), ("--account", account)] {
        if let Some(v) = v {
            args.extend([flag.to_owned(), v.clone()]);
        }
    }
    if let Some(dest) = &cli.ssh {
        return ssh::Remote::parse(dest)?.join(&args, &control);
    }
    let beside = std::env::current_exe()?.with_file_name(format!("arugulad{}", std::env::consts::EXE_SUFFIX));
    let daemon = if beside.exists() { beside } else { PathBuf::from("arugulad") };
    let mut cmd = std::process::Command::new(&daemon);
    cmd.args(&args);
    #[cfg(unix)]
    let err = std::os::unix::process::CommandExt::exec(&mut cmd);
    // No exec on Windows: run it and pass on its exit code.
    #[cfg(not(unix))]
    let err = match cmd.status() {
        Ok(s) => return Ok(s.code().unwrap_or(1)),
        Err(e) => e,
    };
    bail!("running {}: {err}", daemon.display());
}
