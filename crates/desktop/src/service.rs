//! macOS: the daemon as the app's own launch agent (M46).
//!
//! The bundle carries a launch agent
//! (`Contents/Library/LaunchAgents/wtf.widgets.illogical.daemon.plist`)
//! that runs the bundled `illogicald`. On a Mac with no daemon yet, the app
//! registers it through SMAppService, so it shows under Login Items as
//! illogical's, starts at login, and runs the copy inside the app: an app
//! update brings a new daemon with it, and the app restarts the agent
//! (`restart`) when the one running is older. Only from
//! /Applications/illogical.app (`usable`): an app run from anywhere else
//! installs the daemon with `illogicald install` instead.
//!
//! A daemon that `illogicald install` (install.sh, Homebrew, an older
//! app) set up keeps its own plist in `~/Library/LaunchAgents` (or
//! `/Library/LaunchDaemons` with `--system`): the app adopts that one and
//! never registers a second.

use std::process::Command;

use objc2_foundation::NSString;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

pub const PLIST: &str = "wtf.widgets.illogical.daemon.plist";
pub const LABEL: &str = "wtf.widgets.illogical.daemon";
/// The agent's `Program` (see the plist for why it's a fixed path).
pub const PROGRAM: &str = "/Applications/illogical.app/Contents/MacOS/illogicald";

/// This app is the one the agent runs: it's in /Applications.
pub fn usable() -> bool {
    crate::bundled("illogicald").is_some_and(|p| p == std::path::Path::new(PROGRAM))
}

fn agent() -> objc2::rc::Retained<SMAppService> {
    unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(PLIST)) }
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
            Err("macOS wants you to allow illogical in System Settings > General > Login Items before its daemon runs."
                .into())
        }
        _ => Err(format!("SMAppService says the daemon's agent is {}", status())),
    }
}

pub fn unregister() -> Result<(), String> {
    unsafe { agent().unregisterAndReturnError() }.map_err(|e| e.localizedDescription().to_string())
}

/// Restart the running agent on the bundle's (newer) daemon. Panes keep
/// running: the agent sets `ILLOGICAL_KEEP_PANES`, as `illogicald
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

/// A plist from `illogicald install` is there: that install owns the daemon.
/// Its launch agent (label `illogicald`, what install.sh sets up), or the
/// LaunchDaemon `illogicald install --system` wrote, which a later
/// `illogicald install` (install.sh run again) keeps.
pub fn installed_by_script() -> bool {
    let agent = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .is_some_and(|h| h.join("Library/LaunchAgents/illogicald.plist").is_file());
    let daemon = std::env::var("USER")
        .is_ok_and(|u| std::path::Path::new(&format!("/Library/LaunchDaemons/illogicald.{u}.plist")).is_file());
    agent || daemon
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}
