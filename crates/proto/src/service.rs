//! Which service runs this machine's daemon (#322), for the desktop app's
//! *Daemon* menu and `arugula status`: the app's own launch agent, the
//! launch agent or LaunchDaemon `arugulad install` wrote, its systemd
//! user unit, its scheduled task on Windows, or none.
//!
//! Unlike the rest of this crate it looks at the machine (the service
//! files, `launchctl print`, `systemctl --user`), so the app and the CLI
//! say the same. Starting and stopping is the app's (`crates/desktop`).

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// The desktop app's bundle identifier (`tauri.conf.json`), and its
/// launch agent (macOS, `crates/desktop/src/service.rs`), named after it as
/// is the agent's plist in the bundle (`crates/desktop/macos`), whose
/// names a test checks against these.
pub const APP_ID: &str = "io.arugula.desktop";
pub const APP_LABEL: &str = "io.arugula.desktop.daemon";
/// The app: `Arugula.app`, or `illogical.app` (#505), which is where an app
/// from 0.25 or before still is after it updates: Tauri's updater
/// replaces the bundle in place, keeping its name.
pub const APP_BUNDLES: [&str; 2] = ["/Applications/Arugula.app", "/Applications/illogical.app"];
/// Its plist inside the app, which is where it runs the daemon from.
pub const APP_PLIST: &str = "/Applications/Arugula.app/Contents/Library/LaunchAgents/io.arugula.desktop.daemon.plist";
pub const APP_PROGRAM: &str = "/Applications/Arugula.app/Contents/MacOS/arugulad";
/// The app before the rename (#505): its identifier, its launch agent, and
/// where that ran the daemon from. The new app stops that agent (it can't
/// unregister another bundle's) and registers its own: both would run on
/// one state directory.
pub const OLD_APP_ID: &str = "wtf.widgets.illogical";
pub const OLD_APP_LABEL: &str = "wtf.widgets.illogical.daemon";
pub const OLD_APP_PLIST: &str =
    "/Applications/illogical.app/Contents/Library/LaunchAgents/wtf.widgets.illogical.daemon.plist";
pub const OLD_APP_PROGRAM: &str = "/Applications/illogical.app/Contents/MacOS/illogicald";
/// `arugulad install`'s launchd label, and its systemd unit.
pub const LABEL: &str = "arugulad";
pub const UNIT: &str = "arugulad.service";
/// What illogical's install called them, which a machine not yet
/// reinstalled still has (#505).
pub const OLD_LABEL: &str = "illogicald";
pub const OLD_UNIT: &str = "illogicald.service";
/// Both, the new names first: a machine whose daemon illogical set up still
/// has its service found here (#505).
pub const LABELS: [&str; 2] = [LABEL, OLD_LABEL];
pub const UNITS: [&str; 2] = [UNIT, OLD_UNIT];

/// The app's plist and the daemon it carries, in the app that's in
/// /Applications under either name ([`APP_BUNDLES`]); [`APP_PLIST`] and
/// [`APP_PROGRAM`] when there is none.
pub fn app_paths() -> (PathBuf, PathBuf) {
    app_paths_in(Path::new("/"))
}

fn app_paths_in(root: &Path) -> (PathBuf, PathBuf) {
    let plist = |b: &Path| b.join(format!("Contents/Library/LaunchAgents/{APP_LABEL}.plist"));
    let at = |p: &str| root.join(p.trim_start_matches('/'));
    match APP_BUNDLES.iter().map(|b| at(b)).find(|b| plist(b).is_file()) {
        Some(bundle) => (plist(&bundle), bundle.join("Contents/MacOS/arugulad")),
        None => (at(APP_PLIST), at(APP_PROGRAM)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The app's launch agent, registered through SMAppService.
    AppAgent,
    /// `arugulad install`'s launch agent (`~/Library/LaunchAgents`).
    Agent,
    /// `arugulad install --system`'s LaunchDaemon, which runs as the
    /// user from boot (stopping and starting it needs an administrator).
    LaunchDaemon,
    /// `arugulad install`'s systemd user unit.
    Systemd,
    /// `arugulad install`'s scheduled task (Windows).
    Task,
}

/// A service set up to run the daemon here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub kind: Kind,
    /// What its manager calls it: a launchd service target
    /// (`gui/501/arugulad`), the unit or the task.
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
    /// (io.arugula.desktop.daemon)".
    pub fn name(&self) -> String {
        // The label, unit or task: the target's last part.
        let label = self.target.rsplit('/').next().unwrap_or(&self.target);
        match self.kind {
            Kind::AppAgent if label == OLD_APP_LABEL => format!("the old app's launch agent ({label})"),
            Kind::AppAgent => format!("the app's launch agent ({label})"),
            Kind::Agent => format!("arugulad install's launch agent ({label})"),
            Kind::LaunchDaemon => format!("arugulad install's LaunchDaemon ({})", self.target),
            Kind::Systemd => format!("the systemd user unit {label}"),
            Kind::Task => format!("the scheduled task {label}"),
        }
    }

    /// The daemon binary it runs: the plist's program, or the unit's
    /// `ExecStart` (`%h` is the home directory).
    pub fn program(&self) -> Option<PathBuf> {
        if self.kind == Kind::AppAgent {
            let old = self.target.rsplit('/').next() == Some(OLD_APP_LABEL);
            return Some(if old { OLD_APP_PROGRAM.into() } else { app_paths().1 });
        }
        program_in(&std::fs::read_to_string(&self.file).ok()?, home().as_deref())
    }
}

/// The program a plist or unit names.
pub fn program_in(text: &str, home: Option<&Path>) -> Option<PathBuf> {
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
/// older `arugulad install` agent both.
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
    // app would register (no `arugulad install` plist).
    let app = launchd(&format!("{gui}/{APP_LABEL}"));
    let file = |p: PathBuf| p.is_file().then_some(p);
    let (label, agent_plist) = LABELS
        .iter()
        .find_map(|l| Some((*l, file(home()?.join(format!("Library/LaunchAgents/{l}.plist")))?)))
        .map_or((LABEL, None), |(l, p)| (l, Some(p)));
    let user_name = std::env::var("USER").unwrap_or_default();
    let (system_label, daemon_plist) = LABELS
        .iter()
        .find_map(|l| Some((*l, file(format!("/Library/LaunchDaemons/{l}.{user_name}.plist").into())?)))
        .map_or((LABEL, PathBuf::new()), |(l, p)| (l, p));
    let scripted = agent_plist.as_ref().is_some_and(|p| p.is_file()) || daemon_plist.is_file();
    let app_plist = app_paths().0;
    if app.is_some() || (!scripted && app_plist.is_file()) {
        out.push(Service {
            kind: Kind::AppAgent,
            target: format!("{gui}/{APP_LABEL}"),
            file: app_plist,
            running: app == Some(true),
            loaded: app.is_some(),
        });
    }
    // The app's from before the rename (#505), until the new app stops it,
    // or while it's the only app here.
    let old = launchd(&format!("{gui}/{OLD_APP_LABEL}"));
    if old.is_some() || (out.is_empty() && !scripted && Path::new(OLD_APP_PLIST).is_file()) {
        out.push(Service {
            kind: Kind::AppAgent,
            target: format!("{gui}/{OLD_APP_LABEL}"),
            file: OLD_APP_PLIST.into(),
            running: old == Some(true),
            loaded: old.is_some(),
        });
    }
    if daemon_plist.is_file() {
        let target = format!("system/{system_label}.{user_name}");
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
            .map(|d| format!("{d}/{label}"))
            .map(|t| {
                let s = launchd(&t);
                (t, s)
            })
            .find(|(_, s)| s.is_some())
            .unwrap_or((format!("{gui}/{label}"), None));
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
    let Some(dir) = home().map(|h| h.join(".config/systemd/user")) else { return Vec::new() };
    let Some((name, unit)) = UNITS.iter().map(|u| (*u, dir.join(u))).find(|(_, f)| f.is_file()) else {
        return Vec::new();
    };
    let running = quiet(Command::new("systemctl").args(["--user", "is-active", "--quiet", name]));
    vec![Service { kind: Kind::Systemd, target: name.into(), file: unit, running, loaded: true }]
}

#[cfg(windows)]
fn candidates() -> Vec<Service> {
    LABELS
        .iter()
        .find_map(|l| {
            let out = Command::new("schtasks").args(["/Query", "/TN", l, "/FO", "LIST"]).output();
            let out = out.ok().filter(|o| o.status.success())?;
            let running = String::from_utf8_lossy(&out.stdout).contains("Running");
            Some(Service { kind: Kind::Task, target: (*l).into(), file: PathBuf::new(), running, loaded: true })
        })
        .into_iter()
        .collect()
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

/// The daemon's log: `~/Library/Logs/arugulad.log` on macOS (every
/// service there writes it), the journal on Linux, the state directory's
/// `arugulad.log` on Windows.
pub fn log() -> Option<Log> {
    if cfg!(target_os = "macos") {
        return home().map(|h| Log::File(h.join("Library/Logs/arugulad.log")));
    }
    if cfg!(windows) {
        return Some(Log::File(crate::dirs::default_state_dir()?.join("arugulad.log")));
    }
    Some(Log::Journal(format!("journalctl --user -u {} -u {} -e", LABELS[0], LABELS[1])))
}

/// How the daemon here is doing, in one line: what the app's *Daemon*
/// menu and `arugula status` say. `version` is what the daemon said of
/// itself (`None`: it didn't answer).
pub fn line(version: Option<&str>, service: Option<&Service>) -> String {
    let v = version.map(|v| format!("arugulad {v}, ")).unwrap_or_default();
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
        let target = match kind {
            Kind::Systemd => UNIT.to_owned(),
            Kind::AppAgent => format!("gui/501/{APP_LABEL}"),
            _ => "gui/501/arugulad".to_owned(),
        };
        Service { kind, target, file: PathBuf::new(), running, loaded: running }
    }

    #[test]
    fn the_line_says_version_state_and_service() {
        assert_eq!(
            line(Some("0.21.0"), Some(&svc(Kind::AppAgent, true))),
            "arugulad 0.21.0, running as the app's launch agent (io.arugula.desktop.daemon)"
        );
        let old = Service { target: format!("gui/501/{OLD_APP_LABEL}"), ..svc(Kind::AppAgent, true) };
        assert_eq!(
            line(Some("0.25.0"), Some(&old)),
            "arugulad 0.25.0, running as the old app's launch agent (wtf.widgets.illogical.daemon)"
        );
        assert_eq!(old.program(), Some(OLD_APP_PROGRAM.into()));
        assert_eq!(
            line(Some("0.21.0"), Some(&svc(Kind::Agent, false))),
            "arugulad 0.21.0, running, not as a service (arugulad install's launch agent (arugulad) is stopped)"
        );
        assert_eq!(line(Some("0.21.0"), None), "arugulad 0.21.0, running, not as a service");
        assert_eq!(line(None, Some(&svc(Kind::Systemd, false))), "Stopped (the systemd user unit arugulad.service)");
        // #505: a unit an older release set up is named as it is.
        let old = Service { target: UNITS[1].into(), ..svc(Kind::Systemd, false) };
        assert_eq!(line(None, Some(&old)), "Stopped (the systemd user unit illogicald.service)");
        assert_eq!(line(None, None), "Not running, and not set up as a service");
    }

    #[test]
    fn the_program_comes_from_the_plist_or_the_unit() {
        let unit = "[Service]\nType=notify\nExecStart=%h/.local/bin/arugulad --listen 0.0.0.0:7681\n";
        assert_eq!(program_in(unit, Some(Path::new("/home/me"))), Some("/home/me/.local/bin/arugulad".into()));
        let plist = "<key>Label</key>\n<string>arugulad</string>\n<key>ProgramArguments</key>\n<array>\n    \
                     <string>/Users/me/.local/bin/arugulad</string>\n    <string>--headless</string>\n</array>";
        assert_eq!(program_in(plist, None), Some("/Users/me/.local/bin/arugulad".into()));
        assert_eq!(program_in("nothing", None), None);
    }

    #[test]
    fn the_agent_is_named_after_the_app() {
        assert_eq!(APP_LABEL, format!("{APP_ID}.daemon"));
        assert_eq!(OLD_APP_LABEL, format!("{OLD_APP_ID}.daemon"));
        assert!(APP_PLIST.starts_with(APP_BUNDLES[0]) && APP_PLIST.ends_with(&format!("/{APP_LABEL}.plist")));
        assert!(APP_PROGRAM.starts_with(APP_BUNDLES[0]));
        assert!(
            OLD_APP_PLIST.starts_with(APP_BUNDLES[1]) && OLD_APP_PLIST.ends_with(&format!("/{OLD_APP_LABEL}.plist"))
        );
        assert!(OLD_APP_PROGRAM.starts_with(APP_BUNDLES[1]));
    }

    /// #505: the app under its new name, or still under its old one after
    /// an update in place.
    #[test]
    fn the_app_is_found_under_either_name() {
        let root = std::env::temp_dir().join(format!("arugula-app-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let at = |p: &str| root.join(p.trim_start_matches('/'));
        let put = |p: PathBuf| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        };
        assert_eq!(app_paths_in(&root), (at(APP_PLIST), at(APP_PROGRAM)));
        // The app from before the rename isn't this one.
        put(at(OLD_APP_PLIST));
        assert_eq!(app_paths_in(&root), (at(APP_PLIST), at(APP_PROGRAM)));
        // Updated in place: the old bundle, the new app.
        let old = at(APP_BUNDLES[1]);
        let plist = old.join(format!("Contents/Library/LaunchAgents/{APP_LABEL}.plist"));
        put(plist.clone());
        assert_eq!(app_paths_in(&root), (plist, old.join("Contents/MacOS/arugulad")));
        // Arugula.app comes first.
        put(at(APP_PLIST));
        assert_eq!(app_paths_in(&root), (at(APP_PLIST), at(APP_PROGRAM)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
