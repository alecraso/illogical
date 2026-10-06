//! Which service runs this machine's daemon (#322), for the desktop app's
//! *Daemon* menu and `illogical status`: the app's own launch agent, the
//! launch agent or LaunchDaemon `illogicald install` wrote, its systemd
//! user unit, its scheduled task on Windows, or none.
//!
//! Unlike the rest of this crate it looks at the machine (the service
//! files, `launchctl print`, `systemctl --user`), so the app and the CLI
//! say the same. Starting and stopping is the app's (`crates/desktop`).

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The app's launch agent (macOS, `crates/desktop/src/service.rs`).
pub const APP_LABEL: &str = "wtf.widgets.illogical.daemon";
/// Its plist inside the app, which is where it runs the daemon from.
pub const APP_PLIST: &str =
    "/Applications/illogical.app/Contents/Library/LaunchAgents/wtf.widgets.illogical.daemon.plist";
pub const APP_PROGRAM: &str = "/Applications/illogical.app/Contents/MacOS/illogicald";
/// `illogicald install`'s launchd label, and its systemd unit.
pub const LABEL: &str = "illogicald";
pub const UNIT: &str = "illogicald.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The app's launch agent, registered through SMAppService.
    AppAgent,
    /// `illogicald install`'s launch agent (`~/Library/LaunchAgents`).
    Agent,
    /// `illogicald install --system`'s LaunchDaemon, which runs as the
    /// user from boot (stopping and starting it needs an administrator).
    LaunchDaemon,
    /// `illogicald install`'s systemd user unit.
    Systemd,
    /// `illogicald install`'s scheduled task (Windows).
    Task,
}

/// A service set up to run the daemon here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub kind: Kind,
    /// What its manager calls it: a launchd service target
    /// (`gui/501/illogicald`), the unit or the task.
    pub target: String,
    /// Its plist or unit file.
    pub file: PathBuf,
    /// Its manager says it's running.
    pub running: bool,
    /// launchd has it loaded (a stopped agent is unloaded until it's
    /// started again or the next login). Always true elsewhere.
    pub loaded: bool,
}

impl Service {
    /// What runs it, in words: "the app's launch agent
    /// (wtf.widgets.illogical.daemon)".
    pub fn name(&self) -> String {
        match self.kind {
            Kind::AppAgent => format!("the app's launch agent ({APP_LABEL})"),
            Kind::Agent => format!("illogicald install's launch agent ({LABEL})"),
            Kind::LaunchDaemon => format!("illogicald install's LaunchDaemon ({})", self.target),
            Kind::Systemd => format!("the systemd user unit {UNIT}"),
            Kind::Task => format!("the scheduled task {LABEL}"),
        }
    }

    /// The daemon binary it runs: the plist's program, or the unit's
    /// `ExecStart` (`%h` is the home directory).
    pub fn program(&self) -> Option<PathBuf> {
        if self.kind == Kind::AppAgent {
            return Some(APP_PROGRAM.into());
        }
        program_in(&std::fs::read_to_string(&self.file).ok()?, home().as_deref())
    }
}

/// The program a plist or unit names.
fn program_in(text: &str, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(exec) = text.lines().find_map(|l| l.trim().strip_prefix("ExecStart=")) {
        let first = exec.split_whitespace().next()?;
        let h = home.map(|h| h.display().to_string()).unwrap_or_default();
        return Some(first.replace("%h", &h).into());
    }
    let key = ["<key>Program</key>", "<key>ProgramArguments</key>"].into_iter().find_map(|k| text.find(k))?;
    let rest = &text[key..];
    let s = &rest[rest.find("<string>")? + "<string>".len()..];
    let v = &s[..s.find("</string>")?];
    Some(v.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&").into())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The service set up to run the daemon here, running or not, if any. One
/// that's running comes first: a Mac can have the app's agent and an
/// older `illogicald install` agent both.
pub fn find() -> Option<Service> {
    let mut found = candidates();
    found.sort_by_key(|s| !s.running);
    found.into_iter().next()
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<Service> {
    let mut out = Vec::new();
    let Some(uid) = uid() else { return out };
    let gui = format!("gui/{uid}");
    let user = format!("user/{uid}");
    // The app's agent: loaded, or not (stopped) while it's the one the
    // app would register (no `illogicald install` plist).
    let app = launchd(&format!("{gui}/{APP_LABEL}"));
    let agent_plist = home().map(|h| h.join(format!("Library/LaunchAgents/{LABEL}.plist")));
    let user_name = std::env::var("USER").unwrap_or_default();
    let daemon_plist = PathBuf::from(format!("/Library/LaunchDaemons/{LABEL}.{user_name}.plist"));
    let scripted = agent_plist.as_ref().is_some_and(|p| p.is_file()) || daemon_plist.is_file();
    if app.is_some() || (!scripted && Path::new(APP_PLIST).is_file()) {
        out.push(Service {
            kind: Kind::AppAgent,
            target: format!("{gui}/{APP_LABEL}"),
            file: APP_PLIST.into(),
            running: app == Some(true),
            loaded: app.is_some(),
        });
    }
    if daemon_plist.is_file() {
        let target = format!("system/{LABEL}.{user_name}");
        let state = launchd(&target);
        out.push(Service {
            kind: Kind::LaunchDaemon,
            target,
            file: daemon_plist,
            running: state == Some(true),
            loaded: state.is_some(),
        });
    }
    if let Some(plist) = agent_plist.filter(|p| p.is_file()) {
        // Loaded in the GUI domain, or the background one (no GUI login).
        let (target, state) = [&gui, &user]
            .into_iter()
            .map(|d| format!("{d}/{LABEL}"))
            .map(|t| {
                let s = launchd(&t);
                (t, s)
            })
            .find(|(_, s)| s.is_some())
            .unwrap_or((format!("{gui}/{LABEL}"), None));
        out.push(Service {
            kind: Kind::Agent,
            target,
            file: plist,
            running: state == Some(true),
            loaded: state.is_some(),
        });
    }
    out
}

#[cfg(all(unix, not(target_os = "macos")))]
fn candidates() -> Vec<Service> {
    let Some(unit) = home().map(|h| h.join(".config/systemd/user").join(UNIT)).filter(|u| u.is_file()) else {
        return Vec::new();
    };
    let running = quiet(Command::new("systemctl").args(["--user", "is-active", "--quiet", UNIT]));
    vec![Service { kind: Kind::Systemd, target: UNIT.into(), file: unit, running, loaded: true }]
}

#[cfg(windows)]
fn candidates() -> Vec<Service> {
    let out = Command::new("schtasks").args(["/Query", "/TN", LABEL, "/FO", "LIST"]).output();
    let Some(out) = out.ok().filter(|o| o.status.success()) else { return Vec::new() };
    let running = String::from_utf8_lossy(&out.stdout).contains("Running");
    vec![Service { kind: Kind::Task, target: LABEL.into(), file: PathBuf::new(), running, loaded: true }]
}

/// This user's id, for launchd's domains.
pub fn uid() -> Option<u32> {
    let out = Command::new("id").arg("-u").output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// What launchd says of `target`: not loaded (`None`), loaded and running,
/// or loaded and not running.
pub fn launchd(target: &str) -> Option<bool> {
    let out = Command::new("launchctl").args(["print", target]).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).contains("state = running"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn quiet(cmd: &mut Command) -> bool {
    cmd.stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Where the daemon's log is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Log {
    File(PathBuf),
    /// systemd's journal: the command that shows it.
    Journal(String),
}

impl std::fmt::Display for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Log::File(p) => write!(f, "{}", p.display()),
            Log::Journal(cmd) => write!(f, "`{cmd}`"),
        }
    }
}

/// The daemon's log: `~/Library/Logs/illogicald.log` on macOS (every
/// service there writes it), the journal on Linux, the state directory's
/// `illogicald.log` on Windows.
pub fn log() -> Option<Log> {
    if cfg!(target_os = "macos") {
        return home().map(|h| Log::File(h.join("Library/Logs/illogicald.log")));
    }
    if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
        return Some(Log::File(local.join("illogical").join("state").join("illogicald.log")));
    }
    Some(Log::Journal(format!("journalctl --user -u {LABEL} -e")))
}

/// How the daemon here is doing, in one line: what the app's *Daemon*
/// menu and `illogical status` say. `version` is what the daemon said of
/// itself (`None`: it didn't answer).
pub fn line(version: Option<&str>, service: Option<&Service>) -> String {
    let v = version.map(|v| format!("illogicald {v}, ")).unwrap_or_default();
    match (version.is_some(), service) {
        (true, Some(s)) if s.running => format!("{v}running as {}", s.name()),
        (true, Some(s)) => format!("{v}running, not as a service ({} is stopped)", s.name()),
        (true, None) => format!("{v}running, not as a service"),
        (false, Some(s)) if s.running => format!("Not answering, though {} is running", s.name()),
        (false, Some(s)) => format!("Stopped ({})", s.name()),
        (false, None) => "Not running, and not set up as a service".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(kind: Kind, running: bool) -> Service {
        Service { kind, target: "gui/501/illogicald".into(), file: PathBuf::new(), running, loaded: running }
    }

    #[test]
    fn the_line_says_version_state_and_service() {
        assert_eq!(
            line(Some("0.21.0"), Some(&svc(Kind::AppAgent, true))),
            "illogicald 0.21.0, running as the app's launch agent (wtf.widgets.illogical.daemon)"
        );
        assert_eq!(
            line(Some("0.21.0"), Some(&svc(Kind::Agent, false))),
            "illogicald 0.21.0, running, not as a service (illogicald install's launch agent (illogicald) is stopped)"
        );
        assert_eq!(line(Some("0.21.0"), None), "illogicald 0.21.0, running, not as a service");
        assert_eq!(line(None, Some(&svc(Kind::Systemd, false))), "Stopped (the systemd user unit illogicald.service)");
        assert_eq!(line(None, None), "Not running, and not set up as a service");
    }

    #[test]
    fn the_program_comes_from_the_plist_or_the_unit() {
        let unit = "[Service]\nType=notify\nExecStart=%h/.local/bin/illogicald --listen 0.0.0.0:7681\n";
        assert_eq!(program_in(unit, Some(Path::new("/home/me"))), Some("/home/me/.local/bin/illogicald".into()));
        let plist = "<key>Label</key>\n<string>illogicald</string>\n<key>ProgramArguments</key>\n<array>\n    \
                     <string>/Users/me/.local/bin/illogicald</string>\n    <string>--headless</string>\n</array>";
        assert_eq!(program_in(plist, None), Some("/Users/me/.local/bin/illogicald".into()));
        assert_eq!(program_in("nothing", None), None);
    }
}
