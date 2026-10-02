#!/usr/bin/env bash
# S18: start a TUI run in the background.
# usage: run.sh <name> <settings-name> <prompt-name|-> [extra claude args...]
# The prompt is prompts/<prompt-name>.txt ("-" for none). Stop a run with: kill "$(cat work/tui-<name>.pid)"
set -eu
cd "$(dirname "$0")"
name=$1 settings=$2 pname=$3; shift 3
prompt=""
[ "$pname" != "-" ] && prompt=$(cat "prompts/$pname.txt")
rm -f "work/tui-$name.screen" "work/tui-$name.screens"
setsid work/venv/bin/python tui.py "$name" "work/settings-$settings.json" "$prompt" "$@" >"work/tui-$name.out" 2>&1 &
echo $! >"work/tui-$name.pid"
echo "started $name pid $(cat "work/tui-$name.pid")"
