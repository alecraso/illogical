#!/usr/bin/env bash
# serve.sh http ADDR NAME | unix SOCKET NAME — run the spike server in the
# background, logging to work/NAME.log (events) and work/NAME.out (stderr).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$here/work"
mode="$1" arg="$2" name="$3"
S14_LOG="$here/work/$name.log" nohup "$here/target/debug/s14-mcp" "$mode" "$arg" \
  >"$here/work/$name.out" 2>&1 &
echo $! >"$here/work/$name.pid"
sleep 0.5
cat "$here/work/$name.out"
