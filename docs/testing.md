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

## Tests that need something extra

These skip, saying why, unless what they need is there:

| Test | Needs |
|---|---|
| `agents_real.rs`, `swarm-real.spec.ts` | `ILLOGICAL_REAL_AGENTS=claude,codex,...` (real agents; costs a few cents) |
| `resident.rs`, `resident.spec.ts`, `editors-vm.spec.ts` | a wispd token and `just static` |
| `sandbox.spec.ts` (`just e2e-sandbox`) | `ILLOGICAL_E2E_TAILNET_AUTHKEY_FILE` and wispd |
| `workspace.spec.ts` | network on its first run, to install the pinned chant |

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

- `testnet/`, a Compose stack with profiles for network shapes and real
  services: ssh boxes behind a bastion, control and a box that can only
  dial out, Fountain. Modelled on terragucci's `stack/`. (Forgejo and
  GitLab are in `testnet/forges`, above.)
- Client fixtures: recorded daemon sessions a client can replay against,
  and a daemon check against previous releases' fixtures.
- A shared harness crate in place of the per-file `Daemon` copies.
- The Playwright suite in CI.
