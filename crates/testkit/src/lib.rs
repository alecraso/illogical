//! The harness for arugulad's integration tests (#200): a dev daemon with
//! a state dir and socket of its own, the requests tests make to it, waits,
//! and cleanup when it's dropped. `docs/testing.md` has how to use it.
//!
//! ```ignore
//! let d = arugula_testkit::arugulad!("api").env("PS1", "$ ").start();
//! let pane = d.post("/api/run", json!({"command": "exit 4"}))["pane"].as_u64().unwrap();
//! assert_eq!(d.get(&format!("/api/panes/{pane}/wait?until=exit&timeout=10"))["code"], 4);
//! ```

pub mod listen;
pub mod strays;

use std::{
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

#[cfg(unix)]
use nix::{sys::signal::Signal, unistd::Pid};
use serde_json::Value;

/// A [`Builder`] for the calling package's `arugulad`
/// (`CARGO_BIN_EXE_arugulad`, so only the daemon's own tests can use it),
/// with its state dir named after `tag`.
#[macro_export]
macro_rules! arugulad {
    ($tag:expr) => {
        $crate::Builder::new(env!("CARGO_BIN_EXE_arugulad"), $tag)
    };
}

/// The shell test daemons run unless told otherwise: no rc files, so
/// nothing of the user's gets in.
#[cfg(unix)]
pub const SHELL: &str = "bash --norc --noprofile";
/// Windows: PowerShell 7 without its profile or banner.
#[cfg(windows)]
pub const SHELL: &str = "pwsh -NoLogo -NoProfile";

/// A connection to the daemon's local socket: its Unix socket, or on
/// Windows its named pipe (opened as a file: a request and its answer, one
/// after the other, need nothing more).
#[cfg(unix)]
fn connect(sock: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    std::os::unix::net::UnixStream::connect(sock)
}

#[cfg(windows)]
fn connect(sock: &Path) -> std::io::Result<std::fs::File> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(sock) {
            // Every instance busy: the next one is a moment away.
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2))
            }
            r => return r,
        }
    }
}

/// How long a daemon has to come up.
const START: Duration = Duration::from_secs(15);

/// How a test daemon starts. Every one gets `--listen 127.0.0.1:0`,
/// `--no-manager-env` and a `--state-dir` of its own, its output goes
/// nowhere, and `NOTIFY_SOCKET` is taken out of its environment (a test run
/// from a service would pass its own on). `ARUGULA_CHANT` is empty, so
/// it never reads the host's agent config with chant. The rest is the
/// test's to say.
#[derive(Clone, Debug)]
pub struct Builder {
    bin: PathBuf,
    tag: String,
    state: Option<PathBuf>,
    shell: Option<String>,
    args: Vec<OsString>,
    env: Vec<(OsString, Option<OsString>)>,
    block_listen: bool,
    wait: Duration,
}

impl Builder {
    pub fn new(bin: impl Into<PathBuf>, tag: &str) -> Self {
        Self {
            bin: bin.into(),
            tag: tag.to_owned(),
            state: None,
            shell: Some(SHELL.to_owned()),
            args: vec![],
            // No `chant audit --agents` of the host's agent config (#145):
            // every screen rule set runs. A test that wants an inventory
            // points this at a stand-in chant.
            env: vec![("ARUGULA_CHANT".into(), Some("".into()))],
            block_listen: false,
            wait: Duration::from_secs(15),
        }
    }

    /// Use this state dir instead of a fresh one under the temp dir. It
    /// isn't emptied first; it's still removed when the daemon is dropped.
    pub fn state_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.state = Some(dir.into());
        self
    }

    /// `--shell`, instead of [`SHELL`].
    pub fn shell(mut self, shell: &str) -> Self {
        self.shell = Some(shell.to_owned());
        self
    }

    /// No `--shell`: the daemon picks, as it does for a user.
    pub fn default_shell(mut self) -> Self {
        self.shell = None;
        self
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.extend(args.into_iter().map(|a| a.as_ref().to_owned()));
        self
    }

    /// `--wisp-token-file /nonexistent`: no machines, whatever this host has.
    pub fn no_wisp(self) -> Self {
        self.args(["--wisp-token-file", "/nonexistent"])
    }

    /// `--tailscale-socket` pointing nowhere: tailscaled kept out of it.
    pub fn no_tailscale(self) -> Self {
        self.args(["--tailscale-socket", "/nonexistent/tailscaled.sock"])
    }

    /// `--block-listen 127.0.0.1:0` too; [`Daemon::block_port`] is its port.
    pub fn block_listen(mut self) -> Self {
        self.block_listen = true;
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env.push((key.as_ref().to_owned(), Some(value.as_ref().to_owned())));
        self
    }

    pub fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (k, v) in vars {
            self = self.env(k, v);
        }
        self
    }

    pub fn env_remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.env.push((key.as_ref().to_owned(), None));
        self
    }

    /// The daemon's `PATH` (and so its panes' and the programs it looks
    /// for), instead of the test's. A test that must not find something
    /// installed on this host sets one without it.
    pub fn path(self, path: impl AsRef<OsStr>) -> Self {
        self.env("PATH", path)
    }

    /// How long [`Daemon::wait_for`] waits (15 s unless set).
    pub fn wait_secs(mut self, secs: u64) -> Self {
        self.wait = Duration::from_secs(secs);
        self
    }

    /// Start it as a child of the test, and wait until it answers.
    pub fn start(self) -> Daemon {
        let mut d = self.daemon();
        d.start();
        d
    }

    /// Start it as a transient systemd user service (its FD store and
    /// scopes as in production), and wait until it answers. `None`, saying
    /// so, without a user manager. Variables taken out with
    /// [`Builder::env_remove`] don't apply: a service starts with systemd's
    /// environment, plus `PATH` (the test's unless set) and what was set.
    pub fn service(self) -> Option<Daemon> {
        if !systemctl(&["show-environment"]) {
            eprintln!("no systemd user manager; skipping");
            return None;
        }
        let mut d = self.daemon();
        let unit = format!("arugula-test-{}", d.state.file_name().unwrap().to_string_lossy());
        let path = d.b.env.iter().rev().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone());
        let path = path.unwrap_or_else(|| std::env::var_os("PATH")).unwrap_or_default();
        let mut c = Command::new("systemd-run");
        c.args(["--user", "--quiet", &format!("--unit={unit}")])
            .args(["-p", "Type=notify", "-p", "NotifyAccess=main", "-p", "FileDescriptorStoreMax=64"])
            .args(["-p", "KillMode=mixed", "-p", "Restart=on-failure", "-p", "RestartSec=100ms"])
            .arg(setenv(OsStr::new("PATH"), &path));
        for (k, v) in &d.b.env {
            if let Some(v) = v
                && k != "PATH"
            {
                c.arg(setenv(k, v));
            }
        }
        c.arg("--").arg(&d.b.bin).args(d.args(listen::ANY, listen::ANY));
        assert!(c.status().is_ok_and(|s| s.success()), "systemd-run failed");
        d.run = Run::Service(format!("{unit}.service"));
        d.port = listen::wait_port(&d.state);
        if d.b.block_listen {
            d.block_port = listen::wait_block_port(&d.state);
        }
        d.wait_up();
        Some(d)
    }

    fn daemon(self) -> Daemon {
        let state = match &self.state {
            Some(s) => s.clone(),
            None => {
                static N: AtomicU32 = AtomicU32::new(0);
                // Short: Unix socket paths are limited to about 100 bytes.
                let n = N.fetch_add(1, Ordering::Relaxed);
                let s = std::env::temp_dir().join(format!("ilg-{}-{}-{n}", self.tag, std::process::id()));
                let _ = std::fs::remove_dir_all(&s);
                s
            }
        };
        Daemon { port: 0, block_port: 0, state, run: Run::Child(None), b: self }
    }
}

fn setenv(k: &OsStr, v: &OsStr) -> OsString {
    let mut s = OsString::from("--setenv=");
    s.push(k);
    s.push("=");
    s.push(v);
    s
}

enum Run {
    /// A child of the test: what it started goes with it.
    Child(Option<Child>),
    /// A transient systemd user service, by unit name.
    Service(String),
}

/// A running test daemon. Dropping it kills it, kills what its panes left
/// running and removes its state dir; with `ARUGULA_KEEP_TEST_STATE` set
/// the dir stays, and its path is printed.
pub struct Daemon {
    /// Its TCP port (`--listen`).
    pub port: u16,
    /// The block sites' port (`--block-listen`), or 0.
    pub block_port: u16,
    pub state: PathBuf,
    run: Run,
    b: Builder,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.halt();
        if std::env::var_os("ARUGULA_KEEP_TEST_STATE").is_some() {
            strays::kill_programs(&self.state);
            eprintln!("kept {}", self.state.display());
            return;
        }
        strays::remove(&self.state);
    }
}

impl Daemon {
    fn args(&self, listen: &str, block_listen: &str) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["--listen".into(), listen.into()];
        if self.b.block_listen {
            a.extend(["--block-listen".into(), block_listen.into()]);
        }
        if let Some(shell) = &self.b.shell {
            a.extend(["--shell".into(), shell.into()]);
        }
        a.push("--no-manager-env".into());
        a.extend(self.b.args.iter().cloned());
        a.extend(["--state-dir".into(), self.state.clone().into()]);
        a
    }

    /// Start it again after [`Daemon::stop`], on the ports it had (a
    /// daemon's first start is [`Builder::start`]).
    pub fn start(&mut self) {
        assert!(matches!(self.run, Run::Child(None)), "start: it's running");
        let first = self.port == 0;
        if first {
            let _ = std::fs::remove_file(self.state.join("listen"));
            let _ = std::fs::remove_file(self.state.join("block-listen"));
        }
        let at = |p: u16| if p == 0 { listen::ANY.to_owned() } else { format!("127.0.0.1:{p}") };
        let mut c = Command::new(&self.b.bin);
        c.args(self.args(&at(self.port), &at(self.block_port)))
            .env_remove("NOTIFY_SOCKET")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in &self.b.env {
            match v {
                Some(v) => c.env(k, v),
                None => c.env_remove(k),
            };
        }
        self.run = Run::Child(Some(c.spawn().unwrap()));
        if first {
            self.port = self.wait_port("listen");
            if self.b.block_listen {
                self.block_port = self.wait_port("block-listen");
            }
        }
        self.wait_up();
    }

    fn wait_port(&mut self, file: &str) -> u16 {
        let deadline = Instant::now() + START;
        loop {
            self.check_alive();
            let port = std::fs::read_to_string(self.state.join(file)).ok();
            if let Some(p) = port.and_then(|a| a.trim().rsplit(':').next()?.parse().ok()) {
                return p;
            }
            assert!(Instant::now() < deadline, "daemon did not start: no port in {}", self.state.join(file).display());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn check_alive(&mut self) {
        if let Run::Child(Some(c)) = &mut self.run
            && let Some(status) = c.try_wait().unwrap()
        {
            self.run = Run::Child(None);
            panic!("daemon exited: {status}");
        }
    }

    /// Wait until it answers on its socket, and (as a child) on its ports.
    /// A service picks new ports each time it starts, so [`Daemon::port`]
    /// is only its first one's.
    pub fn wait_up(&self) {
        let deadline = Instant::now() + START;
        let child = matches!(self.run, Run::Child(_));
        let tcp = |port: u16| port == 0 || TcpStream::connect(("127.0.0.1", port)).is_ok();
        while connect(&self.sock()).is_err()
            || self.raw("GET", "/api/panes", None).0 != 200
            || (child && !(tcp(self.port) && tcp(self.block_port)))
        {
            assert!(Instant::now() < deadline, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Set a variable for the next [`Daemon::start`] (one that needs the
    /// port the first start took, say).
    pub fn set_env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        let key = key.as_ref().to_owned();
        self.b.env.retain(|(k, _)| *k != key);
        self.b.env.push((key, Some(value.as_ref().to_owned())));
    }

    /// Stop it the way systemd or a reboot does (SIGTERM: it saves first,
    /// and what it started goes too), and wait for it to exit.
    #[cfg(unix)]
    pub fn stop(&mut self) {
        self.signal(Signal::SIGTERM);
    }

    /// Kill it (SIGKILL: no chance to save), and wait.
    #[cfg(unix)]
    pub fn kill(&mut self) {
        self.signal(Signal::SIGKILL);
    }

    /// Windows: TerminateProcess, which gives it no chance to save (a
    /// console control event can't reach a child with no console).
    #[cfg(windows)]
    pub fn kill(&mut self) {
        let Run::Child(c) = &mut self.run else { panic!("no services on Windows") };
        let mut c = c.take().expect("it isn't running");
        c.kill().unwrap();
        c.wait().unwrap();
    }

    /// Send it a signal it exits on, and wait for it to.
    #[cfg(unix)]
    pub fn signal(&mut self, signal: Signal) {
        let Run::Child(c) = &mut self.run else { panic!("a service stops with systemctl") };
        let mut c = c.take().expect("it isn't running");
        nix::sys::signal::kill(Pid::from_raw(c.id() as i32), signal).unwrap();
        c.wait().unwrap();
    }

    /// The daemon's pid, while it runs as a child.
    pub fn pid(&self) -> Option<u32> {
        match &self.run {
            Run::Child(c) => c.as_ref().map(|c| c.id()),
            Run::Service(_) => None,
        }
    }

    /// The systemd unit, when it runs as a service.
    pub fn unit(&self) -> Option<&str> {
        match &self.run {
            Run::Service(u) => Some(u),
            Run::Child(_) => None,
        }
    }

    /// `systemctl --user restart` its unit, and wait until it's back.
    pub fn restart_service(&self) {
        let unit = self.unit().expect("not a service");
        assert!(systemctl(&["restart", unit]));
        std::thread::sleep(Duration::from_millis(300));
        self.wait_up();
    }

    /// Kill it if it's running, as dropping it does, but keep its state.
    pub fn halt(&mut self) {
        match &mut self.run {
            Run::Child(c) => {
                if let Some(mut c) = c.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
            Run::Service(unit) => {
                systemctl(&["stop", unit]);
                systemctl(&["reset-failed", unit]);
            }
        }
    }

    /// Its Unix socket (in the state dir, or where `sock.path` says when
    /// that path would be too long).
    pub fn sock(&self) -> PathBuf {
        match std::fs::read_to_string(self.state.join("sock.path")) {
            Ok(p) => PathBuf::from(p.trim()),
            Err(_) => self.state.join("sock"),
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The local token loopback callers show (from `local-token`, or the
    /// file `ARUGULA_LOCAL_TOKEN_FILE` names if the test set that).
    pub fn token(&self) -> String {
        let set = self.b.env.iter().rev().find(|(k, _)| k == "ARUGULA_LOCAL_TOKEN_FILE").and_then(|(_, v)| v.clone());
        let file = set.map(PathBuf::from).unwrap_or_else(|| self.state.join("local-token"));
        std::fs::read_to_string(file).unwrap_or_default().trim().to_owned()
    }

    /// `Authorization` with the local token.
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.token())
    }

    /// A WebSocket request to `path` on the TCP port, with the local token.
    pub fn ws(&self, path: &str) -> tokio_tungstenite::tungstenite::handshake::client::Request {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://127.0.0.1:{}{path}", self.port).into_client_request().unwrap();
        req.headers_mut().insert("authorization", self.bearer().parse().unwrap());
        req
    }

    /// One request over the socket (the owner's, so no credential): status
    /// and body, chunks joined. Status 0 if it doesn't answer.
    pub fn raw(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String) {
        let Ok(mut s) = connect(&self.sock()) else { return (0, String::new()) };
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let _ = s.write_all(
            format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        let mut r = BufReader::new(s);
        let mut line = String::new();
        if r.read_line(&mut line).is_err() || line.is_empty() {
            return (0, String::new());
        }
        let status = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut chunked = false;
        loop {
            line.clear();
            r.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            chunked |= line.to_ascii_lowercase().starts_with("transfer-encoding: chunked");
        }
        let mut out = Vec::new();
        if chunked {
            loop {
                line.clear();
                r.read_line(&mut line).unwrap();
                let n = usize::from_str_radix(line.trim(), 16).unwrap_or(0);
                if n == 0 {
                    break;
                }
                let mut chunk = vec![0; n + 2];
                r.read_exact(&mut chunk).unwrap();
                out.extend_from_slice(&chunk[..n]);
            }
        } else {
            r.read_to_end(&mut out).unwrap();
        }
        (status, String::from_utf8_lossy(&out).into_owned())
    }

    /// GET over the socket, which must answer 200: its JSON, or its text as
    /// a JSON string.
    pub fn get(&self, path: &str) -> Value {
        let (status, body) = self.raw("GET", path, None);
        assert_eq!(status, 200, "{path}: {body}");
        serde_json::from_str(&body).unwrap_or(Value::String(body))
    }

    /// POST JSON over the socket, which must answer 200 with JSON.
    pub fn post(&self, path: &str, body: Value) -> Value {
        let (status, text) = self.raw("POST", path, Some(body));
        assert_eq!(status, 200, "{path}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    /// One request over TCP with these headers and no others (no
    /// credential unless given; `Host` is `127.0.0.1:<port>` unless given):
    /// status, head (lowercase) and body.
    pub fn tcp(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<&str>) -> (u16, String, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        let body = body.unwrap_or_default();
        let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            req.push_str(&format!("Host: 127.0.0.1:{}\r\n", self.port));
        }
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(body);
        s.write_all(req.as_bytes()).unwrap();
        let mut res = Vec::new();
        s.read_to_end(&mut res).unwrap();
        let res = String::from_utf8_lossy(&res).into_owned();
        let (head, body) = res.split_once("\r\n\r\n").unwrap_or((&res, ""));
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, head.to_ascii_lowercase(), body.to_owned())
    }

    /// Wait until `f` holds, for as long as the builder said (15 s unless
    /// set).
    pub fn wait_for(&self, what: &str, f: impl FnMut() -> bool) {
        wait_for(what, self.b.wait, f);
    }
}

/// Wait until `f` holds, checking every 50 ms; panics after `timeout`.
pub fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `systemctl --user`, and whether it worked.
pub fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl").arg("--user").args(args).output().is_ok_and(|o| o.status.success())
}

/// A test's scratch dir (fake tools, a forge's files), by its real path: on
/// macOS the temp dir is /var, which is /private/var. Deleted when dropped.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("ilg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d.canonicalize().unwrap())
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
