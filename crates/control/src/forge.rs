//! M40: forge webhooks through control's GitHub App, and the App's
//! read-only tokens for hosted boxes.
//!
//! - **Webhooks.** GitHub posts the App's events to `POST /github/webhook`.
//!   Each is checked against `X-Hub-Signature-256` (HMAC-SHA256 of the
//!   body with the App's webhook secret, compared in constant time) and
//!   deduplicated by `X-GitHub-Delivery`. What's kept of it is a *poke*:
//!   `{provider: github, host: github.com, repo, number?, event, delivery}`,
//!   one per pull request or issue it names (check runs, suites and
//!   workflow runs through their `pull_requests`; a status, or a run on a
//!   fork's PR, is repo-wide: no number). Nothing else of the payload goes
//!   anywhere, and only the event, delivery, repository and count are logged.
//! - **Subscriptions.** A daemon with GitHub forge blocks open says which
//!   repositories over its relay socket (a text message
//!   `{"t": "forge.watch", "repos": [...]}`); control answers, and every
//!   minute after (the heartbeat), with `{"t": "forge.watching", "repos":
//!   [{repo, live, why?}]}`. Pokes go only to daemons whose account may see
//!   the repository ([`GithubApp::may`]): the account signed in to control
//!   with GitHub, the App is installed where the repository lives, and that
//!   installation is the person's own account, or GitHub says they're a
//!   collaborator on the repository (`collaborators/{login}/permission`
//!   through an installation token, any permission but `none`). Both are
//!   checked by the person's numeric GitHub id, never the login alone (a
//!   login can change hands). Answers are kept ten minutes. Subscriptions
//!   live as long as the socket. A daemon watches at most 200 repositories
//!   and an account 400; `forge.watch` messages and first-time lookups at
//!   GitHub are rate-limited per daemon and per account (limit.rs).
//! - **Hosted boxes.** `POST /api/daemon/github/token {repo}` (a daemon's
//!   signature): for an account the same rule allows, an installation token
//!   scoped to that one repository with read-only permissions, minted with
//!   a JWT signed by the App's private key (RS256, `iat` a minute back,
//!   `exp` nine minutes on). Control keeps a token until five minutes
//!   before GitHub's `expires_at`, never longer.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use aws_lc_rs::{rand::SystemRandom, rsa::KeyPair, signature::RSA_PKCS1_SHA256};
use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use tracing::{info, warn};

use crate::{ApiError, App, auth::DaemonAuth, err};

/// How long an answer about an installation or a person's access holds.
const KEEP: Duration = Duration::from_secs(600);
/// Repositories one daemon may watch.
const MAX_REPOS: usize = 200;
/// Repositories one account's daemons may watch, together.
const MAX_ACCOUNT_REPOS: usize = 400;
/// Deliveries remembered for deduplication.
const DELIVERIES: usize = 10_000;

/// What a hosted box's token may do: read.
fn read_only() -> Value {
    json!({
        "metadata": "read", "contents": "read", "pull_requests": "read", "issues": "read",
        "checks": "read", "statuses": "read", "actions": "read",
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub id: u64,
    /// The account it's installed on (a user or an organization).
    pub account: String,
    /// That account's numeric id.
    pub account_id: u64,
}

/// A person by their GitHub identity: the numeric id is who they are, the
/// login what control last saw them called.
#[derive(Debug, Clone)]
pub struct GithubUser {
    pub id: u64,
    pub login: String,
}

/// When a person's access was looked at, and the answer.
type Access = (Instant, Result<Install, String>);
/// An installation token, GitHub's `expires_at`, and that in seconds.
type Minted = (String, String, u64);

pub struct GithubApp {
    pub id: String,
    pub slug: String,
    pub webhook_secret: String,
    /// The REST API's base (`https://api.github.com`; tests use a fake).
    pub api: String,
    key: KeyPair,
    installs: Mutex<HashMap<String, (Instant, Option<Install>)>>,
    access: Mutex<HashMap<(u64, String), Access>>,
    /// Installation tokens by (installation, repository), until they expire.
    tokens: Mutex<HashMap<(u64, String), Minted>>,
}

/// An RSA private key from PEM (PKCS#1 `RSA PRIVATE KEY`, as GitHub hands
/// them out, or PKCS#8 `PRIVATE KEY`). A Fly secret pasted with `\n`s
/// works too.
pub fn parse_pem(pem: &str) -> anyhow::Result<KeyPair> {
    let pem = pem.replace("\\n", "\n");
    let begin = pem.find("-----BEGIN ").context("not a PEM key (no BEGIN line)")?;
    let rest = &pem[begin + 11..];
    let label_end = rest.find("-----").context("not a PEM key")?;
    let label = &rest[..label_end];
    let body = &rest[label_end + 5..];
    let end = body.find("-----END ").context("not a PEM key (no END line)")?;
    let b64: String = body[..end].chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD.decode(b64).context("the PEM key isn't base64")?;
    let key = match label {
        "RSA PRIVATE KEY" => KeyPair::from_der(&der),
        "PRIVATE KEY" => KeyPair::from_pkcs8(&der),
        l => bail!("a {l} isn't an RSA private key"),
    };
    key.map_err(|e| anyhow::anyhow!("the GitHub App's private key: {e}"))
}

/// Seconds since the epoch for GitHub's `2026-10-03T12:34:56Z`.
pub fn epoch_of(t: &str) -> Option<u64> {
    let (d, rest) = t.split_once('T')?;
    let mut ymd = d.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, dd) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let hms = rest.trim_end_matches('Z');
    let hms = hms.split(['+', '.']).next()?;
    let mut p = hms.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, s) = (p.next()??, p.next()??, p.next()??);
    // Days from civil (Howard Hinnant's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + s).ok()
}

fn now_s() -> u64 {
    illogical_e2e::now_ms() / 1000
}

impl GithubApp {
    pub fn new(id: String, slug: String, webhook_secret: String, api: &str, key: KeyPair) -> Self {
        Self {
            id,
            slug,
            webhook_secret,
            api: api.trim_end_matches('/').to_owned(),
            key,
            installs: Mutex::default(),
            access: Mutex::default(),
            tokens: Mutex::default(),
        }
    }

    /// The App's JWT: RS256, issued a minute ago (clock drift), for nine
    /// minutes (GitHub takes at most ten).
    pub fn jwt(&self, now: u64) -> anyhow::Result<String> {
        let header = B64.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let iss = self.id.parse::<u64>().map_or_else(|_| json!(self.id), |n| json!(n));
        let claims = B64.encode(json!({ "iat": now - 60, "exp": now + 540, "iss": iss }).to_string());
        let input = format!("{header}.{claims}");
        let mut sig = vec![0u8; self.key.public_modulus_len()];
        self.key
            .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), input.as_bytes(), &mut sig)
            .map_err(|_| anyhow::anyhow!("signing the App's JWT"))?;
        Ok(format!("{input}.{}", B64.encode(sig)))
    }

    fn req(
        &self,
        http: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
        bearer: &str,
    ) -> reqwest::RequestBuilder {
        http.request(method, format!("{}/{}", self.api, path.trim_start_matches('/')))
            .bearer_auth(bearer)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "illogical-control")
    }

    /// Where the App is installed for `repo`, if it is (kept ten minutes).
    pub async fn installation(&self, http: &reqwest::Client, repo: &str) -> anyhow::Result<Option<Install>> {
        let k = repo.to_ascii_lowercase();
        if let Some((at, i)) = self.installs.lock().unwrap().get(&k)
            && at.elapsed() < KEEP
        {
            return Ok(i.clone());
        }
        let r = self
            .req(http, reqwest::Method::GET, &format!("repos/{repo}/installation"), &self.jwt(now_s())?)
            .send()
            .await?;
        let found = match r.status().as_u16() {
            200 => {
                let v: Value = r.json().await?;
                let id = v["id"].as_u64().context("an installation with no id")?;
                Some(Install {
                    id,
                    account: v["account"]["login"].as_str().unwrap_or_default().to_owned(),
                    account_id: v["account"]["id"].as_u64().unwrap_or(0),
                })
            }
            404 => None,
            s => bail!("GitHub said {s} for the App's installation on {repo}"),
        };
        self.installs.lock().unwrap().insert(k, (Instant::now(), found.clone()));
        Ok(found)
    }

    /// An installation token for one repository (its name, without the
    /// owner), read-only: `(token, expires_at)`.
    pub async fn token(&self, http: &reqwest::Client, install: u64, name: &str) -> anyhow::Result<(String, String)> {
        let k = (install, name.to_ascii_lowercase());
        if let Some((t, at, exp)) = self.tokens.lock().unwrap().get(&k)
            && *exp > now_s() + 300
        {
            return Ok((t.clone(), at.clone()));
        }
        let r = self
            .req(
                http,
                reqwest::Method::POST,
                &format!("app/installations/{install}/access_tokens"),
                &self.jwt(now_s())?,
            )
            .json(&json!({ "repositories": [name], "permissions": read_only() }))
            .send()
            .await?;
        let status = r.status();
        let v: Value = r.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!("GitHub said {status} minting an installation token: {}", v["message"].as_str().unwrap_or(""));
        }
        let t = v["token"].as_str().context("no token in GitHub's answer")?.to_owned();
        let at = v["expires_at"].as_str().unwrap_or_default().to_owned();
        let exp = epoch_of(&at).unwrap_or(now_s() + 3600);
        let mut tokens = self.tokens.lock().unwrap();
        tokens.retain(|_, (_, _, e)| *e > now_s());
        tokens.insert(k, (t.clone(), at.clone(), exp));
        Ok((t, at))
    }

    /// Whether the answer for this person and repository is at hand.
    pub fn knows(&self, user: &GithubUser, repo: &str) -> bool {
        let k = (user.id, repo.to_ascii_lowercase());
        self.access.lock().unwrap().get(&k).is_some_and(|(at, _)| at.elapsed() < KEEP)
    }

    /// Whether `user` may hear about `repo` through the App: the
    /// installation that covers it, or why not.
    pub async fn may(&self, http: &reqwest::Client, user: &GithubUser, repo: &str) -> Result<Install, String> {
        let k = (user.id, repo.to_ascii_lowercase());
        if let Some((at, r)) = self.access.lock().unwrap().get(&k)
            && at.elapsed() < KEEP
        {
            return r.clone();
        }
        let r = self.may_now(http, user, repo).await;
        if let Err(e) = &r {
            info!(repo, why = e, "no live updates for a repository");
        }
        self.access.lock().unwrap().insert(k, (Instant::now(), r.clone()));
        r
    }

    async fn may_now(&self, http: &reqwest::Client, user: &GithubUser, repo: &str) -> Result<Install, String> {
        let login = user.login.as_str();
        let owner = repo.split('/').next().unwrap_or_default();
        let install = match self.installation(http, repo).await {
            Ok(Some(i)) => i,
            Ok(None) => {
                return Err(format!(
                    "the GitHub App {} isn't installed on {owner}: https://github.com/apps/{}/installations/new",
                    self.slug, self.slug
                ));
            }
            Err(e) => {
                warn!(error = %e, "asking GitHub about the App's installation");
                return Err("couldn't ask GitHub about the App's installation".into());
            }
        };
        // Their own: by id, as a login that changed hands would match.
        if install.account_id != 0 && install.account_id == user.id {
            return Ok(install);
        }
        // Someone else's installation (an organization's, say): only if
        // GitHub says they're a collaborator on the repository.
        let name = repo.split('/').nth(1).unwrap_or_default();
        let token = match self.token(http, install.id, name).await {
            Ok((t, _)) => t,
            Err(e) => {
                warn!(error = %e, "minting a token to check access");
                return Err("couldn't check your access to it with the App".into());
            }
        };
        let path = format!("repos/{repo}/collaborators/{login}/permission");
        let r = self.req(http, reqwest::Method::GET, &path, &token).send().await.map_err(|e| e.to_string())?;
        let v: Value = if r.status().is_success() { r.json().await.unwrap_or_default() } else { Value::Null };
        // The login control knows may belong to someone else by now.
        if v["user"]["id"].as_u64().is_some_and(|id| id != user.id) {
            return Err(format!("the GitHub login {login} isn't yours any more: sign in to control with GitHub again"));
        }
        match (v["permission"].as_str(), v["user"]["id"].as_u64()) {
            (Some(p), Some(_)) if p != "none" => Ok(install),
            _ => Err(format!("GitHub doesn't list {login} as a collaborator on {repo}")),
        }
    }

    /// Installations changed (an install, a repository added or removed).
    pub fn forget(&self) {
        self.installs.lock().unwrap().clear();
        self.access.lock().unwrap().clear();
    }
}

// ---------------------------------------------------------------- webhooks

type HmacSha256 = hmac::Hmac<Sha256>;

/// `X-Hub-Signature-256: sha256=<hex>` over the body, in constant time.
pub fn signed(secret: &str, header: Option<&str>, body: &[u8]) -> bool {
    use hmac::{KeyInit, Mac};
    let Some(hex_sig) = header.and_then(|h| h.trim().strip_prefix("sha256=")) else { return false };
    let Ok(sig) = hex::decode(hex_sig) else { return false };
    if secret.is_empty() {
        return false;
    }
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac");
    mac.update(body);
    mac.verify_slice(&sig).is_ok()
}

/// What control relays of an event: no more than this.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Poke {
    pub provider: &'static str,
    pub host: &'static str,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    pub event: String,
    pub delivery: String,
}

/// The pokes an event makes: one per pull request or issue it names, or
/// one for the whole repository.
pub fn pokes(event: &str, delivery: &str, v: &Value) -> Vec<Poke> {
    let Some(repo) = v["repository"]["full_name"].as_str().filter(|r| r.contains('/')) else { return vec![] };
    let prs = |list: &Value| -> Vec<u64> {
        list.as_array().into_iter().flatten().filter_map(|p| p["number"].as_u64()).collect()
    };
    let mut numbers: Vec<u64> = match event {
        "pull_request" | "pull_request_review" | "pull_request_review_comment" | "pull_request_review_thread" => {
            v["pull_request"]["number"].as_u64().into_iter().collect()
        }
        "issues" | "issue_comment" => v["issue"]["number"].as_u64().into_iter().collect(),
        "check_run" => prs(&v["check_run"]["pull_requests"]),
        "check_suite" => prs(&v["check_suite"]["pull_requests"]),
        "workflow_run" => prs(&v["workflow_run"]["pull_requests"]),
        _ => vec![],
    };
    numbers.sort_unstable();
    numbers.dedup();
    let one = |number| Poke {
        provider: "github",
        host: "github.com",
        repo: repo.to_owned(),
        number,
        event: event.to_owned(),
        delivery: delivery.to_owned(),
    };
    if numbers.is_empty() { vec![one(None)] } else { numbers.into_iter().map(|n| one(Some(n))).collect() }
}

/// Deliveries seen lately, oldest first.
#[derive(Default)]
struct Seen {
    set: HashSet<String>,
    order: std::collections::VecDeque<String>,
}

impl Seen {
    /// False if it was seen already.
    fn add(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_owned()) {
            return false;
        }
        self.order.push_back(id.to_owned());
        while self.order.len() > DELIVERIES {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

/// Whether a repository's events reach a daemon, and why not.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Standing {
    pub repo: String,
    pub live: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

struct Watch {
    generation: u64,
    account: String,
    repos: Vec<String>,
    standing: BTreeMap<String, Standing>,
}

/// Daemons' subscriptions, and the deliveries seen.
#[derive(Default)]
pub struct Watches {
    by_daemon: Mutex<HashMap<String, Watch>>,
    seen: Mutex<Seen>,
}

impl Watches {
    /// The daemons a poke for `repo` goes to.
    pub fn route(&self, repo: &str) -> Vec<String> {
        let k = repo.to_ascii_lowercase();
        let mut out: Vec<String> = self
            .by_daemon
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, w)| w.standing.get(&k).is_some_and(|s| s.live))
            .map(|(d, _)| d.clone())
            .collect();
        out.sort();
        out
    }

    /// A daemon's socket closed: its subscription goes with it.
    pub fn drop_daemon(&self, daemon: &str, generation: u64) {
        let mut m = self.by_daemon.lock().unwrap();
        if m.get(daemon).is_some_and(|w| w.generation == generation) {
            m.remove(daemon);
        }
    }

    fn set(&self, daemon: &str, generation: u64, account: &str, repos: Vec<String>, standing: Vec<Standing>) {
        let standing = standing.into_iter().map(|s| (s.repo.to_ascii_lowercase(), s)).collect();
        self.by_daemon
            .lock()
            .unwrap()
            .insert(daemon.to_owned(), Watch { generation, account: account.to_owned(), repos, standing });
    }

    /// Repositories (lowercase) the account's other daemons watch.
    fn account_repos(&self, account: &str, except: &str) -> HashSet<String> {
        self.by_daemon
            .lock()
            .unwrap()
            .iter()
            .filter(|(d, w)| w.account == account && d.as_str() != except)
            .flat_map(|(_, w)| w.repos.iter().map(|r| r.to_ascii_lowercase()))
            .collect()
    }

    fn snapshot(&self) -> Vec<(String, u64, String, Vec<String>)> {
        self.by_daemon
            .lock()
            .unwrap()
            .iter()
            .map(|(d, w)| (d.clone(), w.generation, w.account.clone(), w.repos.clone()))
            .collect()
    }

    fn first_time(&self, delivery: &str) -> bool {
        delivery.is_empty() || self.seen.lock().unwrap().add(delivery)
    }
}

fn good_repo(r: &str) -> bool {
    let mut p = r.split('/');
    let ok = |s: Option<&str>| {
        s.is_some_and(|s| {
            !s.is_empty()
                && !s.starts_with('.')
                && s.len() <= 100
                && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    };
    ok(p.next()) && ok(p.next()) && p.next().is_none()
}

/// The account's GitHub identity, if it signed in with GitHub.
fn github_user(app: &App, account: &str) -> anyhow::Result<Option<GithubUser>> {
    Ok(app.db.github_identity(account)?.and_then(|(id, login)| Some(GithubUser { id: id.parse().ok()?, login })))
}

/// Where each repository stands for an account. `fresh`: a daemon just
/// asked (not the heartbeat), so lookups GitHub hasn't answered lately
/// count against the account's limit.
async fn standings(app: &App, account: &str, repos: &[String], fresh: bool) -> Vec<Standing> {
    let user = github_user(app, account).ok().flatten();
    let mut out = Vec::new();
    for repo in repos {
        let why = match (&app.github_app, &user) {
            (None, _) => Some("this control has no GitHub App: the block polls".to_owned()),
            (Some(_), None) => Some("sign in to control with GitHub for live updates from GitHub".to_owned()),
            (Some(gh), Some(u)) => {
                if fresh
                    && !gh.knows(u, repo)
                    && app.limits.check_account(crate::limit::FORGE_LOOKUPS, account).is_err()
                {
                    Some("too many repositories asked about lately: live updates resume within the hour".to_owned())
                } else {
                    gh.may(&app.http, u, repo).await.err()
                }
            }
        };
        out.push(Standing { repo: repo.clone(), live: why.is_none(), why });
    }
    out
}

/// What a daemon says over its relay socket (anything but "trust").
pub fn from_daemon(app: &Arc<App>, daemon: &str, generation: u64, text: &str) {
    #[derive(Deserialize)]
    struct Msg {
        t: String,
        #[serde(default)]
        repos: Vec<String>,
    }
    let Ok(m) = serde_json::from_str::<Msg>(text) else { return };
    if m.t != "forge.watch" {
        return;
    }
    if app.limits.check_daemon(crate::limit::FORGE_WATCHES, daemon).is_err() {
        warn!(%daemon, "too many forge.watch messages; ignoring this one");
        return;
    }
    let mut repos: Vec<String> = m.repos.into_iter().filter(|r| good_repo(r)).take(MAX_REPOS).collect();
    repos.sort_by_key(|r| r.to_ascii_lowercase());
    repos.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let (app, daemon) = (app.clone(), daemon.to_owned());
    tokio::spawn(async move {
        let Ok(Some(account)) = app.db.daemon_account(&daemon) else { return };
        // The account's machines together watch at most so many.
        let mut others = app.forge.account_repos(&account, &daemon);
        let (ok, over): (Vec<String>, Vec<String>) = repos.into_iter().partition(|r| {
            let k = r.to_ascii_lowercase();
            others.contains(&k) || (others.len() < MAX_ACCOUNT_REPOS && others.insert(k))
        });
        let repos = ok;
        let mut st = standings(&app, &account, &repos, true).await;
        st.extend(over.into_iter().map(|repo| Standing {
            repo,
            live: false,
            why: Some(format!("this account's machines watch {MAX_ACCOUNT_REPOS} repositories already")),
        }));
        let live = st.iter().filter(|s| s.live).count();
        info!(%daemon, repos = repos.len(), live, "forge subscriptions");
        app.forge.set(&daemon, generation, &account, repos, st.clone());
        app.relay.text(&daemon, &watching(&st));
    });
}

fn watching(st: &[Standing]) -> String {
    json!({ "t": "forge.watching", "repos": st }).to_string()
}

/// Every `every`, each subscribed daemon hears where its repositories
/// stand (looked at again when the answers are old): the heartbeat that
/// says the webhook path is up.
pub async fn heartbeat(app: Arc<App>, every: Duration) {
    loop {
        tokio::time::sleep(every).await;
        for (daemon, generation, account, repos) in app.forge.snapshot() {
            let st = standings(&app, &account, &repos, false).await;
            app.forge.set(&daemon, generation, &account, repos, st.clone());
            app.relay.text(&daemon, &watching(&st));
        }
    }
}

/// `POST /github/webhook`: the App's events, relayed as pokes.
pub async fn webhook(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(gh) = &app.github_app else {
        return err(StatusCode::NOT_FOUND, "this control has no GitHub App").into_response();
    };
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    let delivery = h("x-github-delivery").unwrap_or_default().chars().take(64).collect::<String>();
    let event = h("x-github-event").unwrap_or_default().chars().take(64).collect::<String>();
    if !signed(&gh.webhook_secret, h("x-hub-signature-256"), &body) {
        warn!(%event, %delivery, "a GitHub webhook with a bad or missing signature");
        return err(StatusCode::UNAUTHORIZED, "bad signature").into_response();
    }
    if !app.forge.first_time(&delivery) {
        return Json(json!({ "duplicate": true })).into_response();
    }
    if event == "ping" {
        info!(%delivery, "GitHub pinged the webhook");
        return Json(json!({ "pong": true })).into_response();
    }
    if event.starts_with("installation") {
        gh.forget();
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or_default();
    let mut relayed = 0;
    for p in pokes(&event, &delivery, &v) {
        let msg = json!({ "t": "forge.poke", "poke": p }).to_string();
        let to = app.forge.route(&p.repo);
        for d in &to {
            app.relay.text(d, &msg);
        }
        relayed += to.len();
        info!(%event, %delivery, repo = p.repo, number = p.number, daemons = to.len(), "webhook relayed");
    }
    (StatusCode::ACCEPTED, Json(json!({ "relayed": relayed }))).into_response()
}

#[derive(Deserialize)]
pub struct TokenAsk {
    repo: String,
}

/// `POST /api/daemon/github/token {repo}`: a hosted box (no `gh` login)
/// reads through the App, for repositories its account may.
pub async fn daemon_token(
    State(app): State<Arc<App>>,
    d: DaemonAuth,
    Json(b): Json<TokenAsk>,
) -> Result<Json<Value>, ApiError> {
    let gh = app.github_app.as_ref().ok_or_else(|| err(StatusCode::NOT_FOUND, "this control has no GitHub App"))?;
    if !good_repo(&b.repo) {
        return Err(err(StatusCode::BAD_REQUEST, "repo: OWNER/NAME"));
    }
    let user = github_user(&app, &d.cert.account)?
        .ok_or_else(|| err(StatusCode::FORBIDDEN, "sign in to control with GitHub to read GitHub through its App"))?;
    if !gh.knows(&user, &b.repo) {
        app.limits.check_account(crate::limit::FORGE_LOOKUPS, &d.cert.account)?;
    }
    let install = gh.may(&app.http, &user, &b.repo).await.map_err(|why| err(StatusCode::FORBIDDEN, &why))?;
    let login = user.login;
    let name = b.repo.split('/').nth(1).unwrap_or_default();
    let (token, expires_at) = gh.token(&app.http, install.id, name).await?;
    // Who and what; never the token.
    info!(daemon = d.cert.device, repo = b.repo, "installation token for a daemon");
    Ok(Json(json!({ "token": token, "expires_at": expires_at, "login": login, "app": gh.slug })))
}

#[cfg(test)]
pub(crate) mod tests {
    use aws_lc_rs::{
        encoding::AsDer,
        rsa::KeySize,
        signature::{KeyPair as _, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey},
    };

    use super::*;

    fn pem(label: &str, der: &[u8]) -> String {
        let b = base64::engine::general_purpose::STANDARD.encode(der);
        let lines: Vec<String> = b.as_bytes().chunks(64).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
        format!("-----BEGIN {label}-----\n{}\n-----END {label}-----\n", lines.join("\n"))
    }

    pub(crate) fn throwaway_key() -> (String, Vec<u8>) {
        let k = KeyPair::generate(KeySize::Rsa2048).unwrap();
        let der = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&k).unwrap();
        (pem("PRIVATE KEY", der.as_ref()), k.public_key().as_ref().to_vec())
    }

    #[test]
    fn jwt_is_rs256_and_checks_out() {
        let (pem, public) = throwaway_key();
        let app =
            GithubApp::new("5171453".into(), "illogical-test".into(), "s".into(), "http://x", parse_pem(&pem).unwrap());
        let t = app.jwt(1_800_000_000).unwrap();
        let parts: Vec<&str> = t.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, json!({ "alg": "RS256", "typ": "JWT" }));
        let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims, json!({ "iat": 1_800_000_000u64 - 60, "exp": 1_800_000_000u64 + 540, "iss": 5171453 }));
        let sig = B64.decode(parts[2]).unwrap();
        let input = format!("{}.{}", parts[0], parts[1]);
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &public).verify(input.as_bytes(), &sig).unwrap();
        // A tampered claim doesn't.
        let bad = format!("{}.{}", parts[0], B64.encode(br#"{"iss":1}"#));
        assert!(UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &public).verify(bad.as_bytes(), &sig).is_err());
        // As a Fly secret with literal \n, too; and not anything else.
        assert!(parse_pem(&pem.replace('\n', "\\n")).is_ok());
        assert!(parse_pem("-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----").is_err());
        assert!(parse_pem("nope").is_err());
    }

    #[test]
    fn signatures() {
        use hmac::{KeyInit, Mac};
        let body = br#"{"zen":"Keep it logically awesome."}"#;
        let mut mac = HmacSha256::new_from_slice(b"whsec").unwrap();
        mac.update(body);
        let good = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        assert!(signed("whsec", Some(&good), body));
        assert!(!signed("other", Some(&good), body));
        assert!(!signed("whsec", Some(&good), b"{}"));
        assert!(!signed("whsec", None, body));
        assert!(!signed("whsec", Some("sha1=abc"), body));
        assert!(!signed("whsec", Some("sha256=zz"), body));
        assert!(!signed("", Some(&good), body));
    }

    #[test]
    fn what_a_poke_keeps() {
        let pr = json!({ "action": "review_requested", "repository": { "full_name": "jhgaylor/hud" },
            "pull_request": { "number": 7, "title": "secret title", "body": "secret body" } });
        let p = pokes("pull_request", "d1", &pr);
        assert_eq!(
            p,
            vec![Poke {
                provider: "github",
                host: "github.com",
                repo: "jhgaylor/hud".into(),
                number: Some(7),
                event: "pull_request".into(),
                delivery: "d1".into()
            }]
        );
        let wire = serde_json::to_string(&p[0]).unwrap();
        assert!(!wire.contains("secret"), "{wire}");
        let c = json!({ "repository": { "full_name": "o/r" }, "issue": { "number": 3, "pull_request": {} } });
        assert_eq!(pokes("issue_comment", "d", &c)[0].number, Some(3));
        let run = json!({ "repository": { "full_name": "o/r" },
            "check_run": { "head_sha": "abc", "pull_requests": [{ "number": 2 }, { "number": 9 }, { "number": 2 }] } });
        assert_eq!(pokes("check_run", "d", &run).iter().map(|p| p.number).collect::<Vec<_>>(), vec![Some(2), Some(9)]);
        // A fork's PR (no pull_requests) and a status: the whole repository.
        let fork = json!({ "repository": { "full_name": "o/r" }, "check_suite": { "pull_requests": [] } });
        assert_eq!(pokes("check_suite", "d", &fork)[0].number, None);
        let st = json!({ "repository": { "full_name": "o/r" }, "sha": "abc", "state": "failure" });
        assert_eq!(pokes("status", "d", &st)[0].number, None);
        let wr =
            json!({ "repository": { "full_name": "o/r" }, "workflow_run": { "pull_requests": [{ "number": 4 }] } });
        assert_eq!(pokes("workflow_run", "d", &wr)[0].number, Some(4));
        assert!(pokes("push", "d", &json!({})).is_empty());
    }

    #[test]
    fn deliveries_once() {
        let w = Watches::default();
        assert!(w.first_time("a"));
        assert!(!w.first_time("a"));
        assert!(w.first_time("b"));
        let mut s = Seen::default();
        for i in 0..DELIVERIES + 5 {
            assert!(s.add(&i.to_string()));
        }
        assert_eq!(s.set.len(), DELIVERIES);
        assert!(s.add("0"), "the oldest are forgotten");
    }

    #[test]
    fn routing_only_where_live() {
        let w = Watches::default();
        let st = |repo: &str, live| Standing { repo: repo.into(), live, why: None };
        w.set("d1", 1, "a1", vec!["O/R".into()], vec![st("O/R", true), st("o/x", false)]);
        w.set("d2", 1, "a2", vec!["o/r".into()], vec![st("o/r", false)]);
        w.set("d3", 4, "a1", vec!["o/r".into()], vec![st("o/r", true)]);
        assert_eq!(w.route("o/r"), vec!["d1", "d3"]);
        assert!(w.route("o/x").is_empty());
        // An old socket's end leaves the new one's subscription.
        w.drop_daemon("d3", 3);
        assert_eq!(w.route("o/r"), vec!["d1", "d3"]);
        w.drop_daemon("d3", 4);
        assert_eq!(w.route("o/r"), vec!["d1"]);
    }

    #[test]
    fn repos_and_times() {
        assert!(good_repo("jhgaylor/illogical"));
        assert!(good_repo("a.b/c-d_e"));
        assert!(!good_repo("a/b/c"));
        assert!(!good_repo("a"));
        assert!(!good_repo("a/b?x"));
        assert!(!good_repo("../x"));
        assert_eq!(epoch_of("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_of("2026-10-03T12:00:00Z"), Some(1_791_028_800));
        assert_eq!(epoch_of("2000-02-29T00:00:01Z"), Some(951_782_401));
        assert_eq!(epoch_of("junk"), None);
    }
}
