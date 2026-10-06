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
    response::{IntoResponse, Response},
};
use illogical_e2e::{
    Cert, Kind, Revocation, Trust,
    cert::{join_code, normalize_code},
    now_ms,
    team::{TeamPin, TeamRole},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::warn;

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

/// A refused enrolment or approval, in the log (#94): which request, the
/// reason, and the ids. Never a signature or a body.
/// The ids are the request's own, so they're clipped and quoted.
fn refused(what: &str, account: &str, c: &Cert, why: &str) {
    let clip = |s: &str| s.chars().take(64).collect::<String>();
    warn!(what, account, device = ?clip(&c.device), kind = c.kind.as_str(), approver = ?clip(&c.approver), why, "refused");
}

/// `cert` checks out against the account's trusted devices. `what` names
/// the request, for the log.
fn approval_ok(app: &App, account: &str, cert: &Cert, what: &str) -> Result<(), ApiError> {
    let (trust, mut certs, revs) = trusted(app, account)?;
    let Some(trust) = trust else {
        refused(what, account, cert, "the account has no devices yet");
        return Err(err(StatusCode::CONFLICT, "this account has no devices yet"));
    };
    // A removed key never counts again (#330): say so, not that it
    // doesn't chain.
    if revs.iter().any(|r| r.device == cert.device) {
        refused(what, account, cert, "this device was removed from the account (revoked): it needs a new key");
        return Err(err(
            StatusCode::FORBIDDEN,
            "that key was removed from this account, so it can't be approved again: it needs a new key",
        ));
    }
    certs.push(cert.clone());
    if trust.evaluate(&certs, &revs).get(&cert.device).is_none_or(|c| c != cert) {
        certs.pop();
        let why = match trust.evaluate(&certs, &revs).get(&cert.approver) {
            None => "the approver isn't a device this account trusts",
            Some(a) if !cert.signed_by(a) => "the signature doesn't verify with the approver's key",
            Some(a) if !a.kind.approves() => "the approver can't approve",
            Some(_) => "it doesn't chain to the account's root (form, kind or time)",
        };
        refused(what, account, cert, why);
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
    if let Err(e) = c.check_request() {
        refused("enroll", &s.account, &c, &e.to_string());
        return Err(err(StatusCode::BAD_REQUEST, &e.to_string()));
    }
    if c.account != s.account || !c.kind.connects() {
        refused("enroll", &s.account, &c, "not a browser or CLI certificate for this account");
        return Err(err(StatusCode::BAD_REQUEST, "a browser or CLI certificate for your account"));
    }
    let account = app.db.account(&s.account)?.ok_or_else(|| err(StatusCode::UNAUTHORIZED, "no such account"))?;
    if let Some((have, true)) = app.db.device(&s.account, &c.device)? {
        return Ok(Json(json!({ "approved": true, "cert": have, "root": account.root })));
    }
    match account.root {
        None => {
            if let Err(e) = c.check_form() {
                refused("enroll", &s.account, &c, &e.to_string());
                return Err(err(StatusCode::BAD_REQUEST, &e.to_string()));
            }
            if c.approver != c.device || !c.signed_by(&c) {
                refused("enroll", &s.account, &c, "the first device's certificate isn't signed by itself");
                return Err(err(StatusCode::BAD_REQUEST, "the first device signs its own certificate"));
            }
            app.db.put_device(&c, true, now_ms())?;
            Ok(Json(json!({ "approved": true, "cert": c, "root": c.device })))
        }
        Some(root) => {
            let new = app.db.device(&s.account, &c.device)?.is_none();
            if new {
                app.limits.check_account(crate::limit::ENROLLS, &s.account)?;
            }
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
    /// New codes replace the old (#106): a revocation for each one still
    /// good, signed by the device making the new ones.
    #[serde(default)]
    revocations: Vec<Revocation>,
}

/// Recovery codes: certificates a device signed for keys only the person
/// holds (on paper). Control never sees the keys. The first device makes
/// them; any device can make new ones, which retire the old.
pub async fn add_recovery(State(app): State<Arc<App>>, s: Session, Json(b): Json<RecoveryCerts>) -> R {
    if b.certs.is_empty() || b.certs.len() > 4 {
        return Err(err(StatusCode::BAD_REQUEST, "one to 4 recovery codes"));
    }
    for c in &b.certs {
        if c.kind != Kind::Recovery || c.account != s.account {
            return Err(err(StatusCode::BAD_REQUEST, "recovery certificates for your account"));
        }
        approval_ok(&app, &s.account, c, "recovery")?;
    }
    let (trust, certs, revs) = trusted(&app, &s.account)?;
    let now = trust.map(|t| t.evaluate(&certs, &revs)).unwrap_or_default();
    for r in &b.revocations {
        let ok = now.get(&r.device).is_some_and(|c| c.kind == Kind::Recovery)
            && now.get(&r.by).is_some_and(|c| c.kind.connects() && r.account == s.account && r.signed_by(c));
        if !ok {
            return Err(err(StatusCode::FORBIDDEN, "that revocation doesn't check out"));
        }
    }
    let old = now.devices.values().filter(|c| c.kind == Kind::Recovery).count();
    let retired = now
        .devices
        .values()
        .filter(|c| c.kind == Kind::Recovery && b.revocations.iter().any(|r| r.device == c.device))
        .count();
    if retired != old {
        return Err(err(StatusCode::CONFLICT, "new recovery codes replace the old ones: revoke every one still good"));
    }
    for c in &b.certs {
        app.db.put_device(c, true, now_ms())?;
    }
    for r in &b.revocations {
        app.db.add_revocation(r)?;
    }
    if !b.revocations.is_empty() {
        nudge(&app, &s.account);
    }
    Ok(Json(json!({})))
}

pub async fn devices(State(app): State<Arc<App>>, s: Session) -> R {
    let (trust, certs, revs) = trusted(&app, &s.account)?;
    let (_, pending) = app.db.devices(&s.account)?;
    Ok(Json(json!({ "trust": trust, "certs": certs, "revocations": revs, "pending": pending })))
}

pub async fn device(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>) -> Result<Response, ApiError> {
    let Some((cert, approved)) = app.db.device(&s.account, &id)? else {
        // Turned down (#105): say by which device, for the one waiting.
        let by = app.db.turned_down(&s.account, &id)?;
        let body = match by {
            Some(by) => json!({ "error": "this device's request was turned down", "turned_down": true, "by": by }),
            None => json!({ "error": "no such device" }),
        };
        return Ok((StatusCode::NOT_FOUND, Json(body)).into_response());
    };
    Ok(Json(json!({ "approved": approved, "cert": cert })).into_response())
}

pub async fn approve(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>, Json(b): Json<Enroll>) -> R {
    let (pending, approved) =
        app.db.device(&s.account, &id)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such device"))?;
    if approved {
        return Ok(Json(json!({ "approved": true })));
    }
    if b.cert.device != id || b.cert.account != s.account || !b.cert.same_request(&pending) {
        refused("approve", &s.account, &b.cert, "not the certificate that asked");
        return Err(err(StatusCode::BAD_REQUEST, "that's not the certificate that asked"));
    }
    approval_ok(&app, &s.account, &b.cert, "approve")?;
    app.db.put_device(&b.cert, true, now_ms())?;
    nudge(&app, &s.account);
    Ok(Json(json!({ "approved": true })))
}

#[derive(Deserialize, Default)]
pub struct Reject {
    /// The device turning it down, to name to the one waiting.
    #[serde(default)]
    by: Option<String>,
}

pub async fn reject(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>, b: Option<Json<Reject>>) -> R {
    let by = b.unwrap_or_default().0.by;
    let name = match by {
        Some(by) => app.db.device(&s.account, &by)?.filter(|(_, ok)| *ok).map(|(c, _)| c.name).unwrap_or_default(),
        None => String::new(),
    };
    app.db.turn_down(&s.account, &id, &name, now_ms())?;
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
    // Only its own: a revoked machine is refused everywhere (#330), so
    // another account's isn't for this one to remove.
    if !app.db.is_accounts(&s.account, &r.device)? {
        return Err(err(StatusCode::FORBIDDEN, "that device isn't this account's"));
    }
    // A revoked daemon leaves the directory and the relay.
    if app.db.daemon_account(&r.device)?.as_deref() == Some(s.account.as_str()) {
        app.db.add_machine_revocation(&r)?;
        nudge(&app, &s.account);
        app.db.drop_daemon(&r.device)?;
        app.relay.drop_daemon(&r.device);
    } else {
        app.db.add_revocation(&r)?;
        nudge(&app, &s.account);
    }
    Ok(Json(json!({})))
}

/// Why a removed key can't come back (#330), when `device` was revoked:
/// the words, and when and by which device (that by name only with
/// `detail`, for the key's holder).
pub fn removed(app: &App, device: &str, kind: Kind, detail: bool) -> anyhow::Result<Option<(String, Value)>> {
    let Some(r) = app.db.revoked(device)? else { return Ok(None) };
    let day = crate::day(r.at);
    if !detail {
        let msg = format!("this key was removed from its account on {day}: it needs a new key, then join again");
        return Ok(Some((msg, json!({ "at": r.at }))));
    }
    let what = if kind == Kind::Daemon { "this machine" } else { "this device" };
    let by = app.db.device(&r.account, &r.by)?.map(|(c, _)| c.name).filter(|n| !n.is_empty());
    let by_words = by.as_deref().map(|n| format!(" by {n}")).unwrap_or_default();
    let msg = format!(
        "{what} was removed from its account on {day}{by_words}, so its key can't join again: it needs a new key"
    );
    Ok(Some((msg, json!({ "at": r.at, "by": by }))))
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
    /// What the daemon understands, comma-separated (older ones send none).
    #[serde(default)]
    features: String,
    /// Its signature with the key it asks with (0.17 and newer).
    #[serde(default)]
    proof: Option<JoinProof>,
}

#[derive(Deserialize)]
pub struct JoinProof {
    ms: u64,
    sig: String,
}

/// How far a join proof's time may be from control's clock.
const PROOF_SKEW_MS: u64 = 5 * 60 * 1000;

/// What an older daemon is told when a join would need its key.
const JOIN_NEEDS_UPDATE: &str =
    "this machine was joined before: update illogical (0.17 or newer) on it, then run join again";

/// Whether the join request comes from the key's holder: an error if it
/// says so and doesn't, `false` if it doesn't say (an older daemon).
fn join_proven(app: &App, b: &JoinReq) -> Result<bool, ApiError> {
    let Some(p) = &b.proof else { return Ok(false) };
    let body = illogical_e2e::cert::join_proof_body(&b.cert, p.ms);
    if !illogical_e2e::cert::verify_hex(&b.cert.sign, body.as_bytes(), &p.sig) {
        return Err(err(StatusCode::UNAUTHORIZED, "the join request isn't signed by the key it asks with"));
    }
    if now_ms().abs_diff(p.ms) > PROOF_SKEW_MS {
        return Err(err(StatusCode::UNAUTHORIZED, "clock skew: check this machine's time"));
    }
    if !app.daemon_sigs.first(&format!("join {}", p.sig), p.ms + PROOF_SKEW_MS + 60_000) {
        return Err(err(StatusCode::UNAUTHORIZED, "that join request was used already; run join again"));
    }
    Ok(true)
}

/// A daemon that can't check this team's rosters (one was written with a
/// presigned invite) doesn't go into it.
fn can_follow(app: &App, team: &str, features: &str, name: &str) -> Result<(), ApiError> {
    if !crate::teams::takes_presigned(features) && crate::teams::has_presigned(app, team)? {
        return Err(err(
            StatusCode::CONFLICT,
            &format!(
                "{name} needs an update before it can join this team (its illogical is older than the team's invites)"
            ),
        ));
    }
    Ok(())
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
) -> Result<Response, ApiError> {
    app.limits.check(crate::limit::JOINS, app.limits.client_ip(peer, &headers))?;
    b.cert.check_request().map_err(|e| err(StatusCode::BAD_REQUEST, &e.to_string()))?;
    // A daemon, or the CLI (M49), which joins the same way: a code shown
    // where it runs, approved on a signed-in device.
    match b.cert.kind {
        Kind::Daemon => {}
        Kind::Cli if b.urls.is_empty() && b.team.is_none() && b.ticket.is_none() && b.proof.is_some() => {}
        Kind::Cli => {
            return Err(err(StatusCode::BAD_REQUEST, "a CLI joins with its key's proof and nothing else"));
        }
        _ => return Err(err(StatusCode::BAD_REQUEST, "a daemon certificate")),
    }
    check_urls(&b.urls)?;
    // A daemon control knows joins again only with its key: anyone may
    // have its certificate.
    let proven = join_proven(&app, &b)?;
    // A removed key never joins again (#330), and gets no code: it says
    // when it was removed (and by which device, to the key's holder), and
    // that it needs a new key. `removed` says so to the program asking.
    if let Some((msg, removed)) = removed(&app, &b.cert.device, b.cert.kind, proven)? {
        return Ok((StatusCode::GONE, Json(json!({ "error": msg, "removed": removed }))).into_response());
    }
    if !proven && app.db.device_known(&b.cert.device)? {
        return Err(err(StatusCode::UPGRADE_REQUIRED, JOIN_NEEDS_UPDATE));
    }
    let code = join_code(&b.cert);
    let poll = token();
    let team_name = match &b.team {
        Some(t) => {
            let name = app.db.team(t)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?.name;
            can_follow(&app, t, &b.features, "this machine")?;
            Some(name)
        }
        None => None,
    };
    let sandbox = match &b.ticket {
        Some(t) => {
            Some(crate::sandboxes::ticket(&app, t)?.ok_or_else(|| err(StatusCode::FORBIDDEN, "that ticket is spent"))?)
        }
        None => None,
    };
    app.db.add_join(
        &code,
        &b.cert,
        &hash(&poll),
        &b.urls,
        b.team.as_deref(),
        sandbox.as_deref(),
        &b.features,
        proven,
        now_ms(),
    )?;
    Ok(Json(
        json!({ "code": code, "poll": poll, "expires_in_secs": crate::db::JOIN_TTL_MS / 1000, "team_name": team_name }),
    )
    .into_response())
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
        .ok_or_else(|| {
            warn!(what = "join_approve", account = %s.account, why = "no such code (expired, used or turned down)", "refused");
            err(StatusCode::NOT_FOUND, "no such code (expired?)")
        })?;
    let c = b.cert;
    if c.account != s.account || c.kind != j.cert.kind || !c.same_request(&j.cert) {
        refused("join_approve", &s.account, &c, "not the daemon that asked");
        return Err(err(StatusCode::BAD_REQUEST, "that's not the daemon that asked"));
    }
    approval_ok(&app, &s.account, &c, "join_approve")?;
    // The CLI (M49): one of the account's devices from now on, like a
    // browser it approved. The account's machines learn of it now.
    if c.kind == Kind::Cli {
        if b.team.is_some() {
            return Err(err(StatusCode::BAD_REQUEST, "a CLI joins your account, not a team"));
        }
        app.db.put_device(&c, true, now_ms())?;
        app.db.approve_join(&code, &c, None)?;
        nudge(&app, &s.account);
        return Ok(Json(json!({ "approved": true, "device": c.device })));
    }
    let known = app.db.device_known(&c.device)?;
    if known && !j.proven {
        refused("join_approve", &s.account, &c, "a known daemon's join without its key");
        return Err(err(StatusCode::CONFLICT, JOIN_NEEDS_UPDATE));
    }
    // A team's machine: any member adds one of their own (#332), and the
    // approving device signs it in, for the daemon to check.
    let team = match &b.team {
        Some(id) => {
            let pin = in_team(&app, &s.account, id, false, "only the team's members add machines to it")?;
            can_follow(&app, id, &j.features, &c.name)?;
            let sig = b.team_sig.as_deref().unwrap_or_default();
            let (trust, certs, revs) = trusted(&app, &s.account)?;
            let approver = trust.and_then(|t| t.evaluate(&certs, &revs).get(&c.approver).cloned());
            if !approver.is_some_and(|a| pin.join_signed_by(&c.device, &a, sig)) {
                refused("join_approve", &s.account, &c, "the team choice isn't signed by the approver");
                return Err(err(StatusCode::BAD_REQUEST, "the team choice isn't signed by the approving device"));
            }
            Some((pin.team, sig.to_owned()))
        }
        None => None,
    };
    // Its holder joined it again: whatever it was before goes.
    if known {
        app.db.drop_daemon(&c.device)?;
        app.relay.drop_daemon(&c.device);
    }
    app.db.put_device(&c, true, now_ms())?;
    app.db.put_daemon(&s.account, &c.device, &c.name, &j.urls)?;
    app.db.set_daemon_features(&c.device, &j.features)?;
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

/// A team `account` is in, as a daemon pins it; `no` when it isn't. Any
/// member adds their own machines (#332), but only owners while it's
/// locked; with `owner`, only owners at all.
fn in_team(app: &App, account: &str, id: &str, owner: bool, no: &str) -> Result<TeamPin, ApiError> {
    let t = app.db.team(id)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
    let r = app.db.latest_roster(id)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
    let r: illogical_e2e::team::Roster = serde_json::from_str(&r).map_err(anyhow::Error::from)?;
    let Some(role) = r.member(account).map(|m| m.role) else { return Err(err(StatusCode::FORBIDDEN, no)) };
    if role != TeamRole::Owner && (owner || t.locked) {
        return Err(err(
            StatusCode::FORBIDDEN,
            if owner { no } else { "the team is locked: only its owners add machines until it's unlocked" },
        ));
    }
    Ok(TeamPin { team: t.id, founder: t.founder, founder_root: t.founder_root })
}

/// How far a move's time may be from control's clock.
const MOVE_SKEW_MS: u64 = 10 * 60 * 1000;

/// *Move to…* on a machine (#100): its own account moves it into a team
/// it's in, between them, or back to the account (#332: members too).
/// A device of the account signs it; the daemon checks. A team's owners
/// may also take a member's machine out, signed by one of their devices,
/// which the daemon checks against the roster.
pub async fn move_daemon(
    State(app): State<Arc<App>>,
    s: Session,
    Path(id): Path<String>,
    Json(m): Json<illogical_e2e::team::Move>,
) -> R {
    let (owner, d) = app.db.daemon_row(&id)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such machine"))?;
    let now = app.db.daemon_team(&id)?;
    if owner != s.account {
        let Some(t) = now.as_ref().filter(|_| m.team.is_none()) else {
            return Err(err(StatusCode::FORBIDDEN, "only the machine's own account moves it"));
        };
        in_team(&app, &s.account, t, true, "only the machine's own account and the team's owners take it out")?;
        if !crate::teams::has_feature(&app.db.daemon_features(&id)?, crate::teams::OWNER_MOVES) {
            return Err(err(
                StatusCode::CONFLICT,
                &format!(
                    "{} runs an older illogical: its owner updates it, then the team's owners can take it out",
                    d.name
                ),
            ));
        }
    }
    let pin = match &m.team {
        Some(p) => Some(in_team(&app, &s.account, &p.team, false, "only the team's members add machines to it")?),
        None => None,
    };
    if pin != m.team {
        return Err(err(StatusCode::BAD_REQUEST, "that's not the team's founder"));
    }
    if let Some(p) = &m.team
        && now.as_ref() != Some(&p.team)
    {
        can_follow(&app, &p.team, &app.db.daemon_features(&id)?, &d.name)?;
    }
    let last = app
        .db
        .daemon_moved(&id)?
        .and_then(|j| serde_json::from_str::<illogical_e2e::team::Move>(&j).ok())
        .map_or(0, |l| l.at);
    if m.at <= last || m.at.abs_diff(now_ms()) > MOVE_SKEW_MS {
        return Err(err(StatusCode::BAD_REQUEST, "that move is out of date; check this device's clock"));
    }
    let (trust, certs, revs) = trusted(&app, &s.account)?;
    let by = trust.and_then(|t| t.evaluate(&certs, &revs).get(&m.by).cloned());
    if !by.is_some_and(|by| m.signed_for(&id, &by)) {
        return Err(err(StatusCode::BAD_REQUEST, "the move isn't signed by one of your devices"));
    }
    app.db.move_daemon(
        &id,
        m.team.as_ref().map(|p| p.team.as_str()),
        &serde_json::to_string(&m).map_err(anyhow::Error::from)?,
    )?;
    app.relay.nudge(&[id]);
    Ok(Json(json!({})))
}

// ---------------------------------------------------------------- daemons

/// The daemon's account's certificates, to evaluate against its root.
pub async fn daemon_trust(State(app): State<Arc<App>>, d: DaemonAuth, Query(q): Query<crate::teams::Features>) -> R {
    app.db.set_daemon_features(&d.cert.device, &q.features)?;
    let (trust, certs, revs) = trusted(&app, &d.cert.account)?;
    // Its last move (#100), for the daemon to check and take.
    let moved: Option<Value> = app.db.daemon_moved(&d.cert.device)?.and_then(|j| serde_json::from_str(&j).ok());
    Ok(Json(json!({ "trust": trust, "certs": certs, "revocations": revs, "moved": moved })))
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
    // Sessions someone offers to share with me, for me to answer first.
    let mut offers = Vec::new();
    for id in app.db.offers_for(&s.account)? {
        let Some((owner, d)) = app.db.daemon_row(&id)? else { continue };
        let Some(who) = app.db.account(&owner)? else { continue };
        offers.push(json!({ "daemon": d.id, "name": d.name, "account": owner, "owner_name": who.name, "owner_login": who.login }));
    }
    Ok(Json(json!({ "daemons": daemons, "offers": offers })))
}
