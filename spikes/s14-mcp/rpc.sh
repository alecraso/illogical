#!/usr/bin/env bash
# rpc.sh URL — a raw Streamable HTTP session with curl: initialize (2025-11-25),
# tools/list, resources, and tool calls with a progress token. Prints every
# response so the wire shapes are visible.
set -euo pipefail
url="${1:-http://127.0.0.1:7914/mcp}"
h=(-sS -D /dev/stderr -H 'content-type: application/json' -H 'accept: application/json, text/event-stream')
init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
sid=$(curl "${h[@]}" "$url" -d "$init" 2>&1 >/dev/null | tr -d '\r' | awk -F': ' 'tolower($1)=="mcp-session-id"{print $2}')
echo "session: $sid"
h+=(-H "mcp-session-id: $sid" -H 'mcp-protocol-version: 2025-11-25')
curl "${h[@]}" "$url" -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' 2>/dev/null
call() { echo "== $1"; curl "${h[@]}" "$url" -d "$1" 2>/dev/null; echo; }
call '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
call '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"pane_summary","arguments":{}}}'
call '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"summary_and_structured","arguments":{}}}'
call '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"close_pane","arguments":{}}}'
call '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"sleep","arguments":{"secs":3,"progress_every":1},"_meta":{"progressToken":"p1"}}}'
call '{"jsonrpc":"2.0","id":7,"method":"resources/templates/list"}'
call '{"jsonrpc":"2.0","id":8,"method":"resources/subscribe","params":{"uri":"s14://counter"}}'
# a GET stream to receive the resource-updated notification
(curl -sS -N -H 'accept: text/event-stream' -H "mcp-session-id: $sid" -H 'mcp-protocol-version: 2025-11-25' "$url" --max-time 3 || true) &
sleep 1
call '{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"bump","arguments":{}}}'
wait
