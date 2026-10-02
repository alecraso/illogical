#!/usr/bin/env bash
# S17: wait until a run's screen matches, then print it (blank lines dropped).
# usage: waitfor.sh <name> <what> [timeout]
#   what: idle  -- the turn finished ("<verb>ed for Ns" in the last lines)
#         ask   -- a permission dialog is showing
#         re:<regex>
cd "$(dirname "$0")"
name=$1 what=$2 t=${3:-60}
case $what in
  idle) re='[A-Z][a-zé]+ed for [0-9]+s' ;;
  ask) re='Do you want to' ;;
  re:*) re=${what#re:} ;;
esac
f="work/tui-$name.screen"
end=$((SECONDS + t))
until [ -f "$f" ] && tail -12 "$f" | grep -qE "$re"; do
  [ $SECONDS -ge $end ] && { echo "(timed out waiting for $what)"; break; }
  sleep 1
done
sed '/^$/d' "$f"
