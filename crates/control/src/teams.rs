//! Teams and people (M19): finding someone to share with, teams with
//! signed rosters, invites, and which accounts a daemon lets in.
//!
//! Control stores rosters and checks them as daemons do (it never keeps
//! one a daemon would throw away), but it can't make one: every version is
//! signed by a team owner's device.
//!
//! **Who control routes to a daemon.** A daemon says which accounts it
//! lets in (`POST /api/daemon/access`), but that list is only a filter:
//! control routes an account to a daemon (the directory, the relay, push
//! notifications, certificates) only if the account has a say in it: it
//! owns the daemon, it's in the daemon's team, it's in a team with the
//! daemon's owner, or it accepted a session the daemon shares with it
//! (`POST /api/shares/{daemon}`). Anyone else a daemon names waits as an
//! offer, which the person sees with the owner's name and answers.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use illogical_e2e::{
    Cert, Revocation, Trust,
    team::{AccountCerts, Invite, Roster, TeamPin, TeamRole},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    ApiError, App,
    auth::{DaemonAuth, Session, hash, token},
    db::{Team, TeamRequest},
    err,
};

type R = Result<Json<Value>, ApiError>;

/// What a daemon that takes presigned invites' rosters says it understands.
pub const PRESIGNED_INVITES: &str = "presigned-invites";

/// How long a daemon counts as checking a team it was shared with, since
/// it last asked (they ask about once a minute).
const WATCH_TTL_MS: u64 = 7 * 86_400 * 1000;

/// The longest a presigned invite lasts: a day, and some slack for the
/// owner's clock.
const PRESIGNED_MAX_MS: u64 = 86_400 * 1000 + 10 * 60 * 1000;

/// Whether a daemon said it understands presigned invites' rosters.
pub fn takes_presigned(features: &str) -> bool {
    features.split(',').any(|f| f == PRESIGNED_INVITES)
}

/// The machines checking this team's rosters that don't understand
/// presigned invites: one would refuse the roster a redeem writes, and
/// then every later one, removals too.
fn behind(app: &App, team: &str, now: u64) -> anyhow::Result<Vec<String>> {
    Ok(app
        .db
        .team_followers(team, now.saturating_sub(WATCH_TTL_MS))?
        .into_iter()
        .filter(|(_, f)| !takes_presigned(f))
        .map(|(name, _)| name)
        .collect())
}

fn need_update(names: &[String], what: &str) -> String {
    format!("{} need{} an update before {what}", names.join(", "), if names.len() == 1 { "s" } else { "" })
}

/// Whether a version of this team's roster was written with a presigned
/// invite: a daemon from before them can't check its history.
pub fn has_presigned(app: &App, team: &str) -> anyhow::Result<bool> {
    Ok(app.db.rosters(team, 0)?.iter().any(|b| parse(b).is_ok_and(|r| r.v == 2)))
}

/// An account's certificates and revocations.
fn certs_of(app: &App, account: &str) -> anyhow::Result<(Vec<Cert>, Vec<Revocation>)> {
    Ok((app.db.devices(account)?.0, app.db.revocations(account)?))
}

fn certs_for<'a>(app: &App, accounts: impl Iterator<Item = &'a str>) -> anyhow::Result<AccountCerts> {
    let mut out = HashMap::new();
    for a in accounts {
        // A deleted account's signing devices, kept to check the history
        // it signed (#173).
        let certs = match app.db.retained_certs(a)? {
            Some(kept) if app.db.account(a)?.is_none() => kept,
            _ => certs_of(app, a)?,
        };
        out.insert(a.to_owned(), certs);
    }
    Ok(out)
}

/// Members' names as they set them (#208): a roster carries one word per
/// member (it's signed text), so "Sam Stranger" is "Sam-Stranger" there.
/// What to show; the roster's word stays what's checked.
fn names_of<'a>(app: &App, accounts: impl Iterator<Item = &'a str>) -> anyhow::Result<HashMap<String, String>> {
    let mut out = HashMap::new();
    for a in accounts {
        if let Some(x) = app.db.account(a)?.filter(|x| !x.name.is_empty()) {
            out.insert(a.to_owned(), x.name);
        }
    }
    Ok(out)
}

#[cfg(test)]
pub fn certs_for_test(app: &App, accounts: &[String]) -> AccountCerts {
    certs_for(app, accounts.iter().map(String::as_str)).unwrap()
}

fn parse(body: &str) -> anyhow::Result<Roster> {
    Ok(serde_json::from_str(body)?)
}

fn pin(t: &Team) -> TeamPin {
    TeamPin { team: t.id.clone(), founder: t.founder.clone(), founder_root: t.founder_root.clone() }
}

fn latest(app: &App, team: &str) -> Result<Roster, ApiError> {
    let body = app.db.latest_roster(team)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
    Ok(parse(&body)?)
}

fn role_in(r: &Roster, account: &str) -> Option<TeamRole> {
    r.member(account).map(|m| m.role)
}

/// The account's daemons should look at their trust again (a roster
/// changed, a team locked).
fn nudge_team(app: &App, team: &str) {
    let mut ds = app.db.team_daemons(team).unwrap_or_default();
    // Members' own machines may have shared sessions with the team (M30):
    // the members of this version and the one before (someone just left).
    if let Ok(Some(latest)) = app.db.latest_roster(team)
        && let Ok(r) = parse(&latest)
    {
        let mut accounts: Vec<String> = Vec::new();
        for b in app.db.rosters(team, r.version.saturating_sub(2)).unwrap_or_default() {
            if let Ok(r) = parse(&b) {
                accounts.extend(r.members.into_iter().map(|m| m.account));
            }
        }
        accounts.sort();
        accounts.dedup();
        for a in accounts {
            ds.extend(app.db.daemons(&a).unwrap_or_default().into_iter().map(|d| d.id));
        }
    }
    app.relay.nudge(&ds);
}

// ---------------------------------------------------------------- people

#[derive(Deserialize)]
pub struct Lookup {
    login: String,
}

/// Someone to share with, by their sign-in login or their name (#102):
/// their account and the root device to pin (compare its fingerprint with
/// them).
pub async fn person(
    State(app): State<Arc<App>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    s: Session,
    Query(q): Query<Lookup>,
) -> R {
    app.limits.check_account(crate::limit::PEOPLE, &s.account)?;
    app.limits.check(crate::limit::PEOPLE, app.limits.client_ip(peer, &headers))?;
    let asked = q.login.split_whitespace().collect::<Vec<_>>().join(" ");
    let a = match app.db.account_by_login(&asked)? {
        Some(a) => Some(a),
        None => match app.db.accounts_named(&asked)?.as_slice() {
            [one] => app.db.account(one)?,
            [] => None,
            _ => {
                return Err(err(
                    StatusCode::CONFLICT,
                    "more than one person goes by that name; ask them for their login",
                ));
            }
        },
    };
    let a = a
        .filter(|a| a.root.is_some())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "nobody by that name here yet (they sign in once first)"))?;
    Ok(Json(json!({ "account": a.id, "name": a.name, "root": a.root })))
}

/// A name for a roster or a request: one word (rosters are signed text).
pub fn member_name(a: &crate::db::Account) -> String {
    let words: Vec<&str> =
        a.name.split(|c: char| c.is_whitespace() || c.is_control()).filter(|w| !w.is_empty()).collect();
    let name: String = words.join("-").chars().take(120).collect();
    if name.is_empty() { format!("account-{}", &a.id[..6.min(a.id.len())]) } else { name }
}

// ---------------------------------------------------------------- teams

#[derive(Deserialize)]
pub struct NewTeam {
    roster: Roster,
}

pub async fn create(State(app): State<Arc<App>>, s: Session, Json(b): Json<NewTeam>) -> R {
    let me = app.db.account(&s.account)?.ok_or_else(|| err(StatusCode::UNAUTHORIZED, "no account"))?;
    let root = me.root.ok_or_else(|| err(StatusCode::CONFLICT, "enroll a device first"))?;
    let r = b.roster;
    if r.version != 1 || r.team.len() != 16 || app.db.team(&r.team)?.is_some() {
        return Err(err(StatusCode::BAD_REQUEST, "a new team starts at version 1 with a fresh id"));
    }
    // Others come in by invite: by asking, or with a link's key.
    if r.members.len() != 1 {
        return Err(err(StatusCode::BAD_REQUEST, "a new team has just you in it; invite the others"));
    }
    app.limits.check_account(crate::limit::TEAMS, &s.account)?;
    let pin = TeamPin { team: r.team.clone(), founder: s.account.clone(), founder_root: root.clone() };
    if !r.follows(None, &pin, &certs_for(&app, [s.account.as_str()].into_iter())?) {
        return Err(err(StatusCode::FORBIDDEN, "the roster must be signed by one of your devices, with you as owner"));
    }
    let t = Team {
        id: r.team.clone(),
        name: r.name.clone(),
        founder: s.account.clone(),
        founder_root: root,
        locked: false,
    };
    app.db.add_team(&t, 1, &serde_json::to_string(&r)?, illogical_e2e::now_ms())?;
    Ok(Json(json!({ "team": t.id })))
}

/// The teams I'm in: their latest rosters, members' certificates, and
/// (for owners) who asked to join.
pub async fn list(State(app): State<Arc<App>>, s: Session) -> R {
    let mut out = Vec::new();
    for body in app.db.teams_of(&s.account)? {
        let r = parse(&body)?;
        let Some(t) = app.db.team(&r.team)? else { continue };
        let mine = role_in(&r, &s.account);
        let requests = if mine == Some(TeamRole::Owner) { app.db.requests(&r.team)? } else { vec![] };
        let certs = certs_for(&app, r.members.iter().map(|m| m.account.as_str()))?;
        let names = names_of(&app, r.members.iter().map(|m| m.account.as_str()))?;
        out.push(json!({
            "team": t.id, "pin": pin(&t), "locked": t.locked, "roster": r, "role": mine,
            "requests": requests, "certs": certs, "names": names,
        }));
    }
    // Teams I asked to join, and whose yes I'm waiting for (#103).
    let mut asked = Vec::new();
    for team in app.db.asked(&s.account)? {
        let Ok(r) = latest(&app, &team) else { continue };
        let names = names_of(&app, r.members.iter().map(|m| m.account.as_str()))?;
        let owners: Vec<&str> = r
            .members
            .iter()
            .filter(|m| m.role == TeamRole::Owner)
            .map(|m| names.get(&m.account).unwrap_or(&m.name).as_str())
            .collect();
        asked.push(json!({ "team": team, "name": r.name, "owners": owners }));
    }
    // Teams that went while I wasn't looking (#206), to show once.
    let notices: Vec<Value> = app
        .db
        .notices(&s.account)?
        .into_iter()
        .map(|(id, title, body)| json!({ "id": id, "title": title, "body": body }))
        .collect();
    Ok(Json(json!({ "teams": out, "asked": asked, "notices": notices })))
}

/// `POST /api/me/notices/{id}/seen`: shown; don't show it again.
pub async fn notice_seen(State(app): State<Arc<App>>, s: Session, Path(id): Path<i64>) -> R {
    if !app.db.drop_notice(&s.account, id)? {
        return Err(err(StatusCode::NOT_FOUND, "no such notice"));
    }
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
pub struct NewRoster {
    roster: Roster,
}

pub async fn set_roster(
    State(app): State<Arc<App>>,
    s: Session,
    Path(team): Path<String>,
    Json(b): Json<NewRoster>,
) -> R {
    let t = app.db.team(&team)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
    let prev = latest(&app, &team)?;
    if b.roster.version != prev.version + 1 {
        // Someone else's change landed first: build on theirs and resend.
        return Err(err(StatusCode::CONFLICT, "the team changed while you were at it; try again"));
    }
    let accounts: Vec<&str> = prev.members.iter().chain(&b.roster.members).map(|m| m.account.as_str()).collect();
    if !b.roster.follows(Some(&prev), &pin(&t), &certs_for(&app, accounts.into_iter())?) {
        return Err(err(StatusCode::FORBIDDEN, "a new roster is the next version, signed by an owner's device"));
    }
    // Nobody is added who didn't ask: each new member asked to join (and
    // is added with the first device they asked with), or is redeeming an
    // invite as themselves (below).
    let requests = app.db.requests(&team)?;
    for m in b.roster.members.iter().filter(|m| prev.member(&m.account).is_none()) {
        let asked = requests.iter().any(|r| r.account == m.account && r.root == m.root);
        let redeeming = b.roster.redeem.is_some() && m.account == s.account;
        if !asked && !redeeming {
            return Err(err(StatusCode::FORBIDDEN, "only people who asked to join (or used an invite) can be added"));
        }
    }
    // A presigned invite: one control still holds (so it works once, and
    // not after a lock), redeemed by whoever is signed in here.
    let joined = match &b.roster.redeem {
        None => None,
        Some(r) => {
            let now = illogical_e2e::now_ms();
            let stored = app.db.presigned(&r.invite.key, now)?.filter(|(t, _, _)| *t == team);
            let Some((_, body, _)) = stored else {
                return Err(err(StatusCode::GONE, "that invite expired, was used, or the team was locked"));
            };
            if serde_json::from_str::<Invite>(&body).ok().as_ref() != Some(&r.invite) || t.locked {
                return Err(err(StatusCode::GONE, "that invite expired, was used, or the team was locked"));
            }
            let me = b.roster.members.last().filter(|m| m.account == s.account);
            let Some(me) = me else {
                return Err(err(StatusCode::FORBIDDEN, "an invite adds the account that's signed in"));
            };
            // With its own root, so owners see its real fingerprint.
            let root = app.db.account(&s.account)?.and_then(|a| a.root);
            if root.as_deref() != Some(me.root.as_str()) {
                return Err(err(StatusCode::FORBIDDEN, "an invite adds the account with its own first device"));
            }
            // Again now: a machine may have started checking the team
            // since the link was made.
            let old = behind(&app, &team, now)?;
            if !old.is_empty() {
                return Err(err(
                    StatusCode::FORBIDDEN,
                    &format!("{}; ask the team's owners for a new link", need_update(&old, "this invite works")),
                ));
            }
            Some(me.clone())
        }
    };
    app.db.add_roster(&team, b.roster.version, &serde_json::to_string(&b.roster)?).map_err(|e| {
        if e.is::<crate::db::Taken>() {
            err(StatusCode::CONFLICT, "the team changed while you were at it; try again")
        } else {
            e.into()
        }
    })?;
    if let (Some(me), Some(r)) = (&joined, &b.roster.redeem) {
        app.db.drop_presigned(&r.invite.key)?;
        // Owners hear who came in on their invite, after the fact.
        let owners: Vec<String> =
            prev.members.iter().filter(|m| m.role == TeamRole::Owner).map(|m| m.account.clone()).collect();
        crate::push::notify(
            &app,
            owners,
            &format!("control-team-{team}"),
            format!("{} joined {} with your invite", me.name, t.name),
            format!("As {}. Open illogical to check their fingerprint, or remove them.", me.role.as_str()),
        );
    }
    for m in &b.roster.members {
        app.db.drop_request(&team, &m.account)?;
    }
    nudge_team(&app, &team);
    // A team pays per seat (M22).
    crate::billing::sync_seats(&app, &team, b.roster.members.len()).await;
    Ok(Json(json!({ "version": b.roster.version })))
}

#[derive(Deserialize)]
pub struct NewInvite {
    role: TeamRole,
    #[serde(default = "week")]
    ttl_secs: u64,
    /// An invite the owner's device signed: accepting it adds the invitee
    /// with no owner's yes. Its one-time key's private half stays in the
    /// link and never comes here.
    #[serde(default)]
    presigned: Option<Invite>,
}

fn week() -> u64 {
    7 * 86_400
}

pub async fn invite(State(app): State<Arc<App>>, s: Session, Path(team): Path<String>, Json(b): Json<NewInvite>) -> R {
    let r = latest(&app, &team)?;
    if role_in(&r, &s.account) != Some(TeamRole::Owner) {
        return Err(err(StatusCode::FORBIDDEN, "owners invite"));
    }
    app.limits.check_account(crate::limit::TEAM_INVITES, &s.account)?;
    if let Some(inv) = b.presigned {
        let now = illogical_e2e::now_ms();
        // A daemon from before presigned invites refuses the roster one
        // writes, and then every later one: the team's members would stop
        // changing there, removals too. Only when every machine that checks
        // this team's rosters understands them.
        let old = behind(&app, &team, now)?;
        if !old.is_empty() {
            return Err(err(StatusCode::CONFLICT, &need_update(&old, "one-click invites work in this team")));
        }
        let me = r.member(&s.account).ok_or_else(|| err(StatusCode::FORBIDDEN, "owners invite"))?;
        let (c, rv) = certs_of(&app, &s.account)?;
        let signed =
            Trust { account: s.account.clone(), root: me.root.clone() }.evaluate(&c, &rv).get(&inv.by).is_some_and(
                |d| d.kind.approves() && illogical_e2e::cert::verify_hex(&d.sign, inv.body().as_bytes(), &inv.sig),
            );
        if inv.team != team
            || inv.role == TeamRole::Owner
            || inv.expires <= now
            || inv.expires > now + PRESIGNED_MAX_MS
            || !(inv.key.len() == 64 && inv.key.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')))
            || !signed
        {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "a presigned invite is for this team, not as an owner, at most a day, and signed by your device",
            ));
        }
        app.db.add_presigned(&inv.key, &team, &serde_json::to_string(&inv)?, inv.expires, &s.account).map_err(|e| {
            if e.is::<crate::db::Taken>() {
                err(StatusCode::CONFLICT, "that invite key is already used")
            } else {
                e.into()
            }
        })?;
        return Ok(Json(json!({ "key": inv.key, "expires": inv.expires })));
    }
    let code = token()[..20].to_owned();
    let expires = illogical_e2e::now_ms() + b.ttl_secs.min(30 * 86_400) * 1000;
    app.db.add_invite(&hash(&code), &team, b.role.as_str(), expires, &s.account)?;
    Ok(Json(
        json!({ "code": code, "link": format!("{}/#invite={team}.{code}", app.cfg.public_url), "expires": expires }),
    ))
}

/// A team's outstanding presigned invites, for its owners (#134): who
/// each is for (the role), when it expires and who made it. Used ones are
/// gone already (the redeem drops them).
pub async fn list_presigned(State(app): State<Arc<App>>, s: Session, Path(team): Path<String>) -> R {
    if role_in(&latest(&app, &team)?, &s.account) != Some(TeamRole::Owner) {
        return Err(err(StatusCode::FORBIDDEN, "owners see invites"));
    }
    let mut out = Vec::new();
    for (key, body, expires, by) in app.db.presigned_of(&team, illogical_e2e::now_ms())? {
        let inv: Invite = serde_json::from_str(&body)?;
        let by_name = app.db.account(&by)?.map(|a| a.name).unwrap_or_default();
        out.push(json!({ "key": key, "role": inv.role, "expires": expires, "by": by, "by_name": by_name }));
    }
    Ok(Json(json!({ "invites": out })))
}

/// Cancel a presigned invite before it's used (#134): control refuses it
/// at redeem from then on. Daemons never knew of it, so this is only as
/// good as control is honest, which is fine for a lost link.
pub async fn cancel_presigned(State(app): State<Arc<App>>, s: Session, Path((team, key)): Path<(String, String)>) -> R {
    if role_in(&latest(&app, &team)?, &s.account) != Some(TeamRole::Owner) {
        return Err(err(StatusCode::FORBIDDEN, "owners cancel invites"));
    }
    app.db
        .presigned(&key, illogical_e2e::now_ms())?
        .filter(|(t, _, _)| *t == team)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "that invite expired, was used, or never was"))?;
    app.db.drop_presigned(&key)?;
    Ok(Json(json!({})))
}

pub async fn show_invite(State(app): State<Arc<App>>, _s: Session, Path((team, code)): Path<(String, String)>) -> R {
    let (t, role) = app
        .db
        .invite(&hash(&code), illogical_e2e::now_ms())?
        .filter(|(t, _)| *t == team)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "that invite expired, or never was"))?;
    let name = app.db.team(&t)?.map(|t| t.name).unwrap_or_default();
    Ok(Json(json!({ "team": t, "name": name, "role": role })))
}

/// What a signed-out page may say about an invite (#103): the team's name
/// and who made it, nothing else. The code is the secret, as for accepting.
pub async fn preview_invite(
    State(app): State<Arc<App>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Path((team, code)): Path<(String, String)>,
) -> R {
    app.limits.check(crate::limit::INVITES, app.limits.client_ip(peer, &headers))?;
    let (t, by) = app
        .db
        .invite_by(&hash(&code), illogical_e2e::now_ms())?
        .filter(|(t, _)| *t == team)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "that invite expired, or never was"))?;
    let name = app.db.team(&t)?.map(|t| t.name).unwrap_or_default();
    let by = app.db.account(&by)?.map(|a| a.name).unwrap_or_default();
    Ok(Json(json!({ "name": name, "by": by })))
}

/// A presigned invite, for the page that redeems it: the invite, and the
/// team's latest roster with its members' certificates to build on.
pub async fn show_presigned(State(app): State<Arc<App>>, _s: Session, Path((team, key)): Path<(String, String)>) -> R {
    let (_, body, _) = app
        .db
        .presigned(&key, illogical_e2e::now_ms())?
        .filter(|(t, _, _)| *t == team)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "that invite expired, was used, or never was"))?;
    let t = app.db.team(&team)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
    let r = latest(&app, &team)?;
    let invite: Invite = serde_json::from_str(&body)?;
    let certs = certs_for(&app, r.members.iter().map(|m| m.account.as_str()))?;
    Ok(Json(json!({ "invite": invite, "name": t.name, "pin": pin(&t), "roster": r, "certs": certs })))
}

/// What a signed-out page may say about a presigned invite: the team's
/// name and who made it.
pub async fn preview_presigned(
    State(app): State<Arc<App>>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Path((team, key)): Path<(String, String)>,
) -> R {
    app.limits.check(crate::limit::INVITES, app.limits.client_ip(peer, &headers))?;
    let (_, _, by) = app
        .db
        .presigned(&key, illogical_e2e::now_ms())?
        .filter(|(t, _, _)| *t == team)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "that invite expired, was used, or never was"))?;
    let name = app.db.team(&team)?.map(|t| t.name).unwrap_or_default();
    let by = app.db.account(&by)?.map(|a| a.name).unwrap_or_default();
    Ok(Json(json!({ "name": name, "by": by })))
}

pub async fn accept_invite(State(app): State<Arc<App>>, s: Session, Path((team, code)): Path<(String, String)>) -> R {
    let (t, role) = app
        .db
        .invite(&hash(&code), illogical_e2e::now_ms())?
        .filter(|(t, _)| *t == team)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "that invite expired, or never was"))?;
    let me = app.db.account(&s.account)?.ok_or_else(|| err(StatusCode::UNAUTHORIZED, "no account"))?;
    let root = me.root.clone().ok_or_else(|| err(StatusCode::CONFLICT, "enroll a device first"))?;
    let name = member_name(&me);
    let new = !app.db.requests(&t)?.iter().any(|r| r.account == s.account);
    app.db.add_request(
        &t,
        &TeamRequest { account: s.account.clone(), root, name, role, created: illogical_e2e::now_ms() },
    )?;
    // The team's owners hear of it (#104), once.
    if new && let Some(team) = app.db.team(&t)? {
        let owners: Vec<String> =
            latest(&app, &t)?.members.into_iter().filter(|m| m.role == TeamRole::Owner).map(|m| m.account).collect();
        let who = if me.name.is_empty() { "Someone" } else { me.name.as_str() };
        crate::push::notify(
            &app,
            owners,
            &format!("control-team-{t}"),
            format!("{who} asks to join {}", team.name),
            "Open illogical to add them.".into(),
        );
    }
    Ok(Json(json!({ "team": t, "pending": true })))
}

pub async fn reject(State(app): State<Arc<App>>, s: Session, Path((team, account)): Path<(String, String)>) -> R {
    if role_in(&latest(&app, &team)?, &s.account) != Some(TeamRole::Owner) {
        return Err(err(StatusCode::FORBIDDEN, "owners decide"));
    }
    app.db.drop_request(&team, &account)?;
    Ok(Json(json!({})))
}

#[derive(Deserialize)]
pub struct Lock {
    locked: bool,
}

/// The kill switch: invites and requests go, and the team's daemons let
/// only owners in until it's unlocked.
pub async fn lock(State(app): State<Arc<App>>, s: Session, Path(team): Path<String>, Json(b): Json<Lock>) -> R {
    if role_in(&latest(&app, &team)?, &s.account) != Some(TeamRole::Owner) {
        return Err(err(StatusCode::FORBIDDEN, "owners lock"));
    }
    app.db.set_locked(&team, b.locked)?;
    if b.locked {
        app.db.drop_invites(&team)?;
    }
    nudge_team(&app, &team);
    Ok(Json(json!({ "locked": b.locked })))
}

// ---------------------------------------------------------------- daemons

/// What a daemon says it understands on its calls, comma-separated (older
/// ones send none).
#[derive(Deserialize)]
pub struct Features {
    #[serde(default)]
    pub features: String,
}

#[derive(Deserialize)]
pub struct Since {
    #[serde(default)]
    since: u64,
    /// What this daemon understands, comma-separated (older ones send none).
    #[serde(default)]
    features: String,
}

/// A team daemon's team: rosters after the version it has, every member's
/// certificates, and whether it's locked.
pub async fn daemon_team(State(app): State<Arc<App>>, d: DaemonAuth, Query(q): Query<Since>) -> R {
    app.db.set_daemon_features(&d.cert.device, &q.features)?;
    let Some(team) = app.db.daemon_team(&d.cert.device)? else { return Ok(Json(json!({ "team": null }))) };
    let t = app.db.team(&team)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
    // A machine downgraded after its team took a presigned invite would
    // stop at the first version one wrote and keep whoever was in then,
    // removed or not (#135). It's told why instead, as `daemon_teams`
    // leaves such a team out.
    if !takes_presigned(&q.features) && has_presigned(&app, &team)? {
        return Err(err(
            StatusCode::CONFLICT,
            "this machine's illogical is older than its team's invites: update illogical to keep up with the team",
        ));
    }
    let rosters: Vec<Roster> =
        app.db.rosters(&team, q.since)?.iter().map(|b| parse(b)).collect::<anyhow::Result<_>>()?;
    // Certificates for everyone in any version it will check, the one it
    // has included (that version's owners sign the next).
    let mut accounts: Vec<String> = Vec::new();
    for b in app.db.rosters(&team, q.since.saturating_sub(1))? {
        accounts.extend(parse(&b)?.members.into_iter().map(|m| m.account));
    }
    accounts.sort();
    accounts.dedup();
    let certs = certs_for(&app, accounts.iter().map(String::as_str))?;
    let names = names_of(&app, accounts.iter().map(String::as_str))?;
    Ok(Json(json!({ "team": pin(&t), "locked": t.locked, "rosters": rosters, "certs": certs, "names": names })))
}

#[derive(Deserialize)]
pub struct TeamIds {
    ids: String,
    #[serde(default)]
    features: String,
}

/// Teams a member's own machine shared sessions with (M30): for each team
/// its owner is in, every roster from the first (the machine checks the
/// chain from the founder it pinned in the grant), every member's
/// certificates, and whether it's locked. Teams its owner isn't in are
/// left out.
pub async fn daemon_teams(State(app): State<Arc<App>>, d: DaemonAuth, Query(q): Query<TeamIds>) -> R {
    let owner = app.db.daemon_account(&d.cert.device)?.unwrap_or_default();
    app.db.set_daemon_features(&d.cert.device, &q.features)?;
    let now = illogical_e2e::now_ms();
    let mut out = serde_json::Map::new();
    for team in q.ids.split(',').filter(|t| !t.is_empty()).take(50) {
        let Some(t) = app.db.team(team)? else { continue };
        let Some(latest) = app.db.latest_roster(team)? else { continue };
        if role_in(&parse(&latest)?, &owner).is_none() {
            continue;
        }
        // A daemon from before presigned invites would stop at the first
        // version one wrote and keep whoever was in then, removed or not:
        // it gets nothing for the team instead, until it's updated.
        if !takes_presigned(&q.features) && has_presigned(&app, team)? {
            continue;
        }
        app.db.watch_team(&d.cert.device, team, now)?;
        let rosters: Vec<Roster> = app.db.rosters(team, 0)?.iter().map(|b| parse(b)).collect::<anyhow::Result<_>>()?;
        let mut accounts: Vec<String> =
            rosters.iter().flat_map(|r| r.members.iter().map(|m| m.account.clone())).collect();
        accounts.sort();
        accounts.dedup();
        let certs = certs_for(&app, accounts.iter().map(String::as_str))?;
        let names = names_of(&app, accounts.iter().map(String::as_str))?;
        out.insert(
            team.to_owned(),
            json!({
                "team": pin(&t), "name": t.name, "locked": t.locked, "rosters": rosters, "certs": certs, "names": names,
            }),
        );
    }
    Ok(Json(Value::Object(out)))
}

#[derive(Deserialize)]
pub struct Peers {
    accounts: String,
}

/// Certificates of accounts a daemon was shared with (by its owner,
/// through their channel): it checks them against the roots it pinned.
/// Only accounts control routes to it (see the top); anyone else it names
/// becomes an offer they answer, and gets certificates here once they
/// accept.
pub async fn daemon_peers(State(app): State<Arc<App>>, d: DaemonAuth, Query(q): Query<Peers>) -> R {
    let owner = app.db.daemon_account(&d.cert.device)?.unwrap_or_else(|| d.cert.account.clone());
    let rel = Relations::of(&app, &d.cert.device, &owner)?;
    let mut accounts: Vec<&str> = q.accounts.split(',').filter(|a| !a.is_empty()).take(200).collect();
    accounts.sort_unstable();
    accounts.dedup();
    let mut out = serde_json::Map::new();
    let mut asking = Vec::new();
    for a in accounts {
        if !rel.routes(&app, a)? {
            // Only real accounts that haven't turned it down are asked.
            if app.db.share_answer(a, &d.cert.device)?.is_none() && app.db.account(a)?.is_some_and(|x| x.root.is_some())
            {
                asking.push(a.to_owned());
            }
            continue;
        }
        let name = app.db.account(a)?.map(|x| x.name).unwrap_or_default();
        let (certs, revocations) = certs_of(&app, a)?;
        out.insert(a.to_owned(), json!({ "certs": certs, "revocations": revocations, "name": name }));
    }
    asking.truncate(50);
    let new = app.db.offer_shares(&d.cert.device, &asking, illogical_e2e::now_ms())?;
    // Each new offer shows someone a prompt: a brake on how many.
    let mut over = false;
    for a in &new {
        if over || app.limits.check_daemon(crate::limit::OFFERS, &d.cert.device).is_err() {
            over = true;
            asking.retain(|x| x != a);
        }
    }
    if over {
        app.db.offer_shares(&d.cert.device, &asking, illogical_e2e::now_ms())?;
    }
    // Each new offer pushes the person offered, once (#232; once a day, if
    // the daemon drops and makes it again): who and which machine, nothing
    // of what's shared (they haven't said yes yet).
    let mut pushing = Vec::new();
    for a in new.into_iter().filter(|a| asking.contains(a)) {
        if app.db.push_offer(&d.cert.device, &a, illogical_e2e::now_ms())? {
            pushing.push(a);
        }
    }
    let new = pushing;
    if !new.is_empty()
        && let Some((owner, row)) = app.db.daemon_row(&d.cert.device)?
    {
        let login = app.db.account(&owner)?.map(|a| a.login).filter(|l| !l.is_empty());
        crate::push::notify(
            &app,
            new,
            &format!("control-share-{}", d.cert.device),
            format!("{} wants to share {} with you", login.as_deref().unwrap_or("Someone"), row.name),
            "Open illogical to accept or turn it down.".into(),
        );
    }
    Ok(Json(Value::Object(out)))
}

/// What control knows of who has a say in a daemon: its owner, its team's
/// members, and the owner's teammates. Share answers are looked up as
/// needed.
pub struct Relations {
    daemon: String,
    owner: String,
    near: HashSet<String>,
}

impl Relations {
    pub fn of(app: &App, daemon: &str, owner: &str) -> anyhow::Result<Self> {
        let mut near = HashSet::from([owner.to_owned()]);
        if let Some(team) = app.db.daemon_team(daemon)?
            && let Some(body) = app.db.latest_roster(&team)?
        {
            near.extend(parse(&body)?.members.into_iter().map(|m| m.account));
        }
        for body in app.db.teams_of(owner)? {
            near.extend(parse(&body)?.members.into_iter().map(|m| m.account));
        }
        Ok(Self { daemon: daemon.to_owned(), owner: owner.to_owned(), near })
    }

    /// Whether control routes `account` to this daemon (if the daemon lets
    /// it in): see the top of this file. An account that turned the
    /// daemon's share down isn't, even a teammate.
    pub fn routes(&self, app: &App, account: &str) -> anyhow::Result<bool> {
        if account == self.owner {
            return Ok(true);
        }
        match app.db.share_answer(account, &self.daemon)? {
            Some(yes) => Ok(yes),
            None => Ok(self.near.contains(account)),
        }
    }
}

#[derive(Deserialize)]
pub struct ShareAnswer {
    accept: bool,
}

/// `POST /api/shares/{daemon} {accept}`: someone's answer to a session a
/// daemon offered to share with them.
pub async fn answer_share(
    State(app): State<Arc<App>>,
    s: Session,
    Path(daemon): Path<String>,
    Json(b): Json<ShareAnswer>,
) -> R {
    if b.accept && !app.db.offered(&daemon, &s.account)? {
        return Err(err(StatusCode::NOT_FOUND, "that machine isn't sharing anything with you (any more?)"));
    }
    app.db.answer_share(&s.account, &daemon, b.accept, illogical_e2e::now_ms())?;
    // It fetches their certificates now, and lets them in.
    app.relay.nudge(&[daemon]);
    Ok(Json(json!({ "accepted": b.accept })))
}

#[derive(Deserialize)]
pub struct Access {
    accounts: Vec<String>,
    /// Until when (ms) read-only links may reach it through the relay.
    links_until: Option<u64>,
}

/// Which accounts a daemon lets in, for the directory and the relay. The
/// daemon decides for itself; control routes only those it would anyway
/// (see the top): this list narrows that, never widens it.
pub async fn daemon_access(State(app): State<Arc<App>>, d: DaemonAuth, Json(b): Json<Access>) -> R {
    if b.accounts.len() > 500 {
        return Err(err(StatusCode::BAD_REQUEST, "too many"));
    }
    app.db.set_access(&d.cert.device, &b.accounts, b.links_until)?;
    Ok(Json(json!({})))
}

/// May `account` reach daemon `id` through the relay?
pub fn may_reach(app: &App, account: &str, id: &str) -> anyhow::Result<bool> {
    let Some((owner, _)) = app.db.daemon_row(id)? else { return Ok(false) };
    if owner == account {
        return Ok(true);
    }
    if let Some(team) = app.db.daemon_team(id)?
        && let Some(t) = app.db.team(&team)?
        && let Some(body) = app.db.latest_roster(&team)?
    {
        let r = parse(&body)?;
        if let Some(role) = role_in(&r, account) {
            return Ok(!t.locked || role == TeamRole::Owner);
        }
    }
    Ok(app.db.daemon_lets_in(id, account)? && Relations::of(app, id, &owner)?.routes(app, account)?)
}

/// Daemons beyond an account's own that it may reach: its teams' and
/// those shared with it.
pub fn reachable(app: &App, account: &str) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    for body in app.db.teams_of(account)? {
        ids.extend(app.db.team_daemons(&parse(&body)?.team)?);
    }
    for id in app.db.shared_daemons(account)? {
        let Some(owner) = app.db.daemon_account(&id)? else { continue };
        if Relations::of(app, &id, &owner)?.routes(app, account)? {
            ids.push(id);
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub fn chain_of(app: &App, account: &str) -> anyhow::Result<Value> {
    let root = app.db.account(account)?.and_then(|a| a.root);
    let (certs, revocations) = certs_of(app, account)?;
    Ok(
        json!({ "trust": root.map(|r| json!({ "account": account, "root": r })), "certs": certs, "revocations": revocations }),
    )
}
