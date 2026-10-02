#!/usr/bin/env bash
# S17: one edit turn against a running TUI run and fake IDE.
# usage: turn.sh <run> <prompt-name> <ide-name> <answer>
#   answer: ide:accept | ide:reject | ide:edit | ide:close   (appended to work/ide-<ide>.cmd)
#           key:<keys>                                      (typed into the terminal, as key.sh)
#           none                                            (just wait for the turn to end)
# Prints the end of the screen, hello.py, and the IDE frames logged during the turn.
set -u
cd "$(dirname "$0")"
run=$1 prompt=$2 ide=$3 answer=$4
log=work/ide-$ide.jsonl
start=$(wc -l <"$log")
./key.sh "$run" "@$prompt"
sleep 1
./key.sh "$run" "<enter>"
if [ "$answer" != none ]; then
  ./waitfor.sh "$run" ask 60 >/dev/null
  sleep 1
  case $answer in
    ide:*) echo "${answer#ide:}" >>"work/ide-$ide.cmd" ;;
    key:*) ./key.sh "$run" "${answer#key:}" ;;
  esac
fi
./waitfor.sh "$run" idle 60 | tail -12
echo "--- hello.py"
cat work/proj/hello.py
echo "--- IDE frames"
tail -n +"$((start + 1))" "$log" | work/venv/bin/python -c '
import json, sys
for l in sys.stdin:
    d = json.loads(l); m = d.get("msg")
    body = m if m else {k: v for k, v in d.items() if k not in ("t", "kind")}
    print(d["t"], d["kind"], json.dumps(body)[:200])'
