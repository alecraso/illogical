#!/usr/bin/env bash
# tui-run.sh NAME PORT SECS EVERY — interactive Claude Code (S18's tui.py, 120x40
# PTY) calling the sleep tool on an HTTP server on PORT, to see what the TUI
# does with a long MCP call (auto-backgrounding at 120s?). Screens go to
# work/tui-NAME.screens. MCP_TOOL_TIMEOUT=600000 so HTTP's 60s silence limit
# is out of the way.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
name=$1 port=$2 secs=$3 every=$4
"$here/serve.sh" http "127.0.0.1:$port" "tui-$name" >/dev/null
echo "{\"mcpServers\":{\"s14\":{\"type\":\"http\",\"url\":\"http://127.0.0.1:$port/mcp\"}}}" >"$here/work/mcp-tui-$name.json"
echo '{"permissions":{"allow":["mcp__s14"]}}' >"$here/work/tui-settings.json"
prompt="Call the tool mcp__s14__sleep exactly once with secs=$secs and progress_every=$every, then tell me what it returned."
cd "$here"
MCP_TOOL_TIMEOUT=600000 setsid work/venv/bin/python tui.py "$name" work/tui-settings.json "$prompt" \
  --mcp-config "$here/work/mcp-tui-$name.json" --strict-mcp-config >"work/tui-$name.out" 2>&1 &
echo $! >"work/tui-$name.pid"
echo "started tui $name"
