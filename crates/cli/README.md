# Arugula

The `arugula` command: drive arugulad from a shell or a script, over the
daemon's HTTP API on its Unix socket (or another daemon's URL, with
`--host`). `--json` prints the API's answers as they are.

Depends on `arugula-core`, `arugula-proto`, `arugula-vt` and
`arugula-e2e`.

Start with `src/main.rs` (the command tree and dispatch); each subcommand is
in `src/cmd/<name>.rs`, shared helpers in `src/util.rs`. `src/ask.rs` and
`src/hook.rs` are Claude Code's hooks, `src/mcp.rs` the stdio bridge to the
daemon's MCP server. `src/tui/` (`arugula tui`) and `src/tmux/`
(`arugula tmux -CC`) are kept working but get no new features.
