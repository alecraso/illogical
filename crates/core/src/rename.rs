//! illogical is now Arugula (#509). Things made under the old name keep
//! working (#505): installs, panes started before the update, unit files,
//! hooks, and daemons or clients still on illogical 0.25 (#504, the bridge,
//! which accepts both names) or older (which know only the old ones).
//!
//! - **Headers:** read under either name ([`either`]). Send both where the
//!   other end may be older than 0.25 ([`AGENT`], [`PANE`],
//!   [`CLAUDE_CONFIG_DIR`], [`SIZE`], [`OFFSET`]); control is always current, so
//!   [`AUTH`] goes under the new name only.
//! - **Environment variables:** `ILLOGICAL_X` stands in for `ARUGULA_X`
//!   when only the old name is set ([`alias_env`]): a pane started before
//!   the update, or a unit file written by an older install.
//!
//! **Never renamed**, because something already stored or running depends
//! on the exact bytes. Each is marked "Frozen (#504)" where it's defined:
//!
//! - the domains signed or hashed into device certificates, join and team
//!   proofs, push and call tokens (`illogical device v1`, `illogical team
//!   invite v1`, …), the Noise prologue `illogical/1`, the sync key's HKDF
//!   info `illogical sync v1`, the key file header `illogical-device-key 1`
//!   and the checkpoint magic `ILLOGICAL-CKPT1`;
//! - the browser's IndexedDB `illogical-device`, which holds its device key;
//! - the names a running pane is found by after a daemon restart: the
//!   holder socket `illogical-hold-*`, Windows' pane pipe
//!   `illogical-<user>-pane-*`, and the systemd scopes `illogical-pane-*`
//!   and `illogical-agent-*`;
//! - the sandbox prefix `illogical-eph-` and the resident service name,
//!   which daemons of different versions on one account read from each
//!   other's sandboxes.

/// The old name of the product, for what was made under it.
pub const OLD: &str = "illogical";

/// `X-Arugula-Agent`: the request comes from an agent on the owner's CLI,
/// so it gets less than the owner would. Either name counts, and the CLI
/// sends both: a daemon older than 0.25 knows only the old one, and must
/// not take an agent for the owner.
pub const AGENT: [&str; 2] = ["x-arugula-agent", "x-illogical-agent"];
/// The pane an MCP client runs in.
pub const PANE: [&str; 2] = ["x-arugula-pane", "x-illogical-pane"];
/// The `CLAUDE_CONFIG_DIR` of the client that started `arugula mcp` (#379,
/// local socket only), so an agent it starts uses that client's login.
pub const CLAUDE_CONFIG_DIR: [&str; 2] = ["arugula-claude-config-dir", "illogical-claude-config-dir"];
/// A daemon's or CLI's signed request to control.
pub const AUTH: [&str; 2] = ["x-arugula-auth", "x-illogical-auth"];
/// A file's size (`/api/fs/read`).
pub const SIZE: [&str; 2] = ["x-arugula-size", "x-illogical-size"];
/// The offset of a ranged read (`/api/fs/read`).
pub const OFFSET: [&str; 2] = ["x-arugula-offset", "x-illogical-offset"];

/// The first of `names` that `get` finds: `either(AGENT, |n| headers.get(n))`.
pub fn either<T>(names: [&str; 2], get: impl FnMut(&str) -> Option<T>) -> Option<T> {
    names.into_iter().find_map(get)
}

/// `old` where only it exists, else `new`: a file or directory made under
/// the old name stays where it is, and anything new gets the new name.
pub fn kept(new: std::path::PathBuf, old: std::path::PathBuf) -> std::path::PathBuf {
    if !new.exists() && old.exists() { old } else { new }
}

/// For each `ILLOGICAL_X` whose `ARUGULA_X` isn't set: the new name and the
/// value, so the rest of the program (and its children) see it there.
pub fn env_aliases(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(String, std::ffi::OsString)> {
    let vars: Vec<_> = vars.into_iter().collect();
    let set = |k: &str| vars.iter().any(|(n, _)| n.to_str() == Some(k));
    vars.iter()
        .filter_map(|(k, v)| {
            let new = format!("ARUGULA_{}", k.to_str()?.strip_prefix("ILLOGICAL_")?);
            (!set(&new)).then(|| (new, v.clone()))
        })
        .collect()
}

/// `ILLOGICAL_X` for `ARUGULA_X`. Panes get the variables a script or an
/// older CLI reads under both names (`ARUGULA_PANE`, `ARUGULA_SOCK`, …),
/// and what the daemon takes out goes under both.
pub fn old_env(new: &str) -> String {
    format!("ILLOGICAL_{}", new.strip_prefix("ARUGULA_").unwrap_or(new))
}

/// `ARUGULA_X` under both names, for a pane's environment: `[(new, v),
/// (old, v)]`.
pub fn both_env(new: &str, v: impl Into<String>) -> [(String, String); 2] {
    let v = v.into();
    [(new.to_owned(), v.clone()), (old_env(new), v)]
}

/// Whether `k` is one of ours under either name (`ARUGULA_*`,
/// `ILLOGICAL_*`).
pub fn is_ours(k: &str) -> bool {
    k.starts_with("ARUGULA_") || k.starts_with("ILLOGICAL_")
}

/// Set `ARUGULA_X` from `ILLOGICAL_X` where only the old name is set.
///
/// # Safety
///
/// It changes the environment: call it first thing in `main`, before any
/// other thread exists.
pub unsafe fn alias_env() {
    for (k, v) in env_aliases(std::env::vars_os()) {
        // SAFETY: the caller promises there's only this thread.
        unsafe { std::env::set_var(k, v) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_is_the_old_path_only_while_it_alone_exists() {
        let d = std::env::temp_dir().join(format!("arugula-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (new, old) = (d.join("arugula"), d.join("illogical"));
        assert_eq!(kept(new.clone(), old.clone()), new, "neither: the new");
        std::fs::create_dir_all(&old).unwrap();
        assert_eq!(kept(new.clone(), old.clone()), old, "only the old");
        std::fs::create_dir_all(&new).unwrap();
        assert_eq!(kept(new.clone(), old.clone()), new, "both: the new");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn vars(v: &[(&str, &str)]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        v.iter().map(|(k, v)| (k.into(), v.into())).collect()
    }

    #[test]
    fn old_env_names_stand_in_for_new_ones() {
        let got = env_aliases(vars(&[
            ("ILLOGICAL_SOCK", "/old"),
            ("ILLOGICAL_PANE", "%3"),
            ("ARUGULA_PANE", "%1"),
            ("ILLOGICAL", "x"),
            ("PATH", "/bin"),
        ]));
        // The new name wins when both are set; a bare ILLOGICAL isn't ours.
        assert_eq!(got, vec![("ARUGULA_SOCK".to_string(), "/old".into())]);
    }

    #[test]
    fn old_env_names() {
        assert_eq!(old_env("ARUGULA_PANE"), "ILLOGICAL_PANE");
        assert_eq!(
            both_env("ARUGULA_SOCK", "/s"),
            [("ARUGULA_SOCK".into(), "/s".into()), ("ILLOGICAL_SOCK".into(), "/s".into())]
        );
        assert!(is_ours("ILLOGICAL_EXEC") && is_ours("ARUGULA_EXEC") && !is_ours("ILLOGICAL"));
    }

    #[test]
    fn either_takes_the_first_name_found() {
        let headers = [("x-illogical-agent", "1")];
        let get = |n: &str| headers.iter().find(|(k, _)| *k == n).map(|(_, v)| *v);
        assert_eq!(either(AGENT, get), Some("1"));
        assert_eq!(either(PANE, get), None);
        let both = [("x-illogical-pane", "1"), ("x-arugula-pane", "2")];
        assert_eq!(either(PANE, |n| both.iter().find(|(k, _)| *k == n).map(|(_, v)| *v)), Some("2"));
    }
}
