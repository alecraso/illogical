# illogical

A personal multiplexer built around durable sessions: a daemon owns the
terminals, and mouse-first clients attach to them.

Start with [BRIEF.md](BRIEF.md), then [PLAN.md](PLAN.md) (decisions and
milestones) and [docs/research.md](docs/research.md).

## Status: M2b (in-place upgrade) works

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

Not yet: the CLI and shell integration (M3).

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
- `crates/daemon`: `illogicald`. A multiplexer task owning the layout, a PTY
  + VT thread per pane with its log and checkpoints (`store.rs`), restore
  and restart policies, the pane shim and FD store handling (`shim.rs`,
  `sys.rs`), axum WebSocket server, embedded web client,
  Host/Origin/tailnet-identity checks, `install`.
- `web`: TypeScript client: Preact for the chrome, xterm.js 6 terminals that
  are moved between slots rather than recreated, Playwright tests (desktop
  and phone).
- `spikes`: S1–S3 write-ups and code.

## Things M0–M2b taught us

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
