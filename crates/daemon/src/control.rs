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
pub const KEY_FILE: &str = "daemon.key";
const REFRESH: Duration = Duration::from_secs(60);
/// What this daemon tells control it understands, so control offers only
/// what every daemon checking a team can take (presigned invites' rosters).
/// `ILLOGICAL_FEATURES` says otherwise (tests play an older daemon with "").
fn features() -> String {
    std::env::var("ILLOGICAL_FEATURES").unwrap_or_else(|_| "presigned-invites".into())
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
/// it: into a team, between teams, or back to the account. The new team's
/// roster is fetched from scratch, checked against the pin as at a join.
/// One no newer than the last it took is a replay (or the same one again).
fn take_move(saved: &mut Saved, m: Move) {
    if m.at <= saved.moved_at {
        return;
    }
    let trusted = saved.trust.evaluate(&saved.certs, &saved.revocations);
    if !trusted.get(&m.by).is_some_and(|by| m.signed_for(&saved.cert.device, by)) {
        warn!(by = m.by, "a move from control isn't signed by this account's devices; ignoring it");
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
    http: reqwest::Client,
    /// What control was last told about access.
    published: std::sync::Mutex<Option<serde_json::Value>>,
    /// Reached through a provider's proxy (a hosted sandbox, M20): no relay
    /// socket, so certificates are fetched more often instead of nudged.
    pub no_relay: bool,
    /// Control takes v2 request signatures (see [`auth_header`]).
    auth_v2: std::sync::atomic::AtomicBool,
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
            http: crate::roots::http().timeout(Duration::from_secs(20)).build().expect("http client"),
            published: Default::default(),
            no_relay,
            auth_v2: Default::default(),
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

    /// Look again soon (grants changed here, say).
    pub fn poke(&self) {
        self.nudge.notify_one();
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
        // the founder the grant pinned.
        let pins: BTreeMap<String, TeamPin> = self
            .acl
            .list()
            .iter()
            .filter_map(|g| {
                let team = g.principal.strip_prefix("team:")?;
                Some((team.to_owned(), team_pin(team, g.root.as_deref()?)?))
            })
            .collect();
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
        next.push = self.push_subs(&next).await;
        if next.trusted.get(&next.saved.cert.device).is_none() {
            warn!("this daemon's own certificate no longer checks out (revoked?)");
        }
        self.publish(&next).await;
        info!(devices = next.trusted.devices.len(), others = next.others.len(), changed, "certificates refreshed");
        self.install(Some(next));
        Ok(changed)
    }

    /// Notify people through control (M21): every verified subscription
    /// `to` accepts, encrypted here for that subscription alone.
    pub fn push(
        self: &Arc<Self>,
        pane: u32,
        title: &str,
        body: &str,
        extra: Option<serde_json::Value>,
        to: impl Fn(&Principal) -> bool,
    ) {
        let Some(e) = self.enrolled() else { return };
        let subs: Vec<PushSub> = e.push.iter().filter(|(p, _)| to(p)).map(|(_, s)| s.clone()).collect();
        if subs.is_empty() {
            return;
        }
        let mut payload = serde_json::json!({
            "title": title, "body": body, "pane": pane, "tag": format!("pane-{pane}"), "daemon": e.saved.cert.device,
        });
        if let Some(serde_json::Value::Object(extra)) = extra {
            payload.as_object_mut().unwrap().extend(extra);
        }
        let payload = payload.to_string();
        let me = self.clone();
        tokio::spawn(async move {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
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
                let res = me.post_json(&e, "/api/daemon/push", &req).send().await;
                if let Err(err) = res {
                    warn!(error = %err, "can't push through control");
                }
            }
        });
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
                    app.mux.send(crate::mux::Cmd::AclChanged);
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
                            Ok(_) => app.mux.send(crate::mux::Cmd::AclChanged),
                            Err(e) => warn!(error = %e, "can't refresh certificates from control"),
                        }
                        stamp = file_stamp(&me.state_dir);
                    }
                    if !me.no_relay && relay.as_ref().is_none_or(|r| r.is_finished()) {
                        relay = Some(tokio::spawn(keep_relay(me.clone(), app.clone())));
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
    let mut backoff = Duration::from_secs(1);
    loop {
        let Some(e) = control.enrolled() else { return };
        let started = std::time::Instant::now();
        match relay_once(&control, &e, &accept).await {
            Ok(()) => info!("relay socket closed"),
            Err(err) => warn!(error = %err, "can't reach control's relay"),
        }
        if started.elapsed() > Duration::from_secs(30) {
            backoff = Duration::from_secs(1);
        }
        let jitter = Duration::from_millis(u64::from(std::process::id() % 500));
        tokio::time::sleep(backoff + jitter).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

async fn relay_once(
    control: &Control,
    e: &Enrolled,
    accept: &mpsc::UnboundedSender<tokio::io::DuplexStream>,
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
    // M40: forge subscriptions out, pokes and heartbeats in.
    let texts = crate::forge::live::watch_messages()
        .map(|out| crate::dial::Texts { on_text: &crate::forge::live::from_control, out });
    crate::dial::serve_mux(ws, accept, Some(&control.nudge), texts).await
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

/// Why control refuses this machine's signature, if it does: what it said
/// ("this machine's account was deleted", or that it left or was
/// revoked). `None` when control still knows it, or can't be asked.
async fn forgotten(s: &Saved, keys: &DeviceKeys) -> Option<String> {
    let http = crate::roots::http().timeout(Duration::from_secs(10)).build().ok()?;
    let v2 = takes_v2(&http, &s.url).await;
    let path = "/api/daemon/trust";
    let res =
        http.get(format!("{}{path}", s.url)).header(AUTH, auth_header(keys, "GET", path, b"", v2)).send().await.ok()?;
    if res.status() != reqwest::StatusCode::UNAUTHORIZED {
        return None;
    }
    let said = control_said(res).await;
    let said = said.strip_prefix("control says: ").map(str::to_owned).unwrap_or(said);
    // Not a clock that's off or a replayed signature: control has no such machine.
    (said.contains("account was deleted") || said.contains("not an enrolled daemon")).then_some(said)
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

/// A join control has started: the code someone approves, and what
/// [`join_finish`] needs to wait for it.
pub struct JoinPending {
    pub url: String,
    pub code: String,
    pub expires_in_secs: u64,
    /// The team `--team` named, by name.
    pub team_name: Option<String>,
    poll: String,
    ask: Cert,
    http: reqwest::Client,
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
) -> anyhow::Result<JoinPending> {
    let url = url.trim_end_matches('/').to_owned();
    if !url.starts_with("https://") && !private_http(&url) {
        bail!("control's URL must be https:// (or http on loopback or a private network, for testing)");
    }
    let keys = DeviceKeys::load_or_create(&state_dir.join(KEY_FILE))?;
    if let Some(s) = read_saved(state_dir)? {
        // Control may have forgotten it (its account deleted, or it was
        // removed): say so, rather than that it's still in (#208).
        if let Some(why) = forgotten(&s, &keys).await {
            bail!(
                "this machine was in {} on {}, but control doesn't know it any more ({why}); run `illogicald leave` to forget that here, then join again",
                whose(&s),
                s.url
            );
        }
        bail!(
            "this machine is already in {} on {}; to move it, run `illogicald leave`, then join again",
            whose(&s),
            s.url
        );
    }
    let ask = Cert { account: String::new(), ..Cert::new(&keys, "", Kind::Daemon, name) };
    // That this is the key's holder asking, not someone with its certificate.
    let ms = now_ms();
    let proof = serde_json::json!({
        "ms": ms,
        "sig": hex::encode(keys.signature(illogical_e2e::cert::join_proof_body(&ask, ms).as_bytes())),
    });
    let http = crate::roots::http().timeout(Duration::from_secs(20)).build()?;
    let res = http
        .post(format!("{url}/api/join"))
        .json(&serde_json::json!({
            "cert": ask, "urls": [], "team": team, "ticket": ticket, "features": features(), "proof": proof,
        }))
        .send()
        .await
        .with_context(|| format!("can't reach control at {url}"))?;
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
    Ok(JoinPending {
        url,
        code: started.code,
        expires_in_secs: started.expires_in_secs,
        team_name: started.team_name,
        poll: started.poll,
        ask,
        http,
    })
}

/// Wait for someone to approve it and check the approval and the team the
/// approving device chose. Nothing is saved until the person confirms the
/// account ([`Approved::save`]).
pub async fn join_finish(p: JoinPending) -> anyhow::Result<Approved> {
    let JoinPending { url, code, expires_in_secs, team_name, poll, ask, http } = p;
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
    let p = join_start(url, name, team, ticket, state_dir).await?;
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
        let a = join_finish(join_start(&url, "box", None, None, &dir).await.unwrap()).await.unwrap();
        assert_eq!(a.joined.account, fingerprint(&root.device));
        assert!(!a.is_account(&mine) && a.is_account(&fingerprint(&root.device)));

        join(&url, "box", None, Some(&fingerprint(&root.device)), None, &dir).await.unwrap();
        assert_eq!(read_saved(&dir).unwrap().unwrap().trust.root, root.device);
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
