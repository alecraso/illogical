//! M51 end to end over a real sshd: the test stack's `ssh` profile
//! (`testnet/`, #200), a bastion and box-bare, which has no Arugula and is
//! reached only by ProxyJump. The CLI's `--ssh` installs Arugula there,
//! starts its daemon, runs and captures a pane, gives the box's panes this
//! client's agent (a `git push` from a pane to the stack's git server works
//! with it, and only with it), survives the connection going away, and a
//! saved ssh host works with `--host`. `web --print` prints the box's link.
//!
//! Needs Docker (it brings the stack's `ssh` profile up if it isn't) and the
//! box's static binaries from this tree (`just static aarch64` on Apple
//! silicon, `just static` on x86_64), or ARUGULA_SSH_BINARIES; without
//! them, or with ones older than this tree or another version, it fails
//! naming `just static`. ARUGULA_SKIP_DOCKER=1 skips it, loudly. It recreates
//! box-bare, so a run starts from a box with nothing on it.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use crate::testnet;

use testnet::{Env, cli_bin, wait_for};

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

/// #259: stale static binaries are named as such, without Docker.
#[test]
fn stale_static_binaries_are_refused() {
    let root = std::env::temp_dir().join(format!("ilg-stale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let krate = root.join("crates/x");
    std::fs::create_dir_all(krate.join("src")).unwrap();
    std::fs::write(root.join("Cargo.lock"), "").unwrap();
    let src = krate.join("src/main.rs");
    std::fs::write(&src, "fn main() {}").unwrap();
    let dir = root.join("target/aarch64-unknown-linux-musl/release");
    std::fs::create_dir_all(&dir).unwrap();
    // Built after the source, from this tree, at this version.
    std::thread::sleep(Duration::from_millis(20));
    let build = |version: &str, from: &Path| {
        for bin in ["arugula", "arugulad"] {
            std::fs::write(dir.join(bin), format!("ELF..\0arugula-version={version}\0..")).unwrap();
            std::fs::write(dir.join(format!("{bin}.d")), format!("{}: {}\n", dir.join(bin).display(), from.display()))
                .unwrap();
        }
    };
    let current = |v: &str| testnet::binaries_current(&dir, v, &krate);
    build("1.2.3", &src);
    assert_eq!(current("1.2.3"), Ok(()));
    let e = current("1.3.0").unwrap_err();
    assert!(e.contains("arugula is 1.2.3, this build is 1.3.0"), "{e}");

    // The source changes afterwards.
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&src, "fn main() { }").unwrap();
    let e = current("1.2.3").unwrap_err();
    assert!(e.contains("main.rs changed after arugula was built"), "{e}");

    // Built from another checkout (a copied target directory).
    build("1.2.3", Path::new("/elsewhere/crates/x/src/main.rs"));
    let e = current("1.2.3").unwrap_err();
    assert!(e.contains("built from another tree"), "{e}");

    // No mark at all (an older build).
    std::fs::write(dir.join("arugula"), "ELF").unwrap();
    assert!(current("1.2.3").unwrap_err().contains("no version mark"));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn ssh_installs_runs_forwards_the_agent_pushes_and_saved_hosts_work() {
    if !testnet::require("ssh", "box-bare", "ssh.rs (M51)") {
        return;
    }
    // A box with nothing on it.
    testnet::recreate(&["box-bare"]);
    let arch = String::from_utf8(testnet::ssh().args(["box-bare", "uname", "-m"]).output().unwrap().stdout).unwrap();
    let binaries = testnet::require_binaries(arch.trim());

    // A short directory for the masters (a socket path is at most 104
    // bytes on macOS, and the temp dir there is long).
    let runtime = PathBuf::from(format!("/tmp/ilg-ssh-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    let agent = agent(&runtime);
    let env = Env {
        cli: cli_bin(),
        binaries,
        runtime,
        agent: Some(agent.1.display().to_string()),
        sock: None,
        owner: Some("box-bare".into()),
    };

    // Missing, so installed (yes was given), and the daemon started.
    let first = env.output(&["--ssh", "box-bare", "ls"]);
    let err = String::from_utf8_lossy(&first.stderr);
    assert!(first.status.success(), "first --ssh: {err}");
    assert!(err.contains("installing") && err.contains("starting arugulad"), "{err}");

    // A pane there.
    let pane = env.ok(&["--ssh", "box-bare", "run", "--", "sh", "-c", "echo over-ssh-$((40+2))"]).trim().to_owned();
    wait_for("the pane's output", || env.ok(&["--ssh", "box-bare", "capture", &pane]).contains("over-ssh-42"));

    // The box daemon's sign-in link, not this machine's (#252).
    let link = env.ok(&["--ssh", "box-bare", "web", "--print"]).trim().to_owned();
    assert!(link.starts_with("http://") && link.contains("/auth?"), "web --print over ssh: {link:?}");
    let told = env.ok(&["--ssh", "box-bare", "web"]);
    assert!(told.contains(&link) && told.contains("ssh -N -L") && told.contains("box-bare"), "{told}");

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

    // Without the agent (ARUGULA_SSH_AGENT=no), the same push is refused.
    env.disconnect("box-bare");
    let mut watching = env
        .cmd(&["--ssh", "box-bare", "events", "-f"])
        .env("ARUGULA_SSH_AGENT", "no")
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
    let mut home = Command::new(env!("CARGO_BIN_EXE_arugulad"))
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
        owner: None,
    };
    saved.ok(&["hosts", "add", "bb", "ssh://box-bare"]);
    assert!(saved.ok(&["hosts"]).contains("ssh ssh://box-bare"));
    assert!(saved.ok(&["--host", "bb", "ls"]).contains(&pane), "--host reaches it over ssh");
    let bad = saved.output(&["hosts", "add", "evil", "ssh://-oProxyCommand=id"]);
    assert!(!bad.status.success(), "an option as a destination is refused");
    let _ = home.kill();
    let _ = home.wait();
}
