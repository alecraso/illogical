#!/usr/bin/env bash
# Flood panes on a dev daemon and measure the S19 TUI drawing them.
# PANES=N (1-4, default 4), SECS (default 10). Needs tmux and a release build
# in ../../target/s19 (see README).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd); root=$here/../..
tui=$root/target/s19/release/s19-tui; cli=$root/target/debug/illogical; daemon=$root/target/debug/illogicald
panes=${PANES:-4}; secs=${SECS:-10}
work=$(mktemp -d); T="tmux -L s19-bench"
cleanup() { $T kill-server 2>/dev/null || true; [ -n "${dpid:-}" ] && kill "$dpid" 2>/dev/null || true; }
trap cleanup EXIT

ILLOGICAL_STATE_DIR=$work/state "$daemon" --listen 127.0.0.1:7791 --name s19 --no-manager-env \
  --shell "bash --norc -i" >"$work/daemon.log" 2>&1 &
dpid=$!
for _ in $(seq 50); do [ -s "$work/state/sock.path" ] || [ -S "$work/state/sock" ] && break; sleep 0.1; done
export ILLOGICAL_SOCK=$(cat "$work/state/sock.path" 2>/dev/null || echo "$work/state/sock")

# Each flooding pane waits for $work/go, then lists /usr with colors for $secs.
cat >"$work/flood.sh" <<F
#!/bin/bash
while [ ! -e $work/go ]; do sleep 0.1; done
end=\$((SECONDS+$secs)); while [ \$SECONDS -lt \$end ]; do ls -la --color=always /usr/bin /usr/lib | head -2000; done
F
chmod +x "$work/flood.sh"
first=$("$cli" run -- "$work/flood.sh"); ids=("$first")
[ "$panes" -ge 2 ] && ids+=("$("$cli" run --split "$first" -- "$work/flood.sh")")
[ "$panes" -ge 3 ] && ids+=("$("$cli" run --split "$first" -- "$work/flood.sh")")
[ "$panes" -ge 4 ] && ids+=("$("$cli" run --split "${ids[1]}" -- "$work/flood.sh")")
# Close the daemon's first shell, so the TUI opens on the flood tab.
"$cli" close %1 >/dev/null

$T -f /dev/null new-session -d -s t -x 200 -y 50 \
  "exec env ${S19_LEGACY:+S19_LEGACY=1} S19_STATS=$work/stats.txt TERM=xterm-256color $tui $ILLOGICAL_SOCK"
sleep 1
pid=$($T list-panes -t t -F '#{pane_pid}')
touch "$work/go"
t0=$(awk '{print $14+$15}' /proc/$pid/stat); sleep "$secs"; t1=$(awk '{print $14+$15}' /proc/$pid/stat)
rss=$(awk '/VmRSS/ {print $2}' /proc/$pid/status)
$T send-keys -t t C-]; $T send-keys -t t -l q; sleep 1
echo "panes $panes, ${secs}s: TUI cpu $(( (t1-t0) / secs ))% of a core, rss ${rss} kB"
cat "$work/stats.txt"
