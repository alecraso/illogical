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
| `just control-smoke` (in `just test`) | `web/control-smoke.ts`: a fake GitHub, Stripe, push service and Sprites API, the real `illogical-control` and a real daemon; the script signs in, enrolls, approves the daemon's join code and reaches it directly and through the relay | Linux and macOS |
| `just e2e` | the Playwright specs in `web/e2e/` against throwaway daemons (`just e2e <url>` tests a running one) | no |
| `just desktop-check` | rustfmt and clippy for `crates/desktop` | Linux |
| `just check-macos` | clippy for the macOS target from Linux (compiles, doesn't link) | Linux |

CI (`.github/workflows/check.yml`) runs on pushes, on our own machines: `just
check` on Linux (geek), `just test` on macOS (jake-mini). See
[development.md](development.md) for the runners.

## The daemon's integration tests

`crates/daemon/tests/*.rs` start `illogicald` (`CARGO_BIN_EXE_illogicald`)
with `--listen 127.0.0.1:0`, a short state directory under the temp dir,
and usually `--shell "bash --norc --noprofile"`, then drive it over its
Unix socket and HTTP API. Shared pieces:

- `listen/`: waits for the port the daemon took (it writes it to
  `state/listen`). Don't pick a free port yourself and pass it in: it can be
  taken before the daemon binds it (#66).
- `strays/`: removes what a test daemon leaves behind, programs and its
  state directory (#35, #68).
- `agentd/`: a daemon for agent block tests, driven over its socket.

Most files still have their own `Daemon` struct and start function; a
shared harness is planned in #200.

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
| `mcp.spec.ts`, "the real Claude Code runs a build over MCP" | `ANTHROPIC_API_KEY` and `claude` on PATH (costs a few cents) |
| `team-swarm-phones.spec.ts`, "a machine on another network, behind netem" | `ILLOGICAL_TESTNET_PHONES=1`, Docker and `just static <arch>`; it builds a small Debian image with `tc` and `socat`, names its container and network after `COMPOSE_PROJECT_NAME`, and removes them after |

## By hand

- `just dev`: a separate daemon on 7682 and Vite on 5173.
- `just fake-fleet`: three throwaway daemons with scripted work on
  7730-7732, for the swarm.
- `just screenshots`: the images in `site/img/`, from a scripted session.
- iTerm2's tmux mode: [development.md](development.md#testing-iterm2).

## Planned

Tracked in #200:

- `testnet/`, a Compose stack with profiles for network shapes and real
  services: ssh boxes behind a bastion, control and a box that can only
  dial out, Fountain, Forgejo. Modelled on terragucci's `stack/`.
- Client fixtures: recorded daemon sessions a client can replay against,
  and a daemon check against previous releases' fixtures.
- A shared harness crate in place of the per-file `Daemon` copies.
- The Playwright suite in CI.
