# Testing

Every test runs the real binaries as child processes, each with its own
temp state directory and a port the OS picks. Anything outside illogical
(GitHub, Fountain, an agent, Stripe) is a fake served by the test or a
small script, or a response recorded from the real service and checked in.
Nothing in the default run reaches the network or costs money.

```sh
just test        # Rust tests, the web typecheck, e2e-interop, control-smoke
just check       # just test, plus rustfmt and clippy (what CI runs on Linux)
just e2e         # the browser tests, in the system Chrome and WebKit
```

## What runs where

| Command | What it runs | In CI |
|---|---|---|
| `cargo test --workspace` (in `just test`) | unit tests in every crate, and the daemon's integration tests in `crates/daemon/tests/` | Linux and macOS |
| `just e2e-interop` (in `just test`) | the browser's end-to-end crypto (`web/src/e2e`) against Rust's (`crates/e2e`): certificate vectors made by `crates/e2e/examples/interop.rs`, and a Noise handshake with its `responder` | Linux and macOS |
| `just control-smoke` (in `just test`) | `web/control-smoke.ts`: a fake GitHub, Stripe, push service and Sprites API, the real `illogical-control` and real daemons; headless devices sign in, enroll, approve the daemons' join codes and reach them directly and through the relay; the CLI (M49) logs in with a code the device approves and, with no daemon of its own, runs, lists and captures on one machine directly and one through the relay | Linux and macOS |
| `just e2e` | the Playwright specs in `web/e2e/` against throwaway daemons (`just e2e <url>` tests a running one): the `chrome` project, and `webkit` for `*.webkit.spec.ts` | Linux (Playwright's Chromium and WebKit) |
| `just e2e-webkit` | only the `webkit` project: Safari's engine, for device keys (#94) and the one-click invite (#137) | macOS |
| `just testnet up`, `test`, `break` (`ssh`, then `control`) | the Docker test stack's claims ([testnet/README.md](../testnet/README.md)), then each claim under `BREAK=1`, where it must fail | Linux |
| `just desktop-check` | rustfmt and clippy for `crates/desktop` | Linux |
| `just desktop-xvfb` | the Linux desktop app under Xvfb in a container (`packaging/desktop/xvfb/`): `join` (#204) and `m46` (see [The desktop app's tests](#the-desktop-apps-tests)) | no |
| `just desktop-packages ARCH` | the .deb on Ubuntu 22.04 and the .rpm on Fedora 42 install and claim `illogical://` (after `just desktop-linux ARCH`) | no |
| `testnet/macos/desktop.sh`, `testnet/macos/update.sh` | the macOS app from its .dmg in a fresh tart VM, and its updater | no |
| `just check-macos` | clippy for the macOS target from Linux (compiles, doesn't link) | Linux |

CI (`.github/workflows/check.yml`) runs on pushes, on our own machines: the
testnet's ssh and control profiles, `just check` and `just e2e` on Linux (geek); `just
test` and `just e2e-webkit` on macOS (jake-mini). Both runners need Docker
(the testnet, and `ssh.rs` in `just test`): without it the run fails.
`just browsers` installs Playwright's browsers (with their system
libraries on Linux, if sudo needs no password; otherwise run `sudo pnpm exec
playwright install-deps` in `web/` once). See
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
- `crates/vt/fixtures/agents/replay.py`: Claude Code or Codex in a
  terminal, played back from a recording ([below](#the-replay-agent)).
- `fake_mcp.py`: an MCP server whose tools ask the user through elicitation.
- `fake_code_server.py`: code-server (M27).
- Fakes inside the test files themselves: Fountain's API (`fountain.rs`),
  control's relay socket and GitHub App token endpoint, GitHub and Forgejo
  with stand-in `gh` and `tea` (`forge_live.rs`), `systemctl` and `sudo`.

### Guest ssh (M54)

`crates/daemon/tests/guest_ssh.rs` runs the system OpenSSH client
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
- `illogical share --guest` prints a command that works, and `illogical
  guests` lists and revokes.

Run them with `cargo test -p illogicald --test guest_ssh`. They skip,
saying so, if there's no `ssh` on PATH. The relay path through control is a
skipped stub until it's built (PLAN.md, M54). `web/e2e/guest-ssh.spec.ts`
covers *Invite over ssh…* in the pane menu, on desktop and phone viewports,
with the same system ssh (`pnpm exec playwright test e2e/guest-ssh.spec.ts`
in `web/`, after `cargo build -p illogicald` and `pnpm run build`).

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

`E2E_CHROMIUM=1` uses Playwright's Chromium instead of the system Chrome,
as CI does. With `CARGO_TARGET_DIR` set, `just e2e` links `target` to it,
since the specs run `../target/debug/*`.

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

## Spike: blocks through control (S27)

`spikes/s27-blocks/` has its own Playwright suite for #148 (blocks served
from control's block domain, carried to the daemon over Noise by a service
worker). It isn't part of `just e2e`; run it from that directory:

```sh
cd spikes/s27-blocks
pnpm install
./fetch-code-server.sh      # once, for tests/code-server.spec.ts (it skips without)
pnpm test                   # builds s27 and the worker, then Chromium and WebKit
pnpm typecheck
./linux-webkit.sh [specs]   # WebKit on Linux in Playwright's container (Docker, Zig)
node safari/safari.ts       # real Safari via safaridriver; --ios for the Simulator
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
`safaridriver --enable`, the test certificate trusted, `/etc/hosts`);
the tart VM harness (below) is where it runs unattended. `--driver
playwright-webkit` checks the script itself without Safari.

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
--account`, and `illogical login --account`) takes the account without
asking; the fingerprint is what `signin` printed. `just control-smoke` and
the testnet's `control` claims use both. `approve` takes a CLI's code from
`illogical login` as well as a daemon's.

## The desktop app's tests

M46's promises, each checked with nobody at the keyboard. Linux runs in a
container under Xvfb and Openbox with xdotool typing; macOS runs in a fresh
tart VM clone (`testnet/macos/vm.sh`) where System Events types into the
logged-in session over ssh.

| Claim | Linux (`just desktop-xvfb m46 CLAIM`) | macOS (`testnet/macos/desktop.sh CLAIM`) | A failure means |
|---|---|---|---|
| Chords reach the page | `keys`: Ctrl-W, T, N, Q, Tab, F1, F10, Alt-x arrive in a recording pane as bytes | `keys`: Ctrl-W, T, N, Q, Tab as bytes; Cmd-W closes the pane and not the window; Cmd-T opens a tab; Cmd-Q, H, M leave the app up | a menu or the toolkit took a key the terminal needs |
| Tabs in the titlebar | `titlebar`: no decorations; dragging the bar moves the window; the bar's maximize and minimize work | `tabs`: Cmd-N opens a window as a native tab | the window can't be moved or managed without the system's titlebar |
| `illogical://` | `links`: a second launch with `illogical://open?cwd=DIR` opens a tab in DIR in the running app and shows it; `illogical://pane/%N` shows N | `links`: the same through `open URL` (the URL scheme in Info.plist) | links start a second app, or open nothing |
| Global hotkey | `hotkey`: off by default; on, Ctrl+Alt+Space hides the focused window and brings it back | `hotkey`: the same with Ctrl-Option-Space | |
| Service registration | (the systemd unit: `illogicald install`, unchanged) | `agent`: the first start registers the launch agent through SMAppService, BTM lists it, the bundle's daemon answers the linked CLI, no second plist | the app runs a daemon that isn't the one Login Items shows, or two |
| Working pane, no terminal | | `install`, `pane`: the .dmg installs, and a command typed into the window runs | |
| Panes outlive the app | | `restart`: the daemon's pids and panes are the same after the app restarts | |
| Packages | `just desktop-packages ARCH`: .deb and .rpm install, libraries resolve, xdg-mime hands `illogical://` to the app | `install` above | |
| Updates | | `update.sh`: 0.17.0 refuses a manifest signed with another key, then replaces itself with 0.17.1 and restarts; a running vim and a counting build carry on | |

`update.sh` makes a throwaway updater key and builds the app twice with
it. Both versions carry the same daemon, so the daemon isn't restarted by
that test; `upgrade.rs`'s path (a newer bundled daemon replaces the
running one, panes kept) is #176's. The macOS app is copied in without
the quarantine flag: an ad-hoc signed download needs a person's
right-click > Open past Gatekeeper, which only a notarized build (#177)
removes.

## Tests that need something extra

### M49: the CLI through control

Three tests cover M49, from fastest to most faithful:

- `just control-smoke` (in `just test`, so in CI): `illogical login` on
  loopback, approved by the headless device, then `--host box` (direct, it
  has `--direct-url`) and `--host box2` (no URL, so relayed) each `run`,
  `ls` and `capture`, with the CLI's `ILLOGICAL_SOCK` pointing at no
  daemon. `illogical-control`'s own `routing_wire` test checks the join
  and the signed requests (`cargo test -p illogical-control the_cli_joins`).
- `web/e2e/host-menu-control.spec.ts` (`just e2e`): a daemon joined by
  code (approved by the device) has *All your machines…* in its host menu,
  opening control's page; one that isn't joined doesn't.
- The testnet's `m49` claim, which needs Docker:

  ```sh
  just testnet up control
  just testnet test control m49        # PASS
  BREAK=1 just testnet test control m49  # must FAIL: the CLI isn't logged in
  ```

  box-systemd and box-bare join the stack's control over ssh; box-bare's
  daemon is restarted listening on the inner network with
  `ILLOGICAL_DIRECT_URL=http://box-bare:7681`, and box-systemd lists no
  URL, so it's only reachable through the relay. The CLI runs on the
  bastion (on the inner network, no daemon, no `hosts.json`): `illogical
  login` there prints a code that `device-cli.ts approve` approves, then
  `--host box-bare` must report "direct" and `--host box-systemd`
  "relayed" (`ILLOGICAL_VERBOSE=1`), and both `run`, `ls` and `capture`.
  Beside another worktree's stack: `COMPOSE_PROJECT_NAME=illo-j
  ILLOGICAL_TESTNET_INNER_NET=10.229.85 ILLOGICAL_TESTNET_SSH_PORT=22955
  ILLOGICAL_TESTNET_CONTROL_PORT=22985 ILLOGICAL_TESTNET_FAKES_PORT=22986`.

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

## A fresh Mac: the tart VM harness

macOS checks that need a whole Mac (a user who never logged in to the GUI,
real Safari, iTerm2, the desktop app) run in a throwaway macOS VM made with
[tart](https://tart.run), driven over ssh. No person and no window on the
host: everything with a GUI happens inside the VM, whose image logs `admin`
in to its own GUI session at boot. The scripts are in `testnet/macos/`.

```sh
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

### Setup

- **tart.** `brew install cirruslabs/cli/tart`, or, while that tap's
  formula fails on current Homebrew, `tart.tar.gz` from its GitHub release
  (`tart.app` into `~/Applications`, `tart` on `PATH`). Without tart every
  script prints `SKIP: tart is not installed` and exits 0.
- **The base image.** `vm.sh up` makes a local VM `illogical-macos-base`
  from `ghcr.io/cirruslabs/macos-tahoe-base:latest` (macOS 26.6, Safari,
  the Command Line Tools, no Xcode; `ILLOGICAL_MACOS_IMAGE` picks another),
  then empties tart's OCI cache (`tart prune --entries=caches`), so the disk
  holds one copy, about 30 GB, not two. The base is never booted.
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
| `launchd` (`install`, `logout`, `reboot`) | A user made with `sysadminctl`, who never had a GUI session and is reached only over ssh, installs the daemon and starts a pane; the daemon and pane outlive the ssh session; after `tart stop` and `run`, with nobody logged in as them, the daemon is back with its pane. | `ILLOGICAL_MACOS_INSTALL` picks the install: `product` (`illogicald install`, the default), `background` (the plist bootstrapped into `user/UID` with `LimitLoadToSessionType` Background, no sudo) or `system` (a LaunchDaemon with `UserName`, sudo once). |
| `safari` | `/key-probe.html` puts its verdict in the DOM (`data-verdict` on `#verdict`: `keys`, `wrapped` or `none`, and JSON in `#result`), and it's `keys` or `wrapped`. A signed-out invitee opens a presigned invite, signs in through GitHub and joins in one click; the owner's Chrome sees them in the roster. | `web/safari/safari.spec.ts` with a small WebDriver client (`web/safari/webdriver.ts`). safaridriver runs in the VM (`sudo safaridriver --enable` once); its port comes to the host over ssh, and control and the fake GitHub, run on the host, are forwarded to the same ports on the VM's loopback. `SAFARIDRIVER_URL` alone runs the spec against any safaridriver. |
| `iterm2` (`attach`, `type`, `output`, `split`, `tab`, `osc52`) | iTerm2 runs `illogical tmux -CC` and opens a native window for the daemon's tab; text written there runs in the pane; the pane's output shows in iTerm2; a split in iTerm2 adds a pane; a daemon tab becomes an iTerm2 tab. `illogical tui` in iTerm2 copies a line in copy mode, and `pbpaste` has it. | iTerm2's latest stable zip, driven by AppleScript over ssh. The VM's TCC database (SIP is off in the image) gets Apple Events for sshd and osascript to iTerm2 before it starts, so nothing asks. |
| `app` (`signin`, `approve`, `machines`, `reach`) | The release's app (`ILLOGICAL_MACOS_APP_ZIP` for another) signs in to control through the browser hand-over, is approved as a new device, lists every machine on the account (one on the host, and the Mac's own daemon once it joins), and keystrokes in its terminal run in that machine's pane. | `testnet/macos/app-cloud.ts`. Control, the fake GitHub and the host's machine run here; `web/fixtures/device.ts` is the person: it reads the app's `/#app=` page from Safari (AppleScript), allows it, hands the grant to the app's loopback port, and approves the app. The app's window is read through accessibility (JXA and System Events). |

What they found (2026-10-05, macOS 26.6.2 in the VM):

- **launchd:** `illogicald install` over ssh with no GUI session fails:
  there's no `gui/UID` domain until the user logs in to the GUI
  (`Bootstrap failed: 125: Domain does not support specified action`). A
  Background agent in `user/UID` installs without sudo and survives the
  logout, but not a restart: nothing loads it until that user logs in to
  the GUI again, and an ssh login doesn't. A LaunchDaemon with `UserName`
  survives both, with its panes restored. So `launchd` fails today in its
  default mode, `background` passes `install` and `logout`, and `system`
  passes all three.
- **Safari 26.6.2:** Ed25519 keys survive a reload, X25519 keys come back
  from IndexedDB as null, and the wrapped fallback works (verdict
  `wrapped`), as in Playwright's WebKit. The presigned invite passes.
- **iTerm2 3.7.3:** every check passes. In copy mode, `[` `o` (a command's
  output by its marks) found nothing in the TUI over macOS's bash 3.2; the
  test copies a line instead.
- **The app (0.17.0):** every check passes.

### On the macos-arm64 runner

The same scripts run on the self-hosted runner (jake-mini) once tart is
installed there: Apple silicon runs the VMs without nesting. A job would
run `just macos launchd`, `safari`, `iterm2` and `app` in turn (never two
at once), and needs about 35 GB free for the base and one clone. It should
make the base once and keep it between runs (the prune leaves no cache
behind), and always end with `vm.sh down`. Not tried there yet: tart
needs the runner's user to be able to use Virtualization.framework from
the runner service. The Safari spec could also run on the runner's own
Safari, without a VM, after a one-time `sudo safaridriver --enable` there
and with a GUI login on the runner.

### What isn't automated

- **The iOS Simulator** (`safari:useSimulator`): the base image has no
  Xcode. cirruslabs' Xcode images have it, at roughly twice the disk; the
  spec would need only that capability. Not run.
- **A physical iPhone's Safari and keychain.** The Simulator approximates
  it; nothing drives a real phone.
- **The Claude desktop app signed in (#81, #83).** It needs a real
  Anthropic account, so its Code tab session records are a fixture
  (`crates/daemon/tests/fixtures/conversations/desktop/`, made up from the
  fields S20 saw) and `conversations.rs` checks the daemon reads them where
  the app keeps them on each OS. What the app shows after illogical
  continues or forks one of its sessions needs the signed-in app.
- **Gatekeeper on a downloaded app.** The test fetches the zip with curl,
  which sets no quarantine flag, so the first-launch prompt a browser
  download gets isn't covered (the app is ad hoc signed until #177).
- **iTerm2 beyond tmux's basics:** dragging dividers, resizing windows,
  detach and reattach from development.md's script aren't in `iterm2` yet;
  they can be, with the same AppleScript.

## Tests that need Docker

Docker is required for these. Without it they fail, saying what to run;
they don't skip. Only `ILLOGICAL_SKIP_DOCKER=1` skips them, and each then
prints that it did not run. CI never sets it.

| Test | Needs |
|---|---|
| `just testnet up`, `test`, `break`, `down` | Docker ([testnet/README.md](../testnet/README.md)) |
| `crates/daemon/tests/ssh.rs`, `reboot.rs` (in `just test`) | Docker, and the box's binaries: `just static aarch64` on Apple silicon, `just static` on x86_64 (or `ILLOGICAL_SSH_BINARIES`). They bring the ssh profile up themselves if it isn't. |
| `just testnet test control` (M52 and M49 end to end) | Docker and node, and `just testnet up control` first, which needs the static binaries |
| `team-swarm-phones.spec.ts`, "a machine on another network, behind netem" (in `just e2e`) | Docker and `just static <arch>`; it builds a small Debian image with `tc` and `socat`, names its container and network after `COMPOSE_PROJECT_NAME`, and removes them after |

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
`ILLOGICAL_TESTNET_SSH_PORT`). CI runs them in `just check` and `just test`,
and the ssh and control profiles' claims on Linux (see "What runs where").

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

## Tests that skip without a secret

These skip without their secret, account or tool, and print `SKIP:` with
what's missing:

| Test | Needs |
|---|---|
| `agents_real.rs`, `swarm-real.spec.ts` | `ILLOGICAL_REAL_AGENTS=claude,codex,screen,...` (real agents; costs a few cents); `screen` uses `ANTHROPIC_API_KEY` when it's set, and VM agents `~/.config/illogical/claude-oauth-token` or `anthropic-key` |
| `mcp.spec.ts`, "the real Claude Code runs a build over MCP" | `ANTHROPIC_API_KEY` and `claude` on PATH (costs a few cents) |
| `resident.rs`, `machines.rs`, `fs.rs`, `mcp.rs`'s VM test, `resident.spec.ts`, `editors-vm.spec.ts` | a wispd token (`ILLOGICAL_WISP_TOKEN_FILE` or `~/.local/share/wisp/token`), and `just static` for the resident tests |
| `sandbox.spec.ts` (`just e2e-sandbox`) | `ILLOGICAL_E2E_TAILNET_AUTHKEY_FILE` and wispd |
| `workspace.spec.ts` | network on its first run, to install the pinned chant |
| `just testnet test ssh`, `ssh.rs` | Docker, and `just testnet up ssh` first ([testnet/README.md](../testnet/README.md)) |
| `just testnet test control` (M52 end to end) | Docker and node, and `just testnet up control` first, which builds the static binaries |
| `testnet/macos/desktop.sh`, `update.sh` | tart (`brew install cirruslabs/cli/tart`) and the base VM; the .dmg from `just desktop-macos` |
| `scripts/macos-sign` in the release | the `APPLE_*` secrets (a Developer ID, #177); without them it says which is missing and leaves the ad-hoc signature |
| updater signatures and `latest.json` in the release | `TAURI_SIGNING_PRIVATE_KEY` (and its password), and the matching public key in `crates/desktop/tauri.conf.json` |

| `just macos launchd`, `safari`, `iterm2`, `app` | tart on an Apple silicon Mac, about 35 GB free, and network for the image, iTerm2 and the app's zip |

`workspace.spec.ts` needs the network on its first run, to install the
pinned chant.

## By hand

- `just dev`: a separate daemon on 7682 and Vite on 5173.
- `just fake-fleet`: three throwaway daemons with scripted work on
  7730-7732, for the swarm.
- `just screenshots`: the images in `site/img/`, from a scripted session.
- iTerm2's tmux mode beyond what `just macos iterm2` checks (dividers,
  resizing, detach and reattach): [development.md](development.md#testing-iterm2).

## Planned

Tracked in #200:

- More `testnet/` profiles: `ssh` (boxes behind a bastion), `control`
  (control with its relay, reached by boxes that only dial out) and
  `tailnet` (headscale and two Tailscale nodes) exist, and Forgejo and
  GitLab are in `testnet/forges` (above); Fountain doesn't have one yet.
- Client fixtures: recorded daemon sessions a client can replay against,
  and a daemon check against previous releases' fixtures.
