//! Control's API: devices and approvals, daemons joining, the directory.
//!
//! Control checks what it's sent with the same rules daemons use
//! ([`illogical_e2e::Trust`]), so it never stores an approval that a daemon
//! would throw away. But it is not the authority: daemons and browsers
//! check again against the root they pinned.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use illogical_e2e::{
    Cert, Kind, Revocation, Trust,
    cert::{join_code, normalize_code},
    now_ms,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ApiError, App,
    auth::{DaemonAuth, Session, hash, token},
    err,
};

type R = Result<Json<Value>, ApiError>;

pub async fn me(State(app): State<Arc<App>>, s: Session) -> R {
    let a = app.db.account(&s.account)?.ok_or_else(|| err(StatusCode::UNAUTHORIZED, "no such account"))?;
    let passkeys = app.db.passkey_count(&a.id)?;
    Ok(Json(json!({ "account": a.id, "login": a.login, "root": a.root, "passkeys": passkeys })))
}

/// What the account trusts now, by control's own reckoning.
fn trusted(app: &App, account: &str) -> anyhow::Result<(Option<Trust>, Vec<Cert>, Vec<Revocation>)> {
    let root = app.db.account(account)?.and_then(|a| a.root);
    let (certs, _) = app.db.devices(account)?;
    let revs = app.db.revocations(account)?;
    Ok((root.map(|root| Trust { account: account.to_owned(), root }), certs, revs))
}

/// The account's daemons should look at its certificates again now.
fn nudge(app: &App, account: &str) {
    if let Ok(ds) = app.db.daemons(account) {
        app.relay.nudge(&ds.into_iter().map(|d| d.id).collect::<Vec<_>>());
    }
}

/// `cert` checks out against the account's trusted devices.
fn approval_ok(app: &App, account: &str, cert: &Cert) -> Result<(), ApiError> {
    let (trust, mut certs, revs) = trusted(app, account)?;
    let trust = trust.ok_or_else(|| err(StatusCode::CONFLICT, "this account has no devices yet"))?;
    certs.push(cert.clone());
    if trust.evaluate(&certs, &revs).get(&cert.device).is_none_or(|c| c != cert) {
        return Err(err(
            StatusCode::FORBIDDEN,
            "that approval doesn't check out (signed by a device this account trusts?)",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct Enroll {
    cert: Cert,
}

/// A browser or CLI asks to join the account. The first device is
/// self-signed and trusted on first use; later ones wait for an approval.
pub async fn enroll(State(app): State<Arc<App>>, s: Session, Json(b): Json<Enroll>) -> R {
    let c = b.cert;
    c.check_request().map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
    if c.account != s.account || !c.kind.connects() {
        return Err(err(StatusCode::BAD_REQUEST, "a browser or CLI certificate for your account"));
    }
    let account = app.db.account(&s.account)?.ok_or_else(|| err(StatusCode::UNAUTHORIZED, "no such account"))?;
    if let Some((have, true)) = app.db.device(&s.account, &c.device)? {
        return Ok(Json(json!({ "approved": true, "cert": have, "root": account.root })));
    }
    match account.root {
        None => {
            c.check_form().map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
            if c.approver != c.device || !c.signed_by(&c) {
                return Err(err(StatusCode::BAD_REQUEST, "the first device signs its own certificate"));
            }
            app.db.put_device(&c, true, now_ms())?;
            Ok(Json(json!({ "approved": true, "cert": c, "root": c.device })))
        }
        Some(root) => {
            app.db.put_device(&Cert { approver: String::new(), sig: String::new(), ..c.clone() }, false, now_ms())?;
            Ok(Json(json!({ "approved": false, "root": root })))
        }
    }
}

#[derive(Deserialize)]
pub struct RecoveryCerts {
    certs: Vec<Cert>,
}

/// The first device's recovery codes: certificates it signed for keys only
/// the person holds (on paper). Control never sees the keys.
pub async fn add_recovery(State(app): State<Arc<App>>, s: Session, Json(b): Json<RecoveryCerts>) -> R {
    if b.certs.len() > 4 {
        return Err(err(StatusCode::BAD_REQUEST, "at most 4 recovery codes"));
    }
    for c in &b.certs {
        if c.kind != Kind::Recovery || c.account != s.account {
            return Err(err(StatusCode::BAD_REQUEST, "recovery certificates for your account"));
        }
        approval_ok(&app, &s.account, c)?;
    }
    for c in &b.certs {
        app.db.put_device(c, true, now_ms())?;
    }
    Ok(Json(json!({})))
}

pub async fn devices(State(app): State<Arc<App>>, s: Session) -> R {
    let (trust, certs, revs) = trusted(&app, &s.account)?;
    let (_, pending) = app.db.devices(&s.account)?;
    Ok(Json(json!({ "trust": trust, "certs": certs, "revocations": revs, "pending": pending })))
}

pub async fn device(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>) -> R {
    let (cert, approved) =
        app.db.device(&s.account, &id)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such device"))?;
    Ok(Json(json!({ "approved": approved, "cert": cert })))
}

pub async fn approve(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>, Json(b): Json<Enroll>) -> R {
    let (pending, approved) =
        app.db.device(&s.account, &id)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such device"))?;
    if approved {
        return Ok(Json(json!({ "approved": true })));
    }
    if b.cert.device != id || b.cert.account != s.account || !b.cert.same_request(&pending) {
        return Err(err(StatusCode::BAD_REQUEST, "that's not the certificate that asked"));
    }
    approval_ok(&app, &s.account, &b.cert)?;
    app.db.put_device(&b.cert, true, now_ms())?;
    nudge(&app, &s.account);
    Ok(Json(json!({ "approved": true })))
}

pub async fn reject(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>) -> R {
    app.db.drop_pending(&s.account, &id)?;
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
pub struct Revoke {
    revocation: Revocation,
}

pub async fn revoke(State(app): State<Arc<App>>, s: Session, Json(b): Json<Revoke>) -> R {
    let r = b.revocation;
    let (trust, certs, revs) = trusted(&app, &s.account)?;
    let trust = trust.ok_or_else(|| err(StatusCode::CONFLICT, "no devices"))?;
    let now = trust.evaluate(&certs, &revs);
    let signer = now.get(&r.by).filter(|c| c.kind.approves() && r.account == s.account && r.signed_by(c));
    if signer.is_none() {
        return Err(err(StatusCode::FORBIDDEN, "that revocation doesn't check out"));
    }
    app.db.add_revocation(&r)?;
    nudge(&app, &s.account);
    // A revoked daemon leaves the directory and the relay.
    if app.db.daemon_account(&r.device)?.as_deref() == Some(s.account.as_str()) {
        app.db.drop_daemon(&r.device)?;
        app.relay.drop_daemon(&r.device);
    }
    Ok(Json(json!({})))
}

// ---------------------------------------------------------------- joining

#[derive(Deserialize)]
pub struct JoinReq {
    cert: Cert,
    #[serde(default)]
    urls: Vec<String>,
    /// A machine that belongs to a team (M19), not a person.
    #[serde(default)]
    team: Option<String>,
}

fn check_urls(urls: &[String]) -> Result<(), ApiError> {
    if urls.len() > 8 || urls.iter().any(|u| u.len() > 256 || !(u.starts_with("https://") || u.starts_with("http://")))
    {
        return Err(err(StatusCode::BAD_REQUEST, "at most 8 http(s) URLs"));
    }
    Ok(())
}

/// A daemon asks to join; no account yet. It gets the code to show, and
/// a token to poll with.
pub async fn join(
    State(app): State<Arc<App>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(b): Json<JoinReq>,
) -> R {
    app.limits.check(crate::limit::JOINS, app.limits.client_ip(peer, &headers))?;
    b.cert.check_request().map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
    if b.cert.kind != Kind::Daemon {
        return Err(err(StatusCode::BAD_REQUEST, "a daemon certificate"));
    }
    check_urls(&b.urls)?;
    let code = join_code(&b.cert);
    let poll = token();
    if let Some(t) = &b.team
        && app.db.team(t)?.is_none()
    {
        return Err(err(StatusCode::NOT_FOUND, "no such team"));
    }
    app.db.add_join(&code, &b.cert, &hash(&poll), &b.urls, b.team.as_deref(), now_ms())?;
    Ok(Json(json!({ "code": code, "poll": poll, "expires_in_secs": crate::db::JOIN_TTL_MS / 1000 })))
}

#[derive(Deserialize)]
pub struct Poll {
    poll: String,
}

/// The daemon waits for someone to approve its code.
pub async fn join_poll(State(app): State<Arc<App>>, Path(code): Path<String>, Query(q): Query<Poll>) -> R {
    let code = normalize_code(&code).map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let j =
        app.db.join(&code, now_ms())?.ok_or_else(|| err(StatusCode::NOT_FOUND, "that code expired; run join again"))?;
    if j.poll_hash != hash(&q.poll) {
        return Err(err(StatusCode::FORBIDDEN, "not your join"));
    }
    let Some(account) = j.account else { return Ok(Json(json!({ "approved": false }))) };
    let (trust, certs, revs) = trusted(&app, &account)?;
    app.db.drop_join(&code)?;
    // A team daemon pins the team's founder too.
    let team = match &j.team {
        Some(t) => app
            .db
            .team(t)?
            .map(|t| json!({ "team": t.id, "founder": t.founder, "founder_root": t.founder_root, "name": t.name })),
        None => None,
    };
    Ok(Json(
        json!({ "approved": true, "cert": j.cert, "trust": trust, "certs": certs, "revocations": revs, "team": team }),
    ))
}

/// What a signed-in person sees before approving a code.
pub async fn join_show(State(app): State<Arc<App>>, _s: Session, Path(code): Path<String>) -> R {
    let code = normalize_code(&code).map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let j = app
        .db
        .join(&code, now_ms())?
        .filter(|j| j.account.is_none())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such code (expired?)"))?;
    let team = match &j.team {
        Some(t) => app.db.team(t)?.map(|t| json!({ "team": t.id, "name": t.name })),
        None => None,
    };
    Ok(Json(json!({ "code": code, "cert": j.cert, "urls": j.urls, "created": j.created, "team": team })))
}

pub async fn join_approve(
    State(app): State<Arc<App>>,
    s: Session,
    Path(code): Path<String>,
    Json(b): Json<Enroll>,
) -> R {
    let code = normalize_code(&code).map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let j = app
        .db
        .join(&code, now_ms())?
        .filter(|j| j.account.is_none())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such code (expired?)"))?;
    let c = b.cert;
    if c.account != s.account || c.kind != Kind::Daemon || !c.same_request(&j.cert) {
        return Err(err(StatusCode::BAD_REQUEST, "that's not the daemon that asked"));
    }
    approval_ok(&app, &s.account, &c)?;
    app.db.put_device(&c, true, now_ms())?;
    // A team's machine: only its owners add one.
    if let Some(team) = &j.team {
        let r = app.db.latest_roster(team)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
        let r: illogical_e2e::team::Roster = serde_json::from_str(&r).map_err(anyhow::Error::from)?;
        if r.member(&s.account).map(|m| m.role) != Some(illogical_e2e::team::TeamRole::Owner) {
            return Err(err(StatusCode::FORBIDDEN, "only the team's owners add its machines"));
        }
    }
    app.db.put_device(&c, true, now_ms())?;
    app.db.put_daemon(&s.account, &c.device, &c.name, &j.urls)?;
    if let Some(team) = &j.team {
        app.db.set_daemon_team(&c.device, team)?;
    }
    app.db.approve_join(&code, &c)?;
    Ok(Json(json!({ "approved": true, "daemon": c.device })))
}

// ---------------------------------------------------------------- daemons

/// The daemon's account's certificates, to evaluate against its root.
pub async fn daemon_trust(State(app): State<Arc<App>>, d: DaemonAuth) -> R {
    let (trust, certs, revs) = trusted(&app, &d.cert.account)?;
    Ok(Json(json!({ "trust": trust, "certs": certs, "revocations": revs })))
}

pub async fn daemon_leave(State(app): State<Arc<App>>, d: DaemonAuth) -> R {
    app.db.drop_daemon(&d.cert.device)?;
    app.relay.drop_daemon(&d.cert.device);
    Ok(Json(json!({})))
}

// ---------------------------------------------------------------- directory

pub async fn directory(State(app): State<Arc<App>>, s: Session) -> R {
    let mut daemons: Vec<Value> = app
        .db
        .daemons(&s.account)?
        .into_iter()
        .map(|d| {
            let online = app.relay.online(&d.id);
            json!({ "id": d.id, "name": d.name, "urls": d.urls, "last_seen": d.last_seen, "online": online })
        })
        .collect();
    // Teams' machines and those shared with me (M19), with their owner
    // account's certificates to check them by.
    for id in crate::teams::reachable(&app, &s.account)? {
        let Some((owner, d)) = app.db.daemon_row(&id)? else { continue };
        if owner == s.account {
            continue;
        }
        let online = app.relay.online(&d.id);
        let team = app.db.daemon_team(&d.id)?;
        let owner_name = app.db.account(&owner)?.map(|a| a.login).unwrap_or_default();
        daemons.push(json!({
            "id": d.id, "name": d.name, "urls": d.urls, "last_seen": d.last_seen, "online": online,
            "account": owner, "owner_name": owner_name, "team": team, "chain": crate::teams::chain_of(&app, &owner)?,
        }));
    }
    Ok(Json(json!({ "daemons": daemons })))
}
