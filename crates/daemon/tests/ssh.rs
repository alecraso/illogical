//! M51 end to end over a real sshd: the test stack's `ssh` profile
//! (`testnet/`, #200), a bastion and box-bare, which has no illogical and is
//! reached only by ProxyJump. The CLI's `--ssh` installs illogical there,
//! starts its daemon, runs and captures a pane, gives the box's panes this
//! client's agent, survives the connection going away, and a saved ssh host
//! works with `--host`.
//!
//! Needs Docker, and the box's static binaries from this tree (`just static
//! aarch64` on Apple silicon, `just static` on x86_64) or
//! ILLOGICAL_SSH_BINARIES. It brings the stack up itself if it isn't (`just
//! testnet up ssh`). Without Docker or the binaries it fails, saying what to
//! run; only ILLOGICAL_SKIP_DOCKER=1 skips it, saying it didn't run. It
//! recreates box-bare, so a run starts from a box with nothing on it.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn ssh_config() -> PathBuf {
    root().join("testnet/.state/ssh_config")
}

fn cli_bin() -> PathBuf {
    let bin = Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

/// The box's binaries: ILLOGICAL_SSH_BINARIES, else `just static`'s output
/// for its architecture.
fn box_binaries(arch: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("ILLOGICAL_SSH_BINARIES").map(PathBuf::from).unwrap_or_else(|| {
        let target = Path::new(env!("CARGO_BIN_EXE_illogicald")).parent().unwrap().parent().unwrap().to_path_buf();
        target.join(format!("{arch}-unknown-linux-musl/release"))
    });
    (dir.join("illogical").is_file() && dir.join("illogicald").is_file()).then_some(dir)
}

/// Every CLI run here: the stack's ssh config, this test's own master
/// directory, and yes to installing.
struct Env {
    cli: PathBuf,
    binaries: PathBuf,
    runtime: PathBuf,
    agent: Option<String>,
    sock: Option<PathBuf>,
    /// Closes the master and removes `runtime` when done.
    owner: bool,
}

impl Env {
    fn cmd(&self, args: &[&str]) -> Command {
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

    fn ok(&self, args: &[&str]) -> String {
        let o = self.cmd(args).output().unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn output(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// Close the master, as a dropped connection would.
    fn disconnect(&self, dest: &str) {
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
        if self.owner {
            self.disconnect("box-bare");
            let _ = std::fs::remove_dir_all(&self.runtime);
        }
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

struct Agent(Child, PathBuf);

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(self.1.parent().unwrap());
    }
}

/// An ssh-agent of our own holding the stack's key.
fn agent(runtime: &Path) -> Agent {
    let sock = runtime.join("agent").join("a.sock");
    std::fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let child = Command::new("ssh-agent").arg("-D").arg("-a").arg(&sock).stdout(Stdio::null()).spawn().unwrap();
    wait_for("ssh-agent", || sock.exists());
    let st = Command::new("ssh-add")
        .arg("-q")
        .arg(root().join("testnet/.state/id_ed25519"))
        .env("SSH_AUTH_SOCK", &sock)
        .status()
        .unwrap();
    assert!(st.success(), "ssh-add");
    Agent(child, sock)
}

/// The ssh profile up: brought up here if it isn't. `false` only when
/// ILLOGICAL_SKIP_DOCKER=1 and there's no Docker; without Docker otherwise,
/// it fails.
fn stack_up() -> bool {
    let quiet = |c: &mut Command| c.stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if !quiet(Command::new("docker").arg("info")) {
        if std::env::var("ILLOGICAL_SKIP_DOCKER").as_deref() == Ok("1") {
            eprintln!("SKIP (ILLOGICAL_SKIP_DOCKER=1): no Docker, so this test did NOT run");
            return false;
        }
        panic!(
            "Docker is not available (`docker info` failed): this test needs it. Start Docker, or set ILLOGICAL_SKIP_DOCKER=1 to skip it on purpose."
        );
    }
    let reachable = || {
        ssh_config().exists()
            && quiet(Command::new("ssh").arg("-F").arg(ssh_config()).args(["-o", "BatchMode=yes", "box-bare", "true"]))
    };
    if !reachable() {
        let up = Command::new(root().join("testnet/up.sh")).arg("ssh").status().unwrap();
        assert!(up.success() && reachable(), "the test stack didn't come up: run `just testnet up ssh` and see why");
    }
    true
}

#[test]
fn ssh_installs_runs_forwards_the_agent_and_saved_hosts_work() {
    if !stack_up() {
        return;
    }
    let cfg = ssh_config();
    // A box with nothing on it.
    let st = Command::new("docker")
        .args(["compose", "-f"])
        .arg(root().join("testnet/compose.yaml"))
        .args(["--profile", "ssh", "up", "-d", "--force-recreate", "--wait", "box-bare"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "recreating box-bare");
    let arch = String::from_utf8(
        Command::new("ssh").arg("-F").arg(&cfg).args(["box-bare", "uname", "-m"]).output().unwrap().stdout,
    )
    .unwrap();
    let Some(binaries) = box_binaries(arch.trim()) else {
        panic!(
            "no static binaries for the box ({0}): run `just static {0}`, or set ILLOGICAL_SSH_BINARIES",
            arch.trim()
        );
    };

    // A short directory for the masters (a socket path is at most 104
    // bytes on macOS, and the temp dir there is long).
    let runtime = PathBuf::from(format!("/tmp/ilg-ssh-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    let agent = agent(&runtime);
    let env =
        Env { cli: cli_bin(), binaries, runtime, agent: Some(agent.1.display().to_string()), sock: None, owner: true };

    // Missing, so installed (yes was given), and the daemon started.
    let first = env.output(&["--ssh", "box-bare", "ls"]);
    let err = String::from_utf8_lossy(&first.stderr);
    assert!(first.status.success(), "first --ssh: {err}");
    assert!(err.contains("installing") && err.contains("starting illogicald"), "{err}");

    // A pane there.
    let pane = env.ok(&["--ssh", "box-bare", "run", "--", "sh", "-c", "echo over-ssh-$((40+2))"]).trim().to_owned();
    wait_for("the pane's output", || env.ok(&["--ssh", "box-bare", "capture", &pane]).contains("over-ssh-42"));

    // The client's agent, in a pane, while a client stays connected (an
    // events stream stands in for someone in the TUI).
    let mut watching = env.cmd(&["--ssh", "box-bare", "events", "-f"]).stdout(Stdio::null()).spawn().unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let want = String::from_utf8(
        Command::new("ssh-keygen")
            .arg("-lf")
            .arg(root().join("testnet/.state/id_ed25519.pub"))
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let want = want.split_whitespace().nth(1).unwrap().to_owned();
    let p2 = env.ok(&["--ssh", "box-bare", "run", "--", "ssh-add", "-l"]).trim().to_owned();
    wait_for("the forwarded key in a pane", || env.ok(&["--ssh", "box-bare", "capture", &p2]).contains(&want));
    let _ = watching.kill();
    let _ = watching.wait();

    // The connection goes away; the pane doesn't.
    env.disconnect("box-bare");
    assert!(env.ok(&["--ssh", "box-bare", "ls"]).contains(&pane), "the pane outlives the connection");

    // A saved ssh host, on a home daemon of our own.
    let state = env.runtime.join("home");
    let mut home = Command::new(env!("CARGO_BIN_EXE_illogicald"))
        .arg("--state-dir")
        .arg(&state)
        .args(["--listen", "127.0.0.1:0", "--name", "home", "--no-manager-env", "--tailscale-socket", "/nonexistent"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let sock = state.join("sock");
    wait_for("the home daemon", || sock.exists());
    let saved = Env {
        sock: Some(sock),
        runtime: env.runtime.clone(),
        agent: None,
        cli: env.cli.clone(),
        binaries: env.binaries.clone(),
        owner: false,
    };
    saved.ok(&["hosts", "add", "bb", "ssh://box-bare"]);
    assert!(saved.ok(&["hosts"]).contains("ssh ssh://box-bare"));
    assert!(saved.ok(&["--host", "bb", "ls"]).contains(&pane), "--host reaches it over ssh");
    let bad = saved.output(&["hosts", "add", "evil", "ssh://-oProxyCommand=id"]);
    assert!(!bad.status.success(), "an option as a destination is refused");
    let _ = home.kill();
    let _ = home.wait();
}
