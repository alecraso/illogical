#!/usr/bin/env bash
# WebSocket upgrade against a *test* illogicald with chosen Host/Origin/identity
# headers, as `tailscale serve` or a browser would send them. Prints the status
# (101 = accepted). usage: origin-matrix.sh <port> [owner-login]
port=${1:-17699}
owner=${2:-owner@example.com}
ts=geek.tail1234.ts.net
try() { # label host origin login
  local args=(-s -o /dev/null -w '%{http_code}' --max-time 2 --http1.1
    -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13'
    -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' -H "Host: $2") # RFC 6455's sample key, gitleaks:allow
  [ -n "$3" ] && args+=(-H "Origin: $3")
  [ -n "$4" ] && args+=(-H "Tailscale-User-Login: $4")
  code=$(curl "${args[@]}" "http://127.0.0.1:$port/ws")
  printf '%-58s %s\n' "$1" "$code"
}
try "loopback, no Origin (CLI)"                       "127.0.0.1:$port" ""                          ""
try "app via serve :443"                              "$ts"             "https://$ts"               "$owner"
try "page on serve :10000 -> app's /ws"               "$ts"             "https://$ts:10000"         "$owner"
try "sandboxed iframe (opaque) -> /ws"                "$ts"             "null"                      "$owner"
try "http:// (port 80) page on the same name"         "$ts"             "http://$ts"                "$owner"
try "explicit :443 in Origin"                         "$ts"             "https://$ts:443"           "$owner"
try "suffix trick ($ts.evil.com)"                     "$ts"             "https://$ts.evil.com"      "$owner"
try "--allow-origin http://localhost:5173 (any Vite app)" "$ts"         "http://localhost:5173"     "$owner"
try "page on loopback daemon port itself"             "127.0.0.1:$port" "http://127.0.0.1:$port"    ""
try "other tailnet user"                              "$ts"             "https://$ts"               "friend@example.com"
try "serve :10000 routed straight at daemon (Host:port)" "$ts:10000"    "https://$ts:10000"         "$owner"
