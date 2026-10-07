//! macOS: the daemon as the app's own launch agent (M46).
//!
//! The bundle carries a launch agent
//! (`Contents/Library/LaunchAgents/io.arugula.desktop.daemon.plist`) that
//! runs the bundled `arugulad`. On a Mac with no daemon yet, the app
//! registers it through SMAppService, so it shows under Login Items as
//! Arugula's, starts at login, and runs the copy inside the app: an app
//! update brings a new daemon with it. Only from /Applications/Arugula.app
//! or /Applications/illogical.app (`usable`, #505: an app from before the
//! rename updates in place and keeps that name): an app run from anywhere
//! else installs the daemon with `arugulad install` instead.
//!
//! #505: the app before the rename had its own agent
//! (`wtf.widgets.illogical.daemon`). Both would run on one state
//! directory, so this app stops that one (`retire_old`) before it runs its
//! own; it can't unregister another bundle's agent, so it unloads and
//! disables it in launchd.
//!
//! The plist is the bundle's, so it carries no daemon flags: `arugulad
//! install -- FLAGS` keeps them in the state dir's `daemon-args.json`,
//! which the daemon reads when this agent starts it with none (#550).
//!
//! A daemon that `arugulad install` (install.sh, Homebrew, an older
//! app) set up keeps its own plist in `~/Library/LaunchAgents` (or
//! `/Library/LaunchDaemons` with `--system`): the app adopts that one and
//! never registers a second.

use std::process::Command;

use arugula_proto::service::{APP_BUNDLES, APP_LABEL, LABELS, OLD_APP_LABEL};
use objc2_foundation::NSString;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

pub const LABEL: &str = APP_LABEL;

/// This app is the one the agent runs: it's in /Applications, under
/// either name (see the plist for why those paths are fixed).
pub fn usable() -> bool {
    crate::bundled("arugulad").is_some_and(|p| runs(&p))
}

/// `exe` is a daemon the agent's plist runs.
fn runs(exe: &std::path::Path) -> bool {
    let exe = exe.to_string_lossy();
    // The default file system ignores case: /Applications/arugula.app is Arugula.app.
    APP_BUNDLES.iter().any(|b| exe.eq_ignore_ascii_case(&format!("{b}/Contents/MacOS/arugulad")))
}

fn agent() -> objc2::rc::Retained<SMAppService> {
    unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(&format!("{APP_LABEL}.plist"))) }
}

/// What SMAppService says of the agent.
pub fn status() -> &'static str {
    let s = unsafe { agent().status() };
    match s {
        SMAppServiceStatus::NotRegistered => "not registered",
        SMAppServiceStatus::Enabled => "enabled",
        SMAppServiceStatus::RequiresApproval => "requires approval",
        SMAppServiceStatus::NotFound => "not found",
        _ => "unknown",
    }
}

/// The agent is registered (and allowed to run).
pub fn registered() -> bool {
    unsafe { agent().status() == SMAppServiceStatus::Enabled }
}

/// Register the agent, which starts it.
pub fn register() -> Result<(), String> {
    let a = agent();
    unsafe { a.registerAndReturnError() }.map_err(|e| e.localizedDescription().to_string())?;
    match unsafe { a.status() } {
        SMAppServiceStatus::Enabled => Ok(()),
        SMAppServiceStatus::RequiresApproval => {
            Err("macOS wants you to allow Arugula in System Settings > General > Login Items before its daemon runs."
                .into())
        }
        _ => Err(format!("SMAppService says the daemon's agent is {}", status())),
    }
}

pub fn unregister() -> Result<(), String> {
    unsafe { agent().unregisterAndReturnError() }.map_err(|e| e.localizedDescription().to_string())
}

/// Restart the running agent on the bundle's (newer) daemon. Panes keep
/// running: the agent sets `ARUGULA_KEEP_PANES`, as `arugulad
/// install`'s plist does.
pub fn restart() -> Result<(), String> {
    let uid = unsafe { libc_getuid() };
    let out = Command::new("launchctl")
        .args(["kickstart", "-k", &format!("gui/{uid}/{LABEL}")])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("launchctl kickstart: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// #505: stop the launch agent of the app from before the rename, if
/// launchd has it: unload it (its daemon stops; panes keep running) and
/// disable it, so a login doesn't start it again beside this app's. True
/// when there was one.
pub fn retire_old() -> bool {
    let uid = unsafe { libc_getuid() };
    let target = format!("gui/{uid}/{OLD_APP_LABEL}");
    let loaded = Command::new("launchctl").args(["print", &target]).output().is_ok_and(|o| o.status.success());
    if !loaded {
        return false;
    }
    let _ = Command::new("launchctl").args(["bootout", &target]).output();
    let _ = Command::new("launchctl").args(["disable", &target]).output();
    eprintln!("arugula: stopped the old app's launch agent ({OLD_APP_LABEL}); this app runs the daemon from now on");
    true
}

/// A plist from `arugulad install` is there: that install owns the daemon.
/// Its launch agent (label `arugulad`, what install.sh sets up), or the
/// LaunchDaemon `arugulad install --system` wrote, which a later
/// `arugulad install` (install.sh run again) keeps.
pub fn installed_by_script() -> bool {
    // Under either name (#505).
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let user = std::env::var("USER").unwrap_or_default();
    LABELS.iter().any(|l| {
        home.as_ref().is_some_and(|h| h.join(format!("Library/LaunchAgents/{l}.plist")).is_file())
            || std::path::Path::new(&format!("/Library/LaunchDaemons/{l}.{user}.plist")).is_file()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_agent_runs_either_app() {
        assert!(super::runs("/Applications/Arugula.app/Contents/MacOS/arugulad".as_ref()));
        assert!(super::runs("/Applications/arugula.app/Contents/MacOS/arugulad".as_ref()));
        assert!(super::runs("/Applications/illogical.app/Contents/MacOS/arugulad".as_ref()));
        assert!(!super::runs("/Users/me/Applications/Arugula.app/Contents/MacOS/arugulad".as_ref()));
    }
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}
