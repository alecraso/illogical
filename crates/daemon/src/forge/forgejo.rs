//! The Forgejo adapter (M36): Forgejo's API onto the normalized model, and
//! its writes. Requests go through the daemon's own HTTP client with a token
//! from the person's `tea` login (never kept anywhere but memory).
//!
//! What's particular to Forgejo (S23):
//!
//! - **No ETags, no rate-limit headers.** A poll is the item plus the
//!   commit's combined status (2 requests). Reviews and the timeline are
//!   read again only when the item's fingerprint moves.
//! - **`requested_reviewers` keeps whoever already reviewed.** A request is
//!   pending only while that reviewer's (or team's) latest review entry is
//!   `REQUEST_REVIEW`.
//! - **Checks are commit statuses.** Actions write them, with a
//!   `target_url` relative to the site (`/o/r/actions/runs/111/jobs/0`).
//! - **No rerun API**, so a failed check links its run.
//! - **Times carry the server's offset** (`+02:00`); kept as UTC ms.

use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::{
    Adapter, Error, Polled, Sent, Write,
    model::{
        Branch, Check, CheckSource, CheckState, Event, EventKind, Item, ItemKind, ItemState, Me, Review, ReviewState,
        Reviewer, RunRef,
    },
};

/// How many timeline events are kept in the state (the rest are in the
/// block's log).
pub const EVENTS: usize = 50;

/// `2026-10-02T23:37:59Z` or `2026-10-02T15:41:18+02:00` (fractions too)
/// as UTC ms.
pub fn time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') || b[13] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let mut rest = &s[19..];
    let mut ms = 0;
    if let Some(f) = rest.strip_prefix('.') {
        let digits = f.find(|c: char| !c.is_ascii_digit()).unwrap_or(f.len());
        ms = format!("{:0<3}", &f[..digits.min(3)]).parse::<i64>().ok()?;
        rest = &f[digits..];
    }
    let offset = match rest {
        "Z" | "" => 0,
        o if o.len() == 6 && (o.starts_with('+') || o.starts_with('-')) => {
            let h = o.get(1..3)?.parse::<i64>().ok()?;
            let mi = o.get(4..6)?.parse::<i64>().ok()?;
            let v = (h * 60 + mi) * 60_000;
            if o.starts_with('-') { -v } else { v }
        }
        _ => return None,
    };
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms - offset)
}

fn t(v: &Value) -> Option<i64> {
    v.as_str().and_then(time)
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_owned()
}

fn login(v: &Value) -> Option<String> {
    v["login"].as_str().filter(|l| !l.is_empty()).map(str::to_owned)
}

/// The site's address, from an API base (`https://h/api/v1` → `https://h`).
pub fn site(api: &str) -> String {
    api.trim_end_matches('/').trim_end_matches("/api/v1").to_owned()
}

/// A team in a review entry: `Org/Name` when it says which org.
fn team(v: &Value) -> Option<String> {
    let name = v["name"].as_str()?;
    Some(match v["organization"]["name"].as_str().or(v["organization"]["username"].as_str()) {
        Some(org) => format!("{org}/{name}"),
        None => name.to_owned(),
    })
}

/// `GET repos/O/R/pulls/N`. `requested` is filled in from the reviews.
pub fn item(it: &Value) -> Item {
    let state = if it["merged"].as_bool() == Some(true) {
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
        merge_base: it["merge_base"].as_str().filter(|m| !m.is_empty()).map(str::to_owned),
        mergeable: it["mergeable"].as_bool(),
        blocked: false,
        requested: vec![],
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
        it["requested_reviewers_teams"].as_array().into_iter().flatten().filter_map(|u| u["name"].as_str()).collect();
    json!([
        it["updated_at"],
        it["head"]["sha"],
        it["comments"],
        it["review_comments"],
        it["state"],
        it["merged"],
        it["mergeable"],
        rr,
        rt
    ])
    .to_string()
}

/// `GET pulls/N/reviews`: the reviews, and the requests still pending
/// (whose latest entry is `REQUEST_REVIEW`).
pub fn reviews(list: &Value, site: &str) -> (Vec<Review>, Vec<Reviewer>) {
    let mut latest: Vec<(Reviewer, &str)> = Vec::new();
    let mut out = Vec::new();
    for r in list.as_array().into_iter().flatten() {
        let who = match (login(&r["user"]), team(&r["team"])) {
            (Some(u), _) => Reviewer::User(u),
            (None, Some(t)) => Reviewer::Team(t),
            (None, None) => continue,
        };
        let state = r["state"].as_str().unwrap_or_default();
        latest.retain(|(w, _)| *w != who);
        latest.push((who.clone(), state));
        if state == "REQUEST_REVIEW" {
            continue;
        }
        let state = if r["dismissed"].as_bool() == Some(true) {
            ReviewState::Dismissed
        } else {
            match state {
                "APPROVED" => ReviewState::Approved,
                "REQUEST_CHANGES" | "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
                "PENDING" => ReviewState::Pending,
                _ => ReviewState::Commented,
            }
        };
        out.push(Review {
            id: r["id"].as_u64().map(|i| i.to_string()).unwrap_or_default(),
            author: match &who {
                Reviewer::User(u) => Some(u.clone()),
                Reviewer::Team(_) => None,
            },
            state,
            commit: r["commit_id"].as_str().filter(|c| !c.is_empty()).map(str::to_owned),
            stale: r["stale"].as_bool().unwrap_or(false),
            at: t(&r["submitted_at"]),
            body: r["body"].as_str().filter(|b| !b.is_empty()).map(str::to_owned),
            url: r["html_url"].as_str().map(|u| absolute(site, u)),
        });
    }
    let requested = latest.into_iter().filter(|(_, s)| *s == "REQUEST_REVIEW").map(|(w, _)| w).collect();
    (out, requested)
}

fn absolute(site: &str, u: &str) -> String {
    if u.starts_with('/') { format!("{site}{u}") } else { u.to_owned() }
}

/// `GET commits/SHA/status`: the combined status's statuses as checks.
pub fn checks(status: &Value, site: &str) -> Vec<Check> {
    status["statuses"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let url = c["target_url"].as_str().filter(|u| !u.is_empty()).map(|u| absolute(site, u));
            let run = url.as_deref().and_then(|u| {
                let at = u.find("/actions/runs/")?;
                let mut parts = u[at + 14..].split('/');
                let id = parts.next()?.to_owned();
                let job = match (parts.next(), parts.next()) {
                    (Some("jobs"), Some(j)) => Some(j.to_owned()),
                    _ => None,
                };
                Some(RunRef { id, job })
            });
            let state = match c["status"].as_str().or(c["state"].as_str()).unwrap_or_default() {
                "success" => CheckState::Success,
                "failure" | "error" => CheckState::Failure,
                "pending" => CheckState::Running,
                "skipped" => CheckState::Skipped,
                _ => CheckState::Neutral, // warning, and whatever's new
            };
            Check {
                name: s(&c["context"]),
                source: if run.is_some() { CheckSource::Action } else { CheckSource::Status },
                state,
                allow_failure: false,
                url,
                description: c["description"].as_str().filter(|d| !d.is_empty()).map(str::to_owned),
                run,
            }
        })
        .collect()
}

/// `GET issues/N/timeline`, oldest first.
pub fn events(list: &Value) -> Vec<Event> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            let ty = e["type"].as_str().unwrap_or_default();
            let kind = match ty {
                "comment" => EventKind::Commented,
                "code" => EventKind::ReviewComment,
                "review" => EventKind::Reviewed,
                "review_request" => EventKind::ReviewRequested,
                "dismiss_review" => EventKind::ReviewDismissed,
                "pull_push" => EventKind::Pushed,
                "label" => EventKind::Labeled,
                "assignees" => EventKind::Assigned,
                "change_title" => EventKind::Renamed,
                "commit_ref" | "issue_ref" | "pull_ref" | "comment_ref" => EventKind::Referenced,
                "merge_pull" => EventKind::Merged,
                "close" => EventKind::Closed,
                "reopen" => EventKind::Reopened,
                "delete_branch" => EventKind::BranchDeleted,
                "milestone" => EventKind::Milestone,
                _ => EventKind::Other,
            };
            let target = (ty == "review_request")
                .then(|| match (login(&e["assignee"]), team(&e["assignee_team"])) {
                    (Some(u), _) => Some(Reviewer::User(u)),
                    (None, t) => t.map(Reviewer::Team),
                })
                .flatten();
            let (mut commits, mut force) = (None, false);
            if ty == "pull_push"
                && let Ok(p) = serde_json::from_str::<Value>(e["body"].as_str().unwrap_or_default())
            {
                commits = p["commit_ids"].as_array().map(|c| c.len() as u32);
                force = p["is_force_push"].as_bool().unwrap_or(false);
            }
            Event {
                id: format!("fj-{}", e["id"].as_u64().unwrap_or(0)),
                at: t(&e["created_at"]).unwrap_or(0),
                actor: login(&e["user"]),
                kind,
                what: (kind == EventKind::Other).then(|| ty.to_owned()),
                target,
                body: matches!(ty, "comment" | "code" | "review")
                    .then(|| e["body"].as_str().filter(|b| !b.is_empty()).map(str::to_owned))
                    .flatten(),
                commits,
                force,
            }
        })
        .collect()
}

/// `GET user` and `GET user/teams`.
pub fn me(user: &Value, teams: &Value) -> Me {
    let mut t = Vec::new();
    for x in teams.as_array().into_iter().flatten() {
        if let Some(full) = team(x) {
            if let Some((_, name)) = full.split_once('/') {
                t.push(name.to_owned());
            }
            t.push(full);
        }
    }
    Me { login: login(user).unwrap_or_default(), teams: t }
}

/// How a review event is named in Forgejo's `CreatePullReviewOptions`.
pub fn review_event(event: super::ReviewEvent) -> &'static str {
    match event {
        super::ReviewEvent::Approve => "APPROVED",
        super::ReviewEvent::RequestChanges => "REQUEST_CHANGES",
        super::ReviewEvent::Comment => "COMMENT",
    }
}

/// A Forgejo instance, through one login's token.
pub struct Forgejo {
    pub api: String,
    http: reqwest::Client,
    token: super::TokenSource,
}

impl Forgejo {
    pub fn new(api: &str, http: reqwest::Client, token: super::TokenSource) -> Self {
        Self { api: api.trim_end_matches('/').to_owned(), http, token }
    }

    /// One request, with the token; on a 401 the token is asked for once
    /// more (an OAuth login refreshes in `tea`).
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(Value, Option<u64>), Error> {
        let url = format!("{}/{}", self.api, path.trim_start_matches('/'));
        for attempt in 0..2 {
            let token = self.token.get(attempt > 0).await.map_err(Error::Login)?;
            let mut req = self.http.request(method.clone(), &url).header("Authorization", format!("token {token}"));
            req = req.header("Accept", "application/json");
            if let Some(b) = body {
                req = req.json(b);
            }
            let res = req.send().await.map_err(|e| Error::Http(format!("{}: {}", super::redact(&url), short(&e))))?;
            let status = res.status();
            let total = res.headers().get("x-total-count").and_then(|v| v.to_str().ok()?.parse().ok());
            if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                continue;
            }
            let text = res.text().await.unwrap_or_default();
            if !status.is_success() {
                let said = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| text.chars().take(200).collect());
                return Err(match status.as_u16() {
                    401 | 403 => Error::Denied(format!("{} {said}", status.as_u16())),
                    404 => Error::NotFound(said),
                    c => Error::Http(format!("{c} {said}")),
                });
            }
            let v =
                if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
            return Ok((v, total));
        }
        unreachable!("the loop returns")
    }

    async fn get(&self, path: &str) -> Result<(Value, Option<u64>), Error> {
        self.send(reqwest::Method::GET, path, None).await
    }
}

pub(super) fn short(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(x) = src {
        s = x.to_string();
        src = x.source();
    }
    s
}

impl Adapter for Forgejo {
    fn me(&self) -> BoxFuture<'_, Result<Me, Error>> {
        Box::pin(async move {
            let (user, _) = self.get("user").await?;
            // Teams need read:organization; without it, no team requests.
            let teams = self.get("user/teams").await.map(|(v, _)| v).unwrap_or(Value::Null);
            Ok(me(&user, &teams))
        })
    }

    fn poll<'a>(&'a self, repo: &'a str, number: u64) -> BoxFuture<'a, Result<Polled, Error>> {
        Box::pin(async move {
            let (raw, _) = self.get(&format!("repos/{repo}/pulls/{number}")).await?;
            let it = item(&raw);
            let site = site(&self.api);
            let status = match it.head.sha.as_str() {
                "" => Value::Null,
                sha => self.get(&format!("repos/{repo}/commits/{sha}/status")).await?.0,
            };
            let checks = checks(&status, &site);
            Ok(Polled { fingerprint: fingerprint(&raw), item: it, checks })
        })
    }

    fn rest<'a>(
        &'a self,
        repo: &'a str,
        number: u64,
    ) -> BoxFuture<'a, Result<(Vec<Review>, Vec<Reviewer>, Vec<Event>), Error>> {
        Box::pin(async move {
            let site = site(&self.api);
            let reviews_path = format!("repos/{repo}/pulls/{number}/reviews?limit=50");
            let timeline_path = format!("repos/{repo}/issues/{number}/timeline?limit={EVENTS}");
            let (r, tl) = tokio::join!(self.get(&reviews_path), self.get(&timeline_path));
            let (r, _) = r?;
            let (mut tl, total) = tl?;
            // The timeline is oldest first: the newest are on its last page.
            if let Some(total) = total.filter(|n| *n > EVENTS as u64) {
                let last = total.div_ceil(EVENTS as u64);
                let (page, _) = self.get(&format!("{timeline_path}&page={last}")).await?;
                let mut both = if last > 1 {
                    self.get(&format!("{timeline_path}&page={}", last - 1)).await.map(|(v, _)| v).unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                let mut all = both.as_array_mut().map(std::mem::take).unwrap_or_default();
                all.extend(page.as_array().cloned().unwrap_or_default());
                let keep = all.len().saturating_sub(EVENTS);
                tl = Value::Array(all.split_off(keep));
            }
            let (reviews, requested) = reviews(&r, &site);
            Ok((reviews, requested, events(&tl)))
        })
    }

    fn write<'a>(&'a self, repo: &'a str, number: u64, w: &'a Write) -> BoxFuture<'a, Result<Sent, Error>> {
        Box::pin(async move {
            let site = site(&self.api);
            match w {
                Write::Comment { body } => {
                    let (v, _) = self
                        .send(
                            reqwest::Method::POST,
                            &format!("repos/{repo}/issues/{number}/comments"),
                            Some(&json!({ "body": body })),
                        )
                        .await?;
                    Ok(Sent { url: v["html_url"].as_str().map(|u| absolute(&site, u)), said: "commented".into() })
                }
                Write::Review { event, body } => {
                    let req = json!({ "event": review_event(*event), "body": body.clone().unwrap_or_default() });
                    let (v, _) = self
                        .send(reqwest::Method::POST, &format!("repos/{repo}/pulls/{number}/reviews"), Some(&req))
                        .await?;
                    let said = match event {
                        super::ReviewEvent::Approve => "approved",
                        super::ReviewEvent::RequestChanges => "requested changes",
                        super::ReviewEvent::Comment => "reviewed",
                    };
                    Ok(Sent { url: v["html_url"].as_str().map(|u| absolute(&site, u)), said: said.into() })
                }
                Write::Merge { style } => {
                    let req = json!({ "Do": style.as_deref().unwrap_or("merge") });
                    self.send(reqwest::Method::POST, &format!("repos/{repo}/pulls/{number}/merge"), Some(&req)).await?;
                    Ok(Sent { url: None, said: format!("merged ({})", style.as_deref().unwrap_or("merge")) })
                }
                Write::RerunChecks => {
                    Err(Error::Http("Forgejo has no API to rerun checks: rerun them on the run's page".into()))
                }
            }
        })
    }

    fn repo_urls<'a>(&'a self, repo: &'a str) -> BoxFuture<'a, Result<Vec<String>, Error>> {
        Box::pin(async move {
            let (v, _) = self.get(&format!("repos/{repo}")).await?;
            Ok(["ssh_url", "clone_url", "html_url"].iter().filter_map(|k| v[*k].as_str().map(str::to_owned)).collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::model::{CheckState, Want, attention, rollup};

    fn fixture(dir: &str, f: &str) -> Value {
        let p = format!("{}/tests/fixtures/forgejo/{dir}/{f}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&p).map(|s| serde_json::from_str(&s).unwrap()).unwrap_or(Value::Null)
    }

    /// A fixture read as the block reads it.
    fn pr(dir: &str) -> crate::forge::model::Pr {
        let raw = fixture(dir, "item.json");
        let mut it = item(&raw);
        let site = site(&format!("https://{}/api/v1", url::Url::parse(&it.url).unwrap().host_str().unwrap()));
        let (reviews, requested) = reviews(&fixture(dir, "reviews.json"), &site);
        it.requested = requested;
        let checks = checks(&fixture(dir, "statuses.json"), &site);
        crate::forge::model::Pr {
            rollup: rollup(&checks),
            item: it,
            reviews,
            checks,
            events: events(&fixture(dir, "timeline.json")),
        }
    }

    fn me(login: &str, teams: &[&str]) -> Me {
        Me { login: login.into(), teams: teams.iter().map(|t| t.to_string()).collect() }
    }

    #[test]
    fn times_with_offsets() {
        assert_eq!(time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(time("2026-10-02T15:41:18+02:00"), time("2026-10-02T13:41:18Z"));
        assert_eq!(time("2026-10-02T13:41:18.25-01:30"), Some(time("2026-10-02T15:11:18Z").unwrap() + 250));
        assert_eq!(time("soon"), None);
    }

    #[test]
    fn this_repos_merged_pr() {
        let p = pr("forgejo-illogical-84");
        let it = &p.item;
        assert_eq!((it.number, it.state, it.draft), (84, ItemState::Merged, false));
        assert_eq!(it.author, "jhgaylor");
        assert_eq!(it.head.branch, "ci-cap-target");
        assert_eq!(it.head_ref, "refs/pull/84/head");
        assert_eq!(it.merge_base.as_deref(), Some("84af63171d0b4dc7093ba7ee72c2401de3c8e7b4"));
        assert!(it.requested.is_empty());
        assert_eq!(p.checks.len(), 2);
        assert_eq!(p.checks[0].source, CheckSource::Action);
        assert_eq!(
            p.checks[0].url.as_deref(),
            Some("https://git.inevitable.fyi/jhgaylor/illogical/actions/runs/111/jobs/1")
        );
        assert_eq!(p.checks[0].run, Some(RunRef { id: "111".into(), job: Some("1".into()) }));
        assert_eq!(p.rollup, Some(CheckState::Success));
        let kinds: Vec<EventKind> = p.events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [EventKind::Pushed, EventKind::Merged, EventKind::Referenced]);
        assert_eq!(p.events[0].commits, Some(1));
        assert_eq!(p.events[0].id, "fj-1403");
        // Merged, and Jake's: done. Someone else: nothing.
        let w = attention(&p, &me("jhgaylor", &[]), 0);
        assert!(matches!(&w[..], [Want::Done { key, .. }] if key == "merged"), "{w:?}");
        assert!(attention(&p, &me("someone", &[]), 0).is_empty());
        let text = p.text("jhgaylor/illogical");
        assert!(text.starts_with("jhgaylor/illogical#84 CI: delete the kept build"), "{text}");
        assert!(text.contains("success   check / linux (push)"), "{text}");
    }

    #[test]
    fn a_review_requested_directly_and_red_checks() {
        // #14657: a draft by n0toose, three reviewers asked, three test jobs red.
        let p = pr("codeberg-forgejo-14657");
        assert!(p.item.draft);
        let names: Vec<&str> = p.item.requested.iter().map(Reviewer::name).collect();
        assert_eq!(names, ["Gusted", "Cyborus", "famfo-cb"]);
        assert!(p.reviews.is_empty(), "requests aren't reviews");
        assert_eq!(p.rollup, Some(CheckState::Failure));
        let states = |s: CheckState| p.checks.iter().filter(|c| c.state == s).count();
        assert_eq!((states(CheckState::Failure), states(CheckState::Skipped), states(CheckState::Success)), (3, 3, 9));
        let w = attention(&p, &me("Gusted", &[]), 0);
        assert!(matches!(&w[..], [Want::Review { why }] if why == "review requested from you"), "{w:?}");
        let w = attention(&p, &me("n0toose", &[]), 0);
        match &w[..] {
            [Want::Failed { why, checks }] => {
                assert_eq!(checks.len(), 3);
                assert!(why.starts_with("3 checks failed: "), "{why}");
            }
            w => panic!("{w:?}"),
        }
    }

    #[test]
    fn a_team_request_stays_after_a_member_approves() {
        // #14606: Reviewers asked, then mfenniak approved; merged since.
        let p = pr("codeberg-forgejo-14606");
        assert_eq!(p.item.requested, [Reviewer::Team("Reviewers".into())]);
        assert_eq!(p.reviews.len(), 1);
        assert_eq!(p.reviews[0].state, ReviewState::Approved);
        assert_eq!(p.reviews[0].author.as_deref(), Some("mfenniak"));
        assert_eq!(p.rollup, None, "no statuses kept for it");
        // #14665: open, the team asked, green, by a bot.
        let p = pr("codeberg-forgejo-14665");
        assert_eq!(p.item.requested, [Reviewer::Team("Reviewers".into())]);
        let w = attention(&p, &me("Gusted", &["forgejo/Reviewers", "Reviewers"]), 0);
        assert!(matches!(&w[..], [Want::Review { why }] if why == "review requested from Reviewers"), "{w:?}");
        let w = attention(&p, &me("viceice-bot", &[]), 0);
        assert!(matches!(&w[..], [Want::Done { why, .. }] if why == "checks green"), "{w:?}");
    }

    #[test]
    fn a_reviewer_who_reviewed_isnt_still_asked() {
        // #14667: mfenniak is in requested_reviewers but approved.
        let p = pr("codeberg-forgejo-14667");
        assert!(p.item.requested.is_empty());
        assert_eq!(p.reviews[0].state, ReviewState::Approved);
        // "Thanks @wetneb!" after the merge: a mention, until seen.
        let w = attention(&p, &me("wetneb", &[]), 0);
        assert!(w.iter().any(|w| matches!(w, Want::Mention { .. })), "{w:?}");
        assert!(w.iter().any(|w| matches!(w, Want::Done { .. })), "{w:?}");
        let last = p.events.iter().map(|e| e.at).max().unwrap();
        let w = attention(&p, &me("wetneb", &[]), last);
        assert!(!w.iter().any(|w| matches!(w, Want::Mention { .. })), "seen: {w:?}");
    }

    /// The states the fixtures don't have, in Forgejo's shapes.
    #[test]
    fn every_review_and_check_state() {
        let list = json!([
            { "id": 1, "state": "REQUEST_REVIEW", "user": { "login": "a" }, "submitted_at": "2026-10-01T00:00:00Z" },
            { "id": 2, "state": "COMMENT", "user": { "login": "a" }, "body": "looks fine", "submitted_at": "2026-10-01T01:00:00Z" },
            { "id": 3, "state": "REQUEST_CHANGES", "user": { "login": "b" }, "submitted_at": "2026-10-01T02:00:00Z", "html_url": "/o/r/pulls/1#r3" },
            { "id": 4, "state": "APPROVED", "user": { "login": "c" }, "dismissed": true, "submitted_at": "2026-10-01T03:00:00Z" },
            { "id": 5, "state": "PENDING", "user": { "login": "d" } },
            { "id": 6, "state": "REQUEST_REVIEW", "user": null, "team": { "name": "Owners", "organization": { "name": "Nested" } } },
            { "id": 7, "state": "REQUEST_REVIEW", "user": { "login": "e" } },
        ]);
        let (rs, req) = reviews(&list, "https://h");
        let st: Vec<ReviewState> = rs.iter().map(|r| r.state).collect();
        use ReviewState::*;
        assert_eq!(st, [Commented, ChangesRequested, Dismissed, Pending]);
        assert_eq!(rs[1].url.as_deref(), Some("https://h/o/r/pulls/1#r3"));
        // a reviewed after being asked; the team and e are still asked.
        assert_eq!(req, [Reviewer::Team("Nested/Owners".into()), Reviewer::User("e".into())]);
        let st = json!({ "statuses": [
            { "context": "a", "status": "success" }, { "context": "b", "status": "failure" },
            { "context": "c", "status": "error" }, { "context": "d", "status": "pending" },
            { "context": "e", "status": "warning" }, { "context": "f", "status": "skipped", "target_url": "https://ci/x" },
        ] });
        let cs = checks(&st, "https://h");
        use CheckState::*;
        let got: Vec<CheckState> = cs.iter().map(|c| c.state).collect();
        assert_eq!(got, [Success, Failure, Failure, Running, Neutral, Skipped]);
        assert_eq!(cs[5].source, CheckSource::Status);
        assert_eq!(cs[5].url.as_deref(), Some("https://ci/x"));
        // Changes requested on my PR is input; an approval after it isn't
        // by the same person, so it stands.
        let mut p = crate::forge::model::Pr { reviews: rs, ..Default::default() };
        p.item.author = "me".into();
        let w = attention(&p, &me("me", &[]), 0);
        assert!(matches!(&w[..], [Want::Changes { why }] if why == "changes requested by b"), "{w:?}");
        let teams = me_of(
            &json!({ "login": "jhgaylor" }),
            &json!([{ "name": "Owners", "organization": { "name": "NestedData" } }]),
        );
        assert_eq!(teams.teams, ["Owners", "NestedData/Owners"]);
    }

    fn me_of(u: &Value, t: &Value) -> Me {
        super::me(u, t)
    }

    #[test]
    fn fingerprints_move_with_the_item() {
        let raw = fixture("forgejo-illogical-84", "item.json");
        let a = fingerprint(&raw);
        let mut b = raw.clone();
        b["updated_at"] = json!("2026-10-04T00:00:00Z");
        assert_ne!(a, fingerprint(&b));
        assert_eq!(a, fingerprint(&raw.clone()));
    }
}
