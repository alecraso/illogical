//! Billing (M22), only when control has a Stripe key: self-hosted control
//! has none and never bills.
//!
//! - **Personal** is free: one person, any number of daemons, relay
//!   traffic up to a monthly allowance.
//! - **Team** is per seat (each member in the signed roster).
//! - **Hosted sandboxes** are by the minute, on a paid plan.
//!
//! Upgrading goes through Stripe Checkout; Stripe tells control what
//! happened through signed webhooks. Seat counts follow the roster;
//! sandbox minutes go to Stripe as meter events. Over the relay allowance
//! a free account sees a warning, and past twice that its relayed traffic
//! slows down. With billing on, hosted sandboxes need a paid plan, and
//! running ones are never stopped for it.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use illogical_e2e::now_ms;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use tracing::{info, warn};

use crate::{ApiError, App, auth::Session, err};

type R = Result<Json<Value>, ApiError>;

pub struct Stripe {
    pub api: String,
    pub secret: String,
    pub webhook_secret: String,
    /// The per-seat price (a team's subscription).
    pub seat_price: String,
    /// The metered price for sandbox minutes.
    pub minutes_price: String,
    /// The meter's event name for sandbox minutes.
    pub minutes_event: String,
}

/// Free relay traffic per account per month.
pub fn relay_allowance(app: &App) -> u64 {
    app.cfg.relay_free_bytes
}

impl Stripe {
    async fn post(&self, http: &reqwest::Client, path: &str, form: &[(String, String)]) -> anyhow::Result<Value> {
        let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(form).finish();
        let r = http
            .post(format!("{}{path}", self.api))
            .bearer_auth(&self.secret)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?;
        let status = r.status();
        let v: Value = r.json().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Stripe {path}: {status} {}", v["error"]["message"].as_str().unwrap_or(""));
        }
        Ok(v)
    }

    async fn get(&self, http: &reqwest::Client, path: &str) -> anyhow::Result<Value> {
        let r = http.get(format!("{}{path}", self.api)).bearer_auth(&self.secret).send().await?;
        let status = r.status();
        let v: Value = r.json().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("Stripe {path}: {status}");
        }
        Ok(v)
    }
}

fn stripe(app: &App) -> Result<&Stripe, ApiError> {
    app.stripe.as_ref().ok_or_else(|| err(StatusCode::NOT_FOUND, "billing isn't set up on this control"))
}

/// The month an instant falls in, `YYYY-MM`.
fn month(ms: u64) -> String {
    crate::day(ms)[..7].to_owned()
}

/// Who pays for something an account does: its team's plan if it's in an
/// active one, else its own.
pub fn paid(app: &App, account: &str) -> anyhow::Result<bool> {
    if app.db.billing(&format!("account:{account}"))?.is_some_and(|b| b.active()) {
        return Ok(true);
    }
    for body in app.db.teams_of(account)? {
        let r: illogical_e2e::team::Roster = serde_json::from_str(&body)?;
        if app.db.billing(&format!("team:{}", r.team))?.is_some_and(|b| b.active()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Relay standing for a free account: (bytes this month, over the
/// allowance, past twice it).
pub fn relay_standing(app: &App, account: &str) -> anyhow::Result<(u64, bool, bool)> {
    let used = app.db.relay_bytes_month(account, &month(now_ms()))?;
    if app.stripe.is_none() || paid(app, account)? {
        return Ok((used, false, false));
    }
    let cap = relay_allowance(app);
    Ok((used, used > cap, used > cap * 2))
}

/// `GET /api/billing`: my plan and my teams', and this month's usage.
pub async fn status(State(app): State<Arc<App>>, s: Session) -> R {
    let on = app.stripe.is_some();
    let now = now_ms();
    let since = month_start(now);
    let (relay, warn_relay, slow) = relay_standing(&app, &s.account)?;
    let mine = app.db.billing(&format!("account:{}", s.account))?;
    let mut teams = Vec::new();
    for body in app.db.teams_of(&s.account)? {
        let r: illogical_e2e::team::Roster = serde_json::from_str(&body)?;
        let b = app.db.billing(&format!("team:{}", r.team))?;
        let owner = r.member(&s.account).is_some_and(|m| m.role == illogical_e2e::team::TeamRole::Owner);
        let minutes: u64 = r.members.iter().map(|m| app.db.sandbox_minutes(&m.account, since, now).unwrap_or(0)).sum();
        teams.push(json!({
            "team": r.team, "name": r.name, "owner": owner, "seats": r.members.len(),
            "plan": if b.as_ref().is_some_and(|b| b.active()) { "team" } else { "free" },
            "status": b.map(|b| b.status), "sandbox_minutes": minutes,
        }));
    }
    Ok(Json(json!({
        "billing": on,
        "plan": if mine.as_ref().is_some_and(|b| b.active()) { "paid" } else { "personal" },
        "relay": { "bytes": relay, "allowance": relay_allowance(&app), "warning": warn_relay, "slowed": slow },
        "sandbox_minutes": app.db.sandbox_minutes(&s.account, since, now)?,
        "teams": teams,
    })))
}

fn month_start(ms: u64) -> u64 {
    let days = ms / 86_400_000;
    // Walk back to the 1st of this month.
    let mut d = days;
    while crate::day(d * 86_400_000)[8..] != *"01" {
        d -= 1;
    }
    d * 86_400_000
}

#[derive(Deserialize)]
pub struct Checkout {
    /// A team (its owners pay per seat); none for a personal plan.
    #[serde(default)]
    team: Option<String>,
}

/// `POST /api/billing/checkout`: a Stripe Checkout page to upgrade.
pub async fn checkout(State(app): State<Arc<App>>, headers: HeaderMap, s: Session, Json(b): Json<Checkout>) -> R {
    let st = stripe(&app)?;
    app.limits.check_account(crate::limit::CHECKOUTS, &s.account)?;
    let (owner, seats) = match &b.team {
        Some(t) => {
            let r: illogical_e2e::team::Roster = serde_json::from_str(
                &app.db.latest_roster(t)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?,
            )?;
            if r.member(&s.account).map(|m| m.role) != Some(illogical_e2e::team::TeamRole::Owner) {
                return Err(err(StatusCode::FORBIDDEN, "a team's owners pay for it"));
            }
            (format!("team:{t}"), r.members.len())
        }
        None => (format!("account:{}", s.account), 0),
    };
    let customer = match app.db.billing(&owner)?.and_then(|b| b.customer) {
        Some(c) => c,
        None => {
            let c = st.post(&app.http, "/v1/customers", &[("metadata[owner]".into(), owner.clone())]).await?;
            let id = c["id"].as_str().unwrap_or_default().to_owned();
            app.db.set_billing(&owner, Some(&id), None, "none", now_ms())?;
            id
        }
    };
    let mut form: Vec<(String, String)> = vec![
        ("mode".into(), "subscription".into()),
        ("customer".into(), customer),
        // Back to the site it was started on (#507).
        ("success_url".into(), format!("{}/#billing=done", app.cfg.site(&headers).url)),
        ("cancel_url".into(), format!("{}/#billing=cancelled", app.cfg.site(&headers).url)),
        ("metadata[owner]".into(), owner.clone()),
        ("subscription_data[metadata][owner]".into(), owner.clone()),
    ];
    let mut i = 0;
    if seats > 0 {
        form.push((format!("line_items[{i}][price]"), st.seat_price.clone()));
        form.push((format!("line_items[{i}][quantity]"), seats.to_string()));
        i += 1;
    }
    form.push((format!("line_items[{i}][price]"), st.minutes_price.clone()));
    let session = st.post(&app.http, "/v1/checkout/sessions", &form).await?;
    Ok(Json(json!({ "url": session["url"] })))
}

type HmacSha256 = hmac::Hmac<Sha256>;

/// Stripe's webhook signature: `t=<ts>,v1=<hex HMAC-SHA256 of "t.body">`,
/// within five minutes.
fn verify_signature(secret: &str, header: &str, body: &[u8], now_s: u64) -> bool {
    use hmac::{KeyInit, Mac};
    let mut t = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.split_once('=') {
            Some(("t", v)) => t = v.parse::<u64>().ok(),
            Some(("v1", v)) => sigs.push(v.to_owned()),
            _ => {}
        }
    }
    let Some(t) = t else { return false };
    if secret.is_empty() || now_s.abs_diff(t) > 300 {
        return false;
    }
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac");
    mac.update(format!("{t}.").as_bytes());
    mac.update(body);
    let want = hex::encode(mac.finalize().into_bytes());
    sigs.iter().any(|s| s.len() == want.len() && s.bytes().zip(want.bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0)
}

/// `POST /api/stripe/webhook`: what happened at Stripe.
pub async fn webhook(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Result<StatusCode, ApiError> {
    let st = stripe(&app)?;
    let sig = headers.get("stripe-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !verify_signature(&st.webhook_secret, sig, &body, now_ms() / 1000) {
        return Err(err(StatusCode::BAD_REQUEST, "bad signature"));
    }
    let ev: Value = serde_json::from_slice(&body)?;
    let obj = &ev["data"]["object"];
    let owner = obj["metadata"]["owner"].as_str().unwrap_or_default().to_owned();
    match ev["type"].as_str().unwrap_or("") {
        "checkout.session.completed" if !owner.is_empty() => {
            let sub = obj["subscription"].as_str().unwrap_or_default();
            app.db.set_billing(&owner, obj["customer"].as_str(), Some(sub), "active", now_ms())?;
            info!(owner, "upgraded");
            // The seat item, for keeping its quantity with the roster.
            if let Ok(s) = st.get(&app.http, &format!("/v1/subscriptions/{sub}")).await {
                for item in s["items"]["data"].as_array().into_iter().flatten() {
                    if item["price"]["id"] == st.seat_price.as_str() {
                        app.db.set_seat_item(&owner, item["id"].as_str().unwrap_or_default())?;
                    }
                }
            }
        }
        "customer.subscription.updated" | "customer.subscription.deleted" if !owner.is_empty() => {
            let status = obj["status"].as_str().unwrap_or("canceled");
            app.db.set_billing(&owner, obj["customer"].as_str(), obj["id"].as_str(), status, now_ms())?;
            info!(owner, status, "subscription changed");
        }
        _ => {}
    }
    Ok(StatusCode::OK)
}

/// A team's roster changed: its seats follow (M19 → M22).
pub async fn sync_seats(app: &App, team: &str, seats: usize) {
    let Some(st) = &app.stripe else { return };
    let owner = format!("team:{team}");
    let Ok(Some(b)) = app.db.billing(&owner) else { return };
    let (true, Some(item)) = (b.active(), b.seat_item) else { return };
    let form =
        [("quantity".to_owned(), seats.to_string()), ("proration_behavior".to_owned(), "create_prorations".to_owned())];
    match st.post(&app.http, &format!("/v1/subscription_items/{item}"), &form).await {
        Ok(_) => info!(team, seats, "seats updated"),
        Err(e) => warn!(team, error = %e, "can't update seats"),
    }
}

/// Report sandbox minutes not yet reported, per paying owner: hourly.
pub async fn report_usage(app: &App) {
    let Some(st) = &app.stripe else { return };
    let now = now_ms();
    let mut per_customer: HashMap<String, u64> = HashMap::new();
    for (account, customer_owner) in app.db.sandbox_accounts().unwrap_or_default().into_iter().filter_map(|a| {
        let owner = payer(app, &a)?;
        Some((a, owner))
    }) {
        let Ok(Some(b)) = app.db.billing(&customer_owner) else { continue };
        let active = b.active();
        let Some(customer) = b.customer.filter(|_| active) else { continue };
        let total = app.db.sandbox_minutes(&account, 0, now).unwrap_or(0);
        let reported = app.db.reported_minutes(&account).unwrap_or(0);
        if total > reported {
            *per_customer.entry(customer).or_default() += total - reported;
            let _ = app.db.set_reported_minutes(&account, total);
        }
    }
    for (customer, minutes) in per_customer {
        let form = [
            ("event_name".to_owned(), st.minutes_event.clone()),
            ("payload[stripe_customer_id]".to_owned(), customer.clone()),
            ("payload[value]".to_owned(), minutes.to_string()),
            ("timestamp".to_owned(), (now / 1000).to_string()),
        ];
        match st.post(&app.http, "/v1/billing/meter_events", &form).await {
            Ok(_) => info!(customer, minutes, "sandbox minutes reported"),
            Err(e) => warn!(error = %e, "can't report usage"),
        }
    }
}

/// Who pays for an account's sandboxes: its own plan, else its team's.
fn payer(app: &App, account: &str) -> Option<String> {
    let own = format!("account:{account}");
    if app.db.billing(&own).ok().flatten().is_some_and(|b| b.active()) {
        return Some(own);
    }
    for body in app.db.teams_of(account).ok()? {
        let r: illogical_e2e::team::Roster = serde_json::from_str(&body).ok()?;
        let t = format!("team:{}", r.team);
        if app.db.billing(&t).ok().flatten().is_some_and(|b| b.active()) {
            return Some(t);
        }
    }
    None
}

/// `POST /api/billing/report` (owners of billing on this control): report
/// usage now, rather than at the next hour.
pub async fn report_now(State(app): State<Arc<App>>, s: Session) -> R {
    stripe(&app)?;
    app.limits.check_account(crate::limit::REPORTS, &s.account)?;
    app.limits.check_all(crate::limit::ALL_REPORTS)?;
    report_usage(&app).await;
    Ok(Json(json!({})))
}

#[cfg(test)]
mod tests {
    use hmac::{KeyInit, Mac};

    use super::*;

    #[test]
    fn webhook_signatures() {
        let body = br#"{"type":"x"}"#;
        let mut mac = HmacSha256::new_from_slice(b"whsec_test").unwrap();
        mac.update(b"1000.");
        mac.update(body);
        let sig = hex::encode(mac.finalize().into_bytes());
        assert!(verify_signature("whsec_test", &format!("t=1000,v1={sig}"), body, 1100));
        assert!(!verify_signature("whsec_test", &format!("t=1000,v1={sig}"), body, 2000), "too old");
        assert!(!verify_signature("whsec_other", &format!("t=1000,v1={sig}"), body, 1100));
        assert!(!verify_signature("whsec_test", &format!("t=1000,v1={sig}"), br#"{"type":"y"}"#, 1100));
        // Never against an empty secret.
        let mut mac = HmacSha256::new_from_slice(b"").unwrap();
        mac.update(b"1000.");
        mac.update(body);
        let empty = hex::encode(mac.finalize().into_bytes());
        assert!(!verify_signature("", &format!("t=1000,v1={empty}"), body, 1100));
    }

    #[test]
    fn months() {
        let t = 1_790_000_000_000; // 2026-09-21
        assert_eq!(crate::day(month_start(t)), "2026-09-01");
        assert_eq!(month(t), "2026-09");
    }
}
