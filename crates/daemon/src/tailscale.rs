//! tailscaled's local API (the Unix socket the `tailscale` CLI talks to):
//! what this node is called, and who is on the other end of a connection.
//!
//! WhoIs is how the daemon knows who is connecting when nothing in front of
//! it says so: a direct connection to a tailnet address, or, under
//! `--tun=userspace-networking` (sandboxes), a connection tailscaled's
//! netstack forwarded to loopback. That second case matters: without it,
//! anyone the tailnet ACL lets reach the sandbox would look like a local
//! process.
//!
//! The Tailscale app on macOS has no such socket (its API is on a loopback
//! port with a token); there the app's CLI is asked instead, which prints
//! the same JSON.

use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, bail};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::access::Peer;

/// Where tailscaled listens unless told otherwise (`--socket`).
const DEFAULT_SOCKETS: [&str; 2] = ["/run/tailscale/tailscaled.sock", "/var/run/tailscale/tailscaled.sock"];

#[derive(Debug, Clone)]
pub struct LocalApi {
    via: Via,
}

#[derive(Debug, Clone)]
enum Via {
    Socket(PathBuf),
    /// The `tailscale` CLI (the macOS app's).
    Cli(PathBuf),
}

/// The macOS app's CLI, wherever the app is.
const APP_CLI: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";

/// What Tailscale says about this machine.
#[derive(Debug, Clone)]
pub struct Status {
    /// MagicDNS name, e.g. `geek.tail1234.ts.net`.
    pub host: String,
    /// The login that owns this node; `None` for a tagged node (a sandbox),
    /// which no user owns.
    pub login: Option<String>,
    /// tailscaled runs in userspace (netstack) mode: tailnet connections to
    /// a port with no `serve` behind it are forwarded to loopback.
    pub userspace: bool,
    pub ips: Vec<IpAddr>,
}

impl LocalApi {
    /// The given socket, else tailscaled's default one if it exists, else
    /// (macOS) the Tailscale app's CLI.
    pub fn find(explicit: Option<&Path>) -> Option<Self> {
        let socket = match explicit {
            Some(p) => Some(p.to_owned()),
            None => DEFAULT_SOCKETS.iter().map(Path::new).find(|p| p.exists()).map(Path::to_owned),
        };
        if let Some(socket) = socket {
            return Some(Self { via: Via::Socket(socket) });
        }
        if !cfg!(target_os = "macos") {
            return None;
        }
        Path::new(APP_CLI).exists().then(|| Self { via: Via::Cli(PathBuf::from(APP_CLI)) })
    }

    /// `tailscale ARGS…`: its stdout on success, else what it said.
    async fn cli(&self, cli: &Path, args: &[&str]) -> anyhow::Result<Result<Vec<u8>, String>> {
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            // The app's binary acts as the CLI only when told so; started
            // from a service (launchd) it tries to open the GUI instead.
            tokio::process::Command::new(cli).args(args).env("TAILSCALE_BE_CLI", "1").kill_on_drop(true).output(),
        )
        .await
        .context("tailscale didn't answer")?
        .with_context(|| format!("running {}", cli.display()))?;
        Ok(if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        })
    }

    /// One GET; the status and body. The local API wants exactly this Host
    /// (its own defence against DNS rebinding).
    async fn get(&self, path: &str) -> anyhow::Result<(u16, Vec<u8>)> {
        let go = async {
            let Via::Socket(socket) = &self.via else { bail!("no tailscaled socket") };
            let mut s = tokio::net::UnixStream::connect(socket)
                .await
                .with_context(|| format!("connecting to {}", socket.display()))?;
            let req = format!("GET {path} HTTP/1.0\r\nHost: local-tailscaled.sock\r\nConnection: close\r\n\r\n");
            s.write_all(req.as_bytes()).await?;
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await?;
            parse_response(&buf)
        };
        tokio::time::timeout(Duration::from_secs(3), go).await.context("tailscaled didn't answer")?
    }

    async fn status_json(&self) -> anyhow::Result<Value> {
        if let Via::Cli(cli) = &self.via {
            let out =
                self.cli(cli, &["status", "--json"]).await?.map_err(|e| anyhow::anyhow!("tailscale status: {e}"))?;
            return Ok(serde_json::from_slice(&out)?);
        }
        let (code, body) = self.get("/localapi/v0/status").await?;
        if code != 200 {
            bail!("tailscaled status: HTTP {code}");
        }
        Ok(serde_json::from_slice(&body)?)
    }

    pub async fn status(&self) -> anyhow::Result<Status> {
        parse_status(&self.status_json().await?).context("tailscaled isn't logged in")
    }

    /// The status once tailscaled has settled: while it says it is still
    /// starting (it was started beside us, as in a sandbox after a reboot),
    /// wait up to `within`.
    pub async fn settled_status(&self, within: Duration) -> anyhow::Result<Status> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let v = self.status_json().await?;
            let starting = matches!(v.get("BackendState").and_then(Value::as_str), Some("NoState" | "Starting"));
            if !starting || tokio::time::Instant::now() > deadline {
                return parse_status(&v).context("tailscaled isn't logged in");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// `Running` once logged in and connected; `NeedsLogin`, `Starting`, …
    pub async fn backend_state(&self) -> anyhow::Result<String> {
        Ok(self.status_json().await?.get("BackendState").and_then(Value::as_str).unwrap_or_default().to_owned())
    }

    /// Who is at `addr` (the far end of a TCP connection we accepted), or
    /// `None` if tailscaled doesn't know it: then it isn't a tailnet
    /// connection.
    pub async fn whois(&self, addr: SocketAddr) -> anyhow::Result<Option<Peer>> {
        if let Via::Cli(cli) = &self.via {
            return match self.cli(cli, &["whois", "--json", &addr.to_string()]).await? {
                Ok(out) => Ok(Some(parse_whois(&serde_json::from_slice(&out)?))),
                Err(e) if e.contains("not found") => Ok(None),
                Err(e) => bail!("tailscale whois: {e}"),
            };
        }
        let (code, body) = self.get(&format!("/localapi/v0/whois?proto=tcp&addr={addr}")).await?;
        match code {
            200 => Ok(Some(parse_whois(&serde_json::from_slice(&body)?))),
            404 => Ok(None),
            _ => bail!("tailscaled whois: HTTP {code}"),
        }
    }
}

fn parse_response(buf: &[u8]) -> anyhow::Result<(u16, Vec<u8>)> {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").context("truncated response")?;
    let head = std::str::from_utf8(&buf[..split])?;
    let code = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).context("bad status line")?;
    Ok((code, buf[split + 4..].to_vec()))
}

fn parse_status(v: &Value) -> Option<Status> {
    let me = v.get("Self")?;
    let host = me.get("DNSName")?.as_str()?.trim_end_matches('.').to_owned();
    if host.is_empty() {
        return None;
    }
    let tagged = me.get("Tags").and_then(Value::as_array).is_some_and(|t| !t.is_empty());
    let login = me
        .get("UserID")
        .and_then(|id| v.get("User")?.get(id.to_string())?.get("LoginName")?.as_str())
        .filter(|_| !tagged)
        .map(str::to_owned);
    let ips = me
        .get("TailscaleIPs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|ip| ip.as_str()?.parse().ok()).collect())
        .unwrap_or_default();
    // `TUN` is false when tailscaled uses its netstack instead of a device.
    let userspace = v.get("TUN").and_then(Value::as_bool) == Some(false);
    Some(Status { host, login, userspace, ips })
}

/// A tagged node is owned by its tags, not a person: it has no login here,
/// whatever placeholder ("tagged-devices") the profile carries.
fn parse_whois(v: &Value) -> Peer {
    let tagged = v.pointer("/Node/Tags").and_then(Value::as_array).is_some_and(|t| !t.is_empty());
    let login = v.pointer("/UserProfile/LoginName").and_then(Value::as_str).filter(|_| !tagged).map(str::to_owned);
    Peer::Tailnet { login }
}

/// Decides who a TCP peer is, per connection.
#[derive(Debug, Clone, Default)]
pub struct Identify {
    api: Option<LocalApi>,
    /// Ask about loopback peers too (tailscaled in userspace mode).
    loopback: bool,
}

impl Identify {
    pub fn new(api: Option<LocalApi>, userspace: bool) -> Self {
        Self { api, loopback: userspace }
    }

    pub async fn peer(&self, addr: SocketAddr) -> Peer {
        let local = addr.ip().is_loopback() || addr.ip().to_canonical().is_loopback();
        if local && !self.loopback {
            return Peer::Local;
        }
        let Some(api) = &self.api else {
            return if local { Peer::Local } else { Peer::Other };
        };
        match api.whois(addr).await {
            Ok(Some(peer)) => peer,
            Ok(None) if local => Peer::Local,
            Ok(None) => Peer::Other,
            Err(e) => {
                // Fail closed: a forwarded connection we can't place must
                // not pass as a local one.
                tracing::warn!(error = %e, %addr, "can't ask tailscaled who is connecting");
                Peer::Other
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_of_a_user_node_and_a_tagged_one() {
        let user = json!({
            "TUN": true,
            "Self": {"DNSName": "geek.example.ts.net.", "UserID": 7, "TailscaleIPs": ["100.1.2.3", "fd7a::1"]},
            "User": {"7": {"LoginName": "me@x.com"}}
        });
        let s = parse_status(&user).unwrap();
        assert_eq!(s.host, "geek.example.ts.net");
        assert_eq!(s.login.as_deref(), Some("me@x.com"));
        assert!(!s.userspace);
        assert_eq!(s.ips.len(), 2);

        let tagged = json!({
            "TUN": false,
            "Self": {"DNSName": "box.example.ts.net.", "UserID": 9, "Tags": ["tag:sandbox"]},
            "User": {"9": {"LoginName": "tagged-devices"}}
        });
        let s = parse_status(&tagged).unwrap();
        assert_eq!(s.login, None, "a tagged node has no owner to default to");
        assert!(s.userspace);
        assert!(parse_status(&json!({"Self": {"DNSName": ""}})).is_none());
    }

    #[test]
    fn whois_of_a_tagged_node_has_no_login() {
        let user = json!({"Node": {"Name": "phone."}, "UserProfile": {"LoginName": "me@x.com"}});
        assert_eq!(parse_whois(&user), Peer::Tailnet { login: Some("me@x.com".into()) });
        let tagged = json!({"Node": {"Tags": ["tag:k8s"]}, "UserProfile": {"LoginName": "tagged-devices"}});
        assert_eq!(parse_whois(&tagged), Peer::Tailnet { login: None });
    }

    #[test]
    fn responses_split_at_the_blank_line() {
        let (code, body) = parse_response(b"HTTP/1.0 404 Not Found\r\nA: b\r\n\r\nno match").unwrap();
        assert_eq!((code, body.as_slice()), (404, &b"no match"[..]));
        assert!(parse_response(b"HTTP/1.0 200 OK\r\n").is_err());
    }

    #[tokio::test]
    async fn without_tailscaled_loopback_is_local_and_the_rest_refused() {
        let id = Identify::new(None, false);
        assert_eq!(id.peer("127.0.0.1:5000".parse().unwrap()).await, Peer::Local);
        assert_eq!(id.peer("[::1]:5000".parse().unwrap()).await, Peer::Local);
        assert_eq!(id.peer("100.64.0.9:5000".parse().unwrap()).await, Peer::Other);
        assert_eq!(id.peer("192.168.1.5:5000".parse().unwrap()).await, Peer::Other);
    }

    #[tokio::test]
    async fn userspace_mode_fails_closed_when_tailscaled_is_unreachable() {
        let api = LocalApi::find(Some(Path::new("/nonexistent/tailscaled.sock")));
        let id = Identify::new(api, true);
        assert_eq!(id.peer("127.0.0.1:5000".parse().unwrap()).await, Peer::Other);
        // Kernel mode never asks about loopback.
        let api = LocalApi::find(Some(Path::new("/nonexistent/tailscaled.sock")));
        assert_eq!(Identify::new(api, false).peer("127.0.0.1:5000".parse().unwrap()).await, Peer::Local);
    }
}
