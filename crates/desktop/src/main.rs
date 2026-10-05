//! illogical's desktop app (M46): the daemon's own web client in a native
//! window, for macOS and Linux.
//!
//! - **The daemon stays a separate service**, so panes outlive the window.
//!   The app finds the local one (`ILLOGICAL_URL`, else the address in the
//!   state directory's `listen` file, else `127.0.0.1:7681`) and loads its
//!   page: the UI and the daemon always match. Loopback callers show the
//!   daemon's local token (`local-token` in the state directory): the
//!   window opens the page through its sign-in link, and the app's own
//!   calls send it as a bearer.
//! - **It installs the daemon when there is none.** The bundle carries
//!   `illogicald` and `illogical` (sidecars, built by `sidecars.sh`). With no
//!   daemon answering, the window opens on a setup page that runs
//!   `illogicald install`: the installed one if there is one (it restarts the
//!   service), else the bundled one, which copies itself to `~/.local/bin`
//!   and registers the launchd agent or systemd unit. The bundled CLI goes
//!   to `~/.local/bin` too, unless an `illogical` is already installed.
//! - **It updates an older daemon** (#176): when the daemon's service runs
//!   an older version than the one bundled, the setup page runs the
//!   bundled `illogicald install`, which keeps its flags and its panes
//!   (`upgrade.rs`).
//! - **Every key reaches the page** (S25): on macOS the menu is Edit only,
//!   so Cmd-W, T, N and Q are the client's; on Linux GTK's F10 menu-bar key
//!   is turned off.
//! - **Native notifications** (S25: a webview has no push): a thread
//!   follows the daemon's state, notifies when a pane starts needing you and
//!   no window has focus, and a click opens that pane. The needs-you count
//!   goes on the dock badge (macOS) and the tray.
//! - **Every machine, through illogical cloud** (M48, #159): once this
//!   machine is joined, the window is control's own client, signed in
//!   through the person's browser (`cloud.rs`). A join (or a leave) while
//!   the app is open moves the window there too (#204).
//! - **Its own profile per older WebKitGTK** (Linux): the .deb and the
//!   AppImage share one, and a newer WebKitGTK's storage breaks an older
//!   one (`profile.rs`).
//! - **Tabs in the titlebar** (M46): the client's bar is the titlebar
//!   (macOS: under the window buttons; Linux: undecorated, with the
//!   client's own buttons). On macOS new windows join the first as native
//!   tabs, which *Move Tab to New Window* takes back out.
//! - **`illogical://` links** (`links.rs`), **a global hotkey**, off by
//!   default (`settings.rs`), and **app updates** (`updates.rs`).
//! - **The file managers** (M47): Finder's *New illogical Tab Here*
//!   service (`finder.rs`) and `.command` files on macOS; Nautilus's
//!   *Open in illogical* (`linux/nautilus/illogical.py`) on Linux.
//! - A tray icon with *New window* and *This machine*; one instance (a
//!   second launch opens a window in the first).
//! - **Windows has no daemon yet** (M54, #217; the daemon comes in M59):
//!   the app is control's client only, so a window opens on sign-in or
//!   control's page, and nothing local is installed, watched or offered.

#[cfg(all(target_os = "linux", feature = "native-calls"))]
mod calls;
mod cloud;
#[cfg(target_os = "macos")]
mod finder;
mod links;
mod profile;
#[cfg(target_os = "macos")]
mod service;
mod settings;
mod updates;
mod upgrade;

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

use illogical_proto::{Attention, ServerMsg, State};
use tauri::{
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::TrayIconBuilder,
};

static WINDOWS: AtomicUsize = AtomicUsize::new(0);
/// Why the daemon couldn't be reached, for the page that says so.
static STATUS: Mutex<String> = Mutex::new(String::new());
static ADDR: OnceLock<String> = OnceLock::new();

fn state_dir() -> Option<PathBuf> {
    // Windows: the daemon's (`%LOCALAPPDATA%\illogical\state`, M56).
    #[cfg(windows)]
    let windows = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("illogical").join("state"));
    #[cfg(not(windows))]
    let windows = None;
    std::env::var_os("ILLOGICAL_STATE_DIR")
        .map(PathBuf::from)
        .or(windows)
        .or_else(|| std::env::var_os("XDG_STATE_HOME").map(|d| PathBuf::from(d).join("illogical")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/illogical")))
}

/// `host:port` of the local daemon.
fn addr() -> &'static str {
    ADDR.get_or_init(|| {
        if let Ok(u) = std::env::var("ILLOGICAL_URL") {
            return u.trim_start_matches("http://").trim_end_matches('/').to_string();
        }
        state_dir()
            .and_then(|d| std::fs::read_to_string(d.join("listen")).ok())
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
    let file = std::env::var_os("ILLOGICAL_LOCAL_TOKEN_FILE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| state_dir().map(|d| d.join("local-token")))?;
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

/// An installed copy of `name`: `~/.local/bin`, Homebrew, then `PATH`;
/// on Windows, where `illogicald install` puts it, then `PATH`.
fn installed(name: &str) -> Option<PathBuf> {
    let name = &format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = home.iter().map(|h| h.join(".local/bin").join(name)).collect();
    if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        candidates.extend(local.map(|l| l.join("Programs").join("illogical").join(name)));
    } else {
        candidates.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(|d| PathBuf::from(d).join(name)));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(name)));
    }
    let ours = bundled(name);
    candidates.into_iter().find(|p| p.is_file() && Some(p) != ours.as_ref())
}

/// The copy of `name` this app carries, next to its own executable.
fn bundled(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
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

/// The bundled CLI into `~/.local/bin`, when no `illogical` is installed.
/// (Windows: `illogicald install` puts it beside itself, on PATH.)
fn install_cli() -> Option<PathBuf> {
    if cfg!(windows) || installed("illogical").is_some() {
        return None;
    }
    let src = bundled("illogical")?;
    let dir = PathBuf::from(std::env::var_os("HOME")?).join(".local/bin");
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join("illogical");
    // An app in Applications: a link into it, which app updates keep
    // current. Elsewhere (a disk image, Downloads) the app may move.
    if cfg!(target_os = "macos") && src.components().any(|c| c.as_os_str() == "Applications") {
        let _ = std::fs::remove_file(&dst);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&src, &dst).ok()?;
        return Some(dst);
    }
    std::fs::copy(&src, &dst).ok()?;
    unquarantine(&dst);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755));
    }
    Some(dst)
}

/// Reach the daemon: start the installed one, or install the bundled one.
/// Never starts a second daemon: `illogicald install` (re)starts the one
/// service.
fn ensure_daemon() -> Result<(), String> {
    // One at a time: a second window's setup page waits for the first's.
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    if reachable() {
        // Answering, but older than ours: replace it.
        return upgrade::run();
    }
    #[cfg(target_os = "macos")]
    if service::usable() && !service::installed_by_script() && std::env::var_os("ILLOGICAL_NO_LAUNCH_AGENT").is_none() {
        match start_agent() {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("illogical: the app's launch agent: {e}; installing with illogicald install instead");
                let _ = service::unregister();
            }
        }
    }
    let Some(bin) = installed("illogicald").or_else(|| bundled("illogicald")) else {
        return Err(format!(
            "Nothing answers at {}, illogicald isn't installed, and this app doesn't carry one.",
            addr()
        ));
    };
    let out =
        std::process::Command::new(&bin).arg("install").output().map_err(|e| format!("{}: {e}", bin.display()))?;
    // `install` copied itself to ~/.local/bin (and restarted the service).
    if let Some(home) = std::env::var_os("HOME") {
        let copied = PathBuf::from(home).join(".local/bin/illogicald");
        if copied != bin {
            unquarantine(&copied);
        }
    }
    let cli = install_cli();
    for _ in 0..60 {
        if reachable() {
            if let Some(cli) = cli {
                eprintln!("illogical: installed the CLI at {}", cli.display());
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
        eprintln!("illogical: registered the daemon's launch agent ({})", service::LABEL);
    }
    let cli = install_cli();
    for _ in 0..60 {
        if reachable() {
            if let Some(cli) = cli {
                eprintln!("illogical: installed the CLI at {}", cli.display());
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("the launch agent is {}, but nothing answers at {}", service::status(), addr()))
}

/// Where a new window starts:
/// - no daemon answering, or an older one to update: the setup page, which
///   installs, starts or updates it (`retry`) and then comes back here;
/// - joined to control and signed in: control's client, every machine;
/// - joined, not signed in: the app's sign-in page;
/// - otherwise (or "just this machine"): the daemon's own page.
fn target(app: &AppHandle) -> WebviewUrl {
    if !reachable() || upgrade::pending().is_some() {
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
        eprintln!("illogical: this machine's control is now {}", now.as_deref().unwrap_or("none"));
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
    if let Some(log) = std::env::var_os("ILLOGICAL_OPEN_LOG") {
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
        eprintln!("illogical: opening {url}: {e}");
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
        Err(e) => eprintln!("illogical: letting {origin} use its window and calls: {e}"),
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
            .tabbing_identifier("illogical")
            // Shown once it's set to join the others as a tab.
            .visible(false);
    }
    #[cfg(target_os = "linux")]
    {
        builder = builder.decorations(false);
    }
    let w = builder
        .title("illogical")
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

fn focus_or_open(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.show();
    match app.webview_windows().values().next() {
        Some(w) => {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
        None => {
            let _ = open_window(app, target(app));
        }
    }
}

/// From a notification: the pane, in a window of ours.
fn open_pane(app: &AppHandle, pane: u32) {
    let url = page_at(&format!("/#pane={pane}"));
    #[cfg(target_os = "macos")]
    let _ = app.show();
    match app.webview_windows().values().next() {
        Some(w) => {
            // Already on the daemon's page: the client opens it (main.tsx),
            // without a reload.
            if w.url().is_ok_and(|u| daemons(&u)) {
                let _ = w.eval(format!("dispatchEvent(new CustomEvent('illogical:open-pane', {{ detail: {pane} }}))"));
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
    if let Some(updating) = upgrade::pending() {
        return updating;
    }
    match installed("illogicald") {
        Some(bin) => format!("Starting {}…", bin.display()),
        None if bundled("illogicald").is_some() => "Installing illogicald (a service that starts at login)…".into(),
        None => format!("Nothing answers at {}.", addr()),
    }
}

#[tauri::command]
async fn retry(app: AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    let r = tauri::async_runtime::spawn_blocking(ensure_daemon).await.map_err(|e| e.to_string())?;
    *STATUS.lock().unwrap() = r.clone().err().unwrap_or_default();
    r?;
    let to = tauri::async_runtime::spawn_blocking(move || home(&app)).await.map_err(|e| e.to_string())?;
    window.navigate(to).map_err(|e| e.to_string())
}

// ---- notifications

fn notify(app: &AppHandle, pane: u32, title: String, body: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        // Windows: a toast under the app's own id (its Start menu shortcut,
        // which the installer makes, carries it). A click opens
        // `illogical://pane/N`, which comes back to this app (one instance)
        // as a link, from the popup or the Action Center alike.
        #[cfg(windows)]
        if let Err(e) = toast(&app.config().identifier, &title, &body, pane) {
            eprintln!("illogical: a notification: {e}");
        }
        #[cfg(target_os = "linux")]
        {
            let Ok(handle) = notify_rust::Notification::new()
                .summary(&title)
                .body(&body)
                .appname("illogical")
                .icon("illogical-desktop")
                .action("default", "Open")
                .show()
            else {
                return;
            };
            handle.wait_for_action(|action| {
                if action == "default" {
                    let a = app.clone();
                    let _ = app.run_on_main_thread(move || open_pane(&a, pane));
                }
            });
        }
        #[cfg(target_os = "macos")]
        {
            use mac_notification_sys::{Notification, NotificationResponse, send_notification, set_application};
            let _ = set_application("wtf.widgets.illogical");
            if let Ok(NotificationResponse::Click) =
                send_notification(&title, None, &body, Some(Notification::new().wait_for_click(true)))
            {
                let a = app.clone();
                let _ = app.run_on_main_thread(move || open_pane(&a, pane));
            }
        }
    });
}

/// A Windows toast whose click opens `illogical://pane/{pane}`.
#[cfg(windows)]
fn toast(app_id: &str, title: &str, body: &str, pane: u32) -> windows::core::Result<()> {
    use windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{ToastNotification, ToastNotificationManager},
        core::HSTRING,
    };
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let xml = format!(
        r#"<toast activationType="protocol" launch="illogical://pane/{pane}"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual></toast>"#,
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
        if let Some(tray) = app2.tray_by_id("illogical") {
            let _ = tray.set_tooltip(Some(if count == 0 {
                "illogical".to_string()
            } else {
                format!("illogical: {count} need you")
            }));
            let _ = tray.set_title(Some(if count == 0 { String::new() } else { count.to_string() }));
        }
    });
}

/// Follows the daemon's state over its WebSocket, reconnecting as needed.
fn watch(app: AppHandle) {
    loop {
        if let Err(e) = watch_once(&app) {
            eprintln!("illogical: watching the daemon: {e}");
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
            // ILLOGICAL_NOTIFY_FOCUSED=1 notifies even with a window focused (for testing).
            let focused = std::env::var_os("ILLOGICAL_NOTIFY_FOCUSED").is_none()
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
                    notify(app, id, format!("%{id} needs you"), what);
                }
            }
        }
        set_count(app, now.values().filter(|n| **n).count());
        seen = Some(now);
    }
}

fn main() {
    // `illogical-desktop --agent status|register|unregister|restart`: the
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
    if updater {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    #[cfg(all(target_os = "linux", feature = "native-calls"))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        daemon_status,
        retry,
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
        cloud::cloud_status,
        cloud::cloud_signin,
        cloud::cloud_local
    ]);
    builder
        .menu(|app| {
            #[cfg(target_os = "macos")]
            {
                // Edit only (copy, paste, select all, which WKWebView needs
                // a menu for): Tauri's default menu takes Cmd-W/Q/H/M.
                use tauri::menu::{PredefinedMenuItem, Submenu};
                let app_menu =
                    Submenu::with_items(app, "illogical", true, &[&PredefinedMenuItem::about(app, None, None)?])?;
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
                        eprintln!("illogical: registering illogical:// links: {e}");
                    }
                }
            }
            profile::init(app.handle());
            #[cfg(target_os = "macos")]
            finder::init(app.handle());
            upgrade::check();
            let prefs = settings::load(app.handle());
            let hotkey_ok = match settings::apply(app.handle(), &prefs) {
                Ok(()) => prefs.hotkey_on,
                Err(e) => {
                    eprintln!("illogical: {e}");
                    false
                }
            };
            open_window(app.handle(), target(app.handle()))?;
            // Linux: a link the app was started with.
            #[cfg(not(target_os = "macos"))]
            for url in links::in_args(std::env::args()) {
                links::handle(app.handle(), url);
            }
            let open = MenuItem::with_id(app, "open", "Open illogical", true, None::<&str>)?;
            let new = MenuItem::with_id(app, "new", "New window", true, None::<&str>)?;
            let this = MenuItem::with_id(app, "this", "This machine", true, None::<&str>)?;
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
            let mut items: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = vec![&open, &new, &this];
            items.push(&hotkey);
            if updater && updates::can_update() {
                items.push(&update);
            }
            items.push(&quit);
            let hotkey_item = hotkey.clone();
            TrayIconBuilder::with_id("illogical")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("illogical")
                .menu(&Menu::with_items(app, &items)?)
                .on_menu_event(move |app, e| match e.id().as_ref() {
                    "open" => focus_or_open(app),
                    "new" => {
                        let _ = open_window(app, target(app));
                    }
                    // The daemon's own page, whatever the window shows.
                    "this" => {
                        let _ = open_window(app, WebviewUrl::External(page_at("/")));
                    }
                    "hotkey" => {
                        let mut prefs = settings::load(app);
                        prefs.hotkey_on = !prefs.hotkey_on;
                        let on = match settings::apply(app, &prefs) {
                            Ok(()) => prefs.hotkey_on,
                            Err(e) => {
                                eprintln!("illogical: {e}");
                                prefs.hotkey_on = false;
                                false
                            }
                        };
                        settings::save(app, &prefs);
                        let _ = hotkey_item.set_checked(on);
                    }
                    "update" => app.restart(),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            app.manage(updates::Item(update));
            if updater {
                updates::start(app.handle().clone());
            } else {
                eprintln!("illogical: this build has no updater key; it doesn't check for updates");
            }
            let handle = app.handle().clone();
            std::thread::Builder::new().name("watch".into()).spawn(move || watch(handle))?;
            let handle = app.handle().clone();
            std::thread::Builder::new().name("join".into()).spawn(move || follow_join(handle))?;
            Ok(())
        })
        .build(context)
        .expect("illogical desktop")
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
            // macOS: an illogical:// link (the app started for it, or was
            // running); a folder dropped on the app or opened with it, a new
            // tab there; a .command file, run in a new tab (M47).
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Opened { urls } => {
                for url in urls {
                    match url.scheme() {
                        "file" => match url.to_file_path() {
                            Ok(p) if p.is_dir() => links::open_dir(app, &p),
                            Ok(p) => links::run_file(app, &p),
                            Err(()) => eprintln!("illogical: not a file this app opens: {url}"),
                        },
                        _ => links::handle(app, url.to_string()),
                    }
                }
            }
            _ => {}
        });
}
