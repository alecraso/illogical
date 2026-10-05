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
- `crates/vt/fixtures/agents/replay.py`: Claude Code or Codex in a
  terminal, played back from a recording ([below](#the-replay-agent)).
- `fake_mcp.py`: an MCP server whose tools ask the user through elicitation.
- `fake_code_server.py`: code-server (M27).
- Fakes inside the test files themselves: Fountain's API (`fountain.rs`),
  control's relay socket and GitHub App token endpoint, GitHub and Forgejo
  with stand-in `gh` and `tea` (`forge_live.rs`), `systemctl` and `sudo`.

## The replay agent

Agents in terminal panes (screen detection #145, `send --wait` #147,
resuming a conversation #146) are tested against recordings of real
sessions, played back in a pane with their timing.

Recordings live in `crates/vt/fixtures/agents/*.cast`: asciicast v2, a
JSON header line and then `[seconds, kind, text]` events, where `o` is
output, `i` is what was typed, and `m` is a marker naming the state the
screen shows at that point (`working`, `blocked`, `idle`).
`record.py claude_turn` makes one from the real `claude` (it needs
`pip install pyte`): a pty at 80x24, a new scratch git repo under `/tmp`,
none of your settings or hooks (`--setting-sources project`), an
`illogical` on `PATH` that does nothing, typing each step once the screen
shows what it waits for. It replaces `$HOME`, your user name and emails,
and removes the conversation it left in your Claude Code. Read the result
before checking it in. Codex isn't installed where these were made, so
`record.py codex_turn` draws Codex from the captured screens in
`fixtures/screens/` and says so in its header.

`replay.py` plays one. A test installs it with `Replay::install(dir,
"claude", "claude_turn")` (`crates/daemon/tests/replay/`), which copies it
to `dir/claude` with the recording beside it as `dir/claude.cast`, so the
daemon sees a program named `claude` and reads its screen as Claude
Code's (`$ILLOGICAL_REPLAY` names another recording). Where the recording
has input it waits for the same last key (Enter, Ctrl-O, an arrow), so the
test drives it with `send` and `keys` as a person would. Each marker it
reaches goes to `dir/claude.log`, and `Replay::reached("m blocked", 2)`
waits for one rather than sleeping. `$ILLOGICAL_REPLAY_SPEED` plays it
faster, and `$ILLOGICAL_REPLAY_PAUSE=working=6` stops six silent seconds at
the first `working` marker (a long think).

It is also the stub `claude` for #146. Every start appends its argv,
working directory and pane to `dir/claude.argv` (`Replay::starts()`), so a
test sees `["--resume", "<id>"]` arrive in the right pane. With
`$ILLOGICAL_REPLAY_CONFIG` set to the test's own `CLAUDE_CONFIG_DIR`, it
keeps Claude Code's records of a conversation there:
`sessions/<pid>.json` while it runs, and a transcript under `projects/`.
The id is `--resume`'s, else `$ILLOGICAL_REPLAY_SESSION`, else a new one,
and `--resume` with no transcript fails as Claude Code does. It never
writes to the real `CLAUDE_CONFIG_DIR`.

Where they're used: `crates/vt/src/detect/tests.rs` plays each recording
through the terminal and checks every marker; `agent_screens.rs` runs them
in a live pane with no hooks; `prompt.rs` and `mcp.rs` prompt the replayed
Claude Code and wait; `resume.rs` stops and starts the daemon the way a
reboot does and checks each pane comes back in its own conversation, a
deleted transcript comes back as a shell that says so, and a session id
with shell metacharacters is never run.

The same against the real Claude Code is the `screen` entry of
`agents_real.rs` (`ILLOGICAL_REAL_AGENTS=screen`): no hooks, the approval
read off the screen, a prompt waited through, and a restart that resumes
the conversation. With `ANTHROPIC_API_KEY` set (CI's secret) it uses a
`CLAUDE_CONFIG_DIR` of its own; without, your login. It skips unless
asked.

## Fixtures

Recorded from real systems and checked in, so tests see real shapes:

| Where | What | Re-recording |
|---|---|---|
| `crates/vt/fixtures/` | raw PTY output of scripted sessions (`.bin`) and their sizes and resizes (`.json`) | `just fixtures [names]` (`record.py`) |
| `crates/vt/fixtures/screens/` | Claude Code and Codex screens, the title on the first line | by hand, from a real terminal or a recording |
| `crates/vt/fixtures/agents/` | Claude Code and Codex sessions with their timing, for the replay agent | `crates/vt/fixtures/agents/record.py NAME` |
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
| `agents_real.rs`, `swarm-real.spec.ts` | `ILLOGICAL_REAL_AGENTS=claude,codex,screen,...` (real agents; costs a few cents); `screen` uses `ANTHROPIC_API_KEY` when it's set |
| `resident.rs`, `resident.spec.ts`, `editors-vm.spec.ts` | a wispd token and `just static` |
| `sandbox.spec.ts` (`just e2e-sandbox`) | `ILLOGICAL_E2E_TAILNET_AUTHKEY_FILE` and wispd |
| `workspace.spec.ts` | network on its first run, to install the pinned chant |

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
