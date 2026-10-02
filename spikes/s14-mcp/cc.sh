#!/usr/bin/env bash
# cc.sh NAME CONFIG PROMPT [extra claude args...]
# Run `claude -p` (haiku) in work/cc against one MCP config, with the parent
# Claude Code session's environment removed. Extra env (MCP_TOOL_TIMEOUT,
# MAX_MCP_OUTPUT_TOKENS, ...) passes through. Stream JSON goes to
# work/cc-NAME.ndjson.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
name="$1" config="$2" prompt="$3"; shift 3
mkdir -p "$here/work/cc"
[ -d "$here/work/cc/.git" ] || git -C "$here/work/cc" init -q
for v in $(env | grep -oE '^(CLAUDE[A-Z_]*|AI_AGENT)=' | tr -d =); do unset "$v"; done
cd "$here/work/cc"
start=$(date +%s.%N)
claude -p "$prompt" --model haiku --mcp-config "$here/$config" --strict-mcp-config \
  --setting-sources local --allowedTools 'mcp__s14' 'ReadMcpResourceTool' 'ListMcpResourcesTool' \
  --output-format stream-json --verbose "$@" >"$here/work/cc-$name.ndjson" 2>"$here/work/cc-$name.err" || echo "exit $?"
end=$(date +%s.%N)
echo "wall: $(echo "$end - $start" | bc)s"
python3 - "$here/work/cc-$name.ndjson" <<'EOF'
import json, sys
for line in open(sys.argv[1]):
    e = json.loads(line)
    t = e.get("type")
    if t == "system" and e.get("subtype") == "init":
        print("init: version", e.get("claude_code_version"), "mcp", e.get("mcp_servers"),
              "tools", [x for x in e.get("tools", []) if "mcp" in x.lower() or "Mcp" in x])
    elif t == "assistant":
        for c in e["message"]["content"]:
            if c["type"] == "tool_use":
                print("tool_use:", c["name"], json.dumps(c["input"])[:200])
            elif c["type"] == "text":
                print("text:", c["text"][:600])
    elif t == "user":
        for c in e["message"]["content"]:
            if isinstance(c, dict) and c.get("type") == "tool_result":
                s = json.dumps(c.get("content"))
                print("tool_result:", "is_error" if c.get("is_error") else "", len(s), "chars:", s[:400], "...", s[-400:] if len(s) > 800 else "")
        if e.get("tool_use_result") is not None:
            s = json.dumps(e["tool_use_result"])
            print("  tool_use_result:", len(s), s[:300])
    elif t == "result":
        print("result:", e.get("subtype"), "turns", e.get("num_turns"), "cost", e.get("total_cost_usd"))
EOF
