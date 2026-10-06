//! The daemon updates itself, when asked (#391).
//!
//! `POST /api/update/apply` (the web notice's Update now, for owners) and
//! `illogicald update` do what install.sh does: download this platform's
//! archive from the release, check it against the release's `SHA256SUMS`,
//! unpack it, and run the new `illogicald install`. That copies the
//! binaries into place, keeps the flags the last install wrote, and
//! restarts the service with the panes kept (the systemd FD store, pane
//! shims on macOS, pane hosts on Windows). Anything that fails before the
//! restart leaves the running daemon as it was, and says why.
//!
//! From the daemon, the new install runs outside the service, since the
//! restart stops what runs in it: in its own transient unit
//! (`systemd-run`) on Linux, in its own session on macOS, broken away from
//! the task's job on Windows.
//!
//! The button is only for a service this can update without asking anyone
//! else: the one `illogicald install` set up (install.sh, the desktop
//! app), run as the user. Homebrew updates with brew; a `--system` service
//! needs sudo or an administrator; a daemon run by hand or from a build
//! isn't a service. Those keep the notice with their command.
//!
//! macOS's app runs the daemon in its bundle, as its launch agent. That
//! copy can't be replaced without breaking the app's signature, so an
//! update goes into `~/.local/bin` and the bundle's copy hands on to it
//! when it's newer (`hand_on`).

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use axum::{Json, Router, http::StatusCode, routing::post};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

/// The macOS app's launch agent (crates/desktop/macos).
pub const APP_AGENT: &str = "wtf.widgets.illogical.daemon";
/// How long a download may take.
const DOWNLOAD: Duration = Duration::from_secs(10 * 60);
/// After the new install says it's done, this daemon should be gone.
const GONE: Duration = Duration::from_secs(30);

/// This platform's build in a release, as the release workflow names it.
fn target() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-musl")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some("aarch64-unknown-linux-musl")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("x86_64-pc-windows-msvc")
    } else {
        None
    }
}

/// `illogical-0.24.0-x86_64-unknown-linux-musl`, the archive's name
/// without its extension (and the folder in it).
fn stem(version: &str) -> Option<String> {
    Some(format!("illogical-{version}-{}", target()?))
}

const EXT: &str = if cfg!(windows) { "zip" } else { "tar.gz" };
const EXE: &str = if cfg!(windows) { "illogicald.exe" } else { "illogicald" };

/// `…/releases/latest` → `…/releases`, where the downloads are.
pub fn releases(latest_url: &str) -> String {
    latest_url.trim_end_matches('/').trim_end_matches("/latest").to_owned()
}

/// The line for `name` in a `SHA256SUMS`.
fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (sum, file) = l.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| sum.to_ascii_lowercase())
    })
}

async fn get(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let res = client.get(url).send().await.with_context(|| format!("downloading {url}"))?;
    ensure!(res.status().is_success(), "downloading {url}: {}", res.status());
    Ok(res.bytes().await.with_context(|| format!("downloading {url}"))?.to_vec())
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(crate::roots::http().timeout(DOWNLOAD).user_agent("illogical").build()?)
}

/// Download release `version` into `into`, check it against the release's
/// `SHA256SUMS` and unpack it: the new `illogicald`. Nothing is unpacked
/// from an archive whose sum doesn't match.
pub async fn fetch(releases: &str, version: &str, into: &Path) -> anyhow::Result<PathBuf> {
    let stem = stem(version).context("there's no release build for this platform")?;
    let name = format!("{stem}.{EXT}");
    let url = |f: &str| format!("{releases}/download/v{version}/{f}");
    let client = client()?;
    let sums = get(&client, &url("SHA256SUMS")).await?;
    let want = sum_for(&String::from_utf8_lossy(&sums), &name)
        .with_context(|| format!("{name} isn't in release v{version}'s SHA256SUMS"))?;
    let bytes = get(&client, &url(&name)).await?;
    let got = hex::encode(Sha256::digest(&bytes));
    ensure!(
        got == want,
        "{name} doesn't match release v{version}'s SHA256SUMS (it's {got}, not {want}): not installing it"
    );

    let _ = std::fs::remove_dir_all(into);
    std::fs::create_dir_all(into)?;
    let archive = into.join(&name);
    std::fs::write(&archive, &bytes)?;
    unpack(&archive, into)?;
    let exe = into.join(&stem).join(EXE);
    let says = version_of(&exe).with_context(|| format!("{} doesn't run", exe.display()))?;
    ensure!(says == version, "{} says it's {says}, not {version}", exe.display());
    Ok(exe)
}

/// Unpack with what the platform has: tar, or on Windows, PowerShell's
/// Expand-Archive (as install.ps1 does; the release's zip has `\` paths).
fn unpack(archive: &Path, into: &Path) -> anyhow::Result<()> {
    let out = if cfg!(windows) {
        let q = |p: &Path| p.display().to_string().replace('\'', "''");
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!("Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force", q(archive), q(into)))
            .output()
    } else {
        Command::new("tar").arg("-xzf").arg(archive).arg("-C").arg(into).output()
    }
    .context("unpacking the release")?;
    ensure!(out.status.success(), "unpacking {}: {}", archive.display(), String::from_utf8_lossy(&out.stderr).trim());
    Ok(())
}

/// `illogicald --version` says `illogicald 0.24.0`.
pub fn version_of(bin: &Path) -> Option<String> {
    // Not into a log file: the app's agent sets ILLOGICAL_LOG_FILE, and
    // `hand_on` asks before main takes it out of the environment.
    let out = Command::new(bin).arg("--version").env_remove("ILLOGICAL_LOG_FILE").output().ok()?;
    out.status.success().then_some(())?;
    String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1).map(str::to_owned)
}

/// How this daemon is kept running, where it's one this can restart as
/// the user.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Service {
    /// `illogicald install`'s systemd user unit.
    Systemd,
    /// `illogicald install`'s launch agent (macOS).
    LaunchAgent,
    /// The macOS app's launch agent.
    AppAgent,
    /// `illogicald install`'s logon task (Windows).
    Task,
}

fn service() -> Option<Service> {
    if cfg!(target_os = "linux") {
        // systemd sets INVOCATION_ID for what it starts; the unit is the one
        // `install` wrote.
        let home = PathBuf::from(std::env::var_os("HOME")?);
        let unit = home.join(".config/systemd/user/illogicald.service");
        let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
        let ours = exe == home.join(".local/bin/illogicald").canonicalize().ok()?;
        (std::env::var_os("INVOCATION_ID").is_some() && unit.is_file() && ours).then_some(Service::Systemd)
    } else if cfg!(target_os = "macos") {
        // launchd names the job it started. A `--system` LaunchDaemon is
        // `illogicald.USER`: that one needs sudo.
        match std::env::var("XPC_SERVICE_NAME").ok()?.as_str() {
            "illogicald" => Some(Service::LaunchAgent),
            APP_AGENT => Some(Service::AppAgent),
            _ => None,
        }
    } else if cfg!(windows) {
        windows_task().then_some(Service::Task)
    } else {
        None
    }
}

/// Windows: this is the installed copy, run by the logon task (not the
/// `--system` one at boot, which needs an administrator to change).
#[cfg(windows)]
fn windows_task() -> bool {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default());
    let installed = local.join("Programs").join("illogical").join("illogicald.exe").canonicalize().ok();
    let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok());
    if installed.is_none() || installed != exe {
        return false;
    }
    Command::new("schtasks")
        .args(["/Query", "/TN", "illogicald", "/XML"])
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("<LogonTrigger>"))
}

#[cfg(not(windows))]
fn windows_task() -> bool {
    false
}

/// Why the button isn't offered here, if it isn't. Asked once: none of
/// it changes while the daemon runs.
pub fn refusal() -> Option<&'static str> {
    static WHY: OnceLock<Option<&'static str>> = OnceLock::new();
    *WHY.get_or_init(|| {
        if target().is_none() {
            return Some("there's no release build for this platform");
        }
        if !crate::update::updates_itself() {
            return Some("this install updates another way (see the command)");
        }
        if service().is_none() {
            return Some("this daemon isn't the service `illogicald install` set up for you");
        }
        None
    })
}

/// Where an update is up to, for the web notice.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Applying {
    /// The version going in.
    pub to: String,
    /// `downloading`, `installing`, `restarting`, or `failed`.
    pub stage: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

static APPLYING: Mutex<Option<Applying>> = Mutex::new(None);

pub fn applying() -> Option<Applying> {
    APPLYING.lock().unwrap().clone()
}

fn stage(to: &str, stage: &'static str, error: Option<String>) {
    *APPLYING.lock().unwrap() = Some(Applying { to: to.to_owned(), stage, error });
}

/// Start updating to `to`, in the background.
fn start(to: String, releases: String, state_dir: PathBuf) -> Result<Applying, (StatusCode, String)> {
    {
        let mut a = APPLYING.lock().unwrap();
        if let Some(a) = a.as_ref().filter(|a| a.stage != "failed") {
            return Ok(a.clone());
        }
        *a = Some(Applying { to: to.clone(), stage: "downloading", error: None });
    }
    info!(to = %to, "updating illogical");
    tokio::spawn(async move {
        let dir = state_dir.join("update");
        let exe = match fetch(&releases, &to, &dir).await {
            Ok(exe) => exe,
            Err(e) => {
                warn!(error = %format!("{e:#}"), "update failed");
                return stage(&to, "failed", Some(format!("{e:#}")));
            }
        };
        stage(&to, "installing", None);
        let log = state_dir.join("update.log");
        // A thread of its own, not spawn_blocking: the runtime's shutdown
        // waits for those, and the install waits for this daemon to exit.
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(hand_off(&exe, &log));
        });
        let ran = rx.await;
        let why = match ran {
            Ok(Ok(())) => {
                // The install restarted the service, so this should be gone
                // by now.
                stage(&to, "restarting", None);
                tokio::time::sleep(GONE).await;
                "the new daemon was installed, but this one wasn't restarted onto it".to_owned()
            }
            Ok(Err(e)) => format!("{e:#}"),
            Err(e) => e.to_string(),
        };
        warn!(error = %why, "update failed");
        stage(&to, "failed", Some(why));
    });
    Ok(applying().unwrap_or(Applying { to: String::new(), stage: "downloading", error: None }))
}

/// Run the new `illogicald install` outside this service, and wait for it.
/// It restarts the service, which ends this daemon; if it ends first
/// instead, it failed (or didn't restart anything), and says why in `log`.
fn hand_off(exe: &Path, log: &Path) -> anyhow::Result<()> {
    let out = std::fs::File::create(log).with_context(|| format!("creating {}", log.display()))?;
    let mut cmd = outside(exe);
    cmd.stdin(std::process::Stdio::null()).stdout(out.try_clone()?).stderr(out);
    let status = spawn(&mut cmd)?.wait()?;
    if status.success() {
        return Ok(());
    }
    let said = std::fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = said.lines().rev().take(8).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    bail!("{} install failed ({status}): {}", exe.display(), tail.join("\n"))
}

/// `exe install`, set to run where the service's restart won't stop it.
fn outside(exe: &Path) -> Command {
    if cfg!(target_os = "linux") {
        let mut c = Command::new("systemd-run");
        let unit = format!("illogicald-update-{}", crate::store::now_ms());
        c.args(["--user", "--collect", "--quiet", "--wait", "--pipe", "--unit", &unit, "--"]).arg(exe).arg("install");
        return c;
    }
    let mut c = Command::new(exe);
    c.arg("install");
    // launchd stops the job's process group: this goes in its own session.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            c.pre_exec(|| {
                nix::unistd::setsid()?;
                Ok(())
            });
        }
    }
    c
}

/// Windows: out of the task's job, which `schtasks /End` ends whole. A
/// job that doesn't allow that refuses it; then try without.
#[cfg(windows)]
fn spawn(cmd: &mut Command) -> anyhow::Result<std::process::Child> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
    };
    let flags = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
    match cmd.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB).spawn() {
        Ok(c) => Ok(c),
        Err(e) => {
            warn!(error = %e, "update: can't leave the task's job; running the install inside it");
            Ok(cmd.creation_flags(flags).spawn()?)
        }
    }
}

#[cfg(not(windows))]
fn spawn(cmd: &mut Command) -> anyhow::Result<std::process::Child> {
    Ok(cmd.spawn()?)
}

/// `POST /api/update/apply`: owners only (the default for /api).
async fn apply() -> Result<Json<Applying>, (StatusCode, String)> {
    if let Some(why) = refusal() {
        return Err((StatusCode::CONFLICT, format!("can't update from here: {why}")));
    }
    let to = crate::update::newer_release().ok_or((StatusCode::CONFLICT, "this is the latest release".into()))?;
    let dir = crate::update::state_dir().ok_or((StatusCode::CONFLICT, "the update check isn't set up".into()))?;
    start(to, releases(&crate::update::url()), dir).map(Json)
}

pub fn routes() -> Router<Arc<crate::server::App>> {
    Router::new().route("/api/update/apply", post(apply))
}

/// `illogicald update`: ask, then do the same from a terminal. The new
/// `install` runs here, in the foreground, so its output (and any
/// question it asks, like sudo's for a `--system` service) is the user's.
pub fn cli(yes: bool, latest_url: &str) -> anyhow::Result<()> {
    if let Some(command) = crate::update::other_command() {
        bail!("this install updates with: {command}");
    }
    let rt = tokio::runtime::Runtime::new()?;
    let latest = rt.block_on(crate::update::latest_once(latest_url))?;
    let current = env!("CARGO_PKG_VERSION");
    if !crate::update::newer(&latest, current) {
        println!("illogical {current} is the latest release.");
        return Ok(());
    }
    println!("illogical {latest} is out; this is {current}. Panes keep running while the daemon restarts.");
    if !yes && !ask("Update now? [y/N] ")? {
        println!("Not updated.");
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("illogical-update-{}", std::process::id()));
    let exe = rt.block_on(fetch(&releases(latest_url), &latest, &dir))?;
    println!("checked {latest} against the release's SHA256SUMS; installing it");
    let status = Command::new(&exe).arg("install").status().with_context(|| format!("running {}", exe.display()))?;
    let _ = std::fs::remove_dir_all(&dir);
    ensure!(status.success(), "{} install failed ({status})", exe.display());
    Ok(())
}

fn ask(question: &str) -> anyhow::Result<bool> {
    use std::io::Write;
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

/// macOS: the app's launch agent runs the `illogicald` in the app's
/// bundle. When an update has put a newer one in `~/.local/bin`, run that
/// instead (same arguments, same environment). Called first thing, before
/// anything else starts.
#[cfg(target_os = "macos")]
pub fn hand_on() {
    use std::os::unix::process::CommandExt;
    if std::env::var("XPC_SERVICE_NAME").as_deref() != Ok(APP_AGENT) {
        return;
    }
    let in_bundle = std::env::current_exe().is_ok_and(|e| e.ends_with("Contents/MacOS/illogicald"));
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let newer = home.join(".local/bin/illogicald");
    if !in_bundle || !version_of(&newer).is_some_and(|v| crate::update::newer(&v, env!("CARGO_PKG_VERSION"))) {
        return;
    }
    let err = Command::new(&newer).args(std::env::args_os().skip(1)).exec();
    eprintln!("illogicald: running the newer {} failed ({err}); carrying on with this one", newer.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_downloads() {
        assert_eq!(
            releases("https://github.com/arugula-salad/illogical/releases/latest"),
            "https://github.com/arugula-salad/illogical/releases"
        );
        assert_eq!(releases("http://127.0.0.1:9/releases/latest/"), "http://127.0.0.1:9/releases");
    }

    #[test]
    fn reads_sha256sums() {
        let sums = "AB12  illogical-1.0.0-x.tar.gz\ncd34 *illogical-1.0.0-y.zip\n";
        assert_eq!(sum_for(sums, "illogical-1.0.0-x.tar.gz").as_deref(), Some("ab12"));
        assert_eq!(sum_for(sums, "illogical-1.0.0-y.zip").as_deref(), Some("cd34"));
        assert_eq!(sum_for(sums, "illogical-1.0.0-x.tar"), None);
    }

    /// A fake release on loopback: `SHA256SUMS` and this platform's
    /// archive, holding an `illogicald` that says it's `version`.
    /// A fresh directory for one test.
    #[cfg(unix)]
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("illogical-selfupdate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    async fn fake_release(name: &str, version: &str, sums_lie: bool) -> String {
        use axum::routing::get;
        let tmp = scratch(&format!("{name}-release"));
        let stem = stem(version).unwrap();
        let folder = tmp.join(&stem);
        std::fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("illogicald");
        std::fs::write(&exe, format!("#!/bin/sh\necho illogicald {version}\n")).unwrap();
        crate::perm::set(&exe, 0o755).unwrap();
        let name = format!("{stem}.tar.gz");
        let mut tar = Command::new("tar");
        tar.arg("-czf").arg(tmp.join(&name)).arg("-C").arg(&tmp).arg(&stem);
        assert!(tar.status().unwrap().success());
        let archive = std::fs::read(tmp.join(&name)).unwrap();
        let sum = if sums_lie { "0".repeat(64) } else { hex::encode(Sha256::digest(&archive)) };
        let sums = format!("{sum}  {name}\n");
        let app = Router::new()
            .route(&format!("/releases/download/v{version}/SHA256SUMS"), get(move || async move { sums }))
            .route(&format!("/releases/download/v{version}/{name}"), get(move || async move { archive }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/releases")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fetches_and_checks_a_release() {
        let releases = fake_release("ok", "9.9.9", false).await;
        let into = scratch("ok");
        let exe = fetch(&releases, "9.9.9", &into).await.unwrap();
        assert_eq!(version_of(&exe).as_deref(), Some("9.9.9"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_a_bad_checksum() {
        let releases = fake_release("bad", "9.9.9", true).await;
        let into = scratch("bad");
        let err = fetch(&releases, "9.9.9", &into).await.unwrap_err().to_string();
        assert!(err.contains("doesn't match"), "{err}");
        assert!(!into.join(stem("9.9.9").unwrap()).exists(), "unpacked a bad archive");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn says_when_the_release_is_missing() {
        let releases = fake_release("missing", "9.9.9", false).await;
        let into = scratch("missing");
        let err = format!("{:#}", fetch(&releases, "9.9.8", &into).await.unwrap_err());
        assert!(err.contains("404"), "{err}");
    }
}
