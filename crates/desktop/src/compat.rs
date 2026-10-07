//! Does the daemon here speak a protocol this app works with (#390)?
//!
//! The daemon reports its protocol number (`arugula_proto::PROTOCOL`)
//! in `GET /api/host`; one too old to report it speaks
//! `PROTOCOL_BASELINE`. At launch, and again after the setup page starts
//! or updates the daemon, the app compares it with [`SUPPORTED`]. Outside
//! that range the window opens on the setup page, which says which side
//! is behind and offers its update: the daemon's own (#391, `POST
//! /api/update/apply`, else the command it gives) or the app's
//! (`updates.rs`).
//!
//! A daemon that reports no protocol (0.23 and earlier) is judged by its
//! version too: below [`MIN_VERSION`] it's behind (#317), whatever the
//! baseline says.

use std::{ops::RangeInclusive, path::Path, sync::Mutex, time::Duration};

use arugula_proto::{PROTOCOL, PROTOCOL_BASELINE};
use serde_json::Value;
use tauri::{AppHandle, WebviewWindow};

/// The oldest daemon protocol this app still handles. Raise it when the
/// app drops what an older daemon needs.
const MIN_PROTOCOL: u32 = 1;

/// The daemon protocols this app works with: up to the one its own
/// `proto` speaks.
pub const SUPPORTED: RangeInclusive<u32> = MIN_PROTOCOL..=PROTOCOL;

/// The oldest daemon this app shows, of those that report no protocol
/// (#317): 0.19.0, the titlebar's release. The window's tabs, its buttons
/// and dragging it come from the daemon's page, so on an older daemon the
/// app can't be moved or closed the way it should. Daemons that report a
/// protocol are all newer than this.
const MIN_VERSION: &str = "0.19.0";

/// Where to get the app by hand, when it can't update itself (a .deb or
/// .rpm, a build without an updater key): the newest app release (#393).
const DOWNLOADS: &str = "https://github.com/arugula-salad/illogical/releases/tag/app-latest";

/// Which side is behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behind {
    /// The daemon speaks an older protocol than this app handles.
    Daemon,
    /// The daemon speaks a newer one than this app knows.
    App,
}

/// A daemon speaking `protocol` (`None`: it doesn't say) at `version`,
/// against the range an app supports.
pub fn judge(protocol: Option<u32>, version: &str, supported: &RangeInclusive<u32>) -> Option<Behind> {
    if below_floor(protocol, version) {
        return Some(Behind::Daemon);
    }
    let p = protocol.unwrap_or(PROTOCOL_BASELINE);
    if p < *supported.start() {
        Some(Behind::Daemon)
    } else if p > *supported.end() {
        Some(Behind::App)
    } else {
        None
    }
}

/// A daemon too old to report a protocol, and older than [`MIN_VERSION`].
/// A version that doesn't parse isn't held against it.
fn below_floor(protocol: Option<u32>, version: &str) -> bool {
    protocol.is_none() && matches!((numbers(version), numbers(MIN_VERSION)), (Some(v), Some(min)) if v < min)
}

/// `X.Y.Z` (a leading `v`, a pre-release or build suffix ignored).
fn numbers(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|n| n.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next().unwrap_or(Some(0))?))
}

/// The daemon's protocol, from what `GET /api/host` answered.
pub fn protocol_of(host: &Value) -> Option<u32> {
    host["protocol"].as_u64().and_then(|p| u32::try_from(p).ok())
}

/// What the last check found wrong.
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub behind: Behind,
    /// The daemon's version, and its protocol if it reports one.
    pub version: String,
    pub protocol: Option<u32>,
}

impl Mismatch {
    pub fn message(&self) -> String {
        let (v, p) = (&self.version, self.protocol.unwrap_or(PROTOCOL_BASELINE));
        match self.behind {
            Behind::Daemon if below_floor(self.protocol, v) => format!(
                "arugulad here is {v}; this app needs {MIN_VERSION} or newer. The window's titlebar \
                 (its tabs, its buttons, dragging it) comes from the daemon's page, and {v}'s doesn't \
                 have it. Update arugulad to use this app."
            ),
            Behind::Daemon => format!(
                "arugulad here is {v}, which speaks protocol {p}; this app needs {} or newer. \
                 Update arugulad to use this app.",
                SUPPORTED.start()
            ),
            Behind::App => format!(
                "arugulad here is {v}, which speaks protocol {p}; this app knows up to {}. \
                 Update the app to use this daemon.",
                SUPPORTED.end()
            ),
        }
    }
}

static MISMATCH: Mutex<Option<Mismatch>> = Mutex::new(None);

/// Ask the daemon, and remember whether it's out of range. One that
/// doesn't answer isn't judged: the setup page starts it, then checks.
pub fn check() {
    let Some(host) = host() else {
        *MISMATCH.lock().unwrap() = None;
        return;
    };
    let protocol = protocol_of(&host);
    let version = host["version"].as_str().unwrap_or("unknown").to_owned();
    let found = judge(protocol, &version, &SUPPORTED).map(|behind| Mismatch { behind, version, protocol });
    if let Some(m) = &found {
        eprintln!("arugula: {}", m.message());
    }
    *MISMATCH.lock().unwrap() = found;
}

/// What the last check found, if the daemon and the app don't match.
pub fn mismatch() -> Option<Mismatch> {
    MISMATCH.lock().unwrap().clone()
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build().into()
}

/// `GET` from the local daemon, with the local token.
fn get(path: &str) -> Option<Value> {
    let mut req = agent(Duration::from_secs(3)).get(&format!("{}{path}", crate::page()));
    if let Some(b) = crate::bearer() {
        req = req.header("Authorization", &b);
    }
    let mut resp = req.call().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.body_mut().read_json().ok()
}

fn host() -> Option<Value> {
    get("/api/host")
}

/// How the daemon here updates, from `GET /api/update` (#391): it can
/// update itself (`apply`), or the command that updates it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonUpdate {
    Apply,
    Command(String),
    /// Neither: a daemon from before `GET /api/update` (0.18), or
    /// installed some way with no command (a build, the app's bundle).
    ByHand,
}

const INSTALL_SH: &str = "curl -fsSL https://illogical.widgets.wtf/install.sh | sh";
const INSTALL_PS1: &str = "irm https://illogical.widgets.wtf/install.ps1 | iex";
const BREW: &str = "brew upgrade arugula && arugulad install";

/// The command for a daemon that doesn't give one, from where its binary
/// is installed (the copy the app would start, `installed()`): Homebrew's
/// (a link into a `Cellar`), else install.sh again, as the daemon's own
/// `update.rs` decides.
fn command_for(installed: Option<&Path>) -> &'static str {
    match installed {
        Some(p) if p.components().any(|c| c.as_os_str() == "Cellar") => BREW,
        _ if cfg!(windows) => INSTALL_PS1,
        _ => INSTALL_SH,
    }
}

fn by_hand() -> &'static str {
    let installed = crate::installed(&crate::DAEMON).map(|p| p.canonicalize().unwrap_or(p));
    command_for(installed.as_deref())
}

pub fn daemon_update_of(update: &Value) -> DaemonUpdate {
    if update["apply"].as_bool() == Some(true) {
        DaemonUpdate::Apply
    } else if let Some(c) = update["command"].as_str().filter(|c| !c.is_empty()) {
        DaemonUpdate::Command(c.to_owned())
    } else {
        DaemonUpdate::ByHand
    }
}

fn daemon_update() -> DaemonUpdate {
    get("/api/update").map_or(DaemonUpdate::ByHand, |u| daemon_update_of(&u))
}

/// What the setup page shows: why, and what its button does (none when
/// the app can't fix it from here).
#[derive(serde::Serialize)]
pub struct Problem {
    message: String,
    action: Option<&'static str>,
}

#[tauri::command]
pub async fn compat(app: AppHandle) -> Result<Option<Problem>, String> {
    let Some(m) = mismatch() else { return Ok(None) };
    let mut message = m.message();
    let action = match m.behind {
        Behind::Daemon => {
            match tauri::async_runtime::spawn_blocking(daemon_update).await.map_err(|e| e.to_string())? {
                DaemonUpdate::Apply => {
                    message.push_str(" Or run:\n\n  arugulad update");
                    Some("Update arugulad")
                }
                DaemonUpdate::Command(c) => {
                    message.push_str(&format!(" To update it, run:\n\n  {c}"));
                    None
                }
                DaemonUpdate::ByHand => {
                    let c = by_hand();
                    message.push_str(&format!(" To update it the way it was installed, run:\n\n  {c}"));
                    None
                }
            }
        }
        Behind::App if app_updates(&app) => Some("Update the app"),
        Behind::App => Some("Download the app"),
    };
    Ok(Some(Problem { message, action }))
}

/// The setup page's button: update whichever side is behind.
#[tauri::command]
pub async fn compat_fix(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    let Some(m) = mismatch() else { return Ok(()) };
    match m.behind {
        Behind::Daemon => {
            tauri::async_runtime::spawn_blocking(update_daemon).await.map_err(|e| e.to_string())??;
            let to =
                tauri::async_runtime::spawn_blocking(move || crate::home(&app)).await.map_err(|e| e.to_string())?;
            window.navigate(to).map_err(|e| e.to_string())
        }
        Behind::App if app_updates(&app) => match crate::updates::fetch(&app, true).await? {
            Some(_) => app.restart(),
            None => Err(format!("No newer app is out yet. Look for one at {DOWNLOADS}.")),
        },
        Behind::App => {
            crate::open_outside(&DOWNLOADS.parse().map_err(|e| format!("{e}"))?);
            Ok(())
        }
    }
}

/// This app can update itself.
fn app_updates(app: &AppHandle) -> bool {
    crate::updates::can_update() && crate::updates::configured(app.config())
}

/// The daemon updates itself (#391's `POST /api/update/apply`, what the
/// web client's Update now does), and the app waits for it to come back
/// speaking a protocol in range. The app has no daemon update of its own
/// for this (#392).
fn update_daemon() -> Result<(), String> {
    let mut req = agent(Duration::from_secs(30)).post(&format!("{}/api/update/apply", crate::page()));
    if let Some(b) = crate::bearer() {
        req = req.header("Authorization", &b);
    }
    let mut resp = req.send_empty().map_err(|e| format!("asking arugulad to update: {e}"))?;
    if !resp.status().is_success() {
        let why = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(format!("arugulad didn't update: {}", why.trim()));
    }
    // It downloads, checks and installs the release, then restarts.
    for _ in 0..300 {
        std::thread::sleep(Duration::from_secs(1));
        if crate::reachable() && host().is_some() {
            check();
            if mismatch().is_none() {
                return Ok(());
            }
        }
    }
    check();
    match mismatch() {
        None => Ok(()),
        Some(m) => Err(format!("arugulad was asked to update, but five minutes on: {}", m.message())),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Behind, DaemonUpdate, Mismatch, command_for, daemon_update_of, judge, protocol_of};

    #[test]
    fn judges_both_directions() {
        let app = 2..=3;
        assert_eq!(judge(Some(2), "0.30.0", &app), None);
        assert_eq!(judge(Some(3), "0.30.0", &app), None);
        // The daemon is behind: it gets the update.
        assert_eq!(judge(Some(1), "0.30.0", &app), Some(Behind::Daemon));
        // The app is behind: it gets the update.
        assert_eq!(judge(Some(4), "0.30.0", &app), Some(Behind::App));
    }

    #[test]
    fn a_daemon_without_a_number_is_the_baseline() {
        assert_eq!(judge(None, "0.23.0", &(1..=1)), None);
        assert_eq!(judge(None, "0.23.0", &(1..=4)), None);
        assert_eq!(judge(None, "0.23.0", &(2..=4)), Some(Behind::Daemon));
    }

    #[test]
    fn this_app_takes_its_own_protocol_and_the_baseline() {
        assert_eq!(judge(Some(arugula_proto::PROTOCOL), "0.24.0", &super::SUPPORTED), None);
        assert_eq!(judge(None, "0.23.0", &super::SUPPORTED), None, "today's apps work with 0.23's daemons");
        assert_eq!(judge(Some(arugula_proto::PROTOCOL + 1), "0.30.0", &super::SUPPORTED), Some(Behind::App));
    }

    /// #317: the baseline protocol, but older than the titlebar's release.
    #[test]
    fn a_daemon_without_a_number_below_the_floor_is_behind() {
        let app = &super::SUPPORTED;
        assert_eq!(judge(None, "0.8.0", app), Some(Behind::Daemon), "the 0.8.0 that #317 saw");
        assert_eq!(judge(None, "0.18.9", app), Some(Behind::Daemon));
        assert_eq!(judge(None, "v0.18.0", app), Some(Behind::Daemon));
        // At or above it, the baseline holds.
        assert_eq!(judge(None, "0.19.0", app), None);
        assert_eq!(judge(None, "0.19.0-rc.1", app), None);
        assert_eq!(judge(None, "0.23.0", app), None);
        assert_eq!(judge(None, "1.0.0", app), None);
        // A version that doesn't parse isn't held against it.
        assert_eq!(judge(None, "unknown", app), None);
        // A daemon that reports a protocol is judged by that alone.
        assert_eq!(judge(Some(arugula_proto::PROTOCOL), "0.8.0", app), None);
    }

    #[test]
    fn says_which_version_runs_and_which_the_app_needs() {
        let m = Mismatch { behind: Behind::Daemon, version: "0.8.0".into(), protocol: None };
        let said = m.message();
        assert!(said.contains("arugulad here is 0.8.0"), "{said}");
        assert!(said.contains("needs 0.19.0 or newer"), "{said}");
        assert!(said.contains("titlebar"), "{said}");
        // Behind on the protocol: says so, as before.
        let m = Mismatch { behind: Behind::Daemon, version: "0.30.0".into(), protocol: Some(1) };
        assert!(m.message().contains("speaks protocol 1"), "{}", m.message());
    }

    #[test]
    fn the_command_for_how_it_was_installed() {
        let brew = Path::new("/opt/homebrew/Cellar/arugula/0.8.0/bin/arugulad");
        assert_eq!(command_for(Some(brew)), "brew upgrade arugula && arugulad install");
        let script = Path::new("/home/me/.local/bin/arugulad");
        let sh = if cfg!(windows) { super::INSTALL_PS1 } else { super::INSTALL_SH };
        assert_eq!(command_for(Some(script)), sh);
        assert_eq!(command_for(None), sh);
    }

    #[test]
    fn reads_how_the_daemon_updates() {
        let v = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(daemon_update_of(&v(r#"{"apply":true,"command":"curl … | sh"}"#)), DaemonUpdate::Apply);
        assert_eq!(
            daemon_update_of(&v(r#"{"apply":false,"command":"brew upgrade arugula && arugulad install"}"#)),
            DaemonUpdate::Command("brew upgrade arugula && arugulad install".into())
        );
        // A daemon from before #391: no `apply`.
        assert_eq!(
            daemon_update_of(&v(r#"{"current":"0.23.0","command":"curl … | sh"}"#)),
            DaemonUpdate::Command("curl … | sh".into())
        );
        assert_eq!(daemon_update_of(&v(r#"{"current":"0.23.0","command":null}"#)), DaemonUpdate::ByHand);
    }

    #[test]
    fn reads_the_number_from_api_host() {
        let host = |v: &str| serde_json::from_str::<serde_json::Value>(v).unwrap();
        assert_eq!(protocol_of(&host(r#"{"name":"a","version":"0.24.0","protocol":2}"#)), Some(2));
        assert_eq!(protocol_of(&host(r#"{"name":"a","version":"0.23.0"}"#)), None);
        assert_eq!(protocol_of(&host(r#"{"protocol":"2"}"#)), None);
        // What the daemon sends, through proto's own type.
        let info = arugula_proto::hosts::HostInfo {
            name: "a".into(),
            version: "0.24.0".into(),
            protocol: Some(arugula_proto::PROTOCOL),
            tailnet_url: None,
            tailnet_seen: false,
            control: None,
            team: None,
            fountain_runner: None,
            features: None,
        };
        assert_eq!(protocol_of(&serde_json::to_value(&info).unwrap()), Some(arugula_proto::PROTOCOL));
    }
}
