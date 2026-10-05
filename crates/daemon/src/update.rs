//! Is there a newer release? (#176)
//!
//! Twice a day at most, the daemon asks GitHub where
//! `releases/latest` redirects to (the redirect, not the rate-limited
//! API) and reads the tag from it. That one request is all it sends: no
//! version, no identity, nothing else. The answer is kept in
//! `update-check.json` in the state directory, so a restart doesn't ask
//! again. `--no-update-check` (`ILLOGICAL_NO_UPDATE_CHECK`) turns it off;
//! a daemon run from where it was built (tests, development) doesn't ask.
//!
//! `GET /api/update` says what it found and how this install updates:
//! the web client shows a notice with the command.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::server::App;

pub const LATEST: &str = "https://github.com/arugula-salad/illogical/releases/latest";
const RELEASES: &str = "https://github.com/arugula-salad/illogical/releases";
pub const INSTALL_SH: &str = "curl -fsSL https://illogical.widgets.wtf/install.sh | sh";
/// Windows' counterpart, in PowerShell (M59).
pub const INSTALL_PS1: &str = "irm https://illogical.widgets.wtf/install.ps1 | iex";
const EVERY_MS: u64 = 12 * 60 * 60 * 1000;
/// After a failed check (offline, say), try again sooner.
const RETRY: Duration = Duration::from_secs(60 * 60);
/// Not while the daemon is starting.
const FIRST_DELAY: Duration = Duration::from_secs(5);
const CACHE: &str = "update-check.json";

pub struct Settings {
    pub state_dir: PathBuf,
    /// Where `releases/latest` is (tests point it at a fake).
    pub url: String,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
struct Checked {
    /// When (ms since the epoch).
    checked_ms: u64,
    /// The latest release's version, without the `v`.
    latest: Option<String>,
    /// Where it was asked (a check elsewhere doesn't count).
    #[serde(default)]
    url: String,
}

static ENABLED: OnceLock<bool> = OnceLock::new();
static LAST: Mutex<Option<Checked>> = Mutex::new(None);

/// Start checking in the background. Never holds up startup.
/// Not for a build run from where it was built (tests, development: they
/// update with git), unless pointed at somewhere to look.
pub fn start(s: Settings) {
    let enabled = s.enabled && (this_kind() != Kind::Source || s.url != LATEST);
    let _ = ENABLED.set(enabled);
    if !enabled {
        info!("update check off");
        return;
    }
    let cached = read_cache(&s.state_dir, &s.url);
    *LAST.lock().unwrap() = cached.clone();
    tokio::spawn(run(s, cached));
}

async fn run(s: Settings, cached: Option<Checked>) {
    let client = match crate::roots::http()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .user_agent("illogical")
        .build()
    {
        Ok(c) => c,
        Err(e) => return debug!(error = %e, "update check: no HTTP client"),
    };
    let mut wait = match cached {
        Some(c) => Duration::from_millis((c.checked_ms + EVERY_MS).saturating_sub(now_ms())).max(FIRST_DELAY),
        None => FIRST_DELAY,
    };
    loop {
        tokio::time::sleep(wait).await;
        match latest(&client, &s.url).await {
            Ok(v) => {
                let c = Checked { checked_ms: now_ms(), latest: Some(v.clone()), url: s.url.clone() };
                if let Ok(json) = serde_json::to_vec(&c) {
                    let _ = crate::store::write_atomic(&s.state_dir.join(CACHE), &json);
                }
                if newer(&v, env!("CARGO_PKG_VERSION")) {
                    info!(latest = %v, "a newer illogical is out");
                }
                *LAST.lock().unwrap() = Some(c);
                wait = Duration::from_millis(EVERY_MS);
            }
            Err(e) => {
                debug!(error = %e, "update check failed");
                wait = RETRY;
            }
        }
    }
}

/// The version `releases/latest` redirects to.
async fn latest(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let res = client.get(url).send().await?;
    let location = res
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|l| l.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("{url}: {} with no redirect", res.status()))?;
    tag_of(location).ok_or_else(|| anyhow::anyhow!("{url}: no release tag in {location}"))
}

/// `…/releases/tag/v0.17.0` → `0.17.0`.
fn tag_of(location: &str) -> Option<String> {
    let tag = location.split_once("/tag/")?.1.trim_end_matches('/');
    let v = tag.strip_prefix('v').unwrap_or(tag);
    parse(v).map(|_| v.to_owned())
}

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
    let v = (it.next()??, it.next()??, it.next().unwrap_or(Some(0))?);
    it.next().is_none().then_some(v)
}

/// `a` is a later release than `b`.
pub fn newer(a: &str, b: &str) -> bool {
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

fn read_cache(dir: &Path, url: &str) -> Option<Checked> {
    let c: Checked = serde_json::from_slice(&std::fs::read(dir.join(CACHE)).ok()?).ok()?;
    (c.url == url).then_some(c)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// How this copy was installed, for the command that updates it.
#[derive(Serialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Kind {
    /// `install.sh` (or `illogicald install` from a download).
    Script,
    /// Homebrew: `brew upgrade`, then `illogicald install` again.
    #[cfg_attr(windows, allow(dead_code))] // Homebrew is macOS's and Linux's.
    Brew,
    /// The desktop app (its .deb or the macOS app): a newer app replaces
    /// the daemon when it opens.
    App,
    /// Built from source, or run from somewhere else.
    Source,
}

#[cfg(any(unix, test))]
/// The service runs a copy in `~/.local/bin` whichever way it came, so
/// look for where that copy came from.
fn kind(exe: &Path, home: &Path, exists: impl Fn(&Path) -> Option<PathBuf>) -> Kind {
    let local = home.join(".local/bin/illogicald");
    let deb = Path::new("/usr/bin/illogicald");
    // The macOS app's own launch agent runs the copy in its bundle (M46).
    if exe.ends_with("Contents/MacOS/illogicald") {
        return Kind::App;
    }
    if exe != local && exe != deb {
        return Kind::Source;
    }
    let brew_prefixes = std::env::var_os("HOMEBREW_PREFIX").map(PathBuf::from);
    let brew = brew_prefixes
        .into_iter()
        .chain(["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"].map(PathBuf::from))
        .filter_map(|p| exists(&p.join("bin/illogicald")))
        .any(|real| real.components().any(|c| c.as_os_str() == "Cellar"));
    if brew {
        return Kind::Brew;
    }
    let app =
        [deb.to_path_buf(), PathBuf::from("/Applications/illogical.app"), home.join("Applications/illogical.app")]
            .iter()
            .any(|p| exists(p).is_some());
    if app {
        return Kind::App;
    }
    Kind::Script
}

#[derive(Serialize)]
struct Status {
    /// This daemon's version.
    current: &'static str,
    /// The latest release, once a check has found it.
    #[serde(skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
    /// `latest` is newer than `current`.
    newer: bool,
    /// The check is on.
    enabled: bool,
    kind: Kind,
    /// The command that updates this install, where there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'static str>,
    /// The release's page.
    url: String,
}

/// Windows: `illogicald install` puts it in `%LOCALAPPDATA%\Programs\illogical`
/// (from install.ps1 or the desktop app, which lives in
/// `%LOCALAPPDATA%\illogical`).
#[cfg(windows)]
fn this_kind() -> Kind {
    let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok()).unwrap_or_default();
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default());
    let installed = local.join("Programs").join("illogical").join("illogicald.exe").canonicalize().ok();
    if installed.as_ref() != Some(&exe) {
        return Kind::Source;
    }
    if local.join("illogical").join("illogical-desktop.exe").is_file() { Kind::App } else { Kind::Script }
}

#[cfg(unix)]
fn this_kind() -> Kind {
    let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok()).unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    kind(&exe, &home, |p| p.canonicalize().ok())
}

fn status() -> Status {
    let current = env!("CARGO_PKG_VERSION");
    let latest = LAST.lock().unwrap().as_ref().and_then(|c| c.latest.clone());
    let kind = this_kind();
    Status {
        current,
        newer: latest.as_deref().is_some_and(|l| newer(l, current)),
        url: latest.as_ref().map_or_else(|| RELEASES.to_owned(), |l| format!("{RELEASES}/tag/v{l}")),
        latest,
        enabled: ENABLED.get().copied().unwrap_or(false),
        command: match kind {
            Kind::Script => Some(if cfg!(windows) { INSTALL_PS1 } else { INSTALL_SH }),
            Kind::Brew => Some("brew upgrade illogical && illogicald install"),
            Kind::App | Kind::Source => None,
        },
        kind,
    }
}

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/update", get(|| async { Json(status()) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_tag_from_the_redirect() {
        assert_eq!(
            tag_of("https://github.com/arugula-salad/illogical/releases/tag/v0.17.0").as_deref(),
            Some("0.17.0")
        );
        assert_eq!(tag_of("/releases/tag/v1.2.3/").as_deref(), Some("1.2.3"));
        assert_eq!(tag_of("https://github.com/arugula-salad/illogical/releases"), None);
        assert_eq!(tag_of("/releases/tag/nightly"), None);
    }

    #[test]
    fn compares_versions() {
        assert!(newer("0.17.0", "0.16.0"));
        assert!(newer("0.16.10", "0.16.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.16.0", "0.16.0"));
        assert!(!newer("0.15.2", "0.16.0"));
        assert!(!newer("junk", "0.16.0"));
    }

    #[test]
    fn tells_installs_apart() {
        let home = Path::new("/home/me");
        let local = home.join(".local/bin/illogicald");
        let none = |_: &Path| None;
        assert_eq!(kind(&local, home, none), Kind::Script);
        assert_eq!(kind(Path::new("/home/me/src/illogical/target/release/illogicald"), home, none), Kind::Source);
        let brew = |p: &Path| {
            (p == Path::new("/opt/homebrew/bin/illogicald"))
                .then(|| PathBuf::from("/opt/homebrew/Cellar/illogical/0.16.0/bin/illogicald"))
        };
        assert_eq!(kind(&local, home, brew), Kind::Brew);
        let deb = |p: &Path| (p == Path::new("/usr/bin/illogicald")).then(|| p.to_path_buf());
        assert_eq!(kind(&local, home, deb), Kind::App);
        assert_eq!(kind(Path::new("/usr/bin/illogicald"), home, deb), Kind::App);
        let mac = |p: &Path| (p == Path::new("/Applications/illogical.app")).then(|| p.to_path_buf());
        assert_eq!(kind(&local, home, mac), Kind::App);
        assert_eq!(kind(Path::new("/Applications/illogical.app/Contents/MacOS/illogicald"), home, none), Kind::App);
    }

    #[test]
    fn cache_round_trips() {
        let dir = std::env::temp_dir().join(format!("illogical-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = Checked { checked_ms: 5, latest: Some("0.17.0".into()), url: LATEST.into() };
        std::fs::write(dir.join(CACHE), serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(read_cache(&dir, LATEST), Some(c));
        assert_eq!(read_cache(&dir, "http://127.0.0.1:1/latest"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
