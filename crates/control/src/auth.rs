//! Who is asking.
//!
//! - **People** sign in with GitHub (OAuth, as a GitHub App) and get a
//!   session cookie for control's own API: the directory, approvals, joins.
//!   A session never reaches a daemon. Daemons trust devices, by key.
//! - **The CLI** (M49) is a `cli` device: it joins with a code, as a
//!   daemon does, and then signs each request as a daemon does (below).
//!   A signed request from an approved CLI device is a session for its
//!   account, with no cookie to ride on, so it needs no origin.
//! - **Daemons** sign each request with their enrolled Ed25519 key:
//!   `x-arugula-auth: v2 <device id> <ms> <nonce> <sig>` over
//!   `illogical daemon auth v2\n<METHOD>\n<path and query>\n<ms>\n<nonce>\n<sha256 of the body, hex>\n`,
//!   within five minutes of now. Each signature is good once: control
//!   remembers the ones it took until they'd be too old anyway.
//!   Daemons from before 0.17 sign `<device id> <ms> <sig>` over
//!   `illogical daemon auth\n<METHOD>\n<path>\n<ms>\n` (no query, no body);
//!   control takes those too, once each, until it's started with
//!   `--refuse-old-daemon-signatures`, when they're told to update.
//!
//! Cookie-authenticated requests that change anything, and every WebSocket
//! upgrade, must come from control's own origin (no cross-site requests
//! riding on the cookie).

use std::sync::Arc;

use arugula_e2e::{Cert, Kind, now_ms};
use axum::{
    extract::{FromRequestParts, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::{ApiError, App, err};

pub const SESSION_COOKIE: &str = "ilg_session";
const STATE_COOKIE: &str = "ilg_oauth";
const SESSION_DAYS: u64 = 30;
/// The signature header, under either name (#504): daemons and CLIs send
/// `x-illogical-auth` until the rename, `x-arugula-auth` after.
pub fn auth_header(headers: &HeaderMap) -> Option<&HeaderValue> {
    arugula_core::rename::either(arugula_core::rename::AUTH, |n| headers.get(n))
}
const SKEW_MS: u64 = 5 * 60 * 1000;

pub fn token() -> String {
    hex::encode(arugula_e2e::random::<32>())
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
        // A CLI device's signature (M49): no cookie, so no origin to check.
        match parts.extensions.get::<Signed>() {
            Some(Signed(Ok(cert))) if cert.kind == Kind::Cli => return Ok(Session { account: cert.account.clone() }),
            Some(Signed(Ok(_))) => return Err(err(StatusCode::UNAUTHORIZED, "sign in first")),
            Some(Signed(Err((status, msg)))) => return Err(err(*status, msg)),
            None => {}
        }
        let upgrade = parts.headers.contains_key(header::UPGRADE);
        if parts.method != Method::GET || upgrade {
            let origin = parts.headers.get(header::ORIGIN).and_then(|o| o.to_str().ok());
            match origin {
                Some(o) if o == app.cfg.site(&parts.headers).origin => {}
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

/// An enrolled daemon, by its signature (checked by [`verify_daemon`],
/// which sees the body).
pub struct DaemonAuth {
    pub cert: Cert,
}

/// What a pre-0.17 daemon signs.
pub fn daemon_auth_message(method: &str, path: &str, ms: u64) -> String {
    format!("illogical daemon auth\n{method}\n{path}\n{ms}\n")
}

/// What a daemon signs: the request's method, path and query, time, a
/// nonce, and its body's hash.
pub fn daemon_auth_message_v2(method: &str, path_and_query: &str, ms: u64, nonce: &str, body: &[u8]) -> String {
    let body = hex::encode(Sha256::digest(body));
    format!("illogical daemon auth v2\n{method}\n{path_and_query}\n{ms}\n{nonce}\n{body}\n")
}

/// The most a signed request's body may be.
const MAX_SIGNED_BODY: usize = 1 << 20;

/// Signatures (or other one-time proofs) already taken, until they'd be
/// too old to take anyway.
#[derive(Default)]
pub struct Replays(std::sync::Mutex<std::collections::HashMap<String, u64>>);

impl Replays {
    /// Whether this is the first time `key` is used; remembered until
    /// `until` (ms).
    pub fn first(&self, key: &str, until: u64) -> bool {
        let now = now_ms();
        let mut m = self.0.lock().unwrap();
        if m.len() > 50_000 {
            m.retain(|_, u| *u > now);
        }
        match m.get(key) {
            Some(u) if *u > now => false,
            _ => {
                m.insert(key.to_owned(), until);
                true
            }
        }
    }
}

/// The outcome of checking a request's daemon signature, for
/// [`DaemonAuth`] to hand over.
#[derive(Clone)]
struct Signed(Result<Cert, (StatusCode, String)>);

/// Checks `x-arugula-auth` on any request that has one, with its body.
pub async fn verify_daemon(
    State(app): State<Arc<App>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if auth_header(req.headers()).is_none() {
        return next.run(req).await;
    }
    let (mut parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_SIGNED_BODY).await else {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "a signed request's body is at most 1 MB").into_response();
    };
    let checked = check_daemon(&app, &parts, &bytes).map_err(|e| (e.0, e.1));
    if let Ok(cert) = &checked {
        note_old_host(&app, &parts.headers, cert);
    }
    parts.extensions.insert(Signed(checked));
    next.run(axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes))).await
}

/// Daemons and CLIs still signing requests at a URL control is moving away
/// from (#535): logged once an hour each, so `fly logs | grep "old URL"`
/// says who's left before the old URL goes.
#[derive(Default)]
pub struct OldHosts(pub(crate) std::sync::Mutex<std::collections::HashMap<String, u64>>);

fn note_old_host(app: &App, headers: &HeaderMap, cert: &Cert) {
    let site = app.cfg.site(headers);
    if site == &app.cfg.sites[0] {
        return;
    }
    let now = now_ms();
    let mut seen = app.old_hosts.0.lock().unwrap();
    if seen.get(&cert.device).is_some_and(|at| now - at < 3_600_000) {
        return;
    }
    seen.insert(cert.device.clone(), now);
    info!(
        device = cert.device,
        account = cert.account,
        kind = cert.kind.as_str(),
        name = cert.name,
        at = site.url,
        "a signed request at an old URL"
    );
}

fn check_daemon(app: &App, parts: &Parts, body: &[u8]) -> Result<Cert, ApiError> {
    let bad = || err(StatusCode::UNAUTHORIZED, "bad daemon signature");
    let h = auth_header(&parts.headers).and_then(|v| v.to_str().ok()).ok_or_else(bad)?;
    let f: Vec<&str> = h.split_whitespace().collect();
    let (id, ms, sig, msg) = match f.as_slice() {
        ["v2", id, ms, nonce, sig] => {
            if !(16..=64).contains(&nonce.len()) || !nonce.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err(bad());
            }
            let ms: u64 = ms.parse().map_err(|_| bad())?;
            let pq = parts.uri.path_and_query().map_or_else(|| parts.uri.path(), |p| p.as_str());
            (*id, ms, *sig, daemon_auth_message_v2(parts.method.as_str(), pq, ms, nonce, body))
        }
        [id, ms, sig] => {
            if !app.cfg.old_daemon_signatures {
                return Err(err(
                    StatusCode::UPGRADE_REQUIRED,
                    "this machine's Arugula is too old for this control: update Arugula (0.17 or newer) and restart it",
                ));
            }
            let ms: u64 = ms.parse().map_err(|_| bad())?;
            (*id, ms, *sig, daemon_auth_message(parts.method.as_str(), parts.uri.path(), ms))
        }
        _ => return Err(bad()),
    };
    if now_ms().abs_diff(ms) > SKEW_MS {
        return Err(err(StatusCode::UNAUTHORIZED, "clock skew: check this machine's time"));
    }
    let cert = match app.db.daemon_cert(id)? {
        Some(c) => c,
        None => match cli_cert(app, id)? {
            Some(c) => c,
            None if app.db.daemon_account_deleted(id)? => {
                return Err(err(StatusCode::UNAUTHORIZED, "this machine's account was deleted"));
            }
            // Removed from a browser (#330): Gone, for the daemon to make
            // a new key and join again.
            None => match crate::api::removed(app, id, Kind::Daemon, false)? {
                Some((msg, _)) => return Err(err(StatusCode::GONE, &msg)),
                None => return Err(err(StatusCode::UNAUTHORIZED, "not an enrolled daemon (left, or revoked?)")),
            },
        },
    };
    if !arugula_e2e::cert::verify_hex(&cert.sign, msg.as_bytes(), sig) {
        return Err(bad());
    }
    // Once each: a copy of a request (from a log, a proxy) does nothing.
    if !app.daemon_sigs.first(&format!("{id} {sig}"), ms + SKEW_MS + 60_000) {
        return Err(err(StatusCode::UNAUTHORIZED, "that signature was used already"));
    }
    Ok(cert)
}

/// An approved CLI device (M49) that its account still trusts: one that
/// was revoked, or whose approver was, signs nothing here.
fn cli_cert(app: &App, id: &str) -> Result<Option<Cert>, ApiError> {
    let Some(cert) = app.db.approved_cert(id, Kind::Cli)? else { return Ok(None) };
    let Some(root) = app.db.account(&cert.account)?.and_then(|a| a.root) else { return Ok(None) };
    let (certs, _) = app.db.devices(&cert.account)?;
    let revs = app.db.revocations(&cert.account)?;
    let trust = arugula_e2e::Trust { account: cert.account.clone(), root };
    Ok(trust.evaluate(&certs, &revs).get(id).filter(|c| **c == cert).cloned())
}

impl FromRequestParts<Arc<App>> for DaemonAuth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _app: &Arc<App>) -> Result<Self, ApiError> {
        match parts.extensions.get::<Signed>() {
            Some(Signed(Ok(cert))) if cert.kind == Kind::Daemon => Ok(DaemonAuth { cert: cert.clone() }),
            Some(Signed(Ok(_))) => Err(err(StatusCode::UNAUTHORIZED, "not an enrolled daemon")),
            Some(Signed(Err((status, msg)))) => Err(err(*status, msg)),
            None => Err(err(StatusCode::UNAUTHORIZED, "bad daemon signature")),
        }
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

/// Back to the site the sign-in started on (#507): its cookies are there.
fn callback_url(app: &App, headers: &HeaderMap) -> String {
    format!("{}/auth/github/callback", app.cfg.site(headers).url)
}

pub async fn github_start(State(app): State<Arc<App>>, headers: HeaderMap, Query(q): Query<Start>) -> Response {
    let Some(gh) = &app.cfg.github else {
        return (StatusCode::NOT_FOUND, "GitHub sign-in isn't configured here").into_response();
    };
    let state = token();
    let next = safe_next(q.next.as_deref());
    let url = url::Url::parse_with_params(
        &format!("{}/login/oauth/authorize", gh.url),
        &[("client_id", gh.client_id.as_str()), ("redirect_uri", &callback_url(&app, &headers)), ("state", &state)],
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

pub async fn github_callback(
    State(app): State<Arc<App>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<Callback>,
) -> Response {
    if let Err(e) = app.limits.check(crate::limit::SIGN_INS, app.limits.client_ip(peer, &headers)) {
        return e.into_response();
    }
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
        .append_pair("redirect_uri", &callback_url(app, headers))
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
        .header(header::USER_AGENT, "arugula-control")
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
    hex::encode(arugula_e2e::random::<8>())
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
