# Working on Arugula

Arugula keeps terminals and agent sessions alive on a machine and lets you
reach them from a browser, a phone, the desktop app or a shell. One daemon,
`arugulad`, per machine owns the panes; every client attaches to it.

Building, running and releasing are in [docs/development.md](docs/development.md);
tests in [docs/testing.md](docs/testing.md); contributing in
[CONTRIBUTING.md](CONTRIBUTING.md).

## Crates

Each has a `README.md` with where to start reading.

- `crates/core`: the multiplexer's state, independent of PTYs and networking:
  sessions, tabs, split trees, the intents that change them, the cell layout.
- `crates/proto`: the wire protocol and the HTTP API's types, shared by the
  daemon and every client.
- `crates/vt`: a pane's terminal state on libghostty-vt (`VtEngine`).
- `crates/e2e`: end-to-end encryption between client devices and daemons
  (Noise IK; design in [docs/control-e2e.md](docs/control-e2e.md)).
- `crates/daemon`: `arugulad`.
- `crates/cli`: `arugula`, the CLI, plus `arugula tui` and
  `arugula tmux -CC`.
- `crates/control`: Arugula control: accounts, devices, the directory and the
  relay.
- `crates/control-wire`: the enrolment, routing and relay messages between
  daemons and control, one type each, so both sides build from the same
  definition.
- `crates/testkit`: the harness for the daemon's integration tests.
- `crates/desktop`: the desktop app (Tauri). Outside the Cargo workspace, with
  its own `Cargo.lock`, so it releases on its own (#388).
- `web/`: the web client (TypeScript, Preact, xterm.js). The daemon embeds its
  build.
- `vendor/libghostty-vt-sys`: libghostty's sys crate, vendored so the build can
  apply `patches/` to Ghostty.

## The daemon, by layer

Paths are under `crates/daemon/src/`.

- **Edge:** the embedded web client and `/ws` (`server.rs`), the HTTP API
  (`api.rs`), MCP (`mcp/`), share links (`share.rs`), guest ssh
  (`guest_ssh.rs`), file uploads (`upload.rs`), the dial-out transport
  (`dial.rs`), end-to-end channels from client devices (`e2e.rs`), enrolment in
  control (`control.rs`), Windows' named pipe (`pipe.rs`).
- **Who may:** who may talk to the daemon (`access.rs`, `localauth.rs`),
  principals and grants (`acl.rs`), the API for someone who isn't the owner
  (`authz.rs`), standing permission rules (`rules.rs`), file modes (`perm.rs`).
- **The mux:** `mux/`: the task that owns the layout, the panes and every
  client (`mod.rs`), with one file per area: `attention.rs`, `clients.rs`,
  `blocks.rs`, `api_calls.rs`, `who_may.rs`, `thread_ops.rs`, `machines.rs`,
  `call_ops.rs`, `info.rs`, `config.rs`. Next to it: threads on panes
  (`threads.rs`), huddles (`calls.rs`), gates waiting for a person
  (`gate.rs`), hands (`hand.rs`), Web Push (`push.rs`).
- **Panes:** a process on a PTY and its VT thread (`pane.rs`), Windows'
  pseudoconsole (`conpty.rs`), what survives a restart (`store.rs`),
  keeping terminals open across restarts (`holder.rs`, `shim.rs`, `sys.rs`),
  prompts and commands in the output (`osc.rs`), shell integration
  (`shellint.rs`, `shellenv.rs`), what a pane is busy with (`classify.rs`),
  process info (`procinfo.rs`), history (`history.rs`), resuming an agent's
  conversation (`resume.rs`), named keys (`keys.rs`).
- **Blocks:** what every block provides (`block.rs`), agents (`agent/`),
  browsers (`browser.rs`) and block sites (`sites.rs`, `tls.rs`, `ports.rs`),
  editors (`editor/`), review (`review/`), files (`fs.rs`), forges (`forge/`),
  Fountain (`fountain/`), workspaces (`workspace/`), studio apps (`apps/`),
  conversations (`conversations/`), the IDE bridge (`ide/`), invites
  (`invite/`), remote blocks (`remote.rs`), configured agent harnesses
  (`inventory.rs`).
- **Reach:** other daemons (`hosts.rs`), machines that aren't this host
  (`machine.rs`), sandbox providers (`provider/`, `provider_tunnel.rs`,
  `resident.rs`, `sandbox.rs`), synced history (`sync.rs`, `seal.rs`),
  tailscaled (`tailscale.rs`), outgoing TLS roots (`roots.rs`).
- **Lifecycle:** the command line (`args.rs`), startup (`main.rs`), `install`
  (`install.rs`), Getting started (`setup.rs`), updates (`update.rs`,
  `selfupdate.rs`), `_host` (`host.rs`), malloc settings (`heap.rs`), paths
  through links (`paths.rs`).

## Invariants

- The mux task decides the order of every change. Other tasks send it a
  `Cmd` and wait for the answer (`mux/mod.rs`).
- libghostty's terminal is `!Send`: each pane's VT thread owns it, and
  everything else asks that thread (`pane.rs`, `crates/vt`).
- The web client's wire types are generated: after changing a type it uses,
  run `just proto-ts`. CI fails if `web/src/proto.gen.ts` is stale.
- Every request passes the access checks before it reaches the mux
  (`access.rs`, `authz.rs`, `arugula_core::access`).
- The daemon serves the web client itself, embedded in the binary
  (`server.rs`). That is on purpose (#387).

## Clients

The web client and the desktop app get new features. The TUI
(`crates/cli/src/tui/`) and tmux -CC (`crates/cli/src/tmux/`) are kept working
and tested, but get no new features.

## Labs

An empty `labs` file in the state dir turns on what a stranger doesn't get
(#385). It's read by `arugula_proto::hosts::labs`. `ARUGULA_STATE_DIR`
moves the state dir. A `labs` cargo feature is planned (#452).

## Commands

- `just bootstrap`, then `just web`: toolchains, web dependencies, and the web
  build the daemon embeds.
- `just check`: what CI runs: `just test` (nextest, doctests, the web
  typecheck), then the `proto-ts` check, fmt and clippy.
- `just e2e`: the browser specs. `just e2e-webkit`: WebKit's.
- `just dev`: a dev daemon on 7682 (state in `~/.local/state/arugula-dev`)
  and Vite on 5173, leaving your daily daemon alone.
- `just proto-ts`: regenerate `web/src/proto.gen.ts`.
- In a worktree, set `CARGO_TARGET_DIR` to a directory of its own, so
  parallel builds don't share one `target/`.

## Milestone codes

Comments carry codes like M43 and S21. They name milestones and spikes from
the original plan; the index at the top of
[docs/plan-archive.md](docs/plan-archive.md) says what each was. When you
touch a module anyway, replace its leading "M43:" tag with a plain name.

## Decisions

[DECISIONS.md](DECISIONS.md): the decisions that still constrain the code.
