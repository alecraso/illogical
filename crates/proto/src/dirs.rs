//! Where Arugula keeps its files on this machine: the state directory
//! (`~/.local/state/arugula`, Windows `%LOCALAPPDATA%\arugula\state`) and
//! the config directory (`~/.config/arugula`). The daemon, the CLI and the
//! desktop app all ask here, so they agree.
//!
//! A machine set up as illogical has them under the old name, and they stay
//! there (#505): pane records hold their paths, holder sockets are named by
//! a hash of them, and running shims were started with them. Moving the
//! directory would strand those panes; using it in place never does. Beside
//! it goes a link under the new name, so people (and docs) find it there.

use std::path::{Path, PathBuf};

use crate::rename::OLD;

const NEW: &str = "arugula";

/// `new`, unless only `old` exists: then `old`, as it is, with a link
/// `new → old` made beside it (Unix) so it's found under the new name too.
/// A link that's already there counts as `old`.
pub fn renamed(new: &Path, old: &Path) -> PathBuf {
    if let Ok(m) = std::fs::symlink_metadata(new) {
        if !m.file_type().is_symlink() {
            return new.to_owned();
        }
        // The link this made (or one that leads to the same place): the
        // path that's in use is the old one.
        let same = |a: &Path, b: &Path| a.canonicalize().ok().is_some_and(|a| b.canonicalize().ok() == Some(a));
        if same(new, old) {
            return old.to_owned();
        }
        // A link to the old directory that's gone (it was deleted to start
        // again): out of the way, so the new one can be made.
        if !new.exists() && std::fs::read_link(new).is_ok_and(|t| t == old || Some(t.as_os_str()) == old.file_name()) {
            let _ = std::fs::remove_file(new);
        }
        return new.to_owned();
    }
    if !old.is_dir() {
        return new.to_owned();
    }
    #[cfg(unix)]
    if let Some(name) = old.file_name().filter(|_| old.parent() == new.parent()) {
        // Relative, so it still holds if the home directory moves. Two at
        // once is fine: one makes it, the other finds it made.
        let _ = std::os::unix::fs::symlink(name, new);
    }
    old.to_owned()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// `$VAR`, else `~/REL`.
fn xdg(var: &str, rel: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| Some(home()?.join(rel)))
}

/// `base/<new>`, or `base/<old>` where only that exists.
pub fn named_in(base: &Path, new: &str, old: &str) -> PathBuf {
    renamed(&base.join(new), &base.join(old))
}

/// The daemon's state directory when nothing says otherwise:
/// `%LOCALAPPDATA%\arugula\state` on Windows, else
/// `$XDG_STATE_HOME/arugula` or `~/.local/state/arugula` (or the
/// `illogical` one a machine already has).
pub fn default_state_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()).map(PathBuf::from) {
        // `%LOCALAPPDATA%\arugula` is also where the desktop app installs
        // itself, so it's the `state` folders that are compared.
        return Some(renamed(&local.join(NEW).join("state"), &local.join(OLD).join("state")));
    }
    state_home(NEW, OLD)
}

/// `ARUGULA_STATE_DIR`, else [`default_state_dir`].
pub fn state_dir() -> Option<PathBuf> {
    std::env::var_os("ARUGULA_STATE_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(default_state_dir)
}

/// `$XDG_STATE_HOME/<new>` (or `~/.local/state/<new>`), or `<old>` there
/// where only that exists: the sandbox's own directory, say.
pub fn state_home(new: &str, old: &str) -> Option<PathBuf> {
    Some(named_in(&xdg("XDG_STATE_HOME", ".local/state")?, new, old))
}

/// `$XDG_CONFIG_HOME/arugula` or `~/.config/arugula` (or the `illogical`
/// one a machine already has): keys, tokens, the CLI's login.
pub fn config_dir() -> Option<PathBuf> {
    Some(named_in(&xdg("XDG_CONFIG_HOME", ".config")?, NEW, OLD))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arugula-dirs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_the_old_one_is_kept_and_linked() {
        let d = scratch("old");
        std::fs::create_dir(d.join("illogical")).unwrap();
        std::fs::write(d.join("illogical/layout.json"), "{}").unwrap();
        assert_eq!(named_in(&d, "arugula", "illogical"), d.join("illogical"));
        // Not moved: still there under the old name.
        assert!(d.join("illogical/layout.json").is_file());
        #[cfg(unix)]
        {
            assert_eq!(std::fs::read_link(d.join("arugula")).unwrap(), Path::new("illogical"));
            assert!(d.join("arugula/layout.json").is_file());
            // Asked again, with the link there: the old path still.
            assert_eq!(named_in(&d, "arugula", "illogical"), d.join("illogical"));
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn only_the_new_one() {
        let d = scratch("new");
        std::fs::create_dir(d.join("arugula")).unwrap();
        assert_eq!(named_in(&d, "arugula", "illogical"), d.join("arugula"));
        assert!(!d.join("illogical").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn both_is_the_new_one() {
        let d = scratch("both");
        std::fs::create_dir(d.join("arugula")).unwrap();
        std::fs::create_dir(d.join("illogical")).unwrap();
        assert_eq!(named_in(&d, "arugula", "illogical"), d.join("arugula"));
        assert!(!std::fs::symlink_metadata(d.join("arugula")).unwrap().file_type().is_symlink());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn neither_is_the_new_one_and_nothing_is_made() {
        let d = scratch("neither");
        assert_eq!(named_in(&d, "arugula", "illogical"), d.join("arugula"));
        assert!(!d.join("arugula").exists() && !d.join("illogical").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_left_after_the_old_one_went_is_cleared() {
        let d = scratch("dangling");
        std::os::unix::fs::symlink("illogical", d.join("arugula")).unwrap();
        assert_eq!(named_in(&d, "arugula", "illogical"), d.join("arugula"));
        assert!(std::fs::symlink_metadata(d.join("arugula")).is_err(), "the dangling link stays");
        std::fs::create_dir_all(d.join("arugula")).unwrap();
        // Someone's own link elsewhere is theirs: used, not touched.
        let mine = scratch("mine");
        let d2 = scratch("linked");
        std::os::unix::fs::symlink(&mine, d2.join("arugula")).unwrap();
        assert_eq!(named_in(&d2, "arugula", "illogical"), d2.join("arugula"));
        assert!(d2.join("arugula").exists());
        for x in [d, mine, d2] {
            std::fs::remove_dir_all(x).unwrap();
        }
    }
}
