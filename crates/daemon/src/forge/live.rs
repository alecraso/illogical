//! M40: live updates for forge blocks. A *poke* says "something changed
//! on this repository (this PR or issue)": the matching blocks poll at
//! once (conditional requests make that cheap), and while the webhook path
//! is healthy (a poke, a delivery or control's heartbeat within
//! [`quiet_after`]) they poll slowly even while drawn. Then the block's
//! state says `live: webhook`; otherwise `live: polling`.
//!
//! - **GitHub**, through Arugula control's GitHub App: the daemon tells
//!   control which github.com repositories it has blocks on (a text
//!   message on its relay socket, `forge.watch`); control answers where
//!   each stands (`forge.watching`, again every minute: the heartbeat) and
//!   relays the App's webhooks as pokes (`forge.poke`). Hosted boxes with
//!   no `gh` login read through the App's installation tokens
//!   ([`app_token`]), read-only.
//! - **Forgejo and GitLab**, straight to the daemon: `live {on: true}` on a
//!   block (the owner; an agent's call is a draft) creates a repository
//!   webhook with the person's login, pointed at this daemon's tailnet URL
//!   (`POST /api/forge/hooks/forgejo?k=…` or `…/gitlab?k=…`) with a secret
//!   made here and kept in `<state>/secrets/forge-hooks.json` (0600). The
//!   route takes nothing but a delivery signed with that secret (Forgejo's
//!   `X-Forgejo-Signature`/`X-Gitea-Signature`, HMAC-SHA256 of the body;
//!   GitLab's `X-Gitlab-Token`), and keeps of it only the repository and
//!   number.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

use axum::{
    Json,
    body::Bytes,
    extract::Query,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{ForgeBlock, model::Provider};
use crate::control::Control;

/// How long the webhook path counts as healthy after the last thing heard
/// from it (`ARUGULA_FORGE_LIVE_MS` in tests).
pub fn quiet_after() -> Duration {
    static AT: OnceLock<Duration> = OnceLock::new();
    *AT.get_or_init(|| {
        std::env::var("ARUGULA_FORGE_LIVE_MS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(600))
    })
}

/// A repository on a forge: `provider:host/owner/name`, lower case.
pub fn key(provider: Provider, host: &str, repo: &str) -> String {
    let p = match provider {
        Provider::Forgejo => "forgejo",
        Provider::Github => "github",
        Provider::Gitlab => "gitlab",
    };
    format!("{p}:{}/{}", host.to_ascii_lowercase(), repo.to_ascii_lowercase())
}

/// A webhook this daemon made on a forge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRec {
    /// The id in the hook's URL that finds this record.
    pub k: String,
    pub provider: Provider,
    pub host: String,
    pub repo: String,
    pub secret: String,
    /// The forge's id for it, once made.
    #[serde(default)]
    pub id: Option<u64>,
    pub url: String,
    pub created_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct HookFile {
    hooks: Vec<HookRec>,
}

/// What a block shows of it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Standing {
    /// `webhook` or `polling`.
    pub live: &'static str,
    /// Where the pokes come from, when there's a way for them to.
    pub via: Option<&'static str>,
    /// Why it polls (control's word, or none heard yet).
    pub why: Option<String>,
    /// When it last heard from the webhook path.
    pub heard_ms: Option<u64>,
    /// A webhook this daemon made is on the repository.
    pub hook: bool,
}

struct Hub {
    state_dir: PathBuf,
    control: Option<Weak<Control>>,
    urls: Vec<String>,
    blocks: Mutex<Vec<Weak<ForgeBlock>>>,
    heard: Mutex<HashMap<String, (Instant, u64)>>,
    /// Control's word on GitHub repositories (lower case): why not, if not.
    github: Mutex<HashMap<String, Option<String>>>,
    hooks: Mutex<Vec<HookRec>>,
    /// The `forge.watch` message for control, when it changes.
    watch: tokio::sync::watch::Sender<Option<String>>,
}

static HUB: OnceLock<Hub> = OnceLock::new();

fn hub() -> Option<&'static Hub> {
    HUB.get()
}

fn hooks_file(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join("secrets").join("forge-hooks.json")
}

/// At start: where to keep hooks' secrets, control (for GitHub), and the
/// URLs this daemon is reached at (for Forgejo's and GitLab's hooks).
pub fn init(state_dir: PathBuf, control: Option<&Arc<Control>>, urls: Vec<String>) {
    let hooks = std::fs::read(hooks_file(&state_dir))
        .ok()
        .and_then(|b| serde_json::from_slice::<HookFile>(&b).ok())
        .map(|f| f.hooks)
        .unwrap_or_default();
    let _ = HUB.set(Hub {
        state_dir,
        control: control.map(Arc::downgrade),
        urls,
        blocks: Mutex::default(),
        heard: Mutex::default(),
        github: Mutex::default(),
        hooks: Mutex::new(hooks),
        watch: tokio::sync::watch::channel(None).0,
    });
}

/// The `forge.watch` messages for control's socket.
pub fn watch_messages() -> Option<tokio::sync::watch::Receiver<Option<String>>> {
    hub().map(|h| h.watch.subscribe())
}

/// A block opened.
pub fn register(b: Weak<ForgeBlock>) {
    let Some(h) = hub() else { return };
    h.blocks.lock().unwrap().push(b);
    resubscribe();
}

fn blocks() -> Vec<Arc<ForgeBlock>> {
    let Some(h) = hub() else { return vec![] };
    let mut bs = h.blocks.lock().unwrap();
    bs.retain(|b| b.upgrade().is_some_and(|b| !b.is_closed()));
    bs.iter().filter_map(Weak::upgrade).collect()
}

/// The github.com repositories with blocks open, told to control when the
/// set changes.
pub fn resubscribe() {
    let Some(h) = hub() else { return };
    let mut repos: Vec<String> = blocks()
        .iter()
        .filter_map(|b| {
            let (p, host, repo, _) = b.live_key();
            (p == Provider::Github && host == "github.com").then_some(repo)
        })
        .collect();
    repos.sort_by_key(|r| r.to_ascii_lowercase());
    repos.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let msg = json!({ "t": "forge.watch", "repos": repos }).to_string();
    h.watch.send_if_modified(|m| {
        if m.as_deref() == Some(&msg) {
            return false;
        }
        *m = Some(msg);
        true
    });
}

fn hear(k: &str) {
    if let Some(h) = hub() {
        h.heard.lock().unwrap().insert(k.to_owned(), (Instant::now(), crate::store::now_ms()));
    }
}

/// How long the path for `k` stays healthy, if it is now.
pub fn fresh(k: &str) -> Option<Duration> {
    let h = hub()?;
    let (at, _) = *h.heard.lock().unwrap().get(k)?;
    quiet_after().checked_sub(at.elapsed()).filter(|d| !d.is_zero())
}

/// What a block with this key shows.
pub fn standing(provider: Provider, host: &str, repo: &str) -> Standing {
    let k = key(provider, host, repo);
    let heard_ms = hub().and_then(|h| h.heard.lock().unwrap().get(&k).map(|(_, ms)| *ms));
    let live = if fresh(&k).is_some() { "webhook" } else { "polling" };
    let hook = hook_of(provider, host, repo).is_some_and(|r| r.id.is_some());
    let (via, why) = match provider {
        Provider::Github if host == "github.com" => {
            let control = hub().and_then(|h| h.control.as_ref()?.upgrade()).is_some_and(|c| c.enrolled().is_some());
            let said = hub().and_then(|h| h.github.lock().unwrap().get(&repo.to_ascii_lowercase()).cloned());
            let why = match (control, said) {
                (false, _) => Some("not joined to Arugula control, whose GitHub App relays webhooks".to_owned()),
                (true, None) => Some("control hasn't said yet".to_owned()),
                (true, Some(w)) => w,
            };
            (Some("github-app"), why)
        }
        Provider::Github => (None, Some("GitHub Enterprise: no webhooks here".to_owned())),
        Provider::Forgejo | Provider::Gitlab if hook => (Some("hook"), None),
        Provider::Forgejo | Provider::Gitlab => (None, Some("no webhook: Live updates turns one on".to_owned())),
    };
    let why = if live == "webhook" {
        None
    } else {
        why.or_else(|| Some(format!("nothing heard from the webhook in {} s", quiet_after().as_secs())))
    };
    Standing { live, via, why, heard_ms, hook }
}

/// Something changed on a repository: its blocks (that PR or issue, or
/// all of them) poll now.
pub fn poke(provider: Provider, host: &str, repo: &str, number: Option<u64>, event: &str) {
    let k = key(provider, host, repo);
    hear(&k);
    let mut n = 0;
    for b in blocks() {
        let (p, h, r, num) = b.live_key();
        if key(p, &h, &r) == k && number.is_none_or(|x| x == num) {
            b.poked();
            n += 1;
        }
    }
    info!(repo, number, event, blocks = n, "forge poke");
}

/// What control said on the relay socket (besides `trust`).
pub fn from_control(text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return };
    match v["t"].as_str() {
        Some("forge.poke") => {
            let p = &v["poke"];
            let (Some(repo), Some("github")) = (p["repo"].as_str(), p["provider"].as_str()) else { return };
            let host = p["host"].as_str().unwrap_or("github.com");
            poke(Provider::Github, host, repo, p["number"].as_u64(), p["event"].as_str().unwrap_or(""));
        }
        Some("forge.watching") => {
            let Some(h) = hub() else { return };
            for r in v["repos"].as_array().into_iter().flatten() {
                let Some(repo) = r["repo"].as_str() else { continue };
                let live = r["live"] == true;
                let why = (!live).then(|| r["why"].as_str().unwrap_or("control says no").to_owned());
                h.github.lock().unwrap().insert(repo.to_ascii_lowercase(), why);
                let k = key(Provider::Github, "github.com", repo);
                if live {
                    hear(&k);
                } else {
                    h.heard.lock().unwrap().remove(&k);
                }
            }
            for b in blocks() {
                if b.live_key().0 == Provider::Github {
                    b.live_changed();
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- GitHub App tokens

/// Tokens from control's GitHub App, by repository, until they expire.
/// A token, the person's GitHub login, and when to stop using it.
type AppToken = (String, String, Instant);
static APP_TOKENS: std::sync::LazyLock<Mutex<HashMap<String, AppToken>>> = std::sync::LazyLock::new(Mutex::default);

/// A read-only installation token for `repo` from control's GitHub App,
/// and the account's GitHub login: for a daemon with no `gh` login (a
/// hosted box). Held in memory, never past a minute before it expires.
pub async fn app_token(repo: &str, fresh: bool) -> Result<(String, String), String> {
    let k = repo.to_ascii_lowercase();
    if !fresh
        && let Some((t, login, until)) = APP_TOKENS.lock().unwrap().get(&k).cloned()
        && until > Instant::now()
    {
        return Ok((t, login));
    }
    let control = hub()
        .and_then(|h| h.control.as_ref()?.upgrade())
        .ok_or("not joined to Arugula control, whose GitHub App could read it")?;
    let v = control.github_token(repo).await?;
    let token = v["token"].as_str().filter(|t| !t.is_empty()).ok_or("control sent no token")?.to_owned();
    let login = v["login"].as_str().unwrap_or_default().to_owned();
    let left = v["expires_at"]
        .as_str()
        .and_then(epoch_of)
        .map(|e| e.saturating_sub(crate::store::now_ms() / 1000))
        .unwrap_or(600);
    let until = Instant::now() + Duration::from_secs(left.saturating_sub(60));
    APP_TOKENS.lock().unwrap().insert(k, (token.clone(), login.clone(), until));
    Ok((token, login))
}

/// Seconds since the epoch for `2026-10-03T12:34:56Z`.
fn epoch_of(t: &str) -> Option<u64> {
    let (d, rest) = t.split_once('T')?;
    let mut ymd = d.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, dd) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let hms = rest.trim_end_matches('Z').split(['+', '.']).next()?;
    let mut p = hms.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, s) = (p.next()??, p.next()??, p.next()??);
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    u64::try_from((era * 146_097 + doe - 719_468) * 86_400 + h * 3600 + mi * 60 + s).ok()
}

// ---------------------------------------------------------------- hooks to this daemon

fn save_hooks(h: &Hub) -> Result<(), String> {
    let path = hooks_file(&h.state_dir);
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let _ = crate::perm::set(dir, 0o700);
    let f = HookFile { hooks: h.hooks.lock().unwrap().clone() };
    crate::store::write_atomic(&path, &serde_json::to_vec_pretty(&f).unwrap_or_default()).map_err(|e| e.to_string())
}

pub fn hook_of(provider: Provider, host: &str, repo: &str) -> Option<HookRec> {
    let k = key(provider, host, repo);
    hub()?.hooks.lock().unwrap().iter().find(|r| key(r.provider, &r.host, &r.repo) == k).cloned()
}

/// Where a forge reaches this daemon: the tailnet's https URL, else
/// another direct URL (`ARUGULA_FORGE_HOOK_BASE` overrides).
fn base() -> Result<String, String> {
    if let Ok(b) = std::env::var("ARUGULA_FORGE_HOOK_BASE")
        && !b.is_empty()
    {
        return Ok(b.trim_end_matches('/').to_owned());
    }
    let h = hub().ok_or("not ready")?;
    h.urls
        .iter()
        .find(|u| u.starts_with("https://"))
        .or(h.urls.first())
        .map(|u| u.trim_end_matches('/').to_owned())
        .ok_or_else(|| {
            "this daemon has no address a forge can reach: run it on the tailnet (tailscale serve), or give --direct-url"
                .to_owned()
        })
}

/// A new hook's record (not yet made on the forge): its URL and secret.
pub fn new_hook(provider: Provider, host: &str, repo: &str) -> Result<HookRec, String> {
    let k = hex::encode(arugula_e2e::random::<12>());
    let path = if provider == Provider::Gitlab { GITLAB_PATH } else { FORGEJO_PATH };
    Ok(HookRec {
        url: format!("{}{path}?k={k}", base()?),
        k,
        provider,
        host: host.to_owned(),
        repo: repo.to_owned(),
        secret: hex::encode(arugula_e2e::random::<32>()),
        id: None,
        created_ms: crate::store::now_ms(),
    })
}

/// The forge made it (`Some(rec)`), or it's gone (`None` for this key).
pub fn keep_hook(provider: Provider, host: &str, repo: &str, rec: Option<HookRec>) -> Result<(), String> {
    let h = hub().ok_or("not ready")?;
    let k = key(provider, host, repo);
    {
        let mut hooks = h.hooks.lock().unwrap();
        hooks.retain(|r| key(r.provider, &r.host, &r.repo) != k);
        if let Some(r) = rec {
            hooks.push(r);
            // Just made: as good as heard.
            drop(hooks);
            hear(&k);
        } else {
            drop(hooks);
            h.heard.lock().unwrap().remove(&k);
        }
    }
    save_hooks(h)
}

pub const FORGEJO_PATH: &str = "/api/forge/hooks/forgejo";
pub const GITLAB_PATH: &str = "/api/forge/hooks/gitlab";

#[derive(Deserialize)]
pub struct HookQuery {
    #[serde(default)]
    k: String,
}

fn rec_by(k: &str, provider: Provider) -> Option<HookRec> {
    hub()?.hooks.lock().unwrap().iter().find(|r| r.k == k && r.provider == provider && !k.is_empty()).cloned()
}

fn eq_ct(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |x, (p, q)| x | (p ^ q)) == 0
}

/// Forgejo's signature: hex HMAC-SHA256 of the body.
pub fn forgejo_signed(secret: &str, sig: Option<&str>, body: &[u8]) -> bool {
    use hmac::{KeyInit, Mac};
    let Some(sig) = sig.and_then(|s| hex::decode(s.trim().trim_start_matches("sha256=")).ok()) else { return false };
    let Ok(mut mac) = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) else { return false };
    mac.update(body);
    mac.verify_slice(&sig).is_ok()
}

fn refused() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({ "error": "bad signature" }))).into_response()
}

/// `POST /api/forge/hooks/forgejo?k=…`: a Forgejo webhook, signed.
pub async fn forgejo_hook(Query(q): Query<HookQuery>, headers: HeaderMap, body: Bytes) -> Response {
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    let Some(rec) = rec_by(&q.k, Provider::Forgejo) else { return refused() };
    let sig = h("x-forgejo-signature").or(h("x-gitea-signature"));
    if !forgejo_signed(&rec.secret, sig, &body) {
        warn!(repo = rec.repo, "a Forgejo webhook with a bad or missing signature");
        return refused();
    }
    let event = h("x-forgejo-event").or(h("x-gitea-event")).unwrap_or_default().to_owned();
    let v: Value = serde_json::from_slice(&body).unwrap_or_default();
    if !v["repository"]["full_name"].as_str().is_some_and(|r| r.eq_ignore_ascii_case(&rec.repo)) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "not this hook's repository" }))).into_response();
    }
    let number = v["pull_request"]["number"].as_u64().or(v["issue"]["number"].as_u64());
    poke(Provider::Forgejo, &rec.host, &rec.repo, number, &event);
    Json(json!({ "ok": true })).into_response()
}

/// `POST /api/forge/hooks/gitlab?k=…`: a GitLab webhook, with its token.
pub async fn gitlab_hook(Query(q): Query<HookQuery>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(rec) = rec_by(&q.k, Provider::Gitlab) else { return refused() };
    let token = headers.get("x-gitlab-token").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !eq_ct(token.as_bytes(), rec.secret.as_bytes()) {
        warn!(repo = rec.repo, "a GitLab webhook with a bad or missing token");
        return refused();
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or_default();
    if !v["project"]["path_with_namespace"].as_str().is_some_and(|r| r.eq_ignore_ascii_case(&rec.repo)) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "not this hook's project" }))).into_response();
    }
    let kind = v["object_kind"].as_str().unwrap_or_default();
    let number = match kind {
        "merge_request" => v["object_attributes"]["iid"].as_u64(),
        "note" | "pipeline" => v["merge_request"]["iid"].as_u64(),
        _ => None,
    };
    if kind == "issue" || (kind == "note" && number.is_none()) {
        // Issues number apart from merge requests on GitLab: heard, no poll.
        hear(&key(Provider::Gitlab, &rec.host, &rec.repo));
    } else {
        poke(Provider::Gitlab, &rec.host, &rec.repo, number, kind);
    }
    Json(json!({ "ok": true })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_and_keys() {
        use hmac::{KeyInit, Mac};
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"s3cret").unwrap();
        mac.update(b"{}");
        let sig = hex::encode(mac.finalize().into_bytes());
        assert!(forgejo_signed("s3cret", Some(&sig), b"{}"));
        assert!(!forgejo_signed("other", Some(&sig), b"{}"));
        assert!(!forgejo_signed("s3cret", Some(&sig), b"{ }"));
        assert!(!forgejo_signed("s3cret", None, b"{}"));
        assert!(!forgejo_signed("s3cret", Some("nothex"), b"{}"));
        assert!(eq_ct(b"abc", b"abc") && !eq_ct(b"abc", b"abd") && !eq_ct(b"abc", b"ab"));
        assert_eq!(key(Provider::Github, "GitHub.com", "Cli/CLI"), "github:github.com/cli/cli");
        assert_eq!(epoch_of("2026-10-03T12:00:00Z"), Some(1_791_028_800));
    }
}
