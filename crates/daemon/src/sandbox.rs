//! Sandboxes (M4a): `arugulad install --tailnet AUTHKEY` puts a machine
//! without systemd (a wisp or Fly sprite, a container) on the tailnet and
//! runs the daemon there; `arugulad sandbox` is what keeps both running.
//!
//! - **tailscaled** runs in userspace (`--tun=userspace-networking`: no
//!   TUN device or root needed) with its own state and socket, separate
//!   from any system tailscaled. It joins with the given (ephemeral,
//!   `tag:sandbox`) auth key, and `tailscale serve` gives the daemon an
//!   https URL. Its netstack also forwards the daemon's port on the tailnet
//!   address to loopback, which the daemon handles by asking tailscaled who
//!   is connecting (`tailscale.rs`).
//! - **The daemon** listens on loopback, lets in the owner's login (a
//!   tagged node has none of its own), and accepts pages from the home
//!   daemon's origin.
//! - **The home daemon's list:** with an invite (`--join`, from `arugula
//!   hosts invite`), the sandbox adds itself; otherwise it prints what to
//!   add. Outbound tailnet traffic goes through tailscaled's proxy (in
//!   userspace mode nothing else reaches the tailnet).
//! - **Supervision:** `arugulad sandbox` runs both and restarts either if
//!   it exits. `install` starts it detached; a provider's service manager
//!   (M4b) can run it in the foreground. It doesn't come back by itself
//!   after a reboot: run `arugulad sandbox` again (or register it).
//!
//! The auth key never goes on a command line (`ps` would show it): it is
//! read from a file (`file:PATH`), stdin (`-`), or the argument, and handed
//! to `tailscale up` as `--auth-key=file:…`.

use std::{
    fs,
    io::Read,
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use arugula_proto::hosts::{AddHost, Joined, Transport};
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde::{Deserialize, Serialize};

use crate::tailscale::LocalApi;

/// tailscaled's HTTP and SOCKS5 proxy, the way out to the tailnet.
const PROXY: &str = "127.0.0.1:1055";

/// What `arugulad sandbox` runs, written by `install --tailnet`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Our own tailscaled and its CLI.
    pub tailscaled: PathBuf,
    pub tailscale: PathBuf,
    /// Its state, socket and logs, and ours.
    pub dir: PathBuf,
    /// The daemon's arguments; none until install has them all.
    #[serde(default)]
    pub daemon: Option<Vec<String>>,
}

pub struct TailnetOpts {
    /// The auth key, `file:PATH`, or `-` for stdin.
    pub authkey: String,
    pub hostname: Option<String>,
    /// The home daemon's URL (its page's origin).
    pub home: Option<String>,
    /// An invite from the home daemon (or `file:PATH`).
    pub join: Option<String>,
    pub owner: Option<String>,
    pub port: u16,
    pub no_serve: bool,
    pub daemon_args: Vec<String>,
}

fn home_dir() -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?))
}

// A sandbox installed before the rename keeps its config, and its
// tailscaled's state (so it stays the same tailnet node) under the old
// names (#505).
fn config_path(home: &Path) -> PathBuf {
    arugula_proto::dirs::config_dir().unwrap_or_else(|| home.join(".config/arugula")).join("sandbox.json")
}

/// tailscaled's state is in it: one illogical made is used where it is
/// (#505), so the sandbox stays the same node.
fn sandbox_dir(home: &Path) -> PathBuf {
    arugula_proto::dirs::state_home("arugula-sandbox", "illogical-sandbox")
        .unwrap_or_else(|| home.join(".local/state/arugula-sandbox"))
}

/// Where a downloaded tailscale is kept (#505: the old place, if it's there).
fn tailscale_lib(home: &Path) -> PathBuf {
    arugula_core::rename::kept(home.join(".local/lib/arugula/tailscale"), home.join(".local/lib/illogical/tailscale"))
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let path = config_path(&home_dir()?);
        let bytes =
            fs::read(&path).with_context(|| format!("reading {} (run `install --tailnet` first)", path.display()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn save(&self) -> anyhow::Result<()> {
        let path = config_path(&home_dir()?);
        fs::create_dir_all(path.parent().unwrap())?;
        crate::store::write_atomic(&path, &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("tailscaled.sock")
    }

    fn tailscale(&self) -> Command {
        let mut c = Command::new(&self.tailscale);
        c.arg(format!("--socket={}", self.socket().display()));
        c
    }

    fn pidfile(&self) -> PathBuf {
        self.dir.join("supervisor.pid")
    }
}

// ---------------------------------------------------------------- install

pub fn install(opts: TailnetOpts) -> anyhow::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(install_async(opts))
}

async fn install_async(opts: TailnetOpts) -> anyhow::Result<()> {
    let home = home_dir()?;
    let exe = crate::install::copy_binaries(&home)?;
    let dir = sandbox_dir(&home);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let (tailscaled, tailscale) = tailscale_binaries(&home).await?;

    // Start the supervisor, with tailscaled alone on a first install: the
    // daemon's arguments need what joining the tailnet tells us. Installing
    // again leaves it (and tailscaled's connection) running; the daemon is
    // restarted at the end.
    let mut config = Config { tailscaled, tailscale, dir, daemon: None };
    if let Ok(old) = Config::load() {
        config.daemon = old.daemon;
    }
    config.save()?;
    if running_supervisor(&config).is_none() {
        start_supervisor(&exe, &config)?;
    }

    let api = LocalApi::find(Some(&config.socket())).expect("an explicit socket");
    // Until it has settled (a restarted one says `Starting` for a moment
    // before `Running`), we can't tell whether it needs logging in.
    wait_for("tailscaled", Duration::from_secs(30), || async {
        api.backend_state().await.is_ok_and(|s| !matches!(s.as_str(), "" | "NoState" | "Starting"))
    })
    .await?;
    if api.backend_state().await? != "Running" {
        let key = KeyFile::new(&opts.authkey, &config.dir)?;
        let mut up = config.tailscale();
        up.arg("up").arg(format!("--auth-key=file:{}", key.path.display())).arg("--timeout=60s");
        if let Some(h) = &opts.hostname {
            up.arg(format!("--hostname={h}"));
        }
        println!("joining the tailnet…");
        let ok = up.status().context("running tailscale up")?.success();
        drop(key);
        if !ok {
            bail!("tailscale up failed");
        }
    }
    let status = api.status().await?;
    let name = opts.hostname.clone().unwrap_or_else(|| status.host.split('.').next().unwrap_or("sandbox").to_owned());
    println!("on the tailnet as {}", status.host);

    // https through serve; plain http on the port as well (the netstack
    // forwards it, and the daemon asks who is connecting).
    let mut urls = vec![];
    if !opts.no_serve {
        match serve(&config, opts.port) {
            Ok(()) => urls.push(format!("https://{}", status.host)),
            Err(e) => println!("warning: tailscale serve didn't work ({e:#}); only plain http"),
        }
    }
    urls.push(format!("http://{}:{}", status.host, opts.port));

    // Join the home daemon's list, learning its owner on the way.
    let mut owner = opts.owner.clone();
    let mut joined = false;
    if let (Some(home_url), Some(token)) = (&opts.home, &opts.join) {
        let token = read_secret(token)?;
        match join(
            home_url,
            &token,
            AddHost { name: name.clone(), urls: urls.clone(), transport: Transport::Tailnet, ssh: None },
        )
        .await
        {
            Ok(j) => {
                println!("added to {home_url}'s hosts as {}", j.host.name);
                owner = owner.or(j.owner);
                joined = true;
            }
            Err(e) => println!("warning: couldn't join {home_url} ({e:#})"),
        }
    }
    let owner = owner.context("who may use this daemon? pass --owner LOGIN (or --home and --join to learn it)")?;

    let mut args = vec![
        "--listen".into(),
        format!("127.0.0.1:{}", opts.port),
        "--tailscale-socket".into(),
        config.socket().display().to_string(),
        "--owner".into(),
        owner.clone(),
        "--name".into(),
        name.clone(),
    ];
    if let Some(h) = &opts.home {
        args.extend(["--allow-origin".into(), origin(h)?]);
    }
    args.extend(opts.daemon_args.iter().cloned());
    config.daemon = Some(args);
    config.save()?;
    // The supervisor picks up the daemon's arguments.
    signal_supervisor(&config, Signal::SIGHUP)?;

    let http = crate::roots::client();
    let local = format!("http://127.0.0.1:{}/api/host", opts.port);
    wait_for("the daemon", Duration::from_secs(30), || async {
        http.get(&local).send().await.is_ok_and(|r| r.status().is_success())
    })
    .await?;

    println!("\narugulad is running, for {owner}:");
    for u in &urls {
        println!("  {u}");
    }
    if !joined {
        println!("\nTo list it on the home daemon, run there:\n  arugula hosts add {name} {}", urls.join(" "));
        if opts.home.is_none() {
            println!("and reinstall here with --home URL, so the home daemon's page may connect.");
        }
    }
    println!("\nLogs: {}. After a reboot: arugulad sandbox &", config.dir.display());
    Ok(())
}

/// The page origin of a URL (what a browser sends as Origin).
fn origin(url: &str) -> anyhow::Result<String> {
    let u = reqwest::Url::parse(url).with_context(|| format!("bad URL {url}"))?;
    Ok(u.origin().ascii_serialization())
}

/// Join the home daemon's list. A tailscaled that just started (or a
/// sprite that just woke) may need a few seconds before the tailnet path
/// works, so failing to connect is tried again for about a minute.
async fn join(home: &str, token: &str, host: AddHost) -> anyhow::Result<Joined> {
    let mut tries = 0;
    loop {
        match join_once(home, token, &host).await {
            Err(e)
                if tries < 5
                    && e.downcast_ref::<reqwest::Error>().is_some_and(|e| e.is_connect() || e.is_timeout()) =>
            {
                tries += 1;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            r => return r,
        }
    }
}

async fn join_once(home: &str, token: &str, host: &AddHost) -> anyhow::Result<Joined> {
    // In userspace mode the tailnet is only reachable through tailscaled.
    let http = crate::roots::http()
        .proxy(reqwest::Proxy::all(format!("http://{PROXY}"))?)
        .timeout(Duration::from_secs(10))
        .build()?;
    let res = http
        .post(format!("{}/api/hosts/join", home.trim_end_matches('/')))
        .json(&serde_json::json!({ "token": token, "host": host }))
        .send()
        .await?;
    let status = res.status();
    let body = res.text().await?;
    if !status.is_success() {
        bail!("HTTP {status}: {body}");
    }
    Ok(serde_json::from_str(&body)?)
}

fn serve(config: &Config, port: u16) -> anyhow::Result<()> {
    let mut c = config.tailscale();
    c.args(["serve", "--bg", "--yes", "--https=443", &format!("http://127.0.0.1:{port}")]);
    // It asks (and waits) when HTTPS isn't enabled for the tailnet.
    let mut child = c.stdin(Stdio::null()).spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(s) = child.try_wait()? {
            return if s.success() { Ok(()) } else { bail!("exit {s}") };
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out (is HTTPS enabled for the tailnet?)");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

async fn wait_for<F, Fut>(what: &str, within: Duration, mut ready: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + within;
    while !ready().await {
        if Instant::now() > deadline {
            bail!("{what} didn't come up (logs in {})", sandbox_dir(&home_dir()?).display());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
}

/// A secret given as `file:PATH`, `-` (stdin) or itself.
fn read_secret(arg: &str) -> anyhow::Result<String> {
    let s = if let Some(path) = arg.strip_prefix("file:") {
        fs::read_to_string(path).with_context(|| format!("reading {path}"))?
    } else if arg == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else {
        arg.to_owned()
    };
    Ok(s.trim().to_owned())
}

/// The auth key in a private file for `tailscale up`, removed afterwards
/// (unless it was the caller's own file).
struct KeyFile {
    path: PathBuf,
    ours: bool,
}

impl KeyFile {
    fn new(arg: &str, dir: &Path) -> anyhow::Result<Self> {
        if let Some(path) = arg.strip_prefix("file:") {
            return Ok(Self { path: PathBuf::from(path), ours: false });
        }
        let key = read_secret(arg)?;
        let path = dir.join(".authkey");
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
        std::io::Write::write_all(&mut f, key.as_bytes())?;
        Ok(Self { path, ours: true })
    }
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        if self.ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// tailscaled and tailscale from PATH, else the static build from
/// pkgs.tailscale.com, kept in `~/.local/lib/arugula/tailscale`.
async fn tailscale_binaries(home: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    let lib = tailscale_lib(home);
    let (d, c) = (lib.join("tailscaled"), lib.join("tailscale"));
    if d.exists() && c.exists() {
        return Ok((d, c));
    }
    if let (Some(d), Some(c)) = (which("tailscaled"), which("tailscale")) {
        return Ok((d, c));
    }
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        a => bail!("no tailscale download for {a}"),
    };
    println!("downloading tailscale…");
    let http = crate::roots::client();
    let index: serde_json::Value =
        http.get("https://pkgs.tailscale.com/stable/?mode=json").send().await?.error_for_status()?.json().await?;
    let file = index["Tarballs"][arch].as_str().context("no tarball in pkgs.tailscale.com's index")?;
    let tgz =
        http.get(format!("https://pkgs.tailscale.com/stable/{file}")).send().await?.error_for_status()?.bytes().await?;
    fs::create_dir_all(&lib)?;
    let tmp = lib.join("download");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    fs::write(tmp.join("tailscale.tgz"), &tgz)?;
    let ok = Command::new("tar").arg("xzf").arg("tailscale.tgz").current_dir(&tmp).status()?.success();
    if !ok {
        bail!("unpacking {file}");
    }
    let unpacked = tmp.join(file.trim_end_matches(".tgz"));
    for (bin, dest) in [("tailscaled", &d), ("tailscale", &c)] {
        fs::rename(unpacked.join(bin), dest)?;
        fs::set_permissions(dest, fs::Permissions::from_mode(0o755))?;
    }
    fs::remove_dir_all(&tmp)?;
    println!("installed {file} in {}", lib.display());
    Ok((d, c))
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

// ---------------------------------------------------------------- supervisor

fn running_supervisor(config: &Config) -> Option<Pid> {
    let pid: i32 = fs::read_to_string(config.pidfile()).ok()?.trim().parse().ok()?;
    let pid = Pid::from_raw(pid);
    // Alive, and ours (not a recycled pid).
    let argv = crate::procinfo::argv(pid.as_raw() as u32)?;
    let is_ours = argv.get(1).map(String::as_str) == Some("sandbox");
    (kill(pid, None).is_ok() && is_ours).then_some(pid)
}

fn signal_supervisor(config: &Config, sig: Signal) -> anyhow::Result<()> {
    let pid = running_supervisor(config).context("the supervisor isn't running")?;
    kill(pid, sig)?;
    Ok(())
}

/// `arugulad sandbox`, detached from this terminal (its own session), so
/// it outlives the shell that installed it.
fn start_supervisor(exe: &Path, config: &Config) -> anyhow::Result<()> {
    let log = fs::OpenOptions::new().create(true).append(true).open(config.dir.join("supervisor.log"))?;
    let mut c = Command::new(exe);
    c.arg("sandbox").stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork
    // and exec.
    unsafe {
        c.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(std::io::Error::from));
    }
    // Not waited for: it lives on after we exit, in its own session.
    drop(c.spawn().context("starting the supervisor")?);
    let deadline = Instant::now() + Duration::from_secs(5);
    while running_supervisor(config).is_none() {
        if Instant::now() > deadline {
            bail!("the supervisor didn't start (see {})", config.dir.join("supervisor.log").display());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// One program kept running: restarted when it exits, with a growing pause
/// if it keeps failing fast.
struct Worker {
    name: &'static str,
    stop: Arc<AtomicBool>,
    pid: Arc<AtomicI32>,
    thread: thread::JoinHandle<()>,
}

impl Worker {
    fn start(name: &'static str, program: PathBuf, args: Vec<String>, log: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let (s, p) = (stop.clone(), pid.clone());
        let thread = thread::spawn(move || {
            let mut pause = Duration::from_secs(1);
            while !s.load(Ordering::SeqCst) {
                let started = Instant::now();
                let child: std::io::Result<Child> = (|| {
                    let out = fs::OpenOptions::new().create(true).append(true).open(&log)?;
                    let mut c = Command::new(&program);
                    c.args(&args).stdin(Stdio::null()).stdout(out.try_clone()?).stderr(out);
                    // The supervisor's threads block the signals it waits
                    // for, and children inherit that: unblock them, or the
                    // daemon never hears SIGTERM. SAFETY: sigprocmask is
                    // async-signal-safe.
                    unsafe {
                        c.pre_exec(|| {
                            nix::sys::signal::SigSet::empty().thread_set_mask().map_err(std::io::Error::from)
                        });
                    }
                    c.spawn()
                })();
                match child {
                    Ok(mut c) => {
                        p.store(c.id() as i32, Ordering::SeqCst);
                        let status = c.wait();
                        p.store(0, Ordering::SeqCst);
                        eprintln!("{name} exited: {status:?}");
                    }
                    Err(e) => eprintln!("{name} didn't start: {e}"),
                }
                if started.elapsed() > Duration::from_secs(60) {
                    pause = Duration::from_secs(1);
                }
                let until = Instant::now() + pause;
                while Instant::now() < until && !s.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(100));
                }
                pause = (pause * 2).min(Duration::from_secs(30));
            }
        });
        Self { name, stop, pid, thread }
    }

    /// TERM, then KILL if it hasn't gone within `grace`.
    fn stop(self, grace: Duration) {
        self.stop.store(true, Ordering::SeqCst);
        let pid = self.pid.load(Ordering::SeqCst);
        if pid > 0 {
            let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
        }
        let deadline = Instant::now() + grace;
        while !self.thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
        let pid = self.pid.load(Ordering::SeqCst);
        if !self.thread.is_finished() && pid > 0 {
            eprintln!("{} didn't stop; killing it", self.name);
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        }
        let _ = self.thread.join();
    }
}

fn tailscaled_args(config: &Config) -> Vec<String> {
    let d = &config.dir;
    vec![
        "--tun=userspace-networking".into(),
        format!("--statedir={}", d.display()),
        format!("--state={}", d.join("tailscaled.state").display()),
        format!("--socket={}", config.socket().display()),
        // Any free port: another tailscaled may be on this machine.
        "--port=0".into(),
        format!("--outbound-http-proxy-listen={PROXY}"),
        format!("--socks5-server={PROXY}"),
    ]
}

fn wait_for_tailnet(config: &Config, within: Duration) {
    let Some(api) = LocalApi::find(Some(&config.socket())) else { return };
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
    rt.block_on(async {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if api.backend_state().await.is_ok_and(|s| s == "Running") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        eprintln!("tailscaled isn't running after {within:?}; starting the daemon anyway");
    });
}

/// `arugulad sandbox`: run tailscaled and the daemon from the saved
/// config until told to stop (SIGTERM, SIGINT); SIGHUP re-reads the config
/// and restarts the daemon.
pub fn supervise() -> anyhow::Result<()> {
    use nix::sys::signal::SigSet;
    let mut config = Config::load()?;
    if let Some(pid) = running_supervisor(&config) {
        bail!("already running (pid {pid})");
    }
    let mut signals = SigSet::empty();
    for s in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        signals.add(s);
    }
    // Blocked here (and in every thread started from now on) and waited for
    // below; children unblock them (`Worker`).
    signals.thread_block()?;
    fs::write(config.pidfile(), std::process::id().to_string())?;
    let exe = std::env::current_exe()?;
    let tailscaled = Worker::start(
        "tailscaled",
        config.tailscaled.clone(),
        tailscaled_args(&config),
        config.dir.join("tailscaled.log"),
    );
    let daemon_log = config.dir.join("arugulad.log");
    if config.daemon.is_some() {
        // The daemon learns its tailnet name as it starts: let tailscaled
        // come up first.
        wait_for_tailnet(&config, Duration::from_secs(60));
    }
    let mut daemon = config.daemon.clone().map(|args| Worker::start("arugulad", exe.clone(), args, daemon_log.clone()));
    eprintln!("supervising (pid {})", std::process::id());
    loop {
        match signals.wait()? {
            Signal::SIGHUP => {
                // A (re)install: new arguments, or a new binary.
                eprintln!("reloading: (re)starting the daemon");
                config = Config::load()?;
                if let Some(d) = daemon.take() {
                    d.stop(Duration::from_secs(15));
                }
                daemon =
                    config.daemon.clone().map(|args| Worker::start("arugulad", exe.clone(), args, daemon_log.clone()));
            }
            s => {
                eprintln!("{s}: stopping");
                break;
            }
        }
    }
    // The daemon first: it saves every pane before it goes.
    if let Some(d) = daemon {
        d.stop(Duration::from_secs(15));
    }
    tailscaled.stop(Duration::from_secs(10));
    let _ = fs::remove_file(config.pidfile());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_come_from_files_or_as_given() {
        let dir = std::env::temp_dir().join(format!("ilg-sandbox-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("key");
        fs::write(&f, "tskey-abc\n").unwrap();
        assert_eq!(read_secret(&format!("file:{}", f.display())).unwrap(), "tskey-abc");
        assert_eq!(read_secret("tskey-xyz").unwrap(), "tskey-xyz");
        // A key given as itself goes to a private file that is removed after.
        let k = KeyFile::new("tskey-xyz", &dir).unwrap();
        let mode = fs::metadata(&k.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let p = k.path.clone();
        drop(k);
        assert!(!p.exists());
        // The caller's own file is left alone.
        drop(KeyFile::new(&format!("file:{}", f.display()), &dir).unwrap());
        assert!(f.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_sandbox_installed_before_the_rename_keeps_its_places() {
        let d = std::env::temp_dir().join(format!("ilg-sandbox-names-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        // A new one: the new name. (The config and tailscaled's state dir
        // follow `arugula_proto::dirs`, tested there.)
        assert_eq!(tailscale_lib(&d), d.join(".local/lib/arugula/tailscale"));
        // One from before: the tailscale it downloaded, where it is.
        for old in ["illogical", "illogical-sandbox", ".local/lib/illogical/tailscale"] {
            fs::create_dir_all(d.join(old)).unwrap();
        }
        fs::write(d.join("illogical/sandbox.json"), "{}").unwrap();
        assert_eq!(tailscale_lib(&d), d.join(".local/lib/illogical/tailscale"));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn origins_are_what_browsers_send() {
        assert_eq!(origin("https://geek.example.ts.net/").unwrap(), "https://geek.example.ts.net");
        assert_eq!(origin("http://100.1.2.3:7690").unwrap(), "http://100.1.2.3:7690");
        assert!(origin("geek").is_err());
    }

    #[test]
    fn tailscaled_runs_in_userspace_with_its_own_socket() {
        let c = Config {
            tailscaled: "/x/tailscaled".into(),
            tailscale: "/x/tailscale".into(),
            dir: "/s".into(),
            daemon: None,
        };
        let a = tailscaled_args(&c);
        assert!(a.contains(&"--tun=userspace-networking".to_owned()));
        assert!(a.contains(&"--socket=/s/tailscaled.sock".to_owned()));
        assert!(a.iter().any(|x| x.starts_with("--outbound-http-proxy-listen=")));
    }
}
