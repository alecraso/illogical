# Testing

Every test runs the real binaries as child processes, each with its own
temp state directory and a port the OS picks. Anything outside Arugula
(GitHub, Fountain, an agent, Stripe) is a fake served by the test or a
small script, or a response recorded from the real service and checked in.
Nothing in the default run costs money, and no test waits for a person:
what still needs one is listed under [By hand](#by-hand).

```sh
just test        # Rust tests, the web typecheck, e2e-interop, control-smoke
just check       # just test, plus rustfmt and clippy (what CI runs on Linux)
just e2e         # the browser tests, in the system Chrome and WebKit
```

A test that can't run without its infrastructure fails, saying what to
run. Missing Docker fails ([Tests that need Docker](#tests-that-need-docker)),
and so do missing tart and its base VM ([the tart VM](#a-fresh-mac-the-tart-vm-harness)).
Only `ARUGULA_SKIP_DOCKER=1` and `ARUGULA_SKIP_MACOS_VM=1` skip them,
and they print that nothing ran; CI sets neither. Tests that need a secret
or an account skip without it and name what's missing
([Tests that skip without a secret](#tests-that-skip-without-a-secret)).

## What runs where

| Command | What it runs | In CI |
|---|---|---|
| `cargo nextest run --workspace` (in `just test`) | unit tests in every crate, and the daemon's integration tests in `crates/daemon/tests/`, `ssh.rs` and `reboot.rs` among them (Docker) | Linux and macOS |
| `just e2e-interop` (in `just test`) | the browser's end-to-end crypto (`web/src/e2e`) against Rust's (`crates/e2e`): certificate vectors made by `crates/e2e/examples/interop.rs`, and a Noise handshake with its `responder` | Linux and macOS |
| `just control-smoke` (in `just test`) | `web/control-smoke.ts`: a fake GitHub, Stripe, push service and Sprites API, the real `arugula-control` and real daemons; headless devices sign in, enroll, approve the daemons' join codes and reach them directly and through the relay; the CLI (M49) logs in with a code the device approves and, with no daemon of its own, runs, lists and captures on one machine directly and one through the relay | Linux and macOS |
| `just e2e` | the Playwright specs in `web/e2e/` against throwaway daemons (`just e2e <url>` tests a running one): the `chrome` project, and `webkit` for `*.webkit.spec.ts` | Linux (Playwright's Chromium and WebKit) |
| `just e2e-webkit` | only the `webkit` project: Safari's engine, for device keys (#94) and the one-click invite (#137) | macOS |
| `just testnet up`, `test`, `break` (`ssh`, then `control`) | the Docker test stack's claims ([testnet/README.md](../testnet/README.md)), then each claim under `BREAK=1`, where it must fail | Linux |
| `just forges`, `just testnet-hosts`, `just testnet-editors` | real forges, two hosts and VS Code over Remote-SSH, in Docker ([below](#real-forges-two-hosts-vs-code-over-remote-ssh)) | the forges nightly (`forges-nightly.yml`) |
| `just macos <test>` | the tart VM's checks: launchd with no GUI session, real Safari, iTerm2, the desktop app ([below](#a-fresh-mac-the-tart-vm-harness)) | no |
| `just desktop-check` | rustfmt and clippy for `crates/desktop` | Linux |
| `just desktop-xvfb` | the Linux desktop app under Xvfb in a container (`packaging/desktop/xvfb/`): `join` (#204), `m46`, `m47` and `stale` (#317) (see [The desktop app's tests](#the-desktop-apps-tests)) | no |
| `just desktop-packages ARCH` | the .deb on Ubuntu 22.04 and the .rpm on Fedora 42 install and claim `arugula://` (after `just desktop-linux ARCH`) | no |
| `testnet/macos/desktop.sh`, `testnet/macos/update.sh`, `testnet/macos/stale-daemon.sh` | the macOS app from its .dmg in a fresh tart VM, its updater, and a daemon too old for it (#317) | no |
| `just check-macos` | clippy for the macOS target from Linux (compiles, doesn't link) | Linux |

CI (`.github/workflows/check.yml`) runs on pushes, every job it can at
once (#287); a new push to a branch cancels that branch's run:
- **lint** (GitHub's runners): rustfmt, shellcheck, `just test-scripts`
  and gitleaks.
- On geek's pool (eight runners, `linux-x86_64-ci`):
  - **build** and **static**: the debug binaries (with `--features
    debug-embed`, so they carry `web/dist`) and the box's static ones,
    once, kept for the run's other jobs (`scripts/ci-env keep`);
  - **clippy**: Linux, both Macs (`just check-macos`), the desktop app
    and `just notices`;
  - **test**: `just test`, the box's binaries from static;
  - **testnet**: the test stack's ssh and control profiles;
  - **e2e**: the specs (`E2E_SET=rest`) in six `--shard`s, each with its
    own test daemon on a free port; **stack**: those on the test stack
    (`E2E_SET=stack`); **perf**: the specs that time things (`E2E_SET=perf`:
    the frame rates, `editors.spec.ts`), after the shards, which leave geek
    too busy to measure them.
  Each job takes one of two build directories kept for its kind of job,
  and a test stack with its own name, ports and subnet (`scripts/ci-env`),
  so jobs and runs on geek don't share either.
  Pushes to branches other than main skip testnet, stack and perf (geek
  runs several branches at once, and their load failed the tests that
  time things); `gh workflow run check.yml --ref BRANCH` runs them all.
- **macos** (GitHub's `macos-15`, Apple silicon, free for a public repo):
  `just test` and `just e2e-webkit`, with `ARUGULA_SKIP_DOCKER=1` (no
  Docker there; geek's test runs the ssh tests).

The Rust tests run under cargo-nextest (`.config/nextest.toml`): each in
its own process and all at once, `ssh.rs` and `reboot.rs` one at a time
(the test stack). CI's profile (`NEXTEST_PROFILE=ci`) retries a failure
once and reports a test that passes the second time as flaky.

geek needs Docker (the testnet, and `ssh.rs` in `just test`): without it
the run fails. To run one shard's specs locally: `E2E_SET=rest
pnpm exec playwright test --shard=2/6` in `web/`. `just browsers` installs Playwright's browsers (with their
system libraries on Linux, if sudo needs no password; otherwise run `sudo
pnpm exec playwright install-deps` in `web/` once). See
[development.md](development.md) for the runners.

Not tests, but useful while working: `just dev` (a separate daemon on 7682
and Vite on 5173), `just fake-fleet` (three throwaway daemons with
scripted work on 7730-7732, for the swarm) and `just screenshots` (the
images in `site/img/`, from a scripted session).

## The daemon's integration tests

`crates/daemon/tests/*.rs` start `arugulad` (`CARGO_BIN_EXE_arugulad`)
through `crates/testkit`, then drive it over its Unix socket and HTTP API.

```rust
use arugula_testkit::{Daemon, arugulad};

let d = arugulad!("api").env("PS1", "$ ").no_wisp().start();
let pane = d.post("/api/run", json!({"command": "exit 4"}))["pane"].as_u64().unwrap();
assert_eq!(d.get(&format!("/api/panes/{pane}/wait?until=exit&timeout=10"))["code"], 4);
d.wait_for("its history", || d.get(&format!("/api/history?pane={pane}"))[0]["exit"] == 4);
```

`arugulad!(tag)` gives a `Builder` for the test's own daemon binary. Every
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
  say). `ARUGULA_CHANT` is empty by default, so no test daemon reads the
  host's agent config with `chant audit --agents`; a test that wants an
  inventory sets it to a stand-in.
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
- `wait_for(what, f)`, and `arugula_testkit::wait_for` with a timeout of
  its own.
- `stop` (SIGTERM, as systemd stops it), `kill` (SIGKILL), `signal`, then
  `start` to bring it back on the same ports and state; `restart_service`
  and `unit` for a service.

Dropping it kills it, kills what its panes left running and removes its
state dir (#35, #68). With `ARUGULA_KEEP_TEST_STATE=1` the dir stays and
its path is printed. A file that needs more (deleting machines it made,
stopping the code-servers it started) wraps the `Daemon` in a struct of its
own with a `Drop` that does that first, as `machines.rs` and `editors.rs`
do.

Also in the crate: `listen`, which reads the port a daemon took from
`state/listen` (don't pick a free port yourself and pass it in: it can be
taken before the daemon binds it, #66); `strays`, the cleanup above; and
`Scratch`, a temp dir removed on drop. `crates/daemon/tests/integration/agentd/` builds
on it for agent block tests: a sessions dir for the fake agent, and helpers
to open blocks and wait on them.

### Waiting

Waiting on something that takes as long as the machine is busy (a flood
of output, a build): stop it or wait for its end, never sleep a fixed time
and hope, and make deadlines failure limits that are generous (tens of
seconds) rather than waits. A reader that has to keep up with a flood
(the tmux client in `tmux.rs`) does as little per line as it can and lets
its own timeouts expire while lines keep coming. `falling_behind_pauses_the_pane`
failed under a load average of 30 because a fixed 40 MB flood was still
running after `continue`; it now runs `yes` until the pane pauses, sends
^C and waits for the prompt in a capture.

The same goes for a step that has to have happened before the next one
makes sense: wait for the daemon to say it happened. Before a ^C, wait
until the pane's `current` command is the one you ran (typed isn't
running: a ^C while bash expands PS0 cancels the line with no command
end, `mcp.rs`). Before drawing an agent's next screen, wait until
`/api/panes/N/detection` shows the last one (`api.rs`). Something that
should be busy for a while runs until the test stops it, not for a fixed
time (`summaries.rs`). Two requests alike in the same millisecond carry a
nonce, or control takes the second for a replay (`forge_wire.rs`). The
first of these turned up a daemon bug: a pane read its agent's screen
only when no command had come for a whole tick, so polling more often
than that kept the screen from being read at all.

Two connections don't keep each other's order: in `team-answers.spec.ts`
the owner's trust grant goes over one and the teammate's follow-up over
another, so the spec waits for the pane's `trusted` before sending (#258).
A shell command that has to pause until the test is ready waits for a
file the test creates (`reattach.spec.ts`), not for a `sleep`: 2000 runs
of `sleep 0.0005` took 17 s on a Mac.

### Standing permission rules

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

### The fakes

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
`arugula` on `PATH` that does nothing, typing each step once the screen
shows what it waits for. It replaces `$HOME`, your user name and emails,
and removes the conversation it left in your Claude Code. Read the result
before checking it in. Codex isn't installed where these were made, so
`record.py codex_turn` draws Codex from the captured screens in
`fixtures/screens/` and says so in its header.

`replay.py` plays one. A test installs it with `Replay::install(dir,
"claude", "claude_turn")` (`crates/daemon/tests/integration/replay/`), which copies it
to `dir/claude` with the recording beside it as `dir/claude.cast`, so the
daemon sees a program named `claude` and reads its screen as Claude
Code's (`$ARUGULA_REPLAY` names another recording). Where the recording
has input it waits for the same last key (Enter, Ctrl-O, an arrow), so the
test drives it with `send` and `keys` as a person would. Each marker it
reaches goes to `dir/claude.log`, and `Replay::reached("m blocked", 2)`
waits for one rather than sleeping. `$ARUGULA_REPLAY_SPEED` plays it
faster, and `$ARUGULA_REPLAY_PAUSE=working=6` stops six silent seconds at
the first `working` marker (a long think).

It is also the stub `claude` for #146. Every start appends its argv,
working directory and pane to `dir/claude.argv` (`Replay::starts()`), so a
test sees `["--resume", "<id>"]` arrive in the right pane. With
`$ARUGULA_REPLAY_CONFIG` set to the test's own `CLAUDE_CONFIG_DIR`, it
keeps Claude Code's records of a conversation there:
`sessions/<pid>.json` while it runs, and a transcript under `projects/`.
The id is `--resume`'s, else `$ARUGULA_REPLAY_SESSION`, else a new one,
and `--resume` with no transcript fails as Claude Code does. It never
writes to the real `CLAUDE_CONFIG_DIR`.

Where they're used: `crates/vt/src/detect/tests.rs` plays each recording
through the terminal and checks every marker; `agent_screens.rs` runs them
in a live pane with no hooks, checks going quiet changes nothing for an
agent whose screen is read, and has a stand-in chant print a recorded
`chant audit --agents` document to decide whose rules run; `prompt.rs` and `mcp.rs` prompt the replayed
Claude Code and wait; `resume.rs` stops and starts the daemon the way a
reboot does and checks each pane comes back in its own conversation, a
deleted transcript comes back as a shell that says so, and a session id
with shell metacharacters is never run.

The same against the real Claude Code is the `screen` entry of
`agents_real.rs` (`ARUGULA_REAL_AGENTS=screen`): no hooks, the approval
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
| `crates/daemon/tests/fixtures/github`, `gitlab`, `forgejo` | API responses for real PRs and issues (from S23) | by hand, as in `archive/spikes:spikes/s23-forge/` |
| `crates/daemon/tests/fixtures/conversations/` | Claude Code transcripts, one per shape (S20) | by hand, as in `archive/spikes:spikes/s20-conversations/` |
| `crates/daemon/tests/fixtures/conversations/desktop/` | the Claude desktop app's Code tab session records (#81, #83), made up from the fields S20 saw | by hand |
| `crates/daemon/tests/fixtures/s13-*`, `s18-*` | Claude Code hook payloads | by hand |
| `crates/daemon/tests/fixtures/fountain/`, `chant/` | Fountain API and chant output | by hand |
| `crates/daemon/tests/fixtures/chant/audit-agents.json` | `chant audit --agents --scope system,user --format json` (chant 0.95.0), with only Claude Code configured | `chant audit --agents` with `HOME` a scratch dir holding only `.claude/CLAUDE.md` and `.claude/settings.json` |

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

`E2E_CHROMIUM=1` uses Playwright's Chromium instead of the system Chrome,
as CI does. With `CARGO_TARGET_DIR` set, `just e2e` links `target` to it,
since the specs run `../target/debug/*`.

Make temp directories in `beforeAll`, not at the top of a spec: Playwright
loads each spec in the runner as well as the worker (#62).
`E2E_DAEMON_LOG=<file>` keeps the test daemon's debug log, and
`E2E_CONTROL_LOG=1` shows control's output in `sandboxes.spec.ts`.

The whole run shares one browser, and Playwright closes only the contexts
its `context` fixture made. A context a spec opens with
`browser.newContext()` stays open, its pages still connected and drawing,
through every spec after it, so later specs ran on a busier machine than
they do alone (#258). Every spec that uses `browser` has
`test.afterAll(closeContexts)` (from `helpers.ts`); a new one should too.

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
cargo build -p arugulad && (cd web && pnpm run build)
cd web && pnpm exec playwright test e2e/palette.spec.ts e2e/palette.webkit.spec.ts
```

WebKit needs `pnpm exec playwright install webkit` once.

### Spike: blocks through control (S27)

S27 (`spikes/s27-blocks/`, now on the `archive/spikes` branch) has its own
Playwright suite for #148 (blocks served from control's block domain, carried
to the daemon over Noise by a service worker). It isn't part of `just e2e`;
check the branch out in its own worktree and run it from the spike's directory:

```sh
git worktree add ../arugula-spikes archive/spikes
cd ../arugula-spikes/spikes/s27-blocks
pnpm test                   # builds s27 and the worker, then Chromium and WebKit
pnpm typecheck
./linux-webkit.sh [specs]   # WebKit on Linux in Playwright's container (Docker, Zig)
node safari/safari.ts       # real Safari via safaridriver; --ios for the Simulator
./webkitgtk.sh              # the same in the desktop app's WebKitGTK (Ubuntu 22.04, Docker)
./netem.sh                  # the latency spec with control, box and browser 20 ms apart (Docker)
```

Hostnames (`control.test`, `*.blocks.test`) go to 127.0.0.1 through a
CONNECT proxy the suite runs on 7753; everything else takes port 0. What
each spec checks:

| Spec | What |
|---|---|
| `basics` | the worker registers in a cross-site frame; pages, POSTs, redirects, a 3 MB download and a WebSocket go through it; control relays no plaintext; cookies; refused grants |
| `vite`, `next`, `code-server` | real dev servers: a save reloads (or hot-updates) the block; VS Code's workbench, a file and a webview |
| `latency` | per-request time, a parallel burst, a 10 MB download and first load, relayed, worker-direct and through the daemon's own block site |
| `lifecycle` | reloads (hard, in Chromium), a stopped worker, cleared storage, idle, grants expiring under an open block, control restarting, a skewed parent clock |

Measurements are appended to `.run/results.jsonl` with the load average.
`safari/safari.ts --print-setup` lists what a Mac needs first (sudo:
`safaridriver --enable`, the test certificate trusted, `/etc/hosts`).
Running it unattended in the tart VM isn't done yet ([Planned](#planned)).
`--driver playwright-webkit` checks the script itself without Safari.

## A device that approves things

Anything that waits for a person to approve it on a signed-in device (a
daemon's `arugulad join`, a second browser, a CLI) is approved in tests by
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

`arugulad join --account <fingerprint>` (and `arugula join
--account`, and `arugula login --account`) takes the account without
asking; the fingerprint is what `signin` printed. `just control-smoke` and
the testnet's `control` claims use both. `approve` takes a CLI's code from
`arugula login` as well as a daemon's.

## The desktop app's tests

M46's and M47's promises, each checked with nobody at the keyboard. Linux runs in a
container under Xvfb and Openbox with xdotool typing; macOS runs in a fresh
tart VM clone (`testnet/macos/vm.sh`) where System Events types into the
logged-in session over ssh.

| Claim | Linux (`just desktop-xvfb m46 CLAIM`) | macOS (`testnet/macos/desktop.sh CLAIM`) | A failure means |
|---|---|---|---|
| Chords reach the page | `keys`: Ctrl-W, T, N, Q, Tab, F1, F10, Alt-x arrive in a recording pane as bytes | `keys`: Ctrl-W, T, N, Q, Tab as bytes; Cmd-W closes the pane and not the window; Cmd-T opens a tab; Cmd-M leaves the app up; Cmd-H hides it and Cmd-Q quits it, the panes still running | a menu or the toolkit took a key the terminal needs |
| Tabs in the titlebar | `titlebar`: no decorations; dragging the bar moves the window; the bar's maximize and minimize work | `tabs`: Cmd-N opens a window as a native tab; with two and three tabs the page's bar starts below AppKit's tab bar, and Cmd-Shift-W back to one tab hides the strip (#323). `this`: the tray's *This machine*, clicked three times, adds no window or tab | the window can't be moved or managed without the system's titlebar |
| App-supplied dragging (#316) | `bare`: a plain page with no drag markup at `ARUGULA_URL` moves by its top strip, not below it, and a double-click on the strip maximizes | `drag` (drag.js, CGEvents): the page's bar moves the window as before; a plain page with no drag markup moves by its top strip, not below it, and a double-click there zooms | a page from an older daemon leaves the window stuck |
| `arugula://` | `links`: a second launch with `arugula://open?cwd=DIR` opens a tab in DIR in the running app and shows it; `arugula://pane/%N` shows N | `links`: the same through `open URL` (the URL scheme in Info.plist) | links start a second app, or open nothing |
| Global hotkey | `hotkey`: off by default; on, Ctrl+Alt+Space hides the focused window and brings it back | `hotkey`: the same with Ctrl-Option-Space | |
| Service registration | (the systemd unit: `arugulad install`, unchanged) | `agent`: the first start registers the launch agent through SMAppService, BTM lists it, the bundle's daemon answers the linked CLI, no second plist | the app runs a daemon that isn't the one Login Items shows, or two |
| Working pane, no terminal | | `install`, `pane`: the .dmg installs, and a command typed into the window runs | |
| Panes outlive the app | | `restart`: the daemon's pids and panes are the same after the app restarts | |
| Right-click a folder (M47) | `m47` (`just desktop-xvfb m47`): with the packages' .desktop file and Nautilus extension, a right-click on a folder (and inside one) in Nautilus, *Open in Arugula*, opens a tab there in the running app and shows it | `finder`: the service is registered (`pbs`); a right-click on a folder in Finder, *New Arugula Tab Here*, opens a tab there; a `.command` file opened with the app runs in a new pane | the file manager's menu has no entry, or it starts a second app |
| Packages | `just desktop-packages ARCH`: .deb and .rpm install, libraries resolve, xdg-mime hands `arugula://` to the app, the .desktop file passes the link (`%u`) and has a New Tab action, the Nautilus extension is in place | `install` above | |
| A daemon too old for the app (#317) | `just desktop-xvfb stale`: a 0.8.0 stand-in (`old-daemon.py`, no protocol) installed as the service, stopped (`stopped`) or running (`running`): the app says 0.8.0 runs and 0.19.0 is needed, 0.8.0 still answers afterwards, and it served no page | `stale-daemon.sh`: the same with the released 0.8.0 (`OLD=` another below 0.19.0), installed with its own `arugulad install` | the app shows an old daemon's page, or replaces a daemon (#392) |
| Updates | | `update.sh`: 0.17.0 refuses a manifest signed with another key, then replaces itself with 0.17.1 and restarts; the new app carries a newer daemon and leaves the running one alone (#392); a running vim and a counting build carry on | |

`update.sh` makes a throwaway updater key and builds the app twice with
it. The installed one carries a daemon one minor version older than the
tree's (built from a copy of the tree with only the version changed, kept
in `target/update-old`), so the new app carries a newer daemon than the
one running and must leave it alone (#392): the same process, at the same
version, with its panes.

Nautilus and Finder are driven as a person would: the mouse right-clicks
the folder (xdotool at the spot AT-SPI reports, under Xvfb; a CGEvent at
the row's position in the VM, since System Events' `AXShowMenu` opens no
menu on a Finder row), and the menu item is clicked through AT-SPI or
System Events. The VM sends Finder no Apple Events: those need a person
to allow them. The Xvfb image links `/usr/lib/libpython3.10.so.1.0`
because 22.04's arm64 `python3-nautilus` looks for it there (amd64's
looks in the right place).

The macOS app is copied in without
the quarantine flag: an ad-hoc signed download needs a person's
right-click > Open past Gatekeeper, which only a notarized build (#177)
removes.

## Tests that need something extra

### M49: the CLI through control

Three tests cover M49, from fastest to most faithful:

- `just control-smoke` (in `just test`, so in CI): `arugula login` on
  loopback, approved by the headless device, then `--host box` (direct, it
  has `--direct-url`) and `--host box2` (no URL, so relayed) each `run`,
  `ls` and `capture`, with the CLI's `ARUGULA_SOCK` pointing at no
  daemon. On both, `events --follow` and `tail --follow` must print while
  they're still running (#254: a streamed answer, in parts), and `attach`
  with a pipe for stdin types a command, sees its answer and detaches on
  Ctrl-]. `arugula-control`'s own `routing_wire` test checks the join
  and the signed requests (`cargo test -p arugula-control the_cli_joins`).
- `web/e2e/host-menu-control.spec.ts` (`just e2e`): a daemon joined by
  code (approved by the device) has *All your machines…* in its host menu,
  opening control's page; one that isn't joined doesn't.
- The testnet's `m49` claim (`just testnet test control m49`; Docker):
  the CLI on the bastion, with no daemon, logs in and reaches box-bare
  directly and box-systemd through the relay; on box-systemd it also
  follows events and a pane's output, and drives `attach` and `tui` in a
  pty (`ssh -tt`, keys piped in with pauses). `m49team` reaches another
  account's machine: an owner (a second headless device, `devs FILE` in
  `testnet/test.sh`) makes a team and joins box-systemd to it, the CLI's
  account asks to join and is let in, and the CLI lists the box as the
  owner's, captures its pane and attaches to it as a team editor. Under
  `BREAK=1` the CLI has pinned another root for the owner's account, so the
  box is refused. Both are in the
  [SSH track's table](#the-ssh-tracks-tests) and
  [testnet/README.md](../testnet/README.md#claims). Beside another
  worktree's stack: `COMPOSE_PROJECT_NAME=illo-j
  ARUGULA_TESTNET_INNER_NET=10.229.85 ARUGULA_TESTNET_SSH_PORT=22955
  ARUGULA_TESTNET_CONTROL_PORT=22985 ARUGULA_TESTNET_FAKES_PORT=22986`.

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
| M26, M30 | `team-swarm-phones.spec.ts` | two teammates' swarms on a Pixel 7 (Chrome's network emulation, 150 ms, 1.6 Mbit/s) and an iPhone: grouped by person, cards along the bottom, a rerun from the iPhone's card seen on the Pixel, a tile tap opening the pane; and a machine on another network behind netem (Docker, [below](#tests-that-need-docker)) |
| #86 | `studio-phone.spec.ts` | a studio app from the template through the token API (a fake studio and box), `arugula studio login`, `hud share --role follower`, `arugula studio follower`, `arugula app`; a question answered from a Pixel 7 and a gate approved from an iPhone, with hud told who |

Run one with `cd web && E2E_PORT=<port> pnpm exec playwright test
e2e/<spec>`; `editors.spec.ts` needs code-server, which its first test
downloads.

## Tests that need Docker

Docker is required for these. Without it they fail, saying what to run;
they don't skip. Only `ARUGULA_SKIP_DOCKER=1` skips them, and each then
prints that it did not run. CI never sets it. The ones that use the test
stack recreate the boxes they use, so give each worktree its own stack
(`COMPOSE_PROJECT_NAME` and the `ARUGULA_TESTNET_*` ports,
[testnet/README.md](../testnet/README.md#conventions)).

| Test | Also needs | Run |
|---|---|---|
| the testnet's claims | `ssh`: nothing more. `control`: node, and the static binaries (`just static aarch64` on Apple silicon, `just static` on x86_64) | `just testnet up ssh` (or `control`), `just testnet test ssh`, `just testnet break ssh`, `just testnet down` |
| `crates/daemon/tests/integration/ssh.rs`, `reboot.rs` (in `just test`) | the box's static binaries (or `ARUGULA_SSH_BINARIES`); `reboot.rs` also node and Playwright's Chromium in `web/` (`cd web && pnpm install`) | `cargo test -p arugulad --test integration ssh::` (or `reboot::`); they bring the `ssh` profile up themselves |
| `testnet/measure-tailnet.sh` (S28) | the static binaries | `just testnet measure tailnet`; it brings the `tailnet` profile up itself |
| `team-swarm-phones.spec.ts`, "a machine on another network, behind netem" (in `just e2e`) | `just static <arch>`; it builds a small Debian image with `tc` and `socat`, names its container and network after `COMPOSE_PROJECT_NAME`, and removes them after | `just e2e` |
| `just desktop-xvfb` | podman or Docker | `just desktop-xvfb` |
| forges, two hosts, VS Code over Remote-SSH | see [below](#real-forges-two-hosts-vs-code-over-remote-ssh) | their own recipes |

What the stack-based tests guard is in [The SSH track's
tests](#the-ssh-tracks-tests).

### The test stack

[`testnet/`](../testnet/README.md) is a Docker Compose stack, one profile per
network shape, for what one host's loopback can't show: `ssh` (a bastion,
a bare box, a systemd box and a git server), `control` (`ssh`'s, plus
control and its fakes) and `tailnet` (headscale and two Tailscale nodes).
Its README has each container, the claims each profile checks (and how
`BREAK=1` breaks them), and how to run two stacks side by side.

### Real forges, two hosts, VS Code over Remote-SSH

These aren't part of `just check`: each has its own recipe.

| What | Run | Details |
|---|---|---|
| Forgejo and GitLab CE with two bot users and webhooks (#93, M36-M40) | `just forges up forgejo && just forges test forgejo`, the same with `gitlab` (3-5 minutes and 4 GB to start), `just forges down` | [testnet/forges/README.md](../testnet/forges/README.md) |
| #17 on two machines: home's layout holds panes on `mac`, which drops off the network (`docker network disconnect`) and comes back | `just testnet-hosts` | [testnet/hosts/README.md](../testnet/hosts/README.md) |
| M28 in real VS Code (downloaded by `@vscode/test-electron`) over Microsoft's Remote-SSH into a box running arugulad: a phone follows the cursor, a breakpoint is a card it continues, an edit is accepted from its rail | `just testnet-editors` (downloads VS Code, its server and Remote-SSH; on Linux it runs under `xvfb-run`) | [testnet/editors/README.md](../testnet/editors/README.md) |

The forge tests are `crates/daemon/tests/integration/forges_real.rs`, marked
`#[ignore]` so `cargo test` doesn't need the containers;
`testnet/forges/test.sh` runs them with `--ignored` against the stack
`up.sh` started, and they fail if it isn't there. The other two are
Playwright specs (`web/e2e/testnet-hosts.spec.ts`,
`web/e2e/editor-remote-ssh.spec.ts`) that bring their stack up and down
themselves. A plain `just e2e` lists them as skipped, since they belong to
their own recipes, which set `ARUGULA_TESTNET_HOSTS=1` or
`ARUGULA_TESTNET_EDITORS=1`; with that set and no Docker they fail.

#### The nightly job against github.com

`.github/workflows/forges-nightly.yml` runs every night and on demand
(never on pull requests): the Forgejo and GitLab tests above, and
`crates/daemon/tests/integration/forges_github_real.rs` against github.com, which
covers #93's GitHub boxes (a review approved from the rail, a red Actions
check rerun, a box with no `gh` login reading through the App's
installation token and refusing writes, and the App's webhook poking the
block). Without its secrets the GitHub job passes with a notice naming
each one that's missing, and each test prints `SKIP <test>: not set: ...`.
It needs, in the repository's Actions settings:

| Name | Kind | What |
|---|---|---|
| `ARUGULA_GH_TEST_REPO` | variable | `org/repo` in a test organization: public, both bots can write, with `.github/workflows/arugula-red.yml` on its default branch (a job that fails on pushes to `red-*`) |
| `ARUGULA_GH_AUTHOR_TOKEN` | secret | the first bot's token: contents, pull requests, issues and actions, read and write, on that repository |
| `ARUGULA_GH_REVIEWER_TOKEN` | secret | the second bot's token, the same |
| `ARUGULA_GH_APP_ID` | variable | a test copy of Arugula's GitHub App, installed on the test organization, with pull request and issue comment events |
| `ARUGULA_GH_APP_PRIVATE_KEY` | secret | that App's private key (PEM) |

Control is stood in for in that test, and the App's deliveries are read
back through GitHub's API (a runner has no public URL), so control
checking GitHub's signature on a real delivery is not covered there.

## The SSH track's tests

S28 (#153), M51 (#154), M52 (#155) and M65 (#198) each promise something
about reaching a machine over ssh (plan-archive.md, "SSH track" and M65). The
tests below guard those promises, so when one fails, the table says which
promise broke.

The promises:

1. A box you can ssh into needs nothing set up first: the first `arugula
   --ssh box` command installs Arugula there and starts its daemon.
2. The daemon outlives the ssh login: a lingering systemd user service on
   Linux, and on macOS a launchd service for a user with no GUI session.
3. Only the owner's forwarded agent reaches panes, so `git push` from a
   pane works while the owner is attached with their agent, and not
   without it.
4. ssh is enough to join control (M52). Afterwards control reaches the box
   with ssh closed, and again after the box reboots, including from a
   phone.
5. A box that can't reach control is told so clearly, and stays reachable
   over `--ssh`.
6. A guest with only OpenSSH can watch or drive one pane, and nothing else
   (M65).

### What the stack stands for

Each container in [`testnet/`](../testnet/README.md#profiles) stands in for
a machine in those promises:

- `bastion`: a ProxyJump host in front of a private network. It's the only
  container the host can reach (`127.0.0.1:22922`).
- `box-bare`: a fresh machine. No Arugula, no state, no route out; it's
  reached only through the bastion, so anything installed on it arrived
  over ssh.
- `box-systemd`: a Linux server with systemd as PID 1, logind and polkit,
  for lingering. `docker restart` is its reboot: systemd shuts down
  cleanly and boots again, and its journal is kept.
- `git`: a forge's ssh git server, bare repositories behind `git-shell`.
  It knows only the client's key, and the boxes have no key of their own,
  so a push from a box can only have used a forwarded agent.
- `control` and `fakes` (the `control` profile): `arugula-control` with
  its relay, at a private address on the inner network that the boxes
  reach by dialing out, and fake GitHub, Stripe and Web Push.
  `web/fixtures/device-cli.ts` on the host is the person's phone or
  browser ([A device that approves things](#a-device-that-approves-things)),
  and `web/fixtures/m52-phones.ts` drives real phone pages (a Pixel 7 in
  Chrome, an iPhone in WebKit) against it.
- `headscale`, `ts-box` and `ts-client` (the `tailnet` profile): the
  tailnet path that S28 compares ssh against.

launchd needs a Mac, so it runs in the [tart VM](#a-fresh-mac-the-tart-vm-harness):
a user made with `sysadminctl` who has never logged in to the GUI and is
reached only over ssh.

### What each test guards

| Test | Closes | What it does | A failure means |
|---|---|---|---|
| `just testnet test ssh` (`login`, `jump`, `inner`, `bare`, `stdio`, `agent`, `push`, `linger`) | the ground under #153 (S28) and #154 (M51) | checks the stack is the shape the other tests assume: the key and host keys work, ProxyJump works, box-bare has no route out and no Arugula, 1 MiB of random bytes cross ssh's stdio unchanged, a forwarded agent shows on the box, `git push` works with only the forwarded agent, and a user turns on lingering with no sudo | the environment changed, not arugula: read it before any other failure. `bare` fails on a box an earlier run installed on (`just testnet down` and `up`); `stdio` means the transport S28's bridge rides on isn't clean; `linger` means no Linux box could keep a daemon past logout without sudo |
| `crates/daemon/tests/integration/ssh.rs` | #154 (M51): "an e2e test drives it against a local sshd"; `git push` uses the client's agent | on a recreated box-bare: the first `--ssh box-bare ls` installs and starts the daemon; `run` and `capture` a pane; the client's key shows in a pane while a client is attached; a `git push` from a pane reaches the git server; the same push with `ARUGULA_SSH_AGENT=no` is refused; the pane outlives the connection; a saved `ssh://box-bare` host works with `--host`, and an option as a destination (`ssh://-oProxyCommand=id`) is refused | promise 1 (no install or no daemon on a fresh box), promise 3 (the push failed with the agent, or worked without it, so panes see some other agent or none), promise 2 (the pane went with the connection), or the host list |
| `crates/daemon/tests/integration/reboot.rs` | #26; M52's "the box survives a reboot" on Linux | installs over `--ssh` as a lingering user service, builds #26's session (splits, a nested directory, coloured output, every restart policy, a browser block, an agent block), then `docker restart` twice with nobody logged in; checks the daemon is up, the journal's "saved for shutdown" and "restored", layout, directories, scrollback with `── restored`, each pane by its policy, both blocks, and a headless web client (`web/reconnect-watch.ts`) reconnecting without a reload | promise 2 on Linux if the daemon isn't up after the restart (lingering, the user service); otherwise a restore regressed (#26), named by the assertion |
| `just testnet test control signin reach` | the ground under #155 (M52) | a device signs in with the fake GitHub and is trusted; box-systemd reaches control at its inner address | the stack, not M52: the fakes or the inner network |
| `just testnet test control m52` | #155 (M52): one step plus the approval, the pane opens from the phone, ssh out of the picture, the box survives a reboot | on a fresh box-systemd, `arugula --ssh box-systemd join` installs and starts the daemon and prints a code; the device approves it; the box is on the device list and online; with the CLI's ssh master closed and the bastion paused, a marker round-trips through a pane over the relay. Then `web/fixtures/m52-phones.ts`: a Pixel 7 (Chrome) and an iPhone (WebKit) sign in to the stack's control, the device approves each, and each opens the box's pane, taps it and types a marker, which the device reads back from the box's pane over the relay; the script `docker restart`s the box, and the same open pages reach it again by themselves and type a second marker; after that the device's own marker round-trips too | promise 4: the join over ssh, the approval, the relay with ssh gone, a phone's page reaching the box through control, or coming back after a reboot (which also rests on promise 2). `m52-phones:` on stderr names the phone and the step |
| `just testnet test control unreachable` | #155 (M52): "a box that can't reach control says so and stays reachable over `--ssh`" | box-bare, with no route out, joins the hosted control; the output must name the box and control and give `arugula --ssh box-bare tui`, and `--ssh box-bare ls` still works | promise 5 |
| `just testnet test control m49` | #149 (M49), #254 | box-systemd and box-bare join over ssh; the CLI on the bastion, with no daemon, logs in with a code the device approves, lists both from control, and runs, lists and captures on box-bare directly and box-systemd through the relay; on box-systemd, `events --follow` and `tail --follow` print while running, and `attach` and `tui` in a pty type a command, see the answer and leave with Ctrl-] ([M49](#m49-the-cli-through-control)) | the CLI through control (login, `hosts`, direct or relayed routing, streamed answers, `/ws` over the channel); the joins it starts with are promise 4 |
| `just testnet test control m49team` | #254: a team machine listed and reachable from `arugula hosts` | an owner's team with box-systemd in it; the CLI's account asks to join and is admitted; `hosts` lists box-systemd as the owner's, and the CLI captures its pane and attaches to it (relayed) | pinning and checking another account's root, or a team member's CLI device not taken by the team's machine |
| `crates/daemon/tests/integration/guest_ssh.rs`, `web/e2e/guest-ssh.spec.ts` | #198 (M65): the direct path and the pane menu entry | the system OpenSSH client as a guest against a dev daemon ([below](#guest-ssh-m65)) | promise 6; the test's name says which part (read-only, read-write, ending a session, refusals, the CLI) |
| `guest_ssh.rs`'s `a_guest_reaches_a_daemon_behind_nat_through_controls_jump_host` | #253 (M65's relay) | box-systemd joined to the `control` profile's control, a guest on the host through control's jump host | promise 6 for a box behind NAT, and control can't read the pane |
| `testnet/measure-tailnet.sh` (`just testnet measure tailnet`) | #153 (S28): the tailnet comparison | installs Arugula on ts-box over ssh from ts-client, checks both paths see the same panes, then times `arugula ls` and an 8 MiB `arugula export` over `--ssh` and over the tailnet | one path no longer reaches the daemon, or the two disagree about its panes. Slower numbers don't fail it: compare them with `spikes/s28-ssh/README.md` |
| `just macos launchd` | #153 (S28) and #155 (M52): jake-mini with no GUI session | in a fresh macOS VM, a user who never had a GUI session runs `arugulad install` over ssh: it installs the background agent, warns that it won't start after a reboot by itself, and the daemon and pane outlive the ssh session; `arugula --ssh` from the host starts it and passes the warning on; `install --system` survives a VM restart with nobody logged in, its pane restored; `uninstall` leaves nothing of either ([its claims](#the-tests)) | promise 2 on macOS: the install a Mac reached only over ssh gets, and what it says about reboots |

### What BREAK=1 proves

Every testnet claim, and `just macos launchd`, has a `BREAK=1` form that
breaks the one property the claim is about (each claim's breakage is in
[testnet/README.md](../testnet/README.md#claims)). `just testnet break
ssh` and `just testnet break control` run every claim that way and pass
only if each one fails. That shows a pass isn't vacuous: the claim can
see the thing it checks go wrong. For the SSH track that means:

- `push` fails with `ForwardAgent=no`, so a passing push used the
  forwarded agent and nothing else on the box.
- `m52` fails with polkit masked (no lingering), at the restart, so a
  passing `m52` proves the box came back by itself rather than through
  something the test left running.
- `unreachable` fails when the box joins a control it can reach, so the
  message check doesn't match just any failed join.
- `bare` fails with a stub `arugula` on the box, so a passing `bare`
  means the tests after it really start from nothing.

`BREAK=1` doesn't prove a claim covers the whole promise, only that it can
fail. The Rust tests have no `BREAK=1` form; they carry their own negative
case instead (`ssh.rs`'s push with `ARUGULA_SSH_AGENT=no`,
`guest_ssh.rs`'s wrong token and different host key).

### Guest ssh (M65)

`crates/daemon/tests/integration/guest_ssh.rs` runs the system OpenSSH client
(`/usr/bin/ssh`, 8.5 or later for `KnownHostsCommand`) against a dev
daemon started with `--guest-ssh 127.0.0.1:0 --guest-ssh-host 127.0.0.1`.
Each guest is the command the daemon printed, run by `sh` on a
pseudo-terminal that is its controlling terminal (so a resize reaches ssh as
SIGWINCH), with `-F /dev/null -o BatchMode=yes` added so the runner's own
ssh config and agent stay out of it. They check:

- a read-only guest sees the screen and live output, its typing never
  reaches the pane, and a single-use token can't log in twice;
- a read-write guest types under its label (`/api/panes/N/drivers`),
  drives, sizes the pane (`stty size`), and holds off a second guest on the
  same reusable invite;
- revoking cuts a live guest off within a second or two, expiry ends a live
  session and refuses new logins, and closing the pane ends the session and
  its invite;
- a wrong token gets `Permission denied` with no prompt; a different host
  key in the command fails host-key verification before the token is sent
  (the invite stays unspent); `exec` is refused; the port closes once the
  last invite is gone;
- `arugula share --guest` prints a command that works, and `arugula
  guests` lists and revokes.

The relay path, `a_guest_reaches_a_daemon_behind_nat_through_controls_jump_host`,
runs against the `control` profile (Docker; `just static` first, and
`node` for the approving device). box-systemd, which has no route out but
to control and which nothing on the host can reach, joins control over ssh
and runs a pane; `arugula --ssh box-systemd share --guest` then gives a
command through control's jump host (`127.0.0.1:22982`,
`ARUGULA_TESTNET_GUEST_SSH_PORT`), and a guest on the host runs it. The
hop's `ssh` is wrapped in a `tee` both ways, which records exactly what
control's jump host carried for the session. The test checks:

- the guest sees the pane's screen and its live output, and `arugula
  guests` on the box counts them;
- what control carried starts with the daemon's `SSH-2.0-` banner and has
  neither the pane's text nor the token in it, and control's log has
  neither (nor the route): control relays ciphertext;
- a wrong route, and the same command after `guests revoke`, are refused
  at the hop, with nothing passed on to the daemon.

Run them with `cargo test -p arugulad --test integration guest_ssh::`. They skip,
saying so, if there's no `ssh` on PATH; the relay test fails without
Docker unless ARUGULA_SKIP_DOCKER=1. `web/e2e/guest-ssh.spec.ts`
covers *Invite over ssh…* in the pane menu, on desktop and phone viewports,
with the same system ssh (`pnpm exec playwright test e2e/guest-ssh.spec.ts`
in `web/`, after `cargo build -p arugulad` and `pnpm run build`).

### Not covered here

- **Tailscale SSH's check mode** (S28): it sends the user to an identity
  provider's login, so no test can answer whether its prompt shows in the
  client's terminal. It's in [By hand](#by-hand).
- **M53**, the desktop app over ssh: gated (plan-archive.md), no tests.

## A fresh Mac: the tart VM harness

macOS checks that need a whole Mac (a user who never logged in to the GUI,
real Safari, iTerm2, the desktop app) run in a throwaway macOS VM made with
[tart](https://tart.run), driven over ssh. No person and no window on the
host: everything with a GUI happens inside the VM, whose image logs `admin`
in to its own GUI session at boot. The scripts are in `testnet/macos/`.

```sh
just macos base              # make the base VM, once (about 30 GB)
just macos launchd           # launchd with no GUI session (S28, M52)
just macos safari            # web/safari against real Safari (#94, #137)
just macos iterm2            # M5 (tmux -CC) and M32 (OSC 52) in iTerm2
just macos app               # the desktop app in cloud mode (#178)
just macos up | ssh CMD | down   # the VM by hand
```

`just macos <test>` builds the debug binaries first, then runs
`testnet/macos/test.sh <test>`. Each test clones a fresh VM, runs, and
deletes the clone (`KEEP=1` leaves it running for a look). `BREAK=1` breaks
what each check is about, and every check must then fail, as in the
testnet's claims.

Without tart, or without the base VM, every script fails and says what to
run; a test that didn't run isn't a pass. Only `ARUGULA_SKIP_MACOS_VM=1`
skips, and it prints that no VM test ran. They need an Apple silicon Mac,
about 35 GB free, and the network for the image, iTerm2 and the app's zip.

### Setup

- **tart.** `brew install cirruslabs/cli/tart`, or, while that tap's
  formula fails on current Homebrew, `tart.tar.gz` from its GitHub release
  (`tart.app` into `~/Applications`, `tart` on `PATH`).
- **The base image.** `just macos base` (`vm.sh base`) makes a local VM
  `arugula-macos-base` from `ghcr.io/cirruslabs/macos-tahoe-base:latest`
  (macOS 26.6, Safari, the Command Line Tools, no Xcode;
  `ARUGULA_MACOS_IMAGE` picks another), then empties tart's OCI cache
  (`tart prune --entries=caches`), so the disk holds one copy, about 30
  GB, not two. The base is never booted.
- **Clones.** Every test VM is an APFS clone of the base (`tart clone`,
  nearly free on disk), booted headless (`tart run --no-graphics`). `up`
  puts the harness key (`testnet/macos/.state/`, ignored by git) into
  admin's `authorized_keys` through the tart guest agent; after that it's
  plain ssh as `admin` (whose password is `admin`, with passwordless sudo).
  `down` deletes the clone. Keep it to the base plus one running clone:
  macOS allows two VMs per host, and each clone grows as it's used.

### The tests

| Test | Checks | How |
|---|---|---|
| `launchd` (`install`, `warning`, `logout`, `uninstall-agent`, `ssh`, `system`, `reboot`, `uninstall`) | A user made with `sysadminctl`, who never had a GUI session and is reached only over ssh, runs `arugulad install`. With no GUI session that's the background agent (with one it's the usual GUI LaunchAgent, which this VM's `admin` has and `illo` never does): it installs, warns that the daemon won't start after a reboot by itself, and the daemon and a pane outlive the ssh session. `arugulad uninstall` leaves nothing behind. `arugula --ssh illo@vm ls` from the host starts the daemon there and passes the warning through. `arugulad install --system` switches to a LaunchDaemon cleanly; after a clean shutdown and `tart run`, with nobody logged in as them, the daemon is back with its pane's output; `arugulad uninstall` removes the LaunchDaemon too. | One VM, in that order. "Nothing behind" means no plist in `~/Library/LaunchAgents` or `/Library/LaunchDaemons`, no `arugulad` service in `gui/UID`, `user/UID` or `system`, and no `arugulad` process for the user. The user gets passwordless sudo before `system`, as an admin would have. `BREAK=1` boots the service out before install, logout, system and reboot, drops the `note:` line before warning, installs again after each uninstall, and has the daemon already running when the ssh check would start it. |
| `safari` | `/key-probe.html` puts its verdict in the DOM (`data-verdict` on `#verdict`: `keys`, `wrapped` or `none`, and JSON in `#result`), and it's `keys` or `wrapped`. A signed-out invitee opens a presigned invite, signs in through GitHub and joins in one click; the owner's Chrome sees them in the roster. | `web/safari/safari.spec.ts` with a small WebDriver client (`web/safari/webdriver.ts`). safaridriver runs in the VM (`sudo safaridriver --enable` once); its port comes to the host over ssh, and control and the fake GitHub, run on the host, are forwarded to the same ports on the VM's loopback. `SAFARIDRIVER_URL` alone runs the spec against any safaridriver. |
| `iterm2` (`attach`, `type`, `output`, `split`, `tab`, `osc52`) | iTerm2 runs `arugula tmux -CC` and opens a native window for the daemon's tab; text written there runs in the pane; the pane's output shows in iTerm2; a split in iTerm2 adds a pane; a daemon tab becomes an iTerm2 tab. `arugula tui` in iTerm2 copies a line in copy mode, and `pbpaste` has it. | iTerm2's latest stable zip, driven by AppleScript over ssh. The VM's TCC database (SIP is off in the image) gets Apple Events for sshd and osascript to iTerm2 before it starts, so nothing asks. |
| `app` (`signin`, `approve`, `machines`, `reach`) | The release's app (`ARUGULA_MACOS_APP_ZIP` for another) signs in to control through the browser hand-over, is approved as a new device, lists every machine on the account (one on the host, and the Mac's own daemon once it joins), and keystrokes in its terminal run in that machine's pane. | `testnet/macos/app-cloud.ts`. Control, the fake GitHub and the host's machine run here; `web/fixtures/device.ts` is the person: it reads the app's `/#app=` page from Safari (AppleScript), allows it, hands the grant to the app's loopback port, and approves the app. The app's window is read through accessibility (JXA and System Events). |

What they found (2026-10-05, macOS 26.6.2 in the VM):

- **launchd:** before 2026-10-04, `arugulad install` over ssh with no
  GUI session failed: there's no `gui/UID` domain until the user logs in
  to the GUI (`Bootstrap failed: 125: Domain does not support specified
  action`). A Background agent in `user/UID` installs without sudo and
  survives the logout, but not a restart: nothing loads it until that user
  logs in to the GUI again, and an ssh login doesn't. A LaunchDaemon with
  `UserName` survives both, with its panes restored. `arugulad install`
  now picks the Background agent when there's no GUI domain and says the
  restart caveat, and `--system` installs the LaunchDaemon (plan-archive.md, M52);
  every `launchd` check passes, and every one fails with `BREAK=1`.
- **A hard stop loses recent pane output.** `tart stop` on this image is
  a power cut (the guest doesn't shut down in time), and a pane made a
  few seconds before it came back with no output; panes saved at an
  earlier shutdown kept theirs. `vm.sh restart` now shuts the guest down
  first, as a person's restart does. Losing the last output on a power
  cut is expected, not checked.
- **Safari 26.6.2:** Ed25519 keys survive a reload, X25519 keys come back
  from IndexedDB as null, and the wrapped fallback works (verdict
  `wrapped`), as in Playwright's WebKit. The presigned invite passes.
- **iTerm2 3.7.3:** every check passes. In copy mode, `[` `o` (a command's
  output by its marks) found nothing in the TUI over macOS's bash 3.2; the
  test copies a line instead.
- **The app (0.17.0):** every check passes.

### On the macos-arm64 runner

The same scripts can run on the self-hosted runner (jake-mini) once tart
is installed there: Apple silicon runs the VMs without nesting. A job
would run `just macos launchd`, `safari`, `iterm2` and `app` in turn (never
two at once), and needs about 35 GB free for the base and one clone. It
should make the base once and keep it between runs (the prune leaves no
cache behind), and always end with `vm.sh down`. Not tried there yet: tart
needs the runner's user to be able to use Virtualization.framework from
the runner service. The Safari spec could also run on the runner's own
Safari, without a VM, after a one-time `sudo safaridriver --enable` there
and with a GUI login on the runner.

What the VM can't check is under [By hand](#by-hand); what it could and
doesn't yet is under [Planned](#planned).

## Tests that skip without a secret

These skip without their secret, account or tool, and print `SKIP:` with
what's missing:

| Test | Needs |
|---|---|
| `agents_real.rs`, `swarm-real.spec.ts` | `ARUGULA_REAL_AGENTS=claude,codex,screen,...` (real agents; costs a few cents); `screen` uses `ANTHROPIC_API_KEY` when it's set, and VM agents `~/.config/arugula/claude-oauth-token` or `anthropic-key` |
| `mcp.spec.ts`, "the real Claude Code runs a build over MCP" | `ANTHROPIC_API_KEY` and `claude` on PATH (costs a few cents) |
| `resident.rs`, `machines.rs`, `vm_reboot.rs`, `fs.rs`, `mcp.rs`'s VM test, `resident.spec.ts`, `editors-vm.spec.ts` | a wispd token (`ARUGULA_WISP_TOKEN_FILE` or `~/.local/share/wisp/token`), and `just static` for the resident tests |
| `sandbox.spec.ts` (`just e2e-sandbox`) | `ARUGULA_E2E_TAILNET_AUTHKEY_FILE` and wispd |
| `forges_github_real.rs` | the test organization's variables and secrets ([the nightly job](#the-nightly-job-against-githubcom)) |
| `guest_ssh.rs`, `guest-ssh.spec.ts` | an `ssh` client on PATH |
| `scripts/macos-sign` in the release | the `APPLE_*` secrets (a Developer ID, #177); without them it says which is missing and leaves the ad-hoc signature |
| updater signatures and `latest.json` in the release | `TAURI_SIGNING_PRIVATE_KEY` (and its password), and the matching public key in `crates/desktop/tauri.conf.json` |

`workspace.spec.ts` needs the network on its first run, to install the
pinned chant.

## By hand

Only what no test can do:

- **A physical iPhone's Safari and keychain.** Playwright's WebKit with an
  iPhone context and real Safari in the VM cover the engine; nothing
  drives a real phone.
- **The Claude desktop app signed in (#81, #83).** It needs a real
  Anthropic account, so its Code tab session records are a fixture
  ([Fixtures](#fixtures)) and `conversations.rs` checks the daemon reads
  them where the app keeps them on each OS. What the app shows after
  Arugula continues or forks one of its sessions needs the signed-in app.
- **Gatekeeper on a downloaded app.** The VM's `app` test fetches the zip
  with curl, which sets no quarantine flag, so the first-launch prompt a
  browser download gets isn't covered (the app is ad hoc signed until
  #177).
- **Tailscale SSH's check mode** (S28): the check sends the user to an
  identity provider's login.
- **Publishing the VS Code extension** to the Marketplace and Open VSX
  (M28): a publisher account and a release step.

## Planned

Real gaps, each one automatable:

- **The iOS Simulator** (`safari:useSimulator`): the VM's base image has
  no Xcode. cirruslabs' Xcode images have it, at roughly twice the disk;
  the Safari spec would need only that capability.
- **iTerm2 beyond tmux's basics:** dragging dividers, resizing windows,
  detach and reattach ([development.md](development.md#testing-iterm2))
  aren't in `just macos iterm2` yet; they can be, with the same
  AppleScript.
- **S27 in iOS Safari:** `archive/spikes:spikes/s27-blocks/safari/safari.ts --ios`
  needs the Simulator, so the Xcode image (#257). macOS Safari runs
  unattended with `testnet/macos/s27-safari.sh`.
- **The tart tests in CI** on the macos-arm64 runner ([above](#on-the-macos-arm64-runner)).
- **Control checking GitHub's signature on a real webhook delivery**: it
  needs a URL github.com can reach.
- **Cursor's Remote-SSH and the Dev Containers extension** (M28).
- **A Fountain profile** in `testnet/` (#200).
- **Client fixtures** (#200): recorded daemon sessions a client can replay
  against, and a daemon check against previous releases' fixtures.
