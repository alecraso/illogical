//! #26's reboot check in a container (#214 section 4): box-systemd from the
//! test stack (`testnet/`), with Arugula installed over `--ssh` as a
//! lingering systemd user service. A session like #26's "before" is built
//! through the CLI: tabs and splits, a shell in a nested directory with
//! coloured output, `rerun`, `rerun-ask`, `hook` (a command pane) and
//! `none` panes, a browser block and an agent block (the fake ACP agent).
//! Then `docker restart`, which shuts systemd down cleanly and boots it
//! again, and the checks:
//!
//! - the daemon is up with nobody logged in (lingering);
//! - the layout, each pane's directory and its scrollback are back, with
//!   the `── restored` marker and the colour;
//! - each pane did what its policy says, the browser block has its page,
//!   and the agent block's session came back with its context;
//! - the journal says "saved for shutdown" and "restored";
//! - a headless web client (`web/reconnect-watch.ts`, Chromium) that was
//!   attached across the restart reconnected on its own, without a reload.
//!
//! The web client reaches the box's daemon through an ssh forward that the
//! test keeps re-opening, as a network path would come back. It's held off
//! until the no-login check is done.
//!
//! Needs Docker (it brings the stack's `ssh` profile up if it isn't), the
//! box's static binaries (as `ssh.rs`), and Playwright's Chromium (`cd web
//! && pnpm install && pnpm exec playwright install chromium`); without them
//! it fails. ARUGULA_SKIP_DOCKER=1 skips it, loudly. It recreates
//! box-systemd.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use std::{
    io::{BufRead, BufReader},
    net::TcpListener,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use serde_json::Value;

use crate::testnet;

use testnet::{Env, cli_bin, wait_for, wait_up_to};

const BOX: &str = "box-systemd";
const ACP: &str = "env FAKE_ACP_DIR=/home/illo/.fake-acp python3 /home/illo/fake_acp.py";

/// `ssh -L` to the box's daemon, opened again whenever it drops, while
/// allowed.
struct Forward {
    port: u16,
    allow: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<()>>,
}

impl Forward {
    fn start(to: &str) -> Self {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let allow = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let child: Arc<Mutex<Option<Child>>> = Arc::default();
        let (a, s, c, to) = (allow.clone(), stop.clone(), child.clone(), to.to_owned());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                {
                    let mut c = c.lock().unwrap();
                    if c.as_mut().is_some_and(|ch| ch.try_wait().is_ok_and(|st| st.is_some())) {
                        *c = None;
                    }
                    if c.is_none() && a.load(Ordering::SeqCst) {
                        *c = testnet::ssh()
                            .args(["-N", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=1"])
                            .args(["-o", "ServerAliveCountMax=3", "-L", &format!("127.0.0.1:{port}:{to}"), BOX])
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn()
                            .ok();
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        });
        Self { port, allow, stop, child, thread: Some(thread) }
    }

    fn hold(&self, on: bool) {
        self.allow.store(!on, Ordering::SeqCst);
        if on && let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.hold(true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The headless web client, and the lines it has printed so far.
struct Watcher {
    child: Child,
    _stdin: ChildStdin,
    seen: Arc<Mutex<Vec<Value>>>,
}

impl Watcher {
    fn start(url: &str, pane: &str, marker: &str, via: &str) -> Self {
        let mut child = Command::new("node")
            .args(["--experimental-strip-types", "--no-warnings", "reconnect-watch.ts", url, pane, marker, via])
            .current_dir(testnet::root().join("web"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let (out, s) = (child.stdout.take().unwrap(), seen.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str(&line) {
                    s.lock().unwrap().push(v);
                }
            }
        });
        let stdin = child.stdin.take().unwrap();
        Self { child, _stdin: stdin, seen }
    }

    fn last(&self) -> Value {
        self.seen.lock().unwrap().last().cloned().unwrap_or(Value::Null)
    }

    fn lines(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn exec_ok(args: &[&str]) -> String {
    let o = testnet::exec(BOX, args);
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

/// The daemon's journal lines with `what` in them (root reads the user's),
/// without the log's colours.
fn journal(what: &str) -> usize {
    exec_ok(&["journalctl", "--no-pager", "-o", "cat", "_SYSTEMD_USER_UNIT=arugulad.service"])
        .lines()
        .map(|l| {
            let mut plain = String::new();
            let mut esc = false;
            for c in l.chars() {
                match (esc, c) {
                    (false, '\x1b') => esc = true,
                    (false, c) => plain.push(c),
                    (true, 'm') => esc = false,
                    (true, _) => {}
                }
            }
            plain
        })
        .filter(|l| l.contains(what))
        .count()
}

/// What a pane printed after the restore marker.
fn after_marker(text: &str) -> Option<&str> {
    text.split_once("── restored").map(|(_, after)| after)
}

/// The panes in a tab's tree of splits.
fn in_tree(node: &Value, out: &mut Vec<Value>) {
    match node["type"].as_str() {
        Some("pane") => out.push(node["pane"].clone()),
        _ => {
            for c in node["children"].as_array().into_iter().flatten() {
                in_tree(&c["node"], out);
            }
        }
    }
}

/// Sessions, tabs and splits, what each pane restarts by (its policy,
/// directory and command, as saved; a hook pane's command aside), and where each is and in which
/// directory, as the daemon reports it: what has to come back as it was.
/// Sizes are left out (they follow whoever is attached).
fn layout(env: &Env) -> Value {
    let raw = testnet::ssh().args([BOX, "cat", ".local/state/arugula/layout.json"]).output().unwrap().stdout;
    let mut v: Value = serde_json::from_slice(&raw).unwrap();
    for t in v["mux"]["tabs"].as_object_mut().unwrap().values_mut() {
        let t = t.as_object_mut().unwrap();
        t.retain(|k, _| matches!(k.as_str(), "id" | "name" | "root"));
    }
    // A hook pane runs its policy's command; what it ran before doesn't
    // matter after a restart (and it's back at its shell then).
    for p in v["panes"].as_object_mut().unwrap().values_mut() {
        if p["policy"]["kind"] == "hook" {
            p.as_object_mut().unwrap().remove("command");
        }
    }
    let live: Value = serde_json::from_str(&env.ok(&["--ssh", BOX, "--json", "ls"])).unwrap();
    let live: Vec<Value> = live
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            serde_json::json!({
                "id": p["id"], "type": p["type"], "session": p["session_name"], "tab": p["tab"],
                "cwd": p["cwd"], "policy": p["policy"],
            })
        })
        .collect();
    serde_json::json!({ "sessions": v["mux"]["sessions"], "tabs": v["mux"]["tabs"], "saved": v["panes"], "panes": live })
}

#[test]
fn a_restarted_box_brings_its_daemon_back_with_no_login_and_every_pane_by_policy() {
    if !testnet::require("ssh", BOX, "reboot.rs (#26)") {
        return;
    }
    let web = testnet::root().join("web");
    let node = Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success());
    assert!(
        node && web.join("node_modules/@playwright/test").exists(),
        "the headless client needs node and Playwright in web/ (`cd web && pnpm install`)"
    );
    testnet::recreate(&[BOX]);
    let arch = String::from_utf8(testnet::ssh().args([BOX, "uname", "-m"]).output().unwrap().stdout).unwrap();
    let binaries = testnet::require_binaries(arch.trim());
    let runtime = PathBuf::from(format!("/tmp/ilg-rb-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    let env = Env { cli: cli_bin(), binaries, runtime, agent: None, sock: None, owner: Some(BOX.into()) };
    let ilg = |args: &[&str]| -> String {
        let mut a = vec!["--ssh", BOX];
        a.extend_from_slice(args);
        env.ok(&a).trim().to_owned()
    };
    // Split panes are narrow and their lines wrap: what they printed, with
    // the wraps taken out.
    let capture = |pane: &str| ilg(&["capture", "--scrollback", pane]).replace('\n', "");

    // Installed as a user service, with lingering turned on from the ssh
    // login (no sudo).
    let first = env.output(&["--ssh", BOX, "ls"]);
    let err = String::from_utf8_lossy(&first.stderr);
    assert!(first.status.success() && err.contains("installing"), "first --ssh: {err}");
    assert_eq!(exec_ok(&["loginctl", "show-user", "illo", "-p", "Linger", "--value"]), "yes");
    assert_eq!(
        exec_ok(&[
            "sh",
            "-c",
            "su illo -c 'XDG_RUNTIME_DIR=/run/user/$(id -u illo) systemctl --user is-enabled arugulad'"
        ]),
        "enabled"
    );

    // #26's "before".
    let st = testnet::ssh()
        .args([BOX, "mkdir -p a/b/c && cat > fake_acp.py"])
        .stdin(std::fs::File::open(testnet::root().join("crates/daemon/tests/fake_acp.py")).unwrap())
        .status()
        .unwrap();
    assert!(st.success(), "copying the fake agent");
    let shell = ilg(&["run", "--session", "work", "--cwd", "/home/illo/a/b/c"]);
    wait_for("the shell's prompt", || capture(&shell).contains("illo@box-systemd"));
    ilg(&["send", "-e", &shell, r"printf '\033[31mred-%s\033[0m\n' $((6*7))"]);
    wait_for("coloured output", || capture(&shell).contains("red-42"));
    let rerun = ilg(&["run", "--split", &shell, "--policy", "rerun", "echo rerun-$((2+3)); exec sleep 100000"]);
    let ask = ilg(&["run", "--split", &shell, "--policy", "rerun-ask", "echo ask-$((3+4)); exec sleep 100000"]);
    let hook = ilg(&["run", "--session", "work", "--policy", "hook:echo hook-$((3*3))", "bash"]);
    let none = ilg(&["run", "--split", &hook, "--policy", "none", "bash"]);
    let server =
        ilg(&["run", "--session", "work", "--policy", "rerun", "exec python3 -m http.server 8000 --bind 127.0.0.1"]);
    // The box has no route out: the page doesn't load, but the block and
    // its address are what has to come back.
    let browser = ilg(&["open", "--split", &server, "https://example.com/"]);
    let agent = ilg(&["agent", "--session", "work", "--acp", ACP, "--wait", "remember kestrel"]);
    let agent = agent.lines().next().unwrap().trim().to_owned();
    wait_for("rerun's output", || capture(&rerun).contains("rerun-5"));
    wait_for("rerun-ask's output", || capture(&ask).contains("ask-7"));
    wait_for("the hook pane's shell", || capture(&hook).contains("illo@box-systemd"));
    let before = {
        let mut last = Value::Null;
        // The layout file is written a moment after a change.
        wait_for("the saved layout", || {
            last = layout(&env);
            let mut placed = Vec::new();
            for t in last["tabs"].as_object().unwrap().values() {
                in_tree(&t["root"], &mut placed);
            }
            last["panes"].as_array().unwrap().iter().all(|p| placed.contains(&p["id"]))
        });
        last
    };
    assert_eq!(journal("saved for shutdown"), 0, "nothing saved for shutdown yet");
    let restored_before = journal("restored sessions=");

    // The web client, attached through a forward to the daemon's own port
    // (its Host check wants the name it knows, so Chromium maps that name
    // to the forward).
    let listen = testnet::ssh().args([BOX, "cat", ".local/state/arugula/listen"]).output().unwrap().stdout;
    let listen = String::from_utf8(listen).unwrap().trim().to_owned();
    let port = listen.rsplit(':').next().unwrap().to_owned();
    let token = testnet::ssh().args([BOX, "cat", ".local/state/arugula/local-token"]).output().unwrap().stdout;
    let token = String::from_utf8(token).unwrap().trim().to_owned();
    let fwd = Forward::start(&listen);
    let via = format!("127.0.0.1:{}", fwd.port);
    let watcher =
        Watcher::start(&format!("http://localhost:{port}/auth?token={token}&next=/"), &shell[1..], "red-42", &via);
    wait_up_to(Duration::from_secs(60), "the web client showing the shell", || {
        let l = watcher.last();
        l["connected"] == true && l["marker"] == true
    });
    assert_eq!(watcher.last()["restored"], false);

    // The reboot. Nobody logs in until the daemon is checked.
    reboot(&env, &fwd, restored_before);
    assert!(!exec_ok(&["pgrep", "-u", "illo", "-x", "arugulad"]).is_empty(), "the daemon runs");
    assert_eq!(exec_ok(&["loginctl", "list-sessions", "--no-legend"]), "", "with nobody logged in");
    assert_eq!(exec_ok(&["loginctl", "show-user", "illo", "-p", "State", "--value"]), "lingering");
    assert_eq!(journal("saved for shutdown"), 1, "saved for shutdown");

    // The web client comes back by itself, on the same page load.
    fwd.hold(false);
    wait_up_to(Duration::from_secs(60), "the web client to reconnect", || {
        let l = watcher.last();
        l["connected"] == true && l["restored"] == true && l["marker"] == true
    });
    let lines = watcher.lines();
    assert!(lines.iter().any(|l| l["connected"] == false && l["marker"] == true), "it saw the daemon go: {lines:?}");
    assert!(lines.iter().all(|l| l["loads"] == 1), "no reload: {lines:?}");

    // The layout and directories as they were.
    assert_eq!(layout(&env), before, "the layout");

    // Scrollback with the marker and its colour; the shell is a live one
    // in the same directory.
    let text = capture(&shell);
    assert!(text.contains("red-42") && after_marker(&text).is_some(), "{text}");
    let ansi = ilg(&["capture", "--ansi", "--scrollback", &shell]).replace('\n', "");
    assert!(ansi.contains("\x1b[38;5;1mred-42") || ansi.contains("\x1b[31mred-42"), "the colour came back: {ansi:?}");
    wait_for("a prompt after the marker", || {
        after_marker(&capture(&shell)).is_some_and(|a| a.contains("illo@box-systemd"))
    });
    ilg(&["send", "-e", &shell, "echo cwd=$PWD"]);
    wait_for("the shell's directory", || {
        after_marker(&capture(&shell)).is_some_and(|a| a.contains("cwd=/home/illo/a/b/c"))
    });

    // Each pane by its policy.
    wait_for("rerun ran it again", || after_marker(&capture(&rerun)).is_some_and(|a| a.contains("rerun-5")));
    wait_for("rerun-ask asks", || {
        after_marker(&capture(&ask)).is_some_and(|a| a.contains("press Enter to re-run") && !a.contains("ask-7"))
    });
    wait_for("the hook ran", || after_marker(&capture(&hook)).is_some_and(|a| a.contains("hook-9")));
    wait_for("none waits", || after_marker(&capture(&none)).is_some_and(|a| a.contains("press Enter for a shell")));
    wait_for("the server is back", || {
        testnet::ssh()
            .args([BOX, "python3", "-c", "'import urllib.request; urllib.request.urlopen(\"http://127.0.0.1:8000/\")'"])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    });

    // The browser block has its page; the agent block its session.
    let b: Value = serde_json::from_str(&ilg(&["--json", "describe", &browser])).unwrap();
    assert_eq!(b["state"]["url"], "https://example.com/", "{b}");
    let entries = |id: &str| -> Vec<String> {
        let a: Value = serde_json::from_str(&ilg(&["--json", "describe", id])).unwrap();
        a["state"]["entries"].as_array().unwrap().iter().filter_map(|e| e["text"].as_str().map(String::from)).collect()
    };
    wait_for("the agent back", || entries(&agent).iter().any(|e| e == "Started the agent again"));
    ilg(&["call", &agent, "send", r#"{"text":"recall"}"#]);
    wait_for("the agent's context", || entries(&agent).iter().any(|e| e == "You said kestrel."));

    // A second reboot: the rerun panes still know their commands.
    reboot(&env, &fwd, restored_before + 1);
    wait_for("rerun ran it again", || capture(&rerun).matches("rerun-5").count() == 3);
    wait_for("rerun-ask asks again", || capture(&ask).matches("press Enter to re-run").count() == 2);
    wait_for("the server is back again", || {
        testnet::ssh()
            .args([BOX, "python3", "-c", "'import urllib.request; urllib.request.urlopen(\"http://127.0.0.1:8000/\")'"])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    });
    assert_eq!(layout(&env)["saved"], before["saved"], "what each pane restarts by");
}

/// `docker restart` the box (systemd shuts down and boots), with the
/// forward held off and our ssh master closed, and wait for the daemon's
/// `restored` past `restored_before`.
fn reboot(env: &Env, fwd: &Forward, restored_before: usize) {
    env.disconnect(BOX);
    fwd.hold(true);
    let st =
        Command::new("docker").args(["restart", "-t", "60", &testnet::container(BOX)]).stdout(Stdio::null()).status();
    assert!(st.unwrap().success(), "docker restart");
    wait_up_to(Duration::from_secs(90), "the daemon after the restart", || {
        journal("restored sessions=") > restored_before
    });
}
