# illogical

The `illogical` command: drive illogicald from a shell or a script, over the
daemon's HTTP API on its Unix socket (or another daemon's URL, with
`--host`). `--json` prints the API's answers as they are.

Depends on `illogical-core`, `illogical-proto`, `illogical-vt` and
`illogical-e2e`.

Start with `src/main.rs` (the command tree and dispatch); each subcommand is
in `src/cmd/<name>.rs`, shared helpers in `src/util.rs`. `src/ask.rs` and
`src/hook.rs` are Claude Code's hooks, `src/mcp.rs` the stdio bridge to the
daemon's MCP server. `src/tui/` (`illogical tui`) and `src/tmux/`
(`illogical tmux -CC`) are kept working but get no new features.
