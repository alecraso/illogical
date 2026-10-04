//! S25: a Tauri 2 shell around illogical's web client.
//!
//! `s25-desktop [URL | probe | bench]` opens one window: the daemon's own
//! page (default `http://127.0.0.1:7681`), the key/clipboard/notification
//! probe, or the xterm.js renderer bench. Both local pages are served from
//! `../probe`.
//!
//! Environment:
//! - `S25_TITLEBAR=overlay`: no system titlebar; the client's `.bar` becomes
//!   the drag region and gets window buttons (macOS keeps its traffic lights).
//! - `S25_HUD=1`: frame rate, worst frame and the last chord, drawn over
//!   the page.
//! - `S25_MENU=default` (macOS): Tauri's stock menu, which keeps Cmd-W/Q/H
//!   for itself. Otherwise an Edit menu only, so every other chord reaches
//!   the page.
//! - `S25_AUTO=1`: the bench runs on load. `S25_EXIT=1`: quit after a report.
//! - `S25_OUT=path`: where the bench writes its results (also printed).
//! - `S25_LOG=path`: the probe's event log, one JSON object a line. With
//!   `S25_AUTO=1` the probe runs what needs no hands and logs every key,
//!   for `drive.py` to press chords against.

use std::{
    sync::OnceLock,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

use tauri::{
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    webview::PageLoadEvent,
};

static START: OnceLock<Instant> = OnceLock::new();
static WINDOWS: AtomicUsize = AtomicUsize::new(0);
static TARGET: OnceLock<WebviewUrl> = OnceLock::new();

const INIT: &str = include_str!("init.js");

fn elapsed_ms() -> u128 {
    START.get().map_or(0, |s| s.elapsed().as_millis())
}

fn target(arg: Option<String>) -> WebviewUrl {
    match arg.as_deref() {
        Some("probe") => WebviewUrl::App("keys.html".into()),
        Some("bench") => WebviewUrl::App("bench.html".into()),
        Some(url) => WebviewUrl::External(url.parse().expect("a URL, probe or bench")),
        None => WebviewUrl::External("http://127.0.0.1:7681".parse().unwrap()),
    }
}

fn flags() -> String {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    serde_json::json!({
        "titlebar": env("S25_TITLEBAR"),
        "hud": env("S25_HUD") == "1",
        "auto": env("S25_AUTO") == "1",
        "os": std::env::consts::OS,
    })
    .to_string()
}

fn open_window(app: &AppHandle, url: WebviewUrl) -> tauri::Result<()> {
    let n = WINDOWS.fetch_add(1, Ordering::SeqCst);
    let overlay = std::env::var("S25_TITLEBAR").as_deref() == Ok("overlay");
    let script = format!("window.__S25_FLAGS = {};\n{INIT}", flags());
    let builder = WebviewWindowBuilder::new(app, format!("w{n}"), url)
        .title("illogical (S25)")
        .inner_size(1280.0, 820.0)
        .initialization_script(&script)
        .on_page_load(|w, p| {
            if p.event() == PageLoadEvent::Finished {
                println!("s25: {} loaded {} at {} ms", w.label(), p.url(), elapsed_ms());
            }
        });
    #[cfg(target_os = "macos")]
    let builder = if overlay {
        builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true)
    } else {
        builder
    };
    #[cfg(not(target_os = "macos"))]
    let builder = builder.decorations(!overlay);
    builder.build()?;
    Ok(())
}

#[tauri::command]
fn s25_new_window(app: AppHandle) -> Result<(), String> {
    open_window(&app, TARGET.get().unwrap().clone()).map_err(|e| e.to_string())
}

#[tauri::command]
fn s25_report(app: AppHandle, json: String) {
    println!("s25 report: {json}");
    if let Ok(path) = std::env::var("S25_OUT") {
        let _ = std::fs::write(path, &json);
    }
    if std::env::var("S25_EXIT").as_deref() == Ok("1") {
        app.exit(0);
    }
}

/// One line of the probe's event log (`S25_LOG`), which a driver tails.
#[tauri::command]
fn s25_log(line: String) {
    use std::io::Write;
    if let Ok(path) = std::env::var("S25_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

#[tauri::command]
fn s25_badge(app: AppHandle, count: u32) {
    // The dock badge (macOS) and the tray tooltip: the "needs you" count.
    for w in app.webview_windows().values() {
        let _ = w.set_badge_count(if count == 0 { None } else { Some(count as i64) });
    }
    if let Some(tray) = app.tray_by_id("s25") {
        let _ = tray.set_tooltip(Some(format!("illogical: {count} need you")));
        let _ = tray.set_title(Some(if count == 0 { String::new() } else { count.to_string() }));
    }
}

/// A native notification. On Linux it carries a default action, and a
/// click is sent back to the window that asked as `s25-notification-click`.
#[tauri::command]
fn s25_notify(window: tauri::WebviewWindow, title: String, body: String, tag: String) -> Result<u32, String> {
    #[cfg(target_os = "linux")]
    {
        let handle = notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .appname("illogical")
            .action("default", "Open")
            .show()
            .map_err(|e| e.to_string())?;
        let id = handle.id();
        println!("s25: notification {tag} is id {id}");
        std::thread::spawn(move || {
            handle.wait_for_action(|action| {
                println!("s25: notification {tag} action {action:?}");
                if action == "default" {
                    let _ = window.emit("s25-notification-click", &tag);
                    let _ = window.set_focus();
                }
            });
        });
        Ok(id)
    }
    #[cfg(not(target_os = "linux"))]
    {
        use tauri_plugin_notification::NotificationExt;
        let _ = tag;
        window.app_handle().notification().builder().title(title).body(body).show().map_err(|e| e.to_string())?;
        Ok(0)
    }
}

fn main() {
    START.set(Instant::now()).unwrap();
    TARGET.set(target(std::env::args().nth(1))).unwrap();
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![s25_new_window, s25_report, s25_notify, s25_badge, s25_log])
        .menu(|app| {
            // macOS needs an Edit menu for Cmd-C/V/A to reach a WKWebView at
            // all; Linux shows no menu. Tauri's default menu also binds
            // Cmd-W (close window), Cmd-Q, Cmd-H and Cmd-M.
            if cfg!(target_os = "macos") && std::env::var("S25_MENU").as_deref() == Ok("default") {
                return Menu::default(app);
            }
            #[cfg(target_os = "macos")]
            {
                use tauri::menu::{PredefinedMenuItem, Submenu};
                let app_menu = Submenu::with_items(app, "illogical", true, &[])?;
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
            println!("s25: setup at {} ms", elapsed_ms());
            open_window(app.handle(), TARGET.get().unwrap().clone())?;
            let new = MenuItem::with_id(app, "new", "New window", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            TrayIconBuilder::with_id("s25")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("illogical")
                .menu(&Menu::with_items(app, &[&new, &quit])?)
                .on_menu_event(|app, e| match e.id().as_ref() {
                    "new" => {
                        let _ = open_window(app, TARGET.get().unwrap().clone());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("tauri");
}
