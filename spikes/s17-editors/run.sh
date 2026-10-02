#!/usr/bin/env bash
# S17: start a Claude Code TUI run in the background (S18's run.sh, with env).
# usage: [S17_ENV='{"CLAUDE_CODE_SSE_PORT":"1234"}'] run.sh <name> <prompt-name|-> [extra claude args...]
# The prompt is prompts/<prompt-name>.txt ("-" for none). Settings: work/settings-$S17_SETTINGS.json (default plain: {}).
# Stop a run with: kill "$(cat work/tui-<name>.pid)"
set -eu
cd "$(dirname "$0")"
name=$1 pname=$2; shift 2
prompt=""
[ "$pname" != "-" ] && prompt=$(cat "prompts/$pname.txt")
[ -f work/settings-plain.json ] || echo '{}' >work/settings-plain.json
rm -f "work/tui-$name.screen" "work/tui-$name.screens"
setsid work/venv/bin/python tui.py "$name" "work/settings-${S17_SETTINGS:-plain}.json" "$prompt" "$@" >"work/tui-$name.out" 2>&1 &
echo $! >"work/tui-$name.pid"
echo "started $name pid $(cat "work/tui-$name.pid")"
