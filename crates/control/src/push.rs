//! The push relay (M21): one VAPID key for every device, so a phone
//! subscribes once and hears from every daemon it may.
//!
//! A device subscribes with control's VAPID key and signs the subscription
//! with its device key (`illogical_e2e::push`). Daemons fetch the
//! subscriptions of the people they serve, check the signatures, encrypt
//! each notification for each subscription themselves (RFC 8291), and hand
//! control only ciphertext: control signs the VAPID token and posts it.
//! Control sees that daemon X notified device Y, and how big; never what.
//!
//! Control sends notices of its own too (#104): a device or a person
//! waiting for the account's owners. Those it encrypts itself, and they
//! say only that something waits; approving happens on the page, with
//! the fingerprint.
//!
//! Endpoints are only the browsers' push services (so control can't be
//! made to post anywhere else), plus `--push-host` for tests.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use illogical_e2e::{now_ms, push::PushSub};
use p256::{
    SecretKey,
    ecdsa::{Signature, SigningKey, signature::Signer},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{
    ApiError, App,
    auth::{DaemonAuth, Session},
    err,
};

type R = Result<Json<Value>, ApiError>;

/// Push services browsers use.
const SERVICES: &[&str] =
    &["fcm.googleapis.com", "push.services.mozilla.com", "web.push.apple.com", "notify.windows.com"];

pub struct Vapid {
    key: SecretKey,
}

impl Vapid {
    /// Control's key, made on first start and kept in the database.
    pub fn load(db: &crate::db::Db) -> anyhow::Result<Self> {
        if let Some(hex_key) = db.setting("vapid")? {
            let key = SecretKey::from_slice(&hex::decode(hex_key)?).map_err(|_| anyhow::anyhow!("bad VAPID key"))?;
            return Ok(Self { key });
        }
        let key = loop {
            if let Ok(k) = SecretKey::from_slice(&illogical_e2e::random::<32>()) {
                break k;
            }
        };
        db.set_setting("vapid", &hex::encode(key.to_bytes()))?;
        Ok(Self { key })
    }

    pub fn public(&self) -> String {
        B64.encode(self.key.public_key().to_sec1_bytes())
    }

    fn jwt(&self, audience: &str, subject: &str) -> String {
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let exp = now_ms() / 1000 + 12 * 3600;
        let claims = B64.encode(json!({ "aud": audience, "exp": exp, "sub": subject }).to_string());
        let input = format!("{header}.{claims}");
        let sig: Signature = SigningKey::from(self.key.clone()).sign(input.as_bytes());
        format!("{input}.{}", B64.encode(sig.to_bytes()))
    }
}

/// An endpoint at a real push service (or one allowed for tests).
fn allowed_endpoint(app: &App, endpoint: &str) -> bool {
    let Ok(u) = url::Url::parse(endpoint) else { return false };
    let Some(host) = u.host_str() else { return false };
    let hostport = match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_owned(),
    };
    if app.cfg.push_hosts.iter().any(|h| h == &hostport) {
        return true;
    }
    u.scheme() == "https" && SERVICES.iter().any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

#[derive(Deserialize)]
pub struct Subscribe {
    sub: PushSub,
}

pub async fn subscribe(State(app): State<Arc<App>>, s: Session, Json(b): Json<Subscribe>) -> R {
    let sub = b.sub;
    if sub.account != s.account || !allowed_endpoint(&app, &sub.endpoint) {
        return Err(err(StatusCode::BAD_REQUEST, "a subscription at a push service, for your account"));
    }
    let Some((cert, true)) = app.db.device(&s.account, &sub.device)? else {
        return Err(err(StatusCode::FORBIDDEN, "subscribe from an approved device"));
    };
    if !sub.signed_by(&cert) {
        return Err(err(StatusCode::FORBIDDEN, "the subscription isn't signed by that device"));
    }
    app.db.put_push_sub(&sub.endpoint, &s.account, &serde_json::to_string(&sub)?)?;
    // The daemons that serve them pick it up now.
    let mut ds: Vec<String> = app.db.daemons(&s.account)?.into_iter().map(|d| d.id).collect();
    ds.extend(crate::teams::reachable(&app, &s.account)?);
    app.relay.nudge(&ds);
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
pub struct Unsubscribe {
    endpoint: String,
}

pub async fn unsubscribe(State(app): State<Arc<App>>, s: Session, Json(b): Json<Unsubscribe>) -> R {
    app.db.drop_push_sub(&b.endpoint, Some(&s.account))?;
    Ok(Json(json!({})))
}

/// The accounts a daemon serves: its own, its team's, and those it lets in
/// that control routes to it (teams.rs): never someone who has no say in
/// it, whatever list it sent.
fn served(app: &App, daemon: &str, own: &str) -> anyhow::Result<Vec<String>> {
    let mut a = vec![own.to_owned()];
    let rel = crate::teams::Relations::of(app, daemon, own)?;
    for x in app.db.daemon_accounts(daemon)? {
        if rel.routes(app, &x)? {
            a.push(x);
        }
    }
    if let Some(team) = app.db.daemon_team(daemon)?
        && let Some(body) = app.db.latest_roster(&team)?
    {
        let r: illogical_e2e::team::Roster = serde_json::from_str(&body)?;
        a.extend(r.members.into_iter().map(|m| m.account));
    }
    a.sort();
    a.dedup();
    Ok(a)
}

/// Subscriptions of the people a daemon serves, for it to check and use.
pub async fn daemon_subs(State(app): State<Arc<App>>, d: DaemonAuth) -> R {
    let mut subs = Vec::new();
    for a in served(&app, &d.cert.device, &d.cert.account)? {
        subs.extend(app.db.push_subs(&a)?);
    }
    Ok(Json(json!({ "subs": subs })))
}

#[derive(Deserialize)]
pub struct Send {
    endpoint: String,
    /// The encrypted notification (aes128gcm), standard base64.
    body: String,
    #[serde(default = "hour")]
    ttl: u32,
    #[serde(default)]
    urgency: Option<String>,
}

fn hour() -> u32 {
    3600
}

/// Post an encrypted notification with control's VAPID signature; its
/// push service's status. A subscription that's gone is forgotten.
async fn post(app: &App, endpoint: &str, body: Vec<u8>, ttl: u32, urgency: Option<&str>) -> Result<u16, ApiError> {
    let url = url::Url::parse(endpoint).map_err(|_| err(StatusCode::BAD_REQUEST, "endpoint"))?;
    // RFC 8292: the push resource's origin, with its port if it has one.
    let audience = url.origin().ascii_serialization();
    let urgency = match urgency {
        Some(u @ ("very-low" | "low" | "normal" | "high")) => u.to_owned(),
        _ => "high".to_owned(),
    };
    let res = app
        .http
        .post(url)
        .header("TTL", ttl.min(86_400).to_string())
        .header("Urgency", urgency)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header(
            "Authorization",
            format!("vapid t={}, k={}", app.vapid.jwt(&audience, &app.cfg.public_url), app.vapid.public()),
        )
        .body(body)
        .send()
        .await
        .map_err(|e| {
            warn!(error = %e, "push service unreachable");
            err(StatusCode::BAD_GATEWAY, "the push service didn't answer")
        })?;
    let status = res.status().as_u16();
    if status == 404 || status == 410 {
        app.db.drop_push_sub(endpoint, None)?;
    }
    Ok(status)
}

/// Tell these accounts' devices (those with push on) that something waits
/// for them (#104): `title`, `body`, and a tap that opens control's page,
/// where the prompt is. In the background; a failure is only logged.
pub fn notify(app: &Arc<App>, accounts: Vec<String>, tag: &str, title: String, body: String) {
    let app = app.clone();
    let msg = json!({ "title": title, "body": body, "tag": tag, "control": true }).to_string();
    tokio::spawn(async move {
        for account in accounts {
            let subs = match app.db.push_subs(&account) {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "push subscriptions");
                    continue;
                }
            };
            for sub in subs {
                let Ok(sub) = serde_json::from_value::<PushSub>(sub) else { continue };
                if let Err(e) = send_own(&app, &sub, msg.as_bytes()).await {
                    warn!(error = e.1, %account, "control's notice didn't go");
                }
            }
        }
    });
}

async fn send_own(app: &App, sub: &PushSub, msg: &[u8]) -> Result<(), ApiError> {
    app.limits.check_all(crate::limit::PUSHES)?;
    let bad = |_| err(StatusCode::BAD_REQUEST, "subscription keys");
    let (ua, auth) = (B64.decode(&sub.p256dh).map_err(bad)?, B64.decode(&sub.auth).map_err(bad)?);
    let body =
        illogical_e2e::push::encrypt(msg, &ua, &auth, &illogical_e2e::push::ephemeral(), &illogical_e2e::random())
            .map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let status = post(app, &sub.endpoint, body, 3600, Some("high")).await?;
    info!(account = sub.account, status, "control's notice sent");
    Ok(())
}

pub async fn daemon_send(State(app): State<Arc<App>>, d: DaemonAuth, Json(b): Json<Send>) -> R {
    let Some(account) = app.db.push_sub_account(&b.endpoint)? else {
        return Err(err(StatusCode::NOT_FOUND, "no such subscription"));
    };
    if !served(&app, &d.cert.device, &d.cert.account)?.contains(&account) {
        return Err(err(StatusCode::FORBIDDEN, "not someone this daemon serves"));
    }
    // A brake on how much one daemon has control relay, and on all of them.
    app.limits.check_daemon(crate::limit::DAEMON_PUSHES, &d.cert.device)?;
    app.limits.check_all(crate::limit::PUSHES)?;
    let body = base64::engine::general_purpose::STANDARD
        .decode(&b.body)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "body: base64"))?;
    if body.len() > 4096 {
        return Err(err(StatusCode::BAD_REQUEST, "a notification is at most 4 KB"));
    }
    let status = post(&app, &b.endpoint, body, b.ttl, b.urgency.as_deref()).await?;
    // Only who and how it went; never what (we couldn't read it anyway).
    info!(daemon = d.cert.device, %account, status, "push relayed");
    Ok(Json(json!({ "status": status })))
}

#[cfg(test)]
mod tests {
    #[test]
    fn push_services_only() {
        let ok = |e: &str| {
            let u = url::Url::parse(e).unwrap();
            u.scheme() == "https"
                && super::SERVICES
                    .iter()
                    .any(|s| u.host_str().unwrap() == *s || u.host_str().unwrap().ends_with(&format!(".{s}")))
        };
        assert!(ok("https://fcm.googleapis.com/fcm/send/x"));
        assert!(ok("https://updates.push.services.mozilla.com/wpush/v2/x"));
        assert!(ok("https://web.push.apple.com/x"));
        assert!(!ok("https://evil.example/fcm.googleapis.com"));
        assert!(!ok("http://fcm.googleapis.com/x"));
        assert!(!ok("https://169.254.169.254/latest"));
    }
}
