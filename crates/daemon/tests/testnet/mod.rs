//! The test stack (`testnet/`, #200) as the tests here see it. Its name is
//! COMPOSE_PROJECT_NAME (default illogical-testnet), which prefixes its
//! containers and picks its state directory, as `testnet/env.sh` does for
//! the scripts.
#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
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

/// The stack's `profile`, up, with `host` answering: brought up with
/// `testnet/up.sh` if it isn't. Docker tests don't skip: no Docker is a
/// failure. Only ILLOGICAL_SKIP_DOCKER=1 skips, which returns false and
/// says loudly that nothing ran.
pub fn require(profile: &str, host: &str, test: &str) -> bool {
    if std::env::var("ILLOGICAL_SKIP_DOCKER").is_ok_and(|v| v == "1") {
        eprintln!(
            "\n{}\nSKIPPED (ILLOGICAL_SKIP_DOCKER=1): {test} did NOT run; nothing was tested\n{}\n",
            "!".repeat(72),
            "!".repeat(72)
        );
        return false;
    }
    let docker = Command::new("docker").arg("info").stdout(Stdio::null()).stderr(Stdio::null()).status();
    assert!(
        docker.is_ok_and(|s| s.success()),
        "{test} needs Docker, which isn't available (ILLOGICAL_SKIP_DOCKER=1 skips it)"
    );
    if !reachable(host) {
        let st = Command::new(root().join("testnet/up.sh"))
            .arg(profile)
            .env("COMPOSE_PROJECT_NAME", name())
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "testnet/up.sh {profile} failed");
        assert!(reachable(host), "{host} doesn't answer after testnet/up.sh {profile}");
    }
    true
}

/// The box's binaries for `arch`, or a failure saying how to build them.
pub fn require_binaries(arch: &str) -> PathBuf {
    box_binaries(arch).unwrap_or_else(|| {
        panic!("no static binaries for {arch}: run `just static {arch}` (or set ILLOGICAL_SSH_BINARIES)")
    })
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

pub fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

/// The box's binaries: ILLOGICAL_SSH_BINARIES, else `just static`'s output
/// for its architecture.
pub fn box_binaries(arch: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("ILLOGICAL_SSH_BINARIES").map(PathBuf::from).unwrap_or_else(|| {
        let target = Path::new(env!("CARGO_BIN_EXE_illogicald")).parent().unwrap().parent().unwrap().to_path_buf();
        target.join(format!("{arch}-unknown-linux-musl/release"))
    });
    (dir.join("illogical").is_file() && dir.join("illogicald").is_file()).then_some(dir)
}

/// The CLI as a test runs it: the stack's ssh config, the test's own
/// master directory, and yes to installing.
pub struct Env {
    pub cli: PathBuf,
    pub binaries: PathBuf,
    pub runtime: PathBuf,
    pub agent: Option<String>,
    pub sock: Option<PathBuf>,
    /// The box whose master is closed, with `runtime` removed, when done;
    /// None for a copy that shares another's.
    pub owner: Option<String>,
}

impl Env {
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.cli);
        c.args(args)
            .env("ILLOGICAL_SSH", format!("ssh -F {}", ssh_config().display()))
            .env("ILLOGICAL_SSH_BINARIES", &self.binaries)
            .env("ILLOGICAL_SSH_INSTALL", "yes")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env_remove("ILLOGICAL_PANE")
            .stdin(Stdio::null());
        match &self.agent {
            Some(a) => c.env("SSH_AUTH_SOCK", a),
            None => c.env_remove("SSH_AUTH_SOCK"),
        };
        match &self.sock {
            Some(s) => c.env("ILLOGICAL_SOCK", s),
            None => c.env_remove("ILLOGICAL_SOCK"),
        };
        c
    }

    pub fn ok(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    pub fn output(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Close the master, as a dropped connection would.
    pub fn disconnect(&self, dest: &str) {
        let _ = Command::new("ssh")
            .arg("-F")
            .arg(ssh_config())
            .arg("-o")
            .arg(format!("ControlPath=\"{}\"", self.runtime.join("illogical-ssh/%C").display()))
            .args(["-O", "exit", dest])
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(dest) = self.owner.take() {
            self.disconnect(&dest);
            let _ = std::fs::remove_dir_all(&self.runtime);
        }
    }
}

pub fn wait_for(what: &str, f: impl FnMut() -> bool) {
    wait_up_to(Duration::from_secs(20), what, f)
}

pub fn wait_up_to(limit: Duration, what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + limit;
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}
