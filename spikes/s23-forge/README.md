# S23: forge blocks (#87)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s23-forge/<file>`.

Run 2026-10-02 on geek. Forgejo: this repo on `git.inevitable.fyi` (Forgejo 16.0.5, `tea` 0.16.0, login
`forgejo`), plus Codeberg (also Forgejo) anonymously for PRs with reviews, forks and red checks, since
this repo has one PR (#84, merged, no reviews). GitHub: `cli/cli` through `gh` 2.101.0 (logged in as
jhgaylor), plus Jake's own open PRs for attention (private repos: states only, nothing saved). GitLab:
`gitlab-org/cli` on gitlab.com, anonymous `curl` (`glab` isn't installed). Every request was a GET;
nothing was posted anywhere.

**Result: go.** A `forge` block can read everything M36 needs from Forgejo and GitHub, and the
normalized model below fits three forges with a short list of gaps. Change the plan in three places:

1. **The daemon makes the HTTP requests itself.** It gets the token from the CLI and doesn't run
   `tea api` or `gh api` per request.
2. **On Forgejo, polling is a cheap change check.** It has no ETags, so polls can't be conditional.
3. **Map the remote's host to a login ourselves.** `tea` matches this repo's remote to no login.

## 1. The CLIs as the read path

**Coverage: yes, all three.** Each forge's `api` passthrough reaches every endpoint the block reads:
the PR, reviews, checks or statuses, the timeline, files, and diff refs. `tea api` and `gh api`
were run. For `glab api`, its docs list `-i/--include`, `-H/--header`, `--hostname` and `--paginate`.

| | Forgejo (`tea api`) | GitHub (`gh api`) | GitLab (`glab api`, from its docs) |
|---|---|---|---|
| item | `repos/O/R/pulls/N` (has `merge_base`, `requested_reviewers[_teams]`) | `repos/O/R/pulls/N` | `projects/ID/merge_requests/IID` (has `diff_refs`, `head_pipeline`) |
| reviews | `pulls/N/reviews` (includes `REQUEST_REVIEW` entries) | `pulls/N/reviews` | `merge_requests/IID/reviewers` (state per reviewer) + `/approvals` |
| checks | `commits/SHA/status` (Actions write statuses); `actions/runs?head_sha=` | `commits/SHA/check-runs` **and** `commits/SHA/status` | `pipelines/PID/jobs` for `head_pipeline` |
| timeline | `issues/N/timeline` | `issues/N/timeline` (+ `pulls/N/comments` for line comments) | `merge_requests/IID/discussions` (**401 anonymously**, even on a public project) |
| files | `pulls/N/files` | `pulls/N/files` | `merge_requests/IID/diffs` |
| rerun checks | **no API** (Forgejo 16 has `actions/runs/ID/cancel`, no rerun) | `check-suites/ID/rerequest`, `actions/runs/ID/rerun-failed-jobs` | `pipelines/PID/retry` |
| you | `user` → `jhgaylor` (id 1); teams `user/teams` | `user` → `jhgaylor`; teams `user/teams` | `user` (needs auth) |

**Conditional requests go through both CLIs, but only GitHub and GitLab return ETags.**

- `gh api -i -H 'If-None-Match: W/"…"'` answered `304 Not Modified`, and `gh` exited 1 with `gh: HTTP 304`
  on stderr, so a caller has to treat that exit as "unchanged".
- `tea api -i -H 'If-None-Match:…'` passes the header along, and `-i` prints the status and headers
  on stderr. But Forgejo sends no `ETag` or `Last-Modified` on any API route tried (pulls, reviews,
  timeline, status, files, `.diff`), and no rate-limit headers, so there's nothing to make
  conditional (`fixtures/forgejo-illogical-84/_poll.headers.json`).
- gitlab.com sends weak ETags on everything that answered (item, approvals, pipelines, diffs) and
  returns 304 (`fixtures/gitlab-cli-3941/_poll.headers.json`).

**Recommendation: get the token from the CLI, and send requests through the daemon's own `reqwest`.**
The daemon already depends on `reqwest` 0.13 (rustls, http2, json). Don't spawn a CLI per request:

- **Cost per request.** A spawn adds ~25 ms with `tea` (36 vs 12 ms for `version`) and ~100–170 ms
  with `gh` (575 vs 407 ms for the PR).
- **Parsing.** Each CLI prints headers in its own way (`gh` on stdout before the body, `tea` on
  stderr), and `gh` exits 1 on a 304.
- **Connections.** A process per request means a new TLS connection every time. A long-lived client
  keeps its connections alive.

Getting each token:

- **Forgejo:** `printf 'protocol=https\nhost=H\n\n' | tea login helper get` returns `password=<token>`
  (16 ms). This is tea's git credential helper. It refreshes OAuth logins too
  (`RefreshOAuthTokenIfNeeded`), which reading `config.yml` ourselves wouldn't.
  `tea logins list -o json` gives `name, url, ssh_host, user` without the token.
- **GitHub:** `gh auth token --hostname github.com [--user U]` (39 ms), from the keyring.
- **GitLab:** `glab auth status --show-token` or `glab config get token --host H` (docs).

Keep the token in memory only, never on disk or in a block's state or config. Ask the CLI again on a
401. All three CLIs run with #74's per-host shell environment, as PLAN says. illogical still stores
no forge tokens.

**How `tea` picks a login** (`modules/context/context_remote.go`, `MatchLogins`, at v0.16.0):

- It takes the remote (main/master/trunk's remote, else `upstream`, else `origin`) and walks the logins.
- An `ssh://` URL or a scp-style `git@host:path` matches a login whose `ssh_host` equals the host. When
  `ssh_host` is empty, the login's URL host is used.
- An `https://` URL matches a login whose URL is a prefix of it.
- If nothing matches, it falls back to the default login, or to the only one, with "NOTE: no login
  matched this repository". The repo slug stays empty then, so `tea pulls ls` here fails with
  "remote repository required".
- `GITEA_INSTANCE_URL` plus `GITEA_TOKEN` and `GITEA_INSTANCE_SSH_HOST` add a login from the
  environment, matched first.

**Why it fails here, and the fix.** The remote is `ssh://git@git.tail1234.ts.net/…`. The login's
`ssh_host` is `git.inevitable.fyi` (the URL host, which `tea login add` defaulted to).

- **The tailnet name serves SSH only.** HTTPS on it timed out on 443, 80 and 3000, so it can't be the
  API base. The API is `https://git.inevitable.fyi/api/v1`.
- **Forgejo knows its SSH host.** `GET repos/jhgaylor/illogical` answers
  `ssh_url: ssh://git@git.tail1234.ts.net/jhgaylor/illogical.git` and
  `clone_url: https://git.inevitable.fyi/…`.
- **Changing `ssh_host` fixes `tea`.** Run with a copy of the config whose `ssh_host` is
  `git.tail1234.ts.net`, `tea pulls ls` here matched the login and listed #84. The cost: a login has
  one `ssh_host`, so remotes on `git@git.inevitable.fyi:` would stop matching (there are none).

**The rule for the daemon** (`forge::resolve(remote)`):

1. Parse the remote's host and path.
2. Candidates: CLI logins whose URL host or `ssh_host` equals that host. For `gh`, `gh auth status`'s
   hosts; for `glab`, its hosts.
3. If no login matches, ask each Forgejo login `GET repos/{path}` and keep the logins whose `ssh_url`
   or `clone_url` host is the remote's host. Here that takes one request (~80 ms) and finds
   `forgejo`. Cache the answer as host → login.
4. When a remote matches no login, the block says "No tea login for git.tail1234.ts.net", with
   *Add login…* and *Use login …* buttons. When it matches two or more, the block lists them to pick
   from. A pick is kept in the daemon's config as `[forge.hosts] "git.tail1234.ts.net" = "tea:forgejo"`,
   never guessed. A block's config holds `{provider, api, login, repo, number}`, so restoring it
   doesn't resolve again.

**Logins on GitHub and GitLab.** `gh` takes the host from the remote, or `--hostname` / `GH_HOST`, and
the host's active account (`gh auth switch`). `glab` takes `--hostname`, or the authenticated host of
the current repository. Both have one login per host per user, so step 3 only matters for Forgejo,
where one instance often has two names (public and tailnet).

## 2. Cost

Wall time is the full read: the item first (for the head sha), then the rest in parallel. Numbers come
from `read.mjs`. "Points" is GitHub's `rate_limit.resources.core.used`, read before and after (that
endpoint is free). Fixtures with `_full.headers.json` and `_poll.headers.json` hold every status,
size and header.

| | requests | wall | bytes | rate limit |
|---|---|---|---|---|
| **GitHub** `cli/cli#14519` (fork; 18 check runs, 15 reviews, 39 timeline events), full read via `gh api` | 7 | 1.56 s | 328 KB | 7 points |
| ...the same via HTTP with `gh auth token` | 7 | 1.38–1.54 s | 328 KB | 7 points |
| ...unchanged poll, all 7 with `If-None-Match` | 7 × **304** | 0.89–1.02 s | 0 | **0 points** (twice, via `gh` and via HTTP) |
| ...unchanged poll, the item alone | 1 × 304 | 0.37–0.48 s | 0 | 0 |
| ...the same read as one GraphQL query (`github-graphql.sh`) | 1 | 1.28–1.77 s | 27 KB | 1 GraphQL point, **no ETag**: every poll costs 1 |
| **Forgejo** `jhgaylor/illogical#84` (`git.inevitable.fyi`), full read via `tea api` | 6 | 0.44 s | 29 KB | none sent |
| ...the same via HTTP with tea's token | 6 | 0.31–0.76 s | 29 KB | none |
| ...unchanged poll | 6 × 200 (no ETags) | 0.09–0.35 s | 29 KB again | none |
| ...the item alone | 1 × 200 | 67–173 ms | 3.5 KB | |
| **Codeberg** `forgejo/forgejo#14667` (19 statuses), anonymous | 6 | 3.4 s (`actions/runs` alone 2.8 s, 145 KB) | 204 KB | none |
| **gitlab.com** `gitlab-org/cli!3941`, anonymous | 5 (discussions 401) | 0.52 s | 16 KB | 5 of 500 |
| ...unchanged poll | 4 × 304 | 0.22 s | 30 B | **5 more: a 304 counts** (`ratelimit-observed` went from 5 to 10) |

What it means for polling (PLAN's freshness rule: every few seconds while drawn, else every few minutes):

- **GitHub: REST with ETags, not GraphQL.** An unchanged poll of the item, check runs and statuses is
  three 304s, about 0.4 s and **0 points**. A change costs only the requests that changed. GraphQL is
  one round trip for the first read, but it costs a point on every poll, so M38 doesn't need it.
  Polling every 5 s for an hour while drawn costs ~0 points if nothing changes, against 5000/h.
- **Forgejo: poll the item and the combined status (2 requests, ~100–200 ms, ~6 KB). Re-read the
  rest only when something changed:** the item's `updated_at`, `head.sha`, `comments` or
  `review_comments`, or the status's `state` and `total_count`. Statuses don't touch the item, so
  both are needed. That a review or comment bumps the PR's `updated_at` comes from Forgejo's model,
  not from a measurement (it needs a write, which this spike doesn't do): M36's fake-Forgejo tests
  should pin it. Leave `actions/runs` out of polls. It's 145 KB on Codeberg and repeats the statuses.
  Read it only for *Rerun checks* or `need_approval`.
  - `notifications?since=` would be a one-request change feed for every PR, but Jake's token lacks
    `read:notification` (403). A new token with that scope is an option, not a need.
- **GitLab: 304s are cheap but not free** (gitlab.com publishes 2000 requests a minute authenticated). Poll `head_pipeline`
  through the item.

## 3. The normalized model

`model.mjs` maps every fixture onto this model, with one adapter per forge. Its field names are the
proposed Rust names, and running `node model.mjs --all` prints each PR mapped and its attention. The
mapping covers one PR on each forge and more: Forgejo #84 and Codeberg #14606, #14657, #14665 and
#14667; GitHub #13788, #13899 and #14519; GitLab !3941.

```rust
pub struct ForgeRef { pub provider: Provider, pub api: String, pub login: String, pub repo: String, pub number: u64 }
pub enum Provider { Forgejo, Github, Gitlab }

pub struct Item {
    pub kind: ItemKind,                 // Pr | Issue (M37)
    pub url: String, pub title: String, pub body: String,
    pub author: String,                 // login
    pub state: ItemState,               // Open | Closed | Merged   (GitLab `opened`/`locked` → Open/Closed)
    pub draft: bool,                    // GitLab `draft` already covers `Draft:`/`WIP:` titles
    pub labels: Vec<String>, pub assignees: Vec<String>,
    pub base: Branch, pub head: Branch, // head.repo != base.repo → a fork
    pub head_ref: String,               // refs/pull/N/head | refs/merge-requests/N/head
    pub merge_base: Option<String>,     // Forgejo `merge_base`, GitLab `diff_refs.base_sha`; GitHub: compute
    pub mergeable: Option<bool>,        // GitHub null while it computes
    pub blocked: bool,                  // GitHub mergeable_state == "blocked"; others: false (unknown)
    pub requested: Vec<Reviewer>,       // pending requests only (see below)
    pub updated_at: i64, pub merged_at: Option<i64>,
}
pub struct Branch { pub repo: Option<String>, pub branch: String, pub sha: String }
pub enum Reviewer { User(String), Team(String) }   // Team: "org/slug" (GitHub), "Org/Name" (Forgejo)

pub struct Review {
    pub id: String, pub author: Option<String>,
    pub state: ReviewState,             // Approved | ChangesRequested | Commented | Dismissed | Pending
    pub commit: Option<String>,         // GitLab: none
    pub stale: bool,                    // not on the current head
    pub at: Option<i64>, pub body: Option<String>,
}

pub struct Check {
    pub name: String,
    pub source: CheckSource,            // CheckRun | Status | Action (Forgejo) | PipelineJob (GitLab)
    pub state: CheckState,              // Queued | Running | Success | Failure | Cancelled | Skipped | Neutral | ActionRequired | Manual
    pub allow_failure: bool,            // GitLab jobs
    pub url: Option<String>, pub description: Option<String>,
    pub run: Option<RunRef>,            // what Rerun acts on: suite / run / pipeline ids
}
pub struct Pr { pub item: Item, pub reviews: Vec<Review>, pub checks: Vec<Check>, pub rollup: Option<CheckState>, pub events: Vec<Event> }

pub struct Event {
    pub id: String,                     // provider-prefixed, stable: dedupe across polls
    pub at: i64, pub actor: Option<String>,
    pub kind: EventKind,                // Commented | ReviewComment | Reviewed | ReviewRequested | ReviewRequestRemoved
                                        // | Pushed{force, commits} | Labeled | Unlabeled | Assigned | Renamed | Referenced
                                        // | Mentioned | Merged | Closed | Reopened | ReadyForReview | ConvertedToDraft
                                        // | BranchDeleted | Milestone | System(String) | Other(String)
    pub target: Option<Reviewer>,       // who a request or mention is for
    pub thread: Option<String>, pub resolved: Option<bool>,   // GitLab discussions, GitHub review threads
    pub body: Option<String>,
}
```

`rollup`: Failure if any counted check failed, was cancelled or needs action. Else Running if any is
queued or running. Else Success. `None` when nothing is counted. Skipped, neutral, manual and
`allow_failure` checks aren't counted.

**What doesn't fit, or needs care:**

- **Checks: GitHub has two systems.**
  - The 18 checks on #14519 were all check runs, and its combined status was `state: "pending"` with
    `total_count: 0`. That means "no statuses", not pending. Ignore the combined state when
    `total_count` is 0, or compute the rollup ourselves (we do).
  - Rerunning is per check suite or workflow run, not per check, so `RunRef` keeps the suite id.
- **Checks: Forgejo has only statuses.** Actions write commit statuses. `target_url` is relative
  (`/forgejo/forgejo/actions/runs/203986/jobs/2`), and the run number in it is `index_in_repo`, not
  `actions/runs`' `id` (276 vs 111 for #84).
  - Forgejo adds a `skipped` status that GitHub's statuses don't have.
  - `need_approval` on an action run (a fork's first PR) is a state that waits on a maintainer, and
    statuses don't show it.
  - There's **no rerun API**, so *Rerun checks* on Forgejo is web-only for now. Hide it, or link to
    the run.
- **Checks: GitLab runs pipelines on merged results.** `head_pipeline` ran on `228f5a05`, a merge
  commit, not the MR's `sha` (`44611ba4`). "Checks on the head sha" has to mean "the MR's
  `head_pipeline`". Its jobs carry `allow_failure` and a `manual` state.
- **Review requests: Forgejo's `requested_reviewers` is everyone asked *or who reviewed*.** mfenniak
  approved #14667 and still appears there. A request is pending only while that user's or team's
  latest entry in `reviews` is `REQUEST_REVIEW`. A team's request is a review row with `user: null,
  team: {name}`, and it stays after a member approves (#14606).
  - GitHub clears a request when the person reviews and re-adds it on re-request (#13899: BagToad
    requested changes and is requested again).
  - GitLab has no team requests (group approval rules need auth). Its reviewers carry their own
    state (`unreviewed`, `reviewed`, `requested_changes`, `approved`), and approvals are separate:
    someone can approve without being a reviewer.
- **Draft vs `WIP:`.** All three have a boolean (`draft`; GitLab sets it from a `Draft:` title
  prefix), so the old `WIP:` convention needs nothing from us.
- **Review threads vs discussions.**
  - GitHub's line comments are their own endpoint (`pulls/N/comments`, 16 on #14519). Thread resolution
    is GraphQL only.
  - Forgejo's review comments are per review (`reviews/ID/comments`, N+1 requests), and the timeline
    has `code` entries.
  - GitLab's timeline *is* discussions: threads, resolvable notes and `system` notes for events. It
    needs auth even on public projects (401 anonymously), so the GitLab fixture has no timeline.
  - The model keeps `thread` and `resolved` on `Event`. M36 shows threads flat.
- **The timeline's size.** GitHub's timeline includes `cross-referenced` events with whole issue
  objects (188 KB for #14519, 292 KB for #13899). Paginate (`per_page=100`), keep only the newest
  events in state, and put the rest in the block's log.
- **Merge state.** Only GitHub says whether branch protection blocks a merge (`mergeable_state`:
  `clean`, `blocked`, `behind`, `unstable`). Forgejo's branch protection needs admin to read, and
  GitLab's `detailed_merge_status` needs auth for the details. So `done` on Forgejo is "green", not
  "mergeable".
- **Forgejo's AGit flow.** #14606's `head.ref` is `refs/pull/14606/head` (`flow: 1`), not a branch, so
  `checkout` uses `head_ref` and never `head.branch`.
- **Timestamps.** Forgejo answers in the server's offset (`+02:00` on Codeberg). Parse with offsets
  and store UTC milliseconds.

## 4. Attention rules

Who "you" is: `GET /user` with the block's login, cached per login. That's jhgaylor on Forgejo (id 1)
and jhgaylor on GitHub (id 1731794). Team requests need your teams, from `GET /user/teams`: Forgejo
gives `{name, organization.name}` (Jake: `NestedData/Owners`), GitHub `{slug, organization.login}`
(Jake: 1 team). Match requests on org plus name.

These are the rules in `attention()` (`model.mjs`), each run on real data:

| reason | rule | seen on |
|---|---|---|
| `ask` | open, and `requested` has you or one of your teams (pending only, per §3) | Codeberg #14657 as Gusted (direct); #14665 as a member of `Reviewers` (team); GitHub #13788 as babakks; #13899 as BagToad (re-requested after asking for changes) |
| `failed` | your PR, open, `rollup == Failure` | **Jake's `arugula-salad/studio#291`: 3 checks failed** (smoke gate, box smoke, smoke-minimal); Codeberg #14657 as n0toose (3 test jobs); GitHub #13788 as happysnaker (3 builds) |
| `input` | your PR, open, some reviewer's latest *decisive* review (approve, changes, dismiss; comments don't count) is `ChangesRequested` | GitHub #13899 as imkp1 ("changes requested by BagToad") |
| `input` | a `Mentioned` event for you (GitHub; its `actor` is who was mentioned, not who wrote it), or `@you` in a comment's body (Forgejo, GitLab), newer than the block's `seen_at` | GitHub #14519 as williammartin; #13899 as imkp1; Codeberg #14667 as wetneb ("Thanks @wetneb!" after the merge) |
| `done` | your PR merged; or open, `rollup == Success`, no changes requested, not a draft, `mergeable != false`, not `blocked` | Forgejo #84 as jhgaylor (merged); **Jake's `arugula-salad/studio#292` and `hud#736`: green, ready to merge** |
| none | your open PR with checks still running | Jake's `ravix-hq/ravix#456` (5 running) |

Notes for M36:

- `done` fires on the *transition* to green or merged. Clear it when the person opens the block, or
  after M24's usual hold.
- A mention needs a watermark (`seen_at`: when the person last opened the block or acted on it),
  otherwise every poll raises it again.
- `blocked` stops a false "ready to merge" on GitHub when a required review is missing. Forgejo can't
  tell (Codeberg #14665 counts as green for its bot author, though a review by `Reviewers` is still
  pending), so its `done` text says "checks green", not "ready to merge".

## 5. The PR's code

`fetch-pr.sh` does a treeless `git fetch` (`--filter=tree:0`) of the base branch and the PR's head ref
into a fresh scratch repo. It diffs `merge-base..head` by name and compares the list with the forge's
own:

| | head ref | fetch | diff | files | match |
|---|---|---|---|---|---|
| Forgejo `jhgaylor/illogical#84`, over the tailnet SSH remote | `refs/pull/84/head` | 182 ms, 196 KiB | 248 ms | 4 | **yes** |
| GitHub `cli/cli#14519`, **from the fork** `waldyrious/cli` | `refs/pull/14519/head` | 409 ms, 5.8 MiB | 317 ms | 2 | **yes** |
| GitLab `gitlab-org/cli!3941`, **from a fork** (project 45049979) | `refs/merge-requests/3941/head` | 669–702 ms, 2.4 MiB | 837–874 ms | 4 | **yes**, with the right base |
| Codeberg `forgejo/forgejo#14667`, **from the fork** `wetneb/forgejo` | `refs/pull/14667/head` | 4.3 s, 13.6 MiB | 1.8 s | 1 | **yes** |

- **A fork's head is in the base repo.** Every forge keeps the PR head ref there, so a fork needs no
  extra remote.
- **The base is a merge-base, never the target's tip.** Compute it with
  `git merge-base <target> <head>`. Forgejo's `merge_base` and GitLab's `diff_refs.base_sha` agree
  with what git computes.
  - GitLab's `diff_refs.start_sha` is the target's tip when the MR was last updated. Diffing from it
    listed **130 files instead of 4**.
  - For a merged PR, compute from the PR's own `base.sha` (GitHub) or use the forge's merge base,
    since the target has moved on.
- **M11's diff block needs no change.** It takes `{repo, rev_a, rev_b}` and runs `git diff -M rev_a
  rev_b`, so the block opens it with `rev_a = merge_base` and `rev_b = refs/illogical/pr/N`.
- **For `checkout`,** fetch `+<head_ref>:refs/illogical/pr/N` into the person's existing clone. This
  touches no branch. Then run `git worktree add .illogical/worktrees/pr-N refs/illogical/pr/N`
  (detached; a branch named `pr-N` if they want to push).
  - That wasn't run on this repo's real clone, because a worktree shares refs with the main checkout.
  - A treeless clone works for listing files, but a checkout fetches every blob, so use the person's
    clone.

## 6. Drafts as asks

Measuring ask → posted is **out of scope**: it needs a real post, and this spike writes to no forge.
Here is what M35 built on this branch, and what M36 changes to put a draft on a `forge` block:

**What M35 built:**

- `POST /api/panes/N/ask` and `BlockCtx::ask` (`crates/daemon/src/block.rs`) put an ask on any block.
- `Mux::ask` refuses only `Agent` and `Remote` blocks (`crates/daemon/src/mux.rs`), so a new
  `BlockType::Forge` can ask without changing that check.
- An answer comes back on the asker's oneshot as `Replied = (AskReply, Option<Driver>)`. Withdraw
  goes by `(id, token)`.
- hud's follower (`crates/daemon/src/apps/hud.rs`, `reconcile`) is the pattern to copy. It raises the
  oldest open question, waits for the reply on a spawned task, then raises the next one.

**What M36 has to change:**

1. **One ask per block.** `Mux.asks` is a `HashMap<PaneId, TermAsk>`, and a second ask *withdraws*
   the first. So the forge block keeps its own queue, `drafts: Vec<Draft{id, method, args, by, at}>`,
   raises the oldest, and raises the next when that one is answered. The block's state lists every
   draft, so the UI can show "2 more drafts". Growing `asks` to a list per pane is the alternative
   (it touches M24, M29 and the clients). The queue is smaller.
2. **The card is a form.** Use `AskKind::Form` with the schema `{body: {type: string, default: <the
   agent's text>, format: "markdown"}}` (plus `event` for a review: approve, request_changes or
   comment), `source: "forge"`, and `agent: "<MCP client>"` ("claude-code drafted a comment on #84").
   Answer with the edited `{body}` to send it, or *decline* to drop it.
   - The web's generic form draws a string as a one-line `<input type="text">`
     (`web/src/blocks/ask.tsx`, `FormCard`), so M36 adds a `<textarea>` for `format: "markdown"` (or
     `maxLength` > 200).
3. **`approve` collides.** `api.rs`'s `call` route sends `approve`, `deny`, `answer` and `decline` on a
   block that *holds an ask* to `answer_terminal`. The only exception is an `approve` with `key` or
   `member` (M35's gates). So a person's `approve` on a PR, while a draft waits, would be read as
   answering the draft's card (and refused: "asks a question: answer it").
   - Recommendation: name the PR writes `review {event: approve | request_changes | comment, body}`,
     `comment`, `merge`, `close` and `rerun_checks`, so there's no `approve` method on a forge block.
   - Alternatively, add `BlockType::Forge` to the exception.
4. **Who called.** `call` resolves `by` (a `Driver`) only for `approve`, `deny`, `answer`, `decline`,
   `send`, `terminal` and `open`, so M36 adds its write methods to that list.
   - **An agent's call** becomes a draft: MCP's `driver()` is `who: "mcp:<client>"`.
   - **A person's call** goes out directly.
   - **`illogical pr comment` from a terminal** reaches the daemon as `Owner` over the socket. The
     CLI sends `agent: true` when `CLAUDECODE` or `AI_AGENT` is set (both are set in Claude Code's
     shells), and the daemon makes that a draft.
   - **This is a courtesy, not a boundary.** An agent on Jake's uid can run `tea` or `gh` itself.
     "Agents draft" holds on illogical's own surfaces, and the README and the tool descriptions should
     say so.
5. **MCP tools.** There's no generic "call a block's method" tool today (`crates/daemon/src/mcp/tools.rs`).
   - Add `open_pr`, `read_pr` (read-only) and `pr_comment`, `pr_review`, `pr_merge`. The write tools
     are `read_only: false`, and `Scope::Read` already refuses those.
   - A write tool returns at once: `{draft: "<id>", status: "waiting"}`, not blocking until a person
     answers, since a review can wait hours and MCP calls time out.
   - The agent follows its draft with `read_pr` (`drafts[]`, each `waiting`, `sent` with a URL, or
     `dropped` with who).
6. **Who may send.** A write goes out with the block's login: the owner's CLI token, the only one the
   daemon has. M35 made `enter` owner-only for the same reason (it signs in as them). So for M36:
   - **Sending a draft, or any direct write:** owner only, until per-person logins exist.
   - **Editors:** may draft and drop.
   - **Viewers:** read.
   - PLAN says "Owner and editors only". Letting an editor send would post as Jake, so this needs a
     decision.

**The send path** (answer → method):

1. A person answers the card: the web, the phone, `illogical attention act`, or
   `POST /api/blocks/N/call/answer {id, content: {body}}`.
2. `answer_terminal` passes it through `Mux::ask_reply`, and the draft's oneshot gets
   `(AskReply::Answer({body}), Some(driver))`.
3. The forge block's task checks the driver's role (owner, per point 6), then runs
   `adapter.write(Write::Comment{body})`. That gets the token (`tea login helper get`, cached) and
   sends the request with `reqwest`:
   - Forgejo: `POST /repos/O/R/issues/N/comments`, or `POST /pulls/N/reviews {event, body}` for a review
   - GitHub: the same paths
   - GitLab: `POST /projects/ID/merge_requests/IID/notes`, or `/approve`
4. On success it appends `{kind: commented, by: driver, drafted_by: "mcp:claude-code", url}` to
   `blocks/%N/`, marks the draft `sent`, refreshes, and raises the next draft.
   - `Decline` marks it `dropped by <name>`.
   - A failed post puts the draft back with the error on its card, so the text isn't lost.
5. `Api::Answered` records "Sent by Jake" on every other client, as M29 does.

## Files

- `read.mjs`: one full read and N unchanged polls through a CLI (`--via cli`) or HTTP (`--via http`,
  token from the CLI and only in memory). `--save DIR` writes the fixtures.
- `github-graphql.sh`: the same GitHub read as one GraphQL query.
- `model.mjs`: the normalized model, an adapter per forge, `rollup` and `attention`. `node model.mjs --all`.
- `fetch-pr.sh`: fetch a PR's head and compare `merge-base..head` with the forge's file list.
- `fixtures/<forge>-<repo>-<n>/`: response bodies as the forges sent them, plus `_full.headers.json` and
  `_poll.headers.json` (status, ms, bytes, and only ETag, rate-limit, paging and cache headers; never
  auth or cookies). They're all public repos. Jake's private PRs were read into the scratch directory
  only, not saved here. The Codeberg `action_runs.json` (and #14606's statuses), and some `files.json` and
  `review_comments.json` were dropped for size.
