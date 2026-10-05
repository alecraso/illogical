# Testing

Every test runs the real binaries as child processes, each with its own
temp state directory and a port the OS picks. Anything outside illogical
(GitHub, Fountain, an agent, Stripe) is a fake served by the test or a
small script, or a response recorded from the real service and checked in.
Nothing in the default run reaches the network or costs money.

```sh
just test        # Rust tests, the web typecheck, e2e-interop, control-smoke
just check       # just test, plus rustfmt and clippy (what CI runs on Linux)
just e2e         # the browser tests, in the system Chrome
```

## What runs where

| Command | What it runs | In CI |
|---|---|---|
| `cargo test --workspace` (in `just test`) | unit tests in every crate, and the daemon's integration tests in `crates/daemon/tests/` | Linux and macOS |
| `just e2e-interop` (in `just test`) | the browser's end-to-end crypto (`web/src/e2e`) against Rust's (`crates/e2e`): certificate vectors made by `crates/e2e/examples/interop.rs`, and a Noise handshake with its `responder` | Linux and macOS |
| `just control-smoke` (in `just test`) | `web/control-smoke.ts`: a fake GitHub, Stripe, push service and Sprites API, the real `illogical-control` and a real daemon; headless devices sign in, enroll, approve the daemon's join code and reach it directly and through the relay | Linux and macOS |
| `just e2e` | the Playwright specs in `web/e2e/` against throwaway daemons (`just e2e <url>` tests a running one) | no |
| `just desktop-check` | rustfmt and clippy for `crates/desktop` | Linux |
| `just desktop-xvfb` | the Linux desktop app under Xvfb in a container (`packaging/desktop/xvfb/`): it opens on a static daemon's page, follows a join to the app's sign-in (a stand-in control) and a leave back (#204) | no |
| `just check-macos` | clippy for the macOS target from Linux (compiles, doesn't link) | Linux |

CI (`.github/workflows/check.yml`) runs on pushes, on our own machines: `just
check` on Linux (geek), `just test` on macOS (jake-mini). See
[development.md](development.md) for the runners.

## The daemon's integration tests

`crates/daemon/tests/*.rs` start `illogicald` (`CARGO_BIN_EXE_illogicald`)
through `crates/testkit`, then drive it over its Unix socket and HTTP API.

```rust
use illogical_testkit::{Daemon, illogicald};

let d = illogicald!("api").env("PS1", "$ ").no_wisp().start();
let pane = d.post("/api/run", json!({"command": "exit 4"}))["pane"].as_u64().unwrap();
assert_eq!(d.get(&format!("/api/panes/{pane}/wait?until=exit&timeout=10"))["code"], 4);
d.wait_for("its history", || d.get(&format!("/api/history?pane={pane}"))[0]["exit"] == 4);
```

`illogicald!(tag)` gives a `Builder` for the test's own daemon binary. Every
daemon it starts gets `--listen 127.0.0.1:0`, `--no-manager-env`,
`--shell "bash --norc --noprofile"` (`.shell()` or `.default_shell()` to
change it), a short state dir under the temp dir named after the tag
(`.state_dir()` for one of the test's own), and no `NOTIFY_SOCKET`; its
output goes nowhere. The rest is the test's to say:

- `.arg()`, `.args()`; `.no_wisp()` and `.no_tailscale()` keep this host's
  wispd and tailscaled out of it; `.block_listen()` adds
  `--block-listen 127.0.0.1:0`.
- `.env()`, `.envs()`, `.env_remove()`, and `.path()` for its `PATH`, so a
  test can keep it from finding something installed on this machine (chant,
  say).
- `.wait_secs()`: how long `wait_for` waits (15 s by default).
- `.start()` runs it as a child of the test; `.service()` as a transient
  systemd user service (FD store and scopes as in production), or `None`,
  saying so, without a user manager.

`start()` returns once the daemon answers on its socket and ports, and
panics if it exits first. A `Daemon` has its `port`, `block_port` and
`state` dir, and:

- `raw`, `get`, `post`: requests over the socket (the owner's, so no
  credential); `tcp`: one over TCP with exactly the headers given; `ws`: a
  WebSocket request with the local token; `token`, `bearer`, `url`, `sock`.
- `wait_for(what, f)`, and `illogical_testkit::wait_for` with a timeout of
  its own.
- `stop` (SIGTERM, as systemd stops it), `kill` (SIGKILL), `signal`, then
  `start` to bring it back on the same ports and state; `restart_service`
  and `unit` for a service.

Dropping it kills it, kills what its panes left running and removes its
state dir (#35, #68). With `ILLOGICAL_KEEP_TEST_STATE=1` the dir stays and
its path is printed. A file that needs more (deleting machines it made,
stopping the code-servers it started) wraps the `Daemon` in a struct of its
own with a `Drop` that does that first, as `machines.rs` and `editors.rs`
do.

Also in the crate: `listen`, which reads the port a daemon took from
`state/listen` (don't pick a free port yourself and pass it in: it can be
taken before the daemon binds it, #66); `strays`, the cleanup above; and
`Scratch`, a temp dir removed on drop. `crates/daemon/tests/agentd/` builds
on it for agent block tests: a sessions dir for the fake agent, and helpers
to open blocks and wait on them.

Waiting on something that takes as long as the machine is busy (a flood
of output, a build): stop it or wait for its end, never sleep a fixed time
and hope, and make deadlines failure limits that are generous (tens of
seconds) rather than waits. A reader that has to keep up with a flood
(the tmux client in `tmux.rs`) does as little per line as it can and lets
its own timeouts expire while lines keep coming. `falling_behind_pauses_the_pane`
failed under a load average of 30 because a fixed 40 MB flood was still
running after `continue`; it now runs `yes` until the pane pauses, sends
^C and waits for the prompt in a capture.

Standing permission rules (#166) are tested in `agents.rs`
(`standing_rules_outlive_the_block_that_made_them`: a `cwd` rule answers a
new block below that directory and not one elsewhere, an `everywhere`
prefix rule allows its command with arguments but not `cargo testify` or
`cargo test; rm`, the rules survive a restart in `rules.json`, and
forgetting one brings the card back), in `rules.rs`'s unit tests (matching,
prefixes, the file) and in `web/e2e/agents.spec.ts` (*From now on…* on a
card, a second block that never asks, and *Permission rules…* in the
session menu forgetting it). The browser test clears the rules first: the
suite's daemon keeps them between specs.

The fakes:

- `fake_acp.py`: an ACP agent, standing in for Claude Code's adapter and
  for `fountain`.
- `fake_claude.py`: Claude Code talking to its IDE (M28), as S17 recorded it.
- `fake_mcp.py`: an MCP server whose tools ask the user through elicitation.
- `fake_code_server.py`: code-server (M27).
- Fakes inside the test files themselves: Fountain's API (`fountain.rs`),
  control's relay socket and GitHub App token endpoint, GitHub and Forgejo
  with stand-in `gh` and `tea` (`forge_live.rs`), `systemctl` and `sudo`.

## Fixtures

Recorded from real systems and checked in, so tests see real shapes:

| Where | What | Re-recording |
|---|---|---|
| `crates/vt/fixtures/` | raw PTY output of scripted sessions (`.bin`) and their sizes and resizes (`.json`) | `just fixtures [names]` (`record.py`) |
| `crates/daemon/tests/fixtures/github`, `gitlab`, `forgejo` | API responses for real PRs and issues (from S23) | by hand, as in `spikes/s23-forge/` |
| `crates/daemon/tests/fixtures/conversations/` | Claude Code transcripts, one per shape (S20) | by hand, as in `spikes/s20-conversations/` |
| `crates/daemon/tests/fixtures/s13-*`, `s18-*` | Claude Code hook payloads | by hand |
| `crates/daemon/tests/fixtures/fountain/`, `chant/` | Fountain API and chant output | by hand |

Scrub anything personal or secret before checking a recording in;
`gitleaks` runs in CI.

## Browser tests

`web/playwright.config.ts` makes the run's directories once and points the
daemons at them: a Claude directory, the IDE lock directory, agent
adapters that are `fake_acp.py`, and a stand-in npm. Daemons and control
take `--listen 127.0.0.1:0` and fake servers listen on port 0
(`web/e2e/ports.ts`), so a run reserves only `E2E_PORT` and two worktrees
can run the suite at once (#67). `web/e2e/helpers.ts` has the page
helpers (`open`, `ready`, `type`, `run`, `text`, ...).

Make temp directories in `beforeAll`, not at the top of a spec: Playwright
loads each spec in the runner as well as the worker (#62).
`E2E_DAEMON_LOG=<file>` keeps the test daemon's debug log, and
`E2E_CONTROL_LOG=1` shows control's output in `sandboxes.spec.ts`.

A check that should hold in Chrome and WebKit goes in a plain module both
projects' specs import, since a spec importing another spec registers its
tests twice. The command palette (#139) is the example:
`web/e2e/palette-steps.ts` holds the checks, `palette.spec.ts` runs them in
Chrome (desktop and a Pixel 7) and `palette.webkit.spec.ts` in WebKit
(Ctrl+Shift+P, Cmd+Shift+P, and an iPhone 13). They cover the chord
opening the palette from a focused terminal without the shell seeing it,
the palette listing exactly the pane menu's actions, running actions by
typing (split, a restart policy, move to a new tab, rename the tab through
a prompt), recent picks listed first, jumping to a tab, and the phone's
full-height sheet from the sheet's Commands button. Run them with:

```sh
cargo build -p illogicald && (cd web && pnpm run build)
cd web && pnpm exec playwright test e2e/palette.spec.ts e2e/palette.webkit.spec.ts
```

WebKit needs `pnpm exec playwright install webkit` once.

## A device that approves things

Anything that waits for a person to approve it on a signed-in device (a
daemon's `illogicald join`, a second browser, a CLI) is approved in tests by
`web/fixtures/device.ts`. It's the web client's own e2e code
(`web/src/e2e`) without a page: it signs in through control's GitHub
sign-in (against the fake GitHub in `web/fixtures/fakes.ts`, which signs in
whoever the device names), makes and enrolls device keys, and checks
everything it accepts against the account root it pinned, as the browser
does.

```ts
import { Device } from "./fixtures/device.ts";

const me = await Device.signIn({ control, login: "alice" }); // the account's first device: trusted
const phone = await Device.signIn({ control, login: "alice", name: "phone" }); // waits
await me.approveDevice(phone);
await me.approveJoin("ABCDE-FGHIJ");              // the code after #join=
const box = await me.waitOnline("box");          // control's directory
await me.roundTrip(box.id, "MARKER");            // echo through its first pane, over the relay
const sock = await me.connect(box.id);           // or an E2ESocket of your own
```

- `control` is control's public URL. When the test reaches it at another
  address (a container's), `via` maps one to the other:
  `{ "http://10.229.80.10:8080": "http://127.0.0.1:22980" }`.
- `trusted()` is the account's devices and machines that chain to the
  pinned root; `api(path, body?)` is any other control call with the
  device's session.
- `save(file)` and `Device.load(file)` keep a device between steps.

`web/fixtures/device-cli.ts` is the same from a shell or a Rust test, with
the device in a state file; each command prints one JSON object:

```sh
d() { node --experimental-strip-types web/fixtures/device-cli.ts --state /tmp/dev.json "$@"; }
d signin --control http://127.0.0.1:7690 --login alice   # {account, fingerprint, device, approved}
d approve ABCDE-FGHIJ                                     # {device, name}
d devices                                                 # {devices: [{id, kind, name}]}
d online box 30                                           # wait up to 30s
d pane box MARKER                                         # round-trip through its first pane
```

`illogicald join --account <fingerprint>` (and `illogical join
--account`) takes the account without asking; the fingerprint is what
`signin` printed. `just control-smoke` and the testnet's `control` claims
use both.

## Phones

The phone checks (#214 section 6) run as Playwright device contexts, with
no phone and no person. `web/e2e/phones.ts` has what they share:

- `pixel7` and `iphone`: context options (viewport, user agent, touch,
  mobile) from Playwright's Pixel 7 and iPhone 15. A Pixel 7 runs in
  Chrome. An iPhone runs in WebKit, either as a `*.webkit.spec.ts` file
  (the `webkit` project) or from a Chrome spec with `launchWebkit()`, which
  drops the project's `chrome` channel.
- `FakePush`: a web-push service on loopback. `subscription(name)` makes
  keys the service holds, so it can decrypt what the daemon sends (RFC
  8291, aes128gcm) and check its VAPID token (RFC 8292); `next(name, match)`
  waits for a decrypted payload, and `refused` lists anything it turned
  away. In Chrome, `stub(page, name)` (before the page loads) makes the
  page's `PushManager` hand out that subscription, so *Notify this device*
  subscribes to the fake service. WebKit in Playwright has no
  `PushManager` or `Notification`, so an iPhone test posts the
  subscription to `/api/push/subscribe` itself, as the page would.
- `deliver(context, page, payload)` hands a payload to the page's service
  worker over CDP and returns the notification's actions; `tap(context,
  tag, action)` dispatches a `notificationclick` on it (Chrome only).
- `daemon(state, args)`: a throwaway daemon on a port of its own.

`web/e2e/team-fixture.ts` has a local control with a fake GitHub sign-in,
people signed in on any browser context, and machines joined to it.

| Milestone | Spec | What runs |
|---|---|---|
| M11 | `changes.spec.ts` (Pixel 7), `changes.webkit.spec.ts` (iPhone) | Changes, a hunk's line, a live file block; a failed build's push through the fake service, and Rerun from the notification (Pixel 7) or Needs you (iPhone) |
| M16 | `mcp.spec.ts`, "watched from a phone" | an MCP client's build drawn live on a Pixel 7, Failed on its Needs you, fixed and rerun by the client |
| M27 | `editors.spec.ts` | an editor block opened from a Pixel 7 and from an iPhone, the file at its line in under 3 s on a warm server |
| M26, M30 | `team-swarm-phones.spec.ts` | two teammates' swarms on a Pixel 7 (Chrome's network emulation, 150 ms, 1.6 Mbit/s) and an iPhone: grouped by person, cards along the bottom, a rerun from the iPhone's card seen on the Pixel, a tile tap opening the pane |
| #86 | `studio-phone.spec.ts` | a studio app from the template through the token API (a fake studio and box), `illogical studio login`, `hud share --role follower`, `illogical studio follower`, `illogical app`; a question answered from a Pixel 7 and a gate approved from an iPhone, with hud told who |

Run one with `cd web && E2E_PORT=<port> pnpm exec playwright test
e2e/<spec>`; `editors.spec.ts` needs code-server, which its first test
downloads.

## Tests that need something extra

These skip, saying why, unless what they need is there:

| Test | Needs |
|---|---|
| `agents_real.rs`, `swarm-real.spec.ts` | `ILLOGICAL_REAL_AGENTS=claude,codex,...` (real agents; costs a few cents) |
| `resident.rs`, `resident.spec.ts`, `editors-vm.spec.ts` | a wispd token and `just static` |
| `sandbox.spec.ts` (`just e2e-sandbox`) | `ILLOGICAL_E2E_TAILNET_AUTHKEY_FILE` and wispd |
| `workspace.spec.ts` | network on its first run, to install the pinned chant |
| `just testnet test ssh` | Docker, and `just testnet up ssh` first ([testnet/README.md](../testnet/README.md)) |
| `just testnet test control` (M52 end to end) | Docker and node, and `just testnet up control` first, which builds the static binaries |
| `mcp.spec.ts`, "the real Claude Code runs a build over MCP" | `ANTHROPIC_API_KEY` and `claude` on PATH (costs a few cents) |
| `team-swarm-phones.spec.ts`, "a machine on another network, behind netem" | `ILLOGICAL_TESTNET_PHONES=1`, Docker and `just static <arch>`; it builds a small Debian image with `tc` and `socat`, names its container and network after `COMPOSE_PROJECT_NAME`, and removes them after |

## The test stack

[`testnet/`](../testnet/README.md) is a Docker Compose stack, one profile per
network shape, for what one host's loopback can't show. Its README has the
profiles, the claims each one checks (and how `BREAK=1` breaks them), and
how to run two stacks side by side (`COMPOSE_PROJECT_NAME`). Tests built on
it:

| What | Where | Run |
|---|---|---|
| M51: `--ssh` installs illogical on a bare box behind a bastion, runs and captures panes, forwards the agent; a `git push` from a pane reaches the stack's git server with the forwarded agent and is refused without it | `crates/daemon/tests/ssh.rs` | `just testnet up ssh`, `just static <arch>`, then `cargo test -p illogicald --test ssh` |
| #26: a lingering daemon on box-systemd survives `docker restart` (twice): up with nobody logged in, layout, directories and coloured scrollback back with `── restored`, every pane by its policy, the browser and agent blocks, "saved for shutdown" and "restored" in the journal | `crates/daemon/tests/reboot.rs` | as above, plus `cd web && pnpm install`; `cargo test -p illogicald --test reboot` |
| A headless web client attached across that restart reconnects by itself, without reloading | `web/reconnect-watch.ts`, driven by `reboot.rs` | (in `reboot.rs`) |
| S28: the same daemon over `--ssh` and over a tailnet (headscale and two Tailscale nodes), timed | `testnet/measure-tailnet.sh` | `just testnet up tailnet`, `just static <arch>`, `just testnet measure tailnet` |

These require Docker: without it they fail, and they bring the stack's
profile up themselves when it isn't. They also need `just static <arch>`
(`reboot.rs` also node and Playwright's Chromium in `web/`), and fail saying
so without it. `ILLOGICAL_SKIP_DOCKER=1` is the only way to skip them, and
they print that nothing ran. They recreate the boxes they use, so give each
worktree its own stack (`COMPOSE_PROJECT_NAME` and
`ILLOGICAL_TESTNET_SSH_PORT`). The stack isn't in CI yet (#200).

## Docker stacks: real forges, two hosts, VS Code over Remote-SSH

These need Docker. Without it they fail (non-zero exit); only
`ILLOGICAL_SKIP_DOCKER=1` skips them, and then they print that nothing
ran. They aren't part of `just check`: each has its own recipe.

| What | Run | Details |
|---|---|---|
| Forgejo and GitLab CE with two bot users and webhooks (#93, M36-M40) | `just forges up forgejo && just forges test forgejo`, the same with `gitlab` (3-5 minutes and 4 GB to start), `just forges down` | [testnet/forges/README.md](../testnet/forges/README.md) |
| #17 on two machines: home's layout holds panes on `mac`, which drops off the network (`docker network disconnect`) and comes back | `just testnet-hosts` | [testnet/hosts/README.md](../testnet/hosts/README.md) |
| M28 in real VS Code (downloaded by `@vscode/test-electron`) over Microsoft's Remote-SSH into a box running illogicald: a phone follows the cursor, a breakpoint is a card it continues, an edit is accepted from its rail | `just testnet-editors` (downloads VS Code, its server and Remote-SSH; on Linux it runs under `xvfb-run`) | [testnet/editors/README.md](../testnet/editors/README.md) |

The forge tests are `crates/daemon/tests/forges_real.rs`, marked
`#[ignore]` so `cargo test` doesn't need the containers;
`testnet/forges/test.sh` runs them with `--ignored` against the stack
`up.sh` started, and they fail if it isn't there. The other two are
Playwright specs (`web/e2e/testnet-hosts.spec.ts`,
`web/e2e/editor-remote-ssh.spec.ts`) that bring their stack up and down
themselves; `just e2e` lists them as skipped unless their recipe's
variable (`ILLOGICAL_TESTNET_HOSTS=1`, `ILLOGICAL_TESTNET_EDITORS=1`) is
set.

### The nightly job against github.com

`.github/workflows/forges-nightly.yml` runs every night and on demand
(never on pull requests): the Forgejo and GitLab tests above, and
`crates/daemon/tests/forges_github_real.rs` against github.com, which
covers #93's GitHub boxes (a review approved from the rail, a red Actions
check rerun, a box with no `gh` login reading through the App's
installation token and refusing writes, and the App's webhook poking the
block). Without its secrets the GitHub job passes with a notice naming
each one that's missing, and each test prints `SKIP <test>: not set: ...`.
It needs, in the repository's Actions settings:

| Name | Kind | What |
|---|---|---|
| `ILLOGICAL_GH_TEST_REPO` | variable | `org/repo` in a test organization: public, both bots can write, with `.github/workflows/illogical-red.yml` on its default branch (a job that fails on pushes to `red-*`) |
| `ILLOGICAL_GH_AUTHOR_TOKEN` | secret | the first bot's token: contents, pull requests, issues and actions, read and write, on that repository |
| `ILLOGICAL_GH_REVIEWER_TOKEN` | secret | the second bot's token, the same |
| `ILLOGICAL_GH_APP_ID` | variable | a test copy of illogical's GitHub App, installed on the test organization, with pull request and issue comment events |
| `ILLOGICAL_GH_APP_PRIVATE_KEY` | secret | that App's private key (PEM) |

Control is stood in for in that test, and the App's deliveries are read
back through GitHub's API (a runner has no public URL), so control
checking GitHub's signature on a real delivery is not covered there.

## By hand

- `just dev`: a separate daemon on 7682 and Vite on 5173.
- `just fake-fleet`: three throwaway daemons with scripted work on
  7730-7732, for the swarm.
- `just screenshots`: the images in `site/img/`, from a scripted session.
- iTerm2's tmux mode: [development.md](development.md#testing-iterm2).

## Planned

Tracked in #200:

- More `testnet/` profiles: `ssh` (boxes behind a bastion), `control`
  (control with its relay, reached by boxes that only dial out) and
  `tailnet` (headscale and two Tailscale nodes) exist, and Forgejo and
  GitLab are in `testnet/forges` (above); Fountain doesn't have one yet.
- Client fixtures: recorded daemon sessions a client can replay against,
  and a daemon check against previous releases' fixtures.
- The Playwright suite in CI.
