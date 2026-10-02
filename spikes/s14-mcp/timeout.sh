#!/usr/bin/env bash
# timeout.sh NAME http|stdio PORT SECS PROGRESS_EVERY [ENV=VAL...]
# One Claude Code timeout trial: its own server (HTTP on PORT, or stdio), one
# `sleep` call, and the wall clock plus the error text Claude Code reports.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
name="$1" transport="$2" port="$3" secs="$4" every="$5"; shift 5
cfg="work/mcp-$name.json"
if [ "$transport" = http ]; then
  "$here/serve.sh" http "127.0.0.1:$port" "$name" >/dev/null
  echo "{\"mcpServers\":{\"s14\":{\"type\":\"http\",\"url\":\"http://127.0.0.1:$port/mcp\"}}}" >"$here/$cfg"
else
  echo "{\"mcpServers\":{\"s14\":{\"type\":\"stdio\",\"command\":\"$here/target/debug/s14-mcp\",\"args\":[\"stdio\"],\"env\":{\"S14_LOG\":\"$here/work/$name.log\"}}}}" >"$here/$cfg"
fi
prompt="Call the tool mcp__s14__sleep exactly once with secs=$secs and progress_every=$every. Do not retry it and do not call any other tool. Then reply with exactly what the tool returned, or the exact error text, verbatim."
env "$@" "$here/cc.sh" "$name" "$cfg" "$prompt" >"$here/work/$name.summary" 2>&1 || true
[ "$transport" = http ] && "$here/stop.sh" "$name"
cat "$here/work/$name.summary"
