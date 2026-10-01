# illogical

A personal multiplexer built around durable sessions: a daemon owns the
terminals, and mouse-first clients attach to them.

Start with [BRIEF.md](BRIEF.md), then [PLAN.md](PLAN.md) (decisions and
milestones) and [docs/research.md](docs/research.md).

## Status: M3c (VM tabs) works

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

- **iTerm2 as a client** (M5). `illogical tmux -CC` speaks tmux's control
  mode, so iTerm2 (and Ghostty's and WezTerm's tmux support) shows
  illogical's sessions, tabs and splits as native windows, tabs and splits,
  live alongside the browser; see *Use it*.

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
illogical tmux -CC attach [-t SESSION]        # be tmux for iTerm2 (see *Use it*)
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

**iTerm2, as a tmux client** (M5). iTerm2's tmux integration works with
illogical in place of tmux: sessions are sessions, tabs are native windows,
splits are native splits, and the same layout stays live in the browser.
From iTerm2 on the Mac:

```
ssh -t geek '~/.local/bin/illogical tmux -CC attach'          # the first session
ssh -t geek '~/.local/bin/illogical tmux -CC attach -t work'
ssh -t geek '~/.local/bin/illogical tmux -CC new -s ipad'
```

iTerm2 sees the control-mode greeting and takes over the window. `-t` is a
session name or `$N`; plain `illogical tmux -CC` is `attach`. To detach,
use iTerm2's *Shell › tmux › Detach* (or press Esc in the gateway
window). Add `--host NAME` before `tmux` to reach another daemon. Anything
that runs `tmux -CC` by name can run illogical instead: the CLI behaves as
`illogical tmux` when it is called `tmux`, so put a link where only that
command looks (`mkdir -p ~/.local/share/illogical/tmux && ln -s
~/.local/bin/illogical ~/.local/share/illogical/tmux/tmux`, then `ssh -t geek
'PATH=~/.local/share/illogical/tmux:$PATH tmux -CC attach'`), not on your
`PATH`, where it would hide the real tmux.

What to expect:
- It reports tmux 3.5a. Typing, splits (*Shell › Split*), divider drags,
  window resizes, new tabs and closing panes all change the daemon's
  layout, which the browser shows at once, and the other way round.
- The window's size follows whoever claimed it last: iTerm2 claims it when
  it resizes a window or you type in it, the browser when you click or
  type there. The other side draws the tab at that size.
- iTerm2 keeps its tab grouping and its attach guard in the session's
  options; they're saved with the layout, so they survive a reattach and a
  reboot.
- Agent and browser blocks show as read-only panes with their text and a
  note to open them in the web app.
- If iTerm2 falls behind a fast pane it shows "paused"; unpausing
  re-captures the pane and carries on.

**Testing it from the Mac** (nothing here has seen a real iTerm2 yet):

1. On geek, install the build (`just install`) and check `illogical ls`
   works. Open <https://geek.tailb2e8f2.ts.net> in a browser beside iTerm2.
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
  (`machine.rs`: the Sprites API and exec TTY sessions), blocks
  (`block.rs`, `browser.rs`; agents in `agent/`: the ACP client, the
  transcript, agent definitions, the local and VM pipes), block sites
  (`sites.rs`: per-block origins and their HTTP proxy; `ports.rs`: reaching
  a port here or in a VM; `tls.rs`: the wildcard certificate and ACME), the
  WebSocket server, embedded web client, access checks, `install`.
  Federation: the host list and invites (`hosts.rs`), tailscaled's local
  API and WhoIs (`tailscale.rs`), and sandboxes (`sandbox.rs`: `install
  --tailnet` and the `sandbox` supervisor).
- `crates/cli`: `illogical`, over the daemon's Unix socket, or HTTP(S) to
  another daemon with `--host` (`hosts.rs`). `tmux/` is the tmux
  control-mode front end (M5): the command parser and `-F` format expander,
  layout strings derived from the daemon's ratios (spike S11's converter),
  and a mirror terminal per pane so captures line up with the output
  stream. `crates/daemon/tests/tmux.rs` replays iTerm2's command sequence
  and compares every reply with what tmux 3.6 answered (S11's transcript).
- `web`: TypeScript client: Preact for the chrome, xterm.js 6 terminals that
  are moved between slots rather than recreated, Playwright tests (desktop
  and phone).
- `spikes`: S1–S3 write-ups and code.

## Things M0–M4a taught us

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
