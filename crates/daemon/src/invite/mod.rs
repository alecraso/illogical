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
//! Neither route is in `authz`: unmatched paths are the owner's. Nor an
//! agent's on the owner's CLI (`X-Illogical-Agent`): an agent asks with
//! `invite_person`, and the owner sends it.
//!
//! From a thread (#297: the owner's offer when an @ named someone who
//! can't read it), it opens that thread: from the message, or all of it,
//! and no other thread's past.
//!
//! An agent's invite (#234, MCP's `invite_person`) is a draft on a card
//! ([`card`]) that only the owner sends: then [`run`] does the same, as
//! them.

pub mod card;

use std::sync::{Arc, OnceLock, Weak};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use illogical_core::{Role, SessionId};
use illogical_proto::{
    PaneId, ThreadTarget,
    api::{InviteDelivery, InviteGrant, InviteRequest, Invited, TeamPins, TeamPinsRequest},
};
use serde_json::json;

use crate::{
    acl::{Principal, ThreadFrom},
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

/// What runs an approved invite card (#234): blocks are made before the
/// server is, so it's set once it is.
pub type Hook = Arc<OnceLock<Weak<App>>>;

/// Who answers an invite card: the owner, by any route.
pub const OWNER_ONLY: &str = "only the session's owner sends or declines an invite";

/// ...and closes the block it waits on (its drafts would go with it).
pub const CLOSE_OWNER_ONLY: &str = "only the session's owner closes an invite block";

/// An agent on the owner's CLI asks; the owner sends.
pub const AGENT_ASKS: &str = "an agent doesn't invite: ask the user with illogical's invite_person tool";

/// Whether the owner's CLI says an agent runs it (as for a forge's drafts).
fn agent(headers: &HeaderMap) -> bool {
    headers.get("x-illogical-agent").is_some()
}

/// An invite an agent drafted and the owner sent (#234), for the audit
/// log: who drafted it, from which pane, and who sent it.
#[derive(Debug, Clone)]
pub struct Drafted {
    pub by: String,
    pub pane: PaneId,
    pub approved_by: String,
}

/// Whom an invite names, and the root their devices chain back to.
#[derive(Debug)]
pub struct Person {
    pub id: String,
    pub name: String,
    root: Option<String>,
    /// Another principal [`nameable`] took to be them (a login by their
    /// name): shown, so the owner sees whom it chose.
    pub merged: Option<String>,
}

const UNKNOWN: &str = "share once from the web (it checks their fingerprint), then invite works";

/// Everyone an invite may name (#297): those shared with, and the members
/// of rosters this daemon checked, as [`resolve`] finds them. One person
/// known two ways (a login and an account of the same name) is their
/// account, which control reaches; not whoever owns this machine.
pub fn nameable(app: &App) -> Vec<Person> {
    let mut out: Vec<Person> = Vec::new();
    let grants = app
        .acl
        .list()
        .into_iter()
        .filter(|g| g.principal.starts_with("tailnet:") || g.principal.starts_with("account:"));
    let people = grants.map(|g| Person { id: g.principal, name: g.name, root: g.root, merged: None }).chain(
        app.control.known().into_iter().map(|k| Person {
            id: format!("account:{}", k.account),
            name: k.name,
            root: Some(k.root),
            merged: None,
        }),
    );
    let account = |p: &Person| p.id.starts_with("account:");
    let key = |p: &Person| p.name.split('@').next().unwrap_or("").trim().to_lowercase();
    for p in people {
        if is_me(app, &p.id) || out.iter().any(|o| o.id == p.id) {
            continue;
        }
        // A login and an account by one name: the account, saying which
        // login it stands for. Two logins, or two accounts, are two people.
        match out.iter().position(|o| account(o) != account(&p) && key(o) == key(&p)) {
            Some(i) if account(&p) => {
                let login = std::mem::replace(&mut out[i], p);
                out[i].merged = Some(login.id);
            }
            Some(i) => out[i].merged = Some(p.id),
            None => out.push(p),
        }
    }
    out
}

/// Who `who` is, by what this daemon knows itself: its grants and the
/// rosters it checked. An explicit root counts only where nothing else
/// vouches for one.
pub fn resolve(app: &App, who: &str, root: Option<String>) -> Result<Person, (StatusCode, String)> {
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
        return Ok(Person { id: format!("tailnet:{login}"), name: login, root: None, merged: None });
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
            return Ok(Person { id: who.to_owned(), name: g.name.clone(), root: g.root.clone(), merged: None });
        }
        if let Some(k) = known.iter().find(|k| k.account == account) {
            return Ok(Person { id: who.to_owned(), name: k.name.clone(), root: Some(k.root.clone()), merged: None });
        }
        return match root {
            Some(r) if ok_id(&r) && r.bytes().all(|c| c.is_ascii_alphanumeric()) => {
                Ok(Person { id: who.to_owned(), name: account.to_owned(), root: Some(r), merged: None })
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
            found.push(Person { id: g.principal.clone(), name: g.name.clone(), root: g.root.clone(), merged: None });
        }
    }
    for k in &known {
        let id = format!("account:{}", k.account);
        if (same(&k.name) || same(&k.account)) && !found.iter().any(|p| p.id == id) {
            found.push(Person { id, name: k.name.clone(), root: Some(k.root.clone()), merged: None });
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

async fn invite(State(app): State<Arc<App>>, headers: HeaderMap, Json(b): Json<InviteRequest>) -> Response {
    if agent(&headers) {
        return refuse(StatusCode::FORBIDDEN, AGENT_ASKS);
    }
    match run(&app, b, None).await {
        Ok(v) => Json(v).into_response(),
        Err((status, why)) => refuse(status, why),
    }
}

/// Whether `id` is this machine's own account, whom an invite can't reach
/// by name: it's the owner.
pub fn is_me(app: &App, id: &str) -> bool {
    id.strip_prefix("account:").is_some_and(|a| app.control.is_me(a))
}

/// Whether `id` owns this machine through its team (#386): every session
/// is theirs already, so an invite only tells them.
pub fn owns_here(app: &App, id: &str) -> bool {
    id.strip_prefix("account:").is_some_and(|a| app.control.owns_here(a))
}

/// Invite someone, as the owner: `POST /api/invite`'s, or an agent's card
/// the owner sent (`drafted`).
pub async fn run(app: &App, b: InviteRequest, drafted: Option<&Drafted>) -> Result<Invited, (StatusCode, String)> {
    let want = b.role.unwrap_or(Role::Viewer);
    if want == Role::Owner {
        return Err(no(StatusCode::BAD_REQUEST, "an invite makes someone a viewer or an editor"));
    }
    if let Some(m) = b.drive_minutes {
        if want != Role::Editor {
            return Err(no(StatusCode::BAD_REQUEST, "only an editor may be trusted to drive"));
        }
        if !(1..=24 * 60).contains(&m) {
            return Err(no(StatusCode::BAD_REQUEST, "drive_minutes: 1 to 1440"));
        }
    }
    let thread = from_thread(app, &b).await?;
    let pane = b.pane.or(match thread.as_ref().and_then(|t| ThreadTarget::parse(&t.thread)) {
        Some(ThreadTarget::Pane(p)) => Some(p),
        _ => None,
    });
    let (pane, session_name) = match app.mux.api(|r| Api::InviteTo(b.session, pane, r)).await {
        Some(Ok(x)) => x,
        Some(Err(why)) => return Err(no(StatusCode::NOT_FOUND, why)),
        None => return Err(no(StatusCode::SERVICE_UNAVAILABLE, "shutting down")),
    };
    let person = resolve(app, &b.who, b.root)?;
    if is_me(app, &person.id) {
        return Err(no(StatusCode::BAD_REQUEST, format!("{} owns this machine", person.name)));
    }
    let co_owner = owns_here(app, &person.id);

    // The grant: none if they hold the role already (a team daemon's
    // members hold theirs on every session, its owners everything), an
    // upgrade if lower, never a downgrade of one.
    let grant =
        app.acl.list().into_iter().find(|g| g.session == b.session && g.principal == person.id).filter(|_| !co_owner);
    if let Some(h) = grant.as_ref().map(|g| g.role).filter(|h| *h > want) {
        return Err(no(
            StatusCode::CONFLICT,
            format!(
                "{} is {} {} already: revoke first to make them {} {}",
                person.name,
                article(h),
                h.as_str(),
                article(want),
                want.as_str()
            ),
        ));
    }
    let granted = !co_owner && app.acl.role_of(&person.id, b.session).is_none_or(|h| h < want);
    if granted {
        if person.id.starts_with("account:") && person.root.is_none() {
            return Err(no(StatusCode::BAD_REQUEST, format!("sharing with {} needs their root device", person.name)));
        }
        // A new share is from now on unless asked; one held keeps its own.
        let from = if b.history || grant.is_some() {
            None
        } else {
            match app.mux.api(|r| Api::SessionEnds(b.session, r)).await.flatten() {
                Some(ends) => Some(ends),
                None => return Err(no(StatusCode::NOT_FOUND, "no such session")),
            }
        };
        // Where the thread it came from starts for them: only a new grant's
        // (one held keeps its own), and moot with history.
        let thread_from = thread.clone().filter(|_| grant.is_none() && from.is_some());
        let set = app.acl.set_full(
            b.session,
            &person.id,
            &person.name,
            Some(want),
            "owner",
            from,
            person.root.clone(),
            thread_from,
        );
        if let Err(e) = set {
            return Err(no(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
        }
        app.mux.send(Cmd::AclChanged);
        app.control.poke();
    }
    let drive = match b.drive_minutes.filter(|_| !co_owner) {
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
    let mut extra = json!({ "tag": format!("invite-{id}"), "invite": id, "session": b.session });
    // A tap opens the thread they were mentioned in.
    if let Some(t) = &thread {
        extra["thread"] = json!(t.thread);
    }
    // Whoever of theirs is here now hears at once.
    let told = if note.is_empty() { title.clone() } else { format!("{title}: {note}") };
    app.mux.send(Cmd::Api(Api::Tell(person.id.clone(), told)));

    let (delivery, reason) = deliver(app, &person, pane, &title, &body, &extra, granted).await;
    let mut line = json!({
        "at": now_ms(), "by": "owner", "action": "invite", "session": b.session, "principal": person.id,
        "name": person.name, "role": want, "pane": pane, "granted": granted, "delivery": delivery,
    });
    // An agent's draft: who sent it, and who drafted it where (#234).
    if let Some(d) = drafted {
        line["approved_by"] = json!(d.approved_by);
        line["drafted_by"] = json!(d.by);
        line["drafted_in"] = json!(d.pane);
    }
    app.acl.record(line);
    let role = if co_owner { Role::Owner } else { app.acl.role_of(&person.id, b.session).unwrap_or(want) };
    Ok(Invited {
        invite: id,
        grant: InviteGrant { session: b.session, principal: person.id, name: person.name, role, granted },
        pane,
        delivery,
        reason,
        drive,
    })
}

/// An invite from a thread (#297): the thread, checked to be in the
/// session, and where it starts for them: the message, posted already and
/// in that thread, or all of it.
async fn from_thread(app: &App, b: &InviteRequest) -> Result<Option<ThreadFrom>, (StatusCode, String)> {
    let Some(key) = &b.thread else {
        if b.msg.is_some() || b.whole_thread {
            return Err(no(StatusCode::BAD_REQUEST, "msg and whole_thread go with a thread"));
        }
        return Ok(None);
    };
    let target = ThreadTarget::parse(key).ok_or_else(|| no(StatusCode::BAD_REQUEST, "thread: pane-N or session-N"))?;
    let (session, at) = match app.mux.api(|r| Api::ThreadPlace(target, b.msg, r)).await {
        Some(Ok(x)) => x,
        Some(Err(why)) => return Err(no(StatusCode::BAD_REQUEST, why)),
        None => return Err(no(StatusCode::SERVICE_UNAVAILABLE, "shutting down")),
    };
    if session != b.session {
        return Err(no(StatusCode::BAD_REQUEST, format!("{key} isn't in session {}", b.session)));
    }
    let from = match (b.msg, at) {
        (None, _) if b.whole_thread => 0,
        (None, _) => {
            return Err(no(StatusCode::BAD_REQUEST, "msg: the message they're brought in to see (or whole_thread)"));
        }
        (Some(m), None) => return Err(no(StatusCode::BAD_REQUEST, format!("{key} has no message {m}"))),
        (Some(_), Some(_)) if b.whole_thread => 0,
        (Some(_), Some(at)) => at,
    };
    Ok(Some(ThreadFrom { thread: target.key(), from }))
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
) -> (InviteDelivery, Option<String>) {
    let to = person.id.as_str();
    let (here, took) = match &app.push {
        Some(p) => p.send_report(pane, title, body, Some(extra.clone()), |w| w == to).await,
        None => (0, 0),
    };
    if took > 0 {
        return (InviteDelivery::Sent, None);
    }
    let through_control = to.starts_with("account:") && app.control.enrolled().is_some();
    if !through_control {
        return match (to.starts_with("account:"), here) {
            (true, _) => (InviteDelivery::Unreachable, Some("this machine isn't joined to illogical control".into())),
            (false, 0) => (InviteDelivery::Unreachable, Some("they haven't turned on notifications here".into())),
            (false, _) => (InviteDelivery::Unreachable, Some("their push service turned it down".into())),
        };
    }
    // Someone just granted needs a refresh to be pushable: control learns
    // they're let in, then hands over their subscriptions (#232).
    let refreshed = !granted || app.control.refresh_now(REFRESH_WAIT).await;
    let got = app.control.push_report(pane, title, body, Some(extra.clone()), |p| p.id() == to).await;
    if got.relayed > 0 {
        return (InviteDelivery::Sent, None);
    }
    let why = if !refreshed {
        "control hasn't answered yet; it goes out once it does"
    } else if got.refused > 0 || !app.control.reaches(to) {
        "they haven't accepted this machine's share yet; it goes out once they do"
    } else if got.matched > 0 {
        "control didn't relay it; it's tried again"
    } else {
        return (InviteDelivery::Unreachable, Some("they haven't turned on notifications".into()));
    };
    app.control.wait_invite(Waiting {
        who: to.to_owned(),
        session: extra["session"].as_u64().and_then(|s| SessionId::try_from(s).ok()),
        pane,
        title: title.to_owned(),
        body: body.to_owned(),
        extra: extra.clone(),
        at: now_ms(),
    });
    (InviteDelivery::Pending, Some(why.into()))
}

fn pins_now(app: &App) -> Response {
    Json(TeamPins { pins: app.control.team_pins(), checked: app.control.checked_teams() }).into_response()
}

async fn pins(State(app): State<Arc<App>>) -> Response {
    pins_now(&app)
}

/// The owner's browser hands over the teams it pinned (#233), and those
/// it left; their rosters are fetched and checked against these, now.
async fn add_pins(State(app): State<Arc<App>>, headers: HeaderMap, Json(b): Json<TeamPinsRequest>) -> Response {
    if agent(&headers) {
        return refuse(StatusCode::FORBIDDEN, "the owner's browser pins teams, not an agent");
    }
    if app.control.enrolled().is_none() {
        return refuse(StatusCode::BAD_REQUEST, "this machine isn't joined to illogical control");
    }
    let well_formed = |team: &str, root: &str| {
        ok_id(team) && root.split_once('.').is_some_and(|(f, r)| ok_id(f) && ok_id(r) && !r.contains('.'))
    };
    if b.pins.len() > 50 || !b.pins.iter().all(|(t, r)| well_formed(t, r)) {
        return refuse(StatusCode::BAD_REQUEST, "pins: team id to <founder>.<founder's root>, 50 at most");
    }
    if b.drop.len() > 50 || !b.drop.iter().all(|t| ok_id(t)) {
        return refuse(StatusCode::BAD_REQUEST, "drop: team ids, 50 at most");
    }
    match app.control.set_team_pins(b.pins, &b.drop) {
        Ok(true) => {
            app.control.refresh_now(REFRESH_WAIT).await;
        }
        Ok(false) => {}
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
    pins_now(&app)
}
