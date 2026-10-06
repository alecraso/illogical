//! `illogical install`: the daemon's own installer, run from here.

use anyhow::bail;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub fn run(args: &Args) -> anyhow::Result<i32> {
    let Args { args } = args;
    // The daemon beside this binary, else the one on PATH.
    let beside = std::env::current_exe()?.with_file_name(format!("illogicald{}", std::env::consts::EXE_SUFFIX));
    let daemon = if beside.exists() { beside } else { PathBuf::from("illogicald") };
    let mut cmd = std::process::Command::new(&daemon);
    cmd.arg("install").args(args);
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
