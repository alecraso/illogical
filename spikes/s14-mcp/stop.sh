#!/usr/bin/env bash
# stop.sh NAME... — stop servers started by serve.sh
here="$(cd "$(dirname "$0")" && pwd)"
for n in "$@"; do
  [ -f "$here/work/$n.pid" ] && kill "$(cat "$here/work/$n.pid")" 2>/dev/null
  rm -f "$here/work/$n.pid"
done
true
