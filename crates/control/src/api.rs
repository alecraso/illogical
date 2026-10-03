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
    team::TeamPin,
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
    Ok(Json(json!({ "account": a.id, "login": a.login, "name": a.name, "root": a.root, "passkeys": passkeys })))
}

/// A display name as people type it: trimmed, spaces folded, no control
/// characters, 1 to 64 characters.
pub fn display_name(s: &str) -> Result<String, ApiError> {
    let name = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(err(StatusCode::BAD_REQUEST, "a name of 1 to 64 characters"));
    }
    Ok(name)
}

#[derive(Deserialize)]
pub struct Name {
    name: String,
}

/// Change what other people see (#102). Rosters keep the name an owner
/// signed until they sign the next one.
pub async fn set_name(State(app): State<Arc<App>>, s: Session, Json(b): Json<Name>) -> R {
    let name = display_name(&b.name)?;
    app.db.set_name(&s.account, &name)?;
    // Daemons that let this account in call it by name.
    let mut ds: Vec<String> = app.db.daemons(&s.account)?.into_iter().map(|d| d.id).collect();
    ds.extend(crate::teams::reachable(&app, &s.account)?);
    app.relay.nudge(&ds);
    Ok(Json(json!({ "name": name })))
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
            let new = app.db.device(&s.account, &c.device)?.is_none();
            app.db.put_device(&Cert { approver: String::new(), sig: String::new(), ..c.clone() }, false, now_ms())?;
            // The account's other devices hear of it once (#104), not on
            // every reload of the waiting page.
            if new {
                crate::push::notify(
                    &app,
                    vec![s.account.clone()],
                    "control-device",
                    "A new browser wants into your account".into(),
                    "Open illogical to check its fingerprint and approve it.".into(),
                );
            }
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
    /// A hosted sandbox's one-time ticket (M20).
    #[serde(default)]
    ticket: Option<String>,
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
    let team_name = match &b.team {
        Some(t) => Some(app.db.team(t)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?.name),
        None => None,
    };
    let sandbox = match &b.ticket {
        Some(t) => {
            Some(crate::sandboxes::ticket(&app, t)?.ok_or_else(|| err(StatusCode::FORBIDDEN, "that ticket is spent"))?)
        }
        None => None,
    };
    app.db.add_join(&code, &b.cert, &hash(&poll), &b.urls, b.team.as_deref(), sandbox.as_deref(), now_ms())?;
    Ok(Json(
        json!({ "code": code, "poll": poll, "expires_in_secs": crate::db::JOIN_TTL_MS / 1000, "team_name": team_name }),
    ))
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
    if let Some(on) = j.rejected {
        app.db.drop_join(&code)?;
        return Ok(Json(json!({ "approved": false, "rejected": on })));
    }
    let Some(account) = j.account else { return Ok(Json(json!({ "approved": false }))) };
    let (trust, certs, revs) = trusted(&app, &account)?;
    app.db.drop_join(&code)?;
    // A team daemon pins the team's founder too, if the approver signed it in.
    let team = match &j.team {
        Some(t) => app.db.team(t)?.map(|t| {
            json!({ "team": t.id, "founder": t.founder, "founder_root": t.founder_root, "name": t.name, "sig": j.team_sig })
        }),
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
        .filter(|j| j.account.is_none() && j.rejected.is_none())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such code (expired?)"))?;
    // The team it asked for (`--team`): the approver may pick another.
    let team = match &j.team {
        Some(t) => app.db.team(t)?.map(|t| json!({ "team": t.id, "name": t.name })),
        None => None,
    };
    Ok(Json(json!({ "code": code, "cert": j.cert, "urls": j.urls, "created": j.created, "team": team })))
}

#[derive(Deserialize)]
pub struct JoinApprove {
    cert: Cert,
    /// The team the approver puts it in (one they own), and their signature
    /// over [`TeamPin::join_body`]; none for their own account (#100).
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    team_sig: Option<String>,
}

pub async fn join_approve(
    State(app): State<Arc<App>>,
    s: Session,
    Path(code): Path<String>,
    Json(b): Json<JoinApprove>,
) -> R {
    let code = normalize_code(&code).map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
    let j = app
        .db
        .join(&code, now_ms())?
        .filter(|j| j.account.is_none() && j.rejected.is_none())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such code (expired?)"))?;
    let c = b.cert;
    if c.account != s.account || c.kind != Kind::Daemon || !c.same_request(&j.cert) {
        return Err(err(StatusCode::BAD_REQUEST, "that's not the daemon that asked"));
    }
    approval_ok(&app, &s.account, &c)?;
    // A team's machine: only its owners add one, and the approving device
    // signs it in, for the daemon to check.
    let team = match &b.team {
        Some(id) => {
            let t = app.db.team(id)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
            let r = app.db.latest_roster(id)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
            let r: illogical_e2e::team::Roster = serde_json::from_str(&r).map_err(anyhow::Error::from)?;
            if r.member(&s.account).map(|m| m.role) != Some(illogical_e2e::team::TeamRole::Owner) {
                return Err(err(StatusCode::FORBIDDEN, "only the team's owners add its machines"));
            }
            let sig = b.team_sig.as_deref().unwrap_or_default();
            let pin = TeamPin { team: t.id.clone(), founder: t.founder, founder_root: t.founder_root };
            let (trust, certs, revs) = trusted(&app, &s.account)?;
            let approver = trust.and_then(|t| t.evaluate(&certs, &revs).get(&c.approver).cloned());
            if !approver.is_some_and(|a| pin.join_signed_by(&c.device, &a, sig)) {
                return Err(err(StatusCode::BAD_REQUEST, "the team choice isn't signed by the approving device"));
            }
            Some((t.id, sig.to_owned()))
        }
        None => None,
    };
    app.db.put_device(&c, true, now_ms())?;
    app.db.put_daemon(&s.account, &c.device, &c.name, &j.urls)?;
    if let Some((team, _)) = &team {
        app.db.set_daemon_team(&c.device, team)?;
    }
    if let Some(sandbox) = &j.sandbox {
        app.db.set_sandbox_daemon(sandbox, &c.device)?;
        crate::sandboxes::enrolled(&app, sandbox, &c).await?;
    }
    app.db.approve_join(&code, &c, team.as_ref().map(|(t, sig)| (t.as_str(), sig.as_str())))?;
    Ok(Json(json!({ "approved": true, "daemon": c.device })))
}

#[derive(Deserialize)]
pub struct JoinReject {
    /// The device turning it down, to name to the daemon.
    #[serde(default)]
    device: String,
}

/// Cancel on the approval page: the daemon stops waiting (#100).
pub async fn join_reject(
    State(app): State<Arc<App>>,
    s: Session,
    Path(code): Path<String>,
    Json(b): Json<JoinReject>,
) -> R {
    let code = normalize_code(&code).map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
    app.db
        .join(&code, now_ms())?
        .filter(|j| j.account.is_none() && j.rejected.is_none())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "no such code (expired?)"))?;
    let on = app.db.device(&s.account, &b.device)?.map(|(c, _)| c.name).unwrap_or_else(|| "a device".into());
    app.db.reject_join(&code, &on)?;
    Ok(Json(json!({})))
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
            // A hosted sandbox is reached through its provider, which wakes it.
            let sandbox = app.db.sandbox_of_daemon(&d.id).ok().flatten();
            let online = app.relay.online(&d.id) || sandbox.is_some();
            // One of my machines may be a team's (I joined it for them, M30).
            let team = app.db.daemon_team(&d.id).ok().flatten();
            json!({ "id": d.id, "name": d.name, "urls": d.urls, "last_seen": d.last_seen, "online": online, "sandbox": sandbox, "team": team })
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
        let owner_name = app.db.account(&owner)?.map(|a| a.name).unwrap_or_default();
        daemons.push(json!({
            "id": d.id, "name": d.name, "urls": d.urls, "last_seen": d.last_seen, "online": online,
            "account": owner, "owner_name": owner_name, "team": team, "chain": crate::teams::chain_of(&app, &owner)?,
        }));
    }
    Ok(Json(json!({ "daemons": daemons })))
}
