# illogical

A personal multiplexer built around durable sessions: a daemon owns the
terminals, and mouse-first clients attach to them.

Start with [BRIEF.md](BRIEF.md), then [PLAN.md](PLAN.md) (decisions and
milestones) and [docs/research.md](docs/research.md).

## Status: M0 (the loop) works

One shell pane, owned by `illogicald`. Close the browser tab (or lose the
connection) and reopen it from any machine or phone on the tailnet: the
screen comes back exactly, including full-screen programs like nvim, and
nothing printed in between is lost.

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

- `crates/proto`: wire protocol (JSON control messages + binary frames with a
  per-pane stream offset). Mirrored by hand in `web/src/proto.ts`.
- `crates/vt`: server-side terminal state on libghostty-vt. Snapshots that
  reproduce the screen (spike S1's fix-ups), answers to terminal queries
  limited to what xterm.js can draw, recorded fixtures.
- `crates/daemon`: `illogicald`. PTY + VT thread per pane, axum WebSocket
  server, embedded web client, Host/Origin/tailnet-identity checks.
- `web`: TypeScript client on xterm.js 6, plus Playwright tests.
- `spikes`: S1–S3 write-ups and code.

## Things M0 taught us

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
- **Sizing:** the last client to connect, focus or type owns the pane size;
  others draw at that size, scaled down to fit if needed.
