//! The bits of systemd the daemon uses without linking libsystemd. On
//! Windows there's no systemd: notifying does nothing and nothing is kept.

use std::process::Command;
#[cfg(unix)]
use std::{
    collections::HashMap,
    io::IoSlice,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
};

#[cfg(unix)]
use nix::{
    fcntl::{FcntlArg, FdFlag, fcntl},
    sys::socket::{AddressFamily, ControlMessage, MsgFlags, SockFlag, SockType, UnixAddr, sendmsg, socket},
};

/// What systemd tells the daemon's service about itself. Not for the
/// programs in its panes: a daemon started in one would take itself for the
/// service (scopes, the FD store).
pub const SERVICE_ENV: &[&str] = &[
    "NOTIFY_SOCKET",
    "LISTEN_FDS",
    "LISTEN_PID",
    "LISTEN_FDNAMES",
    "LISTEN_PIDFDID",
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "WATCHDOG_PID",
    "WATCHDOG_USEC",
    "MEMORY_PRESSURE_WATCH",
    "MEMORY_PRESSURE_WRITE",
    "SYSTEMD_EXEC_PID",
];

/// Tell systemd about the daemon's state (`READY=1`, `STOPPING=1`) when it
/// runs as a `Type=notify` service; a no-op otherwise.
pub fn notify(state: &str) {
    #[cfg(unix)]
    notify_with_fds(state, &[]);
    #[cfg(not(unix))]
    let _ = state;
}

/// Whether systemd is listening (a `Type=notify` service). Without it there
/// is no FD store, so panes can't outlive the daemon.
pub fn under_systemd() -> bool {
    std::env::var_os("NOTIFY_SOCKET").is_some()
}

#[cfg(unix)]
fn notify_with_fds(state: &str, fds: &[RawFd]) -> bool {
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else { return false };
    let path = path.to_string_lossy().into_owned();
    let addr = match path.strip_prefix('@') {
        #[cfg(target_os = "linux")]
        Some(name) => UnixAddr::new_abstract(name.as_bytes()),
        #[cfg(not(target_os = "linux"))]
        Some(_) => return false,
        None => UnixAddr::new(path.as_str()),
    };
    let Ok(addr) = addr else { return false };
    // systemd only exists on Linux, where the socket can be close-on-exec
    // from the start.
    #[cfg(target_os = "linux")]
    let flags = SockFlag::SOCK_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = SockFlag::empty();
    let Ok(sock) = socket(AddressFamily::Unix, SockType::Datagram, flags, None) else {
        return false;
    };
    let iov = [IoSlice::new(state.as_bytes())];
    let rights = [ControlMessage::ScmRights(fds)];
    let cmsgs: &[ControlMessage] = if fds.is_empty() { &[] } else { &rights };
    sendmsg(sock.as_raw_fd(), &iov, cmsgs, MsgFlags::empty(), Some(&addr)).is_ok()
}

/// Keep a pane's PTY master in systemd's FD store, so the terminal stays
/// open (and its programs running) while the daemon restarts. `FDPOLL=0`:
/// keep it even if it hangs up.
#[cfg(unix)]
pub fn store_fd(name: &str, fd: RawFd) -> bool {
    notify_with_fds(&format!("FDSTORE=1\nFDNAME={name}\nFDPOLL=0"), &[fd])
}

pub fn remove_fd(name: &str) {
    notify(&format!("FDSTOREREMOVE=1\nFDNAME={name}"));
}

/// Descriptors systemd handed back from the FD store (or socket
/// activation), by name. Each call after the first returns nothing: the
/// environment variables are cleared so children don't inherit them.
#[cfg(unix)]
pub fn take_listen_fds() -> HashMap<String, OwnedFd> {
    let mut out = HashMap::new();
    let ours = std::env::var("LISTEN_PID").ok().and_then(|p| p.parse::<u32>().ok()) == Some(std::process::id());
    let n: i32 = std::env::var("LISTEN_FDS").ok().and_then(|n| n.parse().ok()).unwrap_or(0);
    let names = std::env::var("LISTEN_FDNAMES").unwrap_or_default();
    // SAFETY: single-threaded at this point (called before the runtime
    // spawns anything that reads the environment).
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    if !ours {
        return out;
    }
    let names: Vec<&str> = names.split(':').collect();
    for i in 0..n {
        let fd = 3 + i;
        let _ = fcntl(unsafe { BorrowedFd::borrow_raw(fd) }, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC));
        // SAFETY: systemd passed these descriptors to us; we own them now.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        out.insert(names.get(i as usize).copied().unwrap_or("").to_owned(), owned);
    }
    out
}

/// The systemd user manager's environment, read fresh for each new pane.
///
/// At boot (lingering) the daemon starts before anyone logs in, so its own
/// environment lacks the graphical session's `WAYLAND_DISPLAY`, `DISPLAY` and
/// `SSH_AUTH_SOCK`. The session imports those into the manager when it
/// starts, so panes started afterwards get them.
pub fn manager_env() -> Vec<(String, String)> {
    let Ok(out) = Command::new("systemctl").args(["--user", "show-environment"]).output() else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(parse_line).collect()
}

/// `KEY=value` or `KEY=$'escaped value'`, as `systemctl show-environment`
/// prints them.
fn parse_line(line: &str) -> Option<(String, String)> {
    let (key, value) = line.split_once('=')?;
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }
    let value = match value.strip_prefix("$'").and_then(|v| v.strip_suffix('\'')) {
        Some(quoted) => unescape(quoted),
        None => value.to_owned(),
    };
    Some((key.to_owned(), value))
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_show_environment_lines() {
        assert_eq!(parse_line("WAYLAND_DISPLAY=wayland-0"), Some(("WAYLAND_DISPLAY".into(), "wayland-0".into())));
        assert_eq!(parse_line("A=b=c"), Some(("A".into(), "b=c".into())));
        assert_eq!(parse_line(r"X=$'two words\nand it\'s'"), Some(("X".into(), "two words\nand it's".into())));
        assert_eq!(parse_line("not a line"), None);
        assert_eq!(parse_line("BAD-KEY=x"), None);
    }
}
