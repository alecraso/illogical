# Run inside the sprite while `vm.sh relay` is up: speak stdio MCP through the
# relay's Unix socket the way an agent's `mcpServers` stdio command would
# (`nc -U /tmp/illogical-mcp.sock`), and time a tool call that sends progress.
python3 - <<'EOF'
import json, socket, time
s = socket.socket(socket.AF_UNIX); s.connect("/tmp/illogical-mcp.sock")
f = s.makefile("rwb")
def send(m): f.write((json.dumps(m) + "\n").encode()); f.flush()
def recv_until(id):
    while True:
        m = json.loads(f.readline())
        if m.get("id") == id: return m
        print("  notification:", json.dumps(m)[:160])
t = time.time()
send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "guest", "version": "0"}}})
print("initialize ->", recv_until(1)["result"]["serverInfo"], f"{(time.time()-t)*1000:.0f}ms")
send({"jsonrpc": "2.0", "method": "notifications/initialized"})
send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
print("tools ->", [x["name"] for x in recv_until(2)["result"]["tools"]])
t = time.time()
send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "summary_and_structured", "arguments": {}}})
print("call ->", json.dumps(recv_until(3)["result"])[:200], f"{(time.time()-t)*1000:.1f}ms")
send({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "sleep", "arguments": {"secs": 3, "progress_every": 1}, "_meta": {"progressToken": "g"}}})
print("sleep ->", json.dumps(recv_until(4)["result"])[:200])
EOF
