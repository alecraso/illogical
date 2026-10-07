//! Is there a newer release? (#176)
//!
//! Twice a day at most, the daemon asks GitHub where
//! `releases/latest` redirects to (the redirect, not the rate-limited
//! API) and reads the tag from it. That one request is all it sends: no
//! version, no identity, nothing else. The answer is kept in
//! `update-check.json` in the state directory, so a restart doesn't ask
//! again. `--no-update-check` (`ARUGULA_NO_UPDATE_CHECK`) turns it off;
//! a daemon run from where it was built (tests, development) doesn't ask.
//!
//! `GET /api/update` says what it found and how this install updates:
//! the web client shows a notice with an Update now button where the
//! daemon can update itself (`selfupdate`, #391), else the command.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use axum::{Json, Router, routing::get};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::server::App;

pub const LATEST: &str = "https://github.com/arugula-salad/arugula/releases/latest";
const RELEASES: &str = "https://github.com/arugula-salad/arugula/releases";
pub const INSTALL_SH: &str = "curl -fsSL https://arugula.io/install.sh | sh";
/// Windows' counterpart, in PowerShell (M59).
pub const INSTALL_PS1: &str = "irm https://arugula.io/install.ps1 | iex";
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
/// Where `releases/latest` is, for the downloads (`selfupdate`).
static URL: OnceLock<String> = OnceLock::new();
static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();
static LAST: Mutex<Option<Checked>> = Mutex::new(None);

/// Start checking in the background. Never holds up startup.
/// Not for a build run from where it was built (tests, development: they
/// update with git), unless pointed at somewhere to look.
pub fn start(s: Settings) {
    let enabled = s.enabled && (this_kind() != Kind::Source || s.url != LATEST);
    let _ = ENABLED.set(enabled);
    let _ = URL.set(s.url.clone());
    let _ = STATE_DIR.set(s.state_dir.clone());
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
        .user_agent("arugula")
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
                    info!(latest = %v, "a newer Arugula is out");
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

/// Ask once, now (`arugulad update`).
pub async fn latest_once(url: &str) -> anyhow::Result<String> {
    let client = crate::roots::http()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .user_agent("arugula")
        .build()?;
    latest(&client, url).await
}

/// The version `releases/latest` redirects to. Redirects that aren't to a
/// tag are followed first, a few at most: a renamed repository answers its
/// old name with a redirect to the same page under the new one (#506).
async fn latest(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let mut at = reqwest::Url::parse(url)?;
    for _ in 0..5 {
        let res = client.get(at.clone()).send().await?;
        let location = res
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|l| l.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("{at}: {} with no redirect", res.status()))?;
        if let Some(v) = tag_of(location) {
            return Ok(v);
        }
        at = at.join(location)?;
    }
    anyhow::bail!("{url}: no release tag after 5 redirects")
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
    /// `install.sh` (or `arugulad install` from a download).
    Script,
    /// Homebrew: `brew upgrade`, then `arugulad install` again.
    #[cfg_attr(windows, allow(dead_code))] // Homebrew is macOS's and Linux's.
    Brew,
    /// The desktop app (its .deb or the macOS app). The daemon updates
    /// itself (`selfupdate`).
    App,
    /// Built from source, or run from somewhere else.
    Source,
}

#[cfg(any(unix, test))]
/// The service runs a copy in `~/.local/bin` whichever way it came, so
/// look for where that copy came from.
fn kind(exe: &Path, home: &Path, exists: impl Fn(&Path) -> Option<PathBuf>) -> Kind {
    let local = home.join(".local/bin/arugulad");
    let deb = Path::new("/usr/bin/arugulad");
    // The macOS app's own launch agent runs the copy in its bundle (M46).
    if exe.ends_with("Contents/MacOS/arugulad") {
        return Kind::App;
    }
    if exe != local && exe != deb {
        return Kind::Source;
    }
    let brew_prefixes = std::env::var_os("HOMEBREW_PREFIX").map(PathBuf::from);
    let brew = brew_prefixes
        .into_iter()
        .chain(["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"].map(PathBuf::from))
        .filter_map(|p| exists(&p.join("bin/arugulad")))
        .any(|real| real.components().any(|c| c.as_os_str() == "Cellar"));
    if brew {
        return Kind::Brew;
    }
    // The app under either name (#505): one from before the rename that
    // updated in place is still illogical.app.
    let bundles = ["Arugula.app", "illogical.app"];
    let app = std::iter::once(deb.to_path_buf())
        .chain(bundles.iter().flat_map(|b| [Path::new("/Applications").join(b), home.join("Applications").join(b)]))
        .any(|p| exists(&p).is_some());
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
    /// This daemon can update itself (`POST /api/update/apply`).
    apply: bool,
    /// An update under way, or the one that failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    applying: Option<crate::selfupdate::Applying>,
}

/// Windows: `arugulad install` puts it in `%LOCALAPPDATA%\Programs\arugula`
/// (from install.ps1 or the desktop app, which lives in
/// `%LOCALAPPDATA%\Arugula`, or before the rename `%LOCALAPPDATA%\illogical`,
/// #505).
#[cfg(windows)]
fn this_kind() -> Kind {
    let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok()).unwrap_or_default();
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default());
    // Ours, or illogical's (#505).
    let installed = [("arugula", "arugulad.exe"), ("illogical", "illogicald.exe")]
        .iter()
        .any(|(d, b)| local.join("Programs").join(d).join(b).canonicalize().ok().as_ref() == Some(&exe));
    if !installed {
        return Kind::Source;
    }
    if desktop_app(&local) { Kind::App } else { Kind::Script }
}

/// The desktop app is installed (Windows): Arugula, or the app from before
/// the rename (#505), which stays until the new app's installer removes it.
#[cfg(any(windows, test))]
fn desktop_app(local: &Path) -> bool {
    [("Arugula", "arugula-desktop.exe"), ("illogical", "illogical-desktop.exe")]
        .iter()
        .any(|(dir, exe)| local.join(dir).join(exe).is_file())
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
        command: command(kind),
        apply: crate::selfupdate::refusal().is_none(),
        applying: crate::selfupdate::applying(),
        kind,
    }
}

fn command(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::Script | Kind::App => Some(if cfg!(windows) { INSTALL_PS1 } else { INSTALL_SH }),
        Kind::Brew => Some(BREW),
        Kind::Source => None,
    }
}

const BREW: &str = "brew upgrade arugula && arugulad install";

/// The install script's copy or the app's: `selfupdate` may replace it.
/// Not Homebrew's (brew does) or a build's.
pub fn updates_itself() -> bool {
    matches!(this_kind(), Kind::Script | Kind::App)
}

/// `arugulad update` isn't how this one updates: the command that is.
pub fn other_command() -> Option<&'static str> {
    match this_kind() {
        Kind::Brew => Some(BREW),
        Kind::Source => Some("git pull, build it, and run `arugulad install` again"),
        Kind::Script | Kind::App => None,
    }
}

/// The latest release found, when it's newer than this.
pub fn newer_release() -> Option<String> {
    let latest = LAST.lock().unwrap().as_ref().and_then(|c| c.latest.clone())?;
    newer(&latest, env!("CARGO_PKG_VERSION")).then_some(latest)
}

/// The daemon's state directory (an update is unpacked there).
pub fn state_dir() -> Option<PathBuf> {
    STATE_DIR.get().cloned()
}

/// Where `releases/latest` is looked up.
pub fn url() -> String {
    URL.get().cloned().unwrap_or_else(|| LATEST.to_owned())
}

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/update", get(|| async { Json(status()) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #505: the Windows app under either name.
    #[test]
    fn finds_the_windows_app_under_either_name() {
        let local = std::env::temp_dir().join(format!("arugula-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&local);
        assert!(!desktop_app(&local));
        for (dir, exe) in [("illogical", "illogical-desktop.exe"), ("Arugula", "arugula-desktop.exe")] {
            std::fs::create_dir_all(local.join(dir)).unwrap();
            std::fs::write(local.join(dir).join(exe), "").unwrap();
            assert!(desktop_app(&local), "{dir}");
            let _ = std::fs::remove_dir_all(local.join(dir));
        }
        let _ = std::fs::remove_dir_all(&local);
    }

    /// #506: the old repository's name redirects to the new one's
    /// `releases/latest`, which redirects to the tag.
    #[tokio::test]
    async fn follows_a_renamed_repository_to_the_tag() {
        use axum::{Router, response::Redirect, routing::get};
        let app = Router::new()
            .route("/old/releases/latest", get(|| async { Redirect::permanent("/new/releases/latest") }))
            .route("/new/releases/latest", get(|| async { Redirect::to("/new/releases/tag/v9.8.7") }))
            .route("/loop/releases/latest", get(|| async { Redirect::to("/loop/releases/latest") }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        assert_eq!(latest(&client, &format!("{base}/old/releases/latest")).await.unwrap(), "9.8.7");
        assert_eq!(latest(&client, &format!("{base}/new/releases/latest")).await.unwrap(), "9.8.7");
        assert!(latest(&client, &format!("{base}/loop/releases/latest")).await.is_err(), "gives up on a loop");
    }

    #[test]
    fn reads_the_tag_from_the_redirect() {
        assert_eq!(tag_of("https://github.com/arugula-salad/arugula/releases/tag/v0.17.0").as_deref(), Some("0.17.0"));
        assert_eq!(tag_of("/releases/tag/v1.2.3/").as_deref(), Some("1.2.3"));
        assert_eq!(tag_of("https://github.com/arugula-salad/arugula/releases"), None);
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
        let local = home.join(".local/bin/arugulad");
        let none = |_: &Path| None;
        assert_eq!(kind(&local, home, none), Kind::Script);
        assert_eq!(kind(Path::new("/home/me/src/arugula/target/release/arugulad"), home, none), Kind::Source);
        let brew = |p: &Path| {
            (p == Path::new("/opt/homebrew/bin/arugulad"))
                .then(|| PathBuf::from("/opt/homebrew/Cellar/arugula/0.16.0/bin/arugulad"))
        };
        assert_eq!(kind(&local, home, brew), Kind::Brew);
        let deb = |p: &Path| (p == Path::new("/usr/bin/arugulad")).then(|| p.to_path_buf());
        assert_eq!(kind(&local, home, deb), Kind::App);
        assert_eq!(kind(Path::new("/usr/bin/arugulad"), home, deb), Kind::App);
        let mac = |p: &Path| (p == Path::new("/Applications/Arugula.app")).then(|| p.to_path_buf());
        assert_eq!(kind(&local, home, mac), Kind::App);
        assert_eq!(kind(Path::new("/Applications/Arugula.app/Contents/MacOS/arugulad"), home, none), Kind::App);
        // #505: the app from before the rename, updated in place.
        let old = |p: &Path| (p == home.join("Applications/illogical.app")).then(|| p.to_path_buf());
        assert_eq!(kind(&local, home, old), Kind::App);
    }

    #[test]
    fn cache_round_trips() {
        let dir = std::env::temp_dir().join(format!("arugula-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = Checked { checked_ms: 5, latest: Some("0.17.0".into()), url: LATEST.into() };
        std::fs::write(dir.join(CACHE), serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(read_cache(&dir, LATEST), Some(c));
        assert_eq!(read_cache(&dir, "http://127.0.0.1:1/latest"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
