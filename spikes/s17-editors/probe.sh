#!/usr/bin/env bash
# S17: start Claude Code, give it a few seconds, report which fake IDEs it connected to, stop it.
# usage: [S17_ENV=...] probe.sh <name> <seconds> [extra claude args...]
set -u
cd "$(dirname "$0")"
name=$1 secs=$2; shift 2
marks=()
for f in work/ide-*.jsonl; do marks+=("$f:$(wc -l <"$f")"); done
./run.sh "$name" - "$@" >/dev/null
sleep "$secs"
sed '/^$/d' "work/tui-$name.screen" | tail -6
for m in "${marks[@]}"; do
  f=${m%:*} n=${m##*:}
  echo "$f: $(tail -n +"$((n + 1))" "$f" | grep -c '"kind": "open"') connection(s), $(tail -n +"$((n + 1))" "$f" | grep -c '"kind": "reject"') rejected"
done
kill "$(cat "work/tui-$name.pid")" 2>/dev/null
sleep 1
