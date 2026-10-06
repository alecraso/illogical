//! illogical is becoming Arugula (#509). This release (#504) is the bridge:
//! it still calls itself illogical, but wherever a name crosses from one
//! version to another, it accepts the new name as well as the old one. A
//! renamed CLI or app then works against a daemon that hasn't been updated
//! yet, and a renamed release can be installed by this one's updater.
//!
//! - **Headers** a client sends and a daemon or control reads: the old name
//!   first, then the new ([`either`]).
//! - **Environment variables:** `ARUGULA_X` stands in for `ILLOGICAL_X`
//!   when only the new name is set ([`alias_env`]).
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

/// `X-Illogical-Agent`: the request comes from an agent on the owner's CLI,
/// so it gets less than the owner would. Either name counts.
pub const AGENT: [&str; 2] = ["x-illogical-agent", "x-arugula-agent"];
/// The pane an MCP client runs in.
pub const PANE: [&str; 2] = ["x-illogical-pane", "x-arugula-pane"];
/// The Claude Code config directory of the agent calling MCP.
pub const CLAUDE_CONFIG_DIR: [&str; 2] = ["illogical-claude-config-dir", "arugula-claude-config-dir"];
/// A daemon's or CLI's signed request to control.
pub const AUTH: [&str; 2] = ["x-illogical-auth", "x-arugula-auth"];
/// A file's size and the offset of a ranged read (`/api/fs/read`).
pub const SIZE: [&str; 2] = ["x-illogical-size", "x-arugula-size"];

/// The first of `names` that `get` finds: `either(AGENT, |n| headers.get(n))`.
pub fn either<T>(names: [&str; 2], get: impl FnMut(&str) -> Option<T>) -> Option<T> {
    names.into_iter().find_map(get)
}

/// For each `ARUGULA_X` whose `ILLOGICAL_X` isn't set: the old name and the
/// value, so the rest of the program (and its children) see it there.
pub fn env_aliases(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(String, std::ffi::OsString)> {
    let vars: Vec<_> = vars.into_iter().collect();
    let set = |k: &str| vars.iter().any(|(n, _)| n.to_str() == Some(k));
    vars.iter()
        .filter_map(|(k, v)| {
            let old = format!("ILLOGICAL_{}", k.to_str()?.strip_prefix("ARUGULA_")?);
            (!set(&old)).then(|| (old, v.clone()))
        })
        .collect()
}

/// Set `ILLOGICAL_X` from `ARUGULA_X` where only the new name is set.
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

    fn vars(v: &[(&str, &str)]) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
        v.iter().map(|(k, v)| (k.into(), v.into())).collect()
    }

    #[test]
    fn new_env_names_stand_in_for_old_ones() {
        let got = env_aliases(vars(&[
            ("ARUGULA_SOCK", "/new"),
            ("ARUGULA_PANE", "%3"),
            ("ILLOGICAL_PANE", "%1"),
            ("ARUGULA", "x"),
            ("PATH", "/bin"),
        ]));
        // The old name wins when both are set; a bare ARUGULA isn't ours.
        assert_eq!(got, vec![("ILLOGICAL_SOCK".to_string(), "/new".into())]);
    }

    #[test]
    fn either_takes_the_first_name_found() {
        let headers = [("x-arugula-agent", "1")];
        let get = |n: &str| headers.iter().find(|(k, _)| *k == n).map(|(_, v)| *v);
        assert_eq!(either(AGENT, get), Some("1"));
        assert_eq!(either(PANE, get), None);
        let both = [("x-illogical-pane", "1"), ("x-arugula-pane", "2")];
        assert_eq!(either(PANE, |n| both.iter().find(|(k, _)| *k == n).map(|(_, v)| *v)), Some("1"));
    }
}
