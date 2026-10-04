# Development

## Build from source

You need [rustup](https://rustup.rs) (the toolchain is pinned in
`rust-toolchain.toml`), [mise](https://mise.jdx.dev) (it installs the exact
Zig that libghostty needs, from `.mise.toml`), [just](https://just.systems),
Node and [pnpm](https://pnpm.io). On macOS, the Xcode command line tools too.

```
just bootstrap      # Zig via mise, web dependencies
just install        # release build, installed and started as a service
```

The daemon embeds the web client (`web/dist`) at build time, so build
through `just` (`just build`, `just install`); a bare `cargo build
--release` stops and says to build the web client first. `just static`
builds static Linux binaries (musl, with Zig as the C compiler);
`just static aarch64` builds them for arm64. `just dist` makes the
release tarballs in `dist/`.

## Working on it

`just dev` runs a separate daemon on 7682 (state in
`~/.local/state/illogical-dev`) plus Vite on 5173, leaving the real one
alone. `just check` is what CI runs; `just e2e` drives the system Chrome
against throwaway daemons, or `just e2e https://home.<tailnet>.ts.net`
against the running one. `workspace.spec.ts` runs the real chant: its first run
installs the pinned version into `web/e2e/fixtures/chant-workspace` with
`npm ci` (CI doesn't run the browser tests; the daemon's own workspace
tests use a stand-in chant). `just screenshots` regenerates the images in
`site/img/` from a throwaway daemon with a scripted demo session.

## Releasing

1. Set the version in the workspace `Cargo.toml` and commit (`just
   notices` if dependencies changed; CI fails if THIRD_PARTY.md is stale).
2. `git tag -a vX.Y.Z -m "illogical X.Y.Z" && git push origin vX.Y.Z`.
   `.github/workflows/release.yml` builds the Linux tarballs on geek and
   the macOS one on jake-mini, attaches them and `SHA256SUMS` to the
   GitHub release, and bumps the formula in `arugula-salad/homebrew-tap`
   (`scripts/release`; the tap's deploy key is the `HOMEBREW_TAP_KEY`
   secret).
3. `install.sh` picks up the latest release by itself. If the page
   changed, `just site-deploy` publishes it (wrangler's login on geek).

CI runs on two self-hosted GitHub Actions runners in the arugula-salad
org's `illogical` runner group, which only this repo may use: geek
(`linux-x86_64`, a systemd user service,
`~/.config/systemd/user/actions-runner-illogical.service`, runner in
`~/.local/share/actions-runner-illogical`) and jake-mini (`macos-arm64`, a
launchd agent, `~/Library/LaunchAgents/illogical.actions-runner.plist`,
with `ProcessType` Interactive: launchd's throttling of background agents
made daemon tests time out). Both run jobs on the host and keep their
build in `~/.cache/illogical-ci/`, which each job deletes first once it
passes 30 GB (`scripts/ci-cap-target`): cargo never prunes it, and on
2026-10-02 it grew to 136 GB, filled jake-mini's disk and took the home
cluster down. Workflows run on pushes and tags only, never on pull
requests, since they run on those hosts; and the repo asks for approval
before any outside contributor's workflow runs, so a fork's PR can't add
a trigger of its own and reach them. A job's log: `gh run view --log
<run id>` (or `--log-failed`).

The repo moved from Forgejo (`git.inevitable.fyi/jhgaylor/illogical`,
archived; v0.1.0–v0.12.0 assets are still there) on 2026-10-03, with issue
and PR numbers kept.

## Testing iTerm2

Nothing here has seen a real iTerm2 yet. From the Mac, against geek:

1. On geek, install the build (`just install`) and check `illogical ls`
   works. Open <https://geek.tail1234.ts.net> in a browser beside iTerm2.
2. In iTerm2: `ssh -t geek '~/.local/bin/illogical tmux -CC attach'`. A new
   iTerm2 window opens with a tab per illogical tab (the gateway window
   says "tmux mode"). The tab's shell prompt is there, with its history.
3. Type `ls` and Enter in it: the output appears in iTerm2 and in the
   browser's same pane.
4. *Shell › Split Vertically*, then *Split Horizontally*: three native
   splits; the browser shows the same three panes within a second.
5. Drag an iTerm2 divider: the browser's divider moves to match. Drag one
   in the browser: iTerm2's moves. Resize the iTerm2 window: the panes
   reflow and the browser letterboxes the tab at iTerm2's size; click in
   the browser's pane and type, and the browser takes the size back.
6. ⌘T for a new tab: a new tab appears in the browser too. Close it in
   iTerm2 (⌘W, *Kill*): it goes from the browser. Close a split with
   `exit`: its pane goes from both.
7. Run `vim` (or `htop`) in a pane, type a little, and leave it running.
8. Detach (*Shell › tmux › Detach*). The iTerm2 windows close; vim keeps
   running in the browser.
9. Reattach with the same `ssh` command: the tabs and splits come back as
   they were, with vim on screen; quit it with `:q` and the shell prompt is
   on the line after the `vim` command.
10. In the browser, split a pane and open a new tab: iTerm2 shows both.

Watch for: an alert from iTerm2 about an unexpected reply (it disconnects
on any error it doesn't expect; note the command it names), panes that
stay blank after attach, output in the wrong pane, a window that keeps
resizing itself when both iTerm2 and the browser are open, and garbled
screens after a reattach. To record the conversation, start it with
`ILLOGICAL_TMUX_LOG`: `ssh -t geek 'ILLOGICAL_TMUX_LOG=/tmp/cc.log
~/.local/bin/illogical tmux -CC attach'` writes every line both ways (`>`
from iTerm2, `<` to it) to `/tmp/cc.log` on geek.

## Layout

- `crates/core`: sessions, tabs and split trees, the intents that change
  them, and the cell layout. Pure state, property-tested.
- `crates/proto`: wire protocol (JSON control messages + binary frames with a
  per-pane stream offset). Mirrored by hand in `web/src/proto.ts`.
- `crates/vt`: server-side terminal state on libghostty-vt (libghostty-rs
  `master`, Zig 0.16). VT snapshots for xterm.js (spike S1's fix-ups),
  checkpoints for disk (GHOSTSNP + zstd, spike S5), answers to terminal
  queries limited to what xterm.js can draw, recorded fixtures.
- `crates/daemon`: `illogicald`. A multiplexer task owning the layout and
  attention (`mux.rs`), a PTY + VT thread per pane with its log, checkpoints
  and OSC scanner (`pane.rs`, `store.rs`, `osc.rs`), restore and restart
  policies, the pane shim and FD store (`shim.rs`, `sys.rs`), shell
  integration (`shellint.rs`, `shell/`), the HTTP API (`api.rs`, history and
  search in `history.rs`), Web Push (`push.rs`), VM panes on wisp
  (`machine.rs`), sandbox providers (`provider/`: the `Provider` trait
  and its capabilities, and the Sprites API adapter: exec TTY and piped
  sessions, the proxy, files and services), sandboxes and resident daemons
  (`resident.rs`), the provider tunnel (`provider_tunnel.rs`), blocks
  (`block.rs`, `browser.rs`; agents in `agent/`: the ACP client, the
  transcript, agent definitions, the local and VM pipes), block sites
  (`sites.rs`: per-block origins and their HTTP proxy; `ports.rs`: reaching
  a port here or in a VM; `tls.rs`: the wildcard certificate and ACME), the
  WebSocket server, embedded web client, access checks, `install`.
  Federation: the host list and invites (`hosts.rs`), tailscaled's local
  API and WhoIs (`tailscale.rs`), and sandboxes (`sandbox.rs`: `install
  --tailnet` and the `sandbox` supervisor). M4c: the dial-out transport
  (`dial.rs`, over `dialout_mux.rs`'s streams), share links (`share.rs`), and
  history sync (`sync.rs`, sealed by `seal.rs`). M7: files on a host
  (`fs.rs`), names (`illogical_core::names`). M11: diff and file blocks
  (`review/`). M6c: questions and forms
  (`illogical_proto::ask`: the card's shape and how its answer becomes
  Claude Code's; agent blocks' elicitations in `agent/`; a terminal's
  questions in `mux.rs` and the `/ask` route). M16: MCP (`mcp/`: the server at `/mcp`, its
  tools, and client and block tokens).
- `crates/cli`: `illogical`, over the daemon's Unix socket, or HTTP(S) to
  another daemon with `--host` (`hosts.rs`); `ask.rs` is Claude Code's
  AskUserQuestion hook; `mcp.rs` is `illogical mcp`, the stdio bridge to
  `/mcp`. `tmux/` is the tmux
  control-mode front end (M5): the command parser and `-F` format expander,
  layout strings derived from the daemon's ratios (spike S11's converter),
  and a mirror terminal per pane so captures line up with the output
  stream. `crates/daemon/tests/tmux.rs` replays iTerm2's command sequence
  and compares every reply with what tmux 3.6 answered (S11's transcript).
- `web`: TypeScript client: Preact for the chrome, xterm.js 6 terminals that
  are moved between slots rather than recreated, Playwright tests (desktop
  and phone).
- `vendor/libghostty-vt-sys`: libghostty-rs's sys crate, vendored (the root
  `Cargo.toml` patches it in) so the build can apply `patches/*.patch` to
  Ghostty after checkout (M9: no Zig signal stack in every thread).
- `spikes`: S1–S3 write-ups and code.

## Things M0–M4c taught us

- **Don't promise what the client can't draw.** libghostty answered Neovim's
  "do you support left/right margins?" with yes, Neovim used them for
  vertical splits, and xterm.js drew garbage. The engine now rewrites its
  replies to xterm.js's measured capabilities (`crates/vt/src/compat.rs`), and
  the client stops xterm.js from answering queries itself, so programs get
  exactly one answer whether or not anyone is attached.
- **libghostty in a debug build is ~3000x slower** (0.2 MB/s). `.cargo/config.toml`
  builds it as Zig ReleaseSafe in every profile: 175–580 MB/s, with safety
  checks kept, since it parses untrusted program output.
- **A library's thread-locals are every thread's** (M9). Zig's 256 KiB
  threadlocal signal stack went into the daemon's static TLS, and glibc
  gave every thread a zeroed copy: 1.3 MB per pane. Check `readelf -S`
  for `.tbss` after a libghostty upgrade. glibc also kept about half a busy
  daemon's peak after panes closed, until `heap.rs` fixed the mmap
  threshold. `crates/daemon/tests/memory.rs` guards both.
- **Offsets need an epoch.** A reconnecting client's offset is only valid for
  the stream it came from; the pane's epoch changes when the daemon restarts.
- **Size travels in order with output.** A client must resize before drawing
  a snapshot, so size changes share the bounded output queue. Only the
  "you fell behind, resync" notice uses a separate channel.
- **Cells, not pixels.** The plan said react-mosaic; it lays panes out in
  its own pixels, which drift from the PTY sizes. The daemon computes cell
  rectangles instead (and M5's tmux layout strings come for free), and the
  client draws them.
- **Size is per tab.** With splits, one window's size decides every pane in a
  tab; per-pane ownership would mix a phone's and a desktop's sizes in one
  tab. A phone claims its tab with one pane zoomed; the others keep their
  sizes until a desktop takes the tab back.
- **WebGL drew nothing in phone emulation** (fractional pixel ratio), so
  touch devices use xterm's DOM renderer; desktops use WebGL for visible panes
  only and release contexts for hidden ones (Chrome allows ~16).
- **Preact 11 no longer appends `px`** to numeric styles. Zeros still worked,
  so the bug looked like a layout one.
- **Subscribe, then catch up.** A store subscription made in an effect misses
  anything that happens before the first paint; the daemon's hello sometimes
  won that race and left a window blank.
- **Upstream fixes move bugs.** The newer libghostty formatter fixed the
  cursor S1 had to re-place, but now writes tab stops before the content and
  leaves the cursor on the last stop, so the first line wrapped. The fixture
  tests caught it on the upgrade; the block is moved to the end.
- **Leaving the alternate screen restores the cursor** even when nothing is
  on it, so the restore marker only sends `?1049l` if a full-screen program
  was showing; otherwise it overwrote the last lines of scrollback.
- **A reboot kills shells and the daemon together.** `KillMode=mixed` stops
  the daemon first (it saves, then exits) and kills the shells after; and a
  shell killed by a signal never closes its pane, so even a race can't lose
  one.
- **"Re-run" reads /proc**, so `bash -c 'a; b'` that exec'd into `b` re-runs
  `b`. The typed command line needs shell integration (M3).
- **A restarted daemon isn't anyone's parent.** The shim records each
  program's pid, start time and exit status; the daemon watches through a
  `pidfd` (which works for non-children) and checks the start time before
  adopting, so a reused pid is never mistaken for the pane's program.
- **A pane closed as it starts can't leave its program behind.** The shim
  records the pid only after the exec, and it owns the SIGKILL that follows
  a close's hangup, so it happens even if the daemon is gone. Test daemons
  also kill whatever their state dir records as running before deleting it.
- **DECSTR doesn't reset input modes.** A pane restored after its program
  died kept that program's mouse and focus reporting, so clicking sent stray
  `ESC [ O` to the new shell. The restore marker now turns them off.
- **"What happened after I typed" needs the offset at send time.** `send`
  then `wait` raced: the pane recorded the input when it processed it, so a
  quick `wait` could return the previous command. The API now records it
  before queueing the input.
- **A notification outlives its command.** Attention set by OSC 9 was
  cleared a moment later when the `printf` that sent it finished.
- **Unix socket paths max out at ~108 bytes.** Long state directories get a
  socket in `$XDG_RUNTIME_DIR` instead, recorded in `state/sock.path`.
- **wisp already had the fix for its replay.** The M3b spike found that
  reattaching resends the whole session and planned to ask for a `since=`
  parameter, but wisp's `output_offset` (not one of the names the spike
  tried) does exactly that. Each VM pane keeps its session id and how many
  bytes it has logged in `exec.json`, and reattaches from there.
- **bash expands `ENV`, command substitution included,** so a VM's shell
  gets the integration with nothing installed: the script travels in an
  environment variable and `ENV='$(…)'` writes it to a temporary file.
- **`kill?signal=HUP` ends a VM shell at once;** wisp's default TERM waits
  10s, because an interactive bash ignores it.
- **A tab's title follows its active pane,** so grabbing a pane to drag it
  can resize its tab under the pointer. The tests aim after the pane is
  active, and anything that measures the tab bar mid-drag should too.
- **In userspace mode, the tailnet arrives on loopback.** tailscaled's
  netstack forwards a tailnet connection to the daemon's port as one from
  127.0.0.1, with any Host header the sender likes, so "loopback means
  local" would let in anyone the ACL lets reach the sandbox. The daemon asks
  tailscaled's WhoIs about every peer there (it knows forwarded
  connections); a second daemon in a sprite with another owner refused geek
  even with a forged loopback Host and serve header.
- **serve sends no identity for tagged nodes** (or Funnel). A request for
  the tailnet name without `Tailscale-User-Login` used to pass as local;
  with sandboxes on the tailnet that would have handed geek's terminals to
  any tagged node the ACL let through. It is refused now, except joining
  the host list with an invite.
- **Zig as a musl C compiler:** cc-rs passes a Rust-style `--target=` that
  Zig rejects, and Zig turns on UBSan for unoptimized C (aws-lc's
  jitterentropy), whose runtime nothing links. `scripts/zig-cc-musl` drops
  the one and turns off the other.
- **Children inherit a blocked signal mask.** The sandbox supervisor
  blocks SIGTERM to wait for it, and std's `Command` passed that on: the
  daemon never heard SIGTERM and was killed instead of saving its panes.
- **A restarted tailscaled says `Starting` for a moment.** A daemon that
  asked then got no tailnet name (and refused its own URL), and an install
  that asked then logged in again. Both wait for it to settle now.
- **Through the home daemon, a host's answer is the home daemon's.** A
  dial-out host's responses are served on geek's origin, so a hostile
  sandbox could have put a page there with the run of geek's API. Only its
  WebSocket and `/api` are forwarded, and every answer is defanged (a
  `sandbox` CSP, `nosniff`, no cookies or CORS).
- **Match paths exactly where identity is relaxed.** A prefix check let
  `/share/<token>/../api/panes` through the viewer's door (the router then
  found nothing, but only by luck); the guard now accepts the exact shapes.
- **clap gives a subcommand's positional the same id as a global flag of
  the same name.** `illogical synced rm sbx` set `--host sbx`. A
  subcommand's own `--host` loses to the global one the same way, so
  `open` and `agent` take `--machine mN`, like `edit` (#61); a `--host
  mN` there that isn't in the host list says so.
- **"Cold" can be had on demand.** wisp turns a suspended sprite cold
  after `--warm-ttl` (1h) by dropping its memory snapshot, which makes the
  next wake a real boot. Its web UI's operator endpoints do the same at
  once (`POST /ui/api/sprites/NAME/suspend`, then `/cool`, with a session
  from `/ui/login`), which is how `resident.spec.ts` tests a cold wake.
  Suspending syncs the guest's disks first, so the resident daemon's log
  and checkpoints are there after the reboot.
- **A TUI on the main screen leaves the cursor mid-screen.** Claude Code
  draws in place and doesn't use the alternate screen, so after a restore
  the marker landed on top of it; it now goes below the last row with
  text.
- **Sprites lists are paged** (50 at a time); wisp here holds more than
  that.

- **A program's exit can overtake its last output.** A pane's reader and
  its wait for the exit are separate threads, so a quick `illogical run`
  could end its command before its output arrived, and `capture
  --scope last-command` came back empty (#60). The exit now waits for the
  terminal to hang up (at most a second) before it's handled.
- **Check the record once more after the shim exits.** A program that ends
  at once (`run true`) can record its pid and be gone between two looks,
  and its start was reported as a failure. A failed agent start now says
  how the shim ended and the last of what it wrote to `agent.err` (#64).
- **A free port isn't free for long.** Tests picked one by binding port 0
  and letting go; under load something else took it first, the daemon
  exited with "address in use", and the test waited out its deadline as
  "daemon did not start" (#66). `--listen 127.0.0.1:0` now has the daemon
  pick its own port, bound before anything else, and record it in
  `state/listen`; test daemons use that. `--block-listen 127.0.0.1:0`
  does the same in `state/block-listen`, and illogical-control's `--listen`
  in `listen` beside its database; `--direct-url` and control's
  `--public-url` with port 0 mean the port it got. The Playwright specs
  use all of these (`web/e2e/ports.ts`), and their fake servers listen on
  port 0, so a run takes no port but `E2E_PORT` and two worktrees can run
  the suite at once (#67).
- **A spec module is loaded more than once.** Playwright loads each spec
  in the runner as well as the worker, so a top-level `mkdtempSync` left
  a directory per run that `afterAll` never saw (#62). Make them in
  `beforeAll`; the config's own run directories go when the runner exits.
- **Send a test's HTTP request in one write.** `write!` on a socket writes
  each piece of the format string separately; a handler that answers
  without reading the body (a 404) closed the connection before the body
  went, and the test's next write failed with a broken pipe.
- **A frame from another site may get no storage at all.** With "Block
  third-party cookies", Chrome also refuses a cross-site frame its
  `localStorage`, IndexedDB and service workers. VS Code falls back to
  memory for IndexedDB but not for `localStorage`: it threw, and an editor
  block was blank while the same page on its own worked (#69). Playwright's
  Chrome allows third-party cookies, and 127.0.0.1 and `*.localhost` are
  already different sites, so the specs never saw it. An editor block's
  site now puts a script of illogical's first in its pages that gives the
  window storage in memory when it's refused (`editor/storage.js`), and
  `editors.spec.ts` runs a block in a Chrome profile that blocks
  third-party cookies (`sec-fetch-storage-access: none` says it's
  refused). When a block's page fails in a frame, `RUST_LOG=illogicald::sites=debug`
  logs every request its site refuses, and why.
