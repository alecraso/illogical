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
//! Where it can update: the macOS app and the Linux AppImage. A .deb or
//! .rpm belongs to the package manager.
//!
//! For tests: `ILLOGICAL_UPDATE_URL` is the manifest to read,
//! `ILLOGICAL_UPDATE_RESTART=1` restarts as soon as the update is in.

use std::time::Duration;

use tauri::{AppHandle, Manager, menu::MenuItem};
use tauri_plugin_updater::UpdaterExt;

/// The tray's update item, once the app has one.
pub struct Item(pub MenuItem<tauri::Wry>);

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
    cfg!(target_os = "macos") || std::env::var_os("APPIMAGE").is_some()
}

pub fn start(app: AppHandle) {
    if !can_update() {
        eprintln!("illogical: updates come from the package manager here");
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        loop {
            match tauri::async_runtime::block_on(check(&app)) {
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

/// Download and put in place a newer release; its version. Also when the
/// daemon here is newer than this app knows (`compat.rs`).
pub async fn check(app: &AppHandle) -> Result<Option<String>, String> {
    let mut b = app.updater_builder();
    if let Some(u) = std::env::var("ILLOGICAL_UPDATE_URL").ok().filter(|u| !u.is_empty()) {
        b = b
            .endpoints(vec![u.parse().map_err(|e| format!("ILLOGICAL_UPDATE_URL: {e}"))?])
            .map_err(|e| e.to_string())?;
    }
    let Some(update) = b.build().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    eprintln!("illogical: updating the app from {} to {}", update.current_version, update.version);
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    eprintln!("illogical: {} is in place; it runs from the next start", update.version);
    Ok(Some(update.version))
}

fn ready(app: &AppHandle, version: &str) {
    if std::env::var_os("ILLOGICAL_UPDATE_RESTART").is_some() {
        app.restart();
    }
    if let Some(item) = app.try_state::<Item>() {
        let _ = item.0.set_text(format!("Restart to update to {version}"));
        let _ = item.0.set_enabled(true);
    }
}
