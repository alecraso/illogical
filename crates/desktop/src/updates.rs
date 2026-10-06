//! App updates (M46): the Tauri updater, against a manifest
//! (`latest.json`) on the release page, signed with the release's updater
//! key.
//!
//! A build without an updater key (`plugins.updater.pubkey` in
//! tauri.conf.json) never checks. One with a key checks a little after
//! launch and then every six hours. A newer release is downloaded,
//! checked against the key and put in place of this app; it runs from the
//! next start, which the tray offers (*Restart to update*). The daemon
//! keeps the panes through that restart. The app never updates the daemon:
//! the daemon updates itself (#391, #392). When the daemon here speaks a
//! newer protocol than this app knows, the setup page offers the update
//! straight away and restarts into it (`compat.rs`).
//!
//! The manifest is the newest app release's, on the rolling `app-latest`
//! release (#393): app releases (`app-v*`) are apart from the daemon's.
//!
//! Where it can update: the macOS app, the Linux AppImage and the Windows
//! installer. A .deb or .rpm belongs to the package manager. On Windows the
//! installer closes the app as it runs, so the background check only finds
//! the update; installing waits for the tray's *Update*.
//!
//! For tests: `ILLOGICAL_UPDATE_URL` is the manifest to read,
//! `ILLOGICAL_UPDATE_RESTART=1` restarts as soon as the update is in.

use std::{
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tauri::{AppHandle, Manager, menu::MenuItem};
use tauri_plugin_updater::UpdaterExt;

/// The tray's update item, once the app has one.
pub struct Item(pub MenuItem<tauri::Wry>);

/// This build has a key and this install can replace itself (`init`).
static ON: AtomicBool = AtomicBool::new(false);

/// The version put in place, waiting for a restart.
static READY: Mutex<Option<String>> = Mutex::new(None);

/// One check or download at a time: the background's or the tray's.
static BUSY: LazyLock<tauri::async_runtime::Mutex<()>> = LazyLock::new(|| tauri::async_runtime::Mutex::new(()));

/// This build has an updater key.
pub fn configured(config: &tauri::Config) -> bool {
    config
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .is_some_and(|k| !k.trim().is_empty())
}

/// This install can replace itself.
pub fn can_update() -> bool {
    cfg!(target_os = "macos") || cfg!(windows) || std::env::var_os("APPIMAGE").is_some()
}

/// At launch, before any window: whether this app updates itself.
pub fn init(configured: bool) {
    ON.store(configured && can_update(), Ordering::Relaxed);
}

/// The tray offers the update.
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn start(app: AppHandle) {
    if !can_update() {
        eprintln!("illogical: updates come from the package manager here");
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        loop {
            // A test that restarts at once installs on Windows too.
            let install = !cfg!(windows) || std::env::var_os("ILLOGICAL_UPDATE_RESTART").is_some();
            match tauri::async_runtime::block_on(fetch(&app, install)) {
                Ok(Some(v)) => {
                    ready(&app, &v);
                    return;
                }
                Ok(None) => {}
                Err(e) => eprintln!("illogical: checking for an update: {e}"),
            }
            std::thread::sleep(Duration::from_secs(6 * 3600));
        }
    });
}

/// A newer release, put in place when `install` (else only found); its
/// version. Already in place: that version, without asking again. Also
/// when the daemon here is newer than this app knows (`compat.rs`).
pub async fn fetch(app: &AppHandle, install: bool) -> Result<Option<String>, String> {
    let _one = BUSY.lock().await;
    if let Some(v) = READY.lock().unwrap().clone() {
        return Ok(Some(v));
    }
    let mut b = app.updater_builder();
    if let Some(u) = std::env::var("ILLOGICAL_UPDATE_URL").ok().filter(|u| !u.is_empty()) {
        b = b
            .endpoints(vec![u.parse().map_err(|e| format!("ILLOGICAL_UPDATE_URL: {e}"))?])
            .map_err(|e| e.to_string())?;
    }
    let Some(update) = b.build().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    if !install {
        eprintln!("illogical: {} is out; it installs from the tray or the page", update.version);
        return Ok(Some(update.version));
    }
    eprintln!("illogical: updating the app from {} to {}", update.current_version, update.version);
    // Windows: the installer closes this app here and opens the new one.
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    eprintln!("illogical: {} is in place; it runs from the next start", update.version);
    *READY.lock().unwrap() = Some(update.version.clone());
    Ok(Some(update.version))
}

fn ready(app: &AppHandle, version: &str) {
    if std::env::var_os("ILLOGICAL_UPDATE_RESTART").is_some() && READY.lock().unwrap().is_some() {
        app.restart();
    }
    if let Some(item) = app.try_state::<Item>() {
        let verb = if READY.lock().unwrap().is_some() { "Restart to update" } else { "Update" };
        let _ = item.0.set_text(format!("{verb} to {version}"));
        let _ = item.0.set_enabled(true);
    }
}

/// The tray's item: restart into the update, installing it first if the
/// background check only found it (Windows).
pub fn from_tray(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match fetch(&app, true).await {
            Ok(Some(_)) => app.restart(),
            Ok(None) => {}
            Err(e) => eprintln!("illogical: updating the app: {e}"),
        }
    });
}

/// *Check for Updates…* (the macOS app menu, and the tray; #419): check
/// now and say what was found, since by the time the tray item's text
/// changes its menu has closed.
pub fn check_now(app: &AppHandle) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let app = app.clone();
    let current = app.package_info().version.to_string();
    if !enabled() {
        let why = if can_update() {
            "This build of illogical doesn't check for updates."
        } else {
            "This copy of illogical updates with its package manager."
        };
        app.dialog().message(why).title(format!("illogical {current}")).show(|_| {});
        return;
    }
    tauri::async_runtime::spawn(async move {
        // Windows: find only; the installer closes the app, so it waits
        // for a yes.
        let found = fetch(&app, !cfg!(windows)).await;
        let dialog = app.dialog().clone();
        match found {
            Ok(None) => {
                dialog.message(format!("illogical {current} is the latest.")).title("No update").show(|_| {});
            }
            Ok(Some(v)) => {
                ready(&app, &v);
                let in_place = READY.lock().unwrap().is_some();
                let (text, yes) = if in_place {
                    (format!("illogical {v} is ready. Restart now to use it?"), "Restart Now")
                } else {
                    (format!("illogical {v} is out. Install it now? The app closes while it installs."), "Update Now")
                };
                let app = app.clone();
                dialog
                    .message(text)
                    .title("Update")
                    .buttons(MessageDialogButtons::OkCancelCustom(yes.into(), "Later".into()))
                    .show(move |yes| {
                        if yes {
                            from_tray(&app);
                        }
                    });
            }
            Err(e) => {
                dialog
                    .message(format!("Couldn't check for updates: {e}"))
                    .title("Update")
                    .kind(MessageDialogKind::Error)
                    .show(|_| {});
            }
        }
    });
}
