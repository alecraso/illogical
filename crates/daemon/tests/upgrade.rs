//! M2b: restarting (or crashing) the daemon under systemd leaves the
//! programs in its panes running. Runs the real binary as a transient
//! systemd user service, so it needs a systemd user manager; it skips
//! itself where there isn't one.

mod listen;
mod strays;

use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use illogical_proto::{AttachPane, ClientMsg, Frame, FrameKind, ServerMsg, State};
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl").arg("--user").args(args).output().is_ok_and(|o| o.status.success())
}

struct Service {
    unit: String,
    state: PathBuf,
}

impl Drop for Service {
    fn drop(&mut self) {
        systemctl(&["stop", &self.unit]);
        systemctl(&["reset-failed", &self.unit]);
        strays::remove(&self.state);
    }
}

impl Service {
    fn start() -> Option<Self> {
        if !systemctl(&["show-environment"]) {
            eprintln!("no systemd user manager; skipping");
            return None;
        }
        static N: AtomicU32 = AtomicU32::new(0);
        let unit = format!("illogical-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let state = std::env::temp_dir().join(&unit);
        let ok = Command::new("systemd-run")
            .args(["--user", "--quiet", &format!("--unit={unit}")])
            .args(["-p", "Type=notify", "-p", "NotifyAccess=main", "-p", "FileDescriptorStoreMax=64"])
            .args(["-p", "KillMode=mixed", "-p", "Restart=on-failure", "-p", "RestartSec=100ms"])
            .args(["--setenv=PS1=$ ", "--"])
            .arg(env!("CARGO_BIN_EXE_illogicald"))
            .args(["--listen", listen::ANY, "--shell", "bash --norc --noprofile"])
            .args(["--no-manager-env", "--state-dir"])
            .arg(&state)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "systemd-run failed");
        Some(Self { unit: format!("{unit}.service"), state })
    }

    /// Connect on the port it took this time (each start picks one).
    async fn connect(&self) -> (Ws, State) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(port) = listen::port(&self.state)
                && let Ok(token) = std::fs::read_to_string(self.state.join("local-token"))
                && let Ok((mut ws, _)) = connect_async({
                    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
                    let mut req = format!("ws://127.0.0.1:{port}/ws").into_client_request().unwrap();
                    req.headers_mut().insert("authorization", format!("Bearer {}", token.trim()).parse().unwrap());
                    req
                })
                .await
                && let In::Msg(ServerMsg::Hello { state, .. }) = recv(&mut ws).await
            {
                return (ws, state);
            }
            assert!(Instant::now() < deadline, "daemon did not come back");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

enum In {
    Msg(ServerMsg),
    Frame(Frame),
}

async fn recv(ws: &mut Ws) -> In {
    loop {
        match timeout(Duration::from_secs(10), ws.next()).await.expect("timed out").unwrap().unwrap() {
            Message::Text(t) => return In::Msg(serde_json::from_str(&t).unwrap()),
            Message::Binary(b) => return In::Frame(Frame::decode(&b).unwrap()),
            _ => {}
        }
    }
}

async fn send(ws: &mut Ws, msg: ClientMsg) {
    ws.send(Message::Text(serde_json::to_string(&msg).unwrap().into())).await.unwrap();
}

async fn type_in(ws: &mut Ws, text: &str) {
    let f = Frame { kind: FrameKind::Input, pane: 1, offset: 0, data: text.as_bytes().to_vec() };
    ws.send(Message::Binary(f.encode().into())).await.unwrap();
}

/// Attach to pane 1 and collect its text (snapshot, then live output) until
/// `done` says so.
async fn watch(ws: &mut Ws, mut done: impl FnMut(&str) -> bool) -> String {
    send(ws, ClientMsg::Attach { panes: vec![AttachPane::new(1, None)], zstd: false, acks: false, kitty_keys: false })
        .await;
    let mut seen = String::new();
    // `recv` times out per message, and a busy pane's deltas arrive every
    // second, so give up here instead of waiting forever.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let tail = seen.len().saturating_sub(300);
        assert!(Instant::now() < deadline, "pane 1 never showed what was awaited; it ends: {:?}", &seen[tail..]);
        if let In::Frame(f) = recv(ws).await
            && f.pane == 1
        {
            seen.push_str(&String::from_utf8_lossy(&f.data));
            if done(&seen) {
                return seen;
            }
        }
    }
}

/// A loop that prints `tick-N` every 50 ms until `stop` exists. Ending it
/// with a file rather than Ctrl-C: an interrupt can land between two
/// `sleep`s, where bash doesn't always end the loop (seen on macOS).
fn tick_loop(stop: &std::path::Path) -> String {
    format!("i=0; while [ ! -e '{}' ]; do i=$((i+1)); echo tick-$i; sleep 0.05; done\r", stop.display())
}

fn stop_file() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let f = std::env::temp_dir().join(format!(
        "illogical-stop-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&f);
    f
}

fn ticks(text: &str) -> Vec<u32> {
    text.split("tick-").skip(1).filter_map(|t| t.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()).collect()
}

fn shell_pid(text: &str) -> Option<i32> {
    text.match_indices("pid=").find_map(|(i, _)| {
        let digits: String = text[i + 4..].chars().take_while(char::is_ascii_digit).collect();
        (!digits.is_empty() && text[i + 4 + digits.len()..].starts_with('x')).then(|| digits.parse().ok())?
    })
}

#[tokio::test]
async fn panes_keep_running_through_restart_and_crash() {
    let Some(svc) = Service::start() else { return };
    let (mut ws, _) = svc.connect().await;
    type_in(&mut ws, "echo pid=$((0+$$))x\r").await;
    let pid = shell_pid(&watch(&mut ws, |s| shell_pid(s).is_some()).await).unwrap();
    let stop = stop_file();
    type_in(&mut ws, &tick_loop(&stop)).await;
    watch(&mut ws, |s| ticks(s).last().is_some_and(|n| *n >= 10)).await;
    drop(ws);

    // Restart: the loop carries on, and nothing it printed is missing.
    assert!(systemctl(&["restart", &svc.unit]));
    let (mut ws, state) = svc.connect().await;
    assert!(state.panes[0].running, "adopted, still running");
    let before = Instant::now();
    let seen = watch(&mut ws, |s| ticks(s).last().is_some_and(|n| *n >= 60)).await;
    let t = ticks(&seen);
    assert!(before.elapsed() < Duration::from_secs(8), "ticking resumed");
    let first = t[0];
    let expected: Vec<u32> = (first..=*t.last().unwrap()).collect();
    let mut got = t.clone();
    got.dedup();
    assert_eq!(got, expected, "ticks across the restart are contiguous");
    drop(ws);

    // Crash: systemd restarts it and the pane is adopted again.
    assert!(systemctl(&["kill", "--kill-whom=main", "--signal=SIGKILL", &svc.unit]));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (mut ws, _) = svc.connect().await;
    watch(&mut ws, |s| ticks(s).last().is_some_and(|n| *n >= 80)).await;
    std::fs::write(&stop, "").unwrap();
    type_in(&mut ws, "echo pid=$((0+$$))x\r").await;
    let again = watch(&mut ws, |s| s.rsplit("tick-").next().is_some_and(|tail| shell_pid(tail).is_some())).await;
    let again = shell_pid(again.rsplit("tick-").next().unwrap()).unwrap();
    assert_eq!(again, pid, "the same shell, through a restart and a crash");
    let _ = std::fs::remove_file(&stop);
    drop(ws);

    // Stop is the end (like a reboot): the shell goes away.
    assert!(systemctl(&["stop", &svc.unit]));
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        assert!(Instant::now() < deadline, "shell {pid} survived a stop");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The daemon as a plain process with `--keep-panes`: no systemd, as on
/// macOS. Each pane's shim keeps its terminal while the daemon is gone.
struct Plain {
    child: Option<std::process::Child>,
    state: PathBuf,
    /// Shells to make sure of at the end, whatever happened.
    shells: Vec<i32>,
}

/// How long shims wait for a daemon in this test.
const GRACE_MS: u64 = 3000;

impl Plain {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let state = std::env::temp_dir().join(format!("illogical-keep-{}-{n}", std::process::id()));
        let mut d = Self { child: None, state, shells: vec![] };
        d.start();
        d
    }

    fn start(&mut self) {
        // Not the last one's port.
        let _ = std::fs::remove_file(self.state.join("listen"));
        let child = Command::new(env!("CARGO_BIN_EXE_illogicald"))
            .args(["--listen", listen::ANY, "--shell", "bash --norc --noprofile"])
            .args(["--no-manager-env", "--keep-panes", "--state-dir"])
            .arg(&self.state)
            .env("PS1", "$ ")
            .env("ILLOGICAL_KEEP_GRACE_MS", GRACE_MS.to_string())
            .env_remove("NOTIFY_SOCKET")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        self.child = Some(child);
    }

    fn signal(&mut self, sig: nix::sys::signal::Signal) {
        let mut child = self.child.take().expect("running");
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.id() as i32), sig).unwrap();
        child.wait().unwrap();
    }

    async fn connect(&self) -> (Ws, State) {
        let svc = Service { unit: String::new(), state: self.state.clone() };
        let r = svc.connect().await;
        std::mem::forget(svc);
        r
    }
}

impl Drop for Plain {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        for pid in &self.shells {
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(*pid), nix::sys::signal::Signal::SIGKILL);
        }
        strays::remove(&self.state);
    }
}

fn alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

#[tokio::test]
async fn shims_keep_panes_without_systemd() {
    use nix::sys::signal::Signal;
    let mut d = Plain::new();
    let (mut ws, _) = d.connect().await;
    type_in(&mut ws, "echo pid=$((0+$$))x\r").await;
    let pid = shell_pid(&watch(&mut ws, |s| shell_pid(s).is_some()).await).unwrap();
    d.shells.push(pid);
    let stop = stop_file();
    type_in(&mut ws, &tick_loop(&stop)).await;
    watch(&mut ws, |s| ticks(s).last().is_some_and(|n| *n >= 10)).await;
    drop(ws);

    // A clean stop and a start (an upgrade): the loop carries on, and
    // nothing it printed in between is missing.
    d.signal(Signal::SIGTERM);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(alive(pid), "the shell outlived the daemon");
    d.start();
    let (mut ws, state) = d.connect().await;
    assert!(state.panes[0].running, "adopted, still running");
    let seen = watch(&mut ws, |s| ticks(s).last().is_some_and(|n| *n >= 60)).await;
    let t = ticks(&seen);
    let expected: Vec<u32> = (t[0]..=*t.last().unwrap()).collect();
    let mut got = t.clone();
    got.dedup();
    assert_eq!(got, expected, "ticks across the restart are contiguous");
    drop(ws);

    // A crash: the same shell is adopted again.
    d.signal(Signal::SIGKILL);
    d.start();
    let (mut ws, _) = d.connect().await;
    watch(&mut ws, |s| ticks(s).last().is_some_and(|n| *n >= 80)).await;
    std::fs::write(&stop, "").unwrap();
    type_in(&mut ws, "echo pid=$((0+$$))x\r").await;
    let again = watch(&mut ws, |s| s.rsplit("tick-").next().is_some_and(|tail| shell_pid(tail).is_some())).await;
    let again = shell_pid(again.rsplit("tick-").next().unwrap()).unwrap();
    assert_eq!(again, pid, "the same shell, through a restart and a crash");
    let _ = std::fs::remove_file(&stop);
    drop(ws);

    // Stopped for good: once no daemon has come back within the grace
    // period, the shim hangs the pane up.
    d.signal(Signal::SIGTERM);
    tokio::time::sleep(Duration::from_millis(GRACE_MS / 2)).await;
    assert!(alive(pid), "kept during the grace period");
    let deadline = Instant::now() + Duration::from_millis(GRACE_MS + 6000);
    while alive(pid) {
        assert!(Instant::now() < deadline, "shell {pid} outlived the grace period");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
