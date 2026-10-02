#!/usr/bin/env bash
# codex.sh NAME stdio|http PORT PROMPT — `codex exec --json` with one extra MCP
# server (s14) added by -c overrides; ~/.codex/config.toml is not edited.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
name="$1" transport="$2" port="$3" prompt="$4"
mkdir -p "$here/work/codex"
[ -d "$here/work/codex/.git" ] || git -C "$here/work/codex" init -q
if [ "$transport" = http ]; then
  "$here/serve.sh" http "127.0.0.1:$port" "codex-$name" >/dev/null
  cfg=(-c "mcp_servers.s14.url=\"http://127.0.0.1:$port/mcp\"")
else
  cfg=(-c "mcp_servers.s14.command=\"$here/target/debug/s14-mcp\"" -c 'mcp_servers.s14.args=["stdio"]'
       -c "mcp_servers.s14.env={S14_LOG=\"$here/work/codex-$name.log\"}")
fi
for v in $(env | grep -oE '^(CLAUDE[A-Z_]*|AI_AGENT)=' | tr -d =); do unset "$v"; done
cd "$here/work/codex"
start=$(date +%s.%N)
[ -n "${TT:-}" ] && cfg+=(-c "mcp_servers.s14.tool_timeout_sec=$TT")
codex exec --json --skip-git-repo-check "${cfg[@]}" \
  -c 'approval_policy="never"' "$prompt" >"$here/work/codex-$name.ndjson" 2>"$here/work/codex-$name.err" || echo "exit $?"
echo "wall: $(echo "$(date +%s.%N) - $start" | bc)s"
[ "$transport" = http ] && "$here/stop.sh" "codex-$name"
grep -E '"type":"(item.completed|turn.completed|error)"' "$here/work/codex-$name.ndjson" | cut -c1-700
