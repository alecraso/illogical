//! The ssh transport (M51): `--ssh user@box`, and hosts saved with transport
//! `ssh`. The client runs the system `ssh`, so the user's `~/.ssh/config`,
//! agent and any password or 2FA prompt work as they always do, and nothing
//! but the destination is stored. One master connection per box
//! (ControlMaster) carries everything: each connection to the daemon is a
//! channel on it running `arugula bridge` on the box, which joins the
//! channel to the daemon's Unix socket (S28, `spikes/s28-ssh`).

#[cfg(unix)]
use std::os::unix::net::UnixStream;
/// Windows: `--ssh` from Windows isn't there yet (#284); a stream
/// type stands in until then.
#[cfg(not(unix))]
type UnixStream = std::net::TcpStream;
use std::{
    fs,
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The hosted control, where `join` goes by default.
pub const CONTROL: &str = "https://control.arugula.io";
const RELEASES: &str = "https://github.com/arugula-salad/illogical/releases/download";

/// `sh -c` that runs the CLI the install put on the box with `args`:
/// `arugula`, or on a box illogical was installed on, `illogical` (#505).
/// `ssh box cmd` doesn't have `~/.local/bin` on PATH (that's `~/.profile`,
/// read by login shells only), and a login shell could print into the
/// bridge's stream, so it's always named in full. `sh` so it reads the same
/// whatever the login shell is.
// The bridge over ssh is Unix-only (`channel`).
#[cfg_attr(not(unix), allow(dead_code))]
fn remote_cli(args: &str) -> String {
    format!(
        "sh -c {}",
        sh_quote(&format!("c=$HOME/.local/bin/arugula; [ -x $c ] || c=$HOME/.local/bin/illogical; exec $c {args}"))
    )
}

/// The same for the box's daemon: `arugulad`, or `illogicald` (#505).
fn remote_daemon(args: &str) -> String {
    format!(
        "sh -c {}",
        sh_quote(&format!("d=$HOME/.local/bin/arugulad; [ -x $d ] || d=$HOME/.local/bin/illogicald; exec $d {args}"))
    )
}

/// A box reached over ssh: what `ssh` is given as its destination
/// (`user@box`, `box` from `~/.ssh/config`, `ssh://user@box:2222`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub dest: String,
}

impl Remote {
    pub fn parse(dest: &str) -> anyhow::Result<Self> {
        let d = dest.trim();
        // It goes to ssh as the destination, so it must never read as an
        // option or carry anything a shell would act on.
        if d.is_empty()
            || d.starts_with('-')
            || !d.bytes().all(|b| b.is_ascii_alphanumeric() || b"@._-:[]%+/".contains(&b))
        {
            bail!("bad ssh destination {d:?}: want user@host, a Host from ~/.ssh/config, or ssh://user@host:port");
        }
        Ok(Self { dest: d.to_owned() })
    }

    /// `ssh` with the options every call shares, for this box's master.
    fn ssh(&self) -> Command {
        // ARUGULA_SSH replaces `ssh` (`ssh -F testnet/.state/ssh_config`).
        let mut words: Vec<String> = std::env::var("ARUGULA_SSH")
            .ok()
            .map(|s| s.split_whitespace().map(String::from).collect())
            .filter(|w: &Vec<String>| !w.is_empty())
            .unwrap_or_else(|| vec!["ssh".into()]);
        let mut c = Command::new(words.remove(0));
        c.args(words);
        let path = control_dir().join("%C");
        for o in [
            format!("ControlPath=\"{}\"", path.display()),
            // A dead link is noticed in about 30s instead of never.
            "ServerAliveInterval=10".into(),
            "ServerAliveCountMax=3".into(),
            // The user's own forwards and commands are for their sessions,
            // not for each of these channels.
            "ClearAllForwardings=yes".into(),
            "PermitLocalCommand=no".into(),
            "RemoteCommand=none".into(),
            "RequestTTY=no".into(),
        ] {
            c.arg("-o").arg(o);
        }
        c
    }

    /// A channel's `ssh`: through the master, never prompting.
    fn channel_cmd(&self, extra: &[&str], command: &str) -> Command {
        let mut c = self.ssh();
        c.args(["-o", "ControlMaster=no", "-o", "BatchMode=yes", "-T"]).args(extra).args(["--", &self.dest, command]);
        c
    }

    /// Start the master connection if there isn't one. This is the one ssh
    /// that may ask for a password, a 2FA code or a new host key, in this
    /// terminal; everything after it goes through it without asking.
    fn master(&self, interactive: bool) -> anyhow::Result<()> {
        let alive = self
            .ssh()
            .args(["-O", "check", "--", &self.dest])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if alive {
            return Ok(());
        }
        let mut c = self.ssh();
        c.args(["-o", "ControlMaster=yes", "-o", "ControlPersist=10m", "-f", "-N"]);
        // `git push` from a pane on the box uses this client's agent (the
        // bridge links it for the box's panes). Channels through a master
        // get the master's agent, and only if the master forwards it, so
        // this is where it's decided. ARUGULA_SSH_AGENT=no keeps it here.
        if forward_agent() {
            c.args(["-o", "ForwardAgent=yes"]);
        }
        if !interactive {
            c.args(["-o", "BatchMode=yes"]);
        }
        let status = c.args(["--", &self.dest]).stdout(Stdio::null()).status().context("running ssh")?;
        if !status.success() {
            bail!("ssh {} failed{}", self.dest, if interactive { "" } else { " (not asking: no terminal)" });
        }
        Ok(())
    }

    /// Run a short command on the box and return its stdout.
    fn run(&self, command: &str, input: Option<&Path>) -> anyhow::Result<String> {
        let mut c = self.channel_cmd(&[], command);
        c.stdin(match input {
            Some(p) => Stdio::from(fs::File::open(p).with_context(|| format!("reading {}", p.display()))?),
            None => Stdio::null(),
        });
        let out = c.output().context("running ssh")?;
        if !out.status.success() {
            bail!(
                "on {}: `{command}` failed: {}",
                self.dest,
                String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("no output")
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// A new connection to the box's daemon: one end of a socket pair,
    /// with `ssh … arugula bridge` on the other (its stdin and stdout), so
    /// it has a real fd to poll like any other connection.
    #[cfg(not(unix))]
    pub fn channel(&self) -> anyhow::Result<UnixStream> {
        bail!("--ssh from Windows isn't supported yet (#284): reach {} with --host or through control", self.dest)
    }

    #[cfg(unix)]
    pub fn channel(&self) -> anyhow::Result<UnixStream> {
        let (ours, theirs) = UnixStream::pair()?;
        // The master decides (see `master`); this matters only when the
        // master has gone and ssh connects on its own.
        let agent: &[&str] = if forward_agent() { &["-o", "ForwardAgent=yes"] } else { &[] };
        let child = self
            .channel_cmd(agent, &remote_cli("bridge"))
            .stdin(Stdio::from(std::os::fd::OwnedFd::from(theirs.try_clone()?)))
            .stdout(Stdio::from(std::os::fd::OwnedFd::from(theirs)))
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("running ssh to {}", self.dest))?;
        reap(child);
        Ok(ours)
    }

    /// Before the first connection: the master, Arugula on the box (offered
    /// once if it's missing, or if it's a different major version), and its
    /// daemon running.
    pub fn prepare(&self) -> anyhow::Result<()> {
        let interactive = io::stdin().is_terminal() && io::stderr().is_terminal();
        self.master(interactive)?;
        let mut p = self.probe()?;
        if !p.installed {
            let q = format!(
                "Arugula isn't installed on {}. Install {VERSION} there (~/.local/bin, and its daemon)?",
                self.dest
            );
            self.offer(&q, interactive)?;
            self.install(&p)?;
            p = self.probe()?;
        } else if let Some(v) = p.daemon_version.clone().or(p.version.clone())
            && !compatible(&v, VERSION)
        {
            let q =
                format!("{} runs Arugula {v}; this is {VERSION}, a different major version. Upgrade it?", self.dest);
            self.offer(&q, interactive).with_context(|| format!("{} runs Arugula {v}, not {VERSION}", self.dest))?;
            self.install(&p)?;
            p = self.probe()?;
        }
        if !p.daemon {
            eprintln!("arugula: starting arugulad on {}", self.dest);
            self.start()?;
            let until = Instant::now() + Duration::from_secs(15);
            while !self.probe()?.daemon {
                if Instant::now() > until {
                    bail!("arugulad on {} didn't start: see ~/.local/state/arugulad.log there", self.dest);
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        Ok(())
    }

    /// M52: set the box up (`prepare`), then run its `arugulad join` here,
    /// in this terminal: its code and the account's fingerprint show here
    /// and its question is answered here, while you approve it from a
    /// signed-in device. Afterwards control reaches it, and ssh isn't needed.
    ///
    /// A box that can't reach `control` (no outbound connection) is told
    /// so, with the way that still works: a terminal over `--ssh`.
    pub fn join(&self, args: &[String], control: &str) -> anyhow::Result<i32> {
        self.prepare()?;
        let quoted: Vec<String> = args.iter().map(|a| sh_quote(a)).collect();
        let mut child = self
            .channel_cmd(&[], &remote_daemon(&quoted.join(" ")))
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("running ssh to {}", self.dest))?;
        // Its errors pass through as they come, and are kept to tell why
        // it failed.
        let mut said = Vec::new();
        if let Some(mut err) = child.stderr.take() {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let _ = io::stderr().write_all(&buf[..n]);
                said.extend_from_slice(&buf[..n]);
            }
        }
        let status = child.wait()?;
        if !status.success() && unreachable(&String::from_utf8_lossy(&said)) {
            eprintln!(
                "arugula: {dest} can't reach control at {control}. Joining needs {dest} to connect out to \
                 control (HTTPS, or http on a private network), and it couldn't. {dest} is still reachable from a \
                 terminal over ssh: `arugula --ssh {dest} tui`.",
                dest = self.dest
            );
        }
        Ok(status.code().unwrap_or(1))
    }

    fn probe(&self) -> anyhow::Result<Probe> {
        // `sh -c` so it reads the same whatever the login shell is.
        // `arugula`, or `illogical` on a box illogical was installed on (#505).
        let out = self.run(
            "sh -c 'c=$HOME/.local/bin/arugula; [ -x $c ] || c=$HOME/.local/bin/illogical; if [ -x $c ]; then $c bridge --probe; else echo \"{}\"; fi; uname -sm'",
            None,
        )?;
        Probe::parse(&out)
    }

    /// Ask once: yes, unless declined before (remembered, so it isn't asked
    /// again) or there's no terminal to ask in. ARUGULA_SSH_INSTALL=yes
    /// answers yes without asking.
    fn offer(&self, question: &str, interactive: bool) -> anyhow::Result<()> {
        if std::env::var("ARUGULA_SSH_INSTALL").is_ok_and(|v| v == "yes") {
            return Ok(());
        }
        let declined = config_dir().join("ssh-declined");
        let known = fs::read_to_string(&declined).unwrap_or_default();
        let how = "ARUGULA_SSH_INSTALL=yes installs it without asking";
        if known.lines().any(|l| l == self.dest) {
            bail!("{question} (declined before; {how})");
        }
        if !interactive {
            bail!("{question} (no terminal to ask in; {how})");
        }
        eprint!("{question} [Y/n] ");
        io::stderr().flush()?;
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        if matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes") {
            return Ok(());
        }
        fs::create_dir_all(declined.parent().unwrap_or(Path::new(".")))?;
        fs::write(&declined, format!("{known}{}\n", self.dest))?;
        bail!("not installing on {} (not asked again; {how})", self.dest);
    }

    /// Put this version's binaries for the box's platform in its
    /// `~/.local/bin`, over the master: the box needs nothing but sshd and
    /// `sh`, not even a network.
    fn install(&self, p: &Probe) -> anyhow::Result<()> {
        let triple = p.triple().with_context(|| format!("no Arugula build for {} ({})", self.dest, p.uname))?;
        let dir = binaries(triple)?;
        eprintln!("arugula: installing {VERSION} ({triple}) on {}", self.dest);
        for name in ["arugulad", "arugula"] {
            let file = dir.join(name);
            self.run(
                &format!(
                    "sh -c 'mkdir -p ~/.local/bin && cat > ~/.local/bin/.{name}.new && chmod 755 ~/.local/bin/.{name}.new && mv -f ~/.local/bin/.{name}.new ~/.local/bin/{name}'"
                ),
                Some(&file),
            )?;
        }
        // The old names, as links to the new (#505): older clients reach the
        // box with `~/.local/bin/illogical bridge`, and hooks call it.
        self.run(
            "sh -c 'cd ~/.local/bin && for p in illogicald:arugulad illogical:arugula; do ln -sf ${p#*:} .${p%:*}.link && mv -f .${p%:*}.link ${p%:*}; done'",
            None,
        )?;
        // A daemon already running there keeps the old binary until it
        // restarts: a systemd service or a launchd agent restarts now (its
        // panes are adopted). A --system LaunchDaemon needs sudo, so not.
        if p.daemon {
            let out = self.run(
                // illogical's service too: the install puts arugulad's in
                // its place (#505).
                "sh -c 'if systemctl --user is-active --quiet arugulad 2>/dev/null || systemctl --user is-active --quiet illogicald 2>/dev/null || [ -f ~/Library/LaunchAgents/arugulad.plist ] || [ -f ~/Library/LaunchAgents/illogicald.plist ]; then ~/.local/bin/arugulad install >/dev/null && echo restarted; fi'",
                None,
            );
            if !out.is_ok_and(|o| o.contains("restarted")) {
                eprintln!(
                    "arugula: the daemon running on {} is still the old one until it restarts (its panes are kept)",
                    self.dest
                );
            }
        }
        Ok(())
    }

    /// Start the daemon so it outlives this login: a launchd agent on a Mac
    /// (in the background session when there's no GUI login: it outlives
    /// the login, not a reboot, and says so); a systemd user service
    /// with lingering where there's systemd (lingering is asked for without
    /// sudo, which polkit usually allows), detached otherwise (it survives
    /// logging out, not a reboot).
    fn start(&self) -> anyhow::Result<()> {
        let out = self.run(START, None)?;
        let how = out.lines().last().unwrap_or_default().trim();
        match how {
            "launchd" => {
                // The install's notes (a Mac with no GUI login: it won't come
                // back after a reboot by itself) pass through.
                for note in out.lines().filter_map(|l| l.strip_prefix("note: ")) {
                    eprintln!("arugula: {}: {note}", self.dest);
                }
            }
            "linger" => {}
            "nolinger" => eprintln!(
                "arugula: arugulad on {} stops when you log out: lingering couldn't be turned on without sudo \
                 (`sudo loginctl enable-linger $USER` there keeps it)",
                self.dest
            ),
            _ => eprintln!(
                "arugula: no systemd user session on {}: arugulad runs detached, so it survives logging out but not a reboot",
                self.dest
            ),
        }
        Ok(())
    }
}

/// On the box: on a Mac, `arugulad install` (a LaunchAgent, or with no
/// GUI login a background agent, whose `note:` line is printed first); a
/// systemd user service if the user has a manager, with lingering; else
/// detached, without this login's agent (the bridge links the current one).
/// Prints how: `launchd`, `linger`, `nolinger` or `detached`.
const START: &str = r#"sh -c '
d=$HOME/.local/bin/arugulad
[ -x "$d" ] || d=$HOME/.local/bin/illogicald
if [ "$(uname -s)" = Darwin ] && out=$("$d" install 2>&1); then
  printf "%s\n" "$out" | grep "^note:"
  echo launchd
elif command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1 && "$d" install >/dev/null 2>&1; then
  loginctl enable-linger >/dev/null 2>&1
  if [ "$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null)" = yes ]; then echo linger; else echo nolinger; fi
else
  mkdir -p "$HOME/.local/state"
  unset SSH_AUTH_SOCK
  if command -v setsid >/dev/null 2>&1; then s=setsid; else s=; fi
  nohup $s "$d" --keep-panes </dev/null >"$HOME/.local/state/arugulad.log" 2>&1 &
  echo detached
fi'"#;

/// Whether `arugulad join`'s errors say it couldn't connect to control
/// at all (as opposed to control refusing, or nobody approving).
fn unreachable(said: &str) -> bool {
    said.contains("can't reach control at")
}

/// One word for the box's shell, whatever it holds.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn forward_agent() -> bool {
    !std::env::var("ARUGULA_SSH_AGENT").is_ok_and(|v| v == "no")
}

#[cfg(unix)]
/// Wait for a finished ssh in the background, so it doesn't linger as a
/// zombie.
fn reap(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// What `bridge --probe` and `uname -sm` said.
#[derive(Debug, Default, PartialEq)]
struct Probe {
    installed: bool,
    version: Option<String>,
    daemon: bool,
    daemon_version: Option<String>,
    uname: String,
}

impl Probe {
    fn parse(out: &str) -> anyhow::Result<Self> {
        let mut lines = out.lines().filter(|l| !l.trim().is_empty());
        let v: Value = serde_json::from_str(lines.next().unwrap_or("{}")).context("reading the box's probe")?;
        Ok(Self {
            installed: v["installed"].as_bool().unwrap_or(false),
            version: v["version"].as_str().map(String::from),
            daemon: v["daemon"].as_bool().unwrap_or(false),
            daemon_version: v["daemon_version"].as_str().map(String::from),
            uname: lines.next().unwrap_or_default().trim().to_owned(),
        })
    }

    /// The release build for the box's `uname -sm`.
    fn triple(&self) -> Option<&'static str> {
        let (os, arch) = self.uname.split_once(' ')?;
        Some(match (os, arch) {
            ("Linux", "x86_64" | "amd64") => "x86_64-unknown-linux-musl",
            ("Linux", "aarch64" | "arm64") => "aarch64-unknown-linux-musl",
            ("Darwin", "arm64") => "aarch64-apple-darwin",
            _ => return None,
        })
    }
}

/// Whether a box's version and ours can work together: the same major
/// version (ruling 7 on #157). Every 0.x so far talks to every other
/// (S28 tried 0.12 to 0.16 both ways), so 0.x counts as one major.
fn compatible(theirs: &str, ours: &str) -> bool {
    let major = |v: &str| v.trim_start_matches('v').split('.').next().and_then(|m| m.parse::<u64>().ok());
    major(theirs).is_some() && major(theirs) == major(ours)
}

/// A directory with `arugula` and `arugulad` for `triple`:
/// ARUGULA_SSH_BINARIES (`DIR/TRIPLE/` or `DIR/`), else this build's own
/// when it's a static build for the same platform, else the release
/// tarball, downloaded once and checked against the release's SHA256SUMS.
fn binaries(triple: &str) -> anyhow::Result<PathBuf> {
    let has = |d: &Path| d.join("arugula").is_file() && d.join("arugulad").is_file();
    if let Some(dir) = std::env::var_os("ARUGULA_SSH_BINARIES").map(PathBuf::from) {
        for d in [dir.join(triple), dir.clone()] {
            if has(&d) {
                return Ok(d);
            }
        }
        bail!("no arugula and arugulad for {triple} in {}", dir.display());
    }
    if own_triple() == Some(triple)
        && let Some(d) = std::env::current_exe()?.parent().map(Path::to_path_buf)
        && has(&d)
    {
        return Ok(d);
    }
    download(triple)
}

/// This build's release triple, if it's one a box can run as is (static
/// musl on Linux; any macOS build).
fn own_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH, cfg!(target_env = "musl")) {
        ("linux", "x86_64", true) => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64", true) => Some("aarch64-unknown-linux-musl"),
        ("macos", "aarch64", _) => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

fn download(triple: &str) -> anyhow::Result<PathBuf> {
    let name = format!("arugula-{VERSION}-{triple}");
    let cache = cache_dir().join("releases").join(VERSION);
    let dir = cache.join(&name);
    if dir.join("arugula").is_file() && dir.join("arugulad").is_file() {
        return Ok(dir);
    }
    fs::create_dir_all(&cache)?;
    let base = format!("{RELEASES}/v{VERSION}");
    let tarball = cache.join(format!("{name}.tar.gz"));
    let sums = cache.join("SHA256SUMS");
    eprintln!("arugula: downloading {name}");
    for (url, to) in [(format!("{base}/{name}.tar.gz"), &tarball), (format!("{base}/SHA256SUMS"), &sums)] {
        let ok = Command::new("curl").args(["-fsSL", "-o"]).arg(to).arg(&url).status().is_ok_and(|s| s.success());
        if !ok {
            bail!("couldn't download {url} (needs curl and a release for {VERSION})");
        }
    }
    // `HASH  FILE` (or `HASH *FILE`, binary mode) per line.
    let want = fs::read_to_string(&sums)?
        .lines()
        .find_map(|l| {
            let (hash, file) = l.split_once(char::is_whitespace)?;
            (file.trim().trim_start_matches('*') == format!("{name}.tar.gz")).then(|| hash.to_owned())
        })
        .with_context(|| format!("{name}.tar.gz isn't in the release's SHA256SUMS"))?;
    let got = sha256(&tarball)?;
    if got != want {
        let _ = fs::remove_file(&tarball);
        bail!("{name}.tar.gz doesn't match the release's SHA256SUMS");
    }
    let ok = Command::new("tar").arg("-xzf").arg(&tarball).arg("-C").arg(&cache).status().is_ok_and(|s| s.success());
    if !ok || !dir.join("arugula").is_file() {
        bail!("couldn't unpack {}", tarball.display());
    }
    Ok(dir)
}

fn sha256(file: &Path) -> anyhow::Result<String> {
    for cmd in [&["sha256sum"][..], &["shasum", "-a", "256"]] {
        if let Ok(out) = Command::new(cmd[0]).args(&cmd[1..]).arg(file).output()
            && out.status.success()
        {
            return Ok(String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap_or_default().to_owned());
        }
    }
    bail!("no sha256sum or shasum to check the download with")
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn config_dir() -> PathBuf {
    arugula_proto::dirs::config_dir().unwrap_or_else(|| home().join(".config/arugula"))
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".cache")).join("arugula")
}

/// Where the masters' sockets go. A Unix socket path is at most 104 bytes on
/// macOS, and ssh adds `/%C` (40) and a temporary suffix (17) to this, so
/// it has to be short: the runtime dir, or the cache, or /tmp.
fn control_dir() -> PathBuf {
    // Ours: owned by whoever owns $HOME.
    #[cfg(unix)]
    let uid = fs::metadata(home()).map(|m| std::os::unix::fs::MetadataExt::uid(&m)).unwrap_or(u32::MAX);
    #[cfg(not(unix))]
    let uid = 0;
    let candidates = [
        std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("arugula-ssh")),
        Some(cache_dir().join("ssh")),
        Some(PathBuf::from(format!("/tmp/arugula-ssh-{uid}"))),
    ];
    for dir in candidates.into_iter().flatten() {
        if dir.as_os_str().len() > 45 || fs::create_dir_all(&dir).is_err() {
            continue;
        }
        // Someone else's directory in /tmp isn't ours to put sockets in.
        #[cfg(unix)]
        if fs::metadata(&dir).is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == uid) {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
            return dir;
        }
        #[cfg(not(unix))]
        return dir;
    }
    cache_dir().join("ssh")
}

/// `arugula bridge` (on the box, run by a client over ssh): join stdin and
/// stdout to the daemon's socket until either side is done. With `probe`:
/// say what's installed and whether the daemon answers, as one JSON line.
#[cfg(unix)]
fn connect_daemon(sock: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(sock)
}

/// Windows: the daemon's named pipe comes in M56 (#219).
#[cfg(not(unix))]
fn connect_daemon(_sock: &Path) -> io::Result<UnixStream> {
    Err(io::Error::other("not on Windows yet (M56, #219)"))
}

/// Something answers at this socket path.
fn listening(p: &Path) -> bool {
    connect_daemon(p).is_ok()
}

pub fn bridge(sock: &Path, probe: bool) -> anyhow::Result<i32> {
    if probe {
        let daemon_version =
            crate::http::request(&crate::http::Target::Socket(sock.to_owned()), "GET", "/api/host", None)
                .and_then(|r| r.json())
                .ok()
                .and_then(|v| v["version"].as_str().map(String::from));
        let v = json!({
            "installed": true,
            "version": VERSION,
            "daemon": daemon_version.is_some(),
            "daemon_version": daemon_version,
        });
        println!("{v}");
        return Ok(0);
    }
    let conn = match connect_daemon(sock) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("arugula bridge: can't reach arugulad at {}: {e}", sock.display());
            return Ok(1);
        }
    };
    let linked = link_agent(sock);
    let mut up = conn.try_clone()?;
    let closer = conn.try_clone()?;
    std::thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock(), &mut up);
        // The client is done. Not a half-close straight away: the daemon
        // (hyper) drops a request it hasn't answered when its client
        // half-closes. Give it a moment to finish, then end.
        std::thread::sleep(Duration::from_secs(5));
        let _ = closer.shutdown(std::net::Shutdown::Both);
    });
    let mut down = conn;
    let mut out = io::stdout().lock();
    let mut buf = [0u8; 16384];
    loop {
        match down.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.write_all(&buf[..n]).and_then(|_| out.flush()).is_err() {
                    break;
                }
            }
        }
    }
    if let Some(link) = linked {
        unlink_agent(&link);
    }
    Ok(0)
}

/// The box's panes use the agent at a fixed path beside the daemon's socket
/// (`agent.sock`; the daemon sets SSH_AUTH_SOCK to it when it has no agent
/// of its own). Point it at the agent this ssh login forwarded, unless it
/// already points at one that answers. Only the box's owner logs in over
/// ssh as this user, so only the owner's agent is ever linked (ruling 2 on
/// #157); guests come through control and never run this.
fn link_agent(sock: &Path) -> Option<(PathBuf, PathBuf)> {
    let agent = PathBuf::from(std::env::var_os("SSH_AUTH_SOCK")?);
    if !listening(&agent) {
        return None;
    }
    let link = agent_link(sock);
    if fs::read_link(&link).is_ok_and(|t| listening(&t)) {
        return None;
    }
    let tmp = link.with_extension(format!("{}.tmp", std::process::id()));
    let _ = fs::remove_file(&tmp);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&agent, &tmp).ok()?;
    #[cfg(not(unix))]
    std::os::windows::fs::symlink_file(&agent, &tmp).ok()?;
    fs::rename(&tmp, &link).ok()?;
    Some((link, agent))
}

/// When the login whose agent is linked ends, so does that agent.
fn unlink_agent((link, agent): &(PathBuf, PathBuf)) {
    if fs::read_link(link).is_ok_and(|t| &t == agent) {
        let _ = fs::remove_file(link);
    }
}

/// The fixed agent path panes get: beside the daemon's socket.
pub fn agent_link(sock: &Path) -> PathBuf {
    sock.with_file_name("agent.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        for ok in ["box", "illo@box-bare", "me@10.0.0.2", "ssh://me@box:2222", "me@[fe80::1%eth0]"] {
            assert!(Remote::parse(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-oProxyCommand=x", "box; rm -rf ~", "a b", "$(id)", "box`id`"] {
            assert!(Remote::parse(bad).is_err(), "{bad}");
        }
    }

    /// #505: a box illogical was installed on has only the old names.
    #[cfg(unix)]
    #[test]
    fn remote_commands_find_either_name() {
        let home = std::env::temp_dir().join(format!("arugula-ssh-names-{}", std::process::id()));
        let bin = home.join(".local/bin");
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&bin).unwrap();
        let run = |cmd: &str| {
            let out = Command::new("sh").arg("-c").arg(cmd).env("HOME", &home).output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let fake = |name: &str| {
            let f = bin.join(name);
            fs::write(&f, format!("#!/bin/sh\necho {name} \"$@\"\n")).unwrap();
            fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        };
        fake("illogical");
        fake("illogicald");
        assert_eq!(run(&remote_cli("bridge")), "illogical bridge");
        assert_eq!(run(&remote_daemon(&[sh_quote("join"), sh_quote("it's")].join(" "))), "illogicald join it's");
        fake("arugula");
        fake("arugulad");
        assert_eq!(run(&remote_cli("bridge --probe")), "arugula bridge --probe");
        assert_eq!(run(&remote_daemon("join")), "arugulad join");
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn probes() {
        let p = Probe::parse("{\"installed\":true,\"version\":\"0.16.0\",\"daemon\":true,\"daemon_version\":\"0.15.0\"}\nLinux aarch64\n").unwrap();
        assert!(p.installed && p.daemon);
        assert_eq!(p.daemon_version.as_deref(), Some("0.15.0"));
        assert_eq!(p.triple(), Some("aarch64-unknown-linux-musl"));
        let none = Probe::parse("{}\nLinux x86_64\n").unwrap();
        assert!(!none.installed && !none.daemon);
        assert_eq!(none.triple(), Some("x86_64-unknown-linux-musl"));
        assert_eq!(Probe::parse("{}\nFreeBSD amd64").unwrap().triple(), None);
    }

    #[test]
    fn versions() {
        assert!(compatible("0.12.0", "0.16.0"));
        assert!(compatible("1.2.0", "1.9.3"));
        assert!(!compatible("1.0.0", "0.16.0"));
        assert!(!compatible("2.0.0", "1.4.0"));
        assert!(!compatible("garbage", "0.16.0"));
    }

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("https://c.example"), "'https://c.example'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn agent_beside_socket() {
        assert_eq!(agent_link(Path::new("/run/user/1/arugula/sock")), Path::new("/run/user/1/arugula/agent.sock"));
    }
}
