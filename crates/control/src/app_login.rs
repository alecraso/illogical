//! Signing in the desktop app (M48, #159).
//!
//! An app's webview can't use passkeys (an unsigned app has no associated
//! domains on macOS, and WebKitGTK has no platform authenticator), so the
//! person signs in where they always do, in their browser, and hands the
//! session to the app. The hand-over is bound to the app that asked, as
//! OAuth does for native apps (RFC 8252 with PKCE): a link someone else
//! started can't sign their app in as you, and a link to redeem can't sign
//! your browser in as them.
//!
//! 1. The app makes a verifier it keeps, listens on a loopback port, and
//!    asks for a ticket: `POST /auth/app {name, challenge, port}`
//!    (`challenge` is the verifier's SHA-256, hex). It gets an id, a short
//!    code, and the page to open (`/#app=<id>`).
//! 2. In the browser, signed in, control's page shows "Sign in the
//!    illogical app on <name>?" with the code and where the request came
//!    from (`GET /api/app-login/{id}`). Allow (`POST …/allow`) binds the
//!    ticket to the account and answers with a one-time grant, which the
//!    page hands to `http://127.0.0.1:<port>/illogical-signin`: only the
//!    app on this computer hears it.
//! 3. The app's webview opens control's page with `#app-redeem=<id>.<grant>.<verifier>`,
//!    which posts them to `POST /auth/app/{id}/redeem` (from control's own
//!    origin, as JSON) and gets the session cookie there.
//!
//! Tickets are single-use, live ten minutes, and are kept in memory (a
//! restart only cancels sign-ins in progress). The session alone reaches
//! no machine: the app is then a new device that a trusted device approves,
//! as any browser is.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ApiError, App,
    auth::{Session, hash, start_session, token},
    err,
};

const TTL_MS: u64 = 10 * 60 * 1000;
/// Tickets waiting at once, all together: past this, asking fails.
const MAX_OPEN: usize = 1000;
pub const ASKS: (&str, usize) = ("app-login", 60);
/// Where the app listens for its grant.
const LOOPBACK_PATH: &str = "/illogical-signin";

#[derive(Clone)]
struct Ticket {
    /// SHA-256 (hex) of the verifier only the app holds.
    challenge: String,
    /// The app's loopback port.
    port: u16,
    name: String,
    /// The address it asked from, to show.
    from: std::net::IpAddr,
    created_ms: u64,
    account: Option<String>,
    /// Hash of the grant the browser handed the app, once allowed.
    grant: Option<String>,
}

#[derive(Default)]
pub struct Tickets(Mutex<HashMap<String, Ticket>>);

fn sha256_hex(s: &str) -> String {
    hash(s)
}

impl Tickets {
    fn sweep(map: &mut HashMap<String, Ticket>, now: u64) {
        map.retain(|_, t| now.saturating_sub(t.created_ms) < TTL_MS);
    }

    fn create(&self, name: &str, challenge: &str, port: u16, from: std::net::IpAddr, now: u64) -> Option<String> {
        let mut map = self.0.lock().unwrap();
        Self::sweep(&mut map, now);
        if map.len() >= MAX_OPEN {
            return None;
        }
        let id = token();
        map.insert(
            id.clone(),
            Ticket {
                challenge: challenge.to_owned(),
                port,
                name: name.to_owned(),
                from,
                created_ms: now,
                account: None,
                grant: None,
            },
        );
        Some(id)
    }

    fn get(&self, id: &str, now: u64) -> Option<Ticket> {
        let mut map = self.0.lock().unwrap();
        Self::sweep(&mut map, now);
        map.get(id).cloned()
    }

    /// Bind it to the account: the grant to hand the app, and its port.
    fn allow(&self, id: &str, account: &str, now: u64) -> Option<(String, u16)> {
        let mut map = self.0.lock().unwrap();
        Self::sweep(&mut map, now);
        match map.get_mut(id) {
            Some(t) if t.account.is_none() => {
                let grant = token();
                t.account = Some(account.to_owned());
                t.grant = Some(hash(&grant));
                Some((grant, t.port))
            }
            _ => None,
        }
    }

    /// The account, once, for the app that holds the verifier and got
    /// the grant.
    fn redeem(&self, id: &str, grant: &str, verifier: &str, now: u64) -> Option<String> {
        let mut map = self.0.lock().unwrap();
        Self::sweep(&mut map, now);
        let t = map.get(id)?;
        if t.grant.as_deref() != Some(hash(grant).as_str()) || t.challenge != sha256_hex(verifier) {
            return None;
        }
        let account = t.account.clone()?;
        map.remove(id);
        Some(account)
    }
}

/// What both screens show, to check they're the same sign-in.
pub fn code(id: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTVWXYZ0123456789";
    let h = hash(&format!("illogical app login\n{id}\n"));
    let n = u64::from_str_radix(&h[..16], 16).unwrap_or(0);
    let mut s = String::new();
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        s.push(ALPHABET[((n >> (59 - 5 * i)) & 31) as usize] as char);
    }
    s
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

#[derive(Deserialize)]
pub struct Ask {
    name: String,
    /// The verifier's SHA-256, hex. Apps from before 0.17 send none.
    #[serde(default)]
    challenge: Option<String>,
    /// The loopback port the app hears its grant on.
    #[serde(default)]
    port: Option<u16>,
}

/// `POST /auth/app`: a ticket for an app that wants a session.
pub async fn ask(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<Ask>,
) -> Result<Json<Value>, ApiError> {
    let ip = app.limits.client_ip(peer, &headers);
    app.limits.check(ASKS, ip)?;
    let (Some(challenge), Some(port)) = (b.challenge.filter(|c| hex64(c)), b.port.filter(|p| *p >= 1024)) else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "this illogical app is out of date: update it (0.17 or newer) to sign in",
        ));
    };
    let name: String = b.name.trim().chars().filter(|c| !c.is_control()).take(80).collect();
    if name.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "the app's name is empty"));
    }
    let id = app
        .app_logins
        .create(&name, &challenge, port, ip, now_ms())
        .ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "too many sign-ins waiting; try again in a few minutes"))?;
    Ok(Json(json!({
        "ticket": id,
        "code": code(&id),
        // The site the app asked on (#507): the browser that approves
        // signs in there.
        "url": format!("{}/#app={id}", app.cfg.site(&headers).url),
        "expires_in_secs": TTL_MS / 1000,
    })))
}

/// `GET /api/app-login/{id}`: what the signed-in page asks about: the
/// app's name and code, where it asked from, and whether that's where
/// this browser is.
pub async fn show(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    _s: Session,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let t = app
        .app_logins
        .get(&id, now_ms())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "that sign-in expired; start it again in the app"))?;
    let here = app.limits.client_ip(peer, &headers);
    let same = crate::limit::bucket(here) == crate::limit::bucket(t.from);
    Ok(Json(json!({
        "name": t.name, "code": code(&id), "allowed": t.account.is_some(),
        "from": t.from.to_string(), "same_network": same,
    })))
}

/// `POST /api/app-login/{id}/allow`: this account, for that app. The
/// answer says where to hand the grant: the app's loopback port, on the
/// computer this page is on.
pub async fn allow(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let (grant, port) = app.app_logins.allow(&id, &s.account, now_ms()).ok_or_else(|| {
        err(StatusCode::NOT_FOUND, "that sign-in expired or was already used; start it again in the app")
    })?;
    Ok(Json(json!({ "redirect": format!("http://127.0.0.1:{port}{LOOPBACK_PATH}?ticket={id}&grant={grant}") })))
}

#[derive(Deserialize)]
pub struct Redeem {
    grant: String,
    verifier: String,
}

/// `POST /auth/app/{id}/redeem {grant, verifier}`: the session, for the
/// app's webview, from control's own page there.
pub async fn redeem(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<Redeem>,
) -> Result<Response, ApiError> {
    // Only control's own page (the app's webview on it) redeems.
    let origin = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok());
    if origin != Some(app.cfg.site(&headers).origin.as_str()) {
        return Err(err(StatusCode::FORBIDDEN, "cross-origin request refused"));
    }
    let account = app.app_logins.redeem(&id, &b.grant, &b.verifier, now_ms()).ok_or_else(|| {
        err(StatusCode::NOT_FOUND, "that sign-in expired or was already used; start it again in the app")
    })?;
    let cookie = start_session(&app, &account)?;
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERE: std::net::IpAddr = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    #[test]
    fn a_ticket_is_allowed_once_and_redeemed_once_by_the_app_that_asked() {
        let t = Tickets::default();
        let verifier = "v".repeat(43);
        let id = t.create("illogical app on jake-air", &sha256_hex(&verifier), 50123, HERE, 1000).unwrap();
        assert_eq!(t.redeem(&id, "", &verifier, 1001), None, "not before it's allowed");
        let (grant, port) = t.allow(&id, "acct", 1002).unwrap();
        assert_eq!(port, 50123);
        assert!(t.allow(&id, "other", 1003).is_none(), "allowed once");
        assert_eq!(t.redeem(&id, "wrong", &verifier, 1004), None, "not without the grant");
        assert_eq!(t.redeem(&id, &grant, "someone else's verifier", 1004), None, "not without the verifier");
        assert_eq!(t.redeem(&id, &grant, &verifier, 1005).as_deref(), Some("acct"));
        assert_eq!(t.redeem(&id, &grant, &verifier, 1006), None, "used once");
    }

    #[test]
    fn tickets_expire() {
        let t = Tickets::default();
        let id = t.create("app", &sha256_hex("v"), 50000, HERE, 0).unwrap();
        assert!(t.allow(&id, "acct", TTL_MS + 1).is_none());
    }

    #[test]
    fn codes_are_stable_and_shaped() {
        let c = code("abc");
        assert_eq!(c, code("abc"));
        assert_ne!(c, code("abd"));
        assert_eq!(c.len(), 9);
        assert_eq!(c.as_bytes()[4], b'-');
    }
}
