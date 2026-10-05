//! Rate limits: for what anyone can do without signing in (start a
//! daemon's join, make an account with a passkey, finish a GitHub sign-in),
//! per client address, and for what a signed-in account or an enrolled
//! daemon can make control do (look people up, make teams and invites, ask
//! GitHub, push), per account or per daemon. In memory (a restart forgets;
//! that's fine for abuse limits).
//!
//! The client address is the TCP peer, or behind a proxy that says so
//! (`--trust-proxy-header Fly-Client-IP`, say) that header. An IPv6 client
//! counts by its /64: one host usually has the whole prefix to pick from.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::http::{HeaderMap, StatusCode};

use crate::{ApiError, err};

pub struct Limits {
    /// Per (what, who): when each recent use happened.
    seen: Mutex<HashMap<(&'static str, String), Vec<Instant>>>,
    pub proxy_header: Option<String>,
}

/// How many per hour.
pub const JOINS: (&str, usize) = ("join", 30);
pub const ACCOUNTS: (&str, usize) = ("account", 10);
pub const SIGN_INS: (&str, usize) = ("sign-in", 60);
pub const LINKS: (&str, usize) = ("link", 240);
/// Invite previews on the signed-out page (#103).
pub const INVITES: (&str, usize) = ("invite", 240);
/// Notifications relayed, all daemons together, per hour: a brake.
pub const PUSHES: (&str, usize) = ("push", 100_000);
/// Notifications one daemon relays per hour.
pub const DAEMON_PUSHES: (&str, usize) = ("daemon-push", 2_000);
/// Looking people up to share with, per account.
pub const PEOPLE: (&str, usize) = ("people", 120);
/// Devices asking into an account, per account.
pub const ENROLLS: (&str, usize) = ("enroll", 30);
/// Teams made, per account.
pub const TEAMS: (&str, usize) = ("team", 20);
/// Invite links made, per account.
pub const TEAM_INVITES: (&str, usize) = ("team-invite", 120);
/// Checkout pages asked for, per account.
pub const CHECKOUTS: (&str, usize) = ("checkout", 20);
/// Usage reports asked for, per account and all together.
pub const REPORTS: (&str, usize) = ("report", 6);
pub const ALL_REPORTS: (&str, usize) = ("report-all", 30);
/// `forge.watch` messages, per daemon.
pub const FORGE_WATCHES: (&str, usize) = ("forge-watch", 120);
/// Repositories looked up at GitHub for the first time in a while (not
/// the heartbeat's re-checks), per account: what a burst of `forge.watch`
/// messages costs the App's quota.
pub const FORGE_LOOKUPS: (&str, usize) = ("forge-lookup", 600);
/// Shares offered to other accounts, per daemon (each shows a prompt).
pub const OFFERS: (&str, usize) = ("offer", 120);
const WINDOW: Duration = Duration::from_secs(3600);

/// What a client address counts as: itself, or for IPv6 its /64.
pub fn bucket(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
    }
}

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

    /// Count one use from a client address; refuse once over the limit.
    pub fn check(&self, what: (&'static str, usize), ip: IpAddr) -> Result<(), ApiError> {
        self.count(what, format!("ip:{}", bucket(ip)), "too many of those from here; try again later")
    }

    /// Count one use by an account; refuse once over the limit.
    pub fn check_account(&self, what: (&'static str, usize), account: &str) -> Result<(), ApiError> {
        self.count(what, format!("account:{account}"), "too many of those lately; try again later")
    }

    /// Count one use by a daemon; refuse once over the limit.
    pub fn check_daemon(&self, what: (&'static str, usize), daemon: &str) -> Result<(), ApiError> {
        self.count(what, format!("daemon:{daemon}"), "too many of those from this machine lately; try again later")
    }

    /// Count one use of something with one limit for everyone.
    pub fn check_all(&self, what: (&'static str, usize)) -> Result<(), ApiError> {
        self.count(what, String::new(), "too many of those lately; try again later")
    }

    fn count(&self, (what, per_hour): (&'static str, usize), who: String, msg: &str) -> Result<(), ApiError> {
        let now = Instant::now();
        let mut seen = self.seen.lock().unwrap();
        if seen.len() > 100_000 {
            seen.retain(|_, v| v.last().is_some_and(|t| now.duration_since(*t) < WINDOW));
        }
        let v = seen.entry((what, who)).or_default();
        v.retain(|t| now.duration_since(*t) < WINDOW);
        if v.len() >= per_hour {
            return Err(err(StatusCode::TOO_MANY_REQUESTS, msg));
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

    #[test]
    fn ipv6_counts_by_its_64() {
        let l = Limits::new(None);
        for i in 0..3 {
            let ip: IpAddr = format!("2001:db8:1:2::{i:x}").parse().unwrap();
            l.check(("t", 3), ip).unwrap();
        }
        let next: IpAddr = "2001:db8:1:2:ffff:1:2:3".parse().unwrap();
        assert!(l.check(("t", 3), next).is_err(), "another address in the same /64");
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert!(l.check(("t", 3), other).is_ok(), "the next /64 is someone else");
        let mapped: IpAddr = "::ffff:10.0.0.9".parse().unwrap();
        assert_eq!(bucket(mapped), "10.0.0.9".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn accounts_and_daemons_count_apart() {
        let l = Limits::new(None);
        l.check_account(("t", 1), "a").unwrap();
        assert!(l.check_account(("t", 1), "a").is_err());
        assert!(l.check_account(("t", 1), "b").is_ok());
        assert!(l.check_daemon(("t", 1), "a").is_ok(), "a daemon with the same id is another bucket");
        l.check_all(("u", 1)).unwrap();
        assert!(l.check_all(("u", 1)).is_err());
    }
}
