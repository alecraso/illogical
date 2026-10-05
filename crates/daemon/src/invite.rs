//! Invite a person into a session (#233): share it with them and tell
//! them, and only them, that the owner wants them there.
//!
//! `POST /api/invite` grants the role (unless they hold it already), then
//! pushes that one person, opening at a pane. It says how that went:
//! `sent` once a subscription took it (here, or relayed by control),
//! `pending` while control can't reach them yet (they haven't accepted
//! this machine's share; it goes out after a later refresh, for a day),
//! else `unreachable`, with why. Never "sent" for having tried.
//!
//! Whom it may name: a tailnet login, someone already shared with, or a
//! member of a roster this daemon checked itself: its own team's, or a
//! team the owner's browser pinned (`POST /api/team-pins`). Control never
//! says who is who. Nobody else: sharing from the web checks their
//! fingerprint first.
//!
//! Neither route is in `authz`: unmatched paths are the owner's.

use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use illogical_core::{Role, SessionId};
use illogical_proto::PaneId;
use serde::Deserialize;
use serde_json::json;

use crate::{
    acl::Principal,
    control::{REFRESH_WAIT, Waiting},
    mux::{Api, Cmd},
    server::App,
    store::now_ms,
};

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/invite", post(invite)).route("/api/team-pins", get(pins).post(add_pins))
}

fn refuse(status: StatusCode, why: impl Into<String>) -> Response {
    (status, Json(json!({ "error": why.into() }))).into_response()
}

fn no(status: StatusCode, why: impl Into<String>) -> (StatusCode, String) {
    (status, why.into())
}

fn ok_id(rest: &str) -> bool {
    !rest.is_empty() && rest.len() <= 200 && !rest.chars().any(|c| c.is_control() || c.is_whitespace())
}

#[derive(Deserialize)]
struct Request {
    session: SessionId,
    /// `tailnet:<login>`, `account:<id>`, or a name: someone shared with,
    /// or in a checked roster.
    who: String,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    note: Option<String>,
    /// Where it opens (default: the session's first pane).
    #[serde(default)]
    pane: Option<PaneId>,
    /// With history (default: from now on), for a new grant.
    #[serde(default)]
    history: bool,
    /// An editor may also type on this machine's pane for so long (M14).
    #[serde(default)]
    drive_minutes: Option<u32>,
    /// For an `account:` no grant or pin vouches for: their root device,
    /// whose fingerprint the owner checked with them.
    #[serde(default)]
    root: Option<String>,
}

/// Whom an invite names, and the root their devices chain back to.
#[derive(Debug)]
struct Person {
    id: String,
    name: String,
    root: Option<String>,
}

const UNKNOWN: &str = "share once from the web (it checks their fingerprint), then invite works";

/// Who `who` is, by what this daemon knows itself: its grants and the
/// rosters it checked. An explicit root counts only where nothing else
/// vouches for one.
fn resolve(app: &App, who: &str, root: Option<String>) -> Result<Person, (StatusCode, String)> {
    let who = who.trim();
    let grants: Vec<_> = app
        .acl
        .list()
        .into_iter()
        .filter(|g| g.principal.starts_with("tailnet:") || g.principal.starts_with("account:"))
        .collect();
    let known = app.control.known();
    if who.starts_with("team:") {
        return Err(no(StatusCode::BAD_REQUEST, "invite a person; share with a team from the share dialog"));
    }
    if let Some(login) = who.strip_prefix("tailnet:") {
        if !ok_id(login) {
            return Err(no(StatusCode::BAD_REQUEST, "tailnet:<login>"));
        }
        let login = login.to_ascii_lowercase();
        return Ok(Person { id: format!("tailnet:{login}"), name: login, root: None });
    }
    let unknown = || {
        let hint = if who.contains('@') { format!(" (a tailnet login: tailnet:{who})") } else { String::new() };
        no(StatusCode::NOT_FOUND, format!("this machine doesn't know {who}: {UNKNOWN}{hint}"))
    };
    if let Some(account) = who.strip_prefix("account:") {
        if !ok_id(account) {
            return Err(no(StatusCode::BAD_REQUEST, "account:<id>"));
        }
        if let Some(g) = grants.iter().find(|g| g.principal == who) {
            return Ok(Person { id: who.to_owned(), name: g.name.clone(), root: g.root.clone() });
        }
        if let Some(k) = known.iter().find(|k| k.account == account) {
            return Ok(Person { id: who.to_owned(), name: k.name.clone(), root: Some(k.root.clone()) });
        }
        return match root {
            Some(r) if ok_id(&r) && r.bytes().all(|c| c.is_ascii_alphanumeric()) => {
                Ok(Person { id: who.to_owned(), name: account.to_owned(), root: Some(r) })
            }
            Some(_) => Err(no(StatusCode::BAD_REQUEST, "root: a device id")),
            None => Err(unknown()),
        };
    }
    // A name: someone shared with, or a checked roster's member.
    let same = |a: &str| a.eq_ignore_ascii_case(who);
    let mut found: Vec<Person> = Vec::new();
    for g in &grants {
        let tail = g.principal.split_once(':').map_or("", |(_, t)| t);
        if (same(&g.name) || same(tail)) && !found.iter().any(|p| p.id == g.principal) {
            found.push(Person { id: g.principal.clone(), name: g.name.clone(), root: g.root.clone() });
        }
    }
    for k in &known {
        let id = format!("account:{}", k.account);
        if (same(&k.name) || same(&k.account)) && !found.iter().any(|p| p.id == id) {
            found.push(Person { id, name: k.name.clone(), root: Some(k.root.clone()) });
        }
    }
    match found.len() {
        0 if who.is_empty() => Err(no(StatusCode::BAD_REQUEST, "who: a name, tailnet:<login> or account:<id>")),
        0 => Err(unknown()),
        1 => Ok(found.remove(0)),
        _ => {
            let ids: Vec<&str> = found.iter().map(|p| p.id.as_str()).collect();
            Err(no(StatusCode::CONFLICT, format!("{who} could be {}: name one", ids.join(" or "))))
        }
    }
}

async fn invite(State(app): State<Arc<App>>, Json(b): Json<Request>) -> Response {
    let want = b.role.unwrap_or(Role::Viewer);
    if want == Role::Owner {
        return refuse(StatusCode::BAD_REQUEST, "an invite makes someone a viewer or an editor");
    }
    if let Some(m) = b.drive_minutes {
        if want != Role::Editor {
            return refuse(StatusCode::BAD_REQUEST, "only an editor may be trusted to drive");
        }
        if !(1..=24 * 60).contains(&m) {
            return refuse(StatusCode::BAD_REQUEST, "drive_minutes: 1 to 1440");
        }
    }
    let (pane, session_name) = match app.mux.api(|r| Api::InviteTo(b.session, b.pane, r)).await {
        Some(Ok(x)) => x,
        Some(Err(why)) => return refuse(StatusCode::NOT_FOUND, why),
        None => return refuse(StatusCode::SERVICE_UNAVAILABLE, "shutting down"),
    };
    let person = match resolve(&app, &b.who, b.root) {
        Ok(p) => p,
        Err((status, why)) => return refuse(status, why),
    };
    if person.id.strip_prefix("account:").is_some_and(|a| app.control.owns_here(a)) {
        return refuse(StatusCode::BAD_REQUEST, format!("{} owns this machine already", person.name));
    }

    // The grant: none if they hold the role already (a team daemon's
    // members hold theirs on every session), an upgrade if lower, never a
    // downgrade of one.
    let grant = app.acl.list().into_iter().find(|g| g.session == b.session && g.principal == person.id);
    if let Some(h) = grant.as_ref().map(|g| g.role).filter(|h| *h > want) {
        return refuse(
            StatusCode::CONFLICT,
            format!(
                "{} is {} {} already: revoke first to make them {} {}",
                person.name,
                article(h),
                h.as_str(),
                article(want),
                want.as_str()
            ),
        );
    }
    let granted = app.acl.role_of(&person.id, b.session).is_none_or(|h| h < want);
    if granted {
        if person.id.starts_with("account:") && person.root.is_none() {
            return refuse(StatusCode::BAD_REQUEST, format!("sharing with {} needs their root device", person.name));
        }
        // A new share is from now on unless asked; one held keeps its own.
        let from = if b.history || grant.is_some() {
            None
        } else {
            match app.mux.api(|r| Api::SessionEnds(b.session, r)).await.flatten() {
                Some(ends) => Some(ends),
                None => return refuse(StatusCode::NOT_FOUND, "no such session"),
            }
        };
        let set = app.acl.set_full(b.session, &person.id, &person.name, Some(want), "owner", from, person.root.clone());
        if let Err(e) = set {
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        }
        app.mux.send(Cmd::AclChanged);
        app.control.poke();
    }
    let drive = match b.drive_minutes {
        Some(m) => Some(app.mux.api(|r| Api::Trust(pane, person.id.clone(), m, r)).await.unwrap_or(false)),
        None => None,
    };

    let id = hex::encode(crate::push::random::<4>());
    // Who invites: the account's login on control, else the owner's here.
    let login = app.control.enrolled().map(|e| e.saved.login.clone()).filter(|l| !l.is_empty());
    let owner = match login {
        Some(l) => l,
        None => app.mux.api(|r| Api::Who(Principal::Owner, r)).await.map_or_else(|| "The owner".into(), |d| d.name),
    };
    let note: String = b.note.as_deref().unwrap_or("").trim().chars().take(300).collect();
    let title = format!("{owner} brought you into {session_name}");
    let body = if note.is_empty() { "Open it to join in.".to_owned() } else { note.clone() };
    let extra = json!({ "tag": format!("invite-{id}"), "invite": id, "session": b.session });
    // Whoever of theirs is here now hears at once.
    let told = if note.is_empty() { title.clone() } else { format!("{title}: {note}") };
    app.mux.send(Cmd::Api(Api::Tell(person.id.clone(), told)));

    let (delivery, reason) = deliver(&app, &person, pane, &title, &body, &extra, granted).await;
    app.acl.record(json!({
        "at": now_ms(), "by": "owner", "action": "invite", "session": b.session, "principal": person.id,
        "name": person.name, "role": want, "pane": pane, "granted": granted, "delivery": delivery,
    }));
    let role = app.acl.role_of(&person.id, b.session).unwrap_or(want);
    Json(json!({
        "invite": id,
        "grant": { "session": b.session, "principal": person.id, "name": person.name, "role": role, "granted": granted },
        "pane": pane,
        "delivery": delivery,
        "reason": reason,
        "drive": drive,
    }))
    .into_response()
}

fn article(r: Role) -> &'static str {
    if r == Role::Viewer { "a" } else { "an" }
}

/// Push the invite to that person alone, by their principal id, here and
/// through control; how it went is what the subscriptions said.
async fn deliver(
    app: &App,
    person: &Person,
    pane: PaneId,
    title: &str,
    body: &str,
    extra: &serde_json::Value,
    granted: bool,
) -> (&'static str, Option<String>) {
    let to = person.id.as_str();
    let (here, took) = match &app.push {
        Some(p) => p.send_report(pane, title, body, Some(extra.clone()), |w| w == to).await,
        None => (0, 0),
    };
    if took > 0 {
        return ("sent", None);
    }
    let through_control = to.starts_with("account:") && app.control.enrolled().is_some();
    if !through_control {
        return match (to.starts_with("account:"), here) {
            (true, _) => ("unreachable", Some("this machine isn't joined to illogical control".into())),
            (false, 0) => ("unreachable", Some("they haven't turned on notifications here".into())),
            (false, _) => ("unreachable", Some("their push service turned it down".into())),
        };
    }
    // Someone just granted needs a refresh to be pushable: control learns
    // they're let in, then hands over their subscriptions (#232).
    let refreshed = !granted || app.control.refresh_now(REFRESH_WAIT).await;
    let got = app.control.push_report(pane, title, body, Some(extra.clone()), |p| p.id() == to).await;
    if got.relayed > 0 {
        return ("sent", None);
    }
    let why = if !refreshed {
        "control hasn't answered yet; it goes out once it does"
    } else if got.refused > 0 || !app.control.reaches(to) {
        "they haven't accepted this machine's share yet; it goes out once they do"
    } else if got.matched > 0 {
        "control didn't relay it; it's tried again"
    } else {
        return ("unreachable", Some("they haven't turned on notifications".into()));
    };
    app.control.wait_invite(Waiting {
        who: to.to_owned(),
        pane,
        title: title.to_owned(),
        body: body.to_owned(),
        extra: extra.clone(),
        at: now_ms(),
    });
    ("pending", Some(why.into()))
}

#[derive(Deserialize)]
struct Pins {
    /// Team id to `<founder device>.<founder's root>`, as the owner's
    /// browser pinned it.
    pins: BTreeMap<String, String>,
}

fn pins_now(app: &App) -> Response {
    Json(json!({ "pins": app.control.team_pins(), "checked": app.control.checked_teams() })).into_response()
}

async fn pins(State(app): State<Arc<App>>) -> Response {
    pins_now(&app)
}

/// The owner's browser hands over the teams it pinned (#233); their
/// rosters are fetched and checked against these, now.
async fn add_pins(State(app): State<Arc<App>>, Json(b): Json<Pins>) -> Response {
    if app.control.enrolled().is_none() {
        return refuse(StatusCode::BAD_REQUEST, "this machine isn't joined to illogical control");
    }
    let well_formed = |team: &str, root: &str| {
        ok_id(team) && root.split_once('.').is_some_and(|(f, r)| ok_id(f) && ok_id(r) && !r.contains('.'))
    };
    if b.pins.len() > 50 || !b.pins.iter().all(|(t, r)| well_formed(t, r)) {
        return refuse(StatusCode::BAD_REQUEST, "pins: team id to <founder>.<founder's root>, 50 at most");
    }
    match app.control.add_team_pins(b.pins) {
        Ok(true) => {
            app.control.refresh_now(REFRESH_WAIT).await;
        }
        Ok(false) => {}
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
    pins_now(&app)
}
