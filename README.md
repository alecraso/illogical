# illogical

A personal multiplexer built around durable sessions: a daemon owns the
terminals, and mouse-first clients attach to them.

Start with [BRIEF.md](BRIEF.md), then [PLAN.md](PLAN.md) (decisions and
milestones) and [docs/research.md](docs/research.md).

## Status: M1 (multiplexer) works

Sessions, tabs and splits of shells owned by `illogicald`, driven by the
mouse: right-click a pane to split, move or close it; drag the grip in a
pane's corner onto another pane's edge to move it; drag dividers to resize;
drag tabs to reorder them or onto a pane's edge to dock them; double-click a
tab to rename it, middle-click to close it. Every window shows the same
layout live, and closing or losing a window loses nothing. On a phone you see
one pane at a time, switch from a sheet, and get a key bar with Esc, Tab,
sticky Ctrl/Alt and arrows.

The layout lives on the daemon and is computed in character cells (like
tmux), so every window draws exactly the panes' real terminal sizes. Each tab
takes the size of the window that last opened, focused or typed in it;
other windows show it scaled to fit. Not yet: anything surviving a daemon
restart or reboot (M2), or the CLI (M3).

## Use it

```
just bootstrap      # Zig 0.15.2 via mise, web dependencies
just run            # release build, daemon on 127.0.0.1:7681
```

Open <http://127.0.0.1:7681>, or <https://geek.tailb2e8f2.ts.net> from
anywhere on the tailnet (`tailscale serve --bg --https=443
http://127.0.0.1:7681` is already configured on geek). The daemon accepts
tailnet requests from the login that owns the node; `--owner` overrides.

Development: `just dev` runs a separate daemon on 7682 plus Vite on 5173, so
the real daemon and its shell are left alone. `just check` is what CI runs;
`just e2e` drives the system Chrome against a throwaway daemon, or
`just e2e https://geek.tailb2e8f2.ts.net` against the running one.

## Layout

- `crates/core`: sessions, tabs and split trees, the intents that change
  them, and the cell layout. Pure state, property-tested.
- `crates/proto`: wire protocol (JSON control messages + binary frames with a
  per-pane stream offset). Mirrored by hand in `web/src/proto.ts`.
- `crates/vt`: server-side terminal state on libghostty-vt. Snapshots that
  reproduce the screen (spike S1's fix-ups), answers to terminal queries
  limited to what xterm.js can draw, recorded fixtures.
- `crates/daemon`: `illogicald`. A multiplexer task owning the layout, a PTY
  + VT thread per pane, axum WebSocket server, embedded web client,
  Host/Origin/tailnet-identity checks.
- `web`: TypeScript client: Preact for the chrome, xterm.js 6 terminals that
  are moved between slots rather than recreated, Playwright tests (desktop
  and phone).
- `spikes`: S1–S3 write-ups and code.

## Things M0 and M1 taught us

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
