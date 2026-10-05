//! Linux: never open a WebKit profile that a newer WebKitGTK wrote.
//!
//! The .deb uses the system's WebKitGTK and the AppImage carries Ubuntu
//! 22.04's, and both keep the window's cookies, IndexedDB and service
//! workers in the same place (`~/.local/share/wtf.widgets.illogical`).
//! WebKitGTK upgrades a profile's storage as it opens it, and an older one
//! can't read the result: after the .deb ran on WebKitGTK 2.52, the
//! AppImage's 2.50 fails to open control's IndexedDB ("Unable to establish
//! IDB database file"), where the device keys live, so control's page
//! breaks on every launch. The local daemon's page keeps no IndexedDB and
//! still worked.
//!
//! So the profile records the newest WebKitGTK that opened it
//! (`webkit-version`), and an older WebKitGTK gets a profile of its own
//! beside it (`webkit-2.50/`), signed in separately. Going forward (a system
//! update, a newer AppImage) keeps the profile.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use tauri::{AppHandle, Manager};

static PROFILE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Decide once, at launch, before the first window.
pub fn init(app: &AppHandle) {
    PROFILE.get_or_init(|| {
        let base = app.path().app_data_dir().ok()?;
        let dir = choose(&base, webkit_version()?);
        if let Some(d) = &dir {
            eprintln!("illogical: {} was opened by a newer WebKitGTK; using {}", base.display(), d.display());
        }
        dir
    });
}

/// The window's own profile, when it can't use the default one.
pub fn dir() -> Option<PathBuf> {
    PROFILE.get().cloned().flatten()
}

#[cfg(target_os = "linux")]
fn webkit_version() -> Option<(u32, u32)> {
    // SAFETY: plain getters of the loaded library's version.
    unsafe { Some((webkit2gtk_sys::webkit_get_major_version(), webkit2gtk_sys::webkit_get_minor_version())) }
}

#[cfg(not(target_os = "linux"))]
fn webkit_version() -> Option<(u32, u32)> {
    None
}

fn parse(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.trim().split_once('.')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// None: the default profile in `base` (recording `running` there).
/// Some: the profile for a WebKitGTK older than the one that last opened
/// `base`.
fn choose(base: &Path, running: (u32, u32)) -> Option<PathBuf> {
    let marker = base.join("webkit-version");
    let seen = std::fs::read_to_string(&marker).ok().as_deref().and_then(parse);
    if seen.is_some_and(|s| s > running) {
        return Some(base.join(format!("webkit-{}.{}", running.0, running.1)));
    }
    if seen != Some(running) {
        let _ = std::fs::create_dir_all(base);
        let _ = std::fs::write(&marker, format!("{}.{}\n", running.0, running.1));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::choose;

    #[test]
    fn older_webkit_gets_its_own_profile() {
        let base = std::env::temp_dir().join(format!("illogical-profile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // First launch records its version; the same or a newer one keeps the profile.
        assert_eq!(choose(&base, (2, 50)), None);
        assert_eq!(choose(&base, (2, 50)), None);
        assert_eq!(choose(&base, (2, 52)), None);
        assert_eq!(std::fs::read_to_string(base.join("webkit-version")).unwrap(), "2.52\n");
        // An older one after that: a profile of its own, and the record stays.
        assert_eq!(choose(&base, (2, 50)), Some(base.join("webkit-2.50")));
        assert_eq!(std::fs::read_to_string(base.join("webkit-version")).unwrap(), "2.52\n");
        assert_eq!(choose(&base, (2, 52)), None);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
