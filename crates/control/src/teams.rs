//! Teams and people (M19): finding someone to share with, teams with
//! signed rosters, invites, and which accounts a daemon lets in.
//!
//! Control stores rosters and checks them as daemons do (it never keeps
//! one a daemon would throw away), but it can't make one: every version is
//! signed by a team owner's device.

use std::{collections::HashMap, sync::Arc};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use illogical_e2e::{
    Cert, Revocation,
    team::{AccountCerts, Roster, TeamPin, TeamRole},
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

/// An account's certificates and revocations.
fn certs_of(app: &App, account: &str) -> anyhow::Result<(Vec<Cert>, Vec<Revocation>)> {
    Ok((app.db.devices(account)?.0, app.db.revocations(account)?))
}

fn certs_for<'a>(app: &App, accounts: impl Iterator<Item = &'a str>) -> anyhow::Result<AccountCerts> {
    let mut out = HashMap::new();
    for a in accounts {
        out.insert(a.to_owned(), certs_of(app, a)?);
    }
    Ok(out)
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
pub async fn person(State(app): State<Arc<App>>, _s: Session, Query(q): Query<Lookup>) -> R {
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
        out.push(json!({
            "team": t.id, "pin": pin(&t), "locked": t.locked, "roster": r, "role": mine,
            "requests": requests, "certs": certs,
        }));
    }
    // Teams I asked to join, and whose yes I'm waiting for (#103).
    let mut asked = Vec::new();
    for team in app.db.asked(&s.account)? {
        let Ok(r) = latest(&app, &team) else { continue };
        let owners: Vec<&str> =
            r.members.iter().filter(|m| m.role == TeamRole::Owner).map(|m| m.name.as_str()).collect();
        asked.push(json!({ "team": team, "name": r.name, "owners": owners }));
    }
    Ok(Json(json!({ "teams": out, "asked": asked })))
}

#[derive(Deserialize)]
pub struct NewRoster {
    roster: Roster,
}

pub async fn set_roster(
    State(app): State<Arc<App>>,
    _s: Session,
    Path(team): Path<String>,
    Json(b): Json<NewRoster>,
) -> R {
    let t = app.db.team(&team)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
    let prev = latest(&app, &team)?;
    let accounts: Vec<&str> = prev.members.iter().chain(&b.roster.members).map(|m| m.account.as_str()).collect();
    if b.roster.version != prev.version + 1
        || !b.roster.follows(Some(&prev), &pin(&t), &certs_for(&app, accounts.into_iter())?)
    {
        return Err(err(StatusCode::FORBIDDEN, "a new roster is the next version, signed by an owner's device"));
    }
    app.db.add_roster(&team, b.roster.version, &serde_json::to_string(&b.roster)?)?;
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
}

fn week() -> u64 {
    7 * 86_400
}

pub async fn invite(State(app): State<Arc<App>>, s: Session, Path(team): Path<String>, Json(b): Json<NewInvite>) -> R {
    let r = latest(&app, &team)?;
    if role_in(&r, &s.account) != Some(TeamRole::Owner) {
        return Err(err(StatusCode::FORBIDDEN, "owners invite"));
    }
    let code = token()[..20].to_owned();
    let expires = illogical_e2e::now_ms() + b.ttl_secs.min(30 * 86_400) * 1000;
    app.db.add_invite(&hash(&code), &team, b.role.as_str(), expires, &s.account)?;
    Ok(Json(
        json!({ "code": code, "link": format!("{}/#invite={team}.{code}", app.cfg.public_url), "expires": expires }),
    ))
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
    let by = app.db.account(&by)?.map(|a| a.login).unwrap_or_default();
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

#[derive(Deserialize)]
pub struct Since {
    #[serde(default)]
    since: u64,
}

/// A team daemon's team: rosters after the version it has, every member's
/// certificates, and whether it's locked.
pub async fn daemon_team(State(app): State<Arc<App>>, d: DaemonAuth, Query(q): Query<Since>) -> R {
    let Some(team) = app.db.daemon_team(&d.cert.device)? else { return Ok(Json(json!({ "team": null }))) };
    let t = app.db.team(&team)?.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such team"))?;
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
    Ok(Json(json!({ "team": pin(&t), "locked": t.locked, "rosters": rosters, "certs": certs })))
}

#[derive(Deserialize)]
pub struct TeamIds {
    ids: String,
}

/// Teams a member's own machine shared sessions with (M30): for each team
/// its owner is in, every roster from the first (the machine checks the
/// chain from the founder it pinned in the grant), every member's
/// certificates, and whether it's locked. Teams its owner isn't in are
/// left out.
pub async fn daemon_teams(State(app): State<Arc<App>>, d: DaemonAuth, Query(q): Query<TeamIds>) -> R {
    let owner = app.db.daemon_account(&d.cert.device)?.unwrap_or_default();
    let mut out = serde_json::Map::new();
    for team in q.ids.split(',').filter(|t| !t.is_empty()).take(50) {
        let Some(t) = app.db.team(team)? else { continue };
        let Some(latest) = app.db.latest_roster(team)? else { continue };
        if role_in(&parse(&latest)?, &owner).is_none() {
            continue;
        }
        let rosters: Vec<Roster> = app.db.rosters(team, 0)?.iter().map(|b| parse(b)).collect::<anyhow::Result<_>>()?;
        let mut accounts: Vec<String> =
            rosters.iter().flat_map(|r| r.members.iter().map(|m| m.account.clone())).collect();
        accounts.sort();
        accounts.dedup();
        let certs = certs_for(&app, accounts.iter().map(String::as_str))?;
        out.insert(
            team.to_owned(),
            json!({ "team": pin(&t), "name": t.name, "locked": t.locked, "rosters": rosters, "certs": certs }),
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
pub async fn daemon_peers(State(app): State<Arc<App>>, _d: DaemonAuth, Query(q): Query<Peers>) -> R {
    let accounts: Vec<&str> = q.accounts.split(',').filter(|a| !a.is_empty()).take(200).collect();
    let mut out = serde_json::Map::new();
    for a in accounts {
        let name = app.db.account(a)?.map(|x| x.name).unwrap_or_default();
        let (certs, revocations) = certs_of(&app, a)?;
        out.insert(a.to_owned(), json!({ "certs": certs, "revocations": revocations, "name": name }));
    }
    Ok(Json(Value::Object(out)))
}

#[derive(Deserialize)]
pub struct Access {
    accounts: Vec<String>,
    /// Until when (ms) read-only links may reach it through the relay.
    links_until: Option<u64>,
}

/// Which accounts a daemon lets in, for the directory and the relay. The
/// daemon decides for itself; this only routes.
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
    app.db.daemon_lets_in(id, account)
}

/// Daemons beyond an account's own that it may reach: its teams' and
/// those shared with it.
pub fn reachable(app: &App, account: &str) -> anyhow::Result<Vec<String>> {
    let mut ids = Vec::new();
    for body in app.db.teams_of(account)? {
        ids.extend(app.db.team_daemons(&parse(&body)?.team)?);
    }
    ids.extend(app.db.shared_daemons(account)?);
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
