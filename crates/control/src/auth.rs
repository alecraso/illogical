//! Who is asking.
//!
//! - **People** sign in with GitHub (OAuth, as a GitHub App) and get a
//!   session cookie for control's own API: the directory, approvals, joins.
//!   A session never reaches a daemon. Daemons trust devices, by key.
//! - **Daemons** sign each request with their enrolled Ed25519 key:
//!   `x-illogical-auth: <device id> <ms> <sig>` over
//!   `illogical daemon auth\n<METHOD>\n<path>\n<ms>\n`, within five
//!   minutes of now.
//!
//! Cookie-authenticated requests that change anything, and every WebSocket
//! upgrade, must come from control's own origin (no cross-site requests
//! riding on the cookie).

use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Redirect, Response},
};
use illogical_e2e::{Cert, now_ms};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::{ApiError, App, err};

pub const SESSION_COOKIE: &str = "ilg_session";
const STATE_COOKIE: &str = "ilg_oauth";
const SESSION_DAYS: u64 = 30;
pub const AUTH_HEADER: &str = "x-illogical-auth";
const SKEW_MS: u64 = 5 * 60 * 1000;

pub fn token() -> String {
    hex::encode(illogical_e2e::random::<32>())
}

pub fn hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn set_cookie(app: &App, name: &str, value: &str, max_age_secs: u64) -> HeaderValue {
    let secure = if app.cfg.public_url.starts_with("https://") { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"))
        .unwrap()
}

/// A signed-in person.
pub struct Session {
    pub account: String,
}

impl FromRequestParts<Arc<App>> for Session {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, ApiError> {
        let upgrade = parts.headers.contains_key(header::UPGRADE);
        if parts.method != Method::GET || upgrade {
            let origin = parts.headers.get(header::ORIGIN).and_then(|o| o.to_str().ok());
            match origin {
                Some(o) if o == app.cfg.origin => {}
                None if !upgrade => {}
                _ => return Err(err(StatusCode::FORBIDDEN, "cross-origin request refused")),
            }
        }
        let token =
            cookie(&parts.headers, SESSION_COOKIE).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "sign in first"))?;
        let account =
            app.db.session(&hash(token), now_ms())?.ok_or_else(|| err(StatusCode::UNAUTHORIZED, "sign in again"))?;
        Ok(Session { account })
    }
}

/// An enrolled daemon, by its signature.
pub struct DaemonAuth {
    pub cert: Cert,
}

pub fn daemon_auth_message(method: &str, path: &str, ms: u64) -> String {
    format!("illogical daemon auth\n{method}\n{path}\n{ms}\n")
}

impl FromRequestParts<Arc<App>> for DaemonAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, ApiError> {
        let bad = || err(StatusCode::UNAUTHORIZED, "bad daemon signature");
        let h = parts.headers.get(AUTH_HEADER).and_then(|v| v.to_str().ok()).ok_or_else(bad)?;
        let f: Vec<&str> = h.split_whitespace().collect();
        let [id, ms, sig] = f.as_slice() else { return Err(bad()) };
        let ms: u64 = ms.parse().map_err(|_| bad())?;
        if now_ms().abs_diff(ms) > SKEW_MS {
            return Err(err(StatusCode::UNAUTHORIZED, "clock skew: check this machine's time"));
        }
        let cert = app
            .db
            .daemon_cert(id)?
            .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "not an enrolled daemon (left, or revoked?)"))?;
        let msg = daemon_auth_message(parts.method.as_str(), parts.uri.path(), ms);
        if !illogical_e2e::cert::verify_hex(&cert.sign, msg.as_bytes(), sig) {
            return Err(bad());
        }
        Ok(DaemonAuth { cert })
    }
}

// ---------------------------------------------------------------- GitHub

#[derive(Deserialize)]
pub struct Start {
    next: Option<String>,
}

/// Only paths on this site, never `//elsewhere`.
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") && !n.contains('\\') => n.to_owned(),
        _ => "/".into(),
    }
}

fn callback_url(app: &App) -> String {
    format!("{}/auth/github/callback", app.cfg.public_url)
}

pub async fn github_start(State(app): State<Arc<App>>, Query(q): Query<Start>) -> Response {
    let Some(gh) = &app.cfg.github else {
        return (StatusCode::NOT_FOUND, "GitHub sign-in isn't configured here").into_response();
    };
    let state = token();
    let next = safe_next(q.next.as_deref());
    let url = url::Url::parse_with_params(
        &format!("{}/login/oauth/authorize", gh.url),
        &[("client_id", gh.client_id.as_str()), ("redirect_uri", &callback_url(&app)), ("state", &state)],
    )
    .unwrap();
    let mut res = Redirect::to(url.as_str()).into_response();
    let value = format!("{state}.{}", base64_url(next.as_bytes()));
    res.headers_mut().append(header::SET_COOKIE, set_cookie(&app, STATE_COOKIE, &value, 600));
    res
}

fn base64_url(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn unbase64_url(s: &str) -> Option<String> {
    use base64::Engine;
    String::from_utf8(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok()?).ok()
}

#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct GithubUser {
    id: u64,
    login: String,
}

pub async fn github_callback(State(app): State<Arc<App>>, headers: HeaderMap, Query(q): Query<Callback>) -> Response {
    match github_finish(&app, &headers, q).await {
        Ok((account, next)) => {
            let cookie = match start_session(&app, &account) {
                Ok(c) => c,
                Err(e) => return e.into_response(),
            };
            info!(%account, "signed in with GitHub");
            let mut res = Redirect::to(&next).into_response();
            let h = res.headers_mut();
            h.append(header::SET_COOKIE, cookie);
            h.append(header::SET_COOKIE, set_cookie(&app, STATE_COOKIE, "", 0));
            res
        }
        Err(why) => {
            warn!(%why, "GitHub sign-in failed");
            (StatusCode::BAD_REQUEST, format!("Sign-in failed: {why}")).into_response()
        }
    }
}

async fn github_finish(app: &App, headers: &HeaderMap, q: Callback) -> Result<(String, String), String> {
    let gh = app.cfg.github.as_ref().ok_or("not configured")?;
    if let Some(e) = q.error {
        return Err(e);
    }
    let (code, state) = q.code.zip(q.state).ok_or("missing code")?;
    let (want, next) =
        cookie(headers, STATE_COOKIE).and_then(|c| c.split_once('.')).ok_or("the sign-in took too long; try again")?;
    if want != state {
        return Err("state mismatch; try again".into());
    }
    let next = safe_next(unbase64_url(next).as_deref());
    let form: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &gh.client_id)
        .append_pair("client_secret", &gh.client_secret)
        .append_pair("code", &code)
        .append_pair("redirect_uri", &callback_url(app))
        .finish();
    let tok: TokenResponse = app
        .http
        .post(format!("{}/login/oauth/access_token", gh.url))
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let access = tok.access_token.ok_or_else(|| tok.error_description.unwrap_or_else(|| "no token".into()))?;
    let user: GithubUser = app
        .http
        .get(format!("{}/user", gh.api))
        .bearer_auth(access)
        .header(header::USER_AGENT, "illogical-control")
        .header(header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let account = app
        .db
        .account_for("github", &user.id.to_string(), &user.login, &new_account_id(), now_ms())
        .map_err(|e| e.to_string())?;
    Ok((account, next))
}

/// A new session for `account`: the Set-Cookie for it.
pub fn start_session(app: &App, account: &str) -> Result<HeaderValue, ApiError> {
    let t = token();
    let now = now_ms();
    app.db.add_session(&hash(&t), account, now, now + SESSION_DAYS * 86_400_000)?;
    Ok(set_cookie(app, SESSION_COOKIE, &t, SESSION_DAYS * 86_400))
}

pub fn new_account_id() -> String {
    hex::encode(illogical_e2e::random::<8>())
}

pub async fn logout(State(app): State<Arc<App>>, _s: Session, headers: HeaderMap) -> Response {
    if let Some(t) = cookie(&headers, SESSION_COOKIE) {
        let _ = app.db.drop_session(&hash(t));
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut().append(header::SET_COOKIE, set_cookie(&app, SESSION_COOKIE, "", 0));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_stays_here() {
        assert_eq!(safe_next(Some("/x?y=1")), "/x?y=1");
        assert_eq!(safe_next(Some("//evil.example")), "/");
        assert_eq!(safe_next(Some("https://evil.example")), "/");
        assert_eq!(safe_next(Some("/\\evil.example")), "/");
        assert_eq!(safe_next(None), "/");
    }

    #[test]
    fn cookies() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; ilg_session=tok; b=2"));
        assert_eq!(cookie(&h, SESSION_COOKIE), Some("tok"));
        assert_eq!(cookie(&h, "c"), None);
    }
}
