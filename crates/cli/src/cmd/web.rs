//! `arugula web`: this machine's page in your browser, signed in.

use crate::http::{self, request};
use anyhow::bail;

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    pub print: bool,
}

/// `arugula web`: the sign-in link, opened in a browser (or printed).
/// With `--ssh`, the box daemon's link: printed, since its page is on the
/// box, with how to forward its port from here.
pub fn web(sock: &http::Target, print: bool) -> anyhow::Result<i32> {
    let v = request(sock, "GET", "/api/signin-link", None)?.json()?;
    let Some(url) = v["url"].as_str() else { bail!("the daemon has no sign-in link: {v}") };
    let page = url.split("/auth?").next().unwrap_or(url);
    if print {
        println!("{url}");
        return Ok(0);
    }
    if let http::Target::Ssh(r) = sock {
        println!("The page is on {}. Open this in a browser here once its port is forwarded:\n\n  {url}\n", r.dest);
        if let Some(hint) = ssh_forward(page, &r.dest) {
            println!("{hint}\n");
        }
        println!("It holds {}'s local token: don't share it.", r.dest);
        return Ok(0);
    }
    // Windows: the URL handler itself (`cmd /c start` would split at `&`).
    let (opener, pre): (&str, &[&str]) = if cfg!(windows) {
        ("rundll32.exe", &["url.dll,FileProtocolHandler"])
    } else if cfg!(target_os = "macos") {
        ("open", &[])
    } else {
        ("xdg-open", &[])
    };
    let opened = (cfg!(any(target_os = "macos", windows))
        || std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some())
        && std::process::Command::new(opener)
            .args(pre)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    if opened {
        println!("Opened {page} in your browser, signed in.");
    } else {
        println!("Open this in a browser on this machine (it signs the browser in, once):\n\n  {url}\n");
        if let Some(hint) = ssh_hint(page, std::env::var_os("SSH_CONNECTION").is_some()) {
            println!("{hint}\n");
        }
        println!("It holds this machine's local token: don't share it.");
    }
    Ok(0)
}

/// Over ssh there's no browser on this machine: forward its port from
/// the computer you're at, and the same link works there.
fn ssh_hint(page: &str, over_ssh: bool) -> Option<String> {
    if !over_ssh {
        return None;
    }
    let (port, addr) = page_port(page)?;
    Some(format!(
        "Over ssh? On the computer you're at, forward the port first, then open the link there:\n\n  ssh -L {port}:{addr} <this machine>"
    ))
}

/// `--ssh dest web`: the forward that makes dest's page reachable here.
fn ssh_forward(page: &str, dest: &str) -> Option<String> {
    let (port, addr) = page_port(page)?;
    Some(format!("Forward it with:\n\n  ssh -N -L {port}:{addr} {dest}"))
}

/// A page's port and address (`http://127.0.0.1:7681/` is 7681 and
/// 127.0.0.1:7681).
fn page_port(page: &str) -> Option<(&str, &str)> {
    let addr = page.strip_prefix("http://")?.trim_end_matches('/');
    Some((addr.rsplit_once(':')?.1, addr))
}
#[cfg(test)]
mod tests {
    #[test]
    fn web_link_over_ssh() {
        assert_eq!(super::ssh_hint("http://127.0.0.1:7681", false), None);
        let hint = super::ssh_hint("http://127.0.0.1:7681", true).unwrap();
        assert!(hint.ends_with("ssh -L 7681:127.0.0.1:7681 <this machine>"), "{hint}");
    }

    #[test]
    fn web_forward_for_an_ssh_box() {
        let hint = super::ssh_forward("http://127.0.0.1:7681/", "box").unwrap();
        assert!(hint.ends_with("ssh -N -L 7681:127.0.0.1:7681 box"), "{hint}");
        assert_eq!(super::ssh_forward("https://x", "box"), None);
    }
}
