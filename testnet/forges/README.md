# testnet/forges: real forges for the forge blocks

Forgejo and GitLab CE in Docker, each with two bot users, for the checks
#93 left to a person (M36-M40). A Compose project of its own
(`illogical-testnet-forges`), separate from the ssh stack in `testnet/`.

```sh
just forges up forgejo      # seconds
just forges test forgejo    # crates/daemon/tests/integration/forges_real.rs, forgejo_*
just forges up gitlab       # GitLab CE and a shell runner: 3-5 minutes, 4 GB
just forges test gitlab
just forges down            # both, with their volumes and tokens
```

They need Docker: without it every script fails, unless
`ILLOGICAL_SKIP_DOCKER=1`, which skips and says nothing ran. The tests are
`#[ignore]`d in a plain `cargo test`; `just forges test` runs them with
`--ignored` and `ILLOGICAL_TESTNET_FORGES` naming the state directory, and
a test fails if its forge's file isn't there.

## What's in it

| Service | Image | Host port | Notes |
|---|---|---|---|
| `forgejo` | `codeberg.org/forgejo/forgejo:16.0.5` | `127.0.0.1:17746` | sqlite, no ssh, `[webhook] ALLOWED_HOST_LIST=host.docker.internal` |
| `gitlab` | `gitlab/gitlab-ce:18.4.1-ce.0` (arm64 and amd64) | `127.0.0.1:17747` | outbound allowlist `host.docker.internal` for hooks |
| `gitlab-runner` | `gitlab/gitlab-runner:v18.4.0` | none | a shell-executor instance runner, so a pipeline can fail and be retried |

Ports can be moved with `ILLOGICAL_TESTNET_FORGEJO_PORT` and
`ILLOGICAL_TESTNET_GITLAB_PORT`. GitLab answers on the same port inside the
container, so the links it writes work from the host.

`up.sh` makes `illo-author` and `illo-reviewer` (and an admin on Forgejo,
`root`'s token on GitLab for registering the runner), gives each a fresh
token, and writes `testnet/forges/.state/<forge>.json` (0600, git-ignored).
Nothing in it is a real account.

## How webhooks get back

A test daemon listens on the host's loopback. The forge reaches it as
`http://host.docker.internal:<port>` (Docker Desktop's name for the host;
on Linux the `host-gateway` entry in compose.yaml), which the test passes as
`ILLOGICAL_FORGE_HOOK_BASE`. Each forge allows that host and nothing else
private: Forgejo by `ALLOWED_HOST_LIST`, GitLab by its outbound allowlist.
That was the open question in #93's last Forgejo box.

## What the tests cover

Each test makes its own repository, so they can run again on the same
stack.

| Test | #93's box |
|---|---|
| `forgejo_a_review_asked_of_you_reaches_the_rail_and_is_approved_as_you` | a review request reaches the rail and the phone (a push), approve, Forgejo shows it as you |
| `forgejo_an_agents_pr_comment_waits_and_goes_out_edited_as_you` | `pr_comment` over MCP waits as a card, edited, sent, on Forgejo with `history` naming who sent it |
| `forgejo_agent_on_this_works_on_a_branch_and_its_pr_joins_the_tab` | *Agent on this* on a real issue: `iNN-<slug>`, the PR block joins the tab |
| `forgejo_live_updates_make_a_real_hook_the_forge_delivers_to` | *Live updates*: the hook is made with your token, Forgejo delivers, the block hears a comment inside a minute-long poll, off removes it |
| `forgejo_a_red_check_is_a_failure_with_its_link` | a red status is a failure with its run's link |
| `gitlab_a_review_asked_of_you_is_approved_from_the_rail_with_glabs_token` | `glab config get token` after `glab auth login --token`; review request on the rail and phone; approve |
| `gitlab_a_red_pipeline_is_rerun_from_the_rail` | *Rerun* on a red pipeline |
| `gitlab_live_updates_make_a_real_hook_the_forge_delivers_to` | GitLab's hook, delivered |

The person is stood in for only where they hand over a token: a stand-in
`tea` knows it (tea isn't installed here), and a real `glab` is logged in
with `glab auth login --token` into a config dir of the test's own (a
stand-in if there's no glab). `glab auth login --web` (OAuth in a browser)
is not covered. The stand-in agent (`fake_acp.py`) plays the agent.

GitHub's boxes run against github.com in the nightly workflow
(`.github/workflows/forges-nightly.yml`), which needs a test org and bots.
