#!/usr/bin/env bash
# S17: runs inside a wisp sprite (pushed there by wisp.mjs).
#   setup   unpack code-server, make a project
#   start   start code-server as a sprite service on :8080 (no auth of its own), print ms to HTTP
#   mem     PSS of code-server's process tree (MB) and the VM's used memory
set -eu
cd /home/sprite
case $1 in
  setup)
    mkdir -p cs proj
    tar xzf cs.tgz -C cs --strip-components=1
    printf 'fn main() {\n    println!("hello from a sprite");\n}\n' >proj/main.rs
    nproc; free -m | sed -n 2p
    ;;
  start)
    t0=$(date +%s%N)
    sprite-env services create cs --cmd /home/sprite/cs/bin/code-server \
      --args "--bind-addr,0.0.0.0:8080,--auth,none,--disable-telemetry,--disable-update-check,--disable-workspace-trust,--user-data-dir,/home/sprite/csdata,--extensions-dir,/home/sprite/csext" \
      --http-port 8080 >/dev/null
    until curl -s -o /dev/null http://127.0.0.1:8080/; do sleep 0.05; done
    echo "http_ms $(( ($(date +%s%N) - t0) / 1000000 ))"
    ;;
  mem)
    root=$(pgrep -o -f "cs/lib/node /home/sprite/cs " || pgrep -o -f code-server)
    kids() { echo "$1"; for c in $(pgrep -P "$1"); do kids "$c"; done; }
    total=0
    for p in $(kids "$root"); do
      kb=$(awk '/^Pss:/ {print $2}' "/proc/$p/smaps_rollup" 2>/dev/null || echo 0)
      total=$((total + ${kb:-0}))
      echo "  $((${kb:-0} / 1024)) MB $(tr '\0' ' ' <"/proc/$p/cmdline" | cut -c1-80)"
    done
    echo "pss_mb $((total / 1024))"
    free -m | sed -n 2p
    ;;
esac
