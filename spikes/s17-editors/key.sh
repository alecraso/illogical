#!/usr/bin/env bash
# S17: send keys to a TUI run.
# usage: key.sh <name> <keys>      escapes as tui.py takes them: <enter> <down> <esc> \r ...
#        key.sh <name> @<prompt>   the text of prompts/<prompt>.txt (no Enter)
cd "$(dirname "$0")"
k=$2
case $k in @*) k=$(cat "prompts/${k#@}.txt") ;; esac
printf '%s' "$k" >"work/tui-$1.keys"
