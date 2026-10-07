# arugulad

The daemon: owns the terminals and blocks on a machine; clients attach over
WebSocket and the HTTP API. [AGENTS.md](../../AGENTS.md) maps its modules by
layer.

Depends on `arugula-core`, `arugula-proto`, `arugula-vt`,
`arugula-e2e` and `arugula-control-wire` (the messages it shares with
control).

Start with `src/mux/mod.rs`: the task that orders every change. Then
`src/pane.rs` (a pane and its VT thread) and `src/server.rs` (the web client
and `/ws`). `src/main.rs` is startup; `src/args.rs` the command line.

Integration tests are one binary, `tests/integration/`, on
`arugula-testkit`; recorded data in `tests/fixtures/`. `tests/integration/tmux.rs`
replays iTerm2's tmux -CC session and compares every reply with tmux 3.6's.
