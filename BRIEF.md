# illogical: brief

Written 2026-10-01. Status: seed, no code yet.

## What this is

A personal multiplexer built around durable sessions. A daemon owns the
terminals; clients (web first, native later) attach to them and draw tabs and
splits as ordinary GUI elements. The multiplexer is never something you operate
by hand.

Inspired by Superlogical's public pitch ("the multiplexer for all work", a
"durable session around the work itself"). This is a single-user take on the
same idea, built now instead of waiting for their beta. Nothing of theirs is
used; only the public description.

## Why

- Wave is crashing the workstation, and nothing on Linux gives mouse-first
  tabs and splits plus sessions that outlive the terminal window.
- tmux and Zellij do persistence but are keyboard-first. Zellij was tried and
  rejected for that reason.
- Agent runs (Claude Code, hal0 jobs) need somewhere durable to live that can
  be checked from another machine or a phone.

## Requirements

1. Tabs and splits driven by mouse: click, drag, right-click menus. No chords
   required for anything.
2. Close the client, or lose the connection, and nothing is lost. Reattach from
   any client and see the same layout.
3. Reboot the machine and get the layout, working directories and scrollback
   back, with each pane restarted according to a policy.
4. Sessions are addressable from a CLI and API so scripts and agents can
   create, feed, tail and wait on them.
5. Works over the tailnet from other machines and a phone browser.

## Non-goals

- Multi-tenant or enterprise access control. (Sharing with a few people is
  planned as PLAN.md's multiplayer track, M12–M15; organisations, SSO and policy
  engines stay out.)
- Writing a terminal emulator. Rendering is borrowed (xterm.js, libghostty).
- Windows.
- tmux keybinding compatibility.

## What "durable" can honestly mean

Processes do not survive a reboot. Durable here means:

- **Client independence:** the daemon owns the PTYs, so clients come and go.
- **History on disk:** every pane's output is appended to a log as it happens.
- **Layout on disk:** the session tree (tabs, splits, sizes, titles, cwd) is
  persisted on every change.
- **Restart policy per pane:** after a reboot each pane is recreated with its
  scrollback and then does one of: nothing, start a shell in the old cwd,
  re-run its command, or run a resume hook (for example `claude --continue`).
- **Daemon restarts without killing panes:** later milestone; hold PTY master
  fds outside the daemon (systemd fd store, or a tiny holder process as shpool
  and abduco do) so the daemon can be upgraded in place.

## Architecture

```
 clients (web, CLI, later native / tmux -CC terminals)
        |  WebSocket or Unix socket, one protocol
 illogicald  (one per host, systemd user service with linger)
   - session tree: session > tab > split tree > pane
   - per pane: PTY, server-side VT state, append-only output log
   - event stream: output, layout change, command start/end, cwd, exit
   - state dir: layout.json, logs/, events/
```

Key decisions and the reasons for them:

- **Layout is server state.** Every client shows the same tabs and splits, and
  it survives the client. Clients send intents (split, move, resize, close).
- **Server-side VT state per pane.** Attaching needs a correct snapshot of the
  screen, not a replay of raw bytes. Without it, full-screen programs redraw
  wrong. This is the hard part and the reason language choice matters.
- **Structured events from shell integration.** OSC 133 marks command
  boundaries and exit codes; OSC 7 gives cwd. That turns a byte stream into
  commands with output, which is what makes sessions useful to scripts and
  agents and makes "re-run on restore" possible.
- **Web client first.** It works on Linux today, on the phone, and needs no
  packaging. Run it as an installed PWA window on the desktop.
- **Auth is the tailnet.** Bind to the Tailscale interface and trust Tailscale
  identity. No accounts.

## Stack (recommended, open to change)

- **Daemon: Rust.** Good PTY and async support, and two workable options for
  VT state: libghostty-vt through its C API, or the `alacritty_terminal`
  crate. shpool is Rust prior art for the persistence side.
- **Web client: TypeScript + xterm.js** to start. ghostty-web (libghostty
  compiled to wasm behind an xterm.js-style API) is a candidate replacement;
  verify its state before depending on it.
- **CLI: same Rust binary** (`illogical`), talking to the daemon socket.

Go is the alternative if familiarity matters more; the cost is weaker
server-side VT libraries, which hits the hardest part of the design.

## Milestones

- **M0 spike:** daemon with one PTY, WebSocket, an xterm.js page. Close the
  browser, reopen, and the shell is still there. Proves the loop end to end.
- **M1 multiplexer:** many sessions, tabs and splits held server-side,
  mouse-first web UI, correct snapshot on attach from server VT state.
- **M2 durability:** output logs and layout on disk, restore after reboot with
  restart policies, systemd user unit.
- **M3 structure:** OSC 133 and OSC 7 handling, command blocks, exit codes;
  CLI and API: `run`, `ls`, `attach`, `send`, `tail`, `wait`.
- **M4 reach:** several hosts in one client over the tailnet, phone layout,
  read-only share links.
- **M5 native:** speak the tmux control mode protocol from the daemon so any
  `tmux -CC` capable terminal (iTerm2 today, Ghostty when its PR stack lands)
  is a native client. MisterTea's HTM takes this approach. See
  `~/ghostty-tmux-control-mode-brief.md` on geek for the Ghostty side.

Done for v1 means M0 to M2, and Jake using it daily instead of Wave.

## Acceptance tests for v1

- Kill the browser mid-command; reattach; output is complete and the screen is
  correct, including inside vim or htop.
- Attach from a second machine and a phone at the same time; both show the
  same layout and live output.
- Reboot; open the client; tabs, splits, cwd and scrollback are back, and
  panes with a re-run policy are running.
- A full day of use without touching a keyboard shortcut for layout.

## Prior art to read first

- tmux control mode (protocol design, what a GUI front end needs)
- Zellij (session resurrection, its web client)
- shpool, abduco, dtach (minimal session persistence, fd holding)
- WezTerm mux (native client over a mux protocol)
- Eternal Terminal and HTM (reconnect, control mode reuse)
- mosh (state sync instead of byte replay)
- ttyd, sshx (web terminals, sharing)
- asciinema cast format (candidate log format)
- Wave and Warp (command blocks from shell integration)

## Open questions

- Rust or Go for the daemon? (Recommendation above: Rust.)
- libghostty-vt or `alacritty_terminal` for server VT state? Needs a spike on
  libghostty-vt's C API maturity.
- Log format: raw bytes plus an index, or asciinema-style timed events?
- How are sizes reconciled when clients with different window sizes attach to
  the same pane? (tmux uses smallest; per-client reflow is much harder.)
- Is the fd-holding trick worth doing before M3, given daemon restarts will be
  frequent during development?
- Does the client need local-only panes, or is everything a daemon pane?

## Names

- Repo: `jhgaylor/illogical`
- Daemon: `illogicald`
- CLI: `illogical`
