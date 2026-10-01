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

**Pane environment.** At boot the daemon starts before you log in, so its own
environment has no `WAYLAND_DISPLAY`, `DISPLAY` or desktop `SSH_AUTH_SOCK`.
Each new pane takes the systemd user manager's environment as it is at that
moment, which your desktop session fills in at login. For variables every
pane should have from boot (`PATH` additions, `EDITOR`), put `KEY=value`
lines in `~/.config/environment.d/50-illogical.conf`. Panes run `$SHELL -l`,
so your profile runs too.

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
  transcript, agent definitions, the local and VM pipes), the WebSocket
  server, embedded web client, access checks, `install`.
- `crates/cli`: `illogical`, over the daemon's Unix socket.
- `web`: TypeScript client: Preact for the chrome, xterm.js 6 terminals that
  are moved between slots rather than recreated, Playwright tests (desktop
  and phone).
- `spikes`: S1–S3 write-ups and code.

## Things M0–M3c taught us

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
