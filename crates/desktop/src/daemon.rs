//! The local daemon in the app's menus and notifications.
//!
//! - **A *Daemon* submenu** (#322) in the tray, and on macOS in the app
//!   menu (beside *About*, with *This machine*) and the Dock icon's menu
//!   (with *New window* and *This machine*), since the tray icon is easy to
//!   miss:
//!   - a status line: the daemon's version, whether it's running and which
//!     service runs it (`illogical_proto::service`, which `illogical
//!     status` uses too);
//!   - when the daemon and this app don't speak a protocol in common
//!     (`compat.rs`, #390), which side is behind, opening the setup page
//!     that updates it; when a newer daemon is out (its own `GET
//!     /api/update`), its update through `POST /api/update/apply` (#391):
//!     the app never updates the daemon itself (#392);
//!   - this machine's standing with control (#325, `ControlState::line()`),
//!     with *Join…* or *Join again…*, which open the daemon's page at
//!     Getting started's cloud step;
//!   - *Restart*, *Stop…* and *Start*, through whichever service runs it,
//!     so a restart keeps the panes as any does (pane shims on macOS, the
//!     FD store on Linux); *Stop…* says first what stopping means;
//!   - *Open log*.
//! - **Dropped by control** (#325): one native notification per drop (a
//!   new `dropped_ms`, remembered across launches in `daemon.json` beside
//!   the app's settings). Its click opens Getting started's join.
//! - **Agents without their adapter** (#335): once a run, when a daemon
//!   speaking a protocol this app works with (`compat.rs`) first answers,
//!   the app asks it about the agents. Claude Code or Codex on this
//!   machine without its ACP adapter (or with one older than the daemon's
//!   pin) gets one notification per pin, remembered in `daemon.json` too,
//!   whose click opens Getting started's Agents step and its "Use Claude
//!   Code with illogical".
//!
//! A thread (`follow`) reads `/api/host`, `/api/update` and the service
//! every few seconds, and rebuilds the menus when what they'd say changes.

use std::{
    path::PathBuf,
    process::Command,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use illogical_proto::{
    hosts::{ControlState, HostInfo},
    service::{self, Kind, Service},
};
use tauri::{
    AppHandle, Manager, WebviewUrl, Wry,
    menu::{IsMenuItem, MenuItem, PredefinedMenuItem, Submenu},
};

use crate::compat::{self, Behind};

/// How often the menus look again.
const POLL: Duration = Duration::from_secs(5);

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().into())
}

/// `GET path` from the local daemon, with the local token; `None` when it
/// doesn't answer, or not with JSON.
fn get(path: &str) -> Option<serde_json::Value> {
    let mut req = agent().get(&format!("{}{path}", crate::page()));
    if let Some(b) = crate::bearer() {
        req = req.header("Authorization", &b);
    }
    req.call().ok()?.body_mut().read_json().ok()
}

/// `GET /api/host`, with the local token; `None` when it doesn't answer.
pub fn host() -> Option<serde_json::Value> {
    get("/api/host")
}

/// A newer daemon, from its own `GET /api/update` (#391).
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub latest: String,
    /// It can update itself (`POST /api/update/apply`).
    pub apply: bool,
    /// Else the command that updates it, if any.
    pub command: Option<String>,
}

fn update_of(v: &serde_json::Value) -> Option<Update> {
    if v["newer"].as_bool() != Some(true) {
        return None;
    }
    Some(Update {
        latest: v["latest"].as_str()?.to_owned(),
        apply: v["apply"].as_bool() == Some(true),
        command: v["command"].as_str().filter(|c| !c.is_empty()).map(str::to_owned),
    })
}

/// What the menus say: read by `read`.
#[derive(Clone, Default, PartialEq)]
pub struct Status {
    /// What the daemon said of itself; `None` when it didn't answer.
    pub host: Option<HostInfo>,
    /// Its standing with control (#325); `None` from an older daemon.
    pub control: Option<ControlState>,
    pub service: Option<Service>,
    /// Which side is behind, when the daemon and the app don't share a
    /// protocol (#390).
    pub behind: Option<Behind>,
    /// A newer daemon is out.
    pub update: Option<Update>,
}

/// The last `read`, to show what an action is doing over.
static LAST: Mutex<Option<Status>> = Mutex::new(None);
/// While an action runs: what it's doing ("Restarting…"), in place of the
/// status line.
static BUSY: Mutex<Option<String>> = Mutex::new(None);
/// The *Daemon* submenus (the tray's, the app menu's), and what they show.
static MENUS: Mutex<Vec<Submenu<Wry>>> = Mutex::new(Vec::new());
static SHOWN: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

pub fn read() -> Status {
    let v = crate::reachable().then(host).flatten();
    let info = v.as_ref().and_then(|v| serde_json::from_value::<HostInfo>(v.clone()).ok());
    let (control, behind, update) = match (&v, &info) {
        (Some(v), Some(info)) => (
            ControlState::of_host(v),
            compat::judge(compat::protocol_of(v), &info.version, &compat::SUPPORTED),
            get("/api/update").as_ref().and_then(update_of),
        ),
        _ => (None, None, None),
    };
    Status { host: info, control, service: service::find(), behind, update }
}

/// One line of the submenu; `id` "-" is a separator.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: &'static str,
    pub text: String,
    pub enabled: bool,
}

fn entry(id: &'static str, text: impl Into<String>, enabled: bool) -> Entry {
    Entry { id, text: text.into(), enabled }
}

fn separator() -> Entry {
    entry("-", "", false)
}

/// What the *Daemon* submenu says for `s`; `busy` in place of the status.
pub fn entries(s: &Status, busy: Option<&str>) -> Vec<Entry> {
    let version = s.host.as_ref().map(|h| h.version.as_str());
    let answering = version.is_some();
    let svc = s.service.as_ref();
    let mut out = vec![entry("daemon-status", busy.map_or_else(|| service::line(version, svc), str::to_owned), false)];
    match (s.behind, &s.update) {
        // The setup page says which side is behind and updates it
        // (compat.rs).
        (Some(Behind::Daemon), _) => out.push(entry("daemon-compat", "Too old for this app: Update…", busy.is_none())),
        (Some(Behind::App), _) => out.push(entry("daemon-compat", "Newer than this app knows: Update the app…", true)),
        (None, Some(u)) if u.apply => out.push(entry(
            "daemon-update",
            format!("Update to illogicald {} (panes keep running)", u.latest),
            busy.is_none(),
        )),
        (None, Some(u)) => out.push(entry(
            "daemon-update",
            match &u.command {
                Some(c) => format!("illogicald {} is out: run {c}", u.latest),
                None => format!("illogicald {} is out: update it the way it was installed", u.latest),
            },
            false,
        )),
        (None, None) => {}
    }
    if answering {
        out.push(separator());
        match &s.control {
            Some(c) => {
                out.push(entry("daemon-control", c.line(), false));
                if c.is_dropped() {
                    out.push(entry("daemon-join", "Join again…", true));
                } else if !c.is_joined() {
                    out.push(entry("daemon-join", "Join…", true));
                }
            }
            // An older daemon says only whether it's joined.
            None => match s.host.as_ref().and_then(|h| h.control.as_ref()) {
                Some(u) => out.push(entry("daemon-control", format!("Joined to {u}"), false)),
                None => {
                    out.push(entry("daemon-control", "Not joined to illogical control", false));
                    out.push(entry("daemon-join", "Join…", true));
                }
            },
        }
    }
    let running = svc.is_some_and(|s| s.running) && busy.is_none();
    out.push(separator());
    out.push(entry("daemon-restart", "Restart", running));
    out.push(entry("daemon-stop", "Stop…", running));
    out.push(entry("daemon-start", "Start", !answering && busy.is_none()));
    out.push(separator());
    out.push(entry("daemon-log", "Open log", service::log().is_some()));
    out
}

/// A *Daemon* submenu, kept current by `follow`.
pub fn submenu(app: &AppHandle) -> tauri::Result<Submenu<Wry>> {
    let sub = Submenu::new(app, "Daemon", true)?;
    fill(app, &sub, &last_entries())?;
    MENUS.lock().unwrap().push(sub.clone());
    Ok(sub)
}

fn fill(app: &AppHandle, sub: &Submenu<Wry>, entries: &[Entry]) -> tauri::Result<()> {
    while sub.remove_at(0)?.is_some() {}
    for e in entries {
        if e.id == "-" {
            sub.append(&PredefinedMenuItem::separator(app)?)?;
        } else {
            let item: MenuItem<Wry> = MenuItem::with_id(app, e.id, &e.text, e.enabled, None::<&str>)?;
            sub.append(&item as &dyn IsMenuItem<Wry>)?;
        }
    }
    Ok(())
}

/// Look again, and redo the menus if what they say changed.
pub fn refresh(app: &AppHandle) {
    let s = read();
    *LAST.lock().unwrap() = Some(s.clone());
    show(app, &s);
}

fn show(app: &AppHandle, s: &Status) {
    let busy = BUSY.lock().unwrap().clone();
    let now = entries(s, busy.as_deref());
    {
        let mut shown = SHOWN.lock().unwrap();
        if *shown == now {
            return;
        }
        shown.clone_from(&now);
    }
    let a = app.clone();
    let _ = app.run_on_main_thread(move || {
        for sub in MENUS.lock().unwrap().iter() {
            if let Err(e) = fill(&a, sub, &now) {
                eprintln!("illogical: the Daemon menu: {e}");
            }
        }
    });
}

/// What the menus show now ("Looking…" before the first read): the Dock's
/// menu is made from it each time it opens.
pub fn last_entries() -> Vec<Entry> {
    let shown = SHOWN.lock().unwrap().clone();
    if shown.is_empty() { entries(&Status::default(), Some("Looking…")) } else { shown }
}

/// The thread: the menus, a notification when control drops this
/// machine, and one about the agents.
pub fn follow(app: AppHandle) {
    let mut asked = false;
    loop {
        let s = read();
        *LAST.lock().unwrap() = Some(s.clone());
        show(&app, &s);
        if let Some(c) = &s.control {
            dropped(&app, c);
        }
        // Not a daemon this app doesn't work with (#390): the setup page
        // updates one side first, and the daemon that answers after is
        // asked.
        if !asked && s.host.is_some() && s.behind.is_none() {
            asked = true;
            let app = app.clone();
            // It runs `claude` and `node`: not in the menus' way.
            std::thread::spawn(move || agents(&app));
        }
        std::thread::sleep(POLL);
    }
}

/// Whether to notify of `c`: a drop whose time isn't `seen`, the last one
/// notified.
fn new_drop(c: &ControlState, seen: Option<u64>) -> Option<u64> {
    c.dropped_ms.filter(|at| c.is_dropped() && seen != Some(*at))
}

/// What the app remembers of the daemon across launches (`daemon.json`
/// beside its settings): the last drop it said, the adapters it said.
fn remembered(app: &AppHandle) -> (Option<PathBuf>, serde_json::Value) {
    let file = app.path().app_config_dir().ok().map(|d| d.join("daemon.json"));
    let v = file
        .as_ref()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    (file, v)
}

fn remember(file: Option<PathBuf>, mut v: serde_json::Value, key: &str, value: serde_json::Value) {
    let Some(f) = file else { return };
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    v[key] = value;
    let _ = std::fs::write(f, v.to_string());
}

/// One notification per drop: a `dropped_ms` not seen before, in this run
/// or an earlier one.
fn dropped(app: &AppHandle, c: &ControlState) {
    let (file, v) = remembered(app);
    let Some(at) = new_drop(c, v["dropped_ms"].as_u64()) else { return };
    remember(file, v, "dropped_ms", at.into());
    eprintln!("illogical: control dropped this machine: {}", c.line());
    let what = match &c.code {
        Some(code) => format!("{}. It asks to join again: approve {code} on a device you use.", c.line()),
        None => format!("{}. Click to join again.", c.line()),
    };
    crate::notify(app, crate::Click::JoinAgain, "This machine is no longer in illogical control".into(), what);
}

/// #335: the daemon's `/api/setup?part=agents`, and a notification for
/// an agent here whose adapter isn't (or is out of date), once per pin.
fn agents(app: &AppHandle) {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
    let mut req = agent.get(&format!("{}/api/setup?part=agents", crate::page()));
    if let Some(b) = crate::bearer() {
        req = req.header("Authorization", &b);
    }
    let Some(v) = req.call().ok().and_then(|mut r| r.body_mut().read_json::<serde_json::Value>().ok()) else { return };
    let (file, mem) = remembered(app);
    let seen: Vec<String> =
        mem["adapters_said"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_owned)).collect();
    let Some(n) = nudge(&v, &seen) else { return };
    let all: Vec<String> = seen.into_iter().chain(n.keys).collect();
    remember(file, mem, "adapters_said", all.into());
    eprintln!("illogical: {}", n.body);
    crate::notify(app, crate::Click::Agents, n.title, n.body);
}

/// What to say about the agents (#335), if anything not said before
/// (`seen`: `kind@pin` keys).
pub struct Nudge {
    pub keys: Vec<String>,
    pub title: String,
    pub body: String,
}

pub fn nudge(v: &serde_json::Value, seen: &[String]) -> Option<Nudge> {
    let mut keys = Vec::new();
    let mut labels = Vec::new();
    let mut outdated = false;
    for a in v["adapters"].as_array().into_iter().flatten() {
        let ready = a["state"] == "installed" && a["outdated"] != true;
        if a["found"] != true || ready {
            continue;
        }
        let key = format!("{}@{}", a["kind"].as_str().unwrap_or_default(), a["pinned"].as_str().unwrap_or_default());
        if seen.contains(&key) {
            continue;
        }
        outdated |= a["state"] == "installed";
        keys.push(key);
        labels.push(a["label"].as_str().unwrap_or("An agent").to_owned());
    }
    let first = labels.first()?.clone();
    let who = labels.join(" and ");
    let title = if outdated && labels.len() == 1 {
        format!("{first}'s adapter is out of date")
    } else {
        format!("Agent panes need {first}'s adapter")
    };
    let body = if outdated && labels.len() == 1 {
        format!("This illogical runs a newer {first} adapter than the one installed. Click to update it.")
    } else {
        format!("{who} is on this machine, but illogical can't run it in agent panes yet. Click to set it up.")
    };
    Some(Nudge { keys, title, body })
}

/// Getting started on the daemon's page, at `section` (`cloud`: to join
/// control, or join again, #325), in a window of ours. Not while the
/// daemon and the app don't match: the setup page says why.
pub fn getting_started(app: &AppHandle, section: &str) {
    if crate::compat::mismatch().is_some() {
        return crate::focus_or_open(app);
    }
    #[cfg(target_os = "macos")]
    let _ = app.show();
    let on_page = app.webview_windows().into_values().find(|w| w.url().is_ok_and(|u| crate::daemons(&u)));
    match on_page {
        // The client opens it (main.tsx) without a reload.
        Some(w) => {
            let _ = w.eval(format!(
                "dispatchEvent(new CustomEvent('illogical:getting-started', {{ detail: {section:?} }}))"
            ));
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
        None => {
            let _ =
                crate::open_window(app, WebviewUrl::External(crate::page_at(&format!("/#getting-started={section}"))));
        }
    }
}

/// An item of the *Daemon* submenu, the app menu's *This machine* or the
/// Dock's items; `false` when `id` isn't one. On the main thread.
///
/// Every menu's events reach the app's handler (and the tray's), so these
/// ids are the app menu's and the Dock's own, apart from the tray's `new`
/// and `this`.
pub fn menu_event(app: &AppHandle, id: &str) -> bool {
    match id {
        "dock-new" => {
            let _ = crate::open_window(app, crate::target(app));
        }
        "menu-this" | "dock-this" => crate::this_machine(app),
        "daemon-restart" => act(app, Action::Restart),
        "daemon-start" => act(app, Action::Start),
        "daemon-stop" => {
            if confirm_stop() {
                act(app, Action::Stop);
            }
        }
        "daemon-update" => act(app, Action::Update),
        // The setup page: which side is behind, and its update (compat.rs).
        "daemon-compat" => {
            compat::check();
            let _ = crate::open_window(app, crate::target(app));
        }
        "daemon-join" => getting_started(app, "cloud"),
        "daemon-log" => open_log(),
        "daemon-status" | "daemon-control" => {}
        _ => return false,
    }
    true
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Restart,
    Stop,
    Start,
    Update,
}

fn act(app: &AppHandle, what: Action) {
    let doing = match what {
        Action::Restart => "Restarting…",
        Action::Stop => "Stopping…",
        Action::Start => "Starting…",
        Action::Update => "Updating…",
    };
    {
        let mut busy = BUSY.lock().unwrap();
        if busy.is_some() {
            return;
        }
        *busy = Some(doing.into());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        if let Some(s) = LAST.lock().unwrap().clone() {
            show(&app, &s);
        }
        let r = run(what);
        *BUSY.lock().unwrap() = None;
        refresh(&app);
        if let Err(e) = r {
            eprintln!("illogical: the daemon: {e}");
            let title = format!("{} illogicald didn't work", doing.trim_end_matches('…'));
            let _ = app.run_on_main_thread(move || alert(&title, &e));
        }
    });
}

fn run(what: Action) -> Result<(), String> {
    let svc = service::find();
    match (what, svc) {
        (Action::Update, _) => update(),
        // Nothing set up: what a window does when it finds no daemon
        // (installs the bundled one, or starts the installed one).
        (Action::Start, None) => crate::ensure_daemon(),
        (Action::Stop | Action::Restart, None) => {
            Err("The daemon here isn't run by a service, so the app can't stop or restart it.".into())
        }
        (Action::Restart, Some(s)) if !s.loaded => start(&s).and_then(|()| wait(true)),
        (Action::Restart, Some(s)) => restart(&s).and_then(|()| wait(true)),
        (Action::Start, Some(s)) => start(&s).and_then(|()| wait(true)),
        (Action::Stop, Some(s)) => stop(&s).and_then(|()| wait(false)),
    }
}

/// The daemon updates itself (#391's `POST /api/update/apply`, what the
/// page's *Update now* does), and the app waits for the new one to answer.
fn update() -> Result<(), String> {
    let before = host().and_then(|h| h["version"].as_str().map(str::to_owned));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut req = agent.post(&format!("{}/api/update/apply", crate::page()));
    if let Some(b) = crate::bearer() {
        req = req.header("Authorization", &b);
    }
    let mut resp = req.send_empty().map_err(|e| format!("asking illogicald to update: {e}"))?;
    if !resp.status().is_success() {
        let why = resp.body_mut().read_to_string().unwrap_or_default();
        return Err(format!("illogicald didn't update: {}", why.trim()));
    }
    // It downloads, checks and installs the release, then restarts.
    let until = Instant::now() + Duration::from_secs(300);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_secs(1));
        let now = host().and_then(|h| h["version"].as_str().map(str::to_owned));
        if now.is_some() && now != before {
            compat::check();
            return Ok(());
        }
    }
    Err("illogicald didn't come back as a newer version in five minutes. The log may say why.".into())
}

/// Until the daemon answers (`up`) or stops answering.
fn wait(up: bool) -> Result<(), String> {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if crate::reachable() == up {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(if up {
        format!("Nothing answers at {} yet. The log may say why.", crate::addr())
    } else {
        format!("Something still answers at {}.", crate::addr())
    })
}

/// launchd: the domain of a service target (`gui/501` of `gui/501/illogicald`).
fn domain(target: &str) -> &str {
    target.rsplit_once('/').map_or(target, |(d, _)| d)
}

fn restart(s: &Service) -> Result<(), String> {
    match s.kind {
        Kind::AppAgent | Kind::Agent => launchctl(&["kickstart", "-k", &s.target]),
        Kind::LaunchDaemon => as_admin(&format!("launchctl kickstart -k {}", s.target)),
        Kind::Systemd => systemctl("restart", &s.target),
        Kind::Task => Err("restarting the scheduled task isn't something the app does".into()),
    }
}

/// Stopped until it's started again, or the next login (boot, for the
/// LaunchDaemon): launchd unloads it (`bootout`), which keeps it set up.
fn stop(s: &Service) -> Result<(), String> {
    match s.kind {
        Kind::AppAgent | Kind::Agent => launchctl(&["bootout", &s.target]),
        Kind::LaunchDaemon => as_admin(&format!("launchctl bootout {}", s.target)),
        Kind::Systemd => systemctl("stop", &s.target),
        Kind::Task => Err("stopping the scheduled task isn't something the app does".into()),
    }
}

fn start(s: &Service) -> Result<(), String> {
    let file = s.file.display().to_string();
    match s.kind {
        _ if s.loaded && s.kind != Kind::Systemd && s.kind != Kind::LaunchDaemon => {
            launchctl(&["kickstart", &s.target])
        }
        Kind::AppAgent => {
            // Stopped (unloaded): load it again. SMAppService keeps it
            // registered, so it's back at the next login anyway; if launchd
            // won't take the plist from here, registering again does it.
            let loaded = launchctl(&["bootstrap", domain(&s.target), &file]);
            #[cfg(target_os = "macos")]
            let loaded = loaded.or_else(|e| {
                let _ = crate::service::unregister();
                crate::service::register().map_err(|r| format!("{e}; registering again: {r}"))
            });
            loaded
        }
        Kind::Agent => {
            let _ = launchctl(&["enable", &s.target]);
            launchctl(&["bootstrap", domain(&s.target), &file])
        }
        Kind::LaunchDaemon if s.loaded => as_admin(&format!("launchctl kickstart {}", s.target)),
        Kind::LaunchDaemon => as_admin(&format!("launchctl bootstrap system {file}")),
        Kind::Systemd => systemctl("start", &s.target),
        Kind::Task => Err("starting the scheduled task isn't something the app does".into()),
    }
}

fn launchctl(args: &[&str]) -> Result<(), String> {
    output(Command::new("launchctl").args(args), &format!("launchctl {}", args.join(" ")))
}

fn systemctl(verb: &str, unit: &str) -> Result<(), String> {
    output(Command::new("systemctl").args(["--user", verb, unit]), &format!("systemctl --user {verb} {unit}"))
}

/// The LaunchDaemon's: macOS asks for an administrator's password.
fn as_admin(cmd: &str) -> Result<(), String> {
    let script = format!("do shell script \"{cmd}\" with administrator privileges");
    output(Command::new("osascript").args(["-e", &script]), cmd)
}

fn output(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    let said = String::from_utf8_lossy(&out.stdout);
    Err(format!("{what}: {}", if err.trim().is_empty() { said.trim() } else { err.trim() }))
}

/// What *Stop…* asks before it stops anything, on macOS and Linux (on
/// Windows the app doesn't stop the scheduled task).
#[cfg(not(windows))]
pub const STOP_TITLE: &str = "Stop illogicald?";
#[cfg(not(windows))]
pub const STOP_TEXT: &str = "Your panes end, and the phone and your other machines can't reach this one until \
                             the daemon is started again (Daemon > Start) or at your next login.";

/// The log: the file (Console opens it on macOS), or on Linux what the
/// journal has, written to a file that the text editor opens.
fn open_log() {
    let path = match service::log() {
        Some(service::Log::File(p)) => p,
        Some(service::Log::Journal(_)) => match journal() {
            Ok(p) => p,
            Err(e) => {
                alert("The daemon's log", &e);
                return;
            }
        },
        None => return,
    };
    match tauri::Url::from_file_path(&path) {
        Ok(u) => crate::open_outside(&u),
        Err(()) => eprintln!("illogical: not a file to open: {}", path.display()),
    }
}

fn journal() -> Result<PathBuf, String> {
    let out = Command::new("journalctl")
        .args(["--user", "-u", service::LABELS[0], "-u", service::LABELS[1], "-n", "5000", "--no-pager"])
        .output()
        .map_err(|e| format!("journalctl: {e}"))?;
    let file = std::env::temp_dir().join("illogicald-journal.log");
    std::fs::write(&file, &out.stdout).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

/// *Stop…*'s question, on the main thread: true to stop.
#[cfg(target_os = "macos")]
fn confirm_stop() -> bool {
    use objc2_app_kit::NSAlert;
    use objc2_foundation::{MainThreadMarker, NSString};
    let Some(mtm) = MainThreadMarker::new() else { return false };
    front(mtm);
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(STOP_TITLE));
    a.setInformativeText(&NSString::from_str(STOP_TEXT));
    a.addButtonWithTitle(&NSString::from_str("Stop"));
    a.addButtonWithTitle(&NSString::from_str("Cancel"));
    // NSAlertFirstButtonReturn
    a.runModal() == 1000
}

#[cfg(target_os = "linux")]
fn confirm_stop() -> bool {
    use gtk::prelude::*;
    let d = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Warning,
        gtk::ButtonsType::None,
        STOP_TITLE,
    );
    d.set_secondary_text(Some(STOP_TEXT));
    d.add_button("Cancel", gtk::ResponseType::Cancel);
    d.add_button("Stop", gtk::ResponseType::Accept);
    let r = d.run();
    d.close();
    r == gtk::ResponseType::Accept
}

#[cfg(windows)]
fn confirm_stop() -> bool {
    false
}

/// A click in the tray doesn't bring the app forward; its question must.
#[cfg(target_os = "macos")]
fn front(mtm: objc2_foundation::MainThreadMarker) {
    #[allow(deprecated)]
    objc2_app_kit::NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

/// What went wrong, on the main thread.
#[cfg(target_os = "macos")]
fn alert(title: &str, text: &str) {
    use objc2_app_kit::NSAlert;
    use objc2_foundation::{MainThreadMarker, NSString};
    let Some(mtm) = MainThreadMarker::new() else { return };
    front(mtm);
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(title));
    a.setInformativeText(&NSString::from_str(text));
    a.runModal();
}

#[cfg(target_os = "linux")]
fn alert(title: &str, text: &str) {
    use gtk::prelude::*;
    let d = gtk::MessageDialog::new(
        None::<&gtk::Window>,
        gtk::DialogFlags::MODAL,
        gtk::MessageType::Error,
        gtk::ButtonsType::Ok,
        title,
    );
    d.set_secondary_text(Some(text));
    d.run();
    d.close();
}

#[cfg(windows)]
fn alert(title: &str, text: &str) {
    eprintln!("illogical: {title}: {text}");
}

/// macOS: the Dock icon's menu (`applicationDockMenu:`), which Tauri has
/// no API for. The app's delegate (tao's) gets the method, and the menu
/// is made fresh each time it opens: *New window*, *This machine* and the
/// *Daemon* items, whose clicks come back through `illogicalDockItem:`.
#[cfg(target_os = "macos")]
pub mod dock {
    use std::sync::{Mutex, OnceLock};

    use objc2::{
        MainThreadOnly,
        ffi::class_addMethod,
        rc::Retained,
        runtime::{AnyClass, AnyObject, Imp, Sel},
        sel,
    };
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::{MainThreadMarker, NSString};
    use tauri::AppHandle;

    static APP: OnceLock<AppHandle> = OnceLock::new();
    /// A dock item's tag is its place here.
    static IDS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    /// On the main thread, once the app's delegate is set (in `setup`).
    pub fn init(app: &AppHandle) {
        let _ = APP.set(app.clone());
        let Some(mtm) = MainThreadMarker::new() else { return };
        let ns_app = NSApplication::sharedApplication(mtm);
        let Some(delegate) = ns_app.delegate() else { return };
        let obj: &AnyObject = delegate.as_ref();
        let cls = obj.class() as *const AnyClass as *mut AnyClass;
        // SAFETY: the delegate's own class, on the main thread; the
        // functions match the type encodings (`-(NSMenu *)m:(id)`,
        // `-(void)m:(id)`).
        unsafe {
            type Menu = unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> *mut NSMenu;
            type Item = unsafe extern "C-unwind" fn(&AnyObject, Sel, &NSMenuItem);
            let menu = std::mem::transmute::<Menu, Imp>(dock_menu);
            let item = std::mem::transmute::<Item, Imp>(clicked);
            class_addMethod(cls, sel!(applicationDockMenu:), menu, c"@@:@".as_ptr());
            class_addMethod(cls, sel!(illogicalDockItem:), item, c"v@:@".as_ptr());
        }
        // AppKit notes what a delegate answers when it's set.
        ns_app.setDelegate(Some(&delegate));
    }

    unsafe extern "C-unwind" fn dock_menu(this: &AnyObject, _: Sel, _: *mut AnyObject) -> *mut NSMenu {
        let Some(mtm) = MainThreadMarker::new() else { return std::ptr::null_mut() };
        let mut ids = IDS.lock().unwrap();
        ids.clear();
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        let mut add = |to: &NSMenu, id: &'static str, text: &str, enabled: bool| {
            if id == "-" {
                to.addItem(&NSMenuItem::separatorItem(mtm));
                return;
            }
            // SAFETY: a plain item whose target (the delegate) answers its
            // action.
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(text),
                    Some(sel!(illogicalDockItem:)),
                    &NSString::from_str(""),
                )
            };
            unsafe { item.setTarget(Some(this)) };
            item.setTag(ids.len() as isize);
            item.setEnabled(enabled);
            ids.push(id);
            to.addItem(&item);
        };
        add(&menu, "dock-new", "New window", true);
        add(&menu, "dock-this", "This machine", true);
        add(&menu, "-", "", false);
        let daemon = NSMenu::new(mtm);
        daemon.setAutoenablesItems(false);
        for e in super::last_entries() {
            add(&daemon, e.id, &e.text, e.enabled);
        }
        let parent = NSMenuItem::new(mtm);
        parent.setTitle(&NSString::from_str("Daemon"));
        parent.setSubmenu(Some(&daemon));
        menu.addItem(&parent);
        Retained::autorelease_return(menu)
    }

    unsafe extern "C-unwind" fn clicked(_: &AnyObject, _: Sel, sender: &NSMenuItem) {
        let Some(id) = IDS.lock().unwrap().get(sender.tag() as usize).copied() else { return };
        if let Some(app) = APP.get() {
            super::menu_event(app, id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use illogical_proto::{
        hosts::{ControlState, HostInfo},
        service::{Kind, Service},
    };

    use super::*;

    fn host() -> HostInfo {
        HostInfo {
            name: "jake-air".into(),
            version: "0.20.0".into(),
            protocol: None,
            tailnet_url: None,
            tailnet_seen: false,
            control: None,
            team: None,
            fountain_runner: None,
            features: None,
        }
    }

    fn agent(running: bool) -> Service {
        Service {
            kind: Kind::AppAgent,
            target: "gui/501/wtf.widgets.illogical.daemon".into(),
            file: PathBuf::new(),
            running,
            loaded: running,
        }
    }

    fn up(service: Option<Service>) -> Status {
        Status { host: Some(host()), service, ..Default::default() }
    }

    fn find<'a>(es: &'a [Entry], id: &str) -> Option<&'a Entry> {
        es.iter().find(|e| e.id == id)
    }

    #[test]
    fn the_menu_says_how_the_daemon_is_and_offers_what_fits() {
        let es = entries(&up(Some(agent(true))), None);
        assert_eq!(
            find(&es, "daemon-status").unwrap().text,
            "illogicald 0.20.0, running as the app's launch agent (wtf.widgets.illogical.daemon)"
        );
        assert!(find(&es, "daemon-update").is_none() && find(&es, "daemon-compat").is_none());
        assert!(find(&es, "daemon-restart").unwrap().enabled);
        assert!(find(&es, "daemon-stop").unwrap().enabled);
        assert!(!find(&es, "daemon-start").unwrap().enabled);
        assert!(find(&es, "daemon-log").is_some());

        // Stopped: Start, not Stop or Restart, and no control line.
        let s = Status { service: Some(agent(false)), ..Default::default() };
        let es = entries(&s, None);
        assert!(find(&es, "daemon-status").unwrap().text.starts_with("Stopped"));
        assert!(find(&es, "daemon-start").unwrap().enabled);
        assert!(!find(&es, "daemon-stop").unwrap().enabled);
        assert!(!find(&es, "daemon-restart").unwrap().enabled);
        assert!(find(&es, "daemon-control").is_none() && find(&es, "daemon-join").is_none());

        // Not a service: nothing to stop or restart it through.
        let es = entries(&up(None), None);
        assert!(find(&es, "daemon-status").unwrap().text.ends_with("not as a service"));
        assert!(!find(&es, "daemon-stop").unwrap().enabled);

        // While an action runs: what it's doing, and nothing else to click.
        let es = entries(&up(Some(agent(true))), Some("Restarting…"));
        assert_eq!(find(&es, "daemon-status").unwrap().text, "Restarting…");
        assert!(!find(&es, "daemon-restart").unwrap().enabled);
    }

    /// The app never updates the daemon (#392): it offers the daemon's own
    /// update, and the setup page when they don't share a protocol (#390).
    #[test]
    fn updates_are_the_daemons_own_or_the_setup_pages() {
        let newer = |apply: bool, command: Option<&str>| Status {
            update: Some(Update { latest: "0.25.0".into(), apply, command: command.map(str::to_owned) }),
            ..up(Some(agent(true)))
        };
        let es = entries(&newer(true, None), None);
        let u = find(&es, "daemon-update").unwrap();
        assert!(u.enabled && u.text == "Update to illogicald 0.25.0 (panes keep running)", "{u:?}");
        let es = entries(&newer(false, Some("brew upgrade illogical")), None);
        let u = find(&es, "daemon-update").unwrap();
        assert!(!u.enabled && u.text.ends_with("run brew upgrade illogical"), "{u:?}");

        let behind = |b| Status { behind: Some(b), ..newer(true, None) };
        let es = entries(&behind(Behind::Daemon), None);
        assert_eq!(find(&es, "daemon-compat").unwrap().text, "Too old for this app: Update…");
        assert!(find(&es, "daemon-update").is_none(), "one way to update, the setup page's");
        let es = entries(&behind(Behind::App), None);
        assert_eq!(find(&es, "daemon-compat").unwrap().text, "Newer than this app knows: Update the app…");

        let v = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert_eq!(update_of(&v(r#"{"current":"0.24.0","newer":false,"apply":true}"#)), None);
        assert_eq!(
            update_of(&v(r#"{"current":"0.24.0","latest":"0.25.0","newer":true,"apply":false,"command":""}"#)),
            Some(Update { latest: "0.25.0".into(), apply: false, command: None })
        );
    }

    #[test]
    fn the_control_line_offers_join_or_join_again() {
        let dropped = ControlState {
            state: "dropped".into(),
            url: Some("https://control.illogical.widgets.wtf".into()),
            kind: Some("team".into()),
            name: Some("arugula".into()),
            said: Some("not an enrolled daemon (left, or revoked?)".into()),
            dropped_ms: Some(1),
            ..Default::default()
        };
        let with = |c: ControlState| entries(&Status { control: Some(c), ..up(None) }, None);
        let es = with(dropped.clone());
        assert_eq!(find(&es, "daemon-control").unwrap().text, dropped.line());
        assert_eq!(find(&es, "daemon-join").unwrap().text, "Join again…");

        // Joined and connected: no join item.
        let joined = ControlState { state: "joined".into(), connected: true, said: None, dropped_ms: None, ..dropped };
        let es = with(joined);
        assert_eq!(
            find(&es, "daemon-control").unwrap().text,
            "In the team arugula on control.illogical.widgets.wtf: connected"
        );
        assert!(find(&es, "daemon-join").is_none());

        // Not joined: Join….
        let es = with(ControlState { state: "not_joined".into(), ..Default::default() });
        assert_eq!(find(&es, "daemon-join").unwrap().text, "Join…");

        // An older daemon: whether it's joined, from `control`.
        let es = entries(&up(None), None);
        assert_eq!(find(&es, "daemon-join").unwrap().text, "Join…");
    }

    #[test]
    fn one_notification_per_drop() {
        let dropped = ControlState { state: "dropped".into(), dropped_ms: Some(7), ..Default::default() };
        assert_eq!(new_drop(&dropped, None), Some(7));
        assert_eq!(new_drop(&dropped, Some(7)), None, "already said");
        assert_eq!(new_drop(&dropped, Some(3)), Some(7), "another drop");
        let joined = ControlState { state: "joined".into(), ..Default::default() };
        assert_eq!(new_drop(&joined, None), None);
    }

    #[test]
    fn says_an_agent_here_without_its_adapter_once_per_pin() {
        let v = serde_json::json!({ "adapters": [
            { "kind": "claude", "label": "Claude Code", "pinned": "0.85.0", "state": "missing", "found": true },
            { "kind": "codex", "label": "Codex", "pinned": "2.1.0", "state": "missing", "found": false },
        ] });
        let n = nudge(&v, &[]).unwrap();
        assert_eq!(n.keys, ["claude@0.85.0"]);
        assert_eq!(n.title, "Agent panes need Claude Code's adapter");
        assert!(n.body.starts_with("Claude Code is on this machine"), "{}", n.body);
        // Said already, for this pin: nothing.
        assert!(nudge(&v, &["claude@0.85.0".into()]).is_none());

        // A new pin, and the installed one older: out of date.
        let v = serde_json::json!({ "adapters": [
            { "kind": "claude", "label": "Claude Code", "pinned": "0.90.0", "state": "installed",
              "version": "0.85.0", "outdated": true, "found": true },
        ] });
        let n = nudge(&v, &["claude@0.85.0".into()]).unwrap();
        assert_eq!(n.title, "Claude Code's adapter is out of date");

        // Installed at the pin, or an older daemon that doesn't say: nothing.
        let v = serde_json::json!({ "adapters": [
            { "kind": "claude", "label": "Claude Code", "pinned": "0.85.0", "state": "installed",
              "outdated": false, "found": true },
        ] });
        assert!(nudge(&v, &[]).is_none());
        assert!(nudge(&serde_json::json!({ "claude": {} }), &[]).is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn stopping_says_what_it_means() {
        assert!(STOP_TEXT.contains("panes end"));
        assert!(STOP_TEXT.contains("phone"));
        assert!(STOP_TEXT.contains("next login"));
    }

    #[test]
    fn a_launchd_target_has_a_domain() {
        assert_eq!(domain("gui/501/illogicald"), "gui/501");
        assert_eq!(domain("user/501/illogicald"), "user/501");
    }
}
