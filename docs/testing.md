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

Waiting on something that takes as long as the machine is busy (a flood
of output, a build): stop it or wait for its end, never sleep a fixed time
and hope, and make deadlines failure limits that are generous (tens of
seconds) rather than waits. A reader that has to keep up with a flood
(the tmux client in `tmux.rs`) does as little per line as it can and lets
its own timeouts expire while lines keep coming. `falling_behind_pauses_the_pane`
failed under a load average of 30 because a fixed 40 MB flood was still
running after `continue`; it now runs `yes` until the pane pauses, sends
^C and waits for the prompt in a capture.

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

## By hand

- `just dev`: a separate daemon on 7682 and Vite on 5173.
- `just fake-fleet`: three throwaway daemons with scripted work on
  7730-7732, for the swarm.
- `just screenshots`: the images in `site/img/`, from a scripted session.
- iTerm2's tmux mode: [development.md](development.md#testing-iterm2).

## Planned

Tracked in #200:

- More `testnet/` profiles: `ssh` (boxes behind a bastion) and `control`
  (control with its relay, reached by boxes that only dial out) exist;
  Fountain and Forgejo don't yet.
- Client fixtures: recorded daemon sessions a client can replay against,
  and a daemon check against previous releases' fixtures.
- The Playwright suite in CI.
