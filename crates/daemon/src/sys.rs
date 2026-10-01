//! The bits of systemd the daemon uses without linking libsystemd.

use std::{
    os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr, UnixDatagram},
    },
    process::Command,
};

/// Tell systemd about the daemon's state (`READY=1`, `STOPPING=1`) when it
/// runs as a `Type=notify` service; a no-op otherwise.
pub fn notify(state: &str) {
    let Some(path) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let path = path.to_string_lossy();
    let addr = match path.strip_prefix('@') {
        Some(name) => SocketAddr::from_abstract_name(name.as_bytes()),
        None => SocketAddr::from_pathname(path.as_ref()),
    };
    if let (Ok(sock), Ok(addr)) = (UnixDatagram::unbound(), addr) {
        let _ = sock.send_to_addr(state.as_bytes(), &addr);
    }
}

/// The systemd user manager's environment, read fresh for each new pane.
///
/// At boot (lingering) the daemon starts before anyone logs in, so its own
/// environment lacks the graphical session's `WAYLAND_DISPLAY`, `DISPLAY` and
/// `SSH_AUTH_SOCK`. The session imports those into the manager when it
/// starts, so panes started afterwards get them.
pub fn manager_env() -> Vec<(String, String)> {
    let Ok(out) = Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
    else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_line)
        .collect()
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
        assert_eq!(
            parse_line("WAYLAND_DISPLAY=wayland-0"),
            Some(("WAYLAND_DISPLAY".into(), "wayland-0".into()))
        );
        assert_eq!(parse_line("A=b=c"), Some(("A".into(), "b=c".into())));
        assert_eq!(
            parse_line(r"X=$'two words\nand it\'s'"),
            Some(("X".into(), "two words\nand it's".into()))
        );
        assert_eq!(parse_line("not a line"), None);
        assert_eq!(parse_line("BAD-KEY=x"), None);
    }
}
