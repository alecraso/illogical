//! Rate limits for what anyone can do without signing in: start a daemon's
//! join, make an account with a passkey, finish a GitHub sign-in. Per
//! client IP, in memory (a restart forgets; that's fine for abuse limits).
//!
//! The client IP is the TCP peer, or behind a proxy that says so
//! (`--trust-proxy-header Fly-Client-IP`, say) that header.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::http::{HeaderMap, StatusCode};

use crate::{ApiError, err};

pub struct Limits {
    /// Per (what, ip): when each recent use happened.
    seen: Mutex<HashMap<(&'static str, IpAddr), Vec<Instant>>>,
    pub proxy_header: Option<String>,
}

/// How many per hour.
pub const JOINS: (&str, usize) = ("join", 30);
pub const ACCOUNTS: (&str, usize) = ("account", 10);
pub const SIGN_INS: (&str, usize) = ("sign-in", 60);
pub const LINKS: (&str, usize) = ("link", 240);
const WINDOW: Duration = Duration::from_secs(3600);

impl Limits {
    pub fn new(proxy_header: Option<String>) -> Self {
        Self { seen: Mutex::new(HashMap::new()), proxy_header }
    }

    pub fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        self.proxy_header
            .as_deref()
            .and_then(|h| headers.get(h))
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(peer.ip())
    }

    /// Count one use; refuse once over the limit.
    pub fn check(&self, (what, per_hour): (&'static str, usize), ip: IpAddr) -> Result<(), ApiError> {
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap();
        if seen.len() > 100_000 {
            seen.retain(|_, v| v.last().is_some_and(|t| now.duration_since(*t) < WINDOW));
        }
        let v = seen.entry((what, ip)).or_default();
        v.retain(|t| now.duration_since(*t) < WINDOW);
        if v.len() >= per_hour {
            return Err(err(StatusCode::TOO_MANY_REQUESTS, "too many of those from here; try again later"));
        }
        v.push(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_ip() {
        let l = Limits::new(Some("fly-client-ip".into()));
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        for _ in 0..3 {
            l.check(("t", 3), a).unwrap();
        }
        assert!(l.check(("t", 3), a).is_err());
        assert!(l.check(("t", 3), b).is_ok());
        let mut h = HeaderMap::new();
        h.insert("fly-client-ip", "203.0.113.9".parse().unwrap());
        assert_eq!(l.client_ip("127.0.0.1:1".parse().unwrap(), &h), "203.0.113.9".parse::<IpAddr>().unwrap());
        let plain = Limits::new(None);
        assert_eq!(plain.client_ip("127.0.0.1:1".parse().unwrap(), &h), "127.0.0.1".parse::<IpAddr>().unwrap());
    }
}
