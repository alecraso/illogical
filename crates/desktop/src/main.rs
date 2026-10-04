//! illogical's desktop app (M46): the daemon's own web client in a native
//! window, for macOS and Linux.
//!
//! - **The daemon stays a separate service**, so panes outlive the window.
//!   The app finds the local one (`ILLOGICAL_URL`, else the address in the
//!   state directory's `listen` file, else `127.0.0.1:7681`), starts it with
//!   `illogicald install` when it's installed but not running, and loads its
//!   page: the UI and the daemon always match. Loopback auth is unchanged.
//! - **Every key reaches the page** (S25): on macOS the menu is Edit only,
//!   so Cmd-W, T, N and Q are the client's; on Linux GTK's F10 menu-bar key
//!   is turned off.
//! - **Native notifications** (S25: a webview has no push): a thread
//!   follows the daemon's state, notifies when a pane starts needing you and
//!   no window has focus, and a click opens that pane. The needs-you count
//!   goes on the dock badge (macOS) and the tray.
//! - A tray icon with *New window*; one instance (a second launch opens a
//!   window in the first).

use std::{
    collections::HashMap,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{Mutex, OnceLock, atomic::{AtomicUsize, Ordering}},
    time::Duration,
};

use illogical_proto::{Attention, ServerMsg, State};
use tauri::{
    AppHandle, Manager, WebviewUrl, WebviewWindowBuilder,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
};

static WINDOWS: AtomicUsize = AtomicUsize::new(0);
/// Why the daemon couldn't be reached, for the page that says so.
static STATUS: Mutex<String> = Mutex::new(String::new());
static ADDR: OnceLock<String> = OnceLock::new();

fn state_dir() -> Option<PathBuf> {
    std::env::var_os("ILLOGICAL_STATE_DIR")
        .map(PathBuf::from)
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

fn reachable() -> bool {
    let Some(sa) = addr().to_socket_addrs().ok().and_then(|mut a| a.next()) else { return false };
    TcpStream::connect_timeout(&sa, Duration::from_millis(400)).is_ok()
}

fn find_illogicald() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = home.iter().map(|h| h.join(".local/bin/illogicald")).collect();
    candidates.extend(["/opt/homebrew/bin/illogicald", "/usr/local/bin/illogicald", "/usr/bin/illogicald"].map(PathBuf::from));
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join("illogicald")));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Reach the daemon, starting it if it's installed. Never starts a second
/// one: `illogicald install` (re)starts the installed service.
fn ensure_daemon() -> Result<(), String> {
    if reachable() {
        return Ok(());
    }
    let Some(bin) = find_illogicald() else {
        return Err(format!("Nothing answers at {} and illogicald isn't installed.", addr()));
    };
    let out = std::process::Command::new(&bin).arg("install").output().map_err(|e| format!("{}: {e}", bin.display()))?;
    for _ in 0..40 {
        if reachable() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "Started {} install, but nothing answers at {}.\n{}",
        bin.display(),
        addr(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

fn target() -> WebviewUrl {
    match ensure_daemon() {
        Ok(()) => WebviewUrl::External(page().parse().unwrap()),
        Err(why) => {
            *STATUS.lock().unwrap() = why;
            WebviewUrl::App("index.html".into())
        }
    }
}

fn open_window(app: &AppHandle, url: WebviewUrl) -> tauri::Result<tauri::WebviewWindow> {
    let n = WINDOWS.fetch_add(1, Ordering::SeqCst);
    let w = WebviewWindowBuilder::new(app, format!("w{n}"), url).title("illogical").inner_size(1280.0, 820.0).build()?;
    let _ = w.set_focus();
    Ok(w)
}

fn focus_or_open(app: &AppHandle) {
    match app.webview_windows().values().next() {
        Some(w) => {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
        None => {
            let _ = open_window(app, target());
        }
    }
}

/// From a notification: the pane, in a window of ours.
fn open_pane(app: &AppHandle, pane: u32) {
    let url: tauri::Url = format!("{}/#pane={pane}", page()).parse().unwrap();
    match app.webview_windows().values().next() {
        Some(w) => {
            let _ = w.navigate(url);
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
        None => {
            let _ = open_window(app, WebviewUrl::External(url));
        }
    }
}

#[tauri::command]
fn daemon_status() -> String {
    STATUS.lock().unwrap().clone()
}

#[tauri::command]
async fn retry(window: tauri::WebviewWindow) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(ensure_daemon).await.map_err(|e| e.to_string())??;
    window.navigate(page().parse().unwrap()).map_err(|e| e.to_string())
}

// ---- notifications

fn notify(app: &AppHandle, pane: u32, title: String, body: String) {
    let app = app.clone();
    std::thread::spawn(move || {
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

fn set_count(app: &AppHandle, count: usize) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        for w in app2.webview_windows().values() {
            let _ = w.set_badge_count(if count == 0 { None } else { Some(count as i64) });
        }
        if let Some(tray) = app2.tray_by_id("illogical") {
            let _ = tray.set_tooltip(Some(if count == 0 { "illogical".to_string() } else { format!("illogical: {count} need you") }));
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
    let (mut ws, _) = tungstenite::client(format!("ws://{}/ws", addr()), tcp).map_err(|e| anyhow::anyhow!("{e}"))?;
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
            state.panes.iter().map(|p| (u32::from(p.id), p.attention == Attention::NeedsInput)).collect();
        if let Some(before) = &seen {
            // ILLOGICAL_NOTIFY_FOCUSED=1 notifies even with a window focused (for testing).
            let focused = std::env::var_os("ILLOGICAL_NOTIFY_FOCUSED").is_none()
                && app.webview_windows().values().any(|w| w.is_focused().unwrap_or(false));
            for p in &state.panes {
                let id = u32::from(p.id);
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
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = open_window(app, target());
        }))
        .invoke_handler(tauri::generate_handler![daemon_status, retry])
        .menu(|app| {
            #[cfg(target_os = "macos")]
            {
                // Edit only (copy, paste, select all, which WKWebView needs
                // a menu for): Tauri's default menu takes Cmd-W/Q/H/M.
                use tauri::menu::{PredefinedMenuItem, Submenu};
                let app_menu = Submenu::with_items(app, "illogical", true, &[&PredefinedMenuItem::about(app, None, None)?])?;
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
        .setup(|app| {
            #[cfg(target_os = "linux")]
            {
                // GTK opens a menu bar on F10; the page wants the key (S25).
                use gtk::prelude::*;
                if let Some(s) = gtk::Settings::default() {
                    s.set_property("gtk-menu-bar-accel", "");
                }
            }
            open_window(app.handle(), target())?;
            let open = MenuItem::with_id(app, "open", "Open illogical", true, None::<&str>)?;
            let new = MenuItem::with_id(app, "new", "New window", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            TrayIconBuilder::with_id("illogical")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("illogical")
                .menu(&Menu::with_items(app, &[&open, &new, &quit])?)
                .on_menu_event(|app, e| match e.id().as_ref() {
                    "open" => focus_or_open(app),
                    "new" => {
                        let _ = open_window(app, target());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            let handle = app.handle().clone();
            std::thread::Builder::new().name("watch".into()).spawn(move || watch(handle))?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("illogical desktop")
        .run(|app, event| match event {
            // macOS: stay in the Dock with no windows, as Mac apps do.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { has_visible_windows: false, .. } => focus_or_open(app),
            _ => {
                let _ = app;
            }
        });
}
