//! Enrolled in illogical control (M17, M18): who may connect, and the way
//! in through control's relay.
//!
//! `illogicald join URL` makes this daemon's keys (`<state>/daemon.key`),
//! asks control for a code, and waits until someone approves it from a
//! device of theirs. It then pins that account's root device and saves it
//! all in `<state>/control.json`. The running daemon notices the file
//! (within a few seconds), and from then on:
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
    team::{AccountCerts, Roster, TeamPin, TeamRole},
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
    /// The team is locked: only its owners get in.
    #[serde(default)]
    pub locked: bool,
    /// People sessions were shared with (by account): their certificates,
    /// checked against the roots pinned in their grants.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub peers: BTreeMap<String, PeerCerts>,
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
                        Principal::User { id: format!("account:{}", m.account), name: m.name.clone(), pic: None }
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
        Self { saved, keys, trusted, others, team_roles, push: Vec::new() }
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
    /// Control said the account's devices changed: refresh now.
    nudge: tokio::sync::Notify,
    http: reqwest::Client,
    /// What control was last told about access.
    published: std::sync::Mutex<Option<serde_json::Value>>,
    /// Reached through a provider's proxy (a hosted sandbox, M20): no relay
    /// socket, so certificates are fetched more often instead of nudged.
    pub no_relay: bool,
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

/// A request signature for control: see control's `auth.rs`.
pub fn auth_header(keys: &DeviceKeys, method: &str, path: &str) -> String {
    let ms = now_ms();
    let msg = format!("illogical daemon auth\n{method}\n{path}\n{ms}\n");
    format!("{} {ms} {}", keys.id(), hex::encode(keys.signature(msg.as_bytes())))
}

const AUTH: &str = "x-illogical-auth";

impl Control {
    pub fn new(state_dir: &Path, direct_urls: Vec<String>, acl: Arc<Acl>, no_relay: bool) -> Arc<Self> {
        let me = Arc::new(Self {
            state_dir: state_dir.to_owned(),
            acl,
            now: RwLock::new(None),
            changed: watch::channel(0).0,
            direct_urls,
            nudge: tokio::sync::Notify::new(),
            http: reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("http client"),
            published: Default::default(),
            no_relay,
        });
        me.reload();
        me
    }

    pub fn enrolled(&self) -> Option<Arc<Enrolled>> {
        self.now.read().unwrap().clone()
    }

    /// Look again soon (grants changed here, say).
    pub fn poke(&self) {
        self.nudge.notify_one();
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
        self.acl.set_team_roles(e.as_ref().map(|e| e.team_roles.clone()).unwrap_or_default());
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

    async fn get<T: serde::de::DeserializeOwned>(&self, e: &Enrolled, path_and_query: &str) -> anyhow::Result<T> {
        let path = path_and_query.split('?').next().unwrap_or(path_and_query);
        let res = self
            .http
            .get(format!("{}{path_and_query}", e.saved.url))
            .header(AUTH, auth_header(&e.keys, "GET", path))
            .send()
            .await?;
        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!(
                "control doesn't know this daemon any more ({}); run `illogicald join` again",
                res.text().await.unwrap_or_default()
            );
        }
        Ok(res.error_for_status()?.json().await?)
    }

    /// Fetch certificates (the account's, the team's, people's shared
    /// with); keep what checks out against what this daemon pinned.
    async fn refresh(&self) -> anyhow::Result<bool> {
        let Some(e) = self.enrolled() else { return Ok(false) };
        #[derive(Deserialize)]
        struct Own {
            certs: Vec<Cert>,
            revocations: Vec<Revocation>,
        }
        let own: Own = self.get(&e, "/api/daemon/trust").await?;
        let mut saved = e.saved.clone();
        saved.certs = own.certs;
        saved.revocations = own.revocations;

        if let Some(pin) = saved.team.clone() {
            #[derive(Deserialize)]
            struct TeamNow {
                locked: bool,
                rosters: Vec<Roster>,
                certs: AccountCerts,
            }
            let since = saved.roster.as_ref().map_or(0, |r| r.version);
            let t: TeamNow = self.get(&e, &format!("/api/daemon/team?since={since}")).await?;
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
            saved.roster = cur;
            saved.team_certs = certs;
            saved.locked = t.locked;
        }

        let accounts: Vec<String> =
            self.acl.list().iter().filter_map(|g| g.principal.strip_prefix("account:").map(str::to_owned)).collect();
        saved.peers = if accounts.is_empty() {
            BTreeMap::new()
        } else {
            self.get(&e, &format!("/api/daemon/peers?accounts={}", accounts.join(","))).await?
        };

        let changed =
            (saved.certs.clone(), saved.revocations.clone(), saved.roster.clone(), saved.locked, saved.peers.clone())
                != (
                    e.saved.certs.clone(),
                    e.saved.revocations.clone(),
                    e.saved.roster.clone(),
                    e.saved.locked,
                    e.saved.peers.clone(),
                )
                || saved.team_certs != e.saved.team_certs;
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
                let path = "/api/daemon/push";
                let req = serde_json::json!({ "endpoint": s.endpoint, "body": base64::engine::general_purpose::STANDARD.encode(body) });
                let res = me
                    .http
                    .post(format!("{}{path}", e.saved.url))
                    .header(AUTH, auth_header(&e.keys, "POST", path))
                    .json(&req)
                    .send()
                    .await;
                if let Err(err) = res {
                    warn!(error = %err, "can't push through control");
                }
            }
        });
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
                .header(AUTH, auth_header(&e.keys, "POST", path))
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
        let path = "/api/daemon/access";
        let res = self
            .http
            .post(format!("{}{path}", e.saved.url))
            .header(AUTH, auth_header(&e.keys, "POST", path))
            .json(&body)
            .send()
            .await;
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
    let ws = crate::dial::open_ws(&url, &[(AUTH, &auth_header(&e.keys, "GET", path))]).await?;
    info!(control = e.saved.url, "connected to control's relay");
    crate::dial::serve_mux(ws, accept, Some(&control.nudge)).await
}

// ---------------------------------------------------------------- join

#[derive(Deserialize)]
struct JoinStarted {
    code: String,
    poll: String,
    expires_in_secs: u64,
}

#[derive(Deserialize)]
struct JoinPoll {
    approved: bool,
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
}

/// `illogicald join URL [--team ID]`: ask, show the code, wait, pin, save.
/// A hosted sandbox (M20) joins with the `ticket` control gave it.
pub async fn join(
    url: &str,
    name: &str,
    team: Option<&str>,
    ticket: Option<&str>,
    state_dir: &Path,
) -> anyhow::Result<()> {
    let url = url.trim_end_matches('/').to_owned();
    if !url.starts_with("https://") && !private_http(&url) {
        bail!("control's URL must be https:// (or http on loopback or a private network, for testing)");
    }
    if let Some(s) = read_saved(state_dir)? {
        bail!("already joined to {} (account {}); `illogicald leave` first", s.url, s.trust.account);
    }
    let keys = DeviceKeys::load_or_create(&state_dir.join(KEY_FILE))?;
    let ask = Cert { account: String::new(), ..Cert::new(&keys, "", Kind::Daemon, name) };
    let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let res = http
        .post(format!("{url}/api/join"))
        .json(&serde_json::json!({ "cert": ask, "urls": [], "team": team, "ticket": ticket }))
        .send()
        .await?;
    if !res.status().is_success() {
        bail!("control said {}: {}", res.status(), res.text().await.unwrap_or_default());
    }
    let started: JoinStarted = res.json().await?;
    debug_assert_eq!(started.code, join_code(&ask));
    println!();
    println!("  To add this machine ({name}) to your account, open");
    println!();
    println!("    {url}/#join={}", started.code);
    println!();
    println!("  on a device that's signed in, and check the code there is {}.", started.code);
    println!();
    let deadline = std::time::Instant::now() + Duration::from_secs(started.expires_in_secs);
    let got = loop {
        if std::time::Instant::now() > deadline {
            bail!("nobody approved it in time; run join again");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let res = http.get(format!("{url}/api/join/{}?poll={}", started.code, started.poll)).send().await;
        let Ok(res) = res else { continue };
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("the code expired; run join again");
        }
        let Ok(p) = res.error_for_status()?.json::<JoinPoll>().await else { continue };
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
    let approver = trusted.get(&cert.approver).map(|c| c.name.clone()).unwrap_or_default();
    let pin = got.team.as_ref().map(|t| TeamPin {
        team: t.team.clone(),
        founder: t.founder.clone(),
        founder_root: t.founder_root.clone(),
    });
    if team.is_some() && pin.is_none() {
        bail!("control approved it, but not as the team's machine; not joining");
    }
    let saved = Saved {
        url,
        trust: trust.clone(),
        cert: cert.clone(),
        certs: all,
        revocations: got.revocations,
        team: pin.clone(),
        roster: None,
        team_certs: Default::default(),
        locked: false,
        peers: Default::default(),
    };
    write_saved(state_dir, &saved)?;
    println!("  Joined. Approved by \"{approver}\"; the account's first device is {}.", fingerprint(&trust.root));
    if let (Some(t), Some(p)) = (&got.team, &pin) {
        println!("  It belongs to the team {} (founded by the device {}).", t.name, fingerprint(&p.founder_root));
    }
    println!("  This machine is {} ({}).", fingerprint(&cert.device), cert.name);
    println!("  A running daemon picks this up within a few seconds.");
    Ok(())
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

/// `illogicald leave`: tell control, forget it.
pub async fn leave(state_dir: &Path) -> anyhow::Result<()> {
    let Some(s) = read_saved(state_dir)? else { bail!("not joined to any control") };
    let keys = DeviceKeys::load(&state_dir.join(KEY_FILE))?;
    let path = "/api/daemon/leave";
    let res = reqwest::Client::new()
        .post(format!("{}{path}", s.url))
        .header(AUTH, auth_header(&keys, "POST", path))
        .send()
        .await;
    match res {
        Ok(r) if r.status().is_success() => println!("Left {} (account {}).", s.url, s.trust.account),
        Ok(r) => println!("control said {} (leaving anyway)", r.status()),
        Err(e) => println!("couldn't reach control ({e}); leaving anyway"),
    }
    std::fs::remove_file(state_dir.join(FILE))?;
    Ok(())
}
