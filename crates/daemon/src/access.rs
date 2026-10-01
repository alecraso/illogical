//! Who may talk to the daemon.
//!
//! The daemon listens on loopback only; the tailnet reaches it through
//! `tailscale serve`, which adds `Tailscale-User-Login` and strips any copy a
//! client sends. So:
//!
//! - **Host** must be a loopback name or the tailnet name. This stops DNS
//!   rebinding: a web page that rebinds its own name to 127.0.0.1 still sends
//!   its own name as Host.
//! - **Origin**, when a browser sends one, must be one of those hosts too.
//!   This stops any other page in a local browser from opening a WebSocket to
//!   127.0.0.1 (cross-site WebSocket hijacking).
//! - **Tailscale-User-Login**, when present, must be the owner. It is absent
//!   for local requests, and any local process could forge it anyway, so its
//!   job is keeping other tailnet users (shared nodes) out.

use std::collections::HashSet;

use axum::http::{HeaderMap, StatusCode, header};

#[derive(Debug, Clone)]
pub struct Access {
    hosts: HashSet<String>,
    origins: HashSet<String>,
    owner: Option<String>,
}

impl Access {
    pub fn new(port: u16, public_hosts: &[String], extra_origins: &[String], owner: Option<String>) -> Self {
        let mut hosts: HashSet<String> =
            ["127.0.0.1", "localhost", "[::1]"].iter().map(|h| format!("{h}:{port}")).collect();
        hosts.extend(public_hosts.iter().map(|h| h.to_ascii_lowercase()));
        let origins = extra_origins.iter().map(|o| o.trim_end_matches('/').to_ascii_lowercase()).collect();
        Self { hosts, origins, owner }
    }

    /// Checks for every request.
    pub fn check(&self, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
        let host = header_str(headers, header::HOST.as_str()).unwrap_or_default().to_ascii_lowercase();
        if !self.hosts.contains(&host) {
            return Err((StatusCode::MISDIRECTED_REQUEST, format!("unknown host {host:?}")));
        }
        if let Some(login) = header_str(headers, "tailscale-user-login") {
            match &self.owner {
                Some(owner) if owner.eq_ignore_ascii_case(login) => {}
                Some(_) => {
                    return Err((StatusCode::FORBIDDEN, format!("{login} is not the owner")));
                }
                None => {
                    return Err((
                        StatusCode::FORBIDDEN,
                        "tailnet request but no owner configured; start illogicald with --owner".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Extra check for WebSocket upgrades.
    pub fn check_origin(&self, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
        let Some(origin) = header_str(headers, header::ORIGIN.as_str()) else {
            // Not a browser (CLI, tests). Browsers always send Origin on WS.
            return Ok(());
        };
        let origin = origin.trim_end_matches('/').to_ascii_lowercase();
        let host = origin.split_once("://").map(|(_, h)| h).unwrap_or_default();
        if self.hosts.contains(host) || self.origins.contains(&origin) {
            Ok(())
        } else {
            Err((StatusCode::FORBIDDEN, format!("origin {origin} not allowed")))
        }
    }
}

fn header_str<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// What Tailscale says about this machine.
pub struct Tailnet {
    /// MagicDNS name, e.g. `geek.tailb2e8f2.ts.net`.
    pub host: String,
    /// Login of the user who owns this node; the default owner.
    pub login: Option<String>,
}

pub fn tailnet() -> Option<Tailnet> {
    let out = std::process::Command::new("tailscale").args(["status", "--json"]).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let me = v.get("Self")?;
    let host = me.get("DNSName")?.as_str()?.trim_end_matches('.').to_owned();
    let login = me
        .get("UserID")
        .and_then(|id| v.get("User")?.get(id.to_string())?.get("LoginName")?.as_str())
        .map(str::to_owned);
    (!host.is_empty()).then_some(Tailnet { host, login })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
        }
        h
    }

    fn access() -> Access {
        Access::new(7681, &["geek.example.ts.net".into()], &["http://localhost:5173".into()], Some("me@x.com".into()))
    }

    #[test]
    fn host_must_be_known() {
        let a = access();
        assert!(a.check(&headers(&[("host", "127.0.0.1:7681")])).is_ok());
        assert!(a.check(&headers(&[("host", "geek.example.ts.net")])).is_ok());
        assert!(a.check(&headers(&[("host", "evil.com")])).is_err());
        assert!(a.check(&headers(&[("host", "127.0.0.1:9999")])).is_err());
        assert!(a.check(&headers(&[])).is_err());
    }

    #[test]
    fn tailnet_identity_must_be_owner() {
        let a = access();
        let ok = headers(&[("host", "geek.example.ts.net"), ("tailscale-user-login", "ME@x.com")]);
        assert!(a.check(&ok).is_ok());
        let other = headers(&[("host", "geek.example.ts.net"), ("tailscale-user-login", "friend@x.com")]);
        assert_eq!(a.check(&other).unwrap_err().0, StatusCode::FORBIDDEN);
        let no_owner = Access::new(7681, &["geek.example.ts.net".into()], &[], None);
        assert!(no_owner.check(&ok).is_err());
    }

    #[test]
    fn websocket_origin() {
        let a = access();
        assert!(a.check_origin(&headers(&[])).is_ok());
        assert!(a.check_origin(&headers(&[("origin", "https://geek.example.ts.net")])).is_ok());
        assert!(a.check_origin(&headers(&[("origin", "http://127.0.0.1:7681")])).is_ok());
        assert!(a.check_origin(&headers(&[("origin", "http://localhost:5173")])).is_ok());
        assert!(a.check_origin(&headers(&[("origin", "https://evil.com")])).is_err());
        assert!(a.check_origin(&headers(&[("origin", "null")])).is_err());
    }
}
