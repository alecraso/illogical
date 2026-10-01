# illogical

A personal multiplexer built around durable sessions: a daemon owns the
terminals, and mouse-first clients attach to them.

Start with [BRIEF.md](BRIEF.md), then [PLAN.md](PLAN.md) (decisions and
milestones) and [docs/research.md](docs/research.md).

## Status: M7 (files and navigation) works, on top of M4c (sandboxes, resident daemons, dial-out, synced history)

Everything from M1 (sessions, tabs and splits held by the daemon, driven by
the mouse, the same live on every window and a phone), and now it survives
the daemon stopping, crashing, or the machine rebooting:

- Every pane's output goes to an append-only log as it happens, and its
  terminal is checkpointed (Ghostty's snapshot format, zstd) after 5s idle
  or every 2 MB. The layout is saved on every change.
- On start, each pane is rebuilt from its checkpoint plus the log after it,
  marked `── restored <time> ──`, and then does what its restart policy says
  (right-click a pane → *After a restart*): start a shell in its last
  directory (default), re-run its last command (asking first, or not), run a
  command you choose (e.g. `claude --continue`), or wait for Enter.
- A shell killed by a signal (OOM, `kill -9`) leaves its pane and scrollback
  in place and offers a new shell. Only an ordinary `exit` closes a pane.
- *Forget history* deletes a pane's saved output and clears its screen.
  History is kept to 256 MB per pane, in `~/.local/state/illogical`, which
  is private to you (0700/0600).

- **Restarting the daemon doesn't touch running programs** (M2b). Each pane's
  program runs in its own systemd scope behind a small shim, and its terminal
  is kept in systemd's FD store while the daemon is gone. A restarted (or
  crashed and auto-restarted) daemon adopts every live pane: vim keeps its
  screen, a build keeps building, output from the gap is read from the
  terminal, and open windows reconnect on their own. `just install` upgrades
  in place. `systemctl --user stop` is still the end of the panes, like a
  reboot.

- **Panes know about commands** (M3). bash gets shell integration
  automatically (the way Ghostty does it: no dotfile changes), so each pane
  knows where every command starts and ends, its exit code and its directory.
  In the browser each finished command gets a mark in the gutter (green or
  red); click it to select the output, right-click to copy it or run it
  again. zsh and fish scripts are included but untested.
- **`illogical`, a CLI for scripts and agents**, works from any shell and
  from inside every pane (`ILLOGICAL_PANE` and `ILLOGICAL_SOCK` are set and
  it's on `PATH`). See below.
- **Attention.** A pane that rings the bell, sends a notification (OSC 9,
  777, 99), has an agent go quiet mid-command, or is told by a hook, shows a
  badge on its tab and pane (and in a "Needs you" list on the phone). A long
  command finishing while you're elsewhere shows "done". With *Notify this
  device* on (session menu), the phone gets a push notification; tapping it
  opens the pane.
- **History.** Closed panes' output is kept for 7 days, and `illogical
  history` / `search` look across all panes.
- **VM tabs and panes** (M3b, M3c). *New VM tab* (the `+` button's
  right-click, the tab and session menus, the phone's sheet) or `illogical
  run --vm-tab` opens a tab with its own throwaway Firecracker microVM, a
  wisp sprite on this host. Splits in it join the VM, so a shell and
  `claude` side by side see the same files; *Split (local)* adds a shell on
  this host instead (badged `local`). Panes on the tab's VM can't be
  dragged out of it (local ones can). The tab's menu can start a new pane
  on the VM or reset it (delete and recreate it; the panes restart by
  policy), and closing the tab deletes it. *New VM pane on the right* or
  `illogical run --vm` gives one pane a VM of its own, deleted with the
  pane; *Share machine with tab* hands it to the tab. It's for agents and
  untrusted builds.
  Its shell gets the same integration (marks, `wait`, history), `process`
  asks the VM, and the output is logged here, so `illogical tail` still has
  the session after the VM is gone. Restarting the daemon reattaches to the
  VM's shell without losing or repeating output. If the VM is deleted from
  under a pane, Enter starts a new one; after a reboot, each VM comes back
  as one fresh VM (one per tab, not per pane) and its panes restore by
  policy. Needs wispd's token at `~/.local/share/wisp/token`
  (`--wisp-url`, `--wisp-token-file`); without it VM panes are off. The
  base image is plain Ubuntu 24.04: install what you need, e.g. Claude Code
  with `curl -fsSL https://claude.ai/install.sh | bash`.

- **Blocks** (M6, in progress). A pane is one kind of block; every kind
  shares the layout, ids, attention, `describe` and `call`. The first other
  kind is a browser block for ordinary pages: *Open a web page…* in the pane
  menu, or `illogical open example.com`. Sites that refuse to be framed get
  a card with "open in new tab". Block directories are `blocks/<id>/` in
  the state directory (`panes/` before; it's moved, and left as a link).

- **Browser blocks on ports** (M6a). Run `npm run dev` in a VM tab, then
  *Open a port on this machine…* (pane menu), *Open a port on machine…*
  (tab menu), *Open port* (phone sheet) or `illogical open --split right
  :5173` puts the app beside it, hot reload and all. A block opened from a
  pane shows that pane's machine's port (or this host's). Each block is
  served on an origin of its own, `https://b-<id>.illogical.widgets.wtf:7443`
  on geek, by the daemon, which proxies it to the port (through the Sprites
  proxy for a VM). The dev server needs no config: the proxy rewrites
  `Host` and `Origin` to `localhost:<port>`, and since that switches off the
  server's own guards, the proxy enforces its own: only you (asked of
  tailscaled), only the block's own origin, nothing cross-site but page
  loads, framed only by the app. It strips `Tailscale-*` headers, so agent
  code never learns who you are, and a script in the page can't reach
  illogical: the app refuses every origin but its own. The block follows
  the frame's navigations; when the server dies it asks for you and shows
  the page again when the server is back. Events: `navigated`,
  `load_error`.
- **Agent blocks** (M6b). An agent run as UI instead of a TUI: messages,
  thoughts, tool-call cards with each command's output in a read-only
  terminal, permission requests as Approve / Always / Deny cards (big
  enough for a thumb, and actions on the push notification), a composer,
  Stop, and cost per turn. The block is an
  [ACP](https://agentclientprotocol.com) client, so one block type drives
  Claude Code (`claude-agent-acp`), Codex (`codex-acp`), a Fountain agent
  (`fountain acp`, in Fountain's sandbox) or any ACP agent server. Start
  one from *Start an agent…* in the pane menu, *New agent* in the phone's
  sheet, or `illogical agent`.
  - *Always* is remembered by the block (in its config) and answered by
    it; it never picks the agent's own "always", which would write
    `.claude/settings.local.json` into your repo. Claude Code runs with no
    settings sources, so your own hooks don't fire inside it.
  - The block's log is the JSON-RPC stream; `capture` is the transcript as
    Markdown, `history` lists the agent's commands and turns, `search`
    covers what agents said and ran.
  - A local agent server runs in its own scope with its pipes in systemd's
    FD store, so restarting the daemon mid-turn (even with an approval
    open) doesn't touch it. After a reboot the session reopens with
    `session/resume` (or `session/load`), unless the policy is `none` or
    `rerun-ask` (then *Resume*). A Fountain turn that ran on while nothing
    followed it shows "running on Fountain" and appears when it ends.
  - The adapters, pinned, go in `~/.local/share/illogical/agents/`:
    `npm install --prefix ~/.local/share/illogical/agents/claude @agentclientprotocol/claude-agent-acp@0.81.2`
    and `npm install --omit=optional --prefix ~/.local/share/illogical/agents/codex @agentclientprotocol/codex-acp@2.1.0`
    (Codex uses `~/.local/bin/codex`). They need Node on PATH (mise's
    shims are added if present).
  - **In a VM** (`--vm`, or the dialog's checkbox) the agent server runs
    over a non-TTY exec on the block's own machine; its first start
    installs Node and the adapter there (about 15s). Claude Code there
    needs credentials: a token from `claude setup-token` in
    `~/.config/illogical/claude-oauth-token` (given to it as
    `CLAUDE_CODE_OAUTH_TOKEN`), or an API key in
    `~/.config/illogical/anthropic-key` (`ANTHROPIC_API_KEY`, used first);
    `--claude-token-file` and `--anthropic-key-file` move them. They reach
    the agent on its stdin, into its environment only: never the VM's
    disk, a URL, an argv, the log or the layout. A VM agent survives a
    daemon restart (its exec session lives on wisp).

- **Other hosts** (M4a). Every daemon is a peer; the one the page comes
  from (geek's, the "home daemon") keeps a list of the others and checks on
  each every minute. The page shows a host switcher (desktop: the bar's
  left end; phone: the sheet), and each host has its own sessions and tabs.
  Switching connects straight to that daemon; nothing is relayed, and the
  host you left gets no connection (so a sandbox can sleep). The list is
  remembered in the browser, and the page itself by its service worker, so
  the other hosts stay reachable while the home daemon is down.
  `illogical --host NAME …` runs any command on another host. A sandbox
  (a sprite, a container: no systemd needed) gets a static daemon on the
  tailnet with one command and adds itself to the list; see *Use it*.
- **Hosts that can only dial out** (M4c). A sandbox that allows nothing
  in but outbound HTTPS runs `illogicald --peer wss://geek.… --token FILE`:
  it keeps one WebSocket open to the home daemon and serves its own
  WebSocket and API over it, many streams at once. The home daemon lists it
  (`dial_out`) and answers for it at `/h/NAME/…`, behind its own access
  checks, so the page's host switcher and `illogical --host NAME` work as
  for any host. It's not a hub: only the home daemon opens streams, the
  host serves nothing that leads elsewhere, and what it answers is served
  defanged (no cookies or CORS, `nosniff`, a sandboxing CSP), since it
  lands on the home daemon's origin. It redials with backoff and works on
  its own meanwhile. The token is per host, minted by the home daemon
  (`illogical hosts token NAME`, or joining with an invite), stored only
  as a hash, good for that host alone, and revocable (`hosts revoke`,
  `hosts rm`).
- **Read-only share links** (M4c). `illogical share %N --ttl 1h`, or *Share
  read-only link…* on a pane, gives a `/share/…` link that shows that pane
  live (its screen and scrollback, then its output) and nothing else: no
  typing, sizes, other panes or API, and a viewer that sends anything is
  hung up on. Any tailnet user may open one (someone the node is shared
  with, say), never a tagged node, Funnel or the internet. Links expire (a
  week at most), are listed (`illogical shares`) and revocable (`shares
  revoke ID`), which cuts off anyone watching.
- **History that outlives a sandbox** (M4c). With `--sync` (closed panes)
  or `--sync-live` (open ones too), a host pushes its panes' log segments
  and indexes to the home daemon with its token, resuming from what is
  already there. The home daemon keeps them encrypted at rest and answers
  `illogical history|search|tail --synced NAME` (or `--host NAME`, once the
  host is gone) from them. Kept 256 MB per pane, 30 days after the last
  push. Encryption: each file is AES-256-GCM records under its own key
  (HKDF from a key ring only the home daemon holds, `<state>/synced/key`,
  0600, or `--sync-key-file`; salted per file, bound to the file's place),
  with counter nonces and the header and record number as associated data.
  `illogical synced rotate-key` re-encrypts everything under a new key and
  drops the old one. File names and sizes aren't secret; contents are.

- **Sandboxes** (M4b). *Sandboxes…* (session menu; *Sandboxes* in the
  phone's sheet) or `illogical sandboxes` lists the home daemon's provider's
  sandboxes (wisp sprites here; Fly's Sprites API fits the same adapter)
  with their state, asked of the provider, which doesn't wake them.
  - *Shell* (`illogical run --sandbox NAME`) opens a pane here whose
    terminal is a plain exec on that sandbox: nothing is installed there.
    It's disposable, and badged so: the output is logged here while it's
    attached, but the provider keeps only its replay buffer while nothing
    follows it (1 MB on wisp, about 6.5 KB on Fly), and closing the pane
    hangs the shell up and leaves the sandbox alone.
  - *Make resident* (`illogical sandboxes promote NAME [--as HOST]`) copies
    the static daemon in (`just static`; the home daemon finds it in
    `--static-dir`, default `~/.local/share/illogical/static`) and registers
    it as a sprite *service*, so it starts on every boot and restarts if it
    exits. It becomes a host in the list, a *provider* host: the client and
    `--host` reach it through the home daemon's **provider tunnel**
    (`/tunnel/HOST/…`, through the Sprites proxy to its port, never a
    public URL). Connecting wakes it; the page lets go of it 10s after it's
    hidden, so it can sleep. After it goes cold (on wisp, a real reboot)
    its daemon restores its layout and scrollback from its own disk, the
    way a reboot does here. A provider host with a tailnet URL too is
    switched to the tailnet if that answers within 5s of the wake.
    `illogical sandboxes demote NAME` stops it.
  - **Identity.** The tunnel is for callers the home daemon already let in
    (the owner, or its Unix socket). It strips their identity headers and
    presents a token it minted for that host when it made it resident; the
    resident daemon keeps only the token's SHA-256 (in its arguments) and
    refuses every loopback connection without it, so other programs in the
    sandbox can't use the provider's proxy path to it. These provider
    tunnel tokens (`ilp_…`, home → host) stay in the home daemon's
    `provider-tokens.json`, never in the host list clients get; dial-out
    host tokens (`ilh_…`, host → home, M4c) are the other direction.
    `hosts revoke` and `hosts rm` drop whatever a host has of both.
  - Like a dial-out host's (`/h/NAME`), only the WebSocket and the API go
    through `/tunnel/NAME`, and the answers are defanged: they're served on
    the home daemon's origin and the sandbox runs untrusted code.

- **Files and navigation** (M7). *Go to directory…* (a pane's menu; *In a
  directory…* on the `+` button's right-click; *Go to directory* in the
  phone's sheet, where it's a full-screen sheet; Ctrl+Shift+G) browses
  directories on the host the pane runs on: this daemon's, another host's
  (it answers for itself), or its VM's (through the provider). It starts
  where the pane is (OSC 7), lists directories used lately there first,
  and filters fuzzily as you type (`/…` or `~…` goes to a path; Backspace
  goes up). Then *New pane here* (on the same host: a VM tab's machine, a
  sandbox's shell), *New tab here* (not for a VM's own directories: they
  exist only in its tab), or *cd there*, which types `cd` into the shell
  only while it waits at its prompt (shell integration says so) and says
  why not otherwise.
  - The `fs` methods behind it (`/api/fs/list|stat|read|watch|recent`,
    `illogical fs`) are read-only and part of the owner's API: share-link
    viewers and host tokens never reach them. On a daemon's host they read
    as the daemon's user, so the OS's permissions are the limit, and they
    also refuse `/proc`, `/sys`, `/dev`, the daemon's state directory and
    the secrets it knows of (the wisp token, agent credentials); paths are
    resolved first and an opened file is checked again through
    `/proc/self/fd`, so no symlink (or one swapped in mid-open) gets
    around that. On a machine the provider's agent reads as the sandbox's
    root, in the user's own sandbox; the same places are refused by path,
    and a read through any symlink is refused. A listing holds at most
    5000 entries and a read at most 1 MiB (read in ranges); `watch`
    polls (1s here, 3s on a machine) until you hang up.
  - New sessions, and the machines of VM tabs and VM panes, get generated
    names ("drifting cedar", unique per daemon). Ids don't change, rename
    is still a double-click, and older sessions keep their names.

### The CLI

```
illogical ls                                  # panes, what they're running, who needs you
illogical run -- make test                    # in a new tab; prints its pane (%N)
illogical run --wait -- cargo build           # and exits with its exit code
illogical send %3 'git status' -e             # type a line and press Enter
illogical keys %3 C-c Up Enter                # named keys
illogical wait %3 --command-end               # exit code of what that started
illogical wait %3 --match 'listening on' --timeout 30
illogical tail %3 -f --text                   # follow output, escapes stripped
illogical tail %3 --last-command              # just the last command's output
illogical capture %3 --scrollback [--ansi|--html]
illogical process %3                          # the foreground process
illogical events -f [--pane %3] [--type command_end,attention]
illogical history --failed --since 2h
illogical search 'panic|Traceback' --since 1d
illogical export %3 -o session.cast           # asciinema play session.cast
illogical run --vm -- 'git clone … && make'   # on a throwaway VM (no command: a shell)
illogical run --vm-tab                        # a tab whose panes share a new VM
illogical machines                            # VMs, their owner (@tab or %pane) and state
illogical open example.com                    # a browser block (--split %3 beside a pane)
illogical open --split right :5173/about      # a port, beside this pane, on its VM tab's machine
illogical open --host m2 :3000                # a port on machine m2 (--host local: this host)
illogical describe %4                         # any block: type, place, state
illogical call %4 navigate '{"url":"…"}'      # a block's own methods
illogical agent "fix the failing test"        # Claude Code here; prints %N (--codex, --fountain A,
                                              #   --acp CMD, --vm, --model haiku, --cwd d, --wait)
illogical wait %5 --needs-input               # it asks to run something…
illogical call %5 approve                     # …or '{"option":"always"}'; deny '{"reason":"…"}'; cancel
illogical call %5 send '{"text":"and then?"}' # the next message (queued while it works)
illogical wait %5 --idle                      # the turn ended: prints idle, done or needs-input
illogical tail %5 -f                          # any block's text as it grows
illogical attach %3                           # from a real terminal; Ctrl-] detaches
illogical close %3                            # its output stays in history
illogical attention needs-input               # from a hook, in the current pane
illogical hosts                               # the home daemon's other hosts, last seen
illogical hosts add box https://box.tailb2e8f2.ts.net
illogical hosts invite                        # a one-time token a sandbox joins with
illogical --host box run --wait -- make       # any command, on another host
illogical hosts token sbx                     # a dial-out host's token (prints it once)
illogical hosts revoke sbx                    # …revoked, and its connection dropped
illogical share %3 --ttl 2h                   # a read-only link to a pane
illogical shares                              # links that still work; shares revoke ID
illogical search 'panic' --synced sbx         # a host's synced history (all: every host)
illogical tail %4 --synced sbx --text         # one of its panes, after it's gone
illogical synced                              # hosts whose history is kept here
illogical sandboxes                           # the provider's sandboxes and their state
illogical run --sandbox s1                    # a disposable shell on one, nothing installed there
illogical sandboxes promote s1 --as s1        # a resident daemon there, a host reached through the tunnel
illogical --host s1 ls                        # through the tunnel (wakes it)
illogical fs ls -l ~/src                      # files on this host (read-only)
illogical fs cat %4:~/app/log.txt             # on the host %4 runs on (its VM); mN:PATH for machine N
illogical fs watch ~/src                      # changes, as NDJSON (also stat, recent)
illogical run --cwd ~/src                     # a shell in a directory, in a new tab
illogical run --split %4 --join --cwd ~/app   # beside %4, where it runs (its VM tab's machine)
illogical cd %4 ~/src                         # typed into %4's shell, only if it's at its prompt
```

`--json` prints the API's JSON. `send` then `wait` only sees what happened
after the send. The same calls are an HTTP API (`/api/...`, documented in
`crates/proto/src/api.rs`) on the Unix socket and, behind the usual access
checks, over the tailnet.

**Claude Code** can tell you when it needs you. In `~/.claude/settings.json`:

```json
{
  "hooks": {
    "Notification": [{ "hooks": [{ "type": "command", "command": "illogical attention needs-input" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "illogical attention done" }] }]
  }
}
```

Outside an illogical pane the command does nothing, so the hooks are safe
everywhere. Without them, an agent going quiet mid-command is the fallback.

## Use it

```
just bootstrap      # Zig 0.16 via mise, web dependencies
just install        # release build, installed as a systemd user service
```

`illogicald install` copies the binary to `~/.local/bin`, writes
`~/.config/systemd/user/illogicald.service` and enables it; with lingering on
(`loginctl enable-linger $USER`) it starts at boot. Logs:
`journalctl --user -u illogicald`.

Open <http://127.0.0.1:7681>, or <https://geek.tailb2e8f2.ts.net> from
anywhere on the tailnet (`tailscale serve --bg --https=443
http://127.0.0.1:7681` is already configured on geek). The daemon accepts
tailnet requests from the login that owns the node; `--owner` overrides.

**Browser blocks on ports** need a listener for block sites; without one
they're off. On geek:

```
illogicald install -- --block-listen 100.71.195.119:7443 \
  --block-domain illogical.widgets.wtf \
  --block-acme-cloudflare-token-file ~/.local/share/wisp/cloudflare-token
```

- `*.illogical.widgets.wtf` is a Cloudflare DNS record (DNS only) pointing
  at geek's tailnet address, so only the tailnet reaches it. Port 443 there
  is `tailscale serve`'s and 8443 is wispd's, hence 7443.
- The daemon gets the wildcard certificate from Let's Encrypt itself, with a
  DNS-01 challenge through Cloudflare (the token wisp already uses), and
  renews it two thirds of the way through its life. It lives in
  `<state>/acme/<CA>/`. `--block-acme-directory staging` uses the test CA;
  `--block-cert`/`--block-key` serve files you renew yourself.
- Callers are checked with `tailscale whois`: only the owner gets in.
- **Dev mode:** `--block-listen 127.0.0.1:7701` without `--block-domain`
  serves `http://b-<id>-<key>.localhost:7701` on loopback, where browsers
  resolve `*.localhost` themselves. There's no identity there, so each
  block's name carries a random key (kept in its config): only the app and
  the CLI know it. The tests use this.

**Pane environment.** At boot the daemon starts before you log in, so its own
environment has no `WAYLAND_DISPLAY`, `DISPLAY` or desktop `SSH_AUTH_SOCK`.
Each new pane takes the systemd user manager's environment as it is at that
moment, which your desktop session fills in at login. For variables every
pane should have from boot (`PATH` additions, `EDITOR`), put `KEY=value`
lines in `~/.config/environment.d/50-illogical.conf`. Panes run `$SHELL -l`,
so your profile runs too.

**A sandbox on the tailnet** (M4a). `just static` builds static x86_64 musl
binaries in `target/x86_64-unknown-linux-musl/release/`. Copy
`illogicald` and `illogical` into the sandbox, then:

```
illogical hosts invite                                  # on geek: prints a token
illogical install --tailnet file:KEYFILE \
  --home https://geek.tailb2e8f2.ts.net --join TOKEN    # in the sandbox
```

The key is an ephemeral, `tag:sandbox` Tailscale auth key, in a file (or
`-` for stdin; it is never put on a command line). This downloads
tailscaled if it isn't there, runs it in userspace mode with its own state
in `~/.local/state/illogical-sandbox`, joins, puts the daemon behind
`tailscale serve`, and adds it to geek's list (learning whom to let in;
`--owner` otherwise). `illogicald sandbox` keeps tailscaled and the daemon
running and restarts them; install starts it detached. After a reboot, run
`illogicald sandbox &` again. Without `--join` it prints the `illogical
hosts add` line to run on geek. Another daemon accepts the home page only
by exact origin: `--allow-origin https://geek.tailb2e8f2.ts.net` (install
sets it from `--home`). Sprites pause when idle and tailnet traffic doesn't
wake them, so wake one through the provider first (M4b does that).

**A sandbox that can only dial out** (M4c). On geek, `illogical hosts
token sbx` (or `hosts invite`); in the sandbox:

```
illogicald --peer wss://geek.tailb2e8f2.ts.net --token ~/.config/illogical/host-token \
  [--join INVITE] [--sync [--sync-live]] &
```

The token file is read (or, with `--join`, made from the invite) and kept
0600. The home daemon must be reachable at that URL from the sandbox and
accept its name as a Host (`--public-host`); the dial and the pushes carry
the token, not an identity. Its page and CLI reach the sandbox at
`/h/sbx/…`, and a share link to one of its panes would be on the
sandbox's own daemon, so the menu doesn't offer one there.

Development: `just dev` runs a separate daemon on 7682 (state in
`~/.local/state/illogical-dev`) plus Vite on 5173, leaving the real one
alone. `just check` is what CI runs; `just e2e` drives the system Chrome
against throwaway daemons, or `just e2e https://geek.tailb2e8f2.ts.net`
against the running one.

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
  (`fs.rs`), names (`illogical_core::names`).
- `crates/cli`: `illogical`, over the daemon's Unix socket, or HTTP(S) to
  another daemon with `--host` (`hosts.rs`).
- `web`: TypeScript client: Preact for the chrome, xterm.js 6 terminals that
  are moved between slots rather than recreated, Playwright tests (desktop
  and phone).
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
  the same name.** `illogical synced rm sbx` set `--host sbx`.
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
