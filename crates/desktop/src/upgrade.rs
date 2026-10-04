//! A newer app replaces an older daemon (#176).
//!
//! The app carries an `illogicald`. When the daemon answering here is an
//! older version and runs as the service `illogicald install` set up, the
//! app runs its own copy's `install`: that copies it to `~/.local/bin`,
//! keeps the flags the last install wrote (in the systemd unit or launchd
//! plist), and restarts the service. Panes keep running across the restart
//! (the systemd FD store on Linux, pane shims holding terminals on macOS).
//!
//! Left alone: a daemon that isn't the service (one run by hand, a
//! development build), a daemon picked with `ILLOGICAL_URL`, and anything
//! when `ILLOGICAL_NO_DAEMON_UPGRADE` is set.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};

/// The versions, when this app is to replace the daemon: (running, ours).
static STALE: Mutex<Option<(String, String)>> = Mutex::new(None);

/// Look once, at launch: is the daemon older than ours?
pub fn check() {
    if std::env::var_os("ILLOGICAL_URL").is_some() || std::env::var_os("ILLOGICAL_NO_DAEMON_UPGRADE").is_some() {
        return;
    }
    let Some(ours) = crate::bundled("illogicald").as_deref().and_then(version_of) else { return };
    if !service_running() {
        return;
    }
    let Some(running) = running_version() else { return };
    if newer(&ours, &running) {
        eprintln!("illogical: the daemon is {running}, this app carries {ours}: updating it");
        *STALE.lock().unwrap() = Some((running, ours));
    }
}

/// What the setup page says while it updates.
pub fn pending() -> Option<String> {
    STALE.lock().unwrap().as_ref().map(|(running, ours)| {
        format!("Updating illogicald from {running} to {ours}. Your panes keep running while it restarts…")
    })
}

/// Run the carried `illogicald install`, and wait for the new one to answer.
pub fn run() -> Result<(), String> {
    let Some((running, ours)) = STALE.lock().unwrap().clone() else { return Ok(()) };
    let bin = crate::bundled("illogicald").ok_or("this app's illogicald is gone")?;
    let out = Command::new(&bin).arg("install").output().map_err(|e| format!("{}: {e}", bin.display()))?;
    if let Some(home) = std::env::var_os("HOME") {
        crate::unquarantine(&PathBuf::from(home).join(".local/bin/illogicald"));
    }
    for _ in 0..80 {
        if crate::reachable() && running_version().as_deref() == Some(ours.as_str()) {
            *STALE.lock().unwrap() = None;
            eprintln!("illogical: updated the daemon from {running} to {ours}");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    // Don't try again on every window: say why, once.
    *STALE.lock().unwrap() = None;
    Err(format!(
        "Ran {} install to update illogicald from {running} to {ours}, but {ours} doesn't answer at {}.\n{}{}",
        bin.display(),
        crate::addr(),
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// What the daemon here says it is (`/api/host`), else the installed
/// binary's `--version`.
fn running_version() -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3))).build().into();
    let asked = agent
        .get(&format!("{}/api/host", crate::page()))
        .call()
        .ok()
        .and_then(|mut r| r.body_mut().read_json::<serde_json::Value>().ok())
        .and_then(|v| v["version"].as_str().map(str::to_owned));
    asked.or_else(|| version_of(&crate::installed("illogicald")?))
}

/// `illogicald --version` says `illogicald 0.16.0`.
fn version_of(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().nth(1).map(str::to_owned)
}

/// The service `illogicald install` set up is what's running.
fn service_running() -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return false };
    if cfg!(target_os = "macos") {
        if !home.join("Library/LaunchAgents/illogicald.plist").is_file() {
            return false;
        }
        let uid = Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        let Ok(uid) = uid else { return false };
        Command::new("launchctl")
            .args(["print", &format!("gui/{uid}/illogicald")])
            .output()
            .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("state = running"))
    } else {
        home.join(".config/systemd/user/illogicald.service").is_file()
            && Command::new("systemctl")
                .args(["--user", "is-active", "--quiet", "illogicald.service"])
                .status()
                .is_ok_and(|s| s.success())
    }
}

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
    let v = (it.next()??, it.next()??, it.next().unwrap_or(Some(0))?);
    it.next().is_none().then_some(v)
}

/// `a` is a later release than `b`.
fn newer(a: &str, b: &str) -> bool {
    matches!((parse(a), parse(b)), (Some(a), Some(b)) if a > b)
}

#[cfg(test)]
mod tests {
    #[test]
    fn compares_versions() {
        assert!(super::newer("0.17.0", "0.16.0"));
        assert!(super::newer("0.16.10", "0.16.9"));
        assert!(!super::newer("0.16.0", "0.16.0"));
        assert!(!super::newer("0.16.0", "0.17.0"));
        assert!(!super::newer("0.17.0", "junk"));
    }
}
