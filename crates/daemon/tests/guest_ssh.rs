//! M65: a pane for a guest with only OpenSSH. Every test runs the system
//! `ssh` with the command `illogical share --guest` prints (plus `-F
//! /dev/null` and `BatchMode`, so the runner's own ssh config stays out of
//! it), on a pseudo-terminal, against a dev daemon.

// Over the daemon's Unix socket; Windows gets its named pipe in M56 (#219).
#![cfg(unix)]

use illogical_testkit::{listen, strays};

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

struct Daemon {
    child: Child,
    state: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        strays::remove(&self.state);
    }
}

fn have_ssh() -> bool {
    Command::new("ssh").arg("-V").stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn start(name: &str) -> Daemon {
    let state = std::env::temp_dir().join(format!("ilg-gssh-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
        .args(["--listen", listen::ANY, "--shell", "bash --norc --noprofile", "--no-manager-env"])
        .args(["--tailscale-socket", "/nonexistent/tailscaled.sock"])
        .args(["--guest-ssh", "127.0.0.1:0", "--guest-ssh-host", "127.0.0.1"])
        .arg("--state-dir")
        .arg(&state)
        .env("PS1", "$ ")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let d = Daemon { child, state };
    listen::wait_port(&d.state);
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(d.sock()).is_err() {
        assert!(Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    d
}

impl Daemon {
    fn sock(&self) -> PathBuf {
        match std::fs::read_to_string(self.state.join("sock.path")) {
            Ok(p) => PathBuf::from(p.trim()),
            Err(_) => self.state.join("sock"),
        }
    }

    fn api(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut s = UnixStream::connect(self.sock()).unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        s.write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut rest = String::new();
        r.read_to_string(&mut rest).unwrap();
        let body = rest.split_once("\r\n\r\n").map(|x| x.1).unwrap_or("");
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    fn first_pane(&self) -> u64 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (_, v) = self.api("GET", "/api/panes", None);
            if let Some(id) = v[0]["id"].as_u64() {
                return id;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn new_pane(&self) -> u64 {
        let (_, run) =
            self.api("POST", "/api/run", Some(json!({"command": "bash --norc --noprofile", "session": null})));
        run["pane"].as_u64().unwrap()
    }

    fn send(&self, pane: u64, text: &str) {
        let (s, v) = self.api("POST", &format!("/api/panes/{pane}/send"), Some(json!({"text": text, "enter": true})));
        assert_eq!(s, 200, "{v}");
    }

    fn capture(&self, pane: u64) -> String {
        let (_, v) = self.api("GET", &format!("/api/panes/{pane}/capture?scope=scrollback&format=json"), None);
        if let Some(t) = v["text"].as_str() {
            return t.to_owned();
        }
        let mut s = UnixStream::connect(self.sock()).unwrap();
        write!(s, "GET /api/panes/{pane}/capture?scope=scrollback HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    fn wait_capture(&self, pane: u64, want: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let c = self.capture(pane);
            if c.contains(want) {
                return c;
            }
            assert!(Instant::now() < deadline, "no {want:?} in %{pane}: {c}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn invite(&self, body: Value) -> Value {
        let (s, v) = self.api("POST", "/api/guests", Some(body));
        assert_eq!(s, 200, "{v}");
        v
    }
}

/// A guest: the pasted command, run by `sh` on a pseudo-terminal that is
/// its controlling terminal, so a resize reaches ssh as SIGWINCH.
struct Guest {
    child: Child,
    master: Arc<std::fs::File>,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn winsize(cols: u16, rows: u16) -> nix::pty::Winsize {
    nix::pty::Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

/// The command as a guest gets it, with the test's own options added.
fn hermetic(command: &str) -> String {
    let rest = command.strip_prefix("ssh ").expect("an ssh command");
    format!("ssh -F /dev/null -o BatchMode=yes -o ConnectTimeout=5 {rest}")
}

impl Guest {
    fn run(command: &str) -> Guest {
        let pty = nix::pty::openpty(Some(&winsize(100, 30)), None).unwrap();
        let slave = |fd: &OwnedFd| unsafe { Stdio::from_raw_fd(nix::libc::dup(fd.as_raw_fd())) };
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(hermetic(command))
            .stdin(slave(&pty.slave))
            .stdout(slave(&pty.slave))
            .stderr(slave(&pty.slave));
        cmd.env("TERM", "xterm-256color");
        unsafe {
            cmd.pre_exec(|| {
                nix::unistd::setsid().map_err(std::io::Error::from)?;
                if nix::libc::ioctl(0, nix::libc::TIOCSCTTY as _, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap();
        drop(pty.slave);
        let master = Arc::new(std::fs::File::from(pty.master));
        let out = Arc::new(Mutex::new(Vec::new()));
        let (m, o) = (master.clone(), out.clone());
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match (&*m).read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => o.lock().unwrap().extend_from_slice(&buf[..n]),
                }
            }
        });
        Guest { child, master, out }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }

    fn wait_for(&self, want: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self.text().contains(want) {
            assert!(Instant::now() < deadline, "no {want:?} for the guest: {:?}", self.text());
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn type_(&self, s: &str) {
        (&*self.master).write_all(s.as_bytes()).unwrap();
    }

    fn resize(&self, cols: u16, rows: u16) {
        let ws = winsize(cols, rows);
        assert_eq!(unsafe { nix::libc::ioctl(self.master.as_raw_fd(), nix::libc::TIOCSWINSZ as _, &ws) }, 0);
    }

    /// Its exit code, once ssh has ended.
    fn exited(&mut self, within: Duration) -> i32 {
        let deadline = Instant::now() + within;
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                // Let the reader catch up with the last of it.
                std::thread::sleep(Duration::from_millis(200));
                return s.code().unwrap_or(-1);
            }
            assert!(Instant::now() < deadline, "ssh still running: {:?}", self.text());
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn skip() -> bool {
    if !have_ssh() {
        eprintln!("skipped: no ssh client on PATH");
        return true;
    }
    false
}

#[test]
fn a_read_only_guest_watches_and_cannot_type() {
    if skip() {
        return;
    }
    let d = start("ro");
    let pane = d.first_pane();
    d.send(pane, "echo before-$((6*7))");
    d.wait_capture(pane, "before-42");
    let inv = d.invite(json!({"pane": pane}));
    let cmd = inv["command"].as_str().unwrap().to_owned();
    let token = inv["token"].as_str().unwrap().to_owned();
    assert!(cmd.contains(&format!("{token}@127.0.0.1")), "{cmd}");
    assert!(cmd.contains("StrictHostKeyChecking=yes") && cmd.contains("KnownHostsCommand="), "{cmd}");
    assert_eq!(inv["rw"], false);
    assert!(inv["fingerprint"].as_str().unwrap().starts_with("SHA256:"));

    let mut g = Guest::run(&cmd);
    // The screen as it was, then live output.
    g.wait_for("before-42");
    d.send(pane, "echo live-$((3*3))");
    g.wait_for("live-9");
    // Typing goes nowhere; the title says so.
    g.type_("echo typed-$((2*3))\r");
    g.wait_for("read-only: your keys aren't sent");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!d.capture(pane).contains("typed-6"), "a read-only guest typed");

    // The list shows one guest on it, spent, and never the token.
    let (_, list) = d.api("GET", "/api/guests", None);
    assert_eq!(list[0]["sessions"], 1, "{list}");
    assert_eq!(list[0]["used"], true);
    assert!(list[0].get("token").is_none() && list[0].get("command").is_none());

    // A single-use invite can't be used again.
    let mut again = Guest::run(&cmd);
    assert_ne!(again.exited(Duration::from_secs(10)), 0);
    assert!(again.text().contains("Permission denied"), "{:?}", again.text());

    // Ctrl-] leaves.
    g.type_("\x1d");
    assert_eq!(g.exited(Duration::from_secs(10)), 0, "{:?}", g.text());
    assert!(g.text().contains("you left"));
}

#[test]
fn a_read_write_guest_types_drives_and_sizes_the_pane() {
    if skip() {
        return;
    }
    let d = start("rw");
    let pane = d.first_pane();
    let inv = d.invite(json!({"pane": pane, "rw": true, "label": "sam", "reusable": true}));
    assert_eq!((inv["rw"].as_bool(), inv["label"].as_str()), (Some(true), Some("sam")));
    let cmd = inv["command"].as_str().unwrap().to_owned();
    let g = Guest::run(&cmd);
    g.wait_for("$ ");
    g.type_("echo rw-$((7*8))\r");
    d.wait_capture(pane, "rw-56");
    g.wait_for("rw-56");
    // Their input is theirs in the pane's driver log.
    let (_, drivers) = d.api("GET", &format!("/api/panes/{pane}/drivers"), None);
    assert!(drivers.to_string().contains("sam"), "{drivers}");
    // They drive, so their window sizes the pane.
    g.resize(91, 27);
    std::thread::sleep(Duration::from_millis(500));
    d.send(pane, "stty size");
    d.wait_capture(pane, "27 91");
    // A reusable invite lets a second guest in, who can't type over the
    // driver.
    let h = Guest::run(&cmd);
    h.wait_for("rw-56");
    h.type_("echo second-$((4*4))\r");
    h.wait_for("sam is driving this pane");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!d.capture(pane).contains("second-16"));
}

#[test]
fn revoke_expiry_and_closing_the_pane_end_sessions() {
    if skip() {
        return;
    }
    let d = start("end");
    let pane = d.first_pane();
    d.send(pane, "echo here-$((5*5))");
    d.wait_capture(pane, "here-25");

    // Revoke: cut off at once.
    let inv = d.invite(json!({"pane": pane, "rw": true}));
    let mut g = Guest::run(inv["command"].as_str().unwrap());
    g.wait_for("here-25");
    let t = Instant::now();
    let (s, _) = d.api("DELETE", &format!("/api/guests/{}", inv["id"]), None);
    assert_eq!(s, 200);
    assert_ne!(g.exited(Duration::from_secs(5)), 0);
    assert!(t.elapsed() < Duration::from_secs(3), "took {:?}", t.elapsed());
    assert!(g.text().contains("the invite was revoked"), "{:?}", g.text());

    // Expiry: a live session ends at the deadline, and nobody gets in
    // after it.
    let inv = d.invite(json!({"pane": pane, "ttl_secs": 3, "reusable": true}));
    let cmd = inv["command"].as_str().unwrap().to_owned();
    let mut g = Guest::run(&cmd);
    g.wait_for("here-25");
    assert_ne!(g.exited(Duration::from_secs(8)), 0);
    assert!(g.text().contains("the invite expired"), "{:?}", g.text());
    let mut late = Guest::run(&cmd);
    assert_ne!(late.exited(Duration::from_secs(10)), 0);
    assert!(late.text().contains("Permission denied"), "{:?}", late.text());

    // Closing the pane ends the session and the invite.
    let other = d.new_pane();
    d.send(other, "echo other-$((8*8))");
    d.wait_capture(other, "other-64");
    let inv = d.invite(json!({"pane": other, "reusable": true}));
    let mut g = Guest::run(inv["command"].as_str().unwrap());
    g.wait_for("other-64");
    let (s, v) = d.api("POST", &format!("/api/panes/{other}/close"), None);
    assert_eq!(s, 200, "{v}");
    g.exited(Duration::from_secs(5));
    assert!(g.text().contains("the pane closed"), "{:?}", g.text());
    let (_, list) = d.api("GET", "/api/guests", None);
    assert!(list.as_array().unwrap().iter().all(|x| x["pane"] != other), "{list}");
}

#[test]
fn wrong_tokens_and_other_host_keys_are_refused_and_the_port_closes() {
    if skip() {
        return;
    }
    let d = start("refuse");
    let pane = d.first_pane();
    let inv = d.invite(json!({"pane": pane}));
    let cmd = inv["command"].as_str().unwrap().to_owned();
    let token = inv["token"].as_str().unwrap();
    let port = inv["port"].as_u64().unwrap() as u16;

    // A wrong token: refused, with no password prompt to wait at.
    let wrong = cmd.replace(token, "g00000000000000000000000000000000");
    let mut g = Guest::run(&wrong);
    assert_eq!(g.exited(Duration::from_secs(10)), 255);
    assert!(g.text().contains("Permission denied"), "{:?}", g.text());

    // Something else answering on that address: the pinned key doesn't
    // match, so ssh stops before it sends the token, and the invite is
    // still unspent.
    let other = std::env::temp_dir().join(format!("ilg-gssh-otherkey-{}", std::process::id()));
    let _ = std::fs::remove_file(&other);
    let _ = std::fs::remove_file(other.with_extension("pub"));
    assert!(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&other)
            .status()
            .unwrap()
            .success()
    );
    let other_pub = std::fs::read_to_string(other.with_extension("pub")).unwrap();
    let other_key: Vec<&str> = other_pub.split_whitespace().take(2).collect();
    let known = inv["known_hosts"].as_str().unwrap();
    let ours: Vec<&str> = known.split_whitespace().skip(1).take(2).collect();
    let forged = cmd.replace(&ours.join(" "), &other_key.join(" "));
    assert_ne!(forged, cmd);
    let mut g = Guest::run(&forged);
    assert_eq!(g.exited(Duration::from_secs(10)), 255);
    assert!(g.text().contains("Host key verification failed"), "{:?}", g.text());
    let (_, list) = d.api("GET", "/api/guests", None);
    assert_eq!(list[0]["used"], false, "the token never left: {list}");
    let _ = std::fs::remove_file(&other);
    let _ = std::fs::remove_file(other.with_extension("pub"));

    // Only a pane: no commands, no sftp.
    let exec = format!("{} true", hermetic(&cmd));
    let out = Command::new("/bin/sh").arg("-c").arg(&exec).stdin(Stdio::null()).output().unwrap();
    assert!(!out.status.success(), "exec ran: {out:?}");

    // With no invite left, nothing listens.
    let (s, _) = d.api("DELETE", &format!("/api/guests/{}", inv["id"]), None);
    assert_eq!(s, 200);
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(Instant::now() < deadline, "still listening on {port}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn cli_bin() -> PathBuf {
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_illogicald")).with_file_name("illogical");
    let status = Command::new(env!("CARGO")).args(["build", "-q", "-p", "illogical"]).status().unwrap();
    assert!(status.success(), "building the CLI");
    bin
}

#[test]
fn the_cli_prints_a_command_that_works_and_lists_and_revokes() {
    if skip() {
        return;
    }
    let d = start("cli");
    let pane = d.first_pane();
    d.send(pane, "echo cli-$((9*9))");
    d.wait_capture(pane, "cli-81");
    let cli = |args: &[&str]| {
        let out = Command::new(cli_bin()).arg("--socket").arg(d.sock()).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        (String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap())
    };
    let (cmd, note) = cli(&["share", "--guest", &format!("%{pane}"), "--name", "kim", "--ttl", "10m"]);
    let cmd = cmd.trim();
    assert!(cmd.starts_with("ssh ") && cmd.ends_with("@127.0.0.1"), "{cmd}");
    assert!(note.contains("read-only") && note.contains("SHA256:"), "{note}");
    let g = Guest::run(cmd);
    g.wait_for("cli-81");
    let (list, _) = cli(&["guests"]);
    assert!(list.contains("kim") && list.contains("1 connected"), "{list}");
    cli(&["guests", "revoke", "1"]);
    let mut g = g;
    assert_ne!(g.exited(Duration::from_secs(5)), 0);
    let (list, _) = cli(&["guests"]);
    assert!(list.contains("no ssh invites"), "{list}");
}

/// The relay path (M65's next step, PLAN.md): `ssh -J <route>@<control>
/// <token>@<daemon id>` through control's jump host to a daemon behind NAT,
/// with control seeing only ssh ciphertext. Control has no ssh listener,
/// route table or raw stream kind on the dial-out mux yet.
#[test]
#[ignore = "not built: control's ssh jump host, routes and a raw dial-out stream (PLAN.md, M65's relay step)"]
fn a_guest_reaches_a_daemon_behind_nat_through_controls_jump_host() {
    unimplemented!("M65 relay step");
}
