# Decisions

The decisions that still constrain the code, one entry each. Before you change
one of these, read its entry; if you change it, change the entry in the same PR.

Each entry says what was decided, why, where it lives in the code today, and
where it came from. Milestone and spike codes (M2, S5 …) are looked up in
[docs/plan-archive.md](docs/plan-archive.md), which is the old PLAN.md kept as
written. Anything here that the archive says differently has changed since;
this file is the one that matches the code.

Not here: what each milestone built (the archive), how to build and test
([docs/development.md](docs/development.md)), what the product does
([docs/features.md](docs/features.md)).

## Protocol and transports

### One protocol, two transports
The web client, the CLI, the TUI and the API speak one protocol. On a
WebSocket it is JSON text frames for control messages and binary frames for
terminal bytes; on the Unix socket (a named pipe on Windows) the same messages
travel length-prefixed. A transport never shapes the protocol or the core, so
a new way to reach a daemon is an adapter.
Where: `crates/proto/src/lib.rs` (`ClientMsg`, `ServerMsg`, `Frame`), `crates/daemon/src/server.rs` (`/ws`), `crates/daemon/src/pipe.rs`.
From: [Protocol](docs/plan-archive.md#protocol-one-protocol-two-transports), [M4](docs/plan-archive.md#m4-reach-a-shell-on-any-machine-or-sandbox).

### Terminal bytes are offsets into a log
Output frames are `[u8 kind][u32 pane][u64 offset][bytes]`, big-endian. Every
byte has an offset in the pane's log, so a client resumes by offset, and
`tail`, replay and scrollback restore are seeks. A client that is within 1 MB
of the log's end gets a replay; anyone further behind gets a snapshot.
Why: asciicast has no offsets and inflates the data.
Where: `crates/proto/src/lib.rs` (`FrameKind`, `HEADER_LEN`), `crates/daemon/src/pane.rs`.
From: [Decisions table](docs/plan-archive.md#decisions-on-the-briefs-open-questions), M1, M2.

### The server sends whole layouts, and deltas for summaries
`State` carries the full tree and a `rev` on every layout change (trees are
small). Pane summaries and attention, which change often and across many
hosts, go as field-level `Delta`s.
Where: `crates/proto/src/lib.rs` (`ServerMsg::State`, `Delta`).
From: [M23](docs/plan-archive.md#m23-pane-summaries), S16 (in the [spikes list](docs/plan-archive.md#s16-swarm-spike-summary-cost-fleet-connections-canvas)).

### Wire snapshots are formatter VT bytes plus fix-ups, zstd-compressed on request
On attach or resync a client gets VT bytes that rebuild the pane in a fresh
terminal, because xterm.js needs VT bytes. libghostty's formatter writes most
of them; `wire.rs` fixes what it gets wrong (cursor, saved cursor, blank-cell
colours, hyperlinks, Kitty keyboard flags, alt-screen order). A client that
asks gets them as `SnapshotZstd`, with scrollback capped at what it keeps
(`history`). After a resync a client asks again with `history: 0`: the screen
alone, with a visible "output skipped" rule, so a flood can't keep it
resyncing. Visible-first attach was measured (S10) and is not built.
Where: `crates/vt/src/ghostty/wire.rs`, `crates/proto/src/lib.rs` (`AttachPane`), `crates/daemon/src/pane.rs`.
From: [S1/S5 follow-ups](docs/plan-archive.md#s-spikes-each-about-half-a-day-before-the-milestone-named), #1, #49, #53.

### Flow control: acks, a window, and a log that absorbs slow clients
A client that attaches with `acks` sends `ack{pane, offset}` about every 64 KB
and is held to a 512 KB unacked window; past it, it is held back and catches
up from the log (or gets `resync` if the log no longer has the gap). A slow
client never pauses the program: the log absorbs its output. The program is
paused only when the daemon itself falls behind, through a bounded queue (64
chunks), and client requests are served before that queue, so Ctrl-C stops a
flood at once. Clients that don't ack (`attach`, the tmux front end, the share
viewer) get `resync` on a full queue.
Where: `crates/daemon/src/pane.rs` (`ACK_WINDOW`, `PROGRAM_QUEUE`).
From: [Protocol, flow control](docs/plan-archive.md#protocol-one-protocol-two-transports), #52.

### Size: a tab has one size, set by its owner
The client that last claimed a tab (it was opened, or the person typed or
focused there) sets its cols and rows, and the PTYs follow. Other viewers
render at the real size, letterboxed on the desktop and scaled on the phone.
The size is never smaller than the split tree needs. The plan first said "per
pane"; the code is per tab.
Why: one person moving between devices; "smallest wins" would make the desktop
suffer whenever the phone is open.
Where: `crates/core/src/mux.rs` (`Mux::view`), `crates/proto/src/lib.rs` (`ClientMsg::View`).
From: [Size arbitration](docs/plan-archive.md#size-arbitration), [M1](docs/plan-archive.md#m1-multiplexer), [M5](docs/plan-archive.md#m5-tmux-control-mode--cc-front-end).

### TypeScript types are generated from `proto`
`web/src/proto.gen.ts` comes from the Rust types (`ts-rs`, `just proto-ts`),
and CI fails when it is stale. The hand-written `web/src/proto.ts` is being
retired.
Where: `crates/proto/src/ts.rs`, `justfile` (`proto-ts`), `web/src/proto.gen.ts`.
From: #387 (the arch tracker), #396.

## State and durability

### The log is the truth; checkpoints are a cache
A pane's history is raw output in 4 MB segments with a sidecar index
(resizes, restores). A checkpoint (GHOSTSNP, zstd, tagged with the engine
that wrote it) speeds up restore, but one that doesn't decode is discarded
and the log tail replayed, because the format changed incompatibly once
without a version bump. Retention is 256 MB a pane.
Where: `crates/daemon/src/store.rs` (`RETAIN_BYTES`, `PaneLog`), `crates/vt/src/ghostty.rs`.
From: [S5](docs/plan-archive.md#s-spikes-each-about-half-a-day-before-the-milestone-named), [M2](docs/plan-archive.md#m2-durability).

### The state dir holds secrets: 0700 and 0600
Logs and checkpoints hold what was typed or echoed, tokens included. The state
dir is `0700` and its files `0600`; retention is enforced;
`illogical purge %p` deletes a pane's history. Logs synced from other hosts
are sealed with a key only the home daemon holds.
Where: `crates/daemon/src/store.rs`, `crates/daemon/src/perm.rs`, `crates/daemon/src/seal.rs`.
From: [M2](docs/plan-archive.md#m2-durability), [M4](docs/plan-archive.md#m4-reach-a-shell-on-any-machine-or-sandbox) (M4c).

### Layout is a file written atomically
`layout.json` holds sessions, tabs, splits, machines and every block's config.
It is written to a temp file, renamed, and the directory fsynced, debounced
to 250 ms, with a schema version. Grants (`acl.json`) are written the same way.
Where: `crates/daemon/src/store.rs`, `crates/daemon/src/acl.rs`.
From: [M2](docs/plan-archive.md#m2-durability), M12.

### Restart policies decide what runs after a restore
Scrollback always comes back. A pane's policy decides what runs in it: `none`
(press Enter for a shell), `shell` (the default: a login shell in the last
OSC 7 directory), `rerun` (the foreground command, asking first by default),
`hook` (a stored command), `resume` (the agent conversation that was running:
`claude --resume <id>`; the default for a pane running Claude Code). Restoring
feeds a fresh engine the checkpoint and log tail, then writes a reset and a
dim "restored" rule before the new process starts.
Where: `crates/proto/src/lib.rs` (`Policy`), `crates/daemon/src/resume.rs`, `crates/daemon/src/store.rs`.
From: [Restart policies](docs/plan-archive.md#restart-policies-m2), [Restoring scrollback](docs/plan-archive.md#restoring-scrollback), #146.

### Panes outlive the daemon
Each pane's program runs under a shim, so the daemon can be restarted or
upgraded without ending shells. On Linux with systemd the PTY master waits in
the FD store and each pane has its own transient scope; elsewhere the shim
holds the master and lends it to whichever daemon connects, ending the pane
after a grace period if none does. Stopping the daemon still ends its panes.
The shim writes the exit status because a restarted daemon is not the
program's parent. On Windows the shim is a pty host (ConPTY).
Where: `crates/daemon/src/shim.rs`, `crates/daemon/src/holder.rs`, `crates/daemon/src/sys.rs`, `crates/daemon/src/conpty.rs`, `crates/daemon/src/install.rs`.
From: [M2b](docs/plan-archive.md#m2b-in-place-daemon-upgrade-start-of-daily-use), S3, #35, [M58](docs/plan-archive.md#m58-panes-survive-daemon-restarts-on-windows-221).

## The mux and panes

### One task decides the order of every change
The mux task owns the layout, the panes and the connected clients; every
client message, API call and pane notice goes through it. A pane's VT thread
owns libghostty's terminal (it is `!Send`), and PTY output, attaches, resizes
and exits arrive on one channel, so a snapshot and the live output after it
are always in order and the log sees the same bytes.
Where: `crates/daemon/src/mux/mod.rs`, `crates/daemon/src/pane.rs`.
From: [Cargo workspace](docs/plan-archive.md#cargo-workspace), M1.

### The model is pure: `core` has no I/O
Sessions hold tabs, tabs hold a split tree of panes. Clients change them by
sending intents, applied by pure functions in `crates/core`, and the tree
stays valid (ratios sum to 1, no empty splits). IDs follow tmux (`$session`,
`@tab`, `%pane`) and are never reused. Ratios are stored; cells are derived.
There are no local-only panes: every terminal belongs to some daemon.
Where: `crates/core/src/mux.rs`, `crates/core/src/layout.rs`, `crates/core/src/tree.rs`.
From: [Architecture](docs/plan-archive.md#architecture), [M1](docs/plan-archive.md#m1-multiplexer).

### The terminal engine sits behind `VtEngine`
libghostty-vt is the only engine, behind a trait (`feed`, `resize`,
`snapshot`, …) so the daemon never touches Ghostty types directly. The Ghostty
commit is pinned.
Why: it ships a formatter that turns state back into VT bytes and keeps more
state than xterm's serializer.
Where: `crates/vt/src/lib.rs`, `crates/vt/src/ghostty.rs`.
From: [Decisions table](docs/plan-archive.md#decisions-on-the-briefs-open-questions), S1.

### A block is a leaf with a type; a terminal is the first type
Every leaf of the tree is a block with a `type`. A terminal keeps its own fast
path (PTY bytes, offsets, snapshots). Every other type implements `Block`:
config saved in `layout.json`, JSON state pushed to clients, attention through
the same notices as terminals (so badges, "needs you" and push work for all of
them), a text rendering for `capture`/history/search, and methods. A pane
(`%N`) is a terminal block; OSC 133 command ranges are *command marks*, never
"blocks".
Where: `crates/daemon/src/block.rs`, `crates/daemon/src/pane.rs`, `crates/daemon/src/mux/blocks.rs`.
From: [M6](docs/plan-archive.md#m6-non-terminal-blocks-after-m4b-m5-is-independent-of-it), [Architecture](docs/plan-archive.md#architecture).

### Layout belongs to a host; a remote block references a pane elsewhere
Each daemon owns its own tree. A tab can hold a `remote` block, `{host,
pane}` and nothing else, whose terminal, size, policy and history stay on the
other daemon. The client asks the other host for the pane and then records
the reference here; the daemon never talks to the other one on a client's
behalf, and closing the reference closes only the reference.
Where: `crates/daemon/src/remote.rs`.
From: [M4](docs/plan-archive.md#m4-reach-a-shell-on-any-machine-or-sandbox), #17.

### Shell integration is injected the way Ghostty does it
bash `ENV`, zsh `ZDOTDIR` and fish `XDG_DATA_DIRS` load scripts that report
prompts, commands, exit codes and the directory (OSC 133, 7, 633), without
touching the user's dotfiles; a pane can switch it off.
Where: `crates/daemon/src/shellint.rs`, `crates/daemon/src/osc.rs`.
From: [M3](docs/plan-archive.md#m3-structure-and-cli).

## Access and identity

### One function says what an intent needs
Every request has an author and every session an access list. Roles per
session are `viewer`, `editor` and `owner`; the daemon's owner owns every
session. `illogical_core::access::need` is the one decision function, and the
daemon enforces it on every path: the WebSocket (in the mux, per message), the
HTTP API (`authz.rs`), the Unix socket, the channel and the tmux front end.
The unit of sharing is the session; tabs, blocks and machines inherit it.
Grants are data (`acl.json`) with an audit log (`audit.jsonl`).
Where: `crates/core/src/access.rs`, `crates/daemon/src/acl.rs`, `crates/daemon/src/authz.rs`, `crates/daemon/src/mux/who_may.rs`.
From: [M12](docs/plan-archive.md#m12-principals-and-roles), [Multiplayer track](docs/plan-archive.md#multiplayer-track-m12m15-added-2026-10-01).

### Who can reach a daemon
The daemon listens on loopback; `tailscale serve` adds `Tailscale-User-Login`
and strips any copy a client sends, and direct tailnet connections are
identified by asking tailscaled (WhoIs). Host must be a loopback or tailnet
name (DNS rebinding). The Unix socket maps the peer's uid to the owner. A
device of the control account the daemon joined is the owner too.
Where: `crates/daemon/src/access.rs`, `crates/daemon/src/tailscale.rs`, `crates/daemon/src/localauth.rs`.
From: [Architecture](docs/plan-archive.md#architecture), [M4](docs/plan-archive.md#m4-reach-a-shell-on-any-machine-or-sandbox), S2.

### A guest types on your machine only by trust, or in a VM
Write access to a local shell is code execution as your uid. A guest's new
panes run in a VM (or join the tab's), and driving one of the owner's local
panes needs a per-pane, time-limited trust grant. Anything that types into a
pane, including uploads, checks both the role and `MayDrive`; a new route that
types must be added to that check by path.
Where: `crates/daemon/src/authz.rs` (`Api::MayDrive`), `crates/daemon/src/mux/who_may.rs`, `crates/daemon/src/upload.rs`.
From: [M14](docs/plan-archive.md#m14-safe-write-access), [M70](docs/plan-archive.md#m70-images-into-terminal-panes-249).

### Only the owner's agent reaches panes over ssh
The fixed `SSH_AUTH_SOCK` path links only to an agent forwarded by the box's
owner. A guest's forwarded agent is never used in the owner's panes, so `git
push` doesn't depend on who attached last.
Where: `crates/cli/src/ssh.rs` (the agent link), `crates/daemon/src/guest_ssh.rs`.
From: [SSH track](docs/plan-archive.md#ssh-track-s28-m51m53-added-2026-10-04), [M65](docs/plan-archive.md#m65-a-pane-for-a-guest-who-has-only-openssh-198-decided-2026-10-04).

## Clients

### The daemon serves the web client, on purpose
`web/dist` is embedded in the daemon (`rust-embed`), so any browser works
against any machine and there is one artifact to install. The price is that a
UI change needs a daemon release. This is not on the list to change; don't
split the web client out by accident.
Where: `crates/daemon/src/server.rs`, `web/`.
From: [Cargo workspace](docs/plan-archive.md#cargo-workspace), #387 (point 4).

### No client links daemon code
`cli` (TUI, tmux front end), `desktop` and `web/` speak the wire through
`illogical-proto`. `core` has no internal dependencies, `proto` depends only
on `core`, `vt` and `e2e` stand alone, and `control` shares only `e2e`.
Where: `Cargo.toml`, `crates/*/Cargo.toml`.
From: #387.

### Only the web client and the desktop app get new features
The TUI (`illogical tui`) and the tmux `-CC` front end are kept working and
tested, but they are frozen: no new features there.
Why: every feature in the web client lands once instead of three times.
Where: `crates/cli/src/tui/`, `crates/cli/src/tmux/`, `web/`, `crates/desktop/`.
From: #387; [S26](docs/plan-archive.md#s26-how-native-can-it-get-141), [M31](docs/plan-archive.md#m31-illogical-tui), [M5](docs/plan-archive.md#m5-tmux-control-mode--cc-front-end).

### The web client draws terminals through one file
xterm.js lives behind `terminal-view.ts` (WebGL on visible panes, DOM for the
rest) so the renderer can be swapped later (M8, gated, not built). Layout
changes are intents sent to the daemon and never applied locally.
Where: `web/src/terminal-view.ts`.
From: [M1](docs/plan-archive.md#m1-multiplexer), [M8](docs/plan-archive.md#m8-client-terminal-engine-ghostty-web-and-local-echo).

### The desktop app is a window and a supervisor; the daemon stays a service
The app (Tauri 2) loads the UI from the local daemon, bundles `illogicald` and
`illogical`, registers the daemon as a login service (SMAppService on macOS)
and upgrades it in place. Sessions outlive the window: if the app owned the
PTYs, quitting it would end every pane. `desktop` depends only on `proto`.
Where: `crates/desktop/src/main.rs`, `crates/desktop/src/service.rs`.
From: [Desktop track](docs/plan-archive.md#desktop-track-s25-m46m48-added-2026-10-04), [S26](docs/plan-archive.md#s26-how-native-can-it-get-141).

### Clients run ssh; daemons don't
The CLI, the TUI and the desktop app run the system `ssh` themselves, where
the user's agent and prompts are. A daemon never relays ssh (that would make
it a hub and put the user's ssh credentials in a background service). ssh is
used to install and join a box to control; control reaches it after that.
Only the target `user@box` is stored.
Where: `crates/cli/src/ssh.rs`, `crates/cli/src/hosts.rs`.
From: [SSH track](docs/plan-archive.md#ssh-track-s28-m51m53-added-2026-10-04), M51, M52.

## Agents and blocks

### An agent block is an ACP client
It speaks the Agent Client Protocol (JSON-RPC on the agent's stdio) to
whatever agent server the block names (Claude Code through
`claude-agent-acp`, Codex, Fountain, any ACP command). One protocol for every
agent, so no per-agent adapter. A permission request puts the block in
`needs-input`; an unanswered question or approval waits indefinitely. "Always"
rules are kept by the daemon in `rules.json`, per machine, never in Claude
Code's settings.
Where: `crates/daemon/src/agent/mod.rs`, `crates/daemon/src/rules.rs`.
From: [M6b](docs/plan-archive.md#m6b-agent-blocks-acp-clients), S7, #166.

### Attention comes from signals, hooks and a fallback
Each pane is `idle | working | needs-input | done`, from notification
sequences (OSC 9/777/99, BEL), Claude Code hooks (`illogical hook`, which also
turns a permission request into an approval card), and a quiet-output
heuristic. Every answer has an author. Web Push goes to the phone when no
client is focused on the pane.
Where: `crates/daemon/src/mux/attention.rs`, `crates/cli/src/hook.rs`, `crates/daemon/src/push.rs`.
From: [M3](docs/plan-archive.md#m3-structure-and-cli), [M29](docs/plan-archive.md#m29-team-answers), M24.

### MCP is served by the daemon, with the API's own checks
`/mcp` (Streamable HTTP) is part of the owner's API: the owner over the
socket (`illogical mcp` bridges stdio), loopback or the tailnet, or per-client
bearer tokens that can be revoked. Tools cover everything the CLI does; the
MCP client's own tool permissions are the guard, so tool annotations matter.
Agent blocks get it injected, scoped to their tab. Every tool answers by name
whether or not it is listed.
Where: `crates/daemon/src/mcp/mod.rs`, `crates/daemon/src/mcp/tools.rs`, `crates/daemon/src/mcp/tokens.rs`.
From: [M16](docs/plan-archive.md#m16-an-mcp-server-illogical-as-tools-for-any-agent), S14.

### Each browser block gets its own origin
A dev server in a machine runs code an agent wrote. Block 42 is served at
`b-42.<domain>`, at `/`, so blocks can't read each other or the app, and dev
servers need no base path. Through control the same origin comes from control
with no DNS or certificate on the machine.
Where: `crates/daemon/src/sites.rs`, `crates/daemon/src/browser.rs`.
From: [M6a](docs/plan-archive.md#m6a-browser-blocks), [M50](docs/plan-archive.md#m50-blocks-through-control-150-after-s27).

### Forge blocks use the person's own CLI, and agents draft while people send
One `forge` block type with a provider adapter (GitHub through `gh`, Forgejo
through `tea`, GitLab through `glab`); illogical stores no forge tokens. An
agent's write (comment, approve, merge, close) becomes a draft on the block;
the owner or an editor sends it, as themselves. Freshness is conditional
polling, with webhooks relayed by control's GitHub App. Launch ships GitHub PR
blocks only; the other providers move into Labs (#457, not yet built).
Where: `crates/daemon/src/forge/`.
From: [Forge track](docs/plan-archive.md#forge-track-s23-m36m40-added-2026-10-02), [M36](docs/plan-archive.md#m36-forgejo-pull-request-blocks-88), #457.

### Uploads land in a per-pane folder on the pane's host
A file from any client goes to `illogical-uploads/<pane>/` under
`$XDG_RUNTIME_DIR` (else `$TMPDIR`, else `/tmp/illogical-<uid>`), mode 0700,
files `O_EXCL` 0600, named by the daemon, swept after 24 h, capped at 20 MB a
file and 200 MB a host. The daemon pastes the path into the pane. The state
dir is never used: it holds secrets and a VM pane can't see it.
Where: `crates/daemon/src/upload.rs`, `web/src/upload.ts`.
From: [Images track](docs/plan-archive.md#images-track-s32-m70m72-added-2026-10-05), [M70](docs/plan-archive.md#m70-images-into-terminal-panes-249).

### Threads live on the daemon that owns the pane
Pane and session threads are kept in `<state>/threads/`, outside the pane's
own directory (closing a pane keeps its conversation), with the pane's access
rules. Team channels are ciphertext in control (MLS), and voice is WebRTC on a
session, signaled through the session's daemon, with control only handing out
short-lived TURN credentials.
Where: `crates/daemon/src/threads.rs`, `crates/daemon/src/calls.rs`.
From: [Talk track](docs/plan-archive.md#talk-track-s30-m61m64-added-2026-10-05), [M61](docs/plan-archive.md#m61-threads-on-panes-and-sessions-240).

## Federation and control

### Every daemon is a peer; control is the only front door
A daemon owns its terminals and serves the page, the protocol and the CLI.
Control's page lists every joined machine, direct or relayed; a daemon's own
page shows that daemon, plus its tailnet `hosts.json` for setups without
control. No daemon is a hub: a joined daemon doesn't list or relay for the
account's other machines. Terminal bytes go between a client and the daemon
that owns the terminal.
Where: `crates/daemon/src/hosts.rs`, `crates/daemon/src/control.rs`, `crates/control/src/api.rs`.
From: [M4](docs/plan-archive.md#m4-reach-a-shell-on-any-machine-or-sandbox), [No special machines track](docs/plan-archive.md#no-special-machines-track-s27-m49m50-added-2026-10-04), [M49](docs/plan-archive.md#m49-the-cli-and-the-daemon-page-without-a-hub-149).

### Control can refuse service but can't read
Terminal output, input, scrollback, snapshots and push payloads are end to end
encrypted; control sees metadata only (who, which host, when, sizes, names).
Every client-to-daemon stream through control is one Noise channel
(`Noise_IK_25519_AESGCM_SHA256`), on the relay and on direct paths from
control's page, with one channel per viewer so revoking closes it. A page a
daemon serves itself still uses plain `/ws`, inside the tailnet's WireGuard.
Where: `crates/e2e/src/channel.rs`, `crates/control/src/relay.rs`, `crates/daemon/src/e2e.rs`.
From: [Control track](docs/plan-archive.md#control-track-s15-m17m22-added-2026-10-01), [M18](docs/plan-archive.md#m18-relay-and-end-to-end-encryption), docs/control-e2e.md.

### Trust is a chain of device approvals
A new device must be approved by a device the account already trusts (the
first is trusted on enrollment). Daemons verify the chain themselves back to
the account's pinned root, so control can't add a reader; team rosters are
signed by an owner's device of the previous version.
Where: `crates/e2e/src/cert.rs`, `crates/e2e/src/team.rs`.
From: [M17](docs/plan-archive.md#m17-illogical-control-accounts-devices-enrollment-directory), [M19](docs/plan-archive.md#m19-teams-sharing-roles-team-daemons-invites), S15.

### Control is open source and self-hostable; the tailnet stays first class
`illogical-control` lives in this repo and the hosted one runs the same code.
Tailnet users can skip control, or enroll and keep direct connections. Free
for one person, per seat for teams, sandboxes by usage.
Where: `crates/control/`, `packaging/control/`.
From: [Control track](docs/plan-archive.md#control-track-s15-m17m22-added-2026-10-01), M22.

## Build and release

### One switch for what a stranger doesn't get: the `labs` file
A file named `labs` in a machine's state dir turns on huddles and chat,
Fountain, studio apps and chant workspaces, VMs and sandboxes, guest ssh and
the swarm's extra views. Present means on, whatever it holds; it is a `stat`
on every read, so no restart is needed. It is a file, not an environment
variable, because `shellenv::clean()` drops every `ILLOGICAL_*` a shell
prints and the desktop app starts the daemon from a fixed plist. Labs is
per machine, visibility rather than enforcement (a machine without it still
answers the routes), and `HostFeatures.labs` tells the page. Per-feature
flags come after launch (#347); a Labs cargo feature is planned (#452).
Where: `crates/proto/src/hosts.rs` (`labs`, `LABS_FILE`, `HostFeatures`), `crates/daemon/src/hosts.rs`, `crates/daemon/src/args.rs` (`labs_command`), `crates/daemon/src/mcp/mod.rs`, `crates/cli/src/main.rs`.
From: #385, #342, #347, #387.

### Zig 0.16 builds Ghostty; the toolchain is pinned
libghostty-vt needs Zig, pinned in `.mise.toml`; cargo runs under `mise exec`
(the `just` recipes do it), and the daemon embeds `web/dist`, so `just web`
comes before any cargo build.
Where: `.mise.toml`, `justfile`, `rust-toolchain.toml`.
From: [S5](docs/plan-archive.md#s-spikes-each-about-half-a-day-before-the-milestone-named), M2.

### Windows is a full platform, built on GitHub's runners
The daemon runs panes on Windows (ConPTY, named pipes, a logon task), not just
drives them. Unix-only code stays behind `cfg` gates, and Windows CI runs
clippy and tests on every change (`illogical-control` is excluded).
Where: `crates/daemon/src/conpty.rs`, `crates/daemon/src/pipe.rs`, `.github/workflows/windows.yml`.
From: [Windows track](docs/plan-archive.md#windows-track-s29-m54m60-added-2026-10-05), M55-M60.
