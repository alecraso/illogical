//! Your account (#173): its sign-in sessions and passkeys, and deleting it.
//!
//! Deleting an account removes everything control holds that is only
//! its: sign-in identities (the GitHub link), sessions, passkeys, devices
//! and machines (which control then refuses), revocations, pending joins,
//! push subscriptions, hosted VMs (torn down), billing rows, usage counts,
//! team requests and the invites it made. Its live relay sockets hang up.
//!
//! Teams, whose rosters are signed by owners' devices and checked from
//! the founder's first device on:
//!
//! - **A team it founded goes with it** (and so does one where nobody
//!   else is left): the signed history starts at the founder's key, so it
//!   can't outlive the account. Its machines stay their owners', now
//!   personal, and their relay sockets are hung up at once so nothing
//!   relayed for the team outlasts it (#206); its members hear it was
//!   deleted, by push and by a notice in the app.
//! - **A team someone else founded** stays. A plain member can delete
//!   right away: the roster keeps listing their name, with no devices
//!   behind it, until an owner removes it, and the owners hear of it. An
//!   owner, or anyone whose devices signed a version of the roster, leaves
//!   the team first (another owner removes them, or they remove themselves
//!   once someone else is an owner).
//! - **Signatures in a surviving team's history** still need checking:
//!   the account's signing devices' certificates (public keys and device
//!   names, no more) are kept for that alone, until the team goes.
//!
//! Billing: an account that pays for a plan (its own, or a team's it
//! founded) cancels it first.

use std::{collections::HashSet, sync::Arc};

use arugula_e2e::{
    now_ms,
    team::{Roster, TeamRole},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{ApiError, App, auth::Session, err};

type R = Result<Json<Value>, ApiError>;

/// How a session is shown and named: the start of its token's hash.
const SESSION_ID_LEN: usize = 16;

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == crate::auth::SESSION_COOKIE)
        .map(|(_, v)| v)
}

/// Set-Cookie that signs this browser out.
fn clear_cookie(app: &App) -> HeaderValue {
    let secure = if app.cfg.public_url.starts_with("https://") { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}",
        crate::auth::SESSION_COOKIE
    ))
    .unwrap()
}

// ---------------------------------------------------------------- sessions

/// `GET /api/me/sessions`: where this account is signed in.
pub async fn sessions(State(app): State<Arc<App>>, s: Session, headers: HeaderMap) -> R {
    let mine = session_cookie(&headers).map(crate::auth::hash).unwrap_or_default();
    let list: Vec<Value> = app
        .db
        .sessions(&s.account, now_ms())?
        .into_iter()
        .map(|r| {
            json!({
                "id": &r.hash[..SESSION_ID_LEN], "created": r.created, "expires": r.expires,
                "agent": r.agent, "current": r.hash == mine,
            })
        })
        .collect();
    Ok(Json(json!({ "sessions": list })))
}

/// `POST /api/me/sessions/{id}/end`: sign one out.
pub async fn end_session(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>) -> R {
    if id.len() != SESSION_ID_LEN || app.db.drop_session_of(&s.account, &id)? == 0 {
        return Err(err(StatusCode::NOT_FOUND, "no such session"));
    }
    info!(account = s.account, "a session signed out");
    Ok(Json(json!({})))
}

/// `POST /api/me/sessions/end-all`: sign out everywhere, here too.
pub async fn end_all(State(app): State<Arc<App>>, s: Session) -> Response {
    match app.db.drop_sessions(&s.account) {
        Ok(n) => {
            info!(account = s.account, sessions = n, "signed out everywhere");
            let mut res = Json(json!({ "ended": n })).into_response();
            res.headers_mut().append(header::SET_COOKIE, clear_cookie(&app));
            res
        }
        Err(e) => ApiError::from(e).into_response(),
    }
}

/// A session's browser or app, as it said when it signed in: kept short.
pub fn agent(headers: &HeaderMap) -> String {
    headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").chars().take(200).collect()
}

/// After any response that starts a session, remember what asked for it
/// (a middleware: every sign-in path sets the cookie).
pub async fn note_agent(
    State(app): State<Arc<App>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let ua = agent(req.headers());
    let res = next.run(req).await;
    for v in res.headers().get_all(header::SET_COOKIE) {
        let Some(token) = v
            .to_str()
            .ok()
            .and_then(|c| c.split(';').next())
            .and_then(|kv| kv.strip_prefix(&format!("{}=", crate::auth::SESSION_COOKIE)))
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        if let Err(e) = app.db.set_session_agent(&crate::auth::hash(token), &ua) {
            warn!(error = %e, "noting a session's browser");
        }
    }
    res
}

// ---------------------------------------------------------------- passkeys

/// `GET /api/me/passkeys`: the account's passkeys, and its GitHub link.
pub async fn passkeys(State(app): State<Arc<App>>, s: Session) -> R {
    Ok(Json(json!({
        "passkeys": app.db.passkeys(&s.account)?, "github": app.db.github_identity(&s.account)?.map(|(_, login)| login), "ways": app.db.sign_ins(&s.account)?,
    })))
}

/// `POST /api/me/passkeys/{id}/remove`: never the last way to sign in.
pub async fn remove_passkey(State(app): State<Arc<App>>, s: Session, Path(id): Path<String>) -> R {
    match app.db.drop_passkey(&s.account, &id)? {
        Ok(()) => {
            info!(account = s.account, "passkey removed");
            Ok(Json(json!({ "passkeys": app.db.passkey_count(&s.account)? })))
        }
        Err(why @ "no such passkey") => Err(err(StatusCode::NOT_FOUND, why)),
        Err(why) => Err(err(StatusCode::CONFLICT, why)),
    }
}

// ---------------------------------------------------------------- deleting

/// What deleting an account would do.
#[derive(Debug, Default)]
pub struct Plan {
    /// What to type to confirm: the GitHub login, else the name, else the
    /// account id.
    pub confirm: String,
    /// (team, name, how many others are in it): they go with the account.
    pub disband: Vec<(String, String, usize)>,
    /// Teams it must leave first, and why: refuse until then.
    pub blockers: Vec<String>,
    /// Plans it pays for: refuse until cancelled.
    pub pays: Vec<String>,
    /// Teams whose history its devices signed: keep its certificates.
    pub retain_for: Vec<String>,
    /// Teams it stays listed in, and their owners, to tell.
    pub tell: Vec<(String, String, Vec<String>)>,
}

pub fn plan(app: &App, account: &str) -> anyhow::Result<Plan> {
    let a = app.db.account(account)?.ok_or_else(|| anyhow::anyhow!("no such account"))?;
    let confirm = match app.db.github_identity(account)?.map(|(_, login)| login) {
        Some(l) if !l.is_empty() => l,
        _ if !a.name.is_empty() => a.name.clone(),
        _ => a.id.clone(),
    };
    let (approved, pending) = app.db.devices(account)?;
    let mine: HashSet<String> = approved.iter().chain(&pending).map(|c| c.device.clone()).collect();
    let mut p = Plan { confirm, ..Default::default() };
    if app.db.billing(&format!("account:{account}"))?.is_some_and(|b| b.active()) {
        p.pays.push("your own plan".into());
    }
    for team in app.db.team_ids()? {
        let Some(t) = app.db.team(&team)? else { continue };
        let rosters: Vec<Roster> =
            app.db.rosters(&team, 0)?.iter().filter_map(|b| serde_json::from_str(b).ok()).collect();
        let Some(latest) = rosters.last() else { continue };
        let role = latest.member(account).map(|m| m.role);
        let others: Vec<_> = latest.members.iter().filter(|m| m.account != account).collect();
        if t.founder == account || (role.is_some() && others.is_empty()) {
            if app.db.billing(&format!("team:{team}"))?.is_some_and(|b| b.active()) {
                p.pays.push(format!("the team plan of {}", t.name));
            }
            p.disband.push((team, t.name, others.len()));
            continue;
        }
        let signed = rosters
            .iter()
            .any(|r| mine.contains(&r.by) || r.redeem.as_ref().is_some_and(|x| mine.contains(&x.invite.by)));
        let owners: Vec<String> =
            others.iter().filter(|m| m.role == TeamRole::Owner).map(|m| m.account.clone()).collect();
        match role {
            None => {}
            Some(TeamRole::Owner) if owners.is_empty() => p
                .blockers
                .push(format!("You're the only owner of {}: make someone else an owner, then leave it", t.name)),
            Some(TeamRole::Owner) => p.blockers.push(format!("You're an owner of {}: leave it first", t.name)),
            Some(_) if signed => p.blockers.push(format!(
                "Your devices signed {}'s roster when you were an owner: ask an owner to remove you first",
                t.name
            )),
            Some(_) => p.tell.push((team.clone(), t.name.clone(), owners)),
        }
        if signed {
            p.retain_for.push(team);
        }
    }
    Ok(p)
}

/// `GET /api/me/delete`: what deleting this account would do.
pub async fn preview(State(app): State<Arc<App>>, s: Session) -> R {
    let p = plan(&app, &s.account)?;
    let machines = app.db.daemons(&s.account)?.len();
    let vms = app.db.sandboxes(&s.account)?.iter().filter(|x| x.deleted.is_none()).count();
    Ok(Json(json!({
        "confirm": p.confirm,
        "disband": p.disband.iter().map(|(id, name, others)| json!({ "team": id, "name": name, "others": others })).collect::<Vec<_>>(),
        "blockers": p.blockers, "pays": p.pays, "machines": machines, "vms": vms,
        "stays_in": p.tell.iter().map(|(_, name, _)| name).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
pub struct Confirm {
    confirm: String,
}

/// `POST /api/me/delete {confirm}`: delete this account, for good.
pub async fn delete(State(app): State<Arc<App>>, s: Session, Json(b): Json<Confirm>) -> Response {
    match delete_account(&app, &s.account, &b.confirm).await {
        Ok(()) => {
            let mut res = Json(json!({ "deleted": true })).into_response();
            res.headers_mut().append(header::SET_COOKIE, clear_cookie(&app));
            res
        }
        Err(e) => e.into_response(),
    }
}

pub async fn delete_account(app: &Arc<App>, account: &str, typed: &str) -> Result<(), ApiError> {
    let p = plan(app, account)?;
    if !typed.trim().eq_ignore_ascii_case(p.confirm.trim()) {
        return Err(err(StatusCode::BAD_REQUEST, &format!("type {} to confirm", p.confirm)));
    }
    if let Some(why) = p.blockers.first() {
        return Err(err(StatusCode::CONFLICT, why));
    }
    if !p.pays.is_empty() {
        return Err(err(StatusCode::CONFLICT, &format!("cancel {} first", p.pays.join(" and "))));
    }
    // Hosted VMs go the way they always do.
    for x in app.db.sandboxes(account)? {
        if x.deleted.is_none()
            && let Err(e) = crate::sandboxes::remove(app, &x.id).await
        {
            warn!(sandbox = x.id, error = %e, "can't delete a deleted account's hosted VM");
        }
    }
    // Who to tell, and which machines to nudge, before the rows go.
    let mut nudge: Vec<String> = Vec::new();
    let mut hang_up: Vec<String> = Vec::new();
    let mut told: Vec<(Vec<String>, String, String, String)> = Vec::new();
    let me = app.db.account(account)?.map(|a| a.name).filter(|n| !n.is_empty()).unwrap_or_else(|| "A member".into());
    for (team, name, _) in &p.disband {
        hang_up.extend(app.db.team_daemons(team)?);
        if let Some(body) = app.db.latest_roster(team)?
            && let Ok(r) = serde_json::from_str::<Roster>(&body)
        {
            let others: Vec<String> = r.members.into_iter().map(|m| m.account).filter(|a| a != account).collect();
            for a in &others {
                nudge.extend(app.db.daemons(a)?.into_iter().map(|d| d.id));
            }
            told.push((
                others,
                format!("control-team-{team}"),
                format!("{name} was deleted"),
                "Its founder deleted their account.".into(),
            ));
        }
    }
    for (team, name, owners) in &p.tell {
        told.push((
            owners.clone(),
            format!("control-team-{team}"),
            format!("{me} left {name}"),
            "They deleted their account. Remove them from the roster when you next change it.".into(),
        ));
    }
    let disband: Vec<String> = p.disband.iter().map(|(t, _, _)| t.clone()).collect();
    let daemons = app.db.delete_account(&crate::db::Erase { account, disband: &disband, retain_for: &p.retain_for })?;
    for d in &daemons {
        app.relay.drop_daemon(d);
    }
    app.relay.account_gone(account);
    // The team's machines that stay (someone else's, now personal) were
    // relaying for its members: hang those connections up now (#206).
    hang_up.retain(|d| !daemons.contains(d));
    app.relay.redial(&hang_up);
    nudge.retain(|d| !daemons.contains(d) && !hang_up.contains(d));
    app.relay.nudge(&nudge);
    // Told twice: a push, and a notice in the app for whoever has no push.
    let now = now_ms();
    for (to, tag, title, body) in told {
        for a in &to {
            if let Err(e) = app.db.add_notice(a, &title, &body, now) {
                warn!(error = %e, "keeping a notice");
            }
        }
        crate::push::notify(app, to, &tag, title, body);
    }
    info!(account, machines = daemons.len(), teams = disband.len(), "account deleted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use arugula_e2e::{
        Cert, DeviceKeys, Kind,
        team::{Member, TeamPin},
    };

    use super::*;
    use crate::db::{Db, Team};

    fn app() -> Arc<App> {
        Arc::new(App::for_tests("http://control.test"))
    }

    /// An account (signed in with GitHub as `login`) with its first device.
    fn person(app: &App, id: &str, login: &str) -> (DeviceKeys, Cert) {
        app.db.account_for("github", &format!("gh-{id}"), login, id, 1).unwrap();
        let keys = DeviceKeys::generate();
        let mut c = Cert::new(&keys, id, Kind::Browser, "laptop");
        c.sign_with(&keys);
        app.db.put_device(&c, true, 1).unwrap();
        (keys, c)
    }

    /// A machine of `account`'s, approved by `by`.
    fn machine(app: &App, account: &str, by: &DeviceKeys, name: &str) -> String {
        let keys = DeviceKeys::generate();
        let mut c = Cert::new(&keys, account, Kind::Daemon, name);
        c.sign_with(by);
        app.db.put_device(&c, true, 1).unwrap();
        app.db.put_daemon(account, &c.device, name, &[]).unwrap();
        c.device
    }

    fn member(id: &str, root: &Cert, role: TeamRole) -> Member {
        Member { account: id.into(), root: root.device.clone(), role, name: id.into() }
    }

    fn roster(team: &str, version: u64, members: Vec<Member>, by: &DeviceKeys) -> Roster {
        let mut r = Roster {
            v: 1,
            team: team.into(),
            name: format!("team {team}"),
            version,
            at: version,
            members,
            spent: vec![],
            redeem: None,
            by: String::new(),
            sig: String::new(),
        };
        r.sign_with(by);
        r
    }

    fn found(app: &App, r: &Roster, founder: &str, root: &Cert) {
        let t = Team {
            id: r.team.clone(),
            name: r.name.clone(),
            founder: founder.into(),
            founder_root: root.device.clone(),
            locked: false,
        };
        app.db.add_team(&t, 1, &serde_json::to_string(r).unwrap(), 1).unwrap();
    }

    fn next(app: &App, r: &Roster) {
        app.db.add_roster(&r.team, r.version, &serde_json::to_string(r).unwrap()).unwrap();
    }

    /// The whole chain checks out with what control would hand a daemon.
    fn checks(app: &App, team: &str) -> bool {
        let t = app.db.team(team).unwrap().unwrap();
        let pin = TeamPin { team: t.id, founder: t.founder, founder_root: t.founder_root };
        let rosters: Vec<Roster> =
            app.db.rosters(team, 0).unwrap().iter().map(|b| serde_json::from_str(b).unwrap()).collect();
        let mut accounts: Vec<String> =
            rosters.iter().flat_map(|r| r.members.iter().map(|m| m.account.clone())).collect();
        accounts.sort();
        accounts.dedup();
        let certs = crate::teams::certs_for_test(app, &accounts);
        let mut prev: Option<&Roster> = None;
        for r in &rosters {
            if !r.follows(prev, &pin, &certs) {
                return false;
            }
            prev = Some(r);
        }
        true
    }

    #[tokio::test]
    async fn deleting_an_account_leaves_nothing_of_it() {
        let app = app();
        let (ak, ac) = person(&app, "acct-alice", "alice");
        let (bk, bc) = person(&app, "acct-bob", "bob");
        let box1 = machine(&app, "acct-alice", &ak, "box");
        let bobs = machine(&app, "acct-bob", &bk, "bobs-box");
        // Everything else an account leaves around.
        app.db.add_session(&crate::auth::hash("t"), "acct-alice", 1, i64::MAX as u64).unwrap();
        app.db.add_passkey("pk-alice", "acct-alice", -7, b"k", 1).unwrap();
        app.db.put_push_sub("https://push.test/acct-alice", "acct-alice", "{}").unwrap();
        app.db.add_relay_bytes("acct-alice", "2026-10-04", 5).unwrap();
        app.db.set_access(&box1, &["acct-bob".into()], Some(i64::MAX as u64)).unwrap();
        app.db.set_access(&bobs, &["acct-alice".into()], None).unwrap();
        // Shares offered both ways, and answered.
        app.db.offer_shares(&bobs, &["acct-alice".into()], 1).unwrap();
        app.db.answer_share("acct-alice", &bobs, true, 1).unwrap();
        app.db.offer_shares(&box1, &["acct-bob".into()], 1).unwrap();
        app.db.answer_share("acct-bob", &box1, false, 1).unwrap();
        // Alice founded a team Bob is in, and Bob's machine is the team's.
        let v1 = roster(
            "t1",
            1,
            vec![member("acct-alice", &ac, TeamRole::Owner), member("acct-bob", &bc, TeamRole::Editor)],
            &ak,
        );
        found(&app, &v1, "acct-alice", &ac);
        app.db.set_daemon_team(&bobs, "t1").unwrap();
        app.db.add_invite("inv", "t1", "viewer", i64::MAX as u64, "acct-alice").unwrap();
        // Bob's own team, which Alice asked to join.
        let w1 = roster("t2", 1, vec![member("acct-bob", &bc, TeamRole::Owner)], &bk);
        found(&app, &w1, "acct-bob", &bc);
        app.db
            .add_request(
                "t2",
                &crate::db::TeamRequest {
                    account: "acct-alice".into(),
                    root: ac.device.clone(),
                    name: "alice".into(),
                    role: "editor".into(),
                    created: 1,
                },
            )
            .unwrap();

        let p = plan(&app, "acct-alice").unwrap();
        assert_eq!(p.confirm, "alice");
        assert_eq!(p.disband.iter().map(|d| d.0.as_str()).collect::<Vec<_>>(), ["t1"]);
        assert!(p.blockers.is_empty() && p.pays.is_empty() && p.retain_for.is_empty());
        assert!(delete_account(&app, "acct-alice", "bob").await.is_err(), "the wrong word");
        delete_account(&app, "acct-alice", " Alice ").await.unwrap();

        assert_eq!(app.db.mentions("acct-alice"), Vec::<String>::new());
        assert_eq!(app.db.mentions(&ac.device), Vec::<String>::new());
        assert_eq!(app.db.mentions(&box1), Vec::<String>::new());
        // Only Bob's notice that it went, which names it.
        assert_eq!(app.db.mentions("t1"), ["notices.title"]);
        assert_eq!(app.db.notices("acct-bob").unwrap()[0].1, "team t1 was deleted");
        // Its machine is refused; Bob's is his own again, untouched.
        assert!(app.db.daemon_cert(&box1).unwrap().is_none());
        // It's told why if it asks again (#208); Bob's isn't on that list.
        assert!(app.db.daemon_account_deleted(&box1).unwrap());
        assert!(!app.db.daemon_account_deleted(&bobs).unwrap());
        assert!(app.db.daemon_cert(&bobs).unwrap().is_some());
        assert_eq!(app.db.daemon_team(&bobs).unwrap(), None);
        assert!(app.db.team("t2").unwrap().is_some());
        // Signing in with that GitHub account again starts afresh.
        let again = app.db.account_for("github", "gh-acct-alice", "alice", "acct-new", 2).unwrap();
        assert_eq!(again, "acct-new");
    }

    #[tokio::test]
    async fn a_team_keeps_checking_after_an_owner_who_signed_leaves() {
        let app = app();
        let (ck, cc) = person(&app, "acct-carol", "carol");
        let (bk, bc) = person(&app, "acct-bob", "bob");
        let (_dk, dc) = person(&app, "acct-dave", "dave");
        let carol = || member("acct-carol", &cc, TeamRole::Owner);
        let v1 = roster("t3", 1, vec![carol()], &ck);
        found(&app, &v1, "acct-carol", &cc);
        next(&app, &roster("t3", 2, vec![carol(), member("acct-bob", &bc, TeamRole::Owner)], &ck));
        // Bob, an owner, signs Dave in.
        let v3 = roster(
            "t3",
            3,
            vec![carol(), member("acct-bob", &bc, TeamRole::Owner), member("acct-dave", &dc, TeamRole::Viewer)],
            &bk,
        );
        next(&app, &v3);
        // While he's still an owner, he leaves first.
        let p = plan(&app, "acct-bob").unwrap();
        assert_eq!(p.blockers.len(), 1, "{:?}", p.blockers);
        assert!(delete_account(&app, "acct-bob", "bob").await.is_err());
        next(&app, &roster("t3", 4, vec![carol(), member("acct-dave", &dc, TeamRole::Viewer)], &ck));
        // Dave, a plain member, could go any time; he stays listed.
        let p = plan(&app, "acct-dave").unwrap();
        assert!(p.blockers.is_empty() && p.retain_for.is_empty() && p.disband.is_empty());
        assert_eq!(p.tell.len(), 1);

        delete_account(&app, "acct-bob", "bob").await.unwrap();
        // Only his signing devices' certificates are left, for the history.
        let mut left = app.db.mentions("acct-bob");
        left.sort();
        assert!(left.iter().all(|t| t.starts_with("retained_") || t.starts_with("rosters.")), "{left:?}");
        assert!(app.db.account("acct-bob").unwrap().is_none());
        assert!(checks(&app, "t3"), "the roster Bob signed still checks");
        // The team goes with its founder; so do the kept certificates.
        delete_account(&app, "acct-carol", "carol").await.unwrap();
        assert!(app.db.retained_certs("acct-bob").unwrap().is_none());
        assert!(app.db.mentions("acct-bob").is_empty(), "{:?}", app.db.mentions("acct-bob"));
    }

    #[tokio::test]
    async fn a_sole_owner_hands_over_first() {
        let app = app();
        let (ak, ac) = person(&app, "acct-ann", "ann");
        let (_bk, bc) = person(&app, "acct-ben", "ben");
        let v1 = roster("t4", 1, vec![member("acct-ann", &ac, TeamRole::Owner)], &ak);
        found(&app, &v1, "acct-ann", &ac);
        next(
            &app,
            &roster(
                "t4",
                2,
                vec![member("acct-ann", &ac, TeamRole::Owner), member("acct-ben", &bc, TeamRole::Owner)],
                &ak,
            ),
        );
        // Ben, made an owner but never signing, is an owner all the same.
        assert!(plan(&app, "acct-ben").unwrap().blockers[0].contains("an owner of"));
    }

    #[test]
    fn session_ids_are_prefixes_of_this_accounts_only() {
        let db = Db::memory();
        db.account_for("github", "1", "a", "acct-a", 1).unwrap();
        db.account_for("github", "2", "b", "acct-b", 1).unwrap();
        let h = crate::auth::hash("tok");
        db.add_session(&h, "acct-a", 1, i64::MAX as u64).unwrap();
        assert_eq!(db.drop_session_of("acct-b", &h[..16]).unwrap(), 0);
        assert_eq!(db.drop_session_of("acct-a", &h[..4]).unwrap(), 0, "too short to name one");
        assert_eq!(db.drop_session_of("acct-a", &h[..16]).unwrap(), 1);
        assert!(db.session(&h, 2).unwrap().is_none());
    }

    #[test]
    fn expired_sessions_are_pruned() {
        let db = Db::memory();
        db.account_for("github", "1", "a", "acct-a", 1).unwrap();
        db.add_session("old", "acct-a", 1, 10).unwrap();
        db.add_session("new", "acct-a", 1, 100).unwrap();
        assert_eq!(db.prune(50).unwrap(), 1);
        assert_eq!(db.sessions("acct-a", 0).unwrap().len(), 1);
    }

    #[test]
    fn the_last_way_to_sign_in_stays() {
        let db = Db::memory();
        let a = db.account_for("passkey", "pk1", "", "acct-a", 1).unwrap();
        db.add_passkey("pk1", &a, -7, b"k", 1).unwrap();
        assert!(db.drop_passkey(&a, "pk1").unwrap().is_err());
        db.add_passkey("pk2", &a, -7, b"k", 2).unwrap();
        assert!(db.drop_passkey(&a, "nope").unwrap().is_err());
        db.drop_passkey(&a, "pk1").unwrap().unwrap();
        assert_eq!(db.passkey_count(&a).unwrap(), 1);
        assert_eq!(db.sign_ins(&a).unwrap(), 1);
        // The passkey that made it was its identity too: gone with it.
        assert!(db.mentions("pk1").is_empty(), "{:?}", db.mentions("pk1"));
    }
}
