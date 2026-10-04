#!/bin/bash
# Q4: claude's stdin/stdout are FIFOs held open read-write by a "keeper"
# (stand-in for systemd's fd store). The "daemon" is plain shell commands that
# come and go: daemon #1 sends turn 1 and leaves while a permission request is
# held; nobody reads stdout for a while; daemon #2 drains the backlog, sends
# turn 2 and reads the answer.
set -u
A=$(cd "$(dirname "$0")" && pwd)
W=$A/../work/pipes; mkdir -p "$W"; cd "$A/../work/claude"
rm -f "$W/in" "$W/out"; mkfifo "$W/in" "$W/out"
sleep 600 3<>"$W/in" 4<>"$W/out" & KEEPER=$!
BUN=/home/me/.local/share/mise/installs/bun/1.4.2/bin/bun
MCP=$(jq -nc --arg b "$BUN" --arg s "$A/perm-mcp.ts" --arg l "$A/samples/held-pipes.perm.ndjson" \
  '{mcpServers:{perm:{type:"stdio",command:$b,args:[$s],env:{PERM_MODE:"hold:10",PERM_LOG:$l}}}}')
"$A/claude.sh" -p --verbose --input-format stream-json --output-format stream-json --model haiku \
  --strict-mcp-config --setting-sources "" --tools Bash --mcp-config "$MCP" \
  --permission-prompt-tool mcp__perm__approve <"$W/in" >"$W/out" 2>"$W/err" & CL=$!
echo "keeper=$KEEPER claude=$CL"
msg() { jq -nc --arg t "$1" '{type:"user",message:{role:"user",content:$t}}'; }
show() { jq -c 'select(.type=="assistant" or .type=="result" or (.type=="user" and (.message.content|type)=="array")) | [.type, (.subtype//""), ((.message.content//[])|map(.text//.name//(.content|tostring)?)|join(" ")), (.result//"")]' 2>/dev/null; }

echo "daemon #1: send turn 1, read for 4s, then go away (permission is held 10s)"
msg 'Remember the word OSPREY. Run exactly `echo piped > piped.txt` with Bash, then say done.' >"$W/in"
timeout 4 cat "$W/out" | tee "$A/samples/held-pipes.ndjson" | show
echo "--- no daemon for 20s; claude alive? $(kill -0 $CL 2>/dev/null && echo yes || echo no)"
sleep 20
echo "daemon #2: drain the backlog, then send turn 2"
timeout 2 cat "$W/out" | tee -a "$A/samples/held-pipes.ndjson" | show
msg 'What word did I ask you to remember? One word.' >"$W/in"
timeout 15 cat "$W/out" | tee -a "$A/samples/held-pipes.ndjson" | show
echo "claude alive after turn 2? $(kill -0 $CL 2>/dev/null && echo yes || echo no); piped.txt: $(cat piped.txt 2>/dev/null)"
kill $CL $KEEPER 2>/dev/null; wait 2>/dev/null
jq -s 'map(select(.type=="result")) | last | .total_cost_usd' "$A/samples/held-pipes.ndjson"
