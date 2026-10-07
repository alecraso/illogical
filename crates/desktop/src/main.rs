//! Arugula's desktop app (M46): the daemon's own web client in a native
//! window, for macOS, Linux and Windows.
//!
//! - **The daemon stays a separate service**, so panes outlive the window.
//!   The app finds the local one (`ARUGULA_URL`, else the address in the
//!   state directory's `listen` file, else `127.0.0.1:7681`) and loads its
//!   page: the UI and the daemon always match. Loopback callers show the
//!   daemon's local token (`local-token` in the state directory): the
//!   window opens the page through its sign-in link, and the app's own
//!   calls send it as a bearer.
//! - **It installs the daemon when there is none.** The bundle carries
//!   `arugulad` and `arugula` (sidecars, built by `sidecars.sh`). With no
//!   daemon answering, the window opens on a setup page that runs
//!   `arugulad install`: the installed one if there is one (it restarts the
//!   service), else the bundled one, which copies itself to `~/.local/bin`
//!   and registers the launchd agent or systemd unit. The bundled CLI goes
//!   to `~/.local/bin` too, unless an `arugula` is already installed.
//! - **It never replaces a running daemon** (#392): the daemon updates
//!   itself (its web notice's *Update now*, `arugulad update`), so the
//!   app and the daemon release apart. The bundled `arugulad` is only for
//!   a machine with none (first run, offline), and may be older than the
//!   one running. On macOS the app's launch agent runs the bundle's copy,
//!   which hands on to a newer one the daemon's update put in
//!   `~/.local/bin`.
//! - **Every key reaches the page** (S25), except a Mac's own: on macOS
//!   the app menu has Hide (Cmd-H), Hide Others and Quit (Cmd-Q, #320) and
//!   Close Window (Cmd-Shift-W, #323), and the rest is Edit only, so Cmd-W,
//!   T, N, M and U are the client's; on Linux GTK's F10 menu-bar key is
//!   turned off.
//! - **Native notifications** (S25: a webview has no push): a thread
//!   follows the daemon's state, notifies when a pane starts needing you and
//!   no window has focus, and a click opens that pane. The needs-you count
//!   goes on the dock badge (macOS) and the tray.
//! - **Every machine, through Arugula cloud** (M48, #159): once this
//!   machine is joined, the window is control's own client, signed in
//!   through the person's browser (`cloud.rs`). A join (or a leave) while
//!   the app is open moves the window there too (#204).
//! - **Its own profile per older WebKitGTK** (Linux): the .deb and the
//!   AppImage share one, and a newer WebKitGTK's storage breaks an older
//!   one (`profile.rs`).
//! - **Tabs in the titlebar** (M46): the client's bar is the titlebar
//!   (macOS: under the window buttons; Linux: undecorated, with the
//!   client's own buttons). On macOS new windows join the first as native
//!   tabs, which *Move Tab to New Window* takes back out. When AppKit shows
//!   its tab bar, the page's bar moves below it (`tab_bars`, #323).
//! - **`arugula://` links** (`links.rs`), **a global hotkey**, off by
//!   default (`settings.rs`), and **app updates** (`updates.rs`).
//! - **The file managers** (M47): Finder's *New Arugula Tab Here*
//!   service (`finder.rs`) and `.command` files on macOS; Nautilus's
//!   *Open in Arugula* (`linux/nautilus/arugula.py`) on Linux.
//! - A tray icon with *New window* and *This machine* (which brings forward
//!   a window already showing this machine, #323); one instance (a second
//!   launch opens a window in the first).
//! - **The daemon in the menus** (#322, `daemon.rs`): a *Daemon* submenu
//!   in the tray, and on macOS in the app menu and the Dock icon's menu
//!   (with *This machine*), says the daemon's version, state and service
//!   and this machine's standing with control (#325), and restarts, stops
//!   and starts it. When control drops this machine, a native
//!   notification, once per drop, whose click opens Getting started's join.
//! - **Windows** (M54, #217; the daemon since M59, #222): the app
//!   carries `arugulad` and sets it up the same way, as a scheduled task
//!   (`arugulad install`). Only the *Daemon* menu and what follows the
//!   daemon for it (`daemon.rs`) aren't there yet.
//!
//! ## What the app uses from the daemon (#390)
//!
//! The app and the daemon will ship apart (#388), so an app can meet an
//! older or a newer daemon. This is everything it relies on. A daemon that
//! changes any of it so that an app built before would break bumps
//! `arugula_proto::PROTOCOL`; the app checks that number at launch
//! (`compat.rs`).
//!
//! - **`proto` types**: [`ServerMsg`]'s `hello`, `state` and `delta`
//!   (others are skipped), [`State`] and [`State::apply`], and of each pane
//!   its `id`, [`Attention`] (`needs_input`), `reason.headline` and
//!   `command`, for notifications and the badge.
//! - **`GET /ws`**: the WebSocket, with the local token as a bearer. The
//!   app only reads it.
//! - **`GET /api/host`**: `version` and `protocol`
//!   (`compat.rs`; absent means the baseline, and below 0.19.0 too old,
//!   #317), `name` and `control` (`cloud.rs`), `control_state`
//!   (`daemon.rs`; absent from older daemons, which then never notify of
//!   a drop).
//! - **`GET /api/update`** (`apply`, `command`; `newer` and `latest` for
//!   the *Daemon* menu) and **`POST /api/update/apply`** (#391): the
//!   daemon's own update, which the setup page offers when the daemon is
//!   too old for this app (`compat.rs`), and the *Daemon* menu when a newer
//!   one is out (`daemon.rs`).
//! - **`GET /api/setup?part=agents`** (`adapters`: `kind`, `label`,
//!   `state`, `outdated`, `found`, `pinned`): an agent here without its
//!   adapter, for one notification (`daemon.rs`, #335).
//! - **`POST /api/run`** `{cwd, command}`, answering `{pane}`: a new tab
//!   for `arugula://open`, a folder or a `.command` file (`links.rs`).
//! - **The page**: `/`, `/#pane=N` and `/#getting-started=SECTION`, the
//!   sign-in link `/auth?token=…&next=…`, and the `arugula:open-pane` and
//!   `arugula:getting-started` events the app sends a page already open. The other way, the page reads
//!   `window.__arugulaApp` and calls the app's huddle commands
//!   (`call_native_*`) and window permissions (capabilities/default.json).
//! - **The local files**: the state directory's `listen` (the address) and
//!   `local-token`.
//! - **The service install**: `arugulad install`, which copies itself to
//!   `~/.local/bin` (Windows: `%LOCALAPPDATA%\Programs\arugula`), keeps the
//!   flags the last install wrote and (re)starts the service: the systemd
//!   user unit `arugulad.service`, the launchd agent `arugulad`, or the
//!   scheduled task `arugulad`. On macOS the app's own launch agent runs
//!   the bundled copy instead (`service.rs`). And `arugulad --version`,
//!   printing `arugulad X.Y.Z`.

#[cfg(all(target_os = "linux", feature = "native-calls"))]
mod calls;
mod cloud;
mod compat;
mod daemon;
#[cfg(target_os = "macos")]
mod finder;
mod links;
mod profile;
#[cfg(target_os = "macos")]
mod service;
mod settings;
mod updates;

use std::{
    collections::HashMap,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use arugula_proto::{Attention, ServerMsg, State};
use tauri::{
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::TrayIconBuilder,
};

static WINDOWS: AtomicUsize = AtomicUsize::new(0);
/// Why the daemon couldn't be reached, for the page that says so.
static STATUS: Mutex<String> = Mutex::new(String::new());
static ADDR: OnceLock<String> = OnceLock::new();
/// Held while the app starts, installs or re-registers the daemon.
static ONE: Mutex<()> = Mutex::new(());

/// The daemon's state directories, the one to read first first: its own
/// name's, then the old name's (#505), which a daemon from before the
/// rename (or one that kept its state where it was) writes to.
fn state_dirs() -> Vec<PathBuf> {
    if let Some(d) = std::env::var_os("ARUGULA_STATE_DIR") {
        return vec![PathBuf::from(d)];
    }
    // Windows: the daemon's (`%LOCALAPPDATA%\arugula\state`, M56).
    #[cfg(windows)]
    let (base, under) = (std::env::var_os("LOCALAPPDATA").map(PathBuf::from), |d: PathBuf| d.join("state"));
    #[cfg(not(windows))]
    let (base, under) = (
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state"))),
        |d: PathBuf| d,
    );
    let Some(base) = base else { return Vec::new() };
    ["arugula", arugula_proto::rename::OLD].iter().map(|n| under(base.join(n))).collect()
}

/// `name` in the daemon's state directory: the first that has it.
fn state_file(name: &str) -> Option<PathBuf> {
    state_dirs().into_iter().map(|d| d.join(name)).find(|f| f.is_file())
}

/// `host:port` of the local daemon.
fn addr() -> &'static str {
    ADDR.get_or_init(|| {
        if let Ok(u) = std::env::var("ARUGULA_URL") {
            return u.trim_start_matches("http://").trim_end_matches('/').to_string();
        }
        state_file("listen")
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "127.0.0.1:7681".into())
    })
}

fn page() -> String {
    format!("http://{}", addr())
}

/// The local daemon's token, which loopback callers show.
fn local_token() -> Option<String> {
    let file = std::env::var_os("ARUGULA_LOCAL_TOKEN_FILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| state_file("local-token"))?;
    std::fs::read_to_string(file).ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// `Authorization` for the app's own calls to the daemon.
fn bearer() -> Option<String> {
    local_token().map(|t| format!("Bearer {t}"))
}

/// `path` on the daemon's page, through its sign-in link (which sets the
/// browser's cookie and goes on to `path`).
fn page_at(path: &str) -> tauri::Url {
    let url = match local_token() {
        Some(t) => {
            let next: String = path
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                        (b as char).to_string()
                    }
                    _ => format!("%{b:02X}"),
                })
                .collect();
            format!("{}/auth?token={t}&next={next}", page())
        }
        None => format!("{}{path}", page()),
    };
    url.parse().unwrap()
}

fn reachable() -> bool {
    let Some(sa) = addr().to_socket_addrs().ok().and_then(|mut a| a.next()) else { return false };
    TcpStream::connect_timeout(&sa, Duration::from_millis(400)).is_ok()
}

/// An installed copy of the first of `names` there is: `~/.local/bin`,
/// Homebrew, then `PATH`; on Windows, where `arugulad install` puts it,
/// then `PATH`.
fn installed(names: &[&str]) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = home.iter().map(|h| h.join(".local/bin")).collect();
    if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        // An install from before the rename is in Programs\illogical (#505).
        dirs.extend(local.iter().flat_map(|l| ["arugula", "illogical"].map(|n| l.join("Programs").join(n))));
    } else {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    names.iter().find_map(|name| {
        let name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        let ours = bundled(&name);
        dirs.iter().map(|d| d.join(&name)).find(|p| p.is_file() && Some(p) != ours.as_ref())
    })
}

/// The daemon, under its name or (installed before the rename, #505) its
/// old one.
const DAEMON: [&str; 2] = ["arugulad", "illogicald"];

/// The copy of `name` this app carries, next to its own executable.
fn bundled(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let name = name.strip_suffix(std::env::consts::EXE_SUFFIX).unwrap_or(name);
    Some(exe.parent()?.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))).filter(|p| p.is_file())
}

/// macOS: a downloaded app's files carry the quarantine flag, and a copy
/// out of the bundle keeps it, which can stop launchd from running the
/// daemon. The person already allowed the app; clear it on what we copy.
fn unquarantine(path: &std::path::Path) {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(path).output();
    }
}

/// The bundled CLI into `~/.local/bin`, when no `arugula` is installed.
/// (Windows: `arugulad install` puts it beside itself, on PATH.) On a Mac,
/// `arugulad` too, as a link, when none is installed (#550): the app's
/// launch agent runs the bundle's, so nothing else puts one on PATH for
/// `arugulad join` or `arugulad update`. (Elsewhere `arugulad install`
/// copies itself there, and a later one replaces the link with a copy.)
fn install_cli() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    // Only the new names: ones from before the rename stay, beside these.
    if installed(&["arugulad"]).is_none() {
        link_to_bundle("arugulad");
    }
    if installed(&["arugula"]).is_some() {
        return None;
    }
    if let Some(dst) = link_to_bundle("arugula") {
        return Some(dst);
    }
    let src = bundled("arugula")?;
    let dir = PathBuf::from(std::env::var_os("HOME")?).join(".local/bin");
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join("arugula");
    std::fs::copy(&src, &dst).ok()?;
    unquarantine(&dst);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755));
    }
    Some(dst)
}

/// macOS, with the app in Applications: `~/.local/bin/NAME` as a link to
/// the bundle's copy, which app updates keep current. Elsewhere (a disk
/// image, Downloads) the app may move, so none.
fn link_to_bundle(name: &str) -> Option<PathBuf> {
    let src = bundled(name)?;
    if !cfg!(target_os = "macos") || !src.components().any(|c| c.as_os_str() == "Applications") {
        return None;
    }
    let dir = PathBuf::from(std::env::var_os("HOME")?).join(".local/bin");
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join(name);
    let _ = std::fs::remove_file(&dst);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&src, &dst).ok()?;
    Some(dst)
}

/// Reach the daemon: start the installed one, or install the bundled one.
/// Never starts a second daemon: `arugulad install` (re)starts the one
/// service.
fn ensure_daemon() -> Result<(), String> {
    // One at a time: a second window's setup page waits for the first's.
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    start_daemon()
}

/// `ensure_daemon`, with `ONE` held.
fn start_daemon() -> Result<(), String> {
    if reachable() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if service::usable() && !service::installed_by_script() && std::env::var_os("ARUGULA_NO_LAUNCH_AGENT").is_none() {
        match start_agent() {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("arugula: the app's launch agent: {e}; installing with arugulad install instead");
                let _ = service::unregister();
            }
        }
    }
    let Some(bin) = installed(&DAEMON).or_else(|| bundled("arugulad")) else {
        return Err(format!(
            "Nothing answers at {}, arugulad isn't installed, and this app doesn't carry one.",
            addr()
        ));
    };
    let out =
        std::process::Command::new(&bin).arg("install").output().map_err(|e| format!("{}: {e}", bin.display()))?;
    // `install` copied itself to ~/.local/bin (and restarted the service).
    if let (Some(home), Some(name)) = (std::env::var_os("HOME"), bin.file_name()) {
        let copied = PathBuf::from(home).join(".local/bin").join(name);
        if copied != bin {
            unquarantine(&copied);
        }
    }
    let cli = install_cli();
    for _ in 0..60 {
        if reachable() {
            if let Some(cli) = cli {
                eprintln!("arugula: installed the CLI at {}", cli.display());
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "Ran {} install, but nothing answers at {}.\n{}{}",
        bin.display(),
        addr(),
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// macOS: the daemon as the app's launch agent (`service.rs`), registered
/// the first time, started again if it isn't answering.
#[cfg(target_os = "macos")]
fn start_agent() -> Result<(), String> {
    if service::registered() {
        service::restart()?;
    } else {
        service::register()?;
        eprintln!("arugula: registered the daemon's launch agent ({})", service::LABEL);
    }
    let cli = install_cli();
    for _ in 0..60 {
        if reachable() {
            if let Some(cli) = cli {
                eprintln!("arugula: installed the CLI at {}", cli.display());
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("the launch agent is {}, but nothing answers at {}", service::status(), addr()))
}

/// Where a new window starts:
/// - no daemon answering: the setup page, which installs or starts it
///   (`retry`) and then comes back here;
/// - a daemon speaking a protocol this app doesn't (#390): the setup page,
///   which says which side is behind and offers its update (`compat.rs`);
/// - joined to control and signed in: control's client, every machine;
/// - joined, not signed in: the app's sign-in page;
/// - otherwise (or "just this machine"): the daemon's own page.
fn target(app: &AppHandle) -> WebviewUrl {
    if !reachable() || compat::mismatch().is_some() {
        return WebviewUrl::App("index.html".into());
    }
    WebviewUrl::External(home(app))
}

fn home(app: &AppHandle) -> tauri::Url {
    let local = cloud::local();
    match local.control {
        Some(c) if !cloud::local_only() => {
            if cloud::signed_in(app, &c) {
                format!("{c}/").parse().unwrap()
            } else {
                cloud::app_url(cloud::SIGNIN)
            }
        }
        _ => page_at("/"),
    }
}

/// The daemon's page, control's page and the app's own pages stay in the
/// window.
fn ours(url: &tauri::Url) -> bool {
    match url.scheme() {
        "tauri" | "about" | "blob" | "data" => true,
        "http" | "https" => {
            let control = cloud::control().and_then(|c| c.parse::<tauri::Url>().ok());
            if control.is_some_and(|c| c.origin() == url.origin()) {
                return true;
            }
            daemons(url) || url.host_str() == Some("tauri.localhost")
        }
        _ => false,
    }
}

/// The local daemon's own page (or its sign-in link).
fn daemons(url: &tauri::Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().zip(url.port_or_known_default()).is_some_and(|(h, p)| {
            format!("{h}:{p}") == addr() || (h == "localhost" && addr().ends_with(&format!(":{p}")))
        })
}

/// Follows the daemon's join (#204): when it joins control (from the CLI,
/// or Getting started's button) or leaves, windows showing the old home
/// move to the new one (`cloud::moves`), as a restart would have.
fn follow_join(app: AppHandle) {
    let mut was = cloud::control();
    loop {
        std::thread::sleep(JOIN_POLL);
        if !reachable() {
            continue;
        }
        let now = cloud::local().control;
        if now == was {
            continue;
        }
        eprintln!("arugula: this machine's control is now {}", now.as_deref().unwrap_or("none"));
        // A new control: "just this machine" was said of the old one.
        cloud::set_local_only(false);
        allow_control(&app);
        let home = home(&app);
        for w in app.webview_windows().into_values() {
            let Ok(url) = w.url() else { continue };
            if cloud::moves(was.as_deref(), now.as_deref(), &url, daemons(&url)) {
                let to = home.clone();
                let _ = app.run_on_main_thread(move || {
                    let _ = w.navigate(to);
                });
            }
        }
        was = now;
    }
}

/// How often the app asks the daemon whether it joined or left control.
const JOIN_POLL: Duration = Duration::from_secs(2);

/// Another site, in the person's own browser: control's approval page,
/// Tailscale's admin console, docs.
fn open_outside(url: &tauri::Url) {
    // For tests: write the URL down instead of opening a browser.
    if let Some(log) = std::env::var_os("ARUGULA_OPEN_LOG") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(f, "{url}");
        }
        return;
    }
    // Windows: the URL handler directly; `cmd /c start` would split it at `&`.
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" })
    };
    if let Err(e) = cmd.arg(url.as_str()).spawn() {
        eprintln!("arugula: opening {url}: {e}");
    }
}

/// The window permissions the bar needs (capabilities/default.json), for
/// control's origin too: when joined, the window shows control's page, and
/// without them its bar can't move, minimize, maximize or close the window
/// on Linux. The control is only known at run time, and can change (#204).
/// Control's page also runs huddles through the app (M63), as the daemon's
/// page does.
fn allow_control(app: &AppHandle) {
    static ALLOWED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let Some(origin) = cloud::control()
        .and_then(|c| c.parse::<tauri::Url>().ok())
        .map(|u| u.origin().ascii_serialization())
        .filter(|o| o != "null")
    else {
        return;
    };
    let mut allowed = ALLOWED.lock().unwrap();
    if allowed.contains(&origin) {
        return;
    }
    let cap = tauri::ipc::CapabilityBuilder::new(format!("control-{}", allowed.len()))
        .remote(format!("{origin}/*"))
        .window("*")
        .permission("core:window:allow-minimize")
        .permission("core:window:allow-toggle-maximize")
        .permission("core:window:allow-internal-toggle-maximize")
        .permission("core:window:allow-close")
        .permission("core:window:allow-start-dragging")
        .permission("allow-call-native-start")
        .permission("allow-call-native-peer")
        .permission("allow-call-native-remote")
        .permission("allow-call-native-drop")
        .permission("allow-call-native-mute")
        .permission("allow-call-native-stop")
        .permission("allow-call-native-status");
    match app.add_capability(cap) {
        Ok(()) => allowed.push(origin),
        Err(e) => eprintln!("arugula: letting {origin} use its window and calls: {e}"),
    }
}

fn open_window(app: &AppHandle, url: WebviewUrl) -> tauri::Result<tauri::WebviewWindow> {
    allow_control(app);
    let n = WINDOWS.fetch_add(1, Ordering::SeqCst);
    let label = format!("w{n}");
    let handle = app.clone();
    let nav = (app.clone(), label.clone());
    let mut builder = WebviewWindowBuilder::new(app, label, url);
    if let Some(dir) = profile::dir() {
        builder = builder.data_directory(dir);
    }
    // The client's bar is the titlebar (M46).
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .tabbing_identifier("arugula")
            // Shown once it's set to join the others as a tab.
            .visible(false);
    }
    #[cfg(target_os = "linux")]
    {
        builder = builder.decorations(false);
    }
    let w = builder
        .title("Arugula")
        .inner_size(1280.0, 820.0)
        .initialization_script(cloud::init_script())
        // Huddles (M63): the microphone for the client's own pages (macOS
        // still asks, once, for the app); nobody else's.
        .on_permission_request(|w, kind| match kind {
            tauri::webview::PermissionKind::Microphone if w.url().is_ok_and(|u| ours(&u)) => {
                tauri::webview::PermissionResponse::Allow
            }
            tauri::webview::PermissionKind::Microphone => tauri::webview::PermissionResponse::Deny,
            _ => tauri::webview::PermissionResponse::Default,
        })
        // Files dropped on a pane are the page's (M70: uploaded to the
        // pane's host); the webview would otherwise take them itself.
        .disable_drag_drop_handler()
        // A link with target=_blank: the client's own pages in a window of
        // ours, anything else in the browser. A webview drops these unless
        // the app handles them.
        .on_new_window(move |url, _| {
            if ours(&url) {
                let h = handle.clone();
                let _ = handle.run_on_main_thread(move || {
                    let _ = open_window(&h, WebviewUrl::External(url));
                });
            } else {
                open_outside(&url);
            }
            tauri::webview::NewWindowResponse::Deny
        })
        // macOS: the window's title, which its native tab shows (#323),
        // says what the page is; and a new page hears whether AppKit's tab
        // bar shows. (Linux keeps "Arugula": the tests find the window
        // by it.)
        .on_page_load(|_w, _load| {
            #[cfg(target_os = "macos")]
            if let tauri::webview::PageLoadEvent::Finished = _load.event() {
                let _ = _w.set_title(&title_for(_load.url()));
                tab_bar_reload(&_w);
            }
        })
        // Control's own sign-in (GitHub) can't finish in the window: the
        // app's sign-in takes over. A link away from the daemon and control:
        // the browser takes it.
        .on_navigation(move |url| {
            if cloud::is_control_signin(url) {
                let (app, label) = nav.clone();
                if let Some(c) = cloud::control() {
                    cloud::set_signed_in(&app, &c, false);
                }
                let a = app.clone();
                let _ = app.run_on_main_thread(move || {
                    if let Some(w) = a.get_webview_window(&label) {
                        let _ = w.navigate(cloud::app_url(cloud::SIGNIN));
                    }
                });
                return false;
            }
            if ours(url) {
                return true;
            }
            open_outside(url);
            false
        })
        .build()?;
    w.on_window_event(|e| {
        if let tauri::WindowEvent::Focused(f) = e {
            settings::focus_changed(*f);
        }
    });
    #[cfg(target_os = "macos")]
    tab_in(&w);
    let _ = w.set_focus();
    Ok(w)
}

/// macOS: a new window opens as a tab of the window in front, whatever the
/// system's "Prefer tabs" setting says.
#[cfg(target_os = "macos")]
fn tab_in(w: &tauri::WebviewWindow) {
    let Ok(ptr) = w.ns_window() else { return };
    let (ptr, w2) = (ptr as usize, w.clone());
    let _ = w.run_on_main_thread(move || {
        // SAFETY: the window's NSWindow, on the main thread, while it lives.
        let ns = unsafe { &*(ptr as *const objc2_app_kit::NSWindow) };
        ns.setTabbingMode(objc2_app_kit::NSWindowTabbingMode::Preferred);
        let _ = w2.show();
        let _ = w2.set_focus();
    });
}

/// What a window's title (a native tab's label) says for `url`.
#[cfg(target_os = "macos")]
fn title_for(url: &tauri::Url) -> String {
    if daemons(url) {
        return "This machine".into();
    }
    let control = cloud::control().and_then(|c| c.parse::<tauri::Url>().ok());
    match control {
        Some(c) if c.origin() == url.origin() => c.host_str().unwrap_or("Arugula").to_owned(),
        _ => "Arugula".into(),
    }
}

/// A window of ours already showing a page `wanted` picks, the focused
/// one first.
fn showing(app: &AppHandle, wanted: impl Fn(&tauri::Url) -> bool) -> Option<tauri::WebviewWindow> {
    let mut found: Vec<_> = app.webview_windows().into_values().filter(|w| w.url().is_ok_and(|u| wanted(&u))).collect();
    found.sort_by_key(|w| !w.is_focused().unwrap_or(false));
    found.into_iter().next()
}

fn bring_forward(w: &tauri::WebviewWindow) {
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
}

/// *Open Arugula*: a window showing home (control's page, or the
/// daemon's) comes forward, else any window, and one opens only when there
/// is none (#323).
fn focus_or_open(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.show();
    let control = cloud::control().and_then(|c| c.parse::<tauri::Url>().ok());
    let home = |u: &tauri::Url| daemons(u) || control.as_ref().is_some_and(|c| c.origin() == u.origin());
    match showing(app, home).or_else(|| showing(app, |_| true)) {
        Some(w) => bring_forward(&w),
        None => {
            let _ = open_window(app, target(app));
        }
    }
}

/// *This machine*: the daemon's own page, whatever home is, unless it
/// doesn't match this app (the setup page). A window already showing it
/// comes forward; another click doesn't add a tab (#323).
fn this_machine(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.show();
    if compat::mismatch().is_some() {
        let _ = open_window(app, target(app));
        return;
    }
    match showing(app, daemons) {
        Some(w) => bring_forward(&w),
        None => {
            let _ = open_window(app, WebviewUrl::External(page_at("/")));
        }
    }
}

/// macOS (#323): once a window has two native tabs AppKit shows its tab
/// bar across the top of the window, over the page (the content runs under
/// the titlebar). The page hears how tall the titlebar and the strip are
/// (`--native-tabs` and `data-native-tabs` on its root), and moves its bar
/// below them; 0 when the strip is gone. AppKit says nothing when it shows
/// or hides the strip (a new tab, *Merge All Windows*, a tab closed), so
/// the app looks a few times a second and tells a page only what changed.
#[cfg(target_os = "macos")]
static TAB_BARS: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

#[cfg(target_os = "macos")]
fn follow_tab_bars(app: AppHandle) {
    loop {
        std::thread::sleep(Duration::from_millis(400));
        let a = app.clone();
        let _ = app.run_on_main_thread(move || tab_bars(&a));
    }
}

/// On the main thread.
#[cfg(target_os = "macos")]
fn tab_bars(app: &AppHandle) {
    let windows = app.webview_windows();
    let mut shown = TAB_BARS.lock().unwrap();
    let shown = shown.get_or_insert_with(HashMap::new);
    shown.retain(|label, _| windows.contains_key(label));
    for (label, w) in windows {
        let Ok(ptr) = w.ns_window() else { continue };
        // SAFETY: the window's NSWindow, on the main thread, while it lives.
        let ns = unsafe { &*(ptr as *const objc2_app_kit::NSWindow) };
        let strip = ns.tabGroup().is_some_and(|g| g.isTabBarVisible());
        let h = if strip {
            (ns.frame().size.height - ns.contentLayoutRect().size.height).max(0.0).round() as u32
        } else {
            0
        };
        if shown.get(&label) == Some(&h) {
            continue;
        }
        let js = format!(
            "(h => {{ const d = document.documentElement; if (h) {{ d.dataset.nativeTabs = ''; d.style.setProperty('--native-tabs', h + 'px'); }} else {{ delete d.dataset.nativeTabs; d.style.removeProperty('--native-tabs'); }} }})({h})"
        );
        if w.eval(js).is_ok() {
            shown.insert(label, h);
        }
    }
}

/// A new page in `w` knows nothing of the strip: tell it again.
#[cfg(target_os = "macos")]
fn tab_bar_reload(w: &tauri::WebviewWindow) {
    if let Some(shown) = TAB_BARS.lock().unwrap().as_mut() {
        shown.remove(w.label());
    }
    let app = w.app_handle().clone();
    let a = app.clone();
    let _ = app.run_on_main_thread(move || tab_bars(&a));
}

/// Cmd-Shift-W (#323): the focused window goes, with its native tab. (Cmd-W
/// is the page's: it closes a pane.)
fn close_window(app: &AppHandle) {
    if let Some(w) = app.webview_windows().into_values().find(|w| w.is_focused().unwrap_or(false)) {
        let _ = w.close();
    }
}

/// From a notification: the pane, in a window of ours. Not while the
/// daemon and the app don't match: the setup page says why.
fn open_pane(app: &AppHandle, pane: u32) {
    if compat::mismatch().is_some() {
        return focus_or_open(app);
    }
    let url = page_at(&format!("/#pane={pane}"));
    #[cfg(target_os = "macos")]
    let _ = app.show();
    match showing(app, daemons).or_else(|| app.webview_windows().into_values().next()) {
        Some(w) => {
            // Already on the daemon's page: the client opens it (main.tsx),
            // without a reload.
            if w.url().is_ok_and(|u| daemons(&u)) {
                let _ = w.eval(format!(
                    "dispatchEvent(new CustomEvent({PAGE_EVENT_PREFIX}+':open-pane', {{ detail: {pane} }}))"
                ));
            } else {
                let _ = w.navigate(url);
            }
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
        None => {
            let _ = open_window(app, WebviewUrl::External(url));
        }
    }
}

#[tauri::command]
fn daemon_status() -> String {
    let why = STATUS.lock().unwrap().clone();
    if !why.is_empty() {
        return why;
    }
    if let Some(m) = compat::mismatch() {
        return m.message();
    }
    match installed(&DAEMON) {
        Some(bin) => format!("Starting {}…", bin.display()),
        None if bundled("arugulad").is_some() => "Installing arugulad (a service that starts at login)…".into(),
        None => format!("Nothing answers at {}.", addr()),
    }
}

#[tauri::command]
async fn retry(app: AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    let r = tauri::async_runtime::spawn_blocking(|| {
        ensure_daemon()?;
        // Answering now: does it speak a protocol this app does?
        compat::check();
        compat::mismatch().map_or(Ok(()), |m| Err(m.message()))
    })
    .await
    .map_err(|e| e.to_string())?;
    *STATUS.lock().unwrap() = r.clone().err().unwrap_or_default();
    r?;
    let to = tauri::async_runtime::spawn_blocking(move || home(&app)).await.map_err(|e| e.to_string())?;
    window.navigate(to).map_err(|e| e.to_string())
}

// ---- notifications

/// What a notification's click opens.
#[derive(Clone, Copy)]
enum Click {
    Pane(u32),
    /// Getting started's cloud step, to join control again (#325).
    JoinAgain,
    /// Getting started's agents step, to install an adapter (#335).
    Agents,
}

impl Click {
    /// Linux and macOS. A Windows toast opens a link instead.
    #[cfg(not(windows))]
    fn open(self, app: &AppHandle) {
        match self {
            Click::Pane(pane) => open_pane(app, pane),
            Click::JoinAgain => daemon::getting_started(app, "cloud"),
            Click::Agents => daemon::getting_started(app, "agents"),
        }
    }
}

fn notify(app: &AppHandle, click: Click, title: String, body: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        // Windows: a toast under the app's own id (its Start menu shortcut,
        // which the installer makes, carries it). A click opens
        // `arugula://pane/N`, which comes back to this app (one instance)
        // as a link, from the popup or the Action Center alike.
        #[cfg(windows)]
        if let Click::Pane(pane) = click
            && let Err(e) = toast(&app.config().identifier, &title, &body, pane)
        {
            eprintln!("arugula: a notification: {e}");
        }
        #[cfg(target_os = "linux")]
        {
            let Ok(handle) = notify_rust::Notification::new()
                .summary(&title)
                .body(&body)
                .appname("Arugula")
                .icon("arugula-desktop")
                .action("default", "Open")
                .show()
            else {
                return;
            };
            handle.wait_for_action(|action| {
                if action == "default" {
                    let a = app.clone();
                    let _ = app.run_on_main_thread(move || click.open(&a));
                }
            });
        }
        #[cfg(target_os = "macos")]
        {
            use mac_notification_sys::{Notification, NotificationResponse, send_notification, set_application};
            let _ = set_application(arugula_proto::service::APP_ID);
            if let Ok(NotificationResponse::Click) =
                send_notification(&title, None, &body, Some(Notification::new().wait_for_click(true)))
            {
                let a = app.clone();
                let _ = app.run_on_main_thread(move || click.open(&a));
            }
        }
    });
}

/// A Windows toast whose click opens `arugula://pane/{pane}`.
#[cfg(windows)]
fn toast(app_id: &str, title: &str, body: &str, pane: u32) -> windows::core::Result<()> {
    use windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{ToastNotification, ToastNotificationManager},
        core::HSTRING,
    };
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let xml = format!(
        r#"<toast activationType="protocol" launch="arugula://pane/{pane}"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual></toast>"#,
        esc(title),
        esc(body)
    );
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))?.Show(&toast)
}

fn set_count(app: &AppHandle, count: usize) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        for w in app2.webview_windows().values() {
            let _ = w.set_badge_count(if count == 0 { None } else { Some(count as i64) });
        }
        if let Some(tray) = app2.tray_by_id("arugula") {
            let _ = tray.set_tooltip(Some(if count == 0 {
                "Arugula".to_string()
            } else {
                format!("Arugula: {count} need you")
            }));
            let _ = tray.set_title(Some(if count == 0 { String::new() } else { count.to_string() }));
        }
    });
}

/// Follows the daemon's state over its WebSocket, reconnecting as needed.
fn watch(app: AppHandle) {
    loop {
        if let Err(e) = watch_once(&app) {
            eprintln!("arugula: watching the daemon: {e}");
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

fn watch_once(app: &AppHandle) -> anyhow::Result<()> {
    let sa: SocketAddr = addr().to_socket_addrs()?.next().ok_or_else(|| anyhow::anyhow!("no address"))?;
    let tcp = TcpStream::connect_timeout(&sa, Duration::from_secs(2))?;
    let mut req = tungstenite::client::IntoClientRequest::into_client_request(format!("ws://{}/ws", addr()))?;
    if let Some(b) = bearer() {
        req.headers_mut().insert("authorization", b.parse()?);
    }
    let (mut ws, _) = tungstenite::client(req, tcp).map_err(|e| anyhow::anyhow!("{e}"))?;
    // pane -> needs you; None until the first state, so what already waits
    // at launch shows on the badge without a burst of notifications.
    let mut seen: Option<HashMap<u32, bool>> = None;
    // The whole state comes with hello and layout changes; pane changes
    // (attention among them) come as deltas on it.
    let mut state: Option<State> = None;
    loop {
        let msg = ws.read()?;
        let tungstenite::Message::Text(t) = msg else { continue };
        match serde_json::from_str(&t) {
            Ok(ServerMsg::Hello { state: s, .. }) | Ok(ServerMsg::State { state: s }) => state = Some(s),
            Ok(ServerMsg::Delta { delta }) => match state.as_mut() {
                Some(s) => s.apply(&delta),
                None => continue,
            },
            _ => continue,
        }
        let Some(state) = state.as_ref() else { continue };
        let now: HashMap<u32, bool> =
            state.panes.iter().map(|p| (p.id, p.attention == Attention::NeedsInput)).collect();
        if let Some(before) = &seen {
            // ARUGULA_NOTIFY_FOCUSED=1 notifies even with a window focused (for testing).
            let focused = std::env::var_os("ARUGULA_NOTIFY_FOCUSED").is_none()
                && app.webview_windows().values().any(|w| w.is_focused().unwrap_or(false));
            for p in &state.panes {
                let id = p.id;
                if now[&id] && !before.get(&id).copied().unwrap_or(false) && !focused {
                    let what = p
                        .reason
                        .as_ref()
                        .map(|r| r.headline.clone())
                        .or_else(|| p.command.clone())
                        .unwrap_or_else(|| "needs you".into());
                    notify(app, Click::Pane(id), format!("%{id} needs you"), what);
                }
            }
        }
        set_count(app, now.values().filter(|n| **n).count());
        seen = Some(now);
    }
}

/// The prefix of the events the app sends into a daemon's page, as JS: a
/// page from after the rename (#505) says so (`window.__arugulaPage`) and
/// listens for `arugula:`, one from a daemon on 0.25 or older only for
/// `illogical:`. One event either way, so a new page doesn't hear it twice.
pub(crate) const PAGE_EVENT_PREFIX: &str = "(window.__arugulaPage ? 'arugula' : 'illogical')";

fn main() {
    // ILLOGICAL_X stands in for ARUGULA_X (#505), before any thread exists.
    // SAFETY: nothing else runs yet.
    unsafe { arugula_proto::rename::alias_env() };
    // `arugula-desktop --agent status|register|unregister|restart`: the
    // daemon's launch agent, for tests and for fixing a Mac by hand.
    #[cfg(target_os = "macos")]
    if std::env::args().nth(1).as_deref() == Some("--agent") {
        let r = match std::env::args().nth(2).as_deref() {
            Some("register") => service::register(),
            Some("unregister") => service::unregister(),
            Some("restart") => service::restart(),
            _ => Ok(()),
        };
        println!("{}", service::status());
        if let Err(e) = r {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    let context = tauri::generate_context!();
    let updater = updates::configured(context.config());
    updates::init(updater);
    let mut builder = tauri::Builder::default()
        // A second launch: its links (Linux runs the app with the link), or
        // a new window.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let urls = links::in_args(args);
            if urls.is_empty() {
                let _ = open_window(app, target(app));
            }
            for url in urls {
                links::handle(app, url);
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(settings::plugin());
    builder = builder.plugin(tauri_plugin_dialog::init());
    if updater {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    #[cfg(all(target_os = "linux", feature = "native-calls"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        daemon_status,
        retry,
        compat::compat,
        compat::compat_fix,
        cloud::cloud_status,
        cloud::cloud_signin,
        cloud::cloud_local,
        calls::call_native_start,
        calls::call_native_peer,
        calls::call_native_remote,
        calls::call_native_drop,
        calls::call_native_mute,
        calls::call_native_stop,
        calls::call_native_status
    ]);
    #[cfg(not(all(target_os = "linux", feature = "native-calls")))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        daemon_status,
        retry,
        compat::compat,
        compat::compat_fix,
        cloud::cloud_status,
        cloud::cloud_signin,
        cloud::cloud_local
    ]);
    builder
        .menu(|app| {
            #[cfg(target_os = "macos")]
            {
                // The app menu, and Edit (copy, paste, select all, which
                // WKWebView needs a menu for). Not Tauri's default menu: it
                // takes Cmd-W and M, which are the page's. Cmd-Q, H and
                // Option-H are the Mac's (#320): the page doesn't use them,
                // and the terminal never gets Cmd keys. Quitting needs no
                // confirming: the panes are the daemon's and keep running.
                use tauri::menu::{PredefinedMenuItem, Submenu};
                let check = MenuItem::with_id(app, "check-updates", "Check for Updates…", true, None::<&str>)?;
                // Where Mac users look first (#322): the tray icon hides
                // behind the notch on a full menu bar.
                let this = MenuItem::with_id(app, "menu-this", "This machine", true, None::<&str>)?;
                let app_menu = Submenu::with_items(
                    app,
                    "Arugula",
                    true,
                    &[
                        &PredefinedMenuItem::about(app, None, None)?,
                        &check,
                        &this,
                        &daemon::submenu(app)?,
                        &PredefinedMenuItem::separator(app)?,
                        &PredefinedMenuItem::hide(app, Some("Hide Arugula"))?,
                        &PredefinedMenuItem::hide_others(app, None)?,
                        &PredefinedMenuItem::show_all(app, None)?,
                        &PredefinedMenuItem::separator(app)?,
                        // Cmd-W is the page's (a pane); this closes the
                        // window, and its native tab (#323).
                        &MenuItem::with_id(app, "close-window", "Close Window", true, Some("CmdOrCtrl+Shift+W"))?,
                        &PredefinedMenuItem::quit(app, Some("Quit Arugula"))?,
                    ],
                )?;
                let edit = Submenu::with_items(
                    app,
                    "Edit",
                    true,
                    &[
                        &PredefinedMenuItem::copy(app, None)?,
                        &PredefinedMenuItem::paste(app, None)?,
                        &PredefinedMenuItem::select_all(app, None)?,
                    ],
                )?;
                Menu::with_items(app, &[&app_menu, &edit])
            }
            #[cfg(not(target_os = "macos"))]
            Menu::new(app)
        })
        // The app menu's Check for Updates… (#419) and Close Window (#323),
        // macOS; the app menu's, the Dock's and the Daemon submenus' (#322).
        .on_menu_event(|app, e| match e.id().as_ref() {
            "check-updates" => updates::check_now(app),
            "close-window" => close_window(app),
            id => {
                daemon::menu_event(app, id);
            }
        })
        .setup(move |app| {
            #[cfg(target_os = "linux")]
            {
                // GTK opens a menu bar on F10; the page wants the key (S25).
                use gtk::prelude::*;
                if let Some(s) = gtk::Settings::default() {
                    s.set_property("gtk-menu-bar-accel", "");
                }
                // An AppImage has no .desktop file of its own to claim the
                // scheme; the packages do.
                if std::env::var_os("APPIMAGE").is_some() {
                    use tauri_plugin_deep_link::DeepLinkExt;
                    if let Err(e) = app.deep_link().register_all() {
                        eprintln!("arugula: registering arugula:// links: {e}");
                    }
                    // #505: the app from before the rename claimed
                    // illogical:// in its own file, which this one now does.
                    if let Some(h) = std::env::var_os("HOME") {
                        let old = PathBuf::from(h).join(".local/share/applications/illogical-desktop-handler.desktop");
                        let _ = std::fs::remove_file(old);
                    }
                }
            }
            settings::adopt_old(app.handle());
            profile::init(app.handle());
            #[cfg(target_os = "macos")]
            finder::init(app.handle());
            // #505: the app from before the rename's launch agent gives way
            // to this app's daemon (a window finding none waits for this).
            #[cfg(target_os = "macos")]
            std::thread::spawn(|| {
                let one = ONE.lock().unwrap_or_else(|e| e.into_inner());
                if service::retire_old() {
                    // Its daemon has up to its ExitTimeOut (15 s) to go.
                    for _ in 0..80 {
                        if !reachable() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    if let Err(e) = start_daemon() {
                        eprintln!("arugula: starting the daemon after the old app's: {e}");
                    }
                }
                // The agent already runs the daemon: `arugulad` and the CLI
                // on PATH all the same (#550).
                if service::registered() {
                    install_cli();
                }
                drop(one);
            });
            compat::check();
            let prefs = settings::load(app.handle());
            let hotkey_ok = match settings::apply(app.handle(), &prefs) {
                Ok(()) => prefs.hotkey_on,
                Err(e) => {
                    eprintln!("arugula: {e}");
                    false
                }
            };
            open_window(app.handle(), target(app.handle()))?;
            // Linux: a link the app was started with.
            #[cfg(not(target_os = "macos"))]
            for url in links::in_args(std::env::args()) {
                links::handle(app.handle(), url);
            }
            let open = MenuItem::with_id(app, "open", "Open Arugula", true, None::<&str>)?;
            let new = MenuItem::with_id(app, "new", "New window", true, None::<&str>)?;
            let this = MenuItem::with_id(app, "this", "This machine", true, None::<&str>)?;
            // Not on Windows yet: its daemon is a scheduled task (M59).
            let daemon_menu = if cfg!(windows) { None } else { Some(daemon::submenu(app.handle())?) };
            let hotkey = CheckMenuItem::with_id(
                app,
                "hotkey",
                format!("Global hotkey ({})", prefs.keys()),
                true,
                hotkey_ok,
                None::<&str>,
            )?;
            let update = MenuItem::with_id(app, "update", "Restart to update", false, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            // Which app this is (#419): there's no other place it shows on
            // Linux and Windows.
            let version = MenuItem::with_id(
                app,
                "version",
                format!("Arugula {}", app.package_info().version),
                false,
                None::<&str>,
            )?;
            let check = MenuItem::with_id(app, "tray-check-updates", "Check for updates…", true, None::<&str>)?;
            let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = vec![&version, &open, &new, &this];
            if let Some(d) = &daemon_menu {
                items.push(d);
            }
            items.push(&hotkey);
            if updates::enabled() {
                items.push(&check);
                items.push(&update);
            }
            items.push(&quit);
            let hotkey_item = hotkey.clone();
            TrayIconBuilder::with_id("arugula")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Arugula")
                .menu(&Menu::with_items(app, &items)?)
                .on_menu_event(move |app, e| match e.id().as_ref() {
                    "open" => focus_or_open(app),
                    "new" => {
                        let _ = open_window(app, target(app));
                    }
                    "this" => this_machine(app),
                    "hotkey" => {
                        let mut prefs = settings::load(app);
                        prefs.hotkey_on = !prefs.hotkey_on;
                        let on = match settings::apply(app, &prefs) {
                            Ok(()) => prefs.hotkey_on,
                            Err(e) => {
                                eprintln!("arugula: {e}");
                                prefs.hotkey_on = false;
                                false
                            }
                        };
                        settings::save(app, &prefs);
                        let _ = hotkey_item.set_checked(on);
                    }
                    "update" => updates::from_tray(app),
                    "tray-check-updates" => updates::check_now(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            app.manage(updates::Item(update));
            if updater {
                updates::start(app.handle().clone());
            } else {
                eprintln!("arugula: this build has no updater key; it doesn't check for updates");
            }
            let handle = app.handle().clone();
            std::thread::Builder::new().name("watch".into()).spawn(move || watch(handle))?;
            let handle = app.handle().clone();
            std::thread::Builder::new().name("join".into()).spawn(move || follow_join(handle))?;
            if !cfg!(windows) {
                let handle = app.handle().clone();
                std::thread::Builder::new().name("daemon".into()).spawn(move || daemon::follow(handle))?;
            }
            #[cfg(target_os = "macos")]
            {
                let handle = app.handle().clone();
                std::thread::Builder::new().name("tab-bars".into()).spawn(move || follow_tab_bars(handle))?;
            }
            #[cfg(target_os = "macos")]
            daemon::dock::init(app.handle());
            Ok(())
        })
        .build(context)
        .expect("Arugula desktop")
        .run(|app, event| match event {
            // macOS: stay in the Dock with no windows, as Mac apps do.
            // Linux: stay in the tray while the global hotkey is on.
            tauri::RunEvent::ExitRequested { code: None, api, .. }
                if cfg!(target_os = "macos") || settings::load(app).hotkey_on =>
            {
                api.prevent_exit()
            }
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { has_visible_windows: false, .. } => focus_or_open(app),
            // macOS: an arugula:// link (the app started for it, or was
            // running); a folder dropped on the app or opened with it, a new
            // tab there; a .command file, run in a new tab (M47).
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Opened { urls } => {
                for url in urls {
                    match url.scheme() {
                        "file" => match url.to_file_path() {
                            Ok(p) if p.is_dir() => links::open_dir(app, &p),
                            Ok(p) => links::run_file(app, &p),
                            Err(()) => eprintln!("arugula: not a file this app opens: {url}"),
                        },
                        _ => links::handle(app, url.to_string()),
                    }
                }
            }
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use arugula_proto::service::{APP_ID, APP_LABEL};

    fn file(p: &str) -> String {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(p)).unwrap()
    }

    /// The identifier and the launch agent's names live in tauri.conf.json
    /// and the plist as well as in proto: they agree (#505).
    #[test]
    fn the_names_agree() {
        let conf: serde_json::Value = serde_json::from_str(&file("tauri.conf.json")).unwrap();
        assert_eq!(conf["identifier"], APP_ID);
        assert_eq!(conf["productName"], "Arugula");
        let plist_name = format!("{APP_LABEL}.plist");
        let files = &conf["bundle"]["macOS"]["files"];
        assert_eq!(files[format!("Library/LaunchAgents/{plist_name}")], format!("macos/{plist_name}"));
        let plist = file(&format!("macos/{plist_name}"));
        assert!(plist.contains(&format!("<key>Label</key>\n  <string>{APP_LABEL}</string>")), "{plist}");
        assert!(plist.contains(&format!("<string>{APP_ID}</string>")), "{plist}");
        // Both schemes, old links keep working.
        assert_eq!(conf["plugins"]["deep-link"]["desktop"]["schemes"], serde_json::json!(["arugula", "illogical"]));
    }

    /// #505: the daemon's state under its new name, else its old one.
    #[test]
    fn reads_the_state_under_either_name() {
        let base = std::env::temp_dir().join(format!("arugula-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // SAFETY: this test alone touches these variables.
        unsafe {
            std::env::remove_var("ARUGULA_STATE_DIR");
            std::env::set_var("XDG_STATE_HOME", &base);
        }
        if cfg!(windows) {
            return;
        }
        assert_eq!(super::state_dirs(), [base.join("arugula"), base.join("illogical")]);
        std::fs::create_dir_all(base.join("illogical")).unwrap();
        std::fs::write(base.join("illogical/listen"), "127.0.0.1:1").unwrap();
        assert_eq!(super::state_file("listen"), Some(base.join("illogical/listen")));
        std::fs::create_dir_all(base.join("arugula")).unwrap();
        std::fs::write(base.join("arugula/listen"), "127.0.0.1:2").unwrap();
        assert_eq!(super::state_file("listen"), Some(base.join("arugula/listen")));
        assert_eq!(super::state_file("local-token"), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
