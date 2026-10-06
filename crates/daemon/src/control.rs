//! Enrolled in illogical control (M17, M18): who may connect, and the way
//! in through control's relay.
//!
//! `illogicald join URL` makes this daemon's keys (`<state>/daemon.key`),
//! asks control for a code, and waits until someone approves it from a
//! device of theirs. It then shows the account's fingerprint (its root
//! device's id), which the person checks against the device they approved
//! on: control could otherwise hand back an account of its own. Only then
//! does it pin that root and save it all in `<state>/control.json`. The
//! running daemon notices the file (within a few seconds), and from then
//! on:
//!
//! - it keeps the account's certificates fresh from control, but decides
//!   for itself which devices to trust ([`Trust::evaluate`] against the
//!   pinned root), so control can't add one;
//! - it keeps a socket open to control's relay and serves channels over
//!   it, for clients that can't reach it directly;
//! - `/e2e` serves the same channels directly (tailnet, LAN).
//!
//! When control is down, the last certificates it sent keep working.
//! `illogicald leave` tells control and removes the file.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, bail};
use illogical_core::Role;
use illogical_e2e::{
    Cert, DeviceKeys, Kind, Revocation, Trust,
    cert::{Trusted, join_code},
    keys::fingerprint,
    now_ms,
    push::PushSub,
    team::{AccountCerts, Move, Roster, TeamPin, TeamRole},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::{
    acl::{Acl, Principal},
    server::App,
};

pub const FILE: &str = "control.json";
const PINS_FILE: &str = "team-pins.json";
const INVITES_FILE: &str = "invites.json";
pub const KEY_FILE: &str = "daemon.key";
const REFRESH: Duration = Duration::from_secs(60);
/// How long [`Control::refresh_now`] waits by default (#232).
pub const REFRESH_WAIT: Duration = Duration::from_secs(10);
/// What this daemon tells control it understands, so control offers only
/// what every daemon checking a team can take (presigned invites' rosters).
/// `ILLOGICAL_FEATURES` says otherwise (tests play an older daemon with "").
fn features() -> String {
    std::env::var("ILLOGICAL_FEATURES").unwrap_or_else(|_| "presigned-invites,owner-moves".into())
}
const WATCH: Duration = Duration::from_secs(3);

/// `<state>/control.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saved {
    pub url: String,
    pub trust: Trust,
    pub cert: Cert,
    #[serde(default)]
    pub certs: Vec<Cert>,
    #[serde(default)]
    pub revocations: Vec<Revocation>,
    /// A team's machine (M19): the team and its founder, pinned at join.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<TeamPin>,
    /// The team's roster as this daemon last verified it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster: Option<Roster>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub team_certs: AccountCerts,
    /// The team's members' names as they set them (#208); the roster has
    /// one word each ("Sam-Stranger").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub team_names: BTreeMap<String, String>,
    /// The team is locked: only its owners get in.
    #[serde(default)]
    pub locked: bool,
    /// People sessions were shared with (by account): their certificates,
    /// checked against the roots pinned in their grants.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub peers: BTreeMap<String, PeerCerts>,
    /// Teams sessions were shared with (M30), by id: the latest roster that
    /// chains back to the founder pinned in the grant, and its members'
    /// certificates.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shared_teams: BTreeMap<String, SharedTeam>,
    /// The account's login on control (M30), for what to call its owner.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub login: String,
    /// When the last move it took was made (#100): older ones are replays.
    #[serde(default)]
    pub moved_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedTeam {
    pub roster: Roster,
    #[serde(default)]
    pub certs: AccountCerts,
    /// Members' names as they set them (#208).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, String>,
    #[serde(default)]
    pub locked: bool,
}

/// Take a move (#100) if a device of this machine's own account signed
/// it: into a team, between teams, or back to the account. An owner of
/// its team (in the roster it checked) may take it out too (#332). The new
/// team's roster is fetched from scratch, checked against the pin as at a
/// join. One no newer than the last it took is a replay (or the same one
/// again).
fn take_move(saved: &mut Saved, m: Move) {
    if m.at <= saved.moved_at {
        return;
    }
    let trusted = saved.trust.evaluate(&saved.certs, &saved.revocations);
    let own = trusted.get(&m.by).is_some_and(|by| m.signed_for(&saved.cert.device, by));
    let owner_out = saved.team.is_some()
        && saved.roster.as_ref().is_some_and(|r| m.owner_takes_out(&saved.cert.device, r, &saved.team_certs));
    if !own && !owner_out {
        warn!(
            by = m.by,
            "a move from control isn't signed by this account's devices or its team's owners; ignoring it"
        );
        return;
    }
    if saved.team != m.team {
        info!(team = m.team.as_ref().map(|p| p.team.as_str()), "moved");
        saved.team = m.team;
        saved.roster = None;
        saved.team_certs = Default::default();
        saved.team_names = Default::default();
        saved.locked = false;
    }
    saved.moved_at = m.at;
}

/// A team grant's pin (`<founder device>.<founder's root>`).
fn team_pin(team: &str, root: &str) -> Option<TeamPin> {
    let (founder, founder_root) = root.split_once('.')?;
    Some(TeamPin { team: team.to_owned(), founder: founder.to_owned(), founder_root: founder_root.to_owned() })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCerts {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub certs: Vec<Cert>,
    #[serde(default)]
    pub revocations: Vec<Revocation>,
}

/// How a push through control went (#232), by subscription.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pushed {
    /// Subscriptions the push was for.
    pub matched: usize,
    /// Those control took to relay.
    pub relayed: usize,
    /// Those control refused: not someone it routes to this daemon.
    pub refused: usize,
}

pub struct Enrolled {
    pub saved: Saved,
    pub keys: Arc<DeviceKeys>,
    /// The account's own devices: the owner.
    pub trusted: Trusted,
    /// Other people's devices (team members, people shared with), as who.
    pub others: Vec<(Cert, Principal)>,
    /// Team members' roles on every session (not owners: they're the owner).
    pub team_roles: HashMap<String, Role>,
    /// Members of teams sessions were shared with, by team, and their role
    /// in it (M30).
    pub shared_roles: HashMap<String, HashMap<String, Role>>,
    /// Push subscriptions (M21) whose signatures checked out, and whose.
    pub push: Vec<(Principal, PushSub)>,
}

impl Enrolled {
    fn build(saved: Saved, keys: Arc<DeviceKeys>, acl: &Acl) -> Self {
        let trusted = saved.trust.evaluate(&saved.certs, &saved.revocations);
        let mut others = Vec::new();
        let mut team_roles = HashMap::new();
        if let Some(r) = &saved.roster {
            for m in &r.members {
                if saved.locked && m.role != TeamRole::Owner {
                    continue;
                }
                let who = match m.role {
                    TeamRole::Owner => Principal::Owner,
                    TeamRole::Editor | TeamRole::Viewer => {
                        let role = if m.role == TeamRole::Editor { Role::Editor } else { Role::Viewer };
                        team_roles.insert(format!("account:{}", m.account), role);
                        let name = saved.team_names.get(&m.account).unwrap_or(&m.name).clone();
                        Principal::User { id: format!("account:{}", m.account), name, pic: None }
                    }
                };
                for c in r.devices(&m.account, &saved.team_certs).devices.into_values() {
                    if c.kind.connects() {
                        others.push((c, who.clone()));
                    }
                }
            }
        }
        // People sessions were shared with, from the roots their grants pin.
        for g in acl.list() {
            let (Some(account), Some(root)) = (g.principal.strip_prefix("account:"), g.root.as_ref()) else { continue };
            let Some(p) = saved.peers.get(account) else { continue };
            let t = Trust { account: account.to_owned(), root: root.clone() }.evaluate(&p.certs, &p.revocations);
            let name = if p.name.is_empty() { g.name.clone() } else { p.name.clone() };
            for c in t.devices.into_values().filter(|c| c.kind.connects()) {
                others.push((c, Principal::User { id: g.principal.clone(), name: name.clone(), pic: None }));
            }
        }
        // Teams sessions were shared with (M30): their members, as people.
        let mut shared_roles: HashMap<String, HashMap<String, Role>> = HashMap::new();
        for (team, t) in &saved.shared_teams {
            if !acl.list().iter().any(|g| g.principal == format!("team:{team}")) {
                continue;
            }
            let roles = shared_roles.entry(team.clone()).or_default();
            for m in &t.roster.members {
                // This account's own devices are the owner already.
                if m.account == saved.cert.account || (t.locked && m.role != TeamRole::Owner) {
                    continue;
                }
                let id = format!("account:{}", m.account);
                roles.insert(id.clone(), if m.role == TeamRole::Viewer { Role::Viewer } else { Role::Editor });
                let who = Principal::User { id, name: t.names.get(&m.account).unwrap_or(&m.name).clone(), pic: None };
                for c in t.roster.devices(&m.account, &t.certs).devices.into_values().filter(|c| c.kind.connects()) {
                    others.push((c, who.clone()));
                }
            }
        }
        Self { saved, keys, trusted, others, team_roles, shared_roles, push: Vec::new() }
    }

    /// A team's other owner (#386): an owner here as this machine's own
    /// account is, yet still someone to name, as `account:<id>`.
    fn co_owner(&self, account: &str) -> Option<Principal> {
        if account == self.saved.cert.account {
            return None;
        }
        let m = self.saved.roster.as_ref()?.member(account).filter(|m| m.role == TeamRole::Owner)?;
        let name = self.saved.team_names.get(account).unwrap_or(&m.name).clone();
        Some(Principal::User { id: format!("account:{account}"), name, pic: None })
    }

    /// Every account outside this one that gets in, for control to route.
    fn accounts(&self) -> Vec<String> {
        let mut a: Vec<String> = self.others.iter().map(|(c, _)| c.account.clone()).collect();
        a.sort();
        a.dedup();
        a
    }
}

impl std::fmt::Debug for Control {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Control").field("enrolled", &self.enrolled().is_some()).finish()
    }
}

/// This daemon's standing with control, shared with the channel handlers.
pub struct Control {
    state_dir: PathBuf,
    acl: Arc<Acl>,
    now: RwLock<Option<Arc<Enrolled>>>,
    /// Bumped whenever the trusted set changes: open channels re-check
    /// their device and close if it's gone.
    pub changed: watch::Sender<u64>,
    /// Direct URLs to give the directory.
    pub direct_urls: Vec<String>,
    /// The control to join when nobody names one (`--control`, #207).
    pub default_url: String,
    /// Control said the account's devices changed: refresh now.
    nudge: tokio::sync::Notify,
    /// Refreshes started, counting from 1 (#232).
    started: std::sync::atomic::AtomicU64,
    /// The number of the last refresh that went through, set as it ends
    /// and nowhere else: [`Control::refresh_now`] waits on it.
    refreshed: watch::Sender<u64>,
    http: reqwest::Client,
    /// What control was last told about access.
    published: std::sync::Mutex<Option<serde_json::Value>>,
    /// Reached through a provider's proxy (a hosted sandbox, M20): no relay
    /// socket, so certificates are fetched more often instead of nudged.
    pub no_relay: bool,
    /// Control takes v2 request signatures (see [`auth_header`]).
    auth_v2: std::sync::atomic::AtomicBool,
    /// TURN credentials for huddles (M63), and when they were fetched.
    turn: tokio::sync::Mutex<Option<(std::time::Instant, serde_json::Value)>>,
    /// Teams the owner's browser pinned (#233), by id: `<founder
    /// device>.<founder's root>`, in `team-pins.json`. Their rosters are
    /// fetched and checked as a shared team's, so their members can be
    /// named; they let no one in.
    team_pins: RwLock<BTreeMap<String, String>>,
    /// Invites waiting for their person to be reachable (#233), in
    /// `invites.json`: tried again after each refresh, for a day.
    invites: std::sync::Mutex<Vec<Waiting>>,
    /// Held while they're tried: one try at a time, beside the refreshes.
    retrying: tokio::sync::Mutex<()>,
}

/// An invite pushed to nobody yet (#233): its person hasn't accepted the
/// share, or control didn't answer in time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Waiting {
    /// Whom, by principal id.
    pub who: String,
    /// Into which session: once they can't read it (revoked), it's not
    /// sent. (`None`: kept from before this was.)
    #[serde(default)]
    pub session: Option<illogical_core::SessionId>,
    pub pane: u32,
    pub title: String,
    pub body: String,
    pub extra: serde_json::Value,
    /// When it was made (ms); a day later it stops waiting.
    pub at: u64,
}

/// How long an invite waits for its person (#233).
const INVITE_WAIT_MS: u64 = 24 * 3600 * 1000;

/// Someone a roster this daemon checked names (#233): its own team's, or
/// a team's the owner pinned or shared with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Known {
    pub account: String,
    pub root: String,
    pub name: String,
    pub role: TeamRole,
}

fn read_saved(dir: &Path) -> anyhow::Result<Option<Saved>> {
    match std::fs::read(dir.join(FILE)) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).context("control.json")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_saved(dir: &Path, s: &Saved) -> anyhow::Result<()> {
    crate::store::write_atomic(&dir.join(FILE), &serde_json::to_vec_pretty(s)?)?;
    Ok(())
}

/// A request signature for control: see control's `auth.rs`. It covers the
/// method, path and query, the time, a fresh nonce and the body's hash
/// (`v2`), or for a control from before 0.17 the method, path and time.
pub fn auth_header(keys: &DeviceKeys, method: &str, path_and_query: &str, body: &[u8], v2: bool) -> String {
    if !v2 {
        let ms = now_ms();
        let path = path_and_query.split('?').next().unwrap_or(path_and_query);
        let msg = format!("illogical daemon auth\n{method}\n{path}\n{ms}\n");
        return format!("{} {ms} {}", keys.id(), hex::encode(keys.signature(msg.as_bytes())));
    }
    illogical_e2e::cert::request_auth(keys, method, path_and_query, body)
}

/// Whether the control at `url` takes signatures over the body and a
/// nonce (it says `daemon_auth: 2` in `control.json`).
async fn takes_v2(http: &reqwest::Client, url: &str) -> bool {
    let Ok(r) = http.get(format!("{url}/control.json")).send().await else { return false };
    let v: serde_json::Value = r.json().await.unwrap_or_default();
    v["daemon_auth"].as_u64().is_some_and(|n| n >= 2)
}

const AUTH: &str = "x-illogical-auth";

impl Control {
    pub fn new(
        state_dir: &Path,
        direct_urls: Vec<String>,
        default_url: String,
        acl: Arc<Acl>,
        no_relay: bool,
    ) -> Arc<Self> {
        let me = Arc::new(Self {
            state_dir: state_dir.to_owned(),
            acl,
            now: RwLock::new(None),
            changed: watch::channel(0).0,
            direct_urls,
            default_url,
            nudge: tokio::sync::Notify::new(),
            started: Default::default(),
            refreshed: watch::channel(0).0,
            http: crate::roots::http().timeout(Duration::from_secs(20)).build().expect("http client"),
            published: Default::default(),
            no_relay,
            auth_v2: Default::default(),
            turn: Default::default(),
            team_pins: RwLock::new(
                std::fs::read(state_dir.join(PINS_FILE))
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default(),
            ),
            invites: std::sync::Mutex::new(
                std::fs::read(state_dir.join(INVITES_FILE))
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default(),
            ),
            retrying: Default::default(),
        });
        me.reload();
        me
    }

    /// Where `control.json` and the device key live.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn enrolled(&self) -> Option<Arc<Enrolled>> {
        self.now.read().unwrap().clone()
    }

    /// Control's ssh jump host for guests (M65), from its `/control.json`;
    /// `None` when it runs none.
    pub async fn guest_jump(&self) -> anyhow::Result<Option<crate::guest_ssh::Jump>> {
        let Some(e) = self.enrolled() else { return Ok(None) };
        let v: serde_json::Value = self
            .http
            .get(format!("{}/control.json", e.saved.url.trim_end_matches('/')))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(match &v["guest_ssh"] {
            serde_json::Value::Null => None,
            j => Some(serde_json::from_value(j.clone())?),
        })
    }

    /// Look again soon (grants changed here, say).
    pub fn poke(&self) {
        self.nudge.notify_one();
    }

    /// Look again now and wait for it (#232): true once a refresh that
    /// started after this call has gone through (so it saw what changed
    /// before it: a grant, say), false if none has within `timeout`. One
    /// already under way when called doesn't count: it may have read the
    /// grants before the change.
    pub async fn refresh_now(&self, timeout: Duration) -> bool {
        let mut done = self.refreshed.subscribe();
        let before = self.started.load(std::sync::atomic::Ordering::SeqCst);
        self.poke();
        matches!(tokio::time::timeout(timeout, done.wait_for(|n| *n > before)).await, Ok(Ok(_)))
    }

    /// What to call an account that's an owner here (M30): this daemon's
    /// own account's login, or a team box's owner as the roster names them.
    pub fn name_of_account(&self, account: &str) -> Option<String> {
        let e = self.enrolled()?;
        if account == e.saved.cert.account {
            return Some(e.saved.login.clone()).filter(|l| !l.is_empty());
        }
        let m = e.saved.roster.as_ref()?.member(account)?;
        Some(e.saved.team_names.get(account).unwrap_or(&m.name).clone())
    }

    /// Teams the owner's browser pinned (#233).
    pub fn team_pins(&self) -> BTreeMap<String, String> {
        self.team_pins.read().unwrap().clone()
    }

    /// Pin more (a newer pin of the same team replaces it) and unpin
    /// teams left (`drop`); true if any changed. Only the owner's browser
    /// sends these: control never does.
    pub fn set_team_pins(&self, pins: BTreeMap<String, String>, drop: &[String]) -> std::io::Result<bool> {
        let mut p = self.team_pins.write().unwrap();
        let before = p.clone();
        p.extend(pins);
        p.retain(|team, _| !drop.contains(team));
        if *p == before {
            return Ok(false);
        }
        crate::store::write_atomic(&self.state_dir.join(PINS_FILE), &serde_json::to_vec_pretty(&*p)?)?;
        Ok(true)
    }

    /// Teams whose roster checked out from its pin, by id.
    pub fn checked_teams(&self) -> Vec<String> {
        self.enrolled().map(|e| e.saved.shared_teams.keys().cloned().collect()).unwrap_or_default()
    }

    /// Everyone the rosters this daemon checked name (#233): its own
    /// team's (a team daemon) and those of teams pinned or shared with;
    /// not this account.
    pub fn known(&self) -> Vec<Known> {
        let Some(e) = self.enrolled() else { return Vec::new() };
        let rosters = e.saved.roster.iter().chain(e.saved.shared_teams.values().map(|t| &t.roster));
        let mut out: Vec<Known> = Vec::new();
        for m in rosters.flat_map(|r| &r.members) {
            if m.account == e.saved.cert.account || out.iter().any(|k| k.account == m.account) {
                continue;
            }
            out.push(Known { account: m.account.clone(), root: m.root.clone(), name: m.name.clone(), role: m.role });
        }
        out
    }

    /// Whether this daemon's own team (a team daemon) has `account` as an
    /// owner: an owner here already.
    pub fn owns_here(&self, account: &str) -> bool {
        self.enrolled().is_some_and(|e| {
            e.saved.roster.as_ref().and_then(|r| r.member(account)).is_some_and(|m| m.role == TeamRole::Owner)
        })
    }

    /// Whether `account` is this machine's own.
    pub fn is_me(&self, account: &str) -> bool {
        self.enrolled().is_some_and(|e| e.saved.cert.account == account)
    }

    /// Whether this daemon is a team's (M19).
    pub fn is_team(&self) -> bool {
        self.enrolled().is_some_and(|e| e.saved.team.is_some())
    }

    /// Whether someone (by principal id) has devices this daemon lets in
    /// through control: control routes them here.
    pub fn reaches(&self, id: &str) -> bool {
        self.enrolled().is_some_and(|e| {
            e.others
                .iter()
                .any(|(c, p)| p.id() == id || (p.is_owner() && id.strip_prefix("account:") == Some(&c.account)))
        })
    }

    /// Keep an invite for later (#233): pushed after a refresh that finds
    /// its person reachable, or dropped after a day. A newer invite of
    /// theirs into the same session replaces one waiting: one push.
    pub fn wait_invite(&self, w: Waiting) {
        let mut l = self.invites.lock().unwrap();
        l.retain(|o| !(o.who == w.who && o.session == w.session));
        l.push(w);
        self.save_invites(&l);
    }

    fn save_invites(&self, l: &[Waiting]) {
        let r = serde_json::to_vec_pretty(l)
            .map_err(std::io::Error::other)
            .and_then(|b| crate::store::write_atomic(&self.state_dir.join(INVITES_FILE), &b));
        if let Err(e) = r {
            warn!(error = %e, "can't save waiting invites");
        }
    }

    /// Try the waiting invites again: each goes once a subscription took
    /// it, or after a day stops waiting, or once its share is revoked.
    pub async fn retry_invites(&self) {
        let Ok(_one) = self.retrying.try_lock() else { return };
        let waiting = self.invites.lock().unwrap().clone();
        if waiting.is_empty() {
            return;
        }
        let mut done = Vec::new();
        for w in &waiting {
            if w.at + INVITE_WAIT_MS <= now_ms() {
                info!(who = w.who, "an invite waited a day; it's unreachable");
                done.push(w.clone());
                continue;
            }
            if w.session.is_some_and(|s| self.acl.role_of(&w.who, s).is_none()) {
                info!(who = w.who, "a waiting invite's share was revoked; it's not sent");
                done.push(w.clone());
                continue;
            }
            let got = self.push_report(w.pane, &w.title, &w.body, Some(w.extra.clone()), |p| p.id() == w.who).await;
            if got.relayed > 0 {
                info!(who = w.who, "a waiting invite went out");
                done.push(w.clone());
            }
        }
        let mut l = self.invites.lock().unwrap();
        l.retain(|w| !done.contains(w));
        self.save_invites(&l);
    }

    /// People who have a team role here, by the roster (this daemon's team,
    /// and teams sessions were shared with), connected or not.
    pub fn team_people(&self) -> Vec<Principal> {
        let Some(e) = self.enrolled() else { return Vec::new() };
        let mut out = Vec::new();
        if let Some(r) = &e.saved.roster {
            for m in &r.members {
                let id = format!("account:{}", m.account);
                if e.team_roles.contains_key(&id) {
                    let name = e.saved.team_names.get(&m.account).unwrap_or(&m.name).clone();
                    out.push(Principal::User { id, name, pic: None });
                }
            }
        }
        for (team, t) in &e.saved.shared_teams {
            for m in &t.roster.members {
                let id = format!("account:{}", m.account);
                if e.shared_roles.get(team).is_some_and(|r| r.contains_key(&id)) {
                    let name = t.names.get(&m.account).unwrap_or(&m.name).clone();
                    out.push(Principal::User { id, name, pic: None });
                }
            }
        }
        out
    }

    /// The team's other owners (#386), by account: they get in as the
    /// owner, but each is someone to @mention.
    pub fn co_owners(&self) -> Vec<Principal> {
        let Some(e) = self.enrolled() else { return Vec::new() };
        let Some(r) = &e.saved.roster else { return Vec::new() };
        r.members.iter().filter_map(|m| e.co_owner(&m.account)).collect()
    }

    /// Who a Noise key belongs to, if this daemon lets them in: a device of
    /// its own account (the owner), a team member's, or someone's a session
    /// was shared with.
    pub fn device(&self, noise: &[u8]) -> Option<(Cert, Principal)> {
        let e = self.enrolled()?;
        if let Some(c) = e.trusted.by_noise(noise).filter(|c| c.kind.connects()) {
            return Some((c.clone(), Principal::Owner));
        }
        let key = hex::encode(noise);
        if let Some(found) = e.others.iter().find(|(c, _)| c.noise == key) {
            return Some(found.clone());
        }
        // A read-only link's key (M19): a viewer of one session, while it lasts.
        let g = self.acl.link_by_key(&key)?;
        let cert = Cert {
            v: 1,
            account: String::new(),
            device: g.principal.clone(),
            kind: Kind::Browser,
            name: g.name.clone(),
            noise: key,
            sign: String::new(),
            created: g.at,
            approver: String::new(),
            sig: String::new(),
        };
        Some((cert, Principal::User { id: g.principal, name: g.name, pic: None }))
    }

    fn install(&self, e: Option<Enrolled>) {
        // Control's page shows this daemon's blocks too: let it frame them.
        crate::sites::set_control_origin(
            e.as_ref().and_then(|e| reqwest::Url::parse(&e.saved.url).ok().map(|u| u.origin().ascii_serialization())),
        );
        self.acl.set_team_roles(e.as_ref().map(|e| e.team_roles.clone()).unwrap_or_default());
        self.acl.set_shared_teams(e.as_ref().map(|e| e.shared_roles.clone()).unwrap_or_default());
        *self.now.write().unwrap() = e.map(Arc::new);
        self.changed.send_modify(|v| *v += 1);
    }

    /// Read `control.json` (and the key) again.
    fn reload(&self) {
        let next = match read_saved(&self.state_dir).and_then(|s| {
            let Some(saved) = s else { return Ok(None) };
            let keys = DeviceKeys::load(&self.state_dir.join(KEY_FILE))?;
            Ok(Some(Enrolled::build(saved, Arc::new(keys), &self.acl)))
        }) {
            Ok(n) => n,
            Err(e) => {
                warn!(error = %e, "can't read the control enrollment; ignoring it");
                None
            }
        };
        if let Some(e) = &next {
            info!(
                control = e.saved.url,
                account = e.saved.trust.account,
                devices = e.trusted.devices.len(),
                others = e.others.len(),
                "enrolled in control"
            );
        }
        self.install(next);
    }

    /// A signature for a request to control (see [`auth_header`]).
    fn sign(&self, e: &Enrolled, method: &str, path_and_query: &str, body: &[u8]) -> String {
        let v2 = self.auth_v2.load(std::sync::atomic::Ordering::Relaxed);
        auth_header(&e.keys, method, path_and_query, body, v2)
    }

    /// Ask control how it takes signatures, until it says v2.
    async fn check_auth(&self, e: &Enrolled) {
        if !self.auth_v2.load(std::sync::atomic::Ordering::Relaxed) && takes_v2(&self.http, &e.saved.url).await {
            self.auth_v2.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// POST JSON to control, signed over exactly the bytes sent.
    fn post_json(&self, e: &Enrolled, path: &str, body: &serde_json::Value) -> reqwest::RequestBuilder {
        let bytes = serde_json::to_vec(body).unwrap_or_default();
        self.http
            .post(format!("{}{path}", e.saved.url))
            .header(AUTH, self.sign(e, "POST", path, &bytes))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, e: &Enrolled, path_and_query: &str) -> anyhow::Result<T> {
        let res = self
            .http
            .get(format!("{}{path_and_query}", e.saved.url))
            .header(AUTH, self.sign(e, "GET", path_and_query, b""))
            .send()
            .await?;
        if res.status() == reqwest::StatusCode::GONE {
            return Err(removed_answer(res).await.into());
        }
        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!(
                "control doesn't know this daemon any more ({}); run `illogicald join` again",
                control_said(res).await
            );
        }
        if !res.status().is_success() {
            bail!("{}", control_said(res).await);
        }
        Ok(res.json().await?)
    }

    /// Fetch certificates (the account's, the team's, people's shared
    /// with); keep what checks out against what this daemon pinned.
    async fn refresh(&self) -> anyhow::Result<bool> {
        let Some(e) = self.enrolled() else { return Ok(false) };
        let number = self.started.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.check_auth(&e).await;
        #[derive(Deserialize)]
        struct Own {
            certs: Vec<Cert>,
            revocations: Vec<Revocation>,
            #[serde(default)]
            moved: Option<Move>,
        }
        let own: Own = self.get(&e, &format!("/api/daemon/trust?features={}", features())).await?;
        let mut saved = e.saved.clone();
        saved.certs = own.certs;
        saved.revocations = own.revocations;
        if let Some(m) = own.moved {
            take_move(&mut saved, m);
        }

        if let Some(pin) = saved.team.clone() {
            #[derive(Deserialize)]
            struct TeamNow {
                locked: bool,
                rosters: Vec<Roster>,
                certs: AccountCerts,
                #[serde(default)]
                names: BTreeMap<String, String>,
            }
            let since = saved.roster.as_ref().map_or(0, |r| r.version);
            let t: TeamNow = self.get(&e, &format!("/api/daemon/team?since={since}&features={}", features())).await?;
            let mut certs = saved.team_certs.clone();
            certs.extend(t.certs);
            let mut cur = saved.roster.clone();
            for r in t.rosters {
                if r.follows(cur.as_ref(), &pin, &certs) {
                    cur = Some(r);
                } else {
                    warn!(version = r.version, "a team roster from control doesn't check out; ignoring it");
                    break;
                }
            }
            // Keep only the current members' certificates.
            if let Some(r) = &cur {
                certs.retain(|a, _| r.member(a).is_some());
            }
            let mut names = t.names;
            names.retain(|a, _| cur.as_ref().is_some_and(|r| r.member(a).is_some()));
            saved.roster = cur;
            saved.team_certs = certs;
            saved.team_names = names;
            saved.locked = t.locked;
        }

        let accounts: Vec<String> =
            self.acl.list().iter().filter_map(|g| g.principal.strip_prefix("account:").map(str::to_owned)).collect();
        // The account's own login comes with them (M30): what to call it.
        let own = saved.cert.account.clone();
        let mut peers: BTreeMap<String, PeerCerts> = self
            .get(
                &e,
                &format!(
                    "/api/daemon/peers?accounts={}",
                    [own.clone()].iter().chain(&accounts).cloned().collect::<Vec<_>>().join(",")
                ),
            )
            .await?;
        saved.login = peers.get(&own).map(|p| p.name.clone()).unwrap_or_default();
        peers.retain(|a, _| accounts.contains(a));
        saved.peers = peers;

        // Teams sessions were shared with (M30): each roster checked from
        // the founder the grant pinned. And those the owner's browser
        // pinned (#233), checked the same way, for naming their members.
        let mut pins: BTreeMap<String, TeamPin> = self
            .acl
            .list()
            .iter()
            .filter_map(|g| {
                let team = g.principal.strip_prefix("team:")?;
                Some((team.to_owned(), team_pin(team, g.root.as_deref()?)?))
            })
            .collect();
        for (team, root) in self.team_pins() {
            if let Some(p) = team_pin(&team, &root) {
                pins.entry(team).or_insert(p);
            }
        }
        saved.shared_teams = BTreeMap::new();
        if !pins.is_empty() {
            #[derive(Deserialize)]
            struct Got {
                locked: bool,
                rosters: Vec<Roster>,
                certs: AccountCerts,
                #[serde(default)]
                names: BTreeMap<String, String>,
            }
            let ids: Vec<&str> = pins.keys().map(String::as_str).collect();
            let got: BTreeMap<String, Got> =
                self.get(&e, &format!("/api/daemon/teams?ids={}&features={}", ids.join(","), features())).await?;
            for (team, g) in got {
                let Some(pin) = pins.get(&team) else { continue };
                let mut cur: Option<Roster> = None;
                for r in g.rosters {
                    if r.follows(cur.as_ref(), pin, &g.certs) {
                        cur = Some(r);
                    } else {
                        warn!(team, version = r.version, "a shared team's roster doesn't check out; stopping there");
                        break;
                    }
                }
                if let Some(roster) = cur {
                    let mut certs = g.certs;
                    certs.retain(|a, _| roster.member(a).is_some());
                    let mut names = g.names;
                    names.retain(|a, _| roster.member(a).is_some());
                    saved.shared_teams.insert(team, SharedTeam { roster, certs, names, locked: g.locked });
                }
            }
        }

        let changed = saved.shared_teams != e.saved.shared_teams
            || saved.login != e.saved.login
            || (saved.team.clone(), saved.moved_at) != (e.saved.team.clone(), e.saved.moved_at)
            || (
                saved.certs.clone(),
                saved.revocations.clone(),
                saved.roster.clone(),
                saved.locked,
                saved.peers.clone(),
            ) != (
                e.saved.certs.clone(),
                e.saved.revocations.clone(),
                e.saved.roster.clone(),
                e.saved.locked,
                e.saved.peers.clone(),
            )
            || saved.team_certs != e.saved.team_certs
            || saved.team_names != e.saved.team_names;
        if changed {
            write_saved(&self.state_dir, &saved)?;
        }
        let mut next = Enrolled::build(saved, e.keys.clone(), &self.acl);
        if next.trusted.get(&next.saved.cert.device).is_none() {
            warn!("this daemon's own certificate no longer checks out (revoked?)");
        }
        // Who gets in first: control hands over only the subscriptions of
        // people it knows this daemon serves (#232), so someone just
        // granted is reachable after this refresh, not the next.
        self.publish(&next).await;
        next.push = self.push_subs(&next).await;
        info!(devices = next.trusted.devices.len(), others = next.others.len(), changed, "certificates refreshed");
        self.install(Some(next));
        self.refreshed.send_modify(|n| *n = (*n).max(number));
        Ok(changed)
    }

    /// Notify people through control (M21): every verified subscription
    /// `to` accepts, encrypted here for that subscription alone. In the
    /// background; [`Control::push_report`] says how it went.
    pub fn push(
        self: &Arc<Self>,
        pane: u32,
        title: &str,
        body: &str,
        extra: Option<serde_json::Value>,
        to: impl Fn(&Principal) -> bool,
    ) {
        let Some((e, subs, payload)) = self.to_push(pane, title, body, extra, to) else { return };
        let me = self.clone();
        tokio::spawn(async move { me.relay_push(&e, subs, &payload).await });
    }

    /// [`Control::push`], waited for (#232): how many subscriptions `to`
    /// matched, how many control took, and how many it refused (someone
    /// it doesn't route to this daemon). `extra.tag` names the
    /// notification (`invite-7`) in place of the pane's.
    pub async fn push_report(
        &self,
        pane: u32,
        title: &str,
        body: &str,
        extra: Option<serde_json::Value>,
        to: impl Fn(&Principal) -> bool,
    ) -> Pushed {
        let Some((e, subs, payload)) = self.to_push(pane, title, body, extra, to) else { return Pushed::default() };
        self.relay_push(&e, subs, &payload).await
    }

    /// The subscriptions `to` picks, and what to tell them.
    fn to_push(
        &self,
        pane: u32,
        title: &str,
        body: &str,
        extra: Option<serde_json::Value>,
        to: impl Fn(&Principal) -> bool,
    ) -> Option<(Arc<Enrolled>, Vec<PushSub>, String)> {
        let e = self.enrolled()?;
        // A team's other owner's devices are the owner's, and theirs by
        // account too (#386): `account:<id>` picks them alone.
        let picks =
            |(p, s): &&(Principal, PushSub)| to(p) || (p.is_owner() && e.co_owner(&s.account).is_some_and(|c| to(&c)));
        let subs: Vec<PushSub> = e.push.iter().filter(picks).map(|(_, s)| s.clone()).collect();
        if subs.is_empty() {
            return None;
        }
        let mut payload = crate::push::payload(pane, title, body, extra);
        payload.entry("daemon").or_insert_with(|| e.saved.cert.device.clone().into());
        let payload = serde_json::Value::Object(payload).to_string();
        Some((e, subs, payload))
    }

    /// Notify one device (S33: wake a hand). False when control has no
    /// subscription from it that we trust.
    pub fn push_device(self: &Arc<Self>, device: &str, payload: serde_json::Value) -> bool {
        let Some(e) = self.enrolled() else { return false };
        let subs: Vec<PushSub> = e.push.iter().filter(|(_, s)| s.device == device).map(|(_, s)| s.clone()).collect();
        if subs.is_empty() {
            return false;
        }
        let mut payload = payload;
        payload["daemon"] = e.saved.cert.device.clone().into();
        let payload = payload.to_string();
        let me = self.clone();
        tokio::spawn(async move { me.relay_push(&e, subs, &payload).await });
        true
    }

    async fn relay_push(&self, e: &Enrolled, subs: Vec<PushSub>, payload: &str) -> Pushed {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mut got = Pushed { matched: subs.len(), ..Default::default() };
        for s in subs {
            let (Ok(ua), Ok(auth)) = (b64.decode(&s.p256dh), b64.decode(&s.auth)) else { continue };
            let Ok(body) = crate::push::encrypt(
                payload.as_bytes(),
                &ua,
                &auth,
                &crate::push::new_secret(),
                &crate::push::random::<16>(),
            ) else {
                continue;
            };
            let req = serde_json::json!({ "endpoint": s.endpoint, "body": base64::engine::general_purpose::STANDARD.encode(body) });
            match self.post_json(e, "/api/daemon/push", &req).send().await {
                Ok(r) if r.status().is_success() => got.relayed += 1,
                Ok(r) if r.status() == reqwest::StatusCode::FORBIDDEN => got.refused += 1,
                Ok(r) => warn!(status = %r.status(), "control didn't relay a push"),
                Err(err) => warn!(error = %err, "can't push through control"),
            }
        }
        got
    }

    /// M40: a read-only installation token for a GitHub repository from
    /// control's GitHub App (for a box with no `gh` login), and the
    /// account's GitHub login. Never logged.
    pub async fn github_token(&self, repo: &str) -> Result<serde_json::Value, String> {
        let e = self.enrolled().ok_or("not joined to illogical control")?;
        let res = self
            .post_json(&e, "/api/daemon/github/token", &serde_json::json!({ "repo": repo }))
            .send()
            .await
            .map_err(|e| format!("can't reach control: {e}"))?;
        let status = res.status();
        let v: serde_json::Value = res.json().await.unwrap_or_default();
        if !status.is_success() {
            return Err(v["error"].as_str().map_or_else(|| format!("control said {status}"), str::to_owned));
        }
        Ok(v)
    }

    /// ICE servers for a huddle (M63): control's TURN credentials, kept
    /// for an hour of their eight; public STUN when this daemon isn't
    /// joined to control or control can't be reached.
    pub async fn ice_servers(&self) -> serde_json::Value {
        let stun =
            || serde_json::json!({ "ice_servers": [{ "urls": ["stun:stun.cloudflare.com:3478"] }], "turn": false });
        let Some(e) = self.enrolled() else { return stun() };
        let mut cached = self.turn.lock().await;
        if let Some((at, v)) = &*cached
            && at.elapsed() < Duration::from_secs(3600)
        {
            return v.clone();
        }
        match self.get::<serde_json::Value>(&e, "/api/daemon/turn").await {
            Ok(v) => {
                *cached = Some((std::time::Instant::now(), v.clone()));
                v
            }
            Err(err) => {
                warn!(error = %err, "no TURN credentials from control: STUN only");
                stun()
            }
        }
    }

    /// A hosted sandbox's last session closed (M20): control deletes it.
    pub fn sandbox_done(self: &Arc<Self>) {
        let Some(e) = self.enrolled() else { return };
        let me = self.clone();
        tokio::spawn(async move {
            let path = "/api/daemon/sandbox-done";
            info!("last session closed: asking control to delete this sandbox");
            let r = me
                .http
                .post(format!("{}{path}", e.saved.url))
                .header(AUTH, me.sign(&e, "POST", path, b""))
                .send()
                .await;
            if let Err(err) = r {
                warn!(error = %err, "can't tell control this sandbox is done");
            }
        });
    }

    /// Subscriptions control has for the people this daemon serves, kept if
    /// a device we trust signed them.
    async fn push_subs(&self, e: &Enrolled) -> Vec<(Principal, PushSub)> {
        #[derive(Deserialize)]
        struct Subs {
            subs: Vec<PushSub>,
        }
        let Ok(got) = self.get::<Subs>(e, "/api/daemon/push-subs").await else { return Vec::new() };
        got.subs
            .into_iter()
            .filter_map(|s| {
                if let Some(c) = e.trusted.get(&s.device) {
                    return s.signed_by(c).then_some((Principal::Owner, s));
                }
                let (c, who) = e.others.iter().find(|(c, _)| c.device == s.device)?;
                s.signed_by(c).then(|| (who.clone(), s))
            })
            .collect()
    }

    /// Tell control which accounts get in (it routes them; we decide).
    async fn publish(&self, e: &Enrolled) {
        let links = self.acl.links_until();
        let body = serde_json::json!({ "accounts": e.accounts(), "links_until": links });
        if self.published.lock().unwrap().as_ref() == Some(&body) {
            return;
        }
        let res = self.post_json(e, "/api/daemon/access", &body).send().await;
        match res {
            Ok(r) if r.status().is_success() => *self.published.lock().unwrap() = Some(body),
            Ok(r) => warn!(status = %r.status(), "control refused the access list"),
            Err(err) => warn!(error = %err, "can't tell control who gets in"),
        }
    }

    /// Run for good: notice joins and leaves, keep certificates fresh, and
    /// keep the relay socket up while enrolled.
    pub fn start(self: &Arc<Self>, app: Arc<App>) {
        let (a, r) = (app.clone(), app.clone());
        self.run(
            move || app.mux.send(crate::mux::Cmd::AclChanged),
            move |me| tokio::spawn(keep_relay(me, a.clone())),
            move |url| crate::setup::rejoin(r.clone(), url),
        );
    }

    /// [`Control::start`]'s loop: `acl_changed` after each reload and
    /// refresh, `relay_with` to keep the socket up, `rejoin` (control's
    /// URL) when control says this machine was removed (#330).
    fn run(
        self: &Arc<Self>,
        acl_changed: impl Fn() + Send + 'static,
        relay_with: impl Fn(Arc<Self>) -> tokio::task::JoinHandle<()> + Send + 'static,
        rejoin: impl Fn(String) + Send + 'static,
    ) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut stamp = file_stamp(&me.state_dir);
            let mut last_refresh = std::time::Instant::now() - REFRESH;
            let mut relay: Option<tokio::task::JoinHandle<()>> = None;
            loop {
                let now = file_stamp(&me.state_dir);
                if now != stamp {
                    stamp = now;
                    me.reload();
                    acl_changed();
                    if let Some(r) = relay.take() {
                        r.abort();
                    }
                    last_refresh = std::time::Instant::now() - REFRESH;
                }
                if me.enrolled().is_some() {
                    let every = if me.no_relay { Duration::from_secs(10) } else { REFRESH };
                    if last_refresh.elapsed() >= every {
                        last_refresh = std::time::Instant::now();
                        match me.refresh().await {
                            // Roles may have changed: re-filter everyone.
                            // Someone an invite waits for may be reachable.
                            // Beside the loop: a slow control doesn't hold up
                            // the next refresh.
                            Ok(_) => {
                                acl_changed();
                                let m = me.clone();
                                tokio::spawn(async move { m.retry_invites().await });
                            }
                            // Removed from a browser (#330): a new key
                            // asks to join the same control again.
                            Err(e) if e.is::<Removed>() => {
                                warn!(error = %e, "this machine was removed from its account; joining again with a new key");
                                if let Some(en) = me.enrolled() {
                                    rejoin(en.saved.url.clone());
                                }
                            }
                            Err(e) => warn!(error = %e, "can't refresh certificates from control"),
                        }
                        stamp = file_stamp(&me.state_dir);
                    }
                    if !me.no_relay && relay.as_ref().is_none_or(|r| r.is_finished()) {
                        relay = Some(relay_with(me.clone()));
                    }
                } else if let Some(r) = relay.take() {
                    r.abort();
                }
                tokio::select! {
                    _ = tokio::time::sleep(WATCH) => {}
                    _ = me.nudge.notified() => last_refresh = std::time::Instant::now() - REFRESH,
                }
            }
        });
    }
}

fn file_stamp(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir.join(FILE)).and_then(|m| m.modified()).ok()
}

/// The relay socket, redialled with backoff while enrolled.
async fn keep_relay(control: Arc<Control>, app: Arc<App>) {
    let (accept, mut streams) = mpsc::unbounded_channel();
    let a = app.clone();
    tokio::spawn(async move {
        while let Some(s) = streams.recv().await {
            tokio::spawn(crate::e2e::serve_stream(a.clone(), s));
        }
    });
    // Raw streams: a guest's ssh connection through control's jump host
    // (M65), which ends here.
    let (raw, mut raws) = mpsc::unbounded_channel::<(Vec<u8>, tokio::io::DuplexStream)>();
    let a = app.clone();
    tokio::spawn(async move {
        while let Some((kind, s)) = raws.recv().await {
            if kind == crate::guest_ssh::STREAM_KIND {
                a.guests.serve_relayed(&a, s);
            }
        }
    });
    let mut backoff = Duration::from_secs(1);
    loop {
        let Some(e) = control.enrolled() else { return };
        let started = std::time::Instant::now();
        let mut wait = None;
        match relay_once(&control, &app, &e, &accept, &raw).await {
            Ok(()) => info!("relay socket closed"),
            Err(err) => {
                warn!(error = %err, "can't reach control's relay");
                wait = err.downcast_ref::<crate::dial::Busy>().map(|b| b.wait);
            }
        }
        // Control may have hung up because this machine was removed
        // (#330): find out now, not at the next refresh.
        control.poke();
        if started.elapsed() > Duration::from_secs(30) {
            backoff = Duration::from_secs(1);
        }
        let jitter = Duration::from_millis(u64::from(std::process::id() % 500));
        tokio::time::sleep(match wait {
            // A full relay (#344): wait as long as control asked, and up to
            // as long again, so its daemons don't all come back at once.
            Some(w) => w.max(backoff) + spread(w),
            None => backoff + jitter,
        })
        .await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// A random wait up to `w`.
fn spread(w: Duration) -> Duration {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    w.mul_f64(f64::from(u32::from_le_bytes(b)) / f64::from(u32::MAX))
}

async fn relay_once(
    control: &Control,
    app: &Arc<App>,
    e: &Enrolled,
    accept: &mpsc::UnboundedSender<tokio::io::DuplexStream>,
    raw: &mpsc::UnboundedSender<(Vec<u8>, tokio::io::DuplexStream)>,
) -> anyhow::Result<()> {
    let path = "/api/relay/dial";
    let mut url = reqwest::Url::parse(&e.saved.url)?.join(path)?;
    url.query_pairs_mut().append_pair("urls", &serde_json::to_string(&control.direct_urls)?);
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme).map_err(|()| anyhow::anyhow!("bad control URL"))?;
    control.check_auth(e).await;
    let signed = format!("{}?{}", url.path(), url.query().unwrap_or_default());
    let ws = crate::dial::open_ws(&url, &[(AUTH, &control.sign(e, "GET", &signed, b""))]).await?;
    info!(control = e.saved.url, "connected to control's relay");
    // M40: forge subscriptions out, pokes and heartbeats in. M65: guest
    // routes out, control's answers in.
    let guests = app.guests.clone();
    let on_text = move |t: &str| {
        if !guests.heard_from_control(t) {
            crate::forge::live::from_control(t);
        }
    };
    let mut out = vec![app.guests.routes_messages()];
    out.extend(crate::forge::live::watch_messages());
    let texts = crate::dial::Texts { on_text: &on_text, out };
    crate::dial::serve_mux(ws, accept, Some(raw), Some(&control.nudge), Some(texts)).await
}

// ---------------------------------------------------------------- join

#[derive(Deserialize)]
struct JoinStarted {
    code: String,
    poll: String,
    expires_in_secs: u64,
    /// The team `--team` named, by name.
    #[serde(default)]
    team_name: Option<String>,
}

#[derive(Deserialize)]
struct JoinPoll {
    approved: bool,
    /// Turned down, on this device (#100).
    #[serde(default)]
    rejected: Option<String>,
    cert: Option<Cert>,
    trust: Option<Trust>,
    #[serde(default)]
    team: Option<JoinTeam>,
    #[serde(default)]
    certs: Vec<Cert>,
    #[serde(default)]
    revocations: Vec<Revocation>,
}

#[derive(Deserialize)]
struct JoinTeam {
    team: String,
    founder: String,
    founder_root: String,
    #[serde(default)]
    name: String,
    /// The approving device's signature over [`TeamPin::join_body`].
    #[serde(default)]
    sig: Option<String>,
}

/// What control said, as a sentence: its `{"error": …}` if it sent one.
async fn control_said(res: reqwest::Response) -> String {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    match serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|v| v["error"].as_str().map(str::to_owned)) {
        Some(e) => format!("control says: {e}"),
        None => format!("control answered {status}"),
    }
}

/// Control said this machine's key was removed from its account (410
/// Gone, #330): someone removed it from a browser. A removed key never
/// counts again, so a new one joins; the old one is kept aside (see
/// [`retire`]). `GET /api/setup` shows this as `control.removed`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Removed {
    /// What control said, in words.
    pub said: String,
    /// When it was removed (ms since the epoch), and on which device, as
    /// control tells the key's holder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// The removed key's fingerprint, and where it's kept now.
    pub old_key: String,
    pub kept: String,
    /// The new key's fingerprint.
    pub new_key: String,
    /// The account it was in (its root device), to rejoin it without
    /// asking the person to check its fingerprint again.
    #[serde(skip)]
    pub root: Option<String>,
}

impl std::fmt::Display for Removed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "control says: {}", self.said)
    }
}

impl std::error::Error for Removed {}

/// Control's 410: the key was removed, and when and by whom if it says.
async fn removed_answer(res: reqwest::Response) -> Removed {
    let v: serde_json::Value = res.json().await.unwrap_or_default();
    Removed {
        said: v["error"].as_str().unwrap_or("this key was removed from its account").to_owned(),
        at: v["removed"]["at"].as_u64(),
        by: v["removed"]["by"].as_str().map(str::to_owned),
        ..Default::default()
    }
}

/// Set a removed key aside (`daemon.key.removed-<ms>`) with the
/// enrollment it had (`control.json.removed-<ms>`), never deleting them,
/// so the next join makes a new key.
fn retire(state_dir: &Path, r: &mut Removed) -> anyhow::Result<()> {
    let ms = now_ms();
    let key = state_dir.join(KEY_FILE);
    if let Ok(k) = DeviceKeys::load(&key) {
        r.old_key = fingerprint(&k.id());
    }
    if let Ok(Some(s)) = read_saved(state_dir) {
        r.root = Some(s.trust.root);
    }
    let kept = state_dir.join(format!("{KEY_FILE}.removed-{ms}"));
    std::fs::rename(&key, &kept).with_context(|| format!("setting {} aside", key.display()))?;
    if state_dir.join(FILE).exists() {
        std::fs::rename(state_dir.join(FILE), state_dir.join(format!("{FILE}.removed-{ms}")))?;
    }
    r.kept = kept.display().to_string();
    warn!(
        old_key = r.old_key,
        kept = r.kept,
        said = r.said,
        "this machine's key was removed; set it aside for a new one"
    );
    Ok(())
}

/// Why control refuses this machine's signature, if it does: what it said
/// ("this machine's account was deleted", or that it left or was
/// revoked). `None` when control still knows it, or can't be asked.
async fn forgotten(s: &Saved, keys: &DeviceKeys) -> Option<Forgot> {
    let http = crate::roots::http().timeout(Duration::from_secs(10)).build().ok()?;
    let v2 = takes_v2(&http, &s.url).await;
    let path = "/api/daemon/trust";
    let res =
        http.get(format!("{}{path}", s.url)).header(AUTH, auth_header(keys, "GET", path, b"", v2)).send().await.ok()?;
    if res.status() == reqwest::StatusCode::GONE {
        return Some(Forgot::Removed);
    }
    if res.status() != reqwest::StatusCode::UNAUTHORIZED {
        return None;
    }
    let said = control_said(res).await;
    let said = said.strip_prefix("control says: ").map(str::to_owned).unwrap_or(said);
    // Not a clock that's off or a replayed signature: control has no such machine.
    (said.contains("account was deleted") || said.contains("not an enrolled daemon")).then_some(Forgot::Said(said))
}

/// Why control doesn't know a saved enrollment any more.
enum Forgot {
    /// Its key was removed (#330): a new key joins again.
    Removed,
    /// What control said (its account was deleted, or it left).
    Said(String),
}

/// Who a saved enrollment belongs to, in words.
fn whose(s: &Saved) -> String {
    match (&s.roster, &s.team) {
        (Some(r), _) => format!("the team {}", r.name),
        (None, Some(t)) => format!("the team {}", t.team),
        _ if !s.login.is_empty() => format!("{}'s account", s.login),
        _ => "an account".into(),
    }
}

/// `<state>/join.lock`: one join at a time per machine (#329). The CLI's
/// `illogicald join` and Getting started's button (the running daemon)
/// would otherwise ask control for the same code, and the second would
/// take it over from the first.
pub const JOIN_LOCK: &str = "join.lock";

/// What `join.lock` says: who holds it, and the code once there is one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinLockInfo {
    pid: u32,
    /// "`illogicald join`" or "Getting started".
    pub by: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub approve: Option<String>,
    /// When it stops counting (ms since the epoch): the code's expiry, or
    /// a short while to ask for one.
    pub expires_ms: u64,
}

impl JoinLockInfo {
    /// Held by this process.
    pub fn mine(&self) -> bool {
        self.pid == std::process::id()
    }
}

/// A held `join.lock`, removed when dropped.
pub struct JoinLock {
    path: PathBuf,
    info: JoinLockInfo,
}

impl JoinLock {
    /// Take the lock, or say which join holds it. One whose process is
    /// gone (a CLI stopped with Ctrl-C) or whose time is up doesn't count.
    fn take(state_dir: &Path, by: &str) -> anyhow::Result<Self> {
        use std::io::Write;
        let path = state_dir.join(JOIN_LOCK);
        let info = JoinLockInfo {
            pid: std::process::id(),
            by: by.to_owned(),
            code: None,
            approve: None,
            expires_ms: now_ms() + 2 * 60 * 1000,
        };
        std::fs::create_dir_all(state_dir)?;
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    f.write_all(&serde_json::to_vec(&info)?)?;
                    return Ok(Self { path, info });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let held = Self::held(state_dir);
                    if let Some(h) = held {
                        let mins = h.expires_ms.saturating_sub(now_ms()).div_ceil(60_000);
                        match (&h.code, &h.approve) {
                            (Some(code), Some(at)) => bail!(
                                "a join is waiting on this machine already ({}, code {code}): approve it at {at}, or wait for it to end (in {mins} min)",
                                h.by
                            ),
                            _ => bail!("{} is asking control to add this machine; try again in a moment", h.by),
                        }
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(e.into()),
            }
        }
        bail!("can't take {}", path.display())
    }

    /// The join holding the lock, if one does.
    pub fn held(state_dir: &Path) -> Option<JoinLockInfo> {
        let b = std::fs::read(state_dir.join(JOIN_LOCK)).ok()?;
        let h: JoinLockInfo = serde_json::from_slice(&b).ok()?;
        (h.expires_ms > now_ms() && crate::procinfo::alive(h.pid)).then_some(h)
    }

    /// Say which code it's waiting on, for the other way in to point at.
    fn waiting(&mut self, code: &str, approve: &str, expires_ms: u64) {
        self.info.code = Some(code.to_owned());
        self.info.approve = Some(approve.to_owned());
        self.info.expires_ms = expires_ms;
        if let Ok(b) = serde_json::to_vec(&self.info) {
            let _ = crate::store::write_atomic(&self.path, &b);
        }
    }
}

impl Drop for JoinLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A join control has started: the code someone approves, and what
/// [`join_finish`] needs to wait for it.
pub struct JoinPending {
    pub url: String,
    pub code: String,
    pub expires_in_secs: u64,
    /// The team `--team` named, by name.
    pub team_name: Option<String>,
    /// Control said the old key was removed, so this join has a new one.
    pub renewed: Option<Removed>,
    poll: String,
    ask: Cert,
    http: reqwest::Client,
    /// Held until the join ends (#329).
    _lock: JoinLock,
}

impl JoinPending {
    /// Where a signed-in device approves it.
    pub fn approve_url(&self) -> String {
        format!("{}/#join={}", self.url, self.code)
    }
}

/// A finished join: where this machine went, and who approved it.
#[derive(Clone)]
pub struct Joined {
    /// "the team X" or "your account".
    pub place: String,
    pub approver: String,
    /// The team asked for, when the approver kept it to their account.
    pub not_team: Option<String>,
    pub key: String,
    /// The account's fingerprint: its root device's id, in groups.
    pub account: String,
}

/// An approval that checks out against the account control sent, not yet
/// saved: control picks that account, so the person first checks its
/// fingerprint against the device they approved on ([`Approved::save`]).
pub struct Approved {
    saved: Saved,
    pub joined: Joined,
}

impl Approved {
    /// Whether `typed` is this account's fingerprint (any case, with or
    /// without the dashes).
    pub fn is_account(&self, typed: &str) -> bool {
        same_fingerprint(&self.saved.trust.root, typed)
    }

    /// Pin the account and save `control.json` (a running daemon picks it
    /// up).
    pub fn save(self, state_dir: &Path) -> anyhow::Result<Joined> {
        write_saved(state_dir, &self.saved)?;
        Ok(self.joined)
    }

    /// Turned down here: ask control to drop the machine again (best
    /// effort), and start over with a new key, so a later join isn't held
    /// up by this approval.
    pub async fn refuse(self, state_dir: &Path) {
        let key = state_dir.join(KEY_FILE);
        if let Ok(keys) = DeviceKeys::load(&key) {
            let path = "/api/daemon/leave";
            let http = crate::roots::client();
            let v2 = takes_v2(&http, &self.saved.url).await;
            let _ = http
                .post(format!("{}{path}", self.saved.url))
                .header(AUTH, auth_header(&keys, "POST", path, b"", v2))
                .timeout(Duration::from_secs(10))
                .send()
                .await;
        }
        let _ = std::fs::remove_file(key);
    }
}

/// `typed` names the device `id`: its hex digits, ignoring case, spaces and
/// dashes.
pub fn same_fingerprint(id: &str, typed: &str) -> bool {
    let typed: String =
        typed.chars().filter(|c| !c.is_whitespace() && *c != '-').map(|c| c.to_ascii_lowercase()).collect();
    !id.is_empty() && typed == id
}

/// A fingerprint as `--account` takes it: 16 hex digits, dashes optional.
pub fn parse_fingerprint(typed: &str) -> anyhow::Result<String> {
    let id: String = typed.chars().filter(|c| *c != '-').map(|c| c.to_ascii_lowercase()).collect();
    if id.len() != 16 || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("an account fingerprint is 16 hex digits, like 1a2b-3c4d-5e6f-7a8b");
    }
    Ok(id)
}

/// Ask control to add this machine: the code to approve. The person
/// approving picks their account or a team they own; `team` picks one
/// ahead. A hosted sandbox (M20) joins with the `ticket` control gave it.
pub async fn join_start(
    url: &str,
    name: &str,
    team: Option<&str>,
    ticket: Option<&str>,
    state_dir: &Path,
    by: &str,
) -> anyhow::Result<JoinPending> {
    let url = url.trim_end_matches('/').to_owned();
    if !url.starts_with("https://") && !private_http(&url) {
        bail!("control's URL must be https:// (or http on loopback or a private network, for testing)");
    }
    let mut keys = DeviceKeys::load_or_create(&state_dir.join(KEY_FILE))?;
    if let Some(s) = read_saved(state_dir)? {
        // Control may have forgotten it (its account deleted, or it was
        // removed): say so, rather than that it's still in (#208). Removed
        // (#330), it asks below with the old key, to hear when and by whom.
        match forgotten(&s, &keys).await {
            Some(Forgot::Removed) => {}
            Some(Forgot::Said(why)) => bail!(
                "this machine was in {} on {}, but control doesn't know it any more ({why}); run `illogicald leave` to forget that here, then join again",
                whose(&s),
                s.url
            ),
            None => bail!(
                "this machine is already in {} on {}; to move it, run `illogicald leave`, then join again",
                whose(&s),
                s.url
            ),
        }
    }
    // One join at a time on this machine (#329).
    let mut lock = JoinLock::take(state_dir, by)?;
    let http = crate::roots::http().timeout(Duration::from_secs(20)).build()?;
    let mut renewed: Option<Removed> = None;
    let (res, ask) = loop {
        let ask = Cert { account: String::new(), ..Cert::new(&keys, "", Kind::Daemon, name) };
        // That this is the key's holder asking, not someone with its certificate.
        let ms = now_ms();
        let proof = serde_json::json!({
            "ms": ms,
            "sig": hex::encode(keys.signature(illogical_e2e::cert::join_proof_body(&ask, ms).as_bytes())),
        });
        let res = http
            .post(format!("{url}/api/join"))
            .json(&serde_json::json!({
                "cert": ask, "urls": [], "team": team, "ticket": ticket, "features": features(), "proof": proof,
            }))
            .send()
            .await
            .with_context(|| format!("can't reach control at {url}"))?;
        // This key was removed from its account (#330): it never counts
        // again, so set it aside and ask once more with a new one.
        if res.status() == reqwest::StatusCode::GONE && renewed.is_none() {
            let mut r = removed_answer(res).await;
            retire(state_dir, &mut r)?;
            keys = DeviceKeys::load_or_create(&state_dir.join(KEY_FILE))?;
            r.new_key = fingerprint(&keys.id());
            renewed = Some(r);
            continue;
        }
        break (res, ask);
    };
    if res.status() == reqwest::StatusCode::NOT_FOUND
        && let Some(t) = team
    {
        bail!("control has no team {t}; copy the command from the team's page (Teams, in the session menu)");
    }
    if !res.status().is_success() {
        bail!("{}", control_said(res).await);
    }
    let started: JoinStarted = res.json().await?;
    debug_assert_eq!(started.code, join_code(&ask));
    lock.waiting(&started.code, &format!("{url}/#join={}", started.code), now_ms() + started.expires_in_secs * 1000);
    Ok(JoinPending {
        url,
        code: started.code,
        expires_in_secs: started.expires_in_secs,
        team_name: started.team_name,
        renewed,
        poll: started.poll,
        ask,
        http,
        _lock: lock,
    })
}

/// Wait for someone to approve it and check the approval and the team the
/// approving device chose. Nothing is saved until the person confirms the
/// account ([`Approved::save`]).
pub async fn join_finish(p: JoinPending) -> anyhow::Result<Approved> {
    let JoinPending { url, code, expires_in_secs, team_name, poll, ask, http, _lock, .. } = p;
    let mins = expires_in_secs / 60;
    let deadline = std::time::Instant::now() + Duration::from_secs(expires_in_secs);
    let got = loop {
        if std::time::Instant::now() > deadline {
            bail!("nobody approved it in {mins} minutes; ask again for a new code");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let res = http.get(format!("{url}/api/join/{code}?poll={poll}")).send().await;
        let Ok(res) = res else { continue };
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("the code expired; ask again for a new one");
        }
        if !res.status().is_success() {
            bail!("{}", control_said(res).await);
        }
        let Ok(p) = res.json::<JoinPoll>().await else { continue };
        if let Some(on) = p.rejected {
            bail!("turned down on {on}; ask again to try again");
        }
        if p.approved {
            break p;
        }
    };
    let (cert, trust) = got.cert.zip(got.trust).context("control approved it but sent no certificate")?;
    if !cert.same_request(&ask) {
        bail!("control sent back a certificate for a different key; not joining");
    }
    let trusted = trust.evaluate(&got.certs, &got.revocations);
    let mut all = got.certs.clone();
    all.push(cert.clone());
    if trust.evaluate(&all, &got.revocations).get(&cert.device) != Some(&cert) {
        bail!("the approval doesn't check out against the account's devices; not joining");
    }
    let approver = trusted.get(&cert.approver);
    // A team is pinned only if the approving device chose it (#100):
    // control can't make a machine a team's on its own.
    let pin = match &got.team {
        Some(t) => {
            let pin =
                TeamPin { team: t.team.clone(), founder: t.founder.clone(), founder_root: t.founder_root.clone() };
            let sig = t.sig.as_deref().unwrap_or_default();
            if !approver.is_some_and(|a| pin.join_signed_by(&cert.device, a, sig)) {
                bail!(
                    "control put this machine in the team {}, but the approving device didn't sign that; not joining",
                    t.name
                );
            }
            Some(pin)
        }
        None => None,
    };
    let saved = Saved {
        url: url.clone(),
        trust: trust.clone(),
        cert: cert.clone(),
        certs: all,
        revocations: got.revocations,
        team: pin.clone(),
        roster: None,
        team_certs: Default::default(),
        team_names: Default::default(),
        locked: false,
        peers: Default::default(),
        shared_teams: Default::default(),
        login: String::new(),
        moved_at: 0,
    };
    let joined = Joined {
        place: match &got.team {
            Some(t) => format!("the team {}", t.name),
            None => "your account".into(),
        },
        approver: approver.map(|c| c.name.clone()).unwrap_or_default(),
        not_team: team_name.filter(|_| got.team.is_none()),
        key: fingerprint(&cert.device),
        account: fingerprint(&trust.root),
    };
    Ok(Approved { saved, joined })
}

/// `illogicald join URL [--team ID] [--account FINGERPRINT]`: ask, show
/// the code, wait, check the account with the person, pin, save.
pub async fn join(
    url: &str,
    name: &str,
    team: Option<&str>,
    account: Option<&str>,
    ticket: Option<&str>,
    state_dir: &Path,
) -> anyhow::Result<()> {
    let account = account.map(parse_fingerprint).transpose()?;
    let p = join_start(url, name, team, ticket, state_dir, "`illogicald join`").await?;
    if let Some(r) = &p.renewed {
        println!();
        println!("  Control says {}.", r.said);
        println!("  So this machine made a new key ({}) and asks to join with it.", r.new_key);
        println!("  The old key ({}) is kept at {}.", r.old_key, r.kept);
    }
    let rejoining = p.renewed.as_ref().and_then(|r| r.root.clone());
    let to = match &p.team_name {
        Some(t) => format!("the team {t}"),
        None => "your account".into(),
    };
    println!();
    println!("  To add this machine ({name}) to {to}, open");
    println!();
    println!("    {}", p.approve_url());
    println!();
    println!("  on a device that's signed in, and check the code there is {}.", p.code);
    println!("  Or sign in at {} and type the code.", p.url);
    if team.is_none() {
        println!("  Whoever approves picks their account or a team they own (--team ID picks one ahead).");
    }
    println!();
    println!("  Waiting for approval (the code lasts {} minutes)…", p.expires_in_secs / 60);
    let url = p.url.clone();
    let a = join_finish(p).await?;
    let fp = a.joined.account.clone();
    let checked = match &account {
        Some(want) if !a.is_account(want) => Err(anyhow::anyhow!(
            "control approved this machine into the account {fp}, not {}; not joining",
            fingerprint(want)
        )),
        Some(_) => Ok(()),
        // A hosted sandbox runs on control's own provider: there is no
        // second device to check against.
        None if ticket.is_some() => Ok(()),
        // Back into the account it was removed from (#330): the person
        // checked that one's fingerprint when it first joined.
        None if rejoining.as_deref().is_some_and(|root| a.is_account(root)) => Ok(()),
        None => confirm_account(&a),
    };
    if let Err(e) = checked {
        a.refuse(state_dir).await;
        return Err(e);
    }
    let j = a.save(state_dir)?;
    println!();
    println!("  Joined. This machine is in {}, approved on \"{}\".", j.place, j.approver);
    if let Some(t) = &j.not_team {
        println!("  (Not the team {t}: the approver kept it to their account.)");
    }
    println!("  Its key is {}; the account is {}.", j.key, j.account);
    println!("  Open {url}; it's in the host menu.");
    Ok(())
}

/// Ask the person whether the account control sent is theirs: its
/// fingerprint here against the one on the device they approved on.
fn confirm_account(a: &Approved) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    println!();
    println!("  Approved on \"{}\". Before this machine trusts it, check the account:", a.joined.approver);
    println!();
    println!("    {}", a.joined.account);
    println!();
    println!("  The device you approved on shows its account's fingerprint when it approves,");
    println!("  and under Devices and machines… in the host menu.");
    print!("  Is it the same? [y/N, or type the fingerprint] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    let n = std::io::stdin().lock().read_line(&mut line)?;
    let said = line.trim();
    if n == 0 {
        bail!(
            "no answer, so not joining; to confirm without a prompt, pass --account with the fingerprint your device shows"
        );
    }
    if matches!(said.to_ascii_lowercase().as_str(), "y" | "yes") || a.is_account(said) {
        return Ok(());
    }
    if parse_fingerprint(said).is_ok() {
        bail!(
            "that's not the account control approved this machine into ({}); not joining. Don't add machines through this control.",
            a.joined.account
        );
    }
    bail!(
        "not joining. If the fingerprints differ, control isn't telling the truth about your account: don't add machines through it"
    )
}

/// A hosted sandbox's side of joining (M20): its key, made here and never
/// leaving, and the request control fetches through the provider.
pub fn join_request(name: &str, out: &Path, state_dir: &Path) -> anyhow::Result<()> {
    let keys = DeviceKeys::load_or_create(&state_dir.join(KEY_FILE))?;
    let ask = Cert { account: String::new(), ..Cert::new(&keys, "", Kind::Daemon, name) };
    crate::store::write_atomic(out, &serde_json::to_vec(&ask)?)?;
    Ok(())
}

/// `http://` to loopback or a private address: a test or lab control.
fn private_http(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    if u.scheme() != "http" {
        return false;
    }
    match u.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => {
            ip.is_loopback() || ip.is_private() || (ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64)
        }
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// `illogicald leave`: tell control, forget it. `listen` is where this
/// machine is still reached locally.
pub async fn leave(state_dir: &Path, listen: &str) -> anyhow::Result<()> {
    let Some(s) = read_saved(state_dir)? else { bail!("this machine isn't joined to any control") };
    let keys = DeviceKeys::load(&state_dir.join(KEY_FILE))?;
    let path = "/api/daemon/leave";
    let http = crate::roots::client();
    let v2 = takes_v2(&http, &s.url).await;
    let res =
        http.post(format!("{}{path}", s.url)).header(AUTH, auth_header(&keys, "POST", path, b"", v2)).send().await;
    match res {
        Ok(r) if r.status().is_success() => println!("Left {} ({}).", s.url, whose(&s)),
        Ok(r) => println!("{} (leaving anyway)", control_said(r).await),
        Err(e) => println!("can't reach control ({e}); leaving anyway"),
    }
    std::fs::remove_file(state_dir.join(FILE))?;
    println!("illogical keeps running here; reach it at http://{listen}.");
    println!("Rejoin with `illogicald join {}` (the approver picks their account or a team).", s.url);
    Ok(())
}

#[cfg(test)]
mod tests {
    use illogical_e2e::{DeviceKeys, Kind};

    use super::*;

    fn device(account: &str, kind: Kind) -> (DeviceKeys, Cert) {
        let keys = DeviceKeys::generate();
        let mut cert = Cert::new(&keys, account, kind, "x");
        cert.sign_with(&keys);
        (keys, cert)
    }

    fn mv(keys: &DeviceKeys, by: &Cert, daemon: &str, team: Option<&TeamPin>, at: u64) -> Move {
        let sig = hex::encode(keys.signature(Move::body(daemon, team, at).as_bytes()));
        Move { team: team.cloned(), at, by: by.device.clone(), sig }
    }

    #[test]
    fn fingerprints_compare_as_typed() {
        assert!(same_fingerprint("0123456789abcdef", "0123-4567-89AB-cdef"));
        assert!(same_fingerprint("0123456789abcdef", " 0123456789abcdef\n"));
        assert!(!same_fingerprint("0123456789abcdef", "0123-4567-89ab-cdee"));
        assert!(!same_fingerprint("", ""));
        assert_eq!(parse_fingerprint("0123-4567-89AB-cdef").unwrap(), "0123456789abcdef");
        assert!(parse_fingerprint("0123-4567").is_err());
        assert!(parse_fingerprint("y").is_err());
    }

    /// A control that approves the machine into an account of its own:
    /// nothing is saved unless the account is the one the person expects.
    #[tokio::test]
    async fn a_join_saves_only_the_account_the_person_confirms() {
        use axum::{
            Json, Router,
            routing::{get, post},
        };
        let (theirs, mut root) = device("evil", Kind::Browser);
        root.approver = root.device.clone();
        root.sig = hex::encode(theirs.signature(root.body().as_bytes()));
        let asked: Arc<std::sync::Mutex<Option<Cert>>> = Default::default();
        let (a1, a2) = (asked.clone(), asked.clone());
        let (root2, theirs) = (root.clone(), Arc::new(theirs));
        let app = Router::new()
            .route(
                "/api/join",
                post(move |Json(b): Json<serde_json::Value>| async move {
                    let c: Cert = serde_json::from_value(b["cert"].clone()).unwrap();
                    let code = join_code(&c);
                    *a1.lock().unwrap() = Some(c);
                    Json(serde_json::json!({ "code": code, "poll": "p", "expires_in_secs": 60 }))
                }),
            )
            .route(
                "/api/join/{code}",
                get(move || async move {
                    let mut c = a2.lock().unwrap().clone().unwrap();
                    c.account = "evil".into();
                    c.sign_with(&theirs);
                    Json(serde_json::json!({
                        "approved": true, "cert": c, "trust": { "account": "evil", "root": root2.device },
                        "certs": [root2], "revocations": [],
                    }))
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let dir = std::env::temp_dir().join(format!("illogical-join-fp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mine = DeviceKeys::generate().id();
        let e = join(&url, "box", None, Some(&mine), None, &dir).await.expect_err("refused");
        assert!(e.to_string().contains("not joining"), "{e}");
        assert!(read_saved(&dir).unwrap().is_none(), "nothing pinned");

        // The approval itself checks out: only the account is wrong.
        let a = join_finish(join_start(&url, "box", None, None, &dir, "a test").await.unwrap()).await.unwrap();
        assert_eq!(a.joined.account, fingerprint(&root.device));
        assert!(!a.is_account(&mine) && a.is_account(&fingerprint(&root.device)));

        join(&url, "box", None, Some(&fingerprint(&root.device)), None, &dir).await.unwrap();
        assert_eq!(read_saved(&dir).unwrap().unwrap().trust.root, root.device);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A control of the test's own (#232). As the real one, it hands a
    /// daemon the subscriptions only of accounts it serves: its own, and
    /// those the daemon told it about that control routes to it (`b1`,
    /// once `routed`); and it refuses to relay to anyone else.
    #[derive(Default)]
    struct Fake {
        /// What `/api/daemon/access` was last told.
        published: Vec<String>,
        routed: bool,
        own: Vec<Cert>,
        peer: Option<Cert>,
        subs: Vec<PushSub>,
        /// Pushes relayed: endpoint and body.
        relayed: Vec<(String, Vec<u8>)>,
    }

    struct Rig {
        dir: PathBuf,
        fake: Arc<std::sync::Mutex<Fake>>,
        /// Held, a refresh stops at `/api/daemon/peers` (after it read the
        /// grants); `at_peers` says it got there.
        hold: Arc<tokio::sync::Mutex<()>>,
        at_peers: mpsc::UnboundedReceiver<()>,
        acl: Arc<Acl>,
        bea: DeviceKeys,
        phone: p256::SecretKey,
    }

    /// Enrolled in account `a1` at a [`Fake`]; `b1` (Bea) has an account,
    /// a phone with push on, and no grant yet.
    async fn rig(name: &str) -> Rig {
        use axum::{
            Json, Router,
            extract::{Query, State},
            http::StatusCode,
            routing::{get, post},
        };
        use base64::Engine;
        type St = (Arc<std::sync::Mutex<Fake>>, Arc<tokio::sync::Mutex<()>>, mpsc::UnboundedSender<()>);

        let (akeys, aroot) = device("a1", Kind::Browser);
        let dkeys = DeviceKeys::generate();
        let mut dcert = Cert::new(&dkeys, "a1", Kind::Daemon, "box");
        dcert.sign_with(&akeys);
        let (bea, broot) = device("b1", Kind::Browser);
        let phone = p256::SecretKey::from_slice(&[7; 32]).unwrap();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mut sub = PushSub {
            v: 1,
            account: "b1".into(),
            device: String::new(),
            endpoint: "https://push.test/b1".into(),
            p256dh: b64.encode(phone.public_key().to_sec1_bytes()),
            auth: b64.encode([7u8; 16]),
            at: now_ms(),
            sig: String::new(),
        };
        sub.sign_with(&bea);
        let fake = Arc::new(std::sync::Mutex::new(Fake {
            own: vec![aroot.clone(), dcert.clone()],
            peer: Some(broot),
            subs: vec![sub],
            ..Default::default()
        }));
        let hold: Arc<tokio::sync::Mutex<()>> = Default::default();
        let (hit, at_peers) = mpsc::unbounded_channel();

        #[derive(Deserialize)]
        struct Accounts {
            accounts: String,
        }
        let app = Router::new()
            .route(
                "/api/daemon/trust",
                get(|State((f, _, _)): State<St>| async move {
                    Json(serde_json::json!({ "certs": f.lock().unwrap().own, "revocations": [] }))
                }),
            )
            .route(
                "/api/daemon/peers",
                get(|State((f, hold, hit)): State<St>, Query(q): Query<Accounts>| async move {
                    let _ = hit.send(());
                    drop(hold.lock().await);
                    let f = f.lock().unwrap();
                    let mut out = serde_json::Map::new();
                    for a in q.accounts.split(',') {
                        if a == "b1" && f.routed {
                            out.insert(
                                a.into(),
                                serde_json::json!({ "name": "bea", "certs": [f.peer], "revocations": [] }),
                            );
                        }
                    }
                    Json(serde_json::Value::Object(out))
                }),
            )
            .route(
                "/api/daemon/access",
                post(|State((f, _, _)): State<St>, Json(b): Json<serde_json::Value>| async move {
                    f.lock().unwrap().published = serde_json::from_value(b["accounts"].clone()).unwrap();
                    Json(serde_json::json!({}))
                }),
            )
            .route(
                "/api/daemon/push-subs",
                get(|State((f, _, _)): State<St>| async move {
                    let f = f.lock().unwrap();
                    let served = |a: &str| f.routed && f.published.iter().any(|p| p == a);
                    let subs: Vec<&PushSub> = f.subs.iter().filter(|s| served(&s.account)).collect();
                    Json(serde_json::json!({ "subs": subs }))
                }),
            )
            .route(
                "/api/daemon/push",
                post(|State((f, _, _)): State<St>, Json(b): Json<serde_json::Value>| async move {
                    let mut f = f.lock().unwrap();
                    let endpoint = b["endpoint"].as_str().unwrap_or_default().to_owned();
                    let Some(s) = f.subs.iter().find(|s| s.endpoint == endpoint) else {
                        return StatusCode::NOT_FOUND;
                    };
                    if !(f.routed && f.published.contains(&s.account)) {
                        return StatusCode::FORBIDDEN;
                    }
                    let body = base64::engine::general_purpose::STANDARD
                        .decode(b["body"].as_str().unwrap_or_default())
                        .unwrap();
                    f.relayed.push((endpoint, body));
                    StatusCode::OK
                }),
            )
            .with_state((fake.clone(), hold.clone(), hit));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });

        let dir = std::env::temp_dir().join(format!("illogical-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dkeys.save(&dir.join(KEY_FILE)).unwrap();
        let saved = Saved {
            url,
            trust: Trust { account: "a1".into(), root: aroot.device.clone() },
            cert: dcert,
            certs: vec![aroot],
            revocations: vec![],
            team: None,
            roster: None,
            team_certs: Default::default(),
            team_names: Default::default(),
            locked: false,
            peers: Default::default(),
            shared_teams: Default::default(),
            login: String::new(),
            moved_at: 0,
        };
        write_saved(&dir, &saved).unwrap();
        let acl = Arc::new(Acl::open(&dir));
        Rig { dir, fake, hold, at_peers, acl, bea, phone }
    }

    impl Rig {
        /// A daemon on it, its loop running (no relay socket: a socket
        /// would nudge it too, and the test wants only its own pokes), and
        /// its first refreshes over: none left to refresh by chance.
        async fn daemon(&self) -> Arc<Control> {
            let c = Control::new(&self.dir, vec![], String::new(), self.acl.clone(), false);
            c.run(|| {}, |_| tokio::spawn(std::future::pending()), |_| {});
            assert!(c.refresh_now(REFRESH_WAIT).await, "the first refresh");
            tokio::time::sleep(Duration::from_millis(300)).await;
            c
        }

        /// The owner shares session 1 with Bea, as `account:b1`.
        fn grant(&self) {
            let root = self.bea.id();
            self.acl.set_full(1, "account:b1", "bea", Some(Role::Editor), "owner", None, Some(root), None).unwrap();
        }

        /// What Bea's phone got, decrypted as a browser would (RFC 8291).
        fn opened(&self) -> Vec<serde_json::Value> {
            use aes_gcm::{Aes128Gcm, KeyInit, aead::Aead};
            use hkdf::Hkdf;
            use sha2::Sha256;
            let f = self.fake.lock().unwrap();
            f.relayed
                .iter()
                .map(|(_, body)| {
                    let (salt, rest) = body.split_at(16);
                    let id_len = rest[4] as usize;
                    let (as_public, sealed) = rest[5..].split_at(id_len);
                    let shared = p256::ecdh::diffie_hellman(
                        self.phone.to_nonzero_scalar(),
                        p256::PublicKey::from_sec1_bytes(as_public).unwrap().as_affine(),
                    );
                    let mut info = b"WebPush: info\0".to_vec();
                    info.extend_from_slice(&self.phone.public_key().to_sec1_bytes());
                    info.extend_from_slice(as_public);
                    let mut ikm = [0u8; 32];
                    Hkdf::<Sha256>::new(Some(&[7u8; 16]), shared.raw_secret_bytes().as_ref())
                        .expand(&info, &mut ikm)
                        .unwrap();
                    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
                    let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
                    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
                    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).unwrap();
                    let mut plain = Aes128Gcm::new_from_slice(&cek).unwrap().decrypt(&nonce.into(), sealed).unwrap();
                    assert_eq!(plain.pop(), Some(2));
                    serde_json::from_slice(&plain).unwrap()
                })
                .collect()
        }
    }

    fn to_bea(p: &Principal) -> bool {
        p.id() == "account:b1"
    }

    /// #232: someone just granted (whom control routes here) is reachable
    /// once the next refresh is through, not a minute later: the access
    /// list goes to control before the subscriptions are fetched.
    #[tokio::test]
    async fn a_grant_is_reachable_after_one_refresh() {
        let r = rig("reach").await;
        r.fake.lock().unwrap().routed = true;
        let c = r.daemon().await;
        let nobody = c.push_report(1, "t", "b", None, to_bea).await;
        assert_eq!(nobody, Pushed::default(), "not before the grant");

        r.grant();
        let t = std::time::Instant::now();
        assert!(c.refresh_now(REFRESH_WAIT).await);
        assert!(t.elapsed() < Duration::from_secs(5), "no waiting out the minute: {:?}", t.elapsed());
        let got = c.push_report(1, "Waiting", "on you", Some(serde_json::json!({ "tag": "invite-7" })), to_bea).await;
        assert_eq!(got, Pushed { matched: 1, relayed: 1, refused: 0 });
        let got = c.push_report(4, "Waiting", "again", None, to_bea).await;
        assert_eq!(got.relayed, 1);
        let tags: Vec<_> = r.opened().iter().map(|n| n["tag"].as_str().unwrap().to_owned()).collect();
        assert_eq!(tags, ["invite-7", "pane-4"], "the caller's tag, else the pane's");

        // Control stops routing her here (she turned it down, say) before
        // this daemon heard: the push still matched, and was refused.
        r.fake.lock().unwrap().routed = false;
        let got = c.push_report(1, "t", "b", None, to_bea).await;
        assert_eq!(got, Pushed { matched: 1, relayed: 0, refused: 1 });
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    /// The trap (#232): a refresh already under way when `refresh_now` is
    /// called may have read the grants before the new one; it doesn't
    /// count. The one after does.
    #[tokio::test]
    async fn refresh_now_waits_for_a_refresh_that_started_after_it() {
        let mut r = rig("trap").await;
        r.fake.lock().unwrap().routed = true;
        let c = r.daemon().await;
        while r.at_peers.try_recv().is_ok() {}

        // One under way, past reading the grants; then the grant.
        let held = r.hold.clone().lock_owned().await;
        c.poke();
        r.at_peers.recv().await.unwrap();
        r.grant();
        let c2 = c.clone();
        let waiting = tokio::spawn(async move { c2.refresh_now(REFRESH_WAIT).await });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!waiting.is_finished());
        drop(held);

        assert!(waiting.await.unwrap());
        assert_eq!(c.push_report(1, "t", "b", None, to_bea).await.matched, 1, "the refresh that saw the grant");
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    /// Only a refresh that went through counts (#232): not a reload or a
    /// change of trust (`changed`), and not one that failed. With control
    /// unreachable, `refresh_now` gives up after the timeout.
    #[tokio::test]
    async fn only_a_refresh_counts_and_an_unreachable_control_times_out() {
        let r = rig("generation").await;
        let c = Control::new(&r.dir, vec![], String::new(), r.acl.clone(), false);
        let changed = *c.changed.borrow();
        c.reload();
        c.install(c.enrolled().map(|e| Enrolled::build(e.saved.clone(), e.keys.clone(), &r.acl)));
        assert!(*c.changed.borrow() > changed);
        assert_eq!(*c.refreshed.borrow(), 0, "not by reload or install");
        c.refresh().await.unwrap();
        assert_eq!(*c.refreshed.borrow(), 1, "by a refresh");

        // Control gone: every refresh fails, so none counts.
        let mut saved = read_saved(&r.dir).unwrap().unwrap();
        saved.url = "http://127.0.0.1:1".into();
        write_saved(&r.dir, &saved).unwrap();
        c.reload();
        assert!(c.refresh().await.is_err());
        assert_eq!(*c.refreshed.borrow(), 1);
        c.run(|| {}, |_| tokio::spawn(std::future::pending()), |_| {});
        let t = std::time::Instant::now();
        assert!(!c.refresh_now(Duration::from_millis(500)).await);
        assert!(t.elapsed() >= Duration::from_millis(500));
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    /// #233: a waiting invite goes out after the refresh that finds its
    /// person reachable, once; one that waited a day stops waiting,
    /// unpushed, as does one whose share is gone. A second invite into the
    /// same session replaces the first. Kept across restarts.
    #[tokio::test]
    async fn waiting_invites_go_once_or_stop_after_a_day() {
        let r = rig("waiting").await;
        let c = r.daemon().await;
        // Shared with session 1 (not yet routed by control); never with 2.
        r.grant();
        let w = |at, session, tag: &str| Waiting {
            who: "account:b1".into(),
            session: Some(session),
            pane: 1,
            title: "alex brought you into api".into(),
            body: "b".into(),
            extra: serde_json::json!({ "tag": tag }),
            at,
        };
        c.wait_invite(w(now_ms() - INVITE_WAIT_MS - 1, 3, "invite-old"));
        c.wait_invite(w(now_ms(), 1, "invite-first"));
        c.wait_invite(w(now_ms(), 1, "invite-new"));
        c.wait_invite(w(now_ms(), 2, "invite-revoked"));
        assert_eq!(c.invites.lock().unwrap().len(), 3, "the second into session 1 replaced the first");
        c.retry_invites().await;
        let left: Vec<_> = c.invites.lock().unwrap().iter().map(|w| w.extra["tag"].clone()).collect();
        assert_eq!(left, ["invite-new"], "the old one and the revoked one stopped waiting");
        let kept = Control::new(&r.dir, vec![], String::new(), r.acl.clone(), false);
        assert_eq!(kept.invites.lock().unwrap().len(), 1, "kept in invites.json");

        r.fake.lock().unwrap().routed = true;
        assert!(c.refresh_now(REFRESH_WAIT).await);
        let t = std::time::Instant::now();
        while r.opened().is_empty() && t.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let tags: Vec<_> = r.opened().iter().map(|n| n["tag"].as_str().unwrap().to_owned()).collect();
        assert_eq!(tags, ["invite-new"]);
        assert!(c.invites.lock().unwrap().is_empty());
        assert!(c.refresh_now(REFRESH_WAIT).await);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(r.opened().len(), 1, "once");
        let _ = std::fs::remove_dir_all(&r.dir);
    }

    /// #330: control says this machine's key was removed from its account.
    /// The join sets the old key and enrollment aside, asks with a new key,
    /// and, back in the same account, saves it without asking again.
    #[tokio::test]
    async fn a_removed_key_is_set_aside_and_a_new_one_joins() {
        use axum::{
            Json, Router,
            http::StatusCode,
            routing::{get, post},
        };
        let (root_keys, mut root) = device("a", Kind::Browser);
        root.approver = root.device.clone();
        root.sig = hex::encode(root_keys.signature(root.body().as_bytes()));
        let dir = std::env::temp_dir().join(format!("illogical-join-removed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Joined before, with a key a browser has since removed.
        let old = DeviceKeys::load_or_create(&dir.join(KEY_FILE)).unwrap();
        let mut old_cert = Cert::new(&old, "a", Kind::Daemon, "box");
        old_cert.sign_with(&root_keys);
        let saved = Saved {
            url: String::new(),
            trust: Trust { account: "a".into(), root: root.device.clone() },
            cert: old_cert,
            certs: vec![root.clone()],
            revocations: vec![],
            team: None,
            roster: None,
            team_certs: Default::default(),
            team_names: Default::default(),
            locked: false,
            peers: Default::default(),
            shared_teams: Default::default(),
            login: String::new(),
            moved_at: 0,
        };

        write_saved(&dir, &saved).unwrap();

        let asked: Arc<std::sync::Mutex<Vec<Cert>>> = Default::default();
        let (a1, a2) = (asked.clone(), asked.clone());
        let (old_id, root2, root_keys) = (old.id(), root.clone(), Arc::new(root_keys));
        let gone = |by: Option<&str>| {
            let mut v = serde_json::json!({ "error": "this machine was removed from its account on 2026-10-03 by laptop, so its key can't join again: it needs a new key" });
            if let Some(by) = by {
                v["removed"] = serde_json::json!({ "at": 1_790_000_000_000u64, "by": by });
            }
            (StatusCode::GONE, Json(v))
        };
        let app = Router::new()
            .route("/control.json", get(|| async { Json(serde_json::json!({ "daemon_auth": 2 })) }))
            .route("/api/daemon/trust", get(move || async move { gone(None) }))
            .route(
                "/api/join",
                post(move |Json(b): Json<serde_json::Value>| async move {
                    let c: Cert = serde_json::from_value(b["cert"].clone()).unwrap();
                    a1.lock().unwrap().push(c.clone());
                    if c.device == old_id {
                        return gone(Some("laptop"));
                    }
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({ "code": join_code(&c), "poll": "p", "expires_in_secs": 60 })),
                    )
                }),
            )
            .route(
                "/api/join/{code}",
                get(move || async move {
                    let mut c = a2.lock().unwrap().last().cloned().unwrap();
                    c.account = "a".into();
                    c.sign_with(&root_keys);
                    Json(serde_json::json!({
                        "approved": true, "cert": c, "trust": { "account": "a", "root": root2.device },
                        "certs": [root2], "revocations": [],
                    }))
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let mut s = read_saved(&dir).unwrap().unwrap();
        s.url = url.clone();
        write_saved(&dir, &s).unwrap();

        // The old key asks once and is told; the new one gets the code.
        let p = join_start(&url, "box", None, None, &dir, "a test").await.unwrap();
        let r = p.renewed.clone().expect("renewed");
        assert_eq!(r.by.as_deref(), Some("laptop"));
        assert_eq!(r.at, Some(1_790_000_000_000));
        assert_eq!(r.old_key, fingerprint(&old.id()));
        assert_eq!(r.root.as_deref(), Some(root.device.as_str()));
        let new = DeviceKeys::load(&dir.join(KEY_FILE)).unwrap();
        assert_ne!(new.id(), old.id());
        assert_eq!(r.new_key, fingerprint(&new.id()));
        assert_eq!(p.code, join_code(&Cert::new(&new, "", Kind::Daemon, "box")));
        let ids: Vec<String> = asked.lock().unwrap().iter().map(|c| c.device.clone()).collect();
        assert_eq!(ids, [old.id(), new.id()]);
        // Kept aside, not deleted: the old key and what it was enrolled as.
        assert_eq!(DeviceKeys::load(Path::new(&r.kept)).unwrap().id(), old.id());
        assert!(read_saved(&dir).unwrap().is_none());
        let kept: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".removed-"))
            .collect();
        assert_eq!(kept.len(), 2, "{kept:?}");
        let rejoining = r.root.unwrap();
        let a = join_finish(p).await.unwrap();
        assert!(a.is_account(&rejoining));

        // `illogicald join` does it all, and back in the same account it
        // doesn't ask (stdin has nothing to say here).
        std::fs::write(dir.join(KEY_FILE), std::fs::read(&r.kept).unwrap()).unwrap();
        write_saved(&dir, &s).unwrap();
        join(&url, "box", None, None, None, &dir).await.unwrap();
        let now = read_saved(&dir).unwrap().unwrap();
        assert_ne!(now.cert.device, old.id());
        assert_eq!(now.trust.root, root.device);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #329: one join at a time on a machine. A second way in is told which
    /// code is waiting and where to approve it; a lock whose process is gone
    /// doesn't count.
    #[tokio::test]
    async fn one_join_at_a_time_on_a_machine() {
        use axum::{Json, Router, routing::post};
        let app = Router::new().route(
            "/api/join",
            post(|Json(b): Json<serde_json::Value>| async move {
                let c: Cert = serde_json::from_value(b["cert"].clone()).unwrap();
                Json(serde_json::json!({ "code": join_code(&c), "poll": "p", "expires_in_secs": 900 }))
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let dir = std::env::temp_dir().join(format!("illogical-join-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let first = join_start(&url, "box", None, None, &dir, "Getting started").await.unwrap();
        let held = JoinLock::held(&dir).unwrap();
        assert!(held.mine() && held.code.as_deref() == Some(first.code.as_str()));
        let e = join_start(&url, "box", None, None, &dir, "`illogicald join`").await.err().unwrap().to_string();
        assert!(e.contains("Getting started") && e.contains(&first.code) && e.contains(&first.approve_url()), "{e}");
        // Ended (approved, failed, or given up): the next one may ask.
        drop(first);
        assert!(JoinLock::held(&dir).is_none());
        let second = join_start(&url, "box", None, None, &dir, "`illogicald join`").await.unwrap();
        drop(second);

        // A CLI stopped with Ctrl-C leaves its lock behind: it doesn't count.
        let gone = JoinLockInfo {
            pid: 999_999_999,
            by: "`illogicald join`".into(),
            code: Some("AAAAA-AAAAA".into()),
            approve: None,
            expires_ms: now_ms() + 600_000,
        };
        std::fs::write(dir.join(JOIN_LOCK), serde_json::to_vec(&gone).unwrap()).unwrap();
        assert!(JoinLock::held(&dir).is_none());
        join_start(&url, "box", None, None, &dir, "Getting started").await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #100: a move signed by the account's own device is taken; one by
    /// anyone else, or an older one control replays, isn't.
    #[test]
    fn a_daemon_takes_only_newer_moves_its_account_signed() {
        let (keys, root) = device("a", Kind::Browser);
        let (_, daemon) = device("a", Kind::Daemon);
        let (mkeys, mallory) = device("m", Kind::Browser);
        let mut saved = Saved {
            url: String::new(),
            trust: Trust { account: "a".into(), root: root.device.clone() },
            cert: daemon.clone(),
            certs: vec![root.clone()],
            revocations: vec![],
            team: None,
            roster: None,
            team_certs: Default::default(),
            team_names: Default::default(),
            locked: false,
            peers: Default::default(),
            shared_teams: Default::default(),
            login: String::new(),
            moved_at: 0,
        };
        let pin = TeamPin { team: "t1".into(), founder: "a".into(), founder_root: root.device.clone() };
        let d = daemon.device.as_str();

        take_move(&mut saved, mv(&mkeys, &mallory, d, Some(&pin), 5));
        assert_eq!(saved.team, None, "not by someone else");
        take_move(&mut saved, mv(&keys, &root, "other", Some(&pin), 5));
        assert_eq!(saved.team, None, "not one signed for another machine");

        let into = mv(&keys, &root, d, Some(&pin), 5);
        take_move(&mut saved, into.clone());
        assert_eq!((saved.team.as_ref(), saved.moved_at), (Some(&pin), 5));
        take_move(&mut saved, mv(&keys, &root, d, None, 9));
        assert_eq!((saved.team.as_ref(), saved.moved_at), (None, 9));
        // Control replays the move into the team: too old.
        take_move(&mut saved, into);
        assert_eq!(saved.team, None);
    }

    /// #332: a member's machine in a team; the team's owners may take it
    /// out, but not put it anywhere, and an editor may do neither.
    #[test]
    fn a_teams_owners_take_a_members_machine_out() {
        let (_, root) = device("m", Kind::Browser);
        let (_, daemon) = device("m", Kind::Daemon);
        let (okeys, owner) = device("o", Kind::Browser);
        let (ekeys, editor) = device("e", Kind::Browser);
        let member = |account: &str, root: &Cert, role| illogical_e2e::team::Member {
            account: account.into(),
            root: root.device.clone(),
            role,
            name: account.into(),
        };
        let roster = Roster {
            v: 1,
            team: "t1".into(),
            name: "Acme".into(),
            version: 2,
            at: 1,
            members: vec![
                member("o", &owner, TeamRole::Owner),
                member("e", &editor, TeamRole::Editor),
                member("m", &root, TeamRole::Editor),
            ],
            spent: vec![],
            redeem: None,
            by: String::new(),
            sig: String::new(),
        };
        let pin = TeamPin { team: "t1".into(), founder: "o".into(), founder_root: owner.device.clone() };
        let mut saved = Saved {
            url: String::new(),
            trust: Trust { account: "m".into(), root: root.device.clone() },
            cert: daemon.clone(),
            certs: vec![root],
            revocations: vec![],
            team: Some(pin.clone()),
            roster: Some(roster),
            team_certs: [
                ("o".to_owned(), (vec![owner.clone()], vec![])),
                ("e".to_owned(), (vec![editor.clone()], vec![])),
            ]
            .into_iter()
            .collect(),
            team_names: Default::default(),
            locked: false,
            peers: Default::default(),
            shared_teams: Default::default(),
            login: String::new(),
            moved_at: 0,
        };
        let d = daemon.device.as_str();

        take_move(&mut saved, mv(&ekeys, &editor, d, None, 5));
        assert_eq!(saved.team.as_ref(), Some(&pin), "not by an editor");
        let other = TeamPin { team: "t2".into(), ..pin.clone() };
        take_move(&mut saved, mv(&okeys, &owner, d, Some(&other), 5));
        assert_eq!(saved.team.as_ref(), Some(&pin), "an owner doesn't move it elsewhere");

        take_move(&mut saved, mv(&okeys, &owner, d, None, 6));
        assert_eq!((saved.team.as_ref(), saved.moved_at), (None, 6));
        // Out of the team, its owners are nobody to it.
        take_move(&mut saved, mv(&okeys, &owner, d, None, 7));
        assert_eq!(saved.moved_at, 6);
    }

    /// #386: a team's other owner gets in as the owner, but is someone of
    /// their own by account: `account:o` reaches their devices alone, the
    /// owner's pushes still reach them, and an editor stays an editor.
    #[test]
    fn a_teams_other_owner_is_reachable_by_account() {
        let (keys, root) = device("m", Kind::Browser);
        let (_, daemon) = device("m", Kind::Daemon);
        let (_, owner) = device("o", Kind::Browser);
        let (_, editor) = device("e", Kind::Browser);
        let member = |account: &str, root: &Cert, role| illogical_e2e::team::Member {
            account: account.into(),
            root: root.device.clone(),
            role,
            name: account.into(),
        };
        let roster = Roster {
            v: 1,
            team: "t1".into(),
            name: "Acme".into(),
            version: 1,
            at: 1,
            members: vec![
                member("m", &root, TeamRole::Owner),
                member("o", &owner, TeamRole::Owner),
                member("e", &editor, TeamRole::Editor),
            ],
            spent: vec![],
            redeem: None,
            by: String::new(),
            sig: String::new(),
        };
        let saved = Saved {
            url: String::new(),
            trust: Trust { account: "m".into(), root: root.device.clone() },
            cert: daemon,
            certs: vec![root.clone()],
            revocations: vec![],
            team: None,
            roster: Some(roster),
            team_certs: [
                ("o".to_owned(), (vec![owner.clone()], vec![])),
                ("e".to_owned(), (vec![editor.clone()], vec![])),
            ]
            .into_iter()
            .collect(),
            team_names: [("o".to_owned(), "Olive".to_owned())].into_iter().collect(),
            locked: false,
            peers: Default::default(),
            shared_teams: Default::default(),
            login: String::new(),
            moved_at: 0,
        };
        let dir = std::env::temp_dir().join(format!("illogical-co-owners-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let acl = Arc::new(Acl::open(&dir));
        let mut e = Enrolled::build(saved, Arc::new(keys), &acl);
        let sub = |account: &str, device: &Cert| PushSub {
            v: 1,
            account: account.into(),
            device: device.device.clone(),
            endpoint: format!("https://push.test/{account}"),
            p256dh: String::new(),
            auth: String::new(),
            at: 0,
            sig: String::new(),
        };
        let editor_is = Principal::User { id: "account:e".into(), name: "e".into(), pic: None };
        e.push = vec![
            (Principal::Owner, sub("m", &root)),
            (Principal::Owner, sub("o", &owner)),
            (editor_is.clone(), sub("e", &editor)),
        ];
        let c = Control::new(&dir, vec![], String::new(), acl.clone(), false);
        c.install(Some(e));

        let ids: Vec<_> = c.co_owners().iter().map(|p| (p.id().to_owned(), c_name(p))).collect();
        assert_eq!(ids, [("account:o".to_owned(), "Olive".to_owned())], "not this machine's own, nor an editor");
        assert!(c.owns_here("o") && c.owns_here("m") && !c.owns_here("e"));
        assert!(c.is_me("m") && !c.is_me("o"));
        assert!(c.reaches("account:o") && c.reaches("account:e") && !c.reaches("account:m"));

        let to = |f: fn(&Principal) -> bool| -> Vec<String> {
            c.to_push(1, "t", "b", None, f)
                .map(|(_, s, _)| s.into_iter().map(|s| s.account).collect())
                .unwrap_or_default()
        };
        assert_eq!(to(|p| p.id() == "account:o"), ["o"], "their own devices, by account");
        assert_eq!(to(|p| p.is_owner()), ["m", "o"], "the owner's pushes still reach them");
        assert_eq!(to(|p| p.id() == "account:e"), ["e"]);
        assert!(to(|p| p.id() == "account:m").is_empty(), "this machine's own account is the owner");
        let _ = std::fs::remove_dir_all(&dir);

        fn c_name(p: &Principal) -> String {
            match p {
                Principal::User { name, .. } => name.clone(),
                Principal::Owner => "owner".into(),
            }
        }
    }

    /// #208: a team member is called what they set ("Sam Stranger"), not
    /// the roster's one-word form, once control says it.
    #[test]
    fn team_members_go_by_the_names_they_set() {
        let (keys, root) = device("a", Kind::Browser);
        let (_, daemon) = device("a", Kind::Daemon);
        let (_, sam) = device("s", Kind::Browser);
        let roster = Roster {
            v: 1,
            team: "t1".into(),
            name: "Acme".into(),
            version: 1,
            at: 1,
            members: vec![illogical_e2e::team::Member {
                account: "s".into(),
                root: sam.device.clone(),
                role: TeamRole::Editor,
                name: "Sam-Stranger".into(),
            }],
            spent: vec![],
            redeem: None,
            by: String::new(),
            sig: String::new(),
        };
        let mut saved = Saved {
            url: String::new(),
            trust: Trust { account: "a".into(), root: root.device.clone() },
            cert: daemon,
            certs: vec![root],
            revocations: vec![],
            team: None,
            roster: Some(roster),
            team_certs: [("s".to_owned(), (vec![sam], vec![]))].into_iter().collect(),
            team_names: Default::default(),
            locked: false,
            peers: Default::default(),
            shared_teams: Default::default(),
            login: String::new(),
            moved_at: 0,
        };
        let dir = std::env::temp_dir().join(format!("illogical-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let acl = Acl::open(&dir);
        let keys = Arc::new(keys);
        let name = |saved: &Saved| match &Enrolled::build(saved.clone(), keys.clone(), &acl).others[0].1 {
            Principal::User { name, .. } => name.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(name(&saved), "Sam-Stranger");
        saved.team_names.insert("s".into(), "Sam Stranger".into());
        assert_eq!(name(&saved), "Sam Stranger");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
