#!/bin/bash
# Restarts the S22 stub in the box (by pid file, so nothing else that names it is matched).
export PATH=$HOME/opt/node/bin:$PATH
cd ~/box
[ -s s22-stub.pid ] && kill "$(cat s22-stub.pid)" 2>/dev/null
for p in $(pgrep -x node); do tr '\0' ' ' < /proc/$p/cmdline | grep -q 's22-stub.mjs' && kill $p; done
sleep 0.5
S22_TOOLS="${S22_TOOLS:-}" setsid nohup node s22-stub.mjs > s22-stub.log 2>&1 < /dev/null &
echo $! > s22-stub.pid
sleep 1; cat s22-stub.log
