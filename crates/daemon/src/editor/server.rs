//! code-server for editor blocks (M27, S17): one per machine, shared by
//! every editor block on it. It starts when a block needs it, and stops by
//! itself when nothing has used it for a while (`--idle-timeout-seconds`).
//!
//! **On this host** it listens on a 0600 Unix socket, never a TCP port that
//! any local process could reach, and has no auth of its own: the only way
//! in from a browser is a block's site (`sites.rs`), so the daemon's access
//! checks are its auth. It runs in a scope of its own (or its own process
//! group), so a daemon restart leaves it running and blocks reconnect.
//!
//! **On a VM** (a sprite) it runs as a sprite service on a loopback port in
//! the VM, reached through the provider's proxy like any port there.
//!
//! **Where it comes from:** a pinned release from GitHub, checked against
//! its SHA-256, unpacked once into the cache (`~/.cache/illogical/
//! code-server/`), or `--code-server` for one already installed.
//!
//! **What it writes:** its settings, extensions and state under
//! `<state>/editor/`, passed explicitly (S17: without them it writes
//! `~/.config/code-server` even for `--help`). code-server still keeps its
//! own logs in `~/.local/share/code-server`.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

use futures_util::future::BoxFuture;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, sync::watch};
use tracing::{info, warn};

use crate::{
    pane::Launcher,
    ports::{Conn, Service},
    provider::{Provider, ServiceDef},
    store,
};

/// The code-server release editor blocks run.
pub const VERSION: &str = "4.140.0";

/// Its release tarballs' SHA-256 (GitHub's asset digests), by platform.
const SHA256: &[(&str, &str)] = &[
    ("linux-amd64", "864c5d01c808ade57e4d12c708717be7a187219fded60428f263b9e2da9f6b48"),
    ("linux-arm64", "ae4b07153f2037b06d24749bc8004221fcbf3ffe317038401be0452f541bf200"),
    ("macos-amd64", "a5393b6eed4aa68b084e724c3c565f805abd996c609356043119f0323e40cf52"),
    ("macos-arm64", "82c7144406ac31c373acfa786b6705c7c5463d895f728fde2cb94402b945b301"),
];

/// Where releases are downloaded from.
pub const RELEASES: &str = "https://github.com/coder/code-server/releases/download";

/// The port code-server listens on inside a VM (loopback there).
pub const VM_PORT: u16 = 13340;

/// How long a start may take once the binary is here.
const START: Duration = Duration::from_secs(60);

/// The extension every editor block has (`ext/`): it reports the active
/// file and carries the theme.
pub const EXT_ID: &str = "illogical.illogical-editor";
pub const EXT_VERSION: &str = "0.2.0";
/// What it was called before (M27): taken out where it's found.
const OLD_IDS: &[&str] = &["illogical.illogical"];
pub const EXT_FILES: &[(&str, &str)] = &[
    ("package.json", include_str!("ext/package.json")),
    ("extension.js", include_str!("ext/extension.js")),
    ("theme.json", include_str!("ext/theme.json")),
];

/// What a new settings folder starts with. Later changes are the user's.
const SETTINGS: &str = r#"{
  "workbench.colorTheme": "illogical",
  "chat.disableAIFeatures": true,
  "workbench.startupEditor": "none",
  "workbench.secondarySideBar.defaultVisibility": "hidden",
  "security.workspace.trust.enabled": false,
  "telemetry.telemetryLevel": "off"
}
"#;

/// How editor blocks get their servers (`--code-server`, `--editor-idle`).
pub struct Settings {
    /// `<state>/editor`: settings, extensions, workspaces, logs.
    pub dir: PathBuf,
    /// This host's server's socket (kept short: Unix socket paths are).
    pub socket: PathBuf,
    /// A code-server to run instead of the pinned release.
    pub binary: Option<PathBuf>,
    /// Where releases are unpacked.
    pub cache: PathBuf,
    /// Where releases come from (tests point it elsewhere).
    pub releases: String,
    /// Seconds without a client before a server stops (at least 60).
    pub idle: u64,
    /// Seconds a window may be away and still reconnect to its session.
    pub grace: u64,
    pub launch: Launcher,
}

static SETTINGS_: OnceLock<Arc<Settings>> = OnceLock::new();

/// Turn editor blocks on (once per process).
pub fn install(s: Settings) {
    let _ = SETTINGS_.set(Arc::new(s));
}

pub fn settings() -> Option<Arc<Settings>> {
    SETTINGS_.get().cloned()
}

/// What a server is doing, as blocks show it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "is")]
pub enum Status {
    /// Not running (yet, or it stopped when idle); the next use starts it.
    Stopped,
    /// Downloading the release: bytes so far, of how many.
    Fetching {
        got: u64,
        of: u64,
    },
    Starting,
    Running,
    Failed {
        error: String,
    },
}

/// Which machine a server is on.
#[derive(Clone)]
pub enum On {
    Here,
    Vm { provider: Arc<dyn Provider>, sprite: String },
}

impl On {
    fn key(&self) -> String {
        match self {
            Self::Here => String::new(),
            Self::Vm { sprite, .. } => sprite.clone(),
        }
    }
}

pub struct Server {
    me: Weak<Server>,
    on: On,
    settings: Arc<Settings>,
    status: watch::Sender<Status>,
    /// One start at a time.
    starting: tokio::sync::Mutex<()>,
    /// The environment it starts with (a pane's, as the first block had it).
    env: Mutex<Vec<(String, String)>>,
}

static SERVERS: OnceLock<Mutex<HashMap<String, Weak<Server>>>> = OnceLock::new();

/// The server for a machine, shared by its blocks.
pub fn get(settings: &Arc<Settings>, on: On, env: Vec<(String, String)>) -> Arc<Server> {
    let mut all = SERVERS.get_or_init(Mutex::default).lock().unwrap();
    let key = on.key();
    if let Some(s) = all.get(&key).and_then(Weak::upgrade) {
        return s;
    }
    let s = Arc::new_cyclic(|me| Server {
        me: me.clone(),
        on,
        settings: settings.clone(),
        status: watch::channel(Status::Stopped).0,
        starting: tokio::sync::Mutex::new(()),
        env: Mutex::new(env),
    });
    all.retain(|_, w| w.strong_count() > 0);
    all.insert(key, Arc::downgrade(&s));
    s
}

impl Server {
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }

    fn set(&self, s: Status) {
        self.status.send_if_modified(|now| {
            let changed = *now != s;
            *now = s;
            changed
        });
    }

    /// Whether it's already up (one left by the last daemon counts).
    pub async fn check(&self) {
        if self.connect().await.is_ok() {
            self.set(Status::Running);
        }
    }

    /// Start it if it isn't running.
    pub async fn ensure(&self) -> Result<(), String> {
        let _one = self.starting.lock().await;
        if self.connect().await.is_ok() {
            self.set(Status::Running);
            return Ok(());
        }
        let started = match &self.on {
            On::Here => self.start_here().await,
            On::Vm { provider, sprite } => self.start_vm(provider.as_ref(), sprite).await,
        };
        match started {
            Ok(()) => {
                self.set(Status::Running);
                Ok(())
            }
            Err(e) => {
                warn!(error = %e, "code-server didn't start");
                self.set(Status::Failed { error: e.clone() });
                Err(e)
            }
        }
    }

    async fn connect(&self) -> io::Result<Conn> {
        match &self.on {
            #[cfg(unix)]
            On::Here => Ok(Box::new(tokio::net::UnixStream::connect(&self.settings.socket).await?)),
            // code-server listens on a Unix socket; Windows has none for it.
            #[cfg(not(unix))]
            On::Here => Err(io::Error::other("code-server here needs a Unix socket")),
            On::Vm { provider, sprite } => provider.dial(sprite, VM_PORT).await,
        }
    }

    async fn start_here(&self) -> Result<(), String> {
        let s = &self.settings;
        let bin = match &s.binary {
            Some(b) => b.clone(),
            None => {
                let platform = platform().ok_or("there's no code-server release for this platform")?;
                let me = self.me.clone();
                fetch(&s.releases, &s.cache, platform, move |got, of| {
                    if let Some(me) = me.upgrade() {
                        me.set(Status::Fetching { got, of });
                    }
                })
                .await?
                .join("bin/code-server")
            }
        };
        self.set(Status::Starting);
        prepare(&s.dir).map_err(|e| format!("can't set up {}: {e}", s.dir.display()))?;
        let _ = std::fs::remove_file(&s.socket);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(s.dir.join("code-server.log"))
            .map_err(|e| e.to_string())?;
        let mut cmd = self.command(&bin);
        cmd.args(args(&s.dir, s.idle, s.grace))
            .arg("--socket")
            .arg(&s.socket)
            .args(["--socket-mode", "600"])
            .current_dir(&s.dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .kill_on_drop(false);
        let mut child = cmd.spawn().map_err(|e| format!("can't run {}: {e}", bin.display()))?;
        let pid = child.id().unwrap_or(0);
        let _ = std::fs::write(s.dir.join("code-server.pid"), format!("{pid}\n"));
        info!(pid, socket = %s.socket.display(), "code-server starting");
        let t0 = Instant::now();
        loop {
            if self.connect().await.is_ok() {
                info!(pid, ms = t0.elapsed().as_millis() as u64, "code-server is up");
                break;
            }
            if let Ok(Some(st)) = child.try_wait() {
                return Err(format!("code-server exited ({st}): {}", tail(&s.dir.join("code-server.log"))));
            }
            if t0.elapsed() > START {
                let _ = child.start_kill();
                return Err("code-server didn't answer in time".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // It stops itself when idle: then the next use starts it again.
        let me = self.me.clone();
        tokio::spawn(async move {
            let st = child.wait().await;
            info!(pid, status = ?st, "code-server stopped");
            if let Some(me) = me.upgrade()
                && me.status() == Status::Running
            {
                me.set(Status::Stopped);
            }
        });
        Ok(())
    }

    /// code-server in its own scope (systemd) or process group, so it
    /// outlives a daemon restart; with a pane's environment.
    fn command(&self, bin: &Path) -> tokio::process::Command {
        let launch = &self.settings.launch;
        let mut c = if launch.scopes {
            let mut c = tokio::process::Command::new("systemd-run");
            c.args(["--user", "--scope", "--quiet", "--collect"]);
            if launch.no_expand {
                c.arg("--expand-environment=no");
            }
            c.arg(format!("--unit=illogical-code-server-{}", unique())).arg("--").arg(bin);
            c
        } else {
            let c = tokio::process::Command::new(bin);
            #[cfg(unix)]
            let c = {
                let mut c = c;
                c.process_group(0);
                c
            };
            c
        };
        for k in crate::sys::SERVICE_ENV {
            c.env_remove(k);
        }
        // Not a VS Code terminal's, if the daemon was started in one.
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("VSCODE_") {
                c.env_remove(k);
            }
        }
        for (k, v) in self.env.lock().unwrap().iter() {
            c.env(k, v);
        }
        // It serves every block, not one pane.
        c.env_remove("ILLOGICAL_PANE");
        c
    }

    /// In a VM: a sprite service that fetches the release (once) and runs
    /// it on a loopback port there.
    async fn start_vm(&self, provider: &dyn Provider, sprite: &str) -> Result<(), String> {
        self.set(Status::Starting);
        for (name, text) in EXT_FILES {
            provider
                .write_file(sprite, &format!("/tmp/illogical-editor-ext/{name}"), text.as_bytes().to_vec(), 0o644)
                .await
                .map_err(|e| e.to_string())?;
        }
        let sha = |p: &str| SHA256.iter().find(|(k, _)| *k == p).map(|(_, v)| *v).unwrap_or_default();
        let def = ServiceDef {
            cmd: "sh".into(),
            args: vec![
                "-c".into(),
                VM_SCRIPT.into(),
                "sh".into(),
                VERSION.into(),
                sha("linux-amd64").into(),
                sha("linux-arm64").into(),
                VM_PORT.to_string(),
                self.settings.idle.to_string(),
                self.settings.grace.to_string(),
                SETTINGS.into(),
                format!("{EXT_ID}-{EXT_VERSION}"),
                self.settings.releases.clone(),
            ],
            env: vec![],
            dir: None,
        };
        provider.put_service(sprite, "illogical-code-server", &def).await.map_err(|e| e.to_string())?;
        // The first start downloads the release in the VM: give it time.
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(600) {
            if self.connect().await.is_ok() {
                info!(sprite, ms = t0.elapsed().as_millis() as u64, "code-server is up in the VM");
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err(format!("code-server didn't start in {sprite}"))
    }
}

impl Service for Server {
    fn dial(&self) -> BoxFuture<'static, io::Result<Conn>> {
        let me = self.me.upgrade();
        Box::pin(async move {
            let me = me.ok_or_else(|| io::Error::other("the editor is closing"))?;
            if let Ok(c) = me.connect().await {
                return Ok(c);
            }
            // Stopped when idle, or never started: start it now.
            me.ensure().await.map_err(io::Error::other)?;
            me.connect().await
        })
    }

    fn name(&self) -> String {
        match &self.on {
            On::Here => "code-server".into(),
            On::Vm { sprite, .. } => format!("code-server in {sprite}"),
        }
    }
}

/// code-server's own flags: no auth, telemetry, update checks or trust
/// prompts, its paths under `dir`, and stopping when idle.
fn args(dir: &Path, idle: u64, grace: u64) -> Vec<String> {
    let p = |s: &str| dir.join(s).display().to_string();
    vec![
        "--config".into(),
        p("config.yaml"),
        "--user-data-dir".into(),
        p("user"),
        "--extensions-dir".into(),
        p("extensions"),
        "--auth".into(),
        "none".into(),
        "--disable-telemetry".into(),
        "--disable-update-check".into(),
        "--disable-workspace-trust".into(),
        "--ignore-last-opened".into(),
        "--idle-timeout-seconds".into(),
        idle.max(60).to_string(),
        "--reconnection-grace-time".into(),
        grace.to_string(),
    ]
}

/// The settings folder: config, default settings (once), and our
/// extension (always the current one).
fn prepare(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir.join("user/User"))?;
    std::fs::create_dir_all(dir.join("extensions"))?;
    let _ = crate::perm::set(dir, 0o700);
    store::write_atomic(&dir.join("config.yaml"), b"auth: none\ncert: false\n")?;
    let settings = dir.join("user/User/settings.json");
    if !settings.exists() {
        store::write_atomic(&settings, SETTINGS.as_bytes())?;
    }
    install_ext(&dir.join("extensions"))
}

/// Put our extension in an extensions folder and list it there (what
/// `--install-extension` would do with a VSIX).
fn install_ext(exts: &Path) -> io::Result<()> {
    let rel = format!("{EXT_ID}-{EXT_VERSION}");
    let at = exts.join(&rel);
    std::fs::create_dir_all(&at)?;
    for (name, text) in EXT_FILES {
        let path = at.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*text) {
            std::fs::write(&path, text)?;
        }
    }
    let list = exts.join("extensions.json");
    let mut all: Vec<serde_json::Value> =
        std::fs::read(&list).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let ours =
        |v: &serde_json::Value| v["identifier"]["id"].as_str().is_some_and(|id| id == EXT_ID || OLD_IDS.contains(&id));
    if all.iter().any(|v| ours(v) && v["relativeLocation"] == rel.as_str()) {
        return Ok(());
    }
    // An older one of ours goes.
    for old in all.iter().filter(|v| ours(v)) {
        if let Some(r) = old["relativeLocation"].as_str().filter(|r| *r != rel && !r.contains('/')) {
            let _ = std::fs::remove_dir_all(exts.join(r));
        }
    }
    all.retain(|v| !ours(v));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    all.push(serde_json::json!({
        "identifier": { "id": EXT_ID },
        "version": EXT_VERSION,
        "location": { "$mid": 1, "path": at.display().to_string(), "scheme": "file" },
        "relativeLocation": rel,
        "metadata": { "installedTimestamp": now, "pinned": true, "source": "vsix" },
    }));
    std::fs::write(list, serde_json::to_vec(&all)?)
}

/// This host's release name part (`linux-amd64`).
fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux-amd64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("macos", "x86_64") => Some("macos-amd64"),
        ("macos", "aarch64") => Some("macos-arm64"),
        _ => None,
    }
}

/// The release for `platform`, unpacked in `cache` (downloaded and checked
/// the first time): its top directory.
async fn fetch(
    releases: &str,
    cache: &Path,
    platform: &str,
    progress: impl Fn(u64, u64) + Send,
) -> Result<PathBuf, String> {
    let sha = SHA256.iter().find(|(p, _)| *p == platform).map(|(_, s)| *s).ok_or("no checksum for this platform")?;
    let name = format!("code-server-{VERSION}-{platform}");
    fetch_release(
        &format!("{releases}/v{VERSION}/{name}.tar.gz"),
        sha,
        &cache.join(format!("{VERSION}-{platform}")),
        progress,
    )
    .await
}

/// Download a `.tar.gz`, check its SHA-256, unpack it (without its top
/// directory) as `dest`. Nothing to do if `dest` is there.
pub async fn fetch_release(
    url: &str,
    sha256: &str,
    dest: &Path,
    progress: impl Fn(u64, u64) + Send,
) -> Result<PathBuf, String> {
    if dest.join("bin").is_dir() {
        return Ok(dest.to_owned());
    }
    let parent = dest.parent().ok_or("no cache directory")?;
    tokio::fs::create_dir_all(parent).await.map_err(|e| format!("{}: {e}", parent.display()))?;
    let unique = unique();
    let part = parent.join(format!(".{unique}.tar.gz"));
    let unpack = parent.join(format!(".{unique}"));
    let result = async {
        info!(url, "downloading code-server");
        let http = crate::roots::http().connect_timeout(Duration::from_secs(15)).build().map_err(|e| e.to_string())?;
        let mut res = http.get(url).send().await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
        let of = res.content_length().unwrap_or(0);
        let mut file = tokio::fs::File::create(&part).await.map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let (mut got, mut shown) = (0u64, Instant::now());
        progress(0, of);
        while let Some(chunk) = res.chunk().await.map_err(|e| format!("downloading code-server: {e}"))? {
            hash.update(&chunk);
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
            got += chunk.len() as u64;
            if shown.elapsed() > Duration::from_millis(250) {
                progress(got, of);
                shown = Instant::now();
            }
        }
        file.flush().await.map_err(|e| e.to_string())?;
        let digest = hex::encode(hash.finalize());
        if !digest.eq_ignore_ascii_case(sha256) {
            return Err(format!("code-server's download doesn't match its checksum (got {digest})"));
        }
        tokio::fs::create_dir_all(&unpack).await.map_err(|e| e.to_string())?;
        let out = tokio::process::Command::new("tar")
            .arg("-xzf")
            .arg(&part)
            .arg("-C")
            .arg(&unpack)
            .arg("--strip-components=1")
            .output()
            .await
            .map_err(|e| format!("tar: {e}"))?;
        if !out.status.success() {
            return Err(format!("unpacking code-server: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        // Another daemon may have got there first: theirs is as good.
        if tokio::fs::rename(&unpack, dest).await.is_err() && !dest.join("bin").is_dir() {
            return Err(format!("can't put code-server in {}", dest.display()));
        }
        Ok(dest.to_owned())
    }
    .await;
    let _ = tokio::fs::remove_file(&part).await;
    let _ = tokio::fs::remove_dir_all(&unpack).await;
    result
}

/// The last lines of a log, for an error.
fn tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" / ")
}

/// A name part no other process picks at the same time.
fn unique() -> String {
    let b = crate::push::random::<6>();
    format!("{}-{}", std::process::id(), hex::encode(b))
}

/// Runs in a VM as a sprite service: `$1` version, `$2`/`$3` SHA-256 for
/// amd64/arm64, `$4` port, `$5` idle, `$6` grace, `$7` default settings,
/// `$8` our extension's folder name, `$9` where releases come from.
const VM_SCRIPT: &str = r#"set -eu
v=$1 port=$4
case $(uname -m) in
  x86_64) a=amd64 sum=$2 ;;
  aarch64|arm64) a=arm64 sum=$3 ;;
  *) echo "no code-server for $(uname -m)" >&2; exit 3 ;;
esac
root=$HOME/.cache/illogical/code-server/$v-linux-$a
d=$HOME/.local/state/illogical-editor
if [ ! -x "$root/bin/code-server" ]; then
  mkdir -p "$root.part"
  curl -fsSL "$9/v$v/code-server-$v-linux-$a.tar.gz" -o "$root.tgz"
  echo "$sum  $root.tgz" | sha256sum -c - >/dev/null
  tar -xzf "$root.tgz" -C "$root.part" --strip-components=1
  rm -f "$root.tgz"
  mv "$root.part" "$root"
fi
mkdir -p "$d/user/User" "$d/extensions/$8"
printf 'auth: none\ncert: false\n' >"$d/config.yaml"
[ -f "$d/user/User/settings.json" ] || printf '%s' "$7" >"$d/user/User/settings.json"
cp /tmp/illogical-editor-ext/* "$d/extensions/$8/" 2>/dev/null || true
printf '[{"identifier":{"id":"%s"},"version":"%s","location":{"$mid":1,"path":"%s","scheme":"file"},"relativeLocation":"%s","metadata":{"pinned":true,"source":"vsix"}}]' \
  "${8%-*}" "${8##*-}" "$d/extensions/$8" "$8" >"$d/extensions/extensions.json"
exec "$root/bin/code-server" --config "$d/config.yaml" --user-data-dir "$d/user" --extensions-dir "$d/extensions" \
  --auth none --disable-telemetry --disable-update-check --disable-workspace-trust --ignore-last-opened \
  --idle-timeout-seconds "$5" --reconnection-grace-time "$6" --bind-addr "127.0.0.1:$port"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    // code-server here is Linux's and macOS's.
    #[cfg(unix)]
    #[test]
    fn every_platform_has_a_checksum() {
        for p in ["linux-amd64", "linux-arm64", "macos-amd64", "macos-arm64"] {
            let sum = SHA256.iter().find(|(k, _)| *k == p).unwrap().1;
            assert_eq!(sum.len(), 64);
        }
        assert!(platform().is_some());
    }

    // code-server here is Linux's and macOS's.
    #[cfg(unix)]
    #[test]
    fn flags_keep_it_to_its_own_folder() {
        let a = args(Path::new("/s/editor"), 30, 300);
        let after = |f: &str| a[a.iter().position(|x| x == f).unwrap() + 1].clone();
        assert_eq!(after("--config"), "/s/editor/config.yaml");
        assert_eq!(after("--user-data-dir"), "/s/editor/user");
        assert_eq!(after("--extensions-dir"), "/s/editor/extensions");
        assert_eq!(after("--auth"), "none");
        // code-server refuses less than a minute.
        assert_eq!(after("--idle-timeout-seconds"), "60");
        assert!(a.contains(&"--disable-workspace-trust".to_owned()));
    }

    // code-server here is Linux's and macOS's.
    #[cfg(unix)]
    #[test]
    fn settings_and_extension_go_in_once() {
        let dir = std::env::temp_dir().join(format!("ilg-editor-{}", unique()));
        prepare(&dir).unwrap();
        let settings = dir.join("user/User/settings.json");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(v["workbench.colorTheme"], "illogical");
        assert_eq!(v["chat.disableAIFeatures"], true);
        // Someone's own settings stay theirs.
        std::fs::write(&settings, "{\"workbench.colorTheme\": \"Default Light Modern\"}").unwrap();
        // An extension they installed stays listed; an old one of ours goes.
        let exts = dir.join("extensions");
        std::fs::create_dir_all(exts.join("illogical.illogical-0.0.1")).unwrap();
        let mut list: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(exts.join("extensions.json")).unwrap()).unwrap();
        list.retain(|v| v["identifier"]["id"] != EXT_ID);
        list.push(serde_json::json!({ "identifier": { "id": "rust-lang.rust-analyzer" }, "relativeLocation": "ra" }));
        list.push(
            // M27's, under its old name.
            serde_json::json!({ "identifier": { "id": "illogical.illogical" }, "relativeLocation": "illogical.illogical-0.0.1" }),
        );
        std::fs::write(exts.join("extensions.json"), serde_json::to_vec(&list).unwrap()).unwrap();
        prepare(&dir).unwrap();
        assert!(std::fs::read_to_string(&settings).unwrap().contains("Light"));
        let list: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(exts.join("extensions.json")).unwrap()).unwrap();
        let ids: Vec<&str> = list.iter().filter_map(|v| v["identifier"]["id"].as_str()).collect();
        assert_eq!(ids, ["rust-lang.rust-analyzer", EXT_ID]);
        assert_eq!(list[1]["relativeLocation"], format!("{EXT_ID}-{EXT_VERSION}"));
        assert!(!exts.join("illogical.illogical-0.0.1").exists());
        let pkg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(exts.join(format!("{EXT_ID}-{EXT_VERSION}/package.json"))).unwrap())
                .unwrap();
        assert_eq!(format!("{}.{}", pkg["publisher"].as_str().unwrap(), pkg["name"].as_str().unwrap()), EXT_ID);
        assert_eq!(pkg["version"], EXT_VERSION);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn downloads_are_checked() {
        // A tiny release, served once per request.
        let dir = std::env::temp_dir().join(format!("ilg-fetch-{}", unique()));
        std::fs::create_dir_all(dir.join("src/cs/bin")).unwrap();
        std::fs::write(dir.join("src/cs/bin/code-server"), "#!/bin/sh\n").unwrap();
        let tgz = dir.join("r.tar.gz");
        let ok = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&tgz)
            .arg("-C")
            .arg(dir.join("src"))
            .arg("cs")
            .status()
            .unwrap();
        assert!(ok.success());
        let body = std::fs::read(&tgz).unwrap();
        let sum = hex::encode(Sha256::digest(&body));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { return };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = tokio::io::AsyncReadExt::read(&mut s, &mut buf).await;
                    let head =
                        format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(&body).await;
                });
            }
        });
        let url = format!("http://127.0.0.1:{port}/r.tar.gz");
        let bad = fetch_release(&url, &"0".repeat(64), &dir.join("cache/bad"), |_, _| {}).await;
        assert!(bad.unwrap_err().contains("checksum"));
        assert!(!dir.join("cache/bad").exists());
        let seen = Arc::new(Mutex::new(vec![]));
        let s = seen.clone();
        let got = fetch_release(&url, &sum, &dir.join("cache/good"), move |g, o| s.lock().unwrap().push((g, o)))
            .await
            .unwrap();
        assert!(got.join("bin/code-server").is_file());
        assert_eq!(seen.lock().unwrap().first(), Some(&(0, std::fs::metadata(&tgz).unwrap().len())));
        // Nothing left over but the release.
        let left: Vec<_> = std::fs::read_dir(dir.join("cache")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["good"]);
        // Already there: no download.
        assert_eq!(fetch_release("http://127.0.0.1:1/none", &sum, &got, |_, _| {}).await.unwrap(), got);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
