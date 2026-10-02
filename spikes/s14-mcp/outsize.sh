#!/usr/bin/env bash
# outsize.sh NAME CHARS STRUCTURED [ENV=VAL...] — one output-size trial
# against the server on :7930 (start it with ./serve.sh http 127.0.0.1:7930 out).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
name="$1" chars="$2" st="$3"; shift 3
echo '{"mcpServers":{"s14":{"type":"http","url":"http://127.0.0.1:7930/mcp"}}}' >"$here/work/mcp-out.json"
prompt="Call the tool mcp__s14__big exactly once with chars=$chars and structured=$st. Do not call any other tool and do not retry. Then reply with: (1) the first 80 characters of what you received, (2) the last 300 characters of what you received, verbatim, and (3) the highest line number you can see."
env "$@" "$here/cc.sh" "$name" work/mcp-out.json "$prompt" >"$here/work/$name.summary" 2>&1 || true
cat "$here/work/$name.summary"
