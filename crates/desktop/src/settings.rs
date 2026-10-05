//! The app's own settings (`desktop.json` in its config directory), and
//! the global hotkey they turn on.
//!
//! The hotkey is off by default (M46). The tray's *Global hotkey* item
//! turns it on or off; `hotkey` in the file picks the keys
//! (`Ctrl+Alt+Space` unless set). Pressed, it brings illogical to the
//! front, or hides it when one of its windows has focus.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+Space";

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Settings {
    /// The global hotkey is on.
    #[serde(default)]
    pub hotkey_on: bool,
    /// Its keys, in the global-shortcut plugin's syntax.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
}

impl Settings {
    pub fn keys(&self) -> &str {
        self.hotkey.as_deref().filter(|k| !k.trim().is_empty()).unwrap_or(DEFAULT_HOTKEY)
    }
}

fn file(app: &AppHandle) -> Option<PathBuf> {
    if let Some(f) = std::env::var_os("ILLOGICAL_DESKTOP_SETTINGS") {
        return Some(f.into());
    }
    Some(app.path().app_config_dir().ok()?.join("desktop.json"))
}

pub fn load(app: &AppHandle) -> Settings {
    file(app)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(app: &AppHandle, s: &Settings) {
    let Some(f) = file(app) else { return };
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&f, serde_json::to_string_pretty(s).unwrap()) {
        eprintln!("illogical: saving {}: {e}", f.display());
    }
}

/// The plugin, with what a press does.
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                toggle(app);
            }
        })
        .build()
}

/// Register the hotkey when it's on. Says why not when it can't be had.
pub fn apply(app: &AppHandle, s: &Settings) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if !s.hotkey_on {
        return Ok(());
    }
    gs.register(s.keys()).map_err(|e| format!("the global hotkey {}: {e}", s.keys()))?;
    eprintln!("illogical: global hotkey {}", s.keys());
    Ok(())
}

static LAST_BLUR: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// A window of ours lost focus (or gained it: `focused`).
pub fn focus_changed(focused: bool) {
    *LAST_BLUR.lock().unwrap() = (!focused).then(std::time::Instant::now);
}

/// Front, or away when a window of ours already has focus.
fn toggle(app: &AppHandle) {
    let windows = app.webview_windows();
    // On X11 the key's grab takes focus from the window just before the
    // press arrives, so a window that had it a moment ago still counts.
    let just = LAST_BLUR.lock().unwrap().is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(500));
    let focused =
        windows.values().any(|w| (w.is_focused().unwrap_or(false) || just) && w.is_visible().unwrap_or(false));
    if focused {
        #[cfg(target_os = "macos")]
        {
            let _ = app.hide();
        }
        #[cfg(not(target_os = "macos"))]
        for w in windows.values() {
            let _ = w.hide();
        }
        return;
    }
    #[cfg(target_os = "macos")]
    let _ = app.show();
    let mut shown = false;
    for w in windows.values() {
        let _ = w.unminimize();
        let _ = w.show();
        shown = true;
    }
    match windows.values().next() {
        Some(w) if shown => {
            let _ = w.set_focus();
        }
        _ => crate::focus_or_open(app),
    }
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn off_by_default_with_a_default_key() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.hotkey_on);
        assert_eq!(s.keys(), super::DEFAULT_HOTKEY);
        let s: Settings = serde_json::from_str(r#"{"hotkey_on": true, "hotkey": "Super+F12"}"#).unwrap();
        assert!(s.hotkey_on);
        assert_eq!(s.keys(), "Super+F12");
    }
}
