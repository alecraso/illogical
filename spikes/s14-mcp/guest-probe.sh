# Run inside the sprite (vm.sh runfile NAME guest-probe.sh): TCP connects to
# host addresses, then a real MCP POST to the spike server on the bridge IP.
#   10.209.0.1:7940      the spike MCP server, bound to the bridge address
#   10.209.0.1:8080      a host service on 0.0.0.0, via the bridge address
#   10.209.0.1:7880      wispd's own listener on the bridge address
#   192.168.1.10:8080   the same host service via the host's LAN address
#   100.64.0.10:7788  a host service on the tailnet address
#   1.1.1.1:443          the internet, for contrast
for t in 10.209.0.1:7940 10.209.0.1:8080 10.209.0.1:7880 192.168.1.10:8080 100.64.0.10:7788 1.1.1.1:443; do
  h=${t%:*} p=${t#*:}
  if timeout 4 bash -c "exec 3<>/dev/tcp/$h/$p" 2>/dev/null; then echo "$t CONNECTED"; else echo "$t FAILED rc=$?"; fi
done
curl -sS -m 5 -o /dev/null -w "mcp POST %{http_code}\n" -X POST -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' http://10.209.0.1:7940/mcp \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"guest","version":"0"}}}' 2>&1
curl -sS -m 5 -o /dev/null -w "example.com %{http_code}\n" https://example.com 2>&1
