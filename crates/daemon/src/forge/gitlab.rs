//! The GitLab adapter (M39): merge requests onto the normalized model, and
//! their writes. Requests go through the daemon's own HTTP client with a
//! token from the person's `glab` (`glab config get token --host H`, run
//! with #74's shell environment), held in memory only and asked for again
//! after a 401. With no `glab` login for the host, a public project is
//! still read anonymously, and the block is read-only.
//!
//! What's particular to GitLab (S23):
//!
//! - **Merge requests by `iid`**, in a project named by its URL-encoded path
//!   (`group/sub/proj` → `group%2Fsub%2Fproj`), under `https://H/api/v4`.
//! - **ETags on everything, but a 304 still counts** against gitlab.com's
//!   rate limit. A poll is the MR (conditional) and, when its head pipeline
//!   moved, that pipeline's jobs. Approvals, reviewers and discussions are
//!   read again only when the MR's fingerprint moves: 5 requests for a
//!   whole read, 1 for an unchanged poll.
//! - **Checks are the head pipeline's jobs** (`allow_failure` kept, manual
//!   jobs not counted). The head pipeline often runs on a merge commit
//!   (merged results), not the MR's `sha`, and can be in the fork's project.
//!   `POST pipelines/ID/retry` reruns it, so failed checks offer *Rerun*.
//! - **Reviews are two things:** each reviewer's own state (`unreviewed`
//!   and `review_started` are pending requests; `reviewed`, `approved`,
//!   `requested_changes`) and approvals, which anyone eligible gives
//!   without being a reviewer.
//! - **The timeline is discussions:** notes, and `system` notes for events
//!   (pushes, review requests, merges). They need a login even on a public
//!   project (401 anonymously), so a read-only block has no timeline.
//! - **The merge base is `diff_refs.base_sha`,** never `start_sha` (the
//!   target's tip: 130 files instead of 4 in S23).
//! - **Request changes:** the REST API has no call that sets a reviewer's
//!   `requested_changes` (GitLab does it from the web UI's review submit),
//!   so `review {event: request_changes}` posts the text as a note and says
//!   so.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::{
    Adapter, Error, Polled, ReviewEvent, Sent, Write,
    forgejo::time,
    model::{
        Branch, Check, CheckSource, CheckState, Event, EventKind, Item, ItemKind, ItemState, Me, Review, ReviewState,
        Reviewer, RunRef,
    },
};
use crate::review::Runner;

/// How many notes are kept in the state (the rest are in the block's log).
pub const EVENTS: usize = super::forgejo::EVENTS;
/// Pages of a pipeline's jobs read at most (100 a page).
const JOB_PAGES: u64 = 5;

/// What the block says when it reads with no login.
pub fn read_only_note(host: &str, why: &str) -> String {
    format!(
        "read-only: no glab login for {host} ({why}). Reading it anonymously: no discussions, no \"you\", no writes. `glab auth login --hostname {host}`, then refresh"
    )
}

fn t(v: &Value) -> Option<i64> {
    v.as_str().and_then(time)
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

fn user(v: &Value) -> Option<String> {
    v["username"].as_str().filter(|l| !l.is_empty()).map(str::to_owned)
}

/// A project's path as one URL segment.
pub fn project(path: &str) -> String {
    path.trim_matches('/').replace('%', "%25").replace('/', "%2F")
}

/// The site's address, from an API base (`https://h/api/v4` → `https://h`).
pub fn site(api: &str) -> String {
    api.trim_end_matches('/').trim_end_matches("/api/v4").to_owned()
}

/// A host that's GitLab by its name alone (the rest are GitLab when `glab`
/// knows them, or when a link says so).
pub fn known_host(host: &str) -> bool {
    let h = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    h == "gitlab.com" || h.starts_with("gitlab.")
}

/// `https://H[/prefix]/G/[SUB/…]P/-/merge_requests/N[/…]` → (host, project
/// path, N, API base). `None` when it isn't a merge request's address.
pub fn parse_mr_url(u: &str) -> Option<(String, String, u64, String)> {
    let url = url::Url::parse(u).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = super::login::url_host(u)?;
    let segs: Vec<&str> = url.path_segments()?.filter(|x| !x.is_empty()).collect();
    let at = segs.windows(2).position(|w| w == ["-", "merge_requests"])?;
    let n: u64 = segs.get(at + 2)?.parse().ok()?;
    if at < 2 {
        return None; // a project needs a namespace and a name
    }
    let repo = segs[..at].join("/");
    Some((host.clone(), repo, n, format!("{}://{host}/api/v4", url.scheme())))
}

/// Whether a title marks a draft (GitLab sets `draft` from these too; this
/// covers older servers' `work_in_progress`).
fn draft_title(title: &str) -> bool {
    let l = title.trim_start().to_ascii_lowercase();
    ["draft:", "[draft]", "(draft)", "wip:", "[wip]"].iter().any(|p| l.starts_with(p))
}

/// `detailed_merge_status` (and `has_conflicts`) as (mergeable, blocked),
/// for an open MR.
pub fn merge_status(it: &Value) -> (Option<bool>, bool) {
    if it["state"] != "opened" {
        return (None, false);
    }
    if it["has_conflicts"] == true {
        return (Some(false), false);
    }
    match it["detailed_merge_status"].as_str() {
        Some("mergeable") => (Some(true), false),
        Some("conflict" | "need_rebase" | "broken_status") => (Some(false), false),
        None | Some("checking" | "unchecked" | "preparing" | "approvals_syncing" | "not_open") => (None, false),
        // The checks and the draft flag say these themselves.
        Some("ci_must_pass" | "ci_still_running" | "draft_status" | "commits_status") => (Some(true), false),
        // not_approved, discussions_not_resolved, blocked_status,
        // requested_changes, external_status_checks, merge_time, …
        Some(_) => (Some(true), true),
    }
}

/// `GET projects/P/merge_requests/N`. `requested` is filled in from the
/// reviewers.
pub fn item(it: &Value) -> Item {
    let state = match it["state"].as_str() {
        Some("merged") => ItemState::Merged,
        Some("closed" | "locked") => ItemState::Closed,
        _ => ItemState::Open,
    };
    let number = it["iid"].as_u64().unwrap_or(0);
    let full = it["references"]["full"].as_str().unwrap_or_default();
    let repo = full.rsplit_once('!').map(|(r, _)| r.to_owned()).filter(|r| !r.is_empty());
    let fork = it["source_project_id"] != it["target_project_id"] && !it["source_project_id"].is_null();
    let (mergeable, blocked) = merge_status(it);
    let title = s(&it["title"]);
    Item {
        kind: ItemKind::Pr,
        number,
        url: s(&it["web_url"]),
        draft: it["draft"] == true || it["work_in_progress"] == true || draft_title(&title),
        title,
        body: s(&it["description"]),
        author: user(&it["author"]).unwrap_or_default(),
        state,
        labels: it["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().or(l["name"].as_str()).map(str::to_owned))
            .collect(),
        assignees: it["assignees"].as_array().into_iter().flatten().filter_map(user).collect(),
        base: Branch { repo: repo.clone(), branch: s(&it["target_branch"]), sha: s(&it["diff_refs"]["start_sha"]) },
        head: Branch {
            repo: if fork { Some(format!("project {}", it["source_project_id"])) } else { repo },
            branch: s(&it["source_branch"]),
            sha: s(&it["sha"]),
        },
        head_ref: format!("refs/merge-requests/{number}/head"),
        // Never start_sha: that's the target's tip (S23).
        merge_base: it["diff_refs"]["base_sha"].as_str().filter(|m| !m.is_empty()).map(str::to_owned),
        mergeable,
        blocked,
        requested: vec![],
        comments: it["user_notes_count"].as_u64().unwrap_or(0),
        updated_at: t(&it["updated_at"]).unwrap_or(0),
        merged_at: t(&it["merged_at"]),
        merged_by: user(&it["merged_by"]).or_else(|| user(&it["merge_user"])),
    }
}

/// What a poll compares: the MR's moving parts (its pipeline is checked
/// apart, with the jobs).
pub fn fingerprint(it: &Value) -> String {
    let rv: Vec<&str> =
        it["reviewers"].as_array().into_iter().flatten().filter_map(|u| u["username"].as_str()).collect();
    json!([
        it["updated_at"],
        it["sha"],
        it["user_notes_count"],
        it["state"],
        it["detailed_merge_status"],
        it["has_conflicts"],
        rv
    ])
    .to_string()
}

/// The head pipeline: `(project, id, what moves when it does)`.
pub fn head_pipeline(it: &Value) -> Option<(u64, u64, String)> {
    let p = &it["head_pipeline"];
    let id = p["id"].as_u64()?;
    let project = p["project_id"].as_u64().or(it["project_id"].as_u64())?;
    Some((project, id, json!([p["status"], p["updated_at"], p["finished_at"]]).to_string()))
}

/// GitLab's job states as checks'.
pub fn job_state(s: &str) -> CheckState {
    match s {
        "success" => CheckState::Success,
        "failed" => CheckState::Failure,
        "canceled" | "canceling" => CheckState::Cancelled,
        "skipped" => CheckState::Skipped,
        "manual" => CheckState::Manual,
        "created" | "pending" | "waiting_for_resource" | "preparing" | "scheduled" | "waiting_for_callback" => {
            CheckState::Queued
        }
        "running" => CheckState::Running,
        _ => CheckState::Neutral,
    }
}

/// `GET projects/P/pipelines/ID/jobs`: the jobs as checks.
pub fn checks(jobs: &Value, pipeline: u64) -> Vec<Check> {
    jobs.as_array()
        .into_iter()
        .flatten()
        .map(|j| Check {
            name: s(&j["name"]),
            source: CheckSource::PipelineJob,
            state: job_state(j["status"].as_str().unwrap_or_default()),
            allow_failure: j["allow_failure"] == true,
            url: j["web_url"].as_str().map(str::to_owned),
            description: j["stage"].as_str().filter(|d| !d.is_empty()).map(str::to_owned),
            run: Some(RunRef {
                id: j["pipeline"]["id"].as_u64().unwrap_or(pipeline).to_string(),
                job: j["id"].as_u64().map(|i| i.to_string()),
            }),
        })
        .collect()
}

/// `GET merge_requests/N/reviewers` and `…/approvals`: the reviews, and
/// the requests still pending.
pub fn reviews(reviewers: &Value, approvals: &Value) -> (Vec<Review>, Vec<Reviewer>) {
    let approved: Vec<(String, Option<i64>, String)> = approvals["approved_by"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some((user(&a["user"])?, t(&a["approved_at"]), a["user"]["id"].as_u64().unwrap_or(0).to_string()))
        })
        .collect();
    let mut out = Vec::new();
    let mut requested = Vec::new();
    for r in reviewers.as_array().into_iter().flatten() {
        let Some(who) = user(&r["user"]) else { continue };
        let at = approved.iter().find(|(u, _, _)| *u == who).and_then(|(_, at, _)| *at).or(t(&r["created_at"]));
        let state = match r["state"].as_str().unwrap_or_default() {
            "unreviewed" | "review_started" => {
                // An approval without a review submitted still counts.
                if approved.iter().any(|(u, _, _)| *u == who) {
                    ReviewState::Approved
                } else {
                    requested.push(Reviewer::User(who));
                    continue;
                }
            }
            "approved" => ReviewState::Approved,
            "requested_changes" => ReviewState::ChangesRequested,
            _ => ReviewState::Commented, // reviewed
        };
        out.push(Review {
            id: format!("gl-rv-{}", r["user"]["id"].as_u64().unwrap_or(0)),
            author: Some(who),
            state,
            commit: None,
            stale: false,
            at,
            body: None,
            url: None,
        });
    }
    // Approvals by people who aren't reviewers.
    for (who, at, id) in approved {
        if out.iter().any(|r| r.author.as_deref() == Some(&who)) {
            continue;
        }
        out.push(Review {
            id: format!("gl-ap-{id}"),
            author: Some(who),
            state: ReviewState::Approved,
            commit: None,
            stale: false,
            at,
            body: None,
            url: None,
        });
    }
    (out, requested)
}

/// The first `@name` in a system note (whom a request is for).
fn first_mention(body: &str) -> Option<String> {
    let at = body.find('@')?;
    let name: String =
        body[at + 1..].chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.')).collect();
    let name = name.trim_end_matches('.').to_owned();
    (!name.is_empty()).then_some(name)
}

/// A system note as an event: its kind, and how many commits a push added.
fn system(body: &str) -> (EventKind, Option<u32>) {
    let first = body.lines().next().unwrap_or_default().trim();
    let l = first.to_ascii_lowercase();
    if let Some(rest) = l.strip_prefix("added ")
        && rest.contains("commit")
    {
        let n = rest.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(1);
        return (EventKind::Pushed, Some(n));
    }
    let kind = if l.starts_with("requested review from") {
        EventKind::ReviewRequested
    } else if l.starts_with("removed review request") {
        EventKind::ReviewRequestRemoved
    } else if l.starts_with("mentioned in") {
        EventKind::Referenced
    } else if l == "merged" || l.starts_with("merged ") {
        EventKind::Merged
    } else if l == "closed" || l.starts_with("closed ") {
        EventKind::Closed
    } else if l == "reopened" || l.starts_with("reopened ") {
        EventKind::Reopened
    } else {
        EventKind::Other
    };
    (kind, None)
}

/// `GET merge_requests/N/discussions`: every note, oldest first, the
/// newest [`EVENTS`] of them.
pub fn events(discussions: &Value) -> Vec<Event> {
    let mut out: Vec<Event> = discussions
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|d| d["notes"].as_array().into_iter().flatten())
        .map(|n| {
            let body = n["body"].as_str().unwrap_or_default();
            let id = format!("gl-{}", n["id"].as_u64().unwrap_or(0));
            let at = t(&n["created_at"]).unwrap_or(0);
            let actor = user(&n["author"]);
            if n["system"] == true {
                // An event, in GitLab's words: never a mention of anyone.
                let (kind, commits) = system(body);
                let first = body.lines().next().unwrap_or_default().trim().to_owned();
                return Event {
                    id,
                    at,
                    actor,
                    kind,
                    target: (kind == EventKind::ReviewRequested)
                        .then(|| first_mention(&first))
                        .flatten()
                        .map(Reviewer::User),
                    what: Some(first),
                    body: None,
                    commits,
                    force: false,
                };
            }
            let code = !n["position"].is_null() || n["type"] == "DiffNote";
            Event {
                id,
                at,
                actor,
                kind: if code { EventKind::ReviewComment } else { EventKind::Commented },
                what: None,
                target: None,
                body: Some(body.to_owned()).filter(|b| !b.is_empty()),
                commits: None,
                force: false,
            }
        })
        .collect();
    out.sort_by_key(|e| e.at);
    let keep = out.len().saturating_sub(EVENTS);
    out.split_off(keep)
}

/// `GET user`.
pub fn me(u: &Value) -> Me {
    Me { login: user(u).unwrap_or_default(), teams: vec![] }
}

// ---------------------------------------------------------------- glab

/// Asks the person's `glab` whether it knows `$1` (gitlab.com, its default
/// host, or a host in its config) and for that host's token. Prints
/// `arugula-no-glab`, or `known yes|no` and a `token=` line.
const GLAB: &str = r#"command -v glab >/dev/null 2>&1 || { echo arugula-no-glab; exit 0; }
h=$1; known=no
[ "$h" = gitlab.com ] && known=yes
[ "$(glab config get host 2>/dev/null)" = "$h" ] && known=yes
cfg="${GLAB_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/glab-cli}/config.yml"
if [ $known = no ] && [ -f "$cfg" ]; then
  sed -n '/^hosts:/,/^[^ #]/p' "$cfg" | sed -n 's/^ \{1,4\}["'\'']\{0,1\}\([^ #"'\''][^"'\'']*\)["'\'']\{0,1\}:[[:space:]]*$/\1/p' | grep -qxF "$h" && known=yes
fi
echo "known $known"
[ $known = yes ] && printf 'token=%s\n' "$(glab config get token --host "$h" 2>/dev/null)"
exit 0"#;

/// What `glab` said about a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Glab {
    /// No `glab` on the block's host.
    Missing,
    /// It doesn't know the host, or has no token for it.
    NoLogin,
    Token(String),
}

pub fn parse_glab(out: &[u8]) -> Glab {
    let out = String::from_utf8_lossy(out);
    if out.starts_with("arugula-no-glab") {
        return Glab::Missing;
    }
    out.lines()
        .find_map(|l| l.strip_prefix("token=").map(str::trim).filter(|t| !t.is_empty()))
        .map_or(Glab::NoLogin, |t| Glab::Token(t.to_owned()))
}

/// Tokens held while the daemon runs, by host. Memory only.
static TOKENS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Mutex::default);

/// Where the adapter's token comes from: `glab`, on the block's host.
#[derive(Clone)]
pub struct GlabToken {
    runner: Runner,
    host: String,
}

impl GlabToken {
    pub async fn ask(runner: &Runner, host: &str) -> Result<Glab, String> {
        let (out, _) = runner.sh(GLAB, &[host.to_owned()]).await?;
        Ok(parse_glab(&out))
    }

    /// The token; `fresh`: the last one was refused, ask glab again.
    pub async fn get(&self, fresh: bool) -> Result<String, String> {
        if !fresh && let Some(t) = TOKENS.lock().unwrap().get(&self.host).cloned() {
            return Ok(t);
        }
        match Self::ask(&self.runner, &self.host).await? {
            Glab::Token(t) => {
                TOKENS.lock().unwrap().insert(self.host.clone(), t.clone());
                Ok(t)
            }
            Glab::Missing => Err("glab is gone from this machine: install it and `glab auth login`".into()),
            Glab::NoLogin => {
                Err(format!("glab has no token for {} any more: `glab auth login --hostname {}`", self.host, self.host))
            }
        }
    }
}

/// How a GitLab block connects: with glab's token, or anonymously.
pub struct Connected {
    pub adapter: Gitlab,
    /// `glab:HOST` when it has a login.
    pub login: Option<String>,
}

/// The adapter for `host` (its API base `api`), through the person's glab
/// when it knows the host, else anonymous and read-only.
pub async fn connect(runner: Runner, host: &str, api: &str, http: reqwest::Client) -> Connected {
    let said = GlabToken::ask(&runner, host).await;
    let why = match said {
        Ok(Glab::Token(t)) => {
            TOKENS.lock().unwrap().insert(host.to_owned(), t);
            let token = GlabToken { runner, host: host.to_owned() };
            return Connected {
                adapter: Gitlab::new(api, http, Some(token), None),
                login: Some(format!("glab:{host}")),
            };
        }
        Ok(Glab::Missing) => "glab isn't installed here".to_owned(),
        Ok(Glab::NoLogin) => "glab doesn't know this host".to_owned(),
        Err(e) => format!("couldn't run glab: {e}"),
    };
    Connected { adapter: Gitlab::new(api, http, None, Some(read_only_note(host, &why))), login: None }
}

// ---------------------------------------------------------------- the adapter

/// A GitLab instance, through glab's token or none.
pub struct Gitlab {
    pub api: String,
    http: reqwest::Client,
    token: Option<GlabToken>,
    read_only: Option<String>,
    /// Conditional GETs: path → (ETag, the answer it was for).
    etags: Mutex<HashMap<String, (String, Value)>>,
    /// The head pipeline's jobs, and what they were read for.
    jobs: Mutex<Option<(String, Vec<Check>)>>,
    /// Requests made (a 304 counts on gitlab.com, so it does here).
    pub requests: std::sync::atomic::AtomicU64,
}

struct Answer {
    v: Value,
    pages: Option<u64>,
}

impl Gitlab {
    pub fn new(api: &str, http: reqwest::Client, token: Option<GlabToken>, read_only: Option<String>) -> Self {
        Self {
            api: api.trim_end_matches('/').to_owned(),
            http,
            token,
            read_only,
            etags: Mutex::default(),
            jobs: Mutex::default(),
            requests: Default::default(),
        }
    }

    fn refused(&self) -> Error {
        Error::Login(self.read_only.clone().unwrap_or_else(|| "no glab login".into()))
    }

    /// One request, with the token if there is one; on a 401 the token is
    /// asked for once more. A GET is conditional when it was read before.
    async fn send(&self, method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Answer, Error> {
        let url = format!("{}/{}", self.api, path.trim_start_matches('/'));
        let get = method == reqwest::Method::GET;
        for attempt in 0..2 {
            let mut req = self.http.request(method.clone(), &url).header("Accept", "application/json");
            if let Some(src) = &self.token {
                let token = src.get(attempt > 0).await.map_err(Error::Login)?;
                req = req.header("Authorization", format!("Bearer {token}"));
            }
            let cached = get.then(|| self.etags.lock().unwrap().get(path).cloned()).flatten();
            if let Some((etag, _)) = &cached {
                req = req.header("If-None-Match", etag);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            self.requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let res = req.send().await.map_err(|e| Error::Http(format!("{}: {}", super::redact(&url), short(&e))))?;
            let status = res.status();
            tracing::debug!(url = super::redact(&url), status = status.as_u16(), "gitlab request");
            let pages = res.headers().get("x-total-pages").and_then(|v| v.to_str().ok()?.parse().ok());
            if status == reqwest::StatusCode::NOT_MODIFIED
                && let Some((_, v)) = cached
            {
                return Ok(Answer { v, pages });
            }
            if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 && self.token.is_some() {
                continue;
            }
            let etag = res.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_owned);
            let text = res.text().await.unwrap_or_default();
            if !status.is_success() {
                let said = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| {
                        let m = v.get("message").or(v.get("error"))?;
                        Some(m.as_str().map(str::to_owned).unwrap_or_else(|| m.to_string()))
                    })
                    .unwrap_or_else(|| text.chars().take(200).collect());
                let anon = if self.token.is_none() { " (read anonymously: no glab login)" } else { "" };
                return Err(match status.as_u16() {
                    401 | 403 => Error::Denied(format!("{} {said}{anon}", status.as_u16())),
                    404 => Error::NotFound(format!("{said}{anon}")),
                    c => Error::Http(format!("{c} {said}")),
                });
            }
            let v =
                if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
            if get && let Some(e) = etag {
                self.etags.lock().unwrap().insert(path.to_owned(), (e, v.clone()));
            }
            return Ok(Answer { v, pages });
        }
        unreachable!("the loop returns")
    }

    async fn get(&self, path: &str) -> Result<Answer, Error> {
        self.send(reqwest::Method::GET, path, None).await
    }

    fn mr(repo: &str, number: u64) -> String {
        format!("projects/{}/merge_requests/{number}", project(repo))
    }

    /// The head pipeline's jobs, read again only when it moved.
    async fn jobs(&self, raw: &Value) -> Result<Vec<Check>, Error> {
        let Some((project, id, moved)) = head_pipeline(raw) else { return Ok(vec![]) };
        let key = format!("{project}/{id} {moved}");
        if let Some((k, c)) = self.jobs.lock().unwrap().as_ref()
            && *k == key
        {
            return Ok(c.clone());
        }
        let path = format!("projects/{project}/pipelines/{id}/jobs?per_page=100");
        let first = self.get(&path).await?;
        let mut all = first.v.as_array().cloned().unwrap_or_default();
        for page in 2..=first.pages.unwrap_or(1).min(JOB_PAGES) {
            all.extend(self.get(&format!("{path}&page={page}")).await?.v.as_array().cloned().unwrap_or_default());
        }
        let c = checks(&Value::Array(all), id);
        *self.jobs.lock().unwrap() = Some((key, c.clone()));
        Ok(c)
    }

    /// Every discussion, or the last pages of them.
    async fn discussions(&self, mr: &str) -> Result<Value, Error> {
        let path = format!("{mr}/discussions?per_page=100");
        let first = self.get(&path).await?;
        let pages = first.pages.unwrap_or(1);
        if pages <= 1 {
            return Ok(first.v);
        }
        // Oldest first: the newest are on the last page (and the one
        // before, so a short last page still gives enough).
        let from = (pages - 1).max(1);
        let mut all = if from == 1 { first.v.as_array().cloned().unwrap_or_default() } else { vec![] };
        for page in from.max(2)..=pages {
            all.extend(self.get(&format!("{path}&page={page}")).await?.v.as_array().cloned().unwrap_or_default());
        }
        Ok(Value::Array(all))
    }

    async fn note(&self, repo: &str, number: u64, body: &str) -> Result<Option<String>, Error> {
        let v = self
            .send(reqwest::Method::POST, &format!("{}/notes", Self::mr(repo, number)), Some(&json!({ "body": body })))
            .await?
            .v;
        let site = site(&self.api);
        Ok(v["id"].as_u64().map(|id| format!("{site}/{repo}/-/merge_requests/{number}#note_{id}")))
    }
}

fn short(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(x) = src {
        s = x.to_string();
        src = x.source();
    }
    s
}

impl Adapter for Gitlab {
    fn me(&self) -> BoxFuture<'_, Result<Me, Error>> {
        Box::pin(async move {
            if self.token.is_none() {
                // Anonymous: nobody is "you".
                return Ok(Me::default());
            }
            Ok(me(&self.get("user").await?.v))
        })
    }

    fn poll<'a>(&'a self, repo: &'a str, number: u64) -> BoxFuture<'a, Result<Polled, Error>> {
        Box::pin(async move {
            let raw = self.get(&Self::mr(repo, number)).await?.v;
            let checks = self.jobs(&raw).await?;
            Ok(Polled { fingerprint: fingerprint(&raw), item: item(&raw), checks })
        })
    }

    fn rest<'a>(
        &'a self,
        repo: &'a str,
        number: u64,
    ) -> BoxFuture<'a, Result<(Vec<Review>, Vec<Reviewer>, Vec<Event>), Error>> {
        Box::pin(async move {
            let mr = Self::mr(repo, number);
            let (rv_path, ap_path) = (format!("{mr}/reviewers"), format!("{mr}/approvals"));
            let (rv, ap, ds) = tokio::join!(self.get(&rv_path), self.get(&ap_path), self.discussions(&mr));
            // An older server has no reviewers route: the MR's own list,
            // all pending.
            let rv = match rv {
                Ok(a) => a.v,
                Err(Error::NotFound(_)) => {
                    let cached = self.etags.lock().unwrap().get(&mr).map(|(_, v)| v.clone()).unwrap_or_default();
                    let list: Vec<Value> = cached["reviewers"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|u| json!({ "user": u, "state": "unreviewed" }))
                        .collect();
                    Value::Array(list)
                }
                Err(e) => return Err(e),
            };
            let ap = ap.map(|a| a.v).unwrap_or(Value::Null);
            // Discussions need a login even on a public project.
            let ds = match ds {
                Ok(v) => v,
                Err(Error::Denied(_)) if self.token.is_none() => Value::Null,
                Err(e) => return Err(e),
            };
            let (reviews, requested) = reviews(&rv, &ap);
            Ok((reviews, requested, events(&ds)))
        })
    }

    fn write<'a>(&'a self, repo: &'a str, number: u64, w: &'a Write) -> BoxFuture<'a, Result<Sent, Error>> {
        Box::pin(async move {
            if self.token.is_none() {
                return Err(self.refused());
            }
            let mr = Self::mr(repo, number);
            match w {
                Write::Comment { body } => {
                    Ok(Sent { url: self.note(repo, number, body).await?, said: "commented".into() })
                }
                Write::Review { event: ReviewEvent::Approve, body } => {
                    self.send(reqwest::Method::POST, &format!("{mr}/approve"), Some(&json!({}))).await?;
                    let url = match body.as_deref().filter(|b| !b.trim().is_empty()) {
                        Some(b) => self.note(repo, number, b).await?,
                        None => None,
                    };
                    Ok(Sent { url, said: "approved".into() })
                }
                Write::Review { event, body } => {
                    let b = body.as_deref().unwrap_or_default();
                    let url = self.note(repo, number, b).await?;
                    let said = if *event == ReviewEvent::RequestChanges {
                        "requested changes (as a comment: GitLab's API can't set a reviewer's state)"
                    } else {
                        "reviewed (as a comment)"
                    };
                    Ok(Sent { url, said: said.into() })
                }
                Write::Merge { style } => {
                    let squash = match style.as_deref().unwrap_or("merge") {
                        "merge" => false,
                        "squash" => true,
                        s => return Err(Error::Http(format!("GitLab merges with merge or squash, not {s}"))),
                    };
                    let v = self
                        .send(reqwest::Method::PUT, &format!("{mr}/merge"), Some(&json!({ "squash": squash })))
                        .await?
                        .v;
                    let style = if squash { "squash" } else { "merge" };
                    Ok(Sent { url: v["web_url"].as_str().map(str::to_owned), said: format!("merged ({style})") })
                }
                Write::Rerun => {
                    let raw = self.get(&mr).await?.v;
                    let (project, id, _) = head_pipeline(&raw)
                        .ok_or_else(|| Error::NotFound("the merge request has no pipeline".into()))?;
                    let v = self
                        .send(reqwest::Method::POST, &format!("projects/{project}/pipelines/{id}/retry"), None)
                        .await?
                        .v;
                    *self.jobs.lock().unwrap() = None;
                    Ok(Sent { url: v["web_url"].as_str().map(str::to_owned), said: format!("retried pipeline {id}") })
                }
                Write::Live { .. } => Err(Error::Http("live updates go through the block's hook".into())),
            }
        })
    }

    /// M40: a project webhook to this daemon (its token in
    /// `X-Gitlab-Token`), or its removal.
    fn hook<'a>(
        &'a self,
        repo: &'a str,
        on: bool,
        rec: &'a crate::forge::live::HookRec,
    ) -> BoxFuture<'a, Result<Option<u64>, Error>> {
        Box::pin(async move {
            if self.token.is_none() {
                return Err(self.refused());
            }
            let base = format!("projects/{}/hooks", project(repo));
            if !on {
                let id = rec.id.unwrap_or_default();
                return match self.send(reqwest::Method::DELETE, &format!("{base}/{id}"), None).await {
                    Ok(_) | Err(Error::NotFound(_)) => Ok(None),
                    Err(e) => Err(e),
                };
            }
            let req = json!({
                "url": rec.url, "token": rec.secret,
                "merge_requests_events": true, "note_events": true, "pipeline_events": true,
                "push_events": false, "issues_events": false, "enable_ssl_verification": true,
            });
            Ok(self.send(reqwest::Method::POST, &base, Some(&req)).await?.v["id"].as_u64())
        })
    }

    fn repo_urls<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<Vec<String>, Error>> {
        Box::pin(async move {
            let v = self.get(&format!("projects/{}", project(repo))).await?.v;
            Ok(["ssh_url_to_repo", "http_url_to_repo", "web_url"]
                .iter()
                .filter_map(|k| v[*k].as_str().map(str::to_owned))
                .collect())
        })
    }

    fn read_only(&self) -> Option<String> {
        self.read_only.clone()
    }

    fn rerun_api(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::model::{Pr, Want, attention, rollup};

    fn fixture(f: &str) -> Value {
        let p = format!("{}/tests/fixtures/gitlab/gitlab-cli-3941/{f}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&p).map(|s| serde_json::from_str(&s).unwrap()).unwrap_or(Value::Null)
    }

    fn pr_of(raw: &Value, reviewers: &Value, approvals: &Value, jobs: &Value, discussions: &Value) -> Pr {
        let mut it = item(raw);
        let (reviews, requested) = reviews(reviewers, approvals);
        it.requested = requested;
        let checks = checks(jobs, 1);
        Pr { rollup: rollup(&checks), item: it, reviews, checks, events: events(discussions) }
    }

    fn recorded() -> Pr {
        pr_of(
            &fixture("item.json"),
            &fixture("reviewers.json"),
            &fixture("approvals.json"),
            &fixture("pipeline_jobs.json"),
            &Value::Null,
        )
    }

    fn me(login: &str) -> Me {
        Me { login: login.into(), teams: vec![] }
    }

    #[test]
    fn the_recorded_merge_request() {
        // gitlab-org/cli!3941: merged, from a fork, approved, 23 jobs.
        let p = recorded();
        let it = &p.item;
        assert_eq!((it.number, it.state, it.draft), (3941, ItemState::Merged, false));
        assert_eq!(it.author, "Kxrma47");
        assert_eq!(it.url, "https://gitlab.com/gitlab-org/cli/-/merge_requests/3941");
        assert_eq!(it.base.repo.as_deref(), Some("gitlab-org/cli"));
        assert_eq!(it.base.branch, "main");
        assert_eq!(it.head.repo.as_deref(), Some("project 45049979"), "a fork");
        assert_eq!(it.head.branch, "kxrma47/8547-validate-list-output");
        assert_eq!(it.head.sha, "44611ba4b29c1e82137091fa9d0de53ec1326e4f");
        assert_eq!(it.head_ref, "refs/merge-requests/3941/head");
        // diff_refs.base_sha, never start_sha.
        assert_eq!(it.merge_base.as_deref(), Some("5e811abcc71af2ab423477e4f58c903a7b2317bd"));
        assert_eq!(it.labels.len(), 11);
        assert_eq!(it.assignees, ["Kxrma47"]);
        assert_eq!(it.merged_by.as_deref(), Some("jhebden"));
        assert_eq!(it.comments, 17);
        assert_eq!((it.mergeable, it.blocked), (None, false), "not open");
        // jhebden approved (as reviewer and approver: one review), the Duo
        // bot reviewed; nobody is still asked.
        assert!(it.requested.is_empty());
        let rs: Vec<(Option<&str>, ReviewState)> = p.reviews.iter().map(|r| (r.author.as_deref(), r.state)).collect();
        assert_eq!(rs, [(Some("jhebden"), ReviewState::Approved), (Some("GitLabDuo"), ReviewState::Commented)]);
        assert_eq!(p.reviews[0].at, time("2026-09-29T06:56:13.207Z"), "the approval's time");
        // The head pipeline ran on a merge commit; its jobs are the checks.
        assert_eq!(head_pipeline(&fixture("item.json")).map(|(p, i, _)| (p, i)), Some((34675721, 2892163626)));
        assert_eq!(p.checks.len(), 23);
        assert!(p.checks.iter().all(|c| c.source == CheckSource::PipelineJob));
        let manual = p.checks.iter().filter(|c| c.state == CheckState::Manual).count();
        assert_eq!(manual, 3);
        assert_eq!(p.checks.iter().filter(|c| c.allow_failure).count(), 6);
        assert_eq!(p.checks[2].name, "snapcraft_build_edge");
        assert_eq!(p.checks[2].description.as_deref(), Some("build"));
        assert_eq!(p.checks[2].url.as_deref(), Some("https://gitlab.com/gitlab-org/cli/-/jobs/16799025465"));
        assert_eq!(p.checks[2].run, Some(RunRef { id: "2892163626".into(), job: Some("16799025465".into()) }));
        assert_eq!(p.rollup, Some(CheckState::Success), "manual and allowed-to-fail jobs don't count");
        assert!(p.events.is_empty(), "discussions need a login: none recorded");
        // Merged, and the author's: done. The approver: nothing.
        let w = attention(&p, &me("Kxrma47"), 0);
        assert!(matches!(&w[..], [Want::Done { key, .. }] if key == "merged"), "{w:?}");
        assert!(attention(&p, &me("jhebden"), 0).is_empty());
        let text = p.text("gitlab-org/cli");
        assert!(text.starts_with("gitlab-org/cli#3941 fix(issue): reject invalid list output flags"), "{text}");
        assert!(text.contains("Kxrma47 wants to merge project 45049979:kxrma47/8547-validate-list-output"), "{text}");
        assert!(text.contains("manual    review-docs-cleanup — review"), "{text}");
    }

    /// The same MR open again, in the states the recording doesn't have.
    fn open_mr() -> Value {
        let mut raw = fixture("item.json");
        raw["state"] = json!("opened");
        raw["merged_at"] = Value::Null;
        raw["merged_by"] = Value::Null;
        raw["detailed_merge_status"] = json!("mergeable");
        raw
    }

    #[test]
    fn a_review_requested_from_you_then_approved() {
        let raw = open_mr();
        let reviewers = json!([
            { "user": { "id": 1, "username": "jake" }, "state": "unreviewed", "created_at": "2026-10-01T00:00:00Z" },
            { "user": { "id": 2, "username": "sam" }, "state": "review_started", "created_at": "2026-10-01T00:00:00Z" },
        ]);
        let p = pr_of(&raw, &reviewers, &json!({ "approved_by": [] }), &fixture("pipeline_jobs.json"), &Value::Null);
        assert_eq!(p.item.requested, [Reviewer::User("jake".into()), Reviewer::User("sam".into())]);
        assert!(p.reviews.is_empty());
        let w = attention(&p, &me("jake"), 0);
        assert!(matches!(&w[..], [Want::Review { why }] if why == "review requested from you"), "{w:?}");
        // Approved without submitting a review: no longer asked.
        let approvals = json!({ "approved_by": [{ "user": { "id": 1, "username": "jake" }, "approved_at": "2026-10-01T01:00:00Z" }] });
        let p = pr_of(&raw, &reviewers, &approvals, &fixture("pipeline_jobs.json"), &Value::Null);
        assert_eq!(p.item.requested, [Reviewer::User("sam".into())]);
        assert_eq!(p.reviews[0].state, ReviewState::Approved);
        assert!(attention(&p, &me("jake"), 0).is_empty());
        // Green, mergeable, the author's: done, approved.
        let w = attention(&p, &me("Kxrma47"), 0);
        assert!(matches!(&w[..], [Want::Done { why, .. }] if why == "approved, checks green"), "{w:?}");
    }

    #[test]
    fn a_failed_pipeline_changes_requested_and_a_mention() {
        let raw = open_mr();
        let mut jobs = fixture("pipeline_jobs.json");
        jobs[8]["status"] = json!("failed"); // tests:integration
        jobs[19]["status"] = json!("failed"); // allowed to fail: doesn't count
        jobs[3]["status"] = json!("running");
        let reviewers = json!([
            { "user": { "id": 3, "username": "rev" }, "state": "requested_changes", "created_at": "2026-10-01T00:00:00Z" },
        ]);
        let discussions = json!([
            { "id": "a", "individual_note": true, "notes": [
                { "id": 11, "body": "added 2 commits\n\n<ul><li>abc</li></ul>", "system": true, "author": { "username": "Kxrma47" }, "created_at": "2026-10-01T00:00:00Z" },
            ]},
            { "id": "b", "individual_note": true, "notes": [
                { "id": 12, "body": "requested review from @rev", "system": true, "author": { "username": "Kxrma47" }, "created_at": "2026-10-01T00:01:00Z" },
            ]},
            { "id": "c", "individual_note": false, "notes": [
                { "id": 13, "body": "This line leaks, @Kxrma47", "system": false, "type": "DiffNote", "position": { "new_line": 3 }, "author": { "username": "rev" }, "created_at": "2026-10-01T00:02:00Z", "resolvable": true, "resolved": false },
                { "id": 14, "body": "fixed", "system": false, "type": "DiscussionNote", "author": { "username": "Kxrma47" }, "created_at": "2026-10-01T00:03:00Z" },
            ]},
            { "id": "d", "individual_note": true, "notes": [
                { "id": 15, "body": "merged", "system": true, "author": { "username": "jhebden" }, "created_at": "2026-10-01T00:04:00Z" },
            ]},
        ]);
        let p = pr_of(&raw, &reviewers, &Value::Null, &jobs, &discussions);
        assert_eq!(p.rollup, Some(CheckState::Failure));
        let w = attention(&p, &me("Kxrma47"), 0);
        let kinds: Vec<&str> = w
            .iter()
            .map(|w| match w {
                Want::Failed { .. } => "failed",
                Want::Changes { .. } => "changes",
                Want::Mention { .. } => "mention",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["failed", "changes", "mention"], "{w:?}");
        assert_eq!(w[0].why(), "1 check failed: tests:integration");
        if let Want::Failed { checks, .. } = &w[0] {
            assert_eq!(checks[0].run.as_ref().map(|r| r.id.as_str()), Some("2892163626"));
        }
        assert_eq!(w[1].why(), "changes requested by rev");
        assert_eq!(w[2].why(), "mentioned by rev");
        let k: Vec<EventKind> = p.events.iter().map(|e| e.kind).collect();
        use EventKind::*;
        assert_eq!(k, [Pushed, ReviewRequested, ReviewComment, Commented, Merged]);
        assert_eq!(p.events[0].commits, Some(2));
        assert_eq!(p.events[1].target, Some(Reviewer::User("rev".into())));
        assert_eq!(p.events[1].body, None, "a system note mentions nobody");
        assert_eq!(p.events[0].id, "gl-11");
        // Seen after the comment: no mention.
        let w = attention(&p, &me("Kxrma47"), time("2026-10-01T00:02:30Z").unwrap());
        assert!(!w.iter().any(|w| matches!(w, Want::Mention { .. })));
    }

    #[test]
    fn every_job_state_and_merge_status() {
        use CheckState::*;
        let states = [
            ("success", Success),
            ("failed", Failure),
            ("canceled", Cancelled),
            ("skipped", Skipped),
            ("manual", Manual),
            ("created", Queued),
            ("pending", Queued),
            ("waiting_for_resource", Queued),
            ("preparing", Queued),
            ("scheduled", Queued),
            ("running", Running),
            ("something_new", Neutral),
        ];
        for (s, want) in states {
            assert_eq!(job_state(s), want, "{s}");
        }
        let ms = |status: &str| merge_status(&json!({ "state": "opened", "detailed_merge_status": status }));
        assert_eq!(ms("mergeable"), (Some(true), false));
        assert_eq!(ms("conflict"), (Some(false), false));
        assert_eq!(ms("need_rebase"), (Some(false), false));
        assert_eq!(ms("checking"), (None, false));
        assert_eq!(ms("ci_still_running"), (Some(true), false));
        assert_eq!(ms("not_approved"), (Some(true), true));
        assert_eq!(ms("discussions_not_resolved"), (Some(true), true));
        assert_eq!(merge_status(&json!({ "state": "opened", "has_conflicts": true })), (Some(false), false));
        assert_eq!(merge_status(&json!({ "state": "merged", "detailed_merge_status": "mergeable" })), (None, false));
        // States, and drafts by flag or title.
        let st = |s: &str| item(&json!({ "state": s })).state;
        assert_eq!(
            [st("opened"), st("closed"), st("merged"), st("locked")],
            [ItemState::Open, ItemState::Closed, ItemState::Merged, ItemState::Closed]
        );
        assert!(item(&json!({ "draft": true, "title": "x" })).draft);
        assert!(item(&json!({ "title": "Draft: x" })).draft);
        assert!(item(&json!({ "title": "WIP: x", "work_in_progress": false })).draft);
        assert!(!item(&json!({ "title": "Drafting the docs" })).draft);
        // Not approved yet: blocked, so green isn't done.
        let mut raw = open_mr();
        raw["detailed_merge_status"] = json!("not_approved");
        let p = pr_of(&raw, &json!([]), &Value::Null, &fixture("pipeline_jobs.json"), &Value::Null);
        assert!(p.item.blocked);
        assert!(attention(&p, &me("Kxrma47"), 0).is_empty());
    }

    #[test]
    fn addresses() {
        assert_eq!(project("gitlab-org/cli"), "gitlab-org%2Fcli");
        assert_eq!(project("/group/sub/proj/"), "group%2Fsub%2Fproj");
        assert_eq!(site("https://gitlab.example/api/v4"), "https://gitlab.example");
        let p = parse_mr_url("https://gitlab.com/gitlab-org/cli/-/merge_requests/3941").unwrap();
        assert_eq!(p, ("gitlab.com".into(), "gitlab-org/cli".into(), 3941, "https://gitlab.com/api/v4".into()));
        let p = parse_mr_url("http://127.0.0.1:8080/group/sub/proj/-/merge_requests/7/diffs?x=1").unwrap();
        assert_eq!((p.1.as_str(), p.2, p.3.as_str()), ("group/sub/proj", 7, "http://127.0.0.1:8080/api/v4"));
        assert!(parse_mr_url("https://gitlab.com/gitlab-org/cli/-/issues/3").is_none());
        assert!(parse_mr_url("https://gitlab.com/cli/-/merge_requests/3").is_none());
        assert!(parse_mr_url("https://git.example/o/r/pulls/3").is_none());
        assert!(known_host("gitlab.com"));
        assert!(known_host("gitlab.example.org"));
        assert!(!known_host("git.inevitable.fyi"));
    }

    #[test]
    fn what_glab_says() {
        assert_eq!(parse_glab(b"arugula-no-glab\n"), Glab::Missing);
        assert_eq!(parse_glab(b"known no\n"), Glab::NoLogin);
        assert_eq!(parse_glab(b"known yes\ntoken=\n"), Glab::NoLogin);
        assert_eq!(parse_glab(b"known yes\ntoken=glpat-abc\n"), Glab::Token("glpat-abc".into()));
        assert!(read_only_note("gitlab.com", "glab isn't installed here").starts_with("read-only: no glab login"));
    }

    #[test]
    fn fingerprints_move_with_the_mr() {
        let raw = fixture("item.json");
        let a = fingerprint(&raw);
        let mut b = raw.clone();
        b["user_notes_count"] = json!(18);
        assert_ne!(a, fingerprint(&b));
        let mut c = raw.clone();
        c["head_pipeline"]["status"] = json!("running");
        assert_eq!(a, fingerprint(&c), "the pipeline is polled apart");
        assert_ne!(head_pipeline(&raw), head_pipeline(&c));
    }

    #[test]
    fn approvals_by_people_who_arent_reviewers() {
        let approvals = json!({ "approved_by": [
            { "user": { "id": 9, "username": "lead" }, "approved_at": "2026-10-01T00:00:00Z" },
        ] });
        let (rs, req) = reviews(&json!([]), &approvals);
        assert!(req.is_empty());
        assert_eq!(rs.len(), 1);
        assert_eq!((rs[0].id.as_str(), rs[0].state), ("gl-ap-9", ReviewState::Approved));
    }
}
