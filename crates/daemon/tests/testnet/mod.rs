//! The test stack (`testnet/`, #200) as the tests here see it. Its name is
//! COMPOSE_PROJECT_NAME (default illogical-testnet), which prefixes its
//! containers and picks its state directory, as `testnet/env.sh` does for
//! the scripts.
#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn name() -> String {
    std::env::var("COMPOSE_PROJECT_NAME").ok().filter(|n| !n.is_empty()).unwrap_or_else(|| "illogical-testnet".into())
}

/// Keys, `known_hosts` and `ssh_config`, written by `testnet/up.sh`.
pub fn state() -> PathBuf {
    if let Some(s) = std::env::var_os("ILLOGICAL_TESTNET_STATE") {
        return PathBuf::from(s);
    }
    match name().as_str() {
        "illogical-testnet" => root().join("testnet/.state"),
        n => root().join(format!("testnet/.state-{n}")),
    }
}

pub fn ssh_config() -> PathBuf {
    state().join("ssh_config")
}

/// A service's container (`box-systemd` → `illogical-testnet-box-systemd`).
pub fn container(service: &str) -> String {
    format!("{}-{service}", name())
}

/// `docker compose` on the stack's file, for this stack.
pub fn compose() -> Command {
    let mut c = Command::new("docker");
    c.args(["compose", "-f"])
        .arg(root().join("testnet/compose.yaml"))
        .env("COMPOSE_PROJECT_NAME", name())
        .env("ILLOGICAL_TESTNET_STATE", state());
    c
}

/// `docker exec` in a service's container, as root.
pub fn exec(service: &str, args: &[&str]) -> Output {
    Command::new("docker").arg("exec").arg(container(service)).args(args).output().unwrap()
}

/// `ssh -F <the stack's config>`.
pub fn ssh() -> Command {
    let mut c = Command::new("ssh");
    c.arg("-F").arg(ssh_config());
    c
}

/// Whether a box answers over ssh (the stack is up).
pub fn reachable(host: &str) -> bool {
    ssh_config().exists()
        && ssh()
            .args(["-o", "BatchMode=yes", host, "true"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// Recreate services, so a run starts from boxes with nothing on them.
pub fn recreate(services: &[&str]) {
    let st = compose()
        .args(["--profile", "ssh", "up", "-d", "--force-recreate", "--wait"])
        .args(services)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "recreating {services:?}");
}
