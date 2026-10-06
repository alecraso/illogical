//! Does the daemon here speak a protocol this app works with (#390)?
//!
//! The daemon reports its protocol number (`illogical_proto::PROTOCOL`)
//! in `GET /api/host`; one too old to report it speaks
//! `PROTOCOL_BASELINE`. At launch, and again after the setup page starts
//! or updates the daemon, the app compares it with [`SUPPORTED`]. Outside
//! that range the window opens on the setup page, which says which side
//! is behind and offers its update: the daemon's own (#391, `POST
//! /api/update/apply`, else the command it gives) or the app's
//! (`updates.rs`).

use std::{ops::RangeInclusive, sync::Mutex, time::Duration};

use illogical_proto::{PROTOCOL, PROTOCOL_BASELINE};
use serde_json::Value;
use tauri::{AppHandle, WebviewWindow};

/// The oldest daemon protocol this app still handles. Raise it when the
/// app drops what an older daemon needs.
const MIN_PROTOCOL: u32 = 1;

/// The daemon protocols this app works with: up to the one its own
/// `proto` speaks.
pub const SUPPORTED: RangeInclusive<u32> = MIN_PROTOCOL..=PROTOCOL;

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

/// A daemon speaking `protocol` (`None`: it doesn't say), against the
/// range an app supports.
pub fn judge(protocol: Option<u32>, supported: &RangeInclusive<u32>) -> Option<Behind> {
    let p = protocol.unwrap_or(PROTOCOL_BASELINE);
    if p < *supported.start() {
        Some(Behind::Daemon)
    } else if p > *supported.end() {
        Some(Behind::App)
    } else {
        None
    }
}

/// The daemon's protocol, from what `GET /api/host` answered.
pub fn protocol_of(host: &Value) -> Option<u32> {
    host["protocol"].as_u64().and_then(|p| u32::try_from(p).ok())
}

/// What the last check found wrong.
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub behind: Behind,
    /// The daemon's version and protocol.
    pub version: String,
    pub protocol: u32,
}

impl Mismatch {
    pub fn message(&self) -> String {
        let (v, p) = (&self.version, self.protocol);
        match self.behind {
            Behind::Daemon => format!(
                "illogicald here is {v}, which speaks protocol {p}; this app needs {} or newer. \
                 Update illogicald to use this app.",
                SUPPORTED.start()
            ),
            Behind::App => format!(
                "illogicald here is {v}, which speaks protocol {p}; this app knows up to {}. \
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
    let found = judge(protocol, &SUPPORTED).map(|behind| Mismatch {
        behind,
        version: host["version"].as_str().unwrap_or("unknown").to_owned(),
        protocol: protocol.unwrap_or(PROTOCOL_BASELINE),
    });
    if let Some(m) = &found {
        eprintln!("illogical: {}", m.message());
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
    /// Neither: a daemon older than #391's `apply`, or installed some way
    /// with no command (a build, the app's bundle).
    ByHand,
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
                DaemonUpdate::Apply => Some("Update illogicald"),
                DaemonUpdate::Command(c) => {
                    message.push_str(&format!(" To update it, run:\n\n  {c}"));
                    None
                }
                DaemonUpdate::ByHand => {
                    message.push_str(&format!(" Update it the way it was installed ({DOWNLOADS})."));
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
    let mut resp = req.send_empty().map_err(|e| format!("asking illogicald to update: {e}"))?;
    if !resp.status().is_success() {
        let why = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(format!("illogicald didn't update: {}", why.trim()));
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
        Some(m) => Err(format!("illogicald was asked to update, but five minutes on: {}", m.message())),
    }
}

#[cfg(test)]
mod tests {
    use super::{Behind, DaemonUpdate, daemon_update_of, judge, protocol_of};

    #[test]
    fn judges_both_directions() {
        let app = 2..=3;
        assert_eq!(judge(Some(2), &app), None);
        assert_eq!(judge(Some(3), &app), None);
        // The daemon is behind: it gets the update.
        assert_eq!(judge(Some(1), &app), Some(Behind::Daemon));
        // The app is behind: it gets the update.
        assert_eq!(judge(Some(4), &app), Some(Behind::App));
    }

    #[test]
    fn a_daemon_without_a_number_is_the_baseline() {
        assert_eq!(judge(None, &(1..=1)), None);
        assert_eq!(judge(None, &(1..=4)), None);
        assert_eq!(judge(None, &(2..=4)), Some(Behind::Daemon));
    }

    #[test]
    fn this_app_takes_its_own_protocol_and_the_baseline() {
        assert_eq!(judge(Some(illogical_proto::PROTOCOL), &super::SUPPORTED), None);
        assert_eq!(judge(None, &super::SUPPORTED), None, "today's apps work with 0.23's daemons");
        assert_eq!(judge(Some(illogical_proto::PROTOCOL + 1), &super::SUPPORTED), Some(Behind::App));
    }

    #[test]
    fn reads_how_the_daemon_updates() {
        let v = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(daemon_update_of(&v(r#"{"apply":true,"command":"curl … | sh"}"#)), DaemonUpdate::Apply);
        assert_eq!(
            daemon_update_of(&v(r#"{"apply":false,"command":"brew upgrade illogical && illogicald install"}"#)),
            DaemonUpdate::Command("brew upgrade illogical && illogicald install".into())
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
        let info = illogical_proto::hosts::HostInfo {
            name: "a".into(),
            version: "0.24.0".into(),
            protocol: Some(illogical_proto::PROTOCOL),
            tailnet_url: None,
            tailnet_seen: false,
            control: None,
            team: None,
            fountain_runner: None,
            features: None,
        };
        assert_eq!(protocol_of(&serde_json::to_value(&info).unwrap()), Some(illogical_proto::PROTOCOL));
    }
}
