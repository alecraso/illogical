//! Signing in the desktop app (M48, #159).
//!
//! An app's webview can't use passkeys (an unsigned app has no associated
//! domains on macOS, and WebKitGTK has no platform authenticator), so the
//! person signs in where they always do, in their browser, and hands the
//! session to the app:
//!
//! 1. The app asks for a ticket: `POST /auth/app {name}` returns an id, a
//!    secret only the app keeps, a short code, and the page to open
//!    (`/#app=<id>`).
//! 2. In the browser, signed in, control's page shows "Sign in the
//!    illogical app on <name>?" with the code (`GET /api/app-login/{id}`),
//!    and Allow binds the ticket to the account (`POST …/allow`). The code
//!    is also on the app's screen, so a link sent by someone else (whose
//!    app would get the session) shows a code that isn't on yours.
//! 3. The app polls `GET /auth/app/{id}/poll?secret=…`; once allowed, its
//!    webview opens `/auth/app/{id}/redeem?secret=…`, which starts a
//!    session there (the cookie lands in the app) and goes to `/`.
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
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
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

#[derive(Clone)]
struct Ticket {
    secret: String,
    name: String,
    created_ms: u64,
    account: Option<String>,
}

#[derive(Default)]
pub struct Tickets(Mutex<HashMap<String, Ticket>>);

impl Tickets {
    fn sweep(map: &mut HashMap<String, Ticket>, now: u64) {
        map.retain(|_, t| now.saturating_sub(t.created_ms) < TTL_MS);
    }

    fn create(&self, name: &str, now: u64) -> Option<(String, String)> {
        let mut map = self.0.lock().unwrap();
        Self::sweep(&mut map, now);
        if map.len() >= MAX_OPEN {
            return None;
        }
        let (id, secret) = (token(), token());
        map.insert(id.clone(), Ticket { secret: hash(&secret), name: name.to_owned(), created_ms: now, account: None });
        Some((id, secret))
    }

    fn get(&self, id: &str, now: u64) -> Option<Ticket> {
        let mut map = self.0.lock().unwrap();
        Self::sweep(&mut map, now);
        map.get(id).cloned()
    }

    fn allow(&self, id: &str, account: &str, now: u64) -> bool {
        let mut map = self.0.lock().unwrap();
        Self::sweep(&mut map, now);
        match map.get_mut(id) {
            Some(t) if t.account.is_none() => {
                t.account = Some(account.to_owned());
                true
            }
            _ => false,
        }
    }

    /// The account, once, for the holder of the secret.
    fn redeem(&self, id: &str, secret: &str, now: u64) -> Option<String> {
        let mut map = self.0.lock().unwrap();
        Self::sweep(&mut map, now);
        let t = map.get(id)?;
        if t.secret != hash(secret) {
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

#[derive(Deserialize)]
pub struct Ask {
    name: String,
}

/// `POST /auth/app`: a ticket for an app that wants a session.
pub async fn ask(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<Ask>,
) -> Result<Json<Value>, ApiError> {
    app.limits.check(ASKS, app.limits.client_ip(peer, &headers))?;
    let name: String = b.name.trim().chars().filter(|c| !c.is_control()).take(80).collect();
    if name.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "the app's name is empty"));
    }
    let (id, secret) = app
        .app_logins
        .create(&name, now_ms())
        .ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "too many sign-ins waiting; try again in a few minutes"))?;
    Ok(Json(json!({
        "ticket": id,
        "secret": secret,
        "code": code(&id),
        "url": format!("{}/#app={id}", app.cfg.public_url),
        "expires_in_secs": TTL_MS / 1000,
    })))
}

/// `GET /api/app-login/{id}`: what the signed-in page asks about.
pub async fn show(State(app): State<Arc<App>>, _s: Session, Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let t = app
        .app_logins
        .get(&id, now_ms())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "that sign-in expired; start it again in the app"))?;
    Ok(Json(json!({ "name": t.name, "code": code(&id), "allowed": t.account.is_some() })))
}

/// `POST /api/app-login/{id}/allow`: this account, for that app.
pub async fn allow(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    if app.app_logins.allow(&id, &s.account, now_ms()) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(err(StatusCode::NOT_FOUND, "that sign-in expired or was already used; start it again in the app"))
    }
}

#[derive(Deserialize)]
pub struct Secret {
    secret: String,
}

/// `GET /auth/app/{id}/poll?secret=`: has it been allowed?
pub async fn poll(State(app): State<Arc<App>>, Path(id): Path<String>, Query(q): Query<Secret>) -> Json<Value> {
    let state = match app.app_logins.get(&id, now_ms()) {
        Some(t) if t.secret == hash(&q.secret) => {
            if t.account.is_some() {
                "allowed"
            } else {
                "waiting"
            }
        }
        _ => "expired",
    };
    Json(json!({ "state": state }))
}

/// `GET /auth/app/{id}/redeem?secret=`: the session, in whatever opened
/// this (the app's webview), then control's page.
pub async fn redeem(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(q): Query<Secret>,
) -> Result<Response, ApiError> {
    let account = app.app_logins.redeem(&id, &q.secret, now_ms()).ok_or_else(|| {
        err(StatusCode::NOT_FOUND, "that sign-in expired or was already used; start it again in the app")
    })?;
    let cookie = start_session(&app, &account)?;
    let mut res = Redirect::to("/").into_response();
    res.headers_mut().append(header::SET_COOKIE, cookie);
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_is_allowed_once_and_redeemed_once_with_its_secret() {
        let t = Tickets::default();
        let (id, secret) = t.create("illogical app on jake-air", 1000).unwrap();
        assert_eq!(t.redeem(&id, &secret, 1001), None, "not before it's allowed");
        assert!(t.allow(&id, "acct", 1002));
        assert!(!t.allow(&id, "other", 1003), "allowed once");
        assert_eq!(t.redeem(&id, "wrong", 1004), None);
        assert_eq!(t.redeem(&id, &secret, 1005).as_deref(), Some("acct"));
        assert_eq!(t.redeem(&id, &secret, 1006), None, "used once");
    }

    #[test]
    fn tickets_expire() {
        let t = Tickets::default();
        let (id, secret) = t.create("app", 0).unwrap();
        assert!(!t.allow(&id, "acct", TTL_MS + 1));
        assert_eq!(t.redeem(&id, &secret, TTL_MS + 2), None);
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
