//! The GitHub adapter (M38): GitHub's REST API onto the normalized model,
//! and its writes. Requests go through the daemon's own HTTP client with a
//! token from the person's `gh` login (`gh auth token --hostname H`, run
//! with their shell environment, #74), held in memory only and asked for
//! again after a 401. GitHub Enterprise is the same with its API at
//! `https://HOST/api/v3`.
//!
//! What's particular to GitHub (S23):
//!
//! - **REST with ETags, not GraphQL.** Every GET is conditional
//!   (`If-None-Match` with the ETag it last answered, per URL); a 304
//!   reuses the body kept with it and costs no rate-limit points. A poll is
//!   the item, its head's check runs and its combined status: three 304s
//!   when nothing moved. Reviews and the timeline are read again only when
//!   the item's fingerprint moves (as on Forgejo), and those are
//!   conditional too.
//! - **Checks are check runs and commit statuses, both.** The combined
//!   status says `pending` when it has no statuses at all (`total_count:
//!   0`); only its statuses are read, never that state, and the rollup is
//!   the model's own.
//! - **Review requests are pending ones** (GitHub drops a request when the
//!   reviewer reviews). A team's (`requested_teams`) is `org/slug`, and it's
//!   "asked of you" when you're in it (`GET /user/teams`).
//! - **Branch protection says so:** `mergeable_state: blocked` holds a
//!   green PR back from `done`.
//! - **Rerun:** a failed check run from Actions names its workflow run;
//!   *Rerun* posts `actions/runs/ID/rerun-failed-jobs` for each.
//! - **Rate limits:** `X-RateLimit-Remaining` under [`LOW`] polls at most
//!   once every [`LOW_EVERY`]; a 403/429 that says the limit is spent (or
//!   gives `Retry-After`) stops reads until then. The block's state says so
//!   (`rate.backoff`).
//! - **A `mentioned` event's actor is who was mentioned,** so it's kept as
//!   the event's target, with no actor.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::{
    Adapter, Error, Polled, ReviewEvent, Sent, Write,
    forgejo::time,
    model::{
        Branch, Check, CheckSource, CheckState, Event, EventKind, Item, ItemKind, ItemState, Linked, Me, Review,
        ReviewState, Reviewer, RunRef,
    },
};
use tracing::info;

use crate::review::Runner;

/// Timeline events kept in the state (as on Forgejo).
pub const EVENTS: usize = super::forgejo::EVENTS;
/// Fewer requests left than this: back off.
pub const LOW: u64 = 100;
/// How often it polls while the limit is low.
pub const LOW_EVERY: Duration = Duration::from_secs(60);

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

fn t(v: &Value) -> Option<i64> {
    v.as_str().and_then(time)
}

fn login(v: &Value) -> Option<String> {
    v["login"].as_str().filter(|l| !l.is_empty()).map(str::to_owned)
}

/// Whether a host is github.com's (a remote's or a link's).
pub fn is_github_host(host: &str) -> bool {
    matches!(host.to_ascii_lowercase().as_str(), "github.com" | "www.github.com" | "ssh.github.com")
}

/// The API base for a GitHub host: `https://api.github.com`, or GitHub
/// Enterprise's `https://HOST/api/v3`. (`ARUGULA_GITHUB_API` stands in
/// for github.com's in tests.)
pub fn api_for(host: &str) -> String {
    if is_github_host(host) {
        std::env::var("ARUGULA_GITHUB_API").ok().filter(|a| !a.is_empty()).unwrap_or("https://api.github.com".into())
    } else {
        format!("https://{host}/api/v3")
    }
}

/// The host `gh` knows a login by, from an API base.
pub fn host_of_api(api: &str) -> String {
    let h = super::login::url_host(api).unwrap_or_default();
    if h == "api.github.com" { "github.com".into() } else { h }
}

/// `https://github.com/OWNER/REPO/pull/N[/files…]` (or a GitHub
/// Enterprise host's) → (host, owner/name, N).
pub fn pr_url(u: &str) -> Option<(String, String, u64)> {
    let url = url::Url::parse(u).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = super::login::url_host(u)?;
    let segs: Vec<&str> = url.path_segments()?.filter(|x| !x.is_empty()).collect();
    match segs.as_slice() {
        [o, r, "pull", n, ..] => Some((host, format!("{o}/{r}"), n.parse().ok()?)),
        [o, r, "pulls", n, ..] if is_github_host(&host) => Some((host, format!("{o}/{r}"), n.parse().ok()?)),
        _ => None,
    }
}

/// M37: `https://github.com/OWNER/REPO/issues/N` → (host, owner/name, N).
pub fn issue_url(u: &str) -> Option<(String, String, u64)> {
    let url = url::Url::parse(u).ok()?;
    let host = super::login::url_host(u)?;
    if !matches!(url.scheme(), "http" | "https") || !is_github_host(&host) {
        return None;
    }
    let segs: Vec<&str> = url.path_segments()?.filter(|x| !x.is_empty()).collect();
    match segs.as_slice() {
        [o, r, "issues", n, ..] => Some(("github.com".into(), format!("{o}/{r}"), n.parse().ok()?)),
        _ => None,
    }
}

/// M37: `GET repos/O/R/issues/N` as an item (no branches).
pub fn issue_item(it: &Value) -> Item {
    Item {
        kind: ItemKind::Issue,
        number: it["number"].as_u64().unwrap_or(0),
        url: s(&it["html_url"]),
        title: s(&it["title"]),
        body: s(&it["body"]),
        author: login(&it["user"]).unwrap_or_default(),
        state: if it["state"] == "closed" { ItemState::Closed } else { ItemState::Open },
        labels: it["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(str::to_owned))
            .collect(),
        assignees: it["assignees"].as_array().into_iter().flatten().filter_map(login).collect(),
        comments: it["comments"].as_u64().unwrap_or(0),
        updated_at: t(&it["updated_at"]).unwrap_or(0),
        ..Item::default()
    }
}

/// M37: the pull requests an issue's timeline says refer to it
/// (`cross-referenced` from a PR), the newest word on each.
pub fn linked(list: &Value) -> Vec<Linked> {
    let mut out: Vec<Linked> = Vec::new();
    for e in list.as_array().into_iter().flatten() {
        let i = &e["source"]["issue"];
        let (Some(n), true) = (i["number"].as_u64(), i["pull_request"].is_object()) else { continue };
        let state = if !i["pull_request"]["merged_at"].is_null() {
            ItemState::Merged
        } else if i["state"] == "closed" {
            ItemState::Closed
        } else {
            ItemState::Open
        };
        out.retain(|l| l.number != n);
        out.push(Linked { number: n, title: s(&i["title"]), state, url: s(&i["html_url"]), head: None });
    }
    out
}

/// A team in a request: `org/slug` (the org from its page, else the
/// repository's owner).
fn team(v: &Value, owner: &str) -> Option<String> {
    let slug = v["slug"].as_str()?;
    let org = v["html_url"]
        .as_str()
        .and_then(|u| u.split("/orgs/").nth(1))
        .and_then(|r| r.split('/').next())
        .filter(|o| !o.is_empty())
        .or(v["organization"]["login"].as_str())
        .unwrap_or(owner);
    Some(format!("{org}/{slug}"))
}

/// `GET repos/O/R/pulls/N`, with its pending requests (people and teams).
pub fn item(it: &Value, repo: &str) -> Item {
    let state = if it["merged"].as_bool() == Some(true) || !it["merged_at"].is_null() {
        ItemState::Merged
    } else if it["state"] == "closed" {
        ItemState::Closed
    } else {
        ItemState::Open
    };
    let side = |v: &Value| Branch {
        repo: v["repo"]["full_name"].as_str().map(str::to_owned),
        branch: s(&v["ref"]),
        sha: s(&v["sha"]),
    };
    let number = it["number"].as_u64().unwrap_or(0);
    let owner = repo.split('/').next().unwrap_or_default();
    let mut requested: Vec<Reviewer> =
        it["requested_reviewers"].as_array().into_iter().flatten().filter_map(login).map(Reviewer::User).collect();
    requested.extend(
        it["requested_teams"].as_array().into_iter().flatten().filter_map(|x| team(x, owner)).map(Reviewer::Team),
    );
    Item {
        kind: ItemKind::Pr,
        number,
        url: s(&it["html_url"]),
        title: s(&it["title"]),
        body: s(&it["body"]),
        author: login(&it["user"]).unwrap_or_default(),
        state,
        draft: it["draft"].as_bool().unwrap_or(false),
        labels: it["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(str::to_owned))
            .collect(),
        assignees: it["assignees"].as_array().into_iter().flatten().filter_map(login).collect(),
        base: side(&it["base"]),
        head: side(&it["head"]),
        head_ref: format!("refs/pull/{number}/head"),
        // GitHub doesn't say: `diff` works it out (S23 §5).
        merge_base: None,
        mergeable: it["mergeable"].as_bool(),
        blocked: it["mergeable_state"] == "blocked",
        requested,
        comments: it["comments"].as_u64().unwrap_or(0) + it["review_comments"].as_u64().unwrap_or(0),
        updated_at: t(&it["updated_at"]).unwrap_or(0),
        merged_at: t(&it["merged_at"]),
        merged_by: login(&it["merged_by"]),
    }
}

/// What a poll compares: the item's moving parts.
pub fn fingerprint(it: &Value) -> String {
    let rr: Vec<&str> =
        it["requested_reviewers"].as_array().into_iter().flatten().filter_map(|u| u["login"].as_str()).collect();
    let rt: Vec<&str> =
        it["requested_teams"].as_array().into_iter().flatten().filter_map(|u| u["slug"].as_str()).collect();
    json!([
        it["updated_at"],
        it["head"]["sha"],
        it["comments"],
        it["review_comments"],
        it["state"],
        it["merged"],
        it["mergeable"],
        it["mergeable_state"],
        rr,
        rt
    ])
    .to_string()
}

/// A check run's state: its status until it completes, then its
/// conclusion.
fn run_state(c: &Value) -> CheckState {
    match c["status"].as_str().unwrap_or_default() {
        "completed" => match c["conclusion"].as_str().unwrap_or_default() {
            "success" => CheckState::Success,
            "failure" | "timed_out" | "startup_failure" => CheckState::Failure,
            "cancelled" => CheckState::Cancelled,
            "skipped" => CheckState::Skipped,
            "action_required" => CheckState::ActionRequired,
            _ => CheckState::Neutral, // neutral, stale, and whatever's new
        },
        "queued" | "waiting" | "requested" | "pending" => CheckState::Queued,
        _ => CheckState::Running,
    }
}

/// `…/actions/runs/RUN/job/JOB` → the run to rerun, and the job.
fn actions_run(u: &str) -> Option<RunRef> {
    let at = u.find("/actions/runs/")?;
    let mut parts = u[at + 14..].split(['/', '?', '#']);
    let id = parts.next().filter(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))?.to_owned();
    let job = match (parts.next(), parts.next()) {
        (Some("job" | "jobs"), Some(j)) if !j.is_empty() => Some(j.to_owned()),
        _ => None,
    };
    Some(RunRef { id, job })
}

/// `GET commits/SHA/check-runs` and `GET commits/SHA/status`: both are
/// checks. The combined status's own `state` isn't read: with no statuses
/// it says `pending`.
pub fn checks(runs: &Value, status: &Value) -> Vec<Check> {
    let mut out: Vec<Check> = runs["check_runs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let url = c["html_url"].as_str().or(c["details_url"].as_str()).filter(|u| !u.is_empty()).map(str::to_owned);
            let actions = c["app"]["slug"].as_str().is_none_or(|a| a == "github-actions");
            Check {
                name: s(&c["name"]),
                source: CheckSource::CheckRun,
                state: run_state(c),
                allow_failure: false,
                run: c["details_url"].as_str().filter(|_| actions).and_then(actions_run),
                url,
                description: c["output"]["title"].as_str().filter(|d| !d.is_empty()).map(str::to_owned),
            }
        })
        .collect();
    out.extend(status["statuses"].as_array().into_iter().flatten().map(|c| Check {
        name: s(&c["context"]),
        source: CheckSource::Status,
        state: match c["state"].as_str().unwrap_or_default() {
            "success" => CheckState::Success,
            "failure" | "error" => CheckState::Failure,
            "pending" => CheckState::Running,
            _ => CheckState::Neutral,
        },
        allow_failure: false,
        url: c["target_url"].as_str().filter(|u| !u.is_empty()).map(str::to_owned),
        description: c["description"].as_str().filter(|d| !d.is_empty()).map(str::to_owned),
        run: None,
    }));
    out
}

/// `GET pulls/N/reviews`, against the current head (`stale` otherwise).
pub fn reviews(list: &Value, head: &str) -> Vec<Review> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let commit = r["commit_id"].as_str().filter(|c| !c.is_empty()).map(str::to_owned);
            Review {
                id: r["id"].as_u64().map(|i| i.to_string()).unwrap_or_default(),
                author: login(&r["user"]),
                state: match r["state"].as_str().unwrap_or_default() {
                    "APPROVED" => ReviewState::Approved,
                    "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
                    "DISMISSED" => ReviewState::Dismissed,
                    "PENDING" => ReviewState::Pending,
                    _ => ReviewState::Commented,
                },
                stale: !head.is_empty() && commit.as_deref().is_some_and(|c| c != head),
                commit,
                at: t(&r["submitted_at"]),
                body: r["body"].as_str().filter(|b| !b.is_empty()).map(str::to_owned),
                url: r["html_url"].as_str().map(str::to_owned),
            }
        })
        .collect()
}

/// `GET issues/N/timeline`, oldest first. Subscriptions and Copilot's
/// work markers aren't events here.
pub fn events(list: &Value, repo: &str) -> Vec<Event> {
    let owner = repo.split('/').next().unwrap_or_default();
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let ty = e["event"].as_str().unwrap_or_default();
            let mut what = None;
            let kind = match ty {
                "commented" => EventKind::Commented,
                "line-commented" | "commit-commented" => EventKind::ReviewComment,
                "reviewed" => EventKind::Reviewed,
                "review_requested" => EventKind::ReviewRequested,
                "review_request_removed" => EventKind::ReviewRequestRemoved,
                "review_dismissed" => EventKind::ReviewDismissed,
                "committed" | "head_ref_force_pushed" => EventKind::Pushed,
                "labeled" => EventKind::Labeled,
                "assigned" => EventKind::Assigned,
                "renamed" => EventKind::Renamed,
                "referenced" | "cross-referenced" | "connected" => EventKind::Referenced,
                "merged" => EventKind::Merged,
                "closed" => EventKind::Closed,
                "reopened" => EventKind::Reopened,
                "head_ref_deleted" => EventKind::BranchDeleted,
                "milestoned" => EventKind::Milestone,
                "mentioned" => EventKind::Mentioned,
                "subscribed" | "unsubscribed" | "copilot_work_started" | "copilot_work_finished" => return None,
                other => {
                    what = Some(match other {
                        "unlabeled" => format!("removed the label {}", s(&e["label"]["name"])),
                        "ready_for_review" => "marked it ready for review".into(),
                        "convert_to_draft" => "made it a draft".into(),
                        "unassigned" => "unassigned someone".into(),
                        "head_ref_restored" => "restored the branch".into(),
                        o => o.replace('_', " "),
                    });
                    EventKind::Other
                }
            };
            // A line comment is a thread of comments: its first one.
            let first = if ty == "line-commented" { &e["comments"][0] } else { e };
            let target = match ty {
                "review_requested" | "review_request_removed" => match login(&e["requested_reviewer"]) {
                    Some(u) => Some(Reviewer::User(u)),
                    None => team(&e["requested_team"], owner).map(Reviewer::Team),
                },
                "mentioned" => login(&e["actor"]).map(Reviewer::User),
                // M37: who an issue was given to.
                "assigned" => login(&e["assignee"]).map(Reviewer::User),
                _ => None,
            };
            let actor = match ty {
                "mentioned" => None,
                "committed" => e["author"]["name"].as_str().map(str::to_owned),
                "line-commented" => login(&first["user"]),
                _ => login(&e["actor"]).or_else(|| login(&e["user"])),
            };
            let at = ["created_at", "submitted_at"]
                .iter()
                .find_map(|k| t(&first[*k]))
                .or_else(|| t(&e["committer"]["date"]))
                .or_else(|| t(&e["author"]["date"]))
                .unwrap_or(0);
            let id = match (first["id"].as_u64(), e["sha"].as_str()) {
                (Some(i), _) if ty == "line-commented" => format!("gh-lc-{i}"),
                (Some(i), _) => format!("gh-{i}"),
                (None, Some(sha)) => format!("gh-{sha}"),
                // A cross-reference has no id: when, and from what.
                _ => format!(
                    "gh-{ty}-{at}-{}",
                    e["source"]["issue"]["number"].as_u64().map(|n| n.to_string()).unwrap_or_default()
                ),
            };
            Some(Event {
                id,
                at,
                actor,
                kind,
                what,
                target,
                body: matches!(ty, "commented" | "reviewed" | "line-commented")
                    .then(|| first["body"].as_str().filter(|b| !b.is_empty()).map(str::to_owned))
                    .flatten(),
                commits: (ty == "committed").then_some(1),
                force: ty == "head_ref_force_pushed",
            })
        })
        .collect()
}

/// `GET user` and `GET user/teams`: teams as `org/slug`.
pub fn me(user: &Value, teams: &Value) -> Me {
    let mut t = Vec::new();
    for x in teams.as_array().into_iter().flatten() {
        if let (Some(org), Some(slug)) = (x["organization"]["login"].as_str(), x["slug"].as_str()) {
            t.push(format!("{org}/{slug}"));
        }
    }
    Me { login: login(user).unwrap_or_default(), teams: t }
}

/// How a review event is named in GitHub's API.
pub fn review_event(event: ReviewEvent) -> &'static str {
    match event {
        ReviewEvent::Approve => "APPROVE",
        ReviewEvent::RequestChanges => "REQUEST_CHANGES",
        ReviewEvent::Comment => "COMMENT",
    }
}

/// A merge style as GitHub's `merge_method`.
pub fn merge_method(style: Option<&str>) -> Result<&'static str, String> {
    match style.unwrap_or("merge") {
        "merge" => Ok("merge"),
        "squash" => Ok("squash"),
        "rebase" => Ok("rebase"),
        s => Err(format!("GitHub merges by merge, squash or rebase, not {s}")),
    }
}

/// A `Link` header's `rel="last"` URL.
pub fn link_last(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"last\"").then(|| url.trim().trim_start_matches('<').trim_end_matches('>').to_owned())
    })
}

fn with_page(url: &str, page: u64) -> String {
    let Ok(mut u) = url::Url::parse(url) else { return url.to_owned() };
    let pairs: Vec<(String, String)> =
        u.query_pairs().filter(|(k, _)| k != "page").map(|(k, v)| (k.into(), v.into())).collect();
    u.query_pairs_mut().clear().extend_pairs(pairs).append_pair("page", &page.to_string());
    u.to_string()
}

fn page_of(url: &str) -> Option<u64> {
    url::Url::parse(url).ok()?.query_pairs().find(|(k, _)| k == "page").and_then(|(_, v)| v.parse().ok())
}

// ---------------------------------------------------------------- gh

/// What [`GhToken::get`] says when there's no `gh` on the host.
pub const NO_GH: &str = "no gh here: install GitHub's CLI and `gh auth login`, then refresh";

const TOKEN: &str = r#"command -v gh >/dev/null 2>&1 || { echo arugula-no-gh; exit 0; }
exec gh auth token --hostname "$1" 2>/dev/null"#;

/// Tokens held while the daemon runs, by host. Memory only.
static TOKENS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Mutex::default);
/// Hosts `gh` has no login for (asked once per daemon).
static UNKNOWN: LazyLock<Mutex<std::collections::HashSet<String>>> = LazyLock::new(Mutex::default);

/// Whether `gh` is logged in to `host` (GitHub Enterprise, when a remote
/// or link is on a host no `tea` login is for). `gh auth token` answers
/// from the keyring without asking the host, unlike `gh auth status`.
pub async fn knows(runner: &Runner, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if TOKENS.lock().unwrap().contains_key(&host) {
        return true;
    }
    if UNKNOWN.lock().unwrap().contains(&host) {
        return false;
    }
    let ok = GhToken::new(runner.clone(), &host).get(false).await.is_ok();
    if !ok {
        UNKNOWN.lock().unwrap().insert(host);
    }
    ok
}

/// Where the GitHub adapter gets its token: `gh`, on the block's host;
/// and (M40) on github.com with no `gh` login, Arugula control's GitHub
/// App, read-only, for the block's repository.
#[derive(Clone)]
pub struct GhToken {
    runner: Runner,
    host: String,
    repo: Option<String>,
    /// Reading through the App: the account's GitHub login.
    app: Arc<Mutex<Option<String>>>,
}

impl GhToken {
    pub fn new(runner: Runner, host: &str) -> Self {
        Self { runner, host: host.to_ascii_lowercase(), repo: None, app: Arc::default() }
    }

    /// ...that falls back to control's App for this repository.
    pub fn for_repo(mut self, repo: &str) -> Self {
        self.repo = Some(repo.to_owned());
        self
    }

    /// The person's GitHub login when it reads through the App.
    pub fn via_app(&self) -> Option<String> {
        self.app.lock().unwrap().clone()
    }

    /// The token; `fresh`: the last one was refused, ask again.
    pub async fn get(&self, fresh: bool) -> Result<String, String> {
        if self.via_app().is_some() {
            return self.app_token(fresh).await;
        }
        match self.gh(fresh).await {
            Ok(t) => Ok(t),
            Err(e) if self.host == "github.com" && self.repo.is_some() => match self.app_token(fresh).await {
                Ok(t) => {
                    info!(host = self.host, "no gh login: reading through Arugula control's GitHub App");
                    Ok(t)
                }
                Err(why) => Err(format!("{e} (and Arugula control's GitHub App can't read it: {why})")),
            },
            Err(e) => Err(e),
        }
    }

    async fn app_token(&self, fresh: bool) -> Result<String, String> {
        let repo = self.repo.as_deref().unwrap_or_default();
        let (t, login) = crate::forge::live::app_token(repo, fresh).await?;
        *self.app.lock().unwrap() = Some(login);
        Ok(t)
    }

    async fn gh(&self, fresh: bool) -> Result<String, String> {
        if !fresh && let Some(t) = TOKENS.lock().unwrap().get(&self.host).cloned() {
            return Ok(t);
        }
        let (out, _) = self.runner.sh(TOKEN, std::slice::from_ref(&self.host)).await?;
        if out.starts_with(b"arugula-no-gh") {
            return Err(NO_GH.into());
        }
        let t = String::from_utf8_lossy(&out).trim().to_owned();
        if t.is_empty() || t.contains(char::is_whitespace) {
            return Err(format!("gh has no login for {}: `gh auth login --hostname {}`", self.host, self.host));
        }
        TOKENS.lock().unwrap().insert(self.host.clone(), t.clone());
        Ok(t)
    }
}

// ---------------------------------------------------------------- the adapter

/// What a URL last answered: its ETag, body and `Link`.
struct Cached {
    etag: String,
    body: Value,
    link: Option<String>,
}

/// Every URL's last answer, while the daemon runs (memory only), shared by
/// the blocks: a second block on the same host asks `user` and
/// `user/teams` for free, and so does a block after `refresh`.
static CACHE: LazyLock<Mutex<HashMap<String, Cached>>> = LazyLock::new(Mutex::default);
/// More URLs than this: start again.
const CACHED: usize = 2000;

#[derive(Default)]
struct Limits {
    limit: Option<u64>,
    remaining: Option<u64>,
    used: Option<u64>,
    /// When the window resets (epoch seconds).
    reset: Option<u64>,
    /// No reads until then (a spent limit, or `Retry-After`).
    until: Option<Instant>,
    why: Option<String>,
    last_poll: Option<Instant>,
    requests: u64,
    not_modified: u64,
}

fn epoch() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn header_u64(h: &reqwest::header::HeaderMap, k: &str) -> Option<u64> {
    h.get(k)?.to_str().ok()?.trim().parse().ok()
}

fn mins(secs: u64) -> String {
    if secs < 90 { format!("{secs}s") } else { format!("{} min", secs.div_ceil(60)) }
}

/// GitHub (or a GitHub Enterprise host), through `gh`'s login there.
pub struct Github {
    pub api: String,
    http: reqwest::Client,
    token: GhToken,
    limits: Mutex<Limits>,
    /// The last poll's answer, reused while backing off, and the head the
    /// reviews are read against.
    last: Mutex<Option<(Item, Vec<Check>, String)>>,
}

impl Github {
    pub fn new(api: &str, http: reqwest::Client, token: GhToken) -> Self {
        Self {
            api: api.trim_end_matches('/').to_owned(),
            http,
            token,
            limits: Mutex::default(),
            last: Mutex::default(),
        }
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_owned()
        } else {
            format!("{}/{}", self.api, path.trim_start_matches('/'))
        }
    }

    /// Why reads wait now, if they do.
    fn holding(&self) -> Option<String> {
        let mut l = self.limits.lock().unwrap();
        match l.until {
            Some(u) if u > Instant::now() => Some(format!(
                "{} (reads again in {})",
                l.why.clone().unwrap_or_default(),
                mins((u - Instant::now()).as_secs())
            )),
            Some(_) => {
                l.until = None;
                l.why = None;
                None
            }
            None => None,
        }
    }

    fn note_limits(&self, h: &reqwest::header::HeaderMap) {
        let mut l = self.limits.lock().unwrap();
        // Search and GraphQL have limits of their own; these are core's.
        if h.get("x-ratelimit-resource").and_then(|v| v.to_str().ok()).is_some_and(|r| r != "core") {
            return;
        }
        if let Some(r) = header_u64(h, "x-ratelimit-remaining") {
            l.remaining = Some(r);
            l.limit = header_u64(h, "x-ratelimit-limit").or(l.limit);
            l.used = header_u64(h, "x-ratelimit-used").or(l.used);
            l.reset = header_u64(h, "x-ratelimit-reset").or(l.reset);
        }
    }

    /// One request with the token. A GET is conditional on what the URL
    /// last answered, and a 304 hands that back. On a 401 the token is
    /// asked for once more.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(Value, Option<String>), Error> {
        let url = self.url(path);
        let get = method == reqwest::Method::GET;
        if get && let Some(why) = self.holding() {
            return Err(Error::Http(why));
        }
        for attempt in 0..2 {
            let token = self.token.get(attempt > 0).await.map_err(Error::Login)?;
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
            let etag = get.then(|| CACHE.lock().unwrap().get(&url).map(|c| c.etag.clone())).flatten();
            if let Some(e) = &etag {
                req = req.header("If-None-Match", e);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            let res = req
                .send()
                .await
                .map_err(|e| Error::Http(format!("{}: {}", super::redact(&url), super::forgejo::short(&e))))?;
            let status = res.status();
            let headers = res.headers().clone();
            self.note_limits(&headers);
            self.limits.lock().unwrap().requests += 1;
            if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                continue;
            }
            if status == reqwest::StatusCode::NOT_MODIFIED {
                self.limits.lock().unwrap().not_modified += 1;
                let c = CACHE.lock().unwrap();
                if let Some(c) = c.get(&url) {
                    return Ok((c.body.clone(), c.link.clone()));
                }
                return Err(Error::Http(format!("{}: 304 for nothing asked", super::redact(&url))));
            }
            let link = headers.get("link").and_then(|v| v.to_str().ok()).map(str::to_owned);
            let text = res.text().await.unwrap_or_default();
            if !status.is_success() {
                let said = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| text.chars().take(200).collect());
                let retry = header_u64(&headers, "retry-after");
                let spent = header_u64(&headers, "x-ratelimit-remaining") == Some(0);
                if matches!(status.as_u16(), 403 | 429) && (retry.is_some() || spent) {
                    let wait = retry.unwrap_or_else(|| {
                        self.limits.lock().unwrap().reset.map_or(60, |r| r.saturating_sub(epoch()).max(1))
                    });
                    let why = format!("GitHub's rate limit: {said}");
                    {
                        let mut l = self.limits.lock().unwrap();
                        l.until = Some(Instant::now() + Duration::from_secs(wait));
                        l.why = Some(why.clone());
                    }
                    return Err(Error::Http(format!("{why} (reads again in {})", mins(wait))));
                }
                return Err(match status.as_u16() {
                    401 | 403 => Error::Denied(format!("{} {said}", status.as_u16())),
                    404 => Error::NotFound(said),
                    c => Error::Http(format!("{c} {said}")),
                });
            }
            let v =
                if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
            if get && let Some(e) = headers.get("etag").and_then(|v| v.to_str().ok()) {
                let mut c = CACHE.lock().unwrap();
                if c.len() >= CACHED {
                    c.clear();
                }
                c.insert(url.clone(), Cached { etag: e.to_owned(), body: v.clone(), link: link.clone() });
            }
            return Ok((v, link));
        }
        unreachable!("the loop returns")
    }

    async fn get(&self, path: &str) -> Result<Value, Error> {
        Ok(self.send(reqwest::Method::GET, path, None).await?.0)
    }

    /// A list that's oldest first: the newest `keep` of it (its last page,
    /// and the one before when that's short).
    async fn newest(&self, path: &str, keep: usize) -> Result<Vec<Value>, Error> {
        let (first, link) = self.send(reqwest::Method::GET, path, None).await?;
        let first = first.as_array().cloned().unwrap_or_default();
        let last = link.as_deref().and_then(link_last).and_then(|u| Some((page_of(&u).filter(|n| *n > 1)?, u)));
        let mut all = match last {
            None => first,
            Some((n, url)) => {
                let mut tail = self.get(&url).await?.as_array().cloned().unwrap_or_default();
                if tail.len() < keep {
                    let mut before = if n == 2 {
                        first
                    } else {
                        self.get(&with_page(&url, n - 1)).await?.as_array().cloned().unwrap_or_default()
                    };
                    before.append(&mut tail);
                    tail = before;
                }
                tail
            }
        };
        let skip = all.len().saturating_sub(keep);
        Ok(all.split_off(skip))
    }

    /// The poll's requests, when not backing off.
    async fn poll_now(&self, repo: &str, number: u64) -> Result<Polled, Error> {
        let raw = self.get(&format!("repos/{repo}/pulls/{number}")).await?;
        let it = item(&raw, repo);
        let (runs, status) = match it.head.sha.as_str() {
            "" => (Value::Null, Value::Null),
            sha => {
                let (rp, sp) = (
                    format!("repos/{repo}/commits/{sha}/check-runs?per_page=100"),
                    format!("repos/{repo}/commits/{sha}/status?per_page=100"),
                );
                let (r, st) = tokio::join!(self.get(&rp), self.get(&sp));
                (r?, st?)
            }
        };
        let checks = checks(&runs, &status);
        let fp = fingerprint(&raw);
        *self.last.lock().unwrap() = Some((it.clone(), checks.clone(), fp.clone()));
        self.limits.lock().unwrap().last_poll = Some(Instant::now());
        Ok(Polled { item: it, checks, fingerprint: fp })
    }

    fn cached(&self) -> Option<Polled> {
        self.last.lock().unwrap().clone().map(|(item, checks, fingerprint)| Polled { item, checks, fingerprint })
    }
}

impl Adapter for Github {
    fn me(&self) -> BoxFuture<'_, Result<Me, Error>> {
        Box::pin(async move {
            // Through the App (M40), `user` isn't the person: control says
            // who they are on GitHub (no teams).
            self.token.get(false).await.map_err(Error::Login)?;
            if let Some(login) = self.token.via_app() {
                return Ok(Me { login, teams: vec![] });
            }
            let user = self.get("user").await?;
            // Teams need read:org; without it, no team requests.
            let teams = self.get("user/teams?per_page=100").await.unwrap_or(Value::Null);
            Ok(me(&user, &teams))
        })
    }

    fn poll<'a>(&'a self, repo: &'a str, number: u64) -> BoxFuture<'a, Result<Polled, Error>> {
        Box::pin(async move {
            // Backing off: what it last read stands.
            let low = {
                let l = self.limits.lock().unwrap();
                l.remaining.is_some_and(|r| r < LOW)
                    && l.reset.is_some_and(|r| r > epoch())
                    && l.last_poll.is_some_and(|p| p.elapsed() < LOW_EVERY)
            };
            if (low || self.holding().is_some())
                && let Some(p) = self.cached()
            {
                return Ok(p);
            }
            self.poll_now(repo, number).await
        })
    }

    fn rest<'a>(
        &'a self,
        repo: &'a str,
        number: u64,
    ) -> BoxFuture<'a, Result<(Vec<Review>, Vec<Reviewer>, Vec<Event>), Error>> {
        Box::pin(async move {
            let (head, requested) = match self.last.lock().unwrap().as_ref() {
                Some((it, _, _)) => (it.head.sha.clone(), it.requested.clone()),
                None => (String::new(), vec![]),
            };
            let rp = format!("repos/{repo}/pulls/{number}/reviews?per_page=100");
            let tp = format!("repos/{repo}/issues/{number}/timeline?per_page=100");
            let (r, tl) = tokio::join!(self.newest(&rp, 100), self.newest(&tp, EVENTS));
            Ok((reviews(&Value::Array(r?), &head), requested, events(&Value::Array(tl?), repo)))
        })
    }

    fn write<'a>(&'a self, repo: &'a str, number: u64, w: &'a Write) -> BoxFuture<'a, Result<Sent, Error>> {
        Box::pin(async move {
            let post = reqwest::Method::POST;
            match w {
                Write::Comment { body } => {
                    let path = format!("repos/{repo}/issues/{number}/comments");
                    let (v, _) = self.send(post, &path, Some(&json!({ "body": body }))).await?;
                    Ok(Sent { url: v["html_url"].as_str().map(str::to_owned), said: "commented".into() })
                }
                Write::Review { event, body } => {
                    let mut req = json!({ "event": review_event(*event) });
                    if let Some(b) = body {
                        req["body"] = json!(b);
                    }
                    let (v, _) = self.send(post, &format!("repos/{repo}/pulls/{number}/reviews"), Some(&req)).await?;
                    let said = match event {
                        ReviewEvent::Approve => "approved",
                        ReviewEvent::RequestChanges => "requested changes",
                        ReviewEvent::Comment => "reviewed",
                    };
                    Ok(Sent { url: v["html_url"].as_str().map(str::to_owned), said: said.into() })
                }
                Write::Merge { style } => {
                    let how = merge_method(style.as_deref()).map_err(Error::Http)?;
                    let path = format!("repos/{repo}/pulls/{number}/merge");
                    self.send(reqwest::Method::PUT, &path, Some(&json!({ "merge_method": how }))).await?;
                    Ok(Sent { url: None, said: format!("merged ({how})") })
                }
                Write::Rerun => {
                    // The failed checks now (conditional, so free if unchanged).
                    let p = self.poll_now(repo, number).await?;
                    let mut runs: Vec<String> = Vec::new();
                    for c in p.checks.iter().filter(|c| c.state.red()) {
                        if let Some(r) = &c.run
                            && !runs.contains(&r.id)
                        {
                            runs.push(r.id.clone());
                        }
                    }
                    if runs.is_empty() {
                        return Err(Error::Http(
                            "no failed workflow runs to rerun (another app's checks rerun on its own page)".into(),
                        ));
                    }
                    for id in &runs {
                        self.send(post.clone(), &format!("repos/{repo}/actions/runs/{id}/rerun-failed-jobs"), None)
                            .await?;
                    }
                    let n = runs.len();
                    Ok(Sent {
                        url: None,
                        said: format!("reran the failed jobs of {n} workflow run{}", if n == 1 { "" } else { "s" }),
                    })
                }
                Write::Live { .. } => Err(Error::Http("live updates go through the block's hook".into())),
            }
        })
    }

    fn repo_urls<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<Vec<String>, Error>> {
        Box::pin(async move {
            let v = self.get(&format!("repos/{repo}")).await?;
            Ok(["ssh_url", "clone_url", "html_url"].iter().filter_map(|k| v[*k].as_str().map(str::to_owned)).collect())
        })
    }

    fn rerun_api(&self) -> bool {
        true
    }

    fn read_only(&self) -> Option<String> {
        self.token.via_app().map(|_| {
            "read-only: no gh login here, so it reads through Arugula control's GitHub App; writes go out as you, so they need your own login (`gh auth login`, then refresh)".to_owned()
        })
    }

    fn issue<'a>(&'a self, repo: &'a str, number: u64) -> BoxFuture<'a, Result<(Item, String), Error>> {
        Box::pin(async move {
            let raw = self.get(&format!("repos/{repo}/issues/{number}")).await?;
            if raw["pull_request"].is_object() {
                return Err(Error::NotFound(format!("{repo}#{number} is a pull request: open it as one")));
            }
            let who: Vec<&str> =
                raw["assignees"].as_array().into_iter().flatten().filter_map(|u| u["login"].as_str()).collect();
            let fp = json!([raw["updated_at"], raw["comments"], raw["state"], who]).to_string();
            Ok((issue_item(&raw), fp))
        })
    }

    #[allow(clippy::type_complexity)]
    fn issue_events<'a>(
        &'a self,
        repo: &'a str,
        number: u64,
    ) -> BoxFuture<'a, Result<(Vec<Event>, Vec<Linked>), Error>> {
        Box::pin(async move {
            let tl = Value::Array(
                self.newest(&format!("repos/{repo}/issues/{number}/timeline?per_page=100"), EVENTS).await?,
            );
            Ok((events(&tl, repo), linked(&tl)))
        })
    }

    fn pr_by_head<'a>(&'a self, repo: &'a str, branch: &'a str) -> BoxFuture<'a, Result<Option<Linked>, Error>> {
        Box::pin(async move {
            // GitHub filters by `owner:branch`, so a fork's isn't listed.
            let owner = repo.split('/').next().unwrap_or_default();
            let list = self.get(&format!("repos/{repo}/pulls?state=all&head={owner}:{branch}&per_page=10")).await?;
            Ok(list.as_array().into_iter().flatten().find(|p| p["head"]["ref"] == branch).map(|p| {
                let it = item(p, repo);
                Linked {
                    number: it.number,
                    title: it.title,
                    state: it.state,
                    url: it.url,
                    head: Some(branch.to_owned()),
                }
            }))
        })
    }

    fn new_issue<'a>(
        &'a self,
        repo: &'a str,
        title: &'a str,
        body: &'a str,
    ) -> BoxFuture<'a, Result<(u64, Sent), Error>> {
        Box::pin(async move {
            let req = json!({ "title": title, "body": body });
            let (v, _) = self.send(reqwest::Method::POST, &format!("repos/{repo}/issues"), Some(&req)).await?;
            let n = v["number"].as_u64().ok_or_else(|| Error::Http("GitHub didn't say the issue's number".into()))?;
            Ok((n, Sent { url: v["html_url"].as_str().map(str::to_owned), said: format!("opened #{n}") }))
        })
    }

    fn default_branch<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<String, Error>> {
        Box::pin(async move {
            let v = self.get(&format!("repos/{repo}")).await?;
            v["default_branch"]
                .as_str()
                .filter(|b| !b.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| Error::Http(format!("{repo} has no default branch")))
        })
    }

    fn rate(&self) -> Option<Value> {
        let why = self.holding();
        let l = self.limits.lock().unwrap();
        let low = l.remaining.is_some_and(|r| r < LOW) && l.reset.is_some_and(|r| r > epoch());
        let backoff = why.or_else(|| {
            low.then(|| {
                format!(
                    "GitHub's rate limit is low ({} of {} left, resets in {}): polling once a minute",
                    l.remaining.unwrap_or(0),
                    l.limit.unwrap_or(0),
                    mins(l.reset.unwrap_or(0).saturating_sub(epoch()))
                )
            })
        });
        Some(json!({
            "limit": l.limit, "remaining": l.remaining, "used": l.used, "reset": l.reset,
            "requests": l.requests, "not_modified": l.not_modified, "backoff": backoff,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::model::{Pr, Want, attention, rollup};

    fn fixture(dir: &str, f: &str) -> Value {
        let p = format!("{}/tests/fixtures/github/{dir}/{f}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&p).map(|s| serde_json::from_str(&s).unwrap()).unwrap_or(Value::Null)
    }

    /// A fixture read as the block reads it.
    fn pr_of(raw: &Value, dir: &str) -> Pr {
        let it = item(raw, "cli/cli");
        let checks = checks(&fixture(dir, "check_runs.json"), &fixture(dir, "statuses.json"));
        Pr {
            rollup: rollup(&checks),
            reviews: reviews(&fixture(dir, "reviews.json"), &it.head.sha),
            events: events(&fixture(dir, "timeline.json"), "cli/cli"),
            item: it,
            checks,
        }
    }

    fn pr(dir: &str) -> Pr {
        pr_of(&fixture(dir, "item.json"), dir)
    }

    fn me_as(login: &str, teams: &[&str]) -> Me {
        Me { login: login.into(), teams: teams.iter().map(|t| t.to_string()).collect() }
    }

    #[test]
    fn a_merged_pr_from_a_fork() {
        // #14519: waldyrious/cli → cli/cli, 18 check runs, no statuses.
        let p = pr("github-cli-cli-14519");
        let it = &p.item;
        assert_eq!((it.number, it.state, it.draft), (14519, ItemState::Merged, false));
        assert_eq!(it.head.repo.as_deref(), Some("waldyrious/cli"));
        assert_eq!(it.base.repo.as_deref(), Some("cli/cli"));
        assert_eq!(it.head_ref, "refs/pull/14519/head");
        assert_eq!(it.merge_base, None);
        assert!(it.requested.is_empty());
        assert_eq!(p.checks.len(), 18);
        assert!(p.checks.iter().all(|c| c.source == CheckSource::CheckRun));
        let n = |s: CheckState| p.checks.iter().filter(|c| c.state == s).count();
        assert_eq!((n(CheckState::Success), n(CheckState::Skipped)), (11, 7));
        // The combined status said "pending" with no statuses: not pending.
        assert_eq!(fixture("github-cli-cli-14519", "statuses.json")["state"], "pending");
        assert_eq!(p.rollup, Some(CheckState::Success));
        assert_eq!(
            p.checks[0].run,
            Some(RunRef { id: "36225027797".into(), job: Some("108357294367".into()) }),
            "an Actions run to rerun"
        );
        let st: Vec<ReviewState> = p.reviews.iter().map(|r| r.state).collect();
        assert_eq!(st.iter().filter(|s| **s == ReviewState::Commented).count(), 13);
        assert_eq!(st.iter().filter(|s| **s == ReviewState::ChangesRequested).count(), 1);
        assert_eq!(st.iter().filter(|s| **s == ReviewState::Approved).count(), 1);
        // Subscriptions and Copilot's markers aren't events; the rest are.
        assert!(p.events.iter().all(|e| e.what.as_deref() != Some("subscribed")));
        let kinds = |k: EventKind| p.events.iter().filter(|e| e.kind == k).count();
        assert_eq!(kinds(EventKind::Pushed), 5);
        assert_eq!(kinds(EventKind::Referenced), 9);
        assert_eq!(kinds(EventKind::Mentioned), 1);
        assert!(p.events.iter().any(|e| e.force));
        // Ids are stable and unique.
        let mut ids: Vec<&str> = p.events.iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), p.events.len());
        // Merged, by them: done. Mentioned williammartin: input, for him.
        let w = attention(&p, &me_as("waldyrious", &[]), 0);
        assert!(matches!(&w[..], [Want::Done { key, .. }] if key == "merged"), "{w:?}");
        let w = attention(&p, &me_as("williammartin", &[]), 0);
        assert!(matches!(&w[..], [Want::Mention { why, .. }] if why == "mentioned"), "{w:?}");
        let seen = p.events.iter().map(|e| e.at).max().unwrap();
        assert!(attention(&p, &me_as("williammartin", &[]), seen).is_empty());
        let text = p.text("cli/cli");
        assert!(
            text.contains("waldyrious wants to merge waldyrious/cli:document-search-operator-support into trunk"),
            "{text}"
        );
        assert!(text.contains("skipped   "), "{text}");
    }

    #[test]
    fn a_review_asked_and_red_checks_blocked() {
        // #13788: babakks asked; 3 builds red; branch protection blocks it.
        let p = pr("github-cli-cli-13788");
        assert_eq!(p.item.requested, [Reviewer::User("babakks".into())]);
        assert!(p.item.blocked);
        assert_eq!(p.item.mergeable, Some(true));
        assert_eq!(p.rollup, Some(CheckState::Failure));
        let w = attention(&p, &me_as("babakks", &[]), 0);
        assert!(matches!(&w[..], [Want::Review { why }] if why == "review requested from you"), "{w:?}");
        match &attention(&p, &me_as("happysnaker", &[]), 0)[..] {
            [Want::Failed { why, checks }] => {
                assert_eq!(why, "3 checks failed: build (ubuntu-latest), build (windows-latest), build (macos-latest)");
                assert!(checks.iter().all(|c| c.run.as_ref().is_some_and(|r| r.id == "28656994029")), "{checks:?}");
            }
            w => panic!("{w:?}"),
        }
        // Green, but blocked: not done. Unblocked: done.
        let mut raw = fixture("github-cli-cli-13788", "item.json");
        let mut green = pr_of(&raw, "github-cli-cli-13788");
        green.checks.retain(|c| c.state == CheckState::Success);
        green.rollup = rollup(&green.checks);
        assert!(attention(&green, &me_as("happysnaker", &[]), 0).is_empty(), "blocked isn't ready");
        raw["mergeable_state"] = json!("clean");
        green.item = item(&raw, "cli/cli");
        let w = attention(&green, &me_as("happysnaker", &[]), 0);
        assert!(matches!(&w[..], [Want::Done { why, .. }] if why == "checks green"), "{w:?}");
    }

    #[test]
    fn changes_requested_rerequested_and_mentions() {
        // #13899: BagToad asked for changes and is asked again.
        let p = pr("github-cli-cli-13899");
        assert_eq!(p.item.requested, [Reviewer::User("BagToad".into())]);
        let w = attention(&p, &me_as("BagToad", &[]), 0);
        assert!(matches!(w.first(), Some(Want::Review { .. })), "{w:?}");
        assert!(w.iter().any(|w| matches!(w, Want::Mention { .. })), "GitHub's mentioned event: {w:?}");
        let w = attention(&p, &me_as("imkp1", &[]), 0);
        assert!(w.iter().any(|w| matches!(w, Want::Changes { why } if why == "changes requested by BagToad")), "{w:?}");
        // A team asked, through the timeline: org/slug from its page.
        let req = p.events.iter().find(|e| e.target == Some(Reviewer::Team("cli/code-reviewers".into())));
        assert!(req.is_some_and(|e| e.kind == EventKind::ReviewRequested));
        let mention = p.events.iter().find(|e| e.kind == EventKind::Mentioned).unwrap();
        assert_eq!(mention.actor, None);
        assert!(mention.line().ends_with("someone mentioned imkp1"), "{}", mention.line());
    }

    #[test]
    fn a_team_request_is_yours_if_youre_in_it() {
        let mut raw = fixture("github-cli-cli-13788", "item.json");
        raw["requested_reviewers"] = json!([]);
        raw["requested_teams"] = json!([{ "slug": "code-reviewers", "name": "code reviewers",
            "html_url": "https://github.com/orgs/cli/teams/code-reviewers" }, { "slug": "docs" }]);
        let p = pr_of(&raw, "github-cli-cli-13788");
        assert_eq!(p.item.requested, [Reviewer::Team("cli/code-reviewers".into()), Reviewer::Team("cli/docs".into())]);
        let mine = me(
            &json!({ "login": "BagToad" }),
            &json!([{ "slug": "code-reviewers", "organization": { "login": "cli" } }]),
        );
        assert_eq!(mine.teams, ["cli/code-reviewers"]);
        let w = attention(&p, &mine, 0);
        assert!(matches!(&w[..], [Want::Review { why }] if why == "review requested from cli/code-reviewers"), "{w:?}");
        let other = me(
            &json!({ "login": "x" }),
            &json!([{ "slug": "code-reviewers", "organization": { "login": "elsewhere" } }]),
        );
        assert!(attention(&p, &other, 0).is_empty());
        // The fingerprint moves with the request.
        let mut b = raw.clone();
        b["requested_teams"] = json!([]);
        assert_ne!(fingerprint(&raw), fingerprint(&b));
        assert_eq!(fingerprint(&raw), fingerprint(&raw.clone()));
    }

    /// The states the fixtures don't have, in GitHub's shapes.
    #[test]
    fn every_review_and_check_state() {
        let list = json!([
            { "id": 1, "state": "COMMENTED", "user": { "login": "a" }, "commit_id": "old", "submitted_at": "2026-10-01T01:00:00Z" },
            { "id": 2, "state": "CHANGES_REQUESTED", "user": { "login": "b" }, "commit_id": "head", "submitted_at": "2026-10-01T02:00:00Z", "body": "x" },
            { "id": 3, "state": "APPROVED", "user": { "login": "c" }, "commit_id": "head", "submitted_at": "2026-10-01T03:00:00Z" },
            { "id": 4, "state": "DISMISSED", "user": { "login": "d" }, "commit_id": "head" },
            { "id": 5, "state": "PENDING", "user": null },
        ]);
        let rs = reviews(&list, "head");
        use ReviewState::*;
        assert_eq!(
            rs.iter().map(|r| r.state).collect::<Vec<_>>(),
            [Commented, ChangesRequested, Approved, Dismissed, Pending]
        );
        assert_eq!(rs.iter().map(|r| r.stale).collect::<Vec<_>>(), [true, false, false, false, false]);
        assert_eq!(rs[4].author, None);
        let run = |status: &str, conclusion: Value| json!({ "name": format!("{status} {conclusion}"), "status": status, "conclusion": conclusion, "app": { "slug": "github-actions" } });
        let runs = json!({ "check_runs": [
            run("queued", Value::Null), run("waiting", Value::Null), run("in_progress", Value::Null),
            run("completed", json!("success")), run("completed", json!("failure")), run("completed", json!("timed_out")),
            run("completed", json!("startup_failure")), run("completed", json!("cancelled")), run("completed", json!("skipped")),
            run("completed", json!("neutral")), run("completed", json!("stale")), run("completed", json!("action_required")),
        ] });
        let st = json!({ "state": "failure", "total_count": 4, "statuses": [
            { "context": "s1", "state": "success" }, { "context": "s2", "state": "failure", "target_url": "https://ci/x" },
            { "context": "s3", "state": "error" }, { "context": "s4", "state": "pending" },
        ] });
        let cs = checks(&runs, &st);
        use CheckState::*;
        assert_eq!(
            cs.iter().map(|c| c.state).collect::<Vec<_>>(),
            [
                Queued,
                Queued,
                Running,
                Success,
                Failure,
                Failure,
                Failure,
                Cancelled,
                Skipped,
                Neutral,
                Neutral,
                ActionRequired,
                Success,
                Failure,
                Failure,
                Running
            ]
        );
        assert_eq!(cs[13].source, CheckSource::Status);
        assert_eq!(cs[13].url.as_deref(), Some("https://ci/x"));
        // No statuses at all: "pending" says nothing.
        let none = json!({ "state": "pending", "total_count": 0, "statuses": [] });
        let only = checks(&json!({ "check_runs": [run("completed", json!("success"))] }), &none);
        assert_eq!(rollup(&only), Some(Success));
        assert_eq!(rollup(&checks(&json!({ "check_runs": [] }), &none)), None);
        // A check run from another app has no Actions run to rerun.
        let other = json!({ "check_runs": [{ "name": "ci", "status": "completed", "conclusion": "failure",
            "app": { "slug": "circleci" }, "details_url": "https://ci/actions/runs/1/job/2" }] });
        assert_eq!(checks(&other, &none)[0].run, None);
        assert_eq!(
            actions_run("https://github.com/o/r/actions/runs/12/job/34?pr=1"),
            Some(RunRef { id: "12".into(), job: Some("34".into()) })
        );
        assert_eq!(actions_run("https://github.com/o/r/actions/runs/12"), Some(RunRef { id: "12".into(), job: None }));
        assert_eq!(actions_run("https://example.com/x"), None);
    }

    #[test]
    fn issues_in_githubs_shapes() {
        assert_eq!(
            issue_url("https://github.com/cli/cli/issues/12"),
            Some(("github.com".into(), "cli/cli".into(), 12))
        );
        assert_eq!(issue_url("https://git.example/o/r/issues/12"), None, "only GitHub's own host");
        let it = issue_item(
            &json!({ "number": 12, "state": "open", "title": "T", "html_url": "https://github.com/o/r/issues/12",
            "user": { "login": "sam" }, "assignees": [{ "login": "me" }], "labels": [{ "name": "bug" }] }),
        );
        assert_eq!(
            (it.kind, it.state, it.assignees.as_slice()),
            (ItemKind::Issue, ItemState::Open, &["me".to_owned()][..])
        );
        let tl = json!([
            { "event": "assigned", "id": 1, "actor": { "login": "sam" }, "assignee": { "login": "me" }, "created_at": "2026-10-01T00:00:00Z" },
            { "event": "cross-referenced", "actor": { "login": "me" }, "created_at": "2026-10-02T00:00:00Z",
              "source": { "type": "issue", "issue": { "number": 13, "title": "Fix it", "state": "closed",
                "html_url": "https://github.com/o/r/pull/13", "pull_request": { "merged_at": "2026-10-02T01:00:00Z" } } } },
            { "event": "cross-referenced", "actor": { "login": "x" }, "created_at": "2026-10-02T00:00:00Z",
              "source": { "type": "issue", "issue": { "number": 9, "title": "Another issue", "state": "open" } } },
        ]);
        let ev = events(&tl, "o/r");
        assert_eq!(ev[0].target, Some(Reviewer::User("me".into())));
        let l = linked(&tl);
        assert_eq!(l.iter().map(|l| (l.number, l.state)).collect::<Vec<_>>(), [(13, ItemState::Merged)]);
        let issue = crate::forge::model::Issue { item: it, events: ev, linked: l };
        let w = crate::forge::model::issue_attention(&issue, &me_as("me", &[]), 0);
        assert!(
            w.iter()
                .any(|w| matches!(w, crate::forge::model::Want::Assigned { why } if why == "assigned to you by sam")),
            "{w:?}"
        );
    }

    #[test]
    fn links_hosts_and_writes() {
        assert_eq!(
            pr_url("https://github.com/cli/cli/pull/14519"),
            Some(("github.com".into(), "cli/cli".into(), 14519))
        );
        assert_eq!(
            pr_url("https://github.com/cli/cli/pull/14519/files#diff"),
            Some(("github.com".into(), "cli/cli".into(), 14519))
        );
        assert_eq!(pr_url("https://ghe.example.com/o/r/pull/3"), Some(("ghe.example.com".into(), "o/r".into(), 3)));
        assert_eq!(pr_url("https://git.inevitable.fyi/o/r/pulls/3"), None, "Forgejo's");
        assert_eq!(pr_url("https://github.com/cli/cli/issues/3"), None);
        assert!(is_github_host("GitHub.com") && is_github_host("ssh.github.com") && !is_github_host("ghe.example.com"));
        assert_eq!(api_for("ghe.example.com"), "https://ghe.example.com/api/v3");
        assert_eq!(host_of_api("https://api.github.com"), "github.com");
        assert_eq!(host_of_api("https://ghe.example.com/api/v3"), "ghe.example.com");
        assert_eq!(review_event(ReviewEvent::Approve), "APPROVE");
        assert_eq!(merge_method(None), Ok("merge"));
        assert_eq!(merge_method(Some("squash")), Ok("squash"));
        assert!(merge_method(Some("fast-forward-only")).is_err());
        let link = r#"<https://api.github.com/repositories/1/issues/2/timeline?per_page=100&page=2>; rel="next", <https://api.github.com/repositories/1/issues/2/timeline?per_page=100&page=4>; rel="last""#;
        let last = link_last(link).unwrap();
        assert_eq!(page_of(&last), Some(4));
        assert_eq!(page_of(&with_page(&last, 3)), Some(3));
        assert!(with_page(&last, 3).contains("per_page=100"));
    }
}
