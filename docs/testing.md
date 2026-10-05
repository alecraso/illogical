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
just macos base             # make the base VM, once
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
  (`tart.app` into `~/Applications`, `tart` on `PATH`). Without tart, or
  without the base VM, every script fails (exit 1) and says how to get
  it: a check that didn't run isn't a pass. `ILLOGICAL_SKIP_MACOS_VM=1` is
  the only way to skip, and it prints that no macOS VM test ran.
- **The base image.** `just macos base` (`vm.sh base`) makes a local VM `illogical-macos-base`
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
| `launchd` (`install`, `warning`, `logout`, `uninstall-agent`, `ssh`, `system`, `reboot`, `uninstall`) | A user made with `sysadminctl`, who never had a GUI session and is reached only over ssh, runs `illogicald install`: it installs, warns that the daemon won't start after a reboot by itself, and the daemon and a pane outlive the ssh session. `illogicald uninstall` leaves nothing behind. `illogical --ssh illo@vm ls` from the host starts the daemon there and passes the warning through. `illogicald install --system` switches to a LaunchDaemon cleanly; after `tart stop` and `run`, with nobody logged in as them, the daemon is back with its pane; `illogicald uninstall` removes the LaunchDaemon too. | One VM, in that order. "Nothing behind" means no plist in `~/Library/LaunchAgents` or `/Library/LaunchDaemons`, no `illogicald` service in `gui/UID`, `user/UID` or `system`, and no `illogicald` process for the user. The user gets passwordless sudo before `system`, as an admin would have. `BREAK=1` boots the service out before install, logout, system and reboot, drops the `note:` line before warning, installs again after each uninstall, and has the daemon already running when the ssh check would start it. |
| `safari` | `/key-probe.html` puts its verdict in the DOM (`data-verdict` on `#verdict`: `keys`, `wrapped` or `none`, and JSON in `#result`), and it's `keys` or `wrapped`. A signed-out invitee opens a presigned invite, signs in through GitHub and joins in one click; the owner's Chrome sees them in the roster. | `web/safari/safari.spec.ts` with a small WebDriver client (`web/safari/webdriver.ts`). safaridriver runs in the VM (`sudo safaridriver --enable` once); its port comes to the host over ssh, and control and the fake GitHub, run on the host, are forwarded to the same ports on the VM's loopback. `SAFARIDRIVER_URL` alone runs the spec against any safaridriver. |
| `iterm2` (`attach`, `type`, `output`, `split`, `tab`, `osc52`) | iTerm2 runs `illogical tmux -CC` and opens a native window for the daemon's tab; text written there runs in the pane; the pane's output shows in iTerm2; a split in iTerm2 adds a pane; a daemon tab becomes an iTerm2 tab. `illogical tui` in iTerm2 copies a line in copy mode, and `pbpaste` has it. | iTerm2's latest stable zip, driven by AppleScript over ssh. The VM's TCC database (SIP is off in the image) gets Apple Events for sshd and osascript to iTerm2 before it starts, so nothing asks. |
| `app` (`signin`, `approve`, `machines`, `reach`) | The release's app (`ILLOGICAL_MACOS_APP_ZIP` for another) signs in to control through the browser hand-over, is approved as a new device, lists every machine on the account (one on the host, and the Mac's own daemon once it joins), and keystrokes in its terminal run in that machine's pane. | `testnet/macos/app-cloud.ts`. Control, the fake GitHub and the host's machine run here; `web/fixtures/device.ts` is the person: it reads the app's `/#app=` page from Safari (AppleScript), allows it, hands the grant to the app's loopback port, and approves the app. The app's window is read through accessibility (JXA and System Events). |

What they found (2026-10-05, macOS 26.6.2 in the VM):

- **launchd:** before 2026-10-04, `illogicald install` over ssh with no
  GUI session failed: there's no `gui/UID` domain until the user logs in
  to the GUI (`Bootstrap failed: 125: Domain does not support specified
  action`). A Background agent in `user/UID` installs without sudo and
  survives the logout, but not a restart: nothing loads it until that user
  logs in to the GUI again, and an ssh login doesn't. A LaunchDaemon with
  `UserName` survives both, with its panes restored. `illogicald install`
  now picks the Background agent when there's no GUI domain and says the
  restart caveat, and `--system` installs the LaunchDaemon (PLAN.md, M52);
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

## Tests that need something extra

These skip, saying why, unless what they need is there:

| Test | Needs |
|---|---|
| `agents_real.rs`, `swarm-real.spec.ts` | `ILLOGICAL_REAL_AGENTS=claude,codex,...` (real agents; costs a few cents) |
| `resident.rs`, `resident.spec.ts`, `editors-vm.spec.ts` | a wispd token and `just static` |
| `sandbox.spec.ts` (`just e2e-sandbox`) | `ILLOGICAL_E2E_TAILNET_AUTHKEY_FILE` and wispd |
| `workspace.spec.ts` | network on its first run, to install the pinned chant |
| `just testnet test ssh`, `ssh.rs` | Docker, and `just testnet up ssh` first ([testnet/README.md](../testnet/README.md)) |
| `just testnet test control` (M52 end to end) | Docker and node, and `just testnet up control` first, which builds the static binaries |
| `just macos launchd`, `safari`, `iterm2`, `app` | tart on an Apple silicon Mac, about 35 GB free, and network for the image, iTerm2 and the app's zip |

## By hand

- `just dev`: a separate daemon on 7682 and Vite on 5173.
- `just fake-fleet`: three throwaway daemons with scripted work on
  7730-7732, for the swarm.
- `just screenshots`: the images in `site/img/`, from a scripted session.
- iTerm2's tmux mode beyond what `just macos iterm2` checks (dividers,
  resizing, detach and reattach): [development.md](development.md#testing-iterm2).

## Planned

Tracked in #200:

- More `testnet/` profiles: `ssh` (boxes behind a bastion) and `control`
  (control with its relay, reached by boxes that only dial out) exist;
  Fountain and Forgejo don't yet.
- Client fixtures: recorded daemon sessions a client can replay against,
  and a daemon check against previous releases' fixtures.
- The Playwright suite in CI.
