//! M51 end to end over a real sshd: the test stack's `ssh` profile
//! (`testnet/`, #200), a bastion and box-bare, which has no illogical and is
//! reached only by ProxyJump. The CLI's `--ssh` installs illogical there,
//! starts its daemon, runs and captures a pane, gives the box's panes this
//! client's agent (a `git push` from a pane to the stack's git server works
//! with it, and only with it), survives the connection going away, and a
//! saved ssh host works with `--host`.
//!
//! Needs `just testnet up ssh` and the box's static binaries from this tree
//! (`just static aarch64` on Apple silicon, `just static` on x86_64), or
//! ILLOGICAL_SSH_BINARIES. Without them it says SKIP and passes. It
//! recreates box-bare, so a run starts from a box with nothing on it.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

mod testnet;

use testnet::ssh_config;

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
        .arg(testnet::state().join("id_ed25519"))
        .env("SSH_AUTH_SOCK", &sock)
        .status()
        .unwrap();
    assert!(st.success(), "ssh-add");
    Agent(child, sock)
}

/// A pane's shell command that commits and pushes `branch` to the stack's
/// git server, then says how it went (the markers are computed, so the
/// command line itself never matches them).
fn push(branch: &str) -> String {
    format!(
        "cd \"$(mktemp -d)\" && git init -q && git -c user.name=illo -c user.email=illo@box-bare commit -q --allow-empty -m {branch} \
         && git push -q git@git:/srv/git/repo.git HEAD:refs/heads/{branch} && echo pushed-$((6*7)) || echo push-failed-$((6*7))"
    )
}

#[test]
fn ssh_installs_runs_forwards_the_agent_pushes_and_saved_hosts_work() {
    if !testnet::reachable("box-bare") {
        eprintln!("SKIP: the test stack isn't up (`just testnet up ssh`)");
        return;
    }
    // A box with nothing on it.
    testnet::recreate(&["box-bare"]);
    let arch = String::from_utf8(testnet::ssh().args(["box-bare", "uname", "-m"]).output().unwrap().stdout).unwrap();
    let Some(binaries) = box_binaries(arch.trim()) else {
        eprintln!("SKIP: no static binaries for {} (`just static {}`)", arch.trim(), arch.trim());
        return;
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
        Command::new("ssh-keygen").arg("-lf").arg(testnet::state().join("id_ed25519.pub")).output().unwrap().stdout,
    )
    .unwrap();
    let want = want.split_whitespace().nth(1).unwrap().to_owned();
    let p2 = env.ok(&["--ssh", "box-bare", "run", "--", "ssh-add", "-l"]).trim().to_owned();
    wait_for("the forwarded key in a pane", || env.ok(&["--ssh", "box-bare", "capture", &p2]).contains(&want));

    // `git push` from a pane there to the stack's git server, which knows
    // only the client's key; the box has no key of its own, so the push
    // signs in with the forwarded agent.
    let branch = format!("m51-{}", std::process::id());
    let p3 = env.ok(&["--ssh", "box-bare", "run", &push(&branch)]).trim().to_owned();
    wait_for("the push", || {
        let out = env.ok(&["--ssh", "box-bare", "capture", &p3]);
        assert!(!out.contains("push-failed-42"), "git push from a pane: {out}");
        out.contains("pushed-42")
    });
    let o = testnet::exec(
        "git",
        &["git", "--git-dir=/srv/git/repo.git", "rev-parse", "--verify", &format!("refs/heads/{branch}")],
    );
    assert!(o.status.success(), "the branch is on the git server");
    let _ = watching.kill();
    let _ = watching.wait();

    // Without the agent (ILLOGICAL_SSH_AGENT=no), the same push is refused.
    env.disconnect("box-bare");
    let mut watching = env
        .cmd(&["--ssh", "box-bare", "events", "-f"])
        .env("ILLOGICAL_SSH_AGENT", "no")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let p4 = env.ok(&["--ssh", "box-bare", "run", &push(&format!("{branch}-noagent"))]).trim().to_owned();
    wait_for("the refused push", || {
        let out = env.ok(&["--ssh", "box-bare", "capture", &p4]);
        assert!(!out.contains("pushed-42"), "pushed with no agent: {out}");
        out.contains("push-failed-42")
    });
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
