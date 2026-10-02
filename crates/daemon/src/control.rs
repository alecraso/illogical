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
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, bail};
use illogical_e2e::{
    Cert, DeviceKeys, Kind, Revocation, Trust,
    cert::{Trusted, join_code},
    keys::fingerprint,
    now_ms,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::server::App;

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
}

pub struct Enrolled {
    pub saved: Saved,
    pub keys: Arc<DeviceKeys>,
    pub trusted: Trusted,
}

/// This daemon's standing with control, shared with the channel handlers.
pub struct Control {
    state_dir: PathBuf,
    now: RwLock<Option<Arc<Enrolled>>>,
    /// Bumped whenever the trusted set changes: open channels re-check
    /// their device and close if it's gone.
    pub changed: watch::Sender<u64>,
    /// Direct URLs to give the directory.
    pub direct_urls: Vec<String>,
    http: reqwest::Client,
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
    pub fn new(state_dir: &Path, direct_urls: Vec<String>) -> Arc<Self> {
        let me = Arc::new(Self {
            state_dir: state_dir.to_owned(),
            now: RwLock::new(None),
            changed: watch::channel(0).0,
            direct_urls,
            http: reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("http client"),
        });
        me.reload();
        me
    }

    pub fn enrolled(&self) -> Option<Arc<Enrolled>> {
        self.now.read().unwrap().clone()
    }

    /// The device behind a Noise key, if this daemon trusts it to connect.
    pub fn device(&self, noise: &[u8]) -> Option<Cert> {
        self.enrolled()?.trusted.by_noise(noise).filter(|c| c.kind.connects()).cloned()
    }

    /// Read `control.json` (and the key) again.
    fn reload(&self) {
        let next = match read_saved(&self.state_dir).and_then(|s| {
            let Some(saved) = s else { return Ok(None) };
            let keys = DeviceKeys::load(&self.state_dir.join(KEY_FILE))?;
            let trusted = saved.trust.evaluate(&saved.certs, &saved.revocations);
            Ok(Some(Enrolled { saved, keys: Arc::new(keys), trusted }))
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
                "enrolled in control"
            );
        }
        *self.now.write().unwrap() = next.map(Arc::new);
        self.changed.send_modify(|v| *v += 1);
    }

    /// Fetch the account's certificates; keep what checks out.
    async fn refresh(&self) -> anyhow::Result<()> {
        let Some(e) = self.enrolled() else { return Ok(()) };
        let path = "/api/daemon/trust";
        let res = self
            .http
            .get(format!("{}{path}", e.saved.url))
            .header(AUTH, auth_header(&e.keys, "GET", path))
            .send()
            .await?;
        if res.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!(
                "control doesn't know this daemon any more ({}); run `illogicald join` again",
                res.text().await.unwrap_or_default()
            );
        }
        #[derive(Deserialize)]
        struct Body {
            certs: Vec<Cert>,
            revocations: Vec<Revocation>,
        }
        let b: Body = res.error_for_status()?.json().await?;
        let mut saved = e.saved.clone();
        if (saved.certs.clone(), saved.revocations.clone()) == (b.certs.clone(), b.revocations.clone()) {
            return Ok(());
        }
        saved.certs = b.certs;
        saved.revocations = b.revocations;
        let trusted = saved.trust.evaluate(&saved.certs, &saved.revocations);
        if trusted.get(&saved.cert.device).is_none() {
            warn!("this daemon's own certificate no longer checks out (revoked?)");
        }
        write_saved(&self.state_dir, &saved)?;
        info!(devices = trusted.devices.len(), "account certificates updated");
        *self.now.write().unwrap() = Some(Arc::new(Enrolled { saved, keys: e.keys.clone(), trusted }));
        self.changed.send_modify(|v| *v += 1);
        Ok(())
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
                    if let Some(r) = relay.take() {
                        r.abort();
                    }
                    last_refresh = std::time::Instant::now() - REFRESH;
                }
                if me.enrolled().is_some() {
                    if last_refresh.elapsed() >= REFRESH {
                        last_refresh = std::time::Instant::now();
                        if let Err(e) = me.refresh().await {
                            warn!(error = %e, "can't refresh certificates from control");
                        }
                        stamp = file_stamp(&me.state_dir);
                    }
                    if relay.as_ref().is_none_or(|r| r.is_finished()) {
                        relay = Some(tokio::spawn(keep_relay(me.clone(), app.clone())));
                    }
                } else if let Some(r) = relay.take() {
                    r.abort();
                }
                tokio::time::sleep(WATCH).await;
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
    crate::dial::serve_mux(ws, accept).await
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
    certs: Vec<Cert>,
    #[serde(default)]
    revocations: Vec<Revocation>,
}

/// `illogicald join URL`: ask, show the code, wait, pin, save.
pub async fn join(url: &str, name: &str, state_dir: &Path) -> anyhow::Result<()> {
    let url = url.trim_end_matches('/').to_owned();
    if !(url.starts_with("https://") || url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")) {
        bail!("control's URL must be https:// (or http on loopback, for testing)");
    }
    if let Some(s) = read_saved(state_dir)? {
        bail!("already joined to {} (account {}); `illogicald leave` first", s.url, s.trust.account);
    }
    let keys = DeviceKeys::load_or_create(&state_dir.join(KEY_FILE))?;
    let ask = Cert { account: String::new(), ..Cert::new(&keys, "", Kind::Daemon, name) };
    let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let res = http.post(format!("{url}/api/join")).json(&serde_json::json!({ "cert": ask, "urls": [] })).send().await?;
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
    let saved = Saved { url, trust: trust.clone(), cert: cert.clone(), certs: all, revocations: got.revocations };
    write_saved(state_dir, &saved)?;
    println!("  Joined. Approved by \"{approver}\"; the account's first device is {}.", fingerprint(&trust.root));
    println!("  This machine is {} ({}).", fingerprint(&cert.device), cert.name);
    println!("  A running daemon picks this up within a few seconds.");
    Ok(())
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
