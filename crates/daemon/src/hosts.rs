//! The host list (M4a): other daemons a client can switch to. The home
//! daemon keeps it in `hosts.json`; clients fetch it and then talk to each
//! host directly, so terminal bytes never pass through here.
//!
//! A host gets on the list by being added (`illogical hosts add`, by the
//! owner), by joining with a one-time invite token (a sandbox installing
//! itself, which has no user identity to be checked), or by being made
//! resident in a provider's sandbox (M4b, `resident.rs`). The home daemon
//! checks on each host every minute and records when it last answered; a
//! provider host's sandbox is asked about through its provider instead,
//! which doesn't wake it.
//!
//! Hosts without tailnet identity carry tokens, one kind per direction:
//!
//! - **Host tokens** (`ilh_…`, M4c), the host → us: minted here
//!   (`illogical hosts token NAME`, or by joining as `dial_out`), kept only
//!   as a hash in `host-tokens.json`. They let that one host dial in
//!   (`dial.rs`) and push its history (`sync.rs`), and
//!   nothing else.
//! - **Provider tunnel tokens** (`ilp_…`, M4b), us → the host: minted when
//!   a daemon is made resident in a sandbox, kept here in
//!   `provider-tokens.json` (we present them; the host keeps the hash), and
//!   sent by the provider tunnel (`provider_tunnel.rs`) to reach it.
//!
//! Neither is ever in the list clients get. `hosts revoke` and `hosts rm`
//! drop whatever a host has of both.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use illogical_proto::hosts::{
    AddHost, Host, HostFeatures, HostInfo, HostList, HostToken, Invite, JoinRequest, Joined, ProviderRef, Transport,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::{
    provider::Provider,
    server::App,
    store::{now_ms, write_atomic},
};

const PROBE_EVERY: Duration = Duration::from_secs(60);
const DEFAULT_INVITE_TTL_SECS: u64 = 3600;

pub struct Hosts {
    /// This daemon's own name.
    name: String,
    path: PathBuf,
    invites_path: PathBuf,
    provider_tokens_path: PathBuf,
    tokens_path: PathBuf,
    inner: Mutex<Saved>,
    http: reqwest::Client,
    /// Where provider hosts' sandboxes are asked about.
    provider: Option<Arc<dyn Provider>>,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    hosts: Vec<Host>,
    /// Outstanding invites: SHA-256 of the token, and when it expires.
    #[serde(skip)]
    invites: Vec<(String, u64)>,
    /// Provider hosts' tunnel tokens, by host name.
    #[serde(skip)]
    provider_tokens: std::collections::BTreeMap<String, String>,
    #[serde(skip)]
    tokens: Vec<SavedToken>,
}

/// A per-host token, as kept: its SHA-256 only.
#[derive(Clone, Serialize, Deserialize)]
struct SavedToken {
    name: String,
    digest: String,
    created_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct SavedTokens {
    tokens: Vec<SavedToken>,
}

#[derive(Default, Serialize, Deserialize)]
struct SavedInvites {
    invites: Vec<(String, u64)>,
}

impl Hosts {
    pub fn open(state_dir: &std::path::Path, name: String, provider: Option<Arc<dyn Provider>>) -> Arc<Self> {
        let path = state_dir.join("hosts.json");
        let invites_path = state_dir.join("invites.json");
        let provider_tokens_path = state_dir.join("provider-tokens.json");
        let tokens_path = state_dir.join("host-tokens.json");
        let mut saved: Saved =
            std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        saved.invites = std::fs::read(&invites_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SavedInvites>(&b).ok())
            .map(|s| s.invites)
            .unwrap_or_default();
        saved.provider_tokens =
            std::fs::read(&provider_tokens_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        saved.tokens = std::fs::read(&tokens_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SavedTokens>(&b).ok())
            .map(|s| s.tokens)
            .unwrap_or_default();
        let http =
            crate::roots::http().timeout(Duration::from_secs(5)).build().expect("an HTTP client with default settings");
        Arc::new(Self {
            name,
            path,
            invites_path,
            provider_tokens_path,
            tokens_path,
            inner: Mutex::new(saved),
            http,
            provider,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn list(&self) -> HostList {
        HostList { this: self.name.clone(), hosts: self.inner.lock().unwrap().hosts.clone() }
    }

    /// Add a host, or replace the one with the same name (keeping when it
    /// was first added and last seen, if its URLs are the same).
    pub fn add(&self, req: AddHost) -> Result<Host, String> {
        if req.transport == Transport::Provider {
            return Err("a provider host is added by making a daemon resident in its sandbox".into());
        }
        self.insert(validate(req, &self.name)?, None)
    }

    /// Add (or replace) a host whose daemon lives in a provider's sandbox,
    /// reached through our tunnel with `token`.
    pub fn add_provider(&self, name: String, at: ProviderRef, token: String) -> Result<Host, String> {
        let req = validate(AddHost { name, urls: vec![], transport: Transport::Provider, ssh: None }, &self.name)?;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.provider_tokens.insert(req.name.clone(), token);
            self.save_provider_tokens(&inner);
        }
        self.insert(req, Some(at))
    }

    /// A provider host: where it is, and the token its daemon wants.
    pub fn provider_tunnel(&self, name: &str) -> Option<(ProviderRef, String)> {
        let inner = self.inner.lock().unwrap();
        let at = inner.hosts.iter().find(|h| h.name == name)?.provider.clone()?;
        Some((at, inner.provider_tokens.get(name)?.clone()))
    }

    fn insert(&self, req: AddHost, provider: Option<ProviderRef>) -> Result<Host, String> {
        let mut inner = self.inner.lock().unwrap();
        let old = inner.hosts.iter().position(|h| h.name == req.name).map(|i| inner.hosts.remove(i));
        let host = Host {
            added_ms: old.as_ref().map_or_else(now_ms, |o| o.added_ms),
            last_seen_ms: old.as_ref().filter(|o| o.urls == req.urls).and_then(|o| o.last_seen_ms),
            status: old.as_ref().filter(|o| o.provider == provider).and_then(|o| o.status.clone()),
            name: req.name,
            urls: req.urls,
            transport: req.transport,
            provider,
            ssh: req.ssh,
        };
        inner.hosts.push(host.clone());
        inner.hosts.sort_by(|a, b| a.name.cmp(&b.name));
        self.save(&inner);
        info!(name = host.name, urls = ?host.urls, "host added");
        Ok(host)
    }

    /// Remove a host, and revoke its tokens: a removed host can't dial back
    /// in, nor be reached through a provider tunnel.
    pub fn remove(&self, name: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.hosts.len();
        inner.hosts.retain(|h| h.name != name);
        let gone = inner.hosts.len() != before;
        if gone {
            self.save(&inner);
            if inner.provider_tokens.remove(name).is_some() {
                self.save_provider_tokens(&inner);
            }
            info!(name, "host removed");
        }
        let had_token = inner.tokens.iter().any(|t| t.name == name);
        inner.tokens.retain(|t| t.name != name);
        if had_token {
            self.save_tokens(&inner);
        }
        gone
    }

    /// Mint `name`'s token, replacing any it had. A host that isn't on the
    /// list yet goes on it as a dial-out host (it has no URL to add).
    pub fn mint_token(&self, name: &str) -> Result<HostToken, String> {
        let exists = self.inner.lock().unwrap().hosts.iter().any(|h| h.name == name);
        if !exists {
            self.add(AddHost { name: name.to_owned(), urls: vec![], transport: Transport::DialOut, ssh: None })?;
        }
        let token = format!("ilh_{}", hex(&random::<24>()));
        let mut inner = self.inner.lock().unwrap();
        inner.tokens.retain(|t| t.name != name);
        inner.tokens.push(SavedToken { name: name.to_owned(), digest: digest(&token), created_ms: now_ms() });
        self.save_tokens(&inner);
        info!(name, "host token minted");
        Ok(HostToken { name: name.to_owned(), token })
    }

    /// Revoke `name`'s tokens: its host token, and the provider tunnel
    /// token we reach it with (a provider host stays listed, unreachable
    /// until it's made resident again). Whether it had either.
    pub fn revoke_token(&self, name: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.tokens.len();
        inner.tokens.retain(|t| t.name != name);
        let had = inner.tokens.len() != before;
        if had {
            self.save_tokens(&inner);
            info!(name, "host token revoked");
        }
        let had_tunnel = inner.provider_tokens.remove(name).is_some();
        if had_tunnel {
            self.save_provider_tokens(&inner);
            info!(name, "provider tunnel token revoked");
        }
        had || had_tunnel
    }

    /// The host a token belongs to, if it is current.
    pub fn host_for_token(&self, token: &str) -> Option<String> {
        let want = digest(token);
        let inner = self.inner.lock().unwrap();
        inner.tokens.iter().find(|t| t.digest == want).map(|t| t.name.clone())
    }

    /// A dial-out host is connected: that counts as seeing it.
    pub fn seen(&self, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(h) = inner.hosts.iter_mut().find(|h| h.name == name) {
            h.last_seen_ms = Some(now_ms());
            self.save(&inner);
        }
    }

    /// A one-time token a sandbox can `join` with until it expires.
    pub fn invite(&self, ttl_secs: u64) -> Invite {
        let token = format!("ilj_{}", hex(&random::<24>()));
        let expires_ms = now_ms() + ttl_secs.saturating_mul(1000);
        let mut inner = self.inner.lock().unwrap();
        let now = now_ms();
        inner.invites.retain(|(_, exp)| *exp > now);
        inner.invites.push((digest(&token), expires_ms));
        self.save_invites(&inner);
        Invite { token, expires_ms }
    }

    /// Spend an invite on adding a host; a dial-out host gets its token.
    pub fn join(&self, req: JoinRequest) -> Result<(Host, Option<String>), String> {
        {
            let mut inner = self.inner.lock().unwrap();
            let now = now_ms();
            inner.invites.retain(|(_, exp)| *exp > now);
            let want = digest(&req.token);
            let i = inner.invites.iter().position(|(d, _)| *d == want).ok_or("invalid or expired invite")?;
            // Spent even if the host turns out to be bad: one try per token.
            inner.invites.remove(i);
            self.save_invites(&inner);
        }
        // An ssh host names where clients ssh to: only the owner adds those.
        if req.host.transport == Transport::Ssh {
            return Err("an invite adds a daemon, not an ssh host".into());
        }
        let dial_out = req.host.transport == Transport::DialOut;
        let host = self.add(req.host)?;
        let token = if dial_out { Some(self.mint_token(&host.name)?.token) } else { None };
        Ok((host, token))
    }

    fn save(&self, inner: &Saved) {
        let bytes = serde_json::to_vec_pretty(inner).expect("serialize hosts");
        if let Err(e) = write_atomic(&self.path, &bytes) {
            warn!(error = %e, "can't save the host list");
        }
    }

    fn save_provider_tokens(&self, inner: &Saved) {
        let bytes = serde_json::to_vec(&inner.provider_tokens).expect("serialize");
        if let Err(e) = write_atomic(&self.provider_tokens_path, &bytes) {
            warn!(error = %e, "can't save provider tunnel tokens");
        }
    }

    fn save_tokens(&self, inner: &Saved) {
        let bytes = serde_json::to_vec_pretty(&SavedTokens { tokens: inner.tokens.clone() }).expect("serialize");
        if let Err(e) = write_atomic(&self.tokens_path, &bytes) {
            warn!(error = %e, "can't save host tokens");
        }
    }

    fn save_invites(&self, inner: &Saved) {
        let bytes = serde_json::to_vec(&SavedInvites { invites: inner.invites.clone() }).expect("serialize");
        if let Err(e) = write_atomic(&self.invites_path, &bytes) {
            warn!(error = %e, "can't save invites");
        }
    }

    /// Ask every host who it is; note the ones that answer. A provider
    /// host's sandbox is asked about through its provider instead, which
    /// doesn't wake it (connecting would, and keep it awake).
    pub async fn probe(&self) {
        self.ask_providers().await;
        // Dial-out hosts have no URL: their connection says they're there
        // (`dial.rs` marks them seen).
        let targets: Vec<(String, Vec<String>)> = self
            .inner
            .lock()
            .unwrap()
            .hosts
            .iter()
            // Provider hosts are asked about through their provider.
            .filter(|h| h.transport == Transport::Tailnet)
            .map(|h| (h.name.clone(), h.urls.clone()))
            .collect();
        self.probe_urls(targets).await;
    }

    /// What provider hosts' sandboxes are doing, from their provider.
    pub async fn ask_providers(&self) {
        let sandboxes: Vec<(String, ProviderRef)> = self
            .inner
            .lock()
            .unwrap()
            .hosts
            .iter()
            .filter_map(|h| Some((h.name.clone(), h.provider.clone()?)))
            .collect();
        for (name, at) in sandboxes {
            let status = match &self.provider {
                Some(p) if p.name() == at.provider => match p.status(&at.sandbox).await {
                    Ok(Some(s)) => s.status,
                    Ok(None) => "gone".into(),
                    Err(e) => {
                        info!(host = name, error = %e, "can't ask the provider about a host");
                        continue;
                    }
                },
                _ => "unknown".into(),
            };
            self.note_status(&name, &status);
        }
    }

    async fn probe_urls(&self, targets: Vec<(String, Vec<String>)>) {
        for (name, urls) in targets {
            let mut seen = false;
            for url in &urls {
                // A daemon that wants a credential we don't show it (one on
                // this machine's loopback, which wants its local token) is
                // there all the same.
                let ok = self
                    .http
                    .get(format!("{url}/api/host"))
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success() || r.status() == reqwest::StatusCode::UNAUTHORIZED);
                if ok {
                    seen = true;
                    break;
                }
            }
            if seen {
                let mut inner = self.inner.lock().unwrap();
                if let Some(h) = inner.hosts.iter_mut().find(|h| h.name == name && h.urls == urls) {
                    h.last_seen_ms = Some(now_ms());
                    self.save(&inner);
                }
            }
        }
    }

    /// What the provider says of a provider host's sandbox (and, if it's
    /// running, that it was seen).
    pub fn note_status(&self, name: &str, status: &str) {
        let mut inner = self.inner.lock().unwrap();
        let Some(h) = inner.hosts.iter_mut().find(|h| h.name == name) else { return };
        let seen = (status == "running").then(now_ms);
        if h.status.as_deref() == Some(status) && seen.is_none() {
            return;
        }
        h.status = Some(status.to_owned());
        h.last_seen_ms = seen.or(h.last_seen_ms);
        self.save(&inner);
    }

    pub fn spawn_probe(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(PROBE_EVERY);
            loop {
                tick.tick().await;
                let Some(me) = me.upgrade() else { return };
                me.probe().await;
            }
        });
    }
}

/// Names are what `--host` and the client's switcher show; URLs must be
/// plain origins (a client appends `/ws` and `/api/...`).
fn validate(mut req: AddHost, this: &str) -> Result<AddHost, String> {
    let name = req.name.trim();
    // Names are directory names too (synced history), so not `.` or `..`.
    if name.is_empty()
        || name.len() > 63
        || name.starts_with('.')
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(format!("bad host name {name:?}: letters, digits, '-', '_' and '.' (not first)"));
    }
    if name == this {
        return Err(format!("{name} is this daemon's own name"));
    }
    req.name = name.to_owned();
    match req.transport {
        Transport::Tailnet if req.urls.is_empty() => return Err("a host needs at least one URL".into()),
        // Reached through the home daemon, never at a URL of its own.
        Transport::DialOut if !req.urls.is_empty() => return Err("a dial-out host has no URLs".into()),
        // Each client runs ssh to it; we keep its destination and nothing
        // else, and it must never read as one of ssh's options.
        Transport::Ssh => {
            if !req.urls.is_empty() {
                return Err("an ssh host has no URLs".into());
            }
            let dest = req.ssh.as_deref().map(str::trim).unwrap_or_default();
            if dest.is_empty()
                || dest.starts_with('-')
                || !dest.bytes().all(|b| b.is_ascii_alphanumeric() || b"@._-:[]%+/".contains(&b))
            {
                return Err(format!("bad ssh destination {dest:?}: want user@host or a Host from ~/.ssh/config"));
            }
            req.ssh = Some(dest.to_owned());
        }
        // Reached through the home daemon's provider tunnel; URLs are
        // optional (tailnet ones to upgrade to).
        _ => {}
    }
    for url in &mut req.urls {
        let parsed = reqwest::Url::parse(url.trim()).map_err(|e| format!("bad URL {url:?}: {e}"))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(format!("bad URL {url:?}: want http(s)://host[:port]"));
        }
        if parsed.path() != "/" || parsed.query().is_some() || !parsed.username().is_empty() {
            return Err(format!("bad URL {url:?}: just the origin, without a path"));
        }
        *url = parsed.origin().ascii_serialization();
    }
    Ok(req)
}

fn random<const N: usize>() -> [u8; N] {
    crate::push::random()
}

pub fn digest(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---------------------------------------------------------------- routes

type AppState = State<Arc<App>>;

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/api/host", get(host))
        .route("/api/hosts", get(list).post(add))
        .route("/api/hosts/invite", post(invite))
        .route(JOIN_PATH, post(join))
        .route("/api/hosts/{name}", delete(remove))
        .route("/api/hosts/{name}/token", post(mint_token).delete(revoke_token))
}

/// The one route a caller without a user identity may reach (the token in
/// the body is the credential); see `server::guard`.
pub const JOIN_PATH: &str = "/api/hosts/join";

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

async fn host(State(app): AppState) -> Json<HostInfo> {
    let joined = app.control.enrolled();
    let saved = joined.as_ref().map(|e| &e.saved);
    Json(HostInfo {
        name: app.hosts.name().to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: Some(illogical_proto::PROTOCOL),
        tailnet_url: app.access.tailnet_url(),
        tailnet_seen: app.tailnet_seen.load(std::sync::atomic::Ordering::Relaxed),
        control: saved.map(|s| s.url.clone()),
        // The team's name once its roster is in, else its id.
        team: saved
            .and_then(|s| s.roster.as_ref().map(|r| r.name.clone()).or_else(|| Some(s.team.as_ref()?.team.clone()))),
        // M45b: only where the runner's unit is; from what was last read.
        fountain_runner: crate::fountain::runner::host_info(&app.mux.shell_env),
        features: Some(features(&app)),
    })
}

/// What this machine is set up for, so the menus offer only that (#180)
/// or say how to turn it on (#171). Cheap: nothing here asks anyone.
///
/// What a stranger doesn't get follows the `labs` file in the state dir:
/// threads and huddles are its alone, and Fountain, studio and VMs need it
/// as well as their own setup.
pub(crate) fn features(app: &App) -> HostFeatures {
    let labs = illogical_proto::hosts::labs(app.control.state_dir());
    HostFeatures {
        labs,
        blocks: crate::sites::get().is_some(),
        vms: labs && app.mux.provider.is_some(),
        fountain: labs && fountain_login_here(&app.mux.shell_env),
        studio: labs && crate::apps::studio::get().and_then(|s| s.url()).is_some(),
        threads: labs,
        calls: labs,
    }
}

/// A Fountain login the catalog would find (`fountain::login`'s order): a
/// key in the daemon's or the shell's environment, or the CLI's
/// credentials file.
fn fountain_login_here(shell_env: &crate::shellenv::ShellEnv) -> bool {
    let shell = shell_env.local_now();
    let var = |k: &str| {
        shell
            .as_ref()
            .and_then(|r| r.get(k).map(str::to_owned))
            .or_else(|| std::env::var(k).ok())
            .filter(|v| !v.is_empty())
    };
    if var("FOUNTAIN_API_KEY").is_some() {
        return true;
    }
    let file = var("ILLOGICAL_FOUNTAIN_CREDENTIALS")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".fountain/credentials")));
    file.is_some_and(|f| f.is_file())
}

async fn list(State(app): AppState) -> Json<HostList> {
    // Sandboxes' states are cheap to ask for (and asking doesn't wake
    // them), so the list a client gets is current.
    let _ = tokio::time::timeout(Duration::from_secs(3), app.hosts.ask_providers()).await;
    Json(app.hosts.list())
}

async fn add(State(app): AppState, Json(req): Json<AddHost>) -> Response {
    match app.hosts.add(req) {
        Ok(h) => {
            let hosts = app.hosts.clone();
            tokio::spawn(async move { hosts.probe().await });
            Json(h).into_response()
        }
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

async fn remove(State(app): AppState, Path(name): Path<String>) -> Response {
    // Its tunnel goes with it.
    app.dial_outs.drop_host(&name);
    if app.hosts.remove(&name) {
        Json(serde_json::json!({})).into_response()
    } else {
        error(StatusCode::NOT_FOUND, format!("no host {name}"))
    }
}

#[derive(Deserialize)]
struct InviteQuery {
    #[serde(default)]
    ttl: Option<u64>,
}

async fn invite(State(app): AppState, Query(q): Query<InviteQuery>) -> Json<Invite> {
    Json(app.hosts.invite(q.ttl.unwrap_or(DEFAULT_INVITE_TTL_SECS)))
}

async fn mint_token(State(app): AppState, Path(name): Path<String>) -> Response {
    match app.hosts.mint_token(&name) {
        // A new token replaces the old one: so does the connection.
        Ok(t) => {
            app.dial_outs.drop_host(&name);
            Json(t).into_response()
        }
        Err(e) => error(StatusCode::BAD_REQUEST, e),
    }
}

async fn revoke_token(State(app): AppState, Path(name): Path<String>) -> Response {
    let had = app.hosts.revoke_token(&name);
    let connected = app.dial_outs.drop_host(&name);
    if had || connected {
        Json(serde_json::json!({})).into_response()
    } else {
        error(StatusCode::NOT_FOUND, format!("{name} has no token"))
    }
}

async fn join(State(app): AppState, Json(req): Json<JoinRequest>) -> Response {
    match app.hosts.join(req) {
        Ok((h, token)) => {
            let hosts = app.hosts.clone();
            tokio::spawn(async move { hosts.probe().await });
            Json(Joined { host: h, owner: app.access.owner().map(str::to_owned), token }).into_response()
        }
        Err(e) => {
            warn!(error = e, "refused a join");
            error(StatusCode::FORBIDDEN, e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("ilg-hosts-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn req(name: &str, url: &str) -> AddHost {
        AddHost { name: name.into(), urls: vec![url.into()], transport: Transport::Tailnet, ssh: None }
    }

    #[test]
    fn add_replace_remove_and_persist() {
        let d = dir();
        let h = Hosts::open(&d, "geek".into(), None);
        h.add(req("box", "https://box.example.ts.net/")).unwrap();
        h.add(req("alpha", "http://127.0.0.1:7691")).unwrap();
        let l = h.list();
        assert_eq!(l.this, "geek");
        assert_eq!(l.hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["alpha", "box"]);
        assert_eq!(l.hosts[1].urls, ["https://box.example.ts.net"], "normalized to an origin");
        h.add(req("box", "https://box2.example.ts.net")).unwrap();
        assert_eq!(h.list().hosts.len(), 2);
        assert!(h.remove("alpha"));
        assert!(!h.remove("alpha"));
        drop(h);
        let again = Hosts::open(&d, "geek".into(), None);
        assert_eq!(again.list().hosts.len(), 1);
        assert_eq!(again.list().hosts[0].urls, ["https://box2.example.ts.net"]);
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn bad_hosts_are_refused() {
        let d = dir();
        let h = Hosts::open(&d, "geek".into(), None);
        assert!(h.add(req("geek", "https://x.example")).is_err(), "our own name");
        assert!(h.add(req("", "https://x.example")).is_err());
        assert!(h.add(req("a b", "https://x.example")).is_err());
        assert!(h.add(req("x", "ftp://x.example")).is_err());
        assert!(h.add(req("x", "https://x.example/path")).is_err());
        assert!(h.add(req("x", "https://user@x.example")).is_err());
        assert!(h.add(req("x", "javascript:alert(1)")).is_err());
        assert!(h.add(AddHost { name: "x".into(), urls: vec![], transport: Transport::Tailnet, ssh: None }).is_err());
        assert!(h.add(req("..", "https://x.example")).is_err(), "a directory name");
        assert!(h.add(req(".x", "https://x.example")).is_err());
        let dial = |urls: Vec<String>| AddHost { name: "d".into(), urls, transport: Transport::DialOut, ssh: None };
        assert!(h.add(dial(vec!["https://x.example".into()])).is_err(), "dial-out hosts have no URL");
        assert!(h.list().hosts.is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn invites_are_single_use_and_survive_a_restart() {
        let d = dir();
        let h = Hosts::open(&d, "geek".into(), None);
        let inv = h.invite(60);
        assert!(inv.token.starts_with("ilj_"));
        let join =
            |h: &Hosts, token: &str| h.join(JoinRequest { token: token.into(), host: req("box", "https://b.x") });
        assert!(join(&h, "ilj_wrong").is_err());
        drop(h);
        let h = Hosts::open(&d, "geek".into(), None);
        assert_eq!(join(&h, &inv.token).unwrap().1, None, "no token for a tailnet host");
        assert!(join(&h, &inv.token).is_err(), "spent");
        let expired = h.invite(0);
        assert!(join(&h, &expired.token).is_err(), "expired");
        // The token itself is never stored.
        let saved = std::fs::read_to_string(d.join("invites.json")).unwrap_or_default();
        assert!(!saved.contains(&expired.token));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn host_tokens_are_hashed_scoped_revocable_and_replaced() {
        let d = dir();
        let h = Hosts::open(&d, "geek".into(), None);
        let t = h.mint_token("sbx").unwrap();
        assert!(t.token.starts_with("ilh_"));
        let entry = h.list().hosts.into_iter().find(|x| x.name == "sbx").unwrap();
        assert_eq!((entry.transport, entry.urls.len()), (Transport::DialOut, 0));
        assert_eq!(h.host_for_token(&t.token).as_deref(), Some("sbx"));
        assert_eq!(h.host_for_token("ilh_nope"), None);
        let other = h.mint_token("other").unwrap();
        assert_eq!(h.host_for_token(&other.token).as_deref(), Some("other"), "each token names one host");
        // Survives a restart; only the hash is on disk.
        drop(h);
        let h = Hosts::open(&d, "geek".into(), None);
        assert_eq!(h.host_for_token(&t.token).as_deref(), Some("sbx"));
        let saved = std::fs::read_to_string(d.join("host-tokens.json")).unwrap();
        assert!(!saved.contains(&t.token) && saved.contains(&digest(&t.token)));
        // Modes are Unix's; Windows has the profile's ACL.
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(d.join("host-tokens.json")).unwrap().permissions()
            ) & 0o777,
            0o600
        );
        // A new one replaces the old.
        let t2 = h.mint_token("sbx").unwrap();
        assert_eq!(h.host_for_token(&t.token), None);
        assert_eq!(h.host_for_token(&t2.token).as_deref(), Some("sbx"));
        assert!(h.revoke_token("sbx"));
        assert!(!h.revoke_token("sbx"));
        assert_eq!(h.host_for_token(&t2.token), None);
        // Removing a host revokes its token.
        assert!(h.remove("other"));
        assert_eq!(h.host_for_token(&other.token), None);
        // Joining as dial-out hands one out.
        let inv = h.invite(60);
        let req = AddHost { name: "joiner".into(), urls: vec![], transport: Transport::DialOut, ssh: None };
        let (_, token) = h.join(JoinRequest { token: inv.token, host: req }).unwrap();
        assert_eq!(h.host_for_token(&token.unwrap()).as_deref(), Some("joiner"));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn revoking_or_removing_a_provider_host_drops_both_kinds_of_token() {
        let d = dir();
        let h = Hosts::open(&d, "geek".into(), None);
        let at = ProviderRef { provider: "wisp".into(), sandbox: "s1".into(), port: 7681 };
        let host = h.add_provider("res".into(), at.clone(), "ilp_x".into()).unwrap();
        assert_eq!(host.transport, Transport::Provider);
        assert!(h.add(AddHost { name: "y".into(), urls: vec![], transport: Transport::Provider, ssh: None }).is_err());
        let ilh = h.mint_token("res").unwrap().token;
        assert_eq!(h.provider_tunnel("res").unwrap().1, "ilp_x");
        // Neither token is in what clients get.
        let listed = serde_json::to_string(&h.list()).unwrap();
        assert!(!listed.contains("ilp_x") && !listed.contains(&ilh));
        assert!(h.revoke_token("res"));
        assert!(h.provider_tunnel("res").is_none() && h.host_for_token(&ilh).is_none());
        assert_eq!(h.list().hosts.len(), 1, "revoking keeps the host");
        h.add_provider("res".into(), at, "ilp_y".into()).unwrap();
        drop(h);
        let h = Hosts::open(&d, "geek".into(), None);
        assert_eq!(h.provider_tunnel("res").unwrap().1, "ilp_y", "kept across a restart");
        assert!(h.remove("res"));
        assert!(h.provider_tunnel("res").is_none());
        assert!(!std::fs::read_to_string(d.join("provider-tokens.json")).unwrap().contains("ilp_y"));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn ssh_hosts_keep_only_their_destination() {
        let d = dir();
        let h = Hosts::open(&d, "geek".into(), None);
        let ssh = |urls: Vec<String>, dest: Option<&str>| AddHost {
            name: "box".into(),
            urls,
            transport: Transport::Ssh,
            ssh: dest.map(String::from),
        };
        let added = h.add(ssh(vec![], Some(" illo@box-bare "))).unwrap();
        assert_eq!(
            (added.transport, added.ssh.as_deref(), added.urls.len()),
            (Transport::Ssh, Some("illo@box-bare"), 0)
        );
        assert!(h.add(ssh(vec!["https://box".into()], Some("box"))).is_err(), "no URLs");
        assert!(h.add(ssh(vec![], None)).is_err(), "needs a destination");
        for bad in ["-oProxyCommand=sh", "box; id", "a b", "$(id)"] {
            assert!(h.add(ssh(vec![], Some(bad))).is_err(), "{bad}");
        }
        std::fs::remove_dir_all(d).unwrap();
    }
}
