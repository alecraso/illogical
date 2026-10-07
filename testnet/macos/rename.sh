#!/usr/bin/env bash
#
# From illogical to Arugula (#505), in a fresh tart VM: the last illogical
# release, installed as the install script does, then this tree's
# `arugulad install` over it.
#
#   testnet/macos/rename.sh          builds this tree's daemon (debug)
#   OLD=0.25.0 ...                   the illogical release to start from
#   KEEP=1 ...                       leave the clone running
#
# Claims:
#   old       the illogical release runs under its launch agent (illogicald)
#   agent     after `arugulad install`: the arugulad agent runs the daemon,
#             the illogicald agent and its plist are gone
#   panes     a counter pane started under illogical is the same process
#             and still counting, and the new CLI lists it
#   names     ~/.local/bin/illogical and illogicald are links to arugula and
#             arugulad, and `illogical ls` still works
#   state     the state stays in ~/.local/state/illogical, with an arugula
#             link beside it
# Exit codes: 0 every claim held, 1 a claim failed (or setup did), 2 usage.
# shellcheck disable=SC2016,SC2329 # strings run in the VM expand there; cleanup runs from the trap
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ARUGULA_MACOS_VM:-arugula-macos-rename}"
OLD="${OLD:-0.25.0}"
TARGET=aarch64-apple-darwin
# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"

failed=0
v() { "$V" "$1" "$VM" "${@:2}"; }
vs() { v ssh "$@"; }
pass() { echo "[macos rename $1] ok${2:+: $2}"; }
fail() { echo "[macos rename $1] FAIL: $2" >&2; failed=1; }
wait_for() {
  local until=$((SECONDS + $1)); shift
  until "$@"; do
    [ "$SECONDS" -lt "$until" ] || return 1
    sleep 1
  done
}

work=$(mktemp -d)
cleanup() {
  rm -rf "$work"
  [ -n "${KEEP:-}" ] || v down >/dev/null
}
trap cleanup EXIT

# The old release, as GitHub has it.
stem=illogical-$OLD-$TARGET
curl -fsSL -o "$work/$stem.tar.gz" "https://github.com/arugula-salad/illogical/releases/download/v$OLD/$stem.tar.gz"
tar -xzf "$work/$stem.tar.gz" -C "$work"
# This tree's daemon and CLI.
mkdir -p "$ROOT/web/dist"
(cd "$ROOT" && mise exec -- cargo build -q -p arugulad -p arugula) >"$work/build.log" 2>&1 || { tail -20 "$work/build.log"; exit 1; }
tgt="${CARGO_TARGET_DIR:-$ROOT/target}/debug"
echo "old: illogical $OLD (release); new: this tree (debug)"

v up >/dev/null
vs 'mkdir -p /tmp/old /tmp/new'
v push "$work/$stem/illogicald" "$work/$stem/illogical" /tmp/old/
v push "$tgt/arugulad" "$tgt/arugula" /tmp/new/

OLDSOCK='$HOME/.local/state/illogical/sock'
alive() { vs "curl -s --max-time 5 --unix-socket $1 http://x/api/host" 2>/dev/null | grep -q '"version"'; }
svc_pid() { vs "launchctl print gui/\$(id -u)/$1 2>/dev/null | awk '\$1 == \"pid\" { print \$3 }'" || true; }
pane_pid() { vs "pgrep -f '[>] $1; sleep'" | head -1 || true; }
count() { vs "cat $1" 2>/dev/null || echo 0; }
diag() { vs 'tail -20 ~/Library/Logs/arugulad.log ~/Library/Logs/illogicald.log 2>/dev/null' | sed 's/^/    /' >&2 || true; }

# --- illogical, as its install script leaves it.
vs '/tmp/old/illogicald install' >"$work/old.log" 2>&1 || { cat "$work/old.log"; exit 1; }
if wait_for 30 alive "$OLDSOCK" && [ -n "$(svc_pid illogicald)" ]; then
  pass old "illogicald $OLD under gui/UID/illogicald (pid $(svc_pid illogicald))"
else
  fail old "it never answered"; diag; exit 1
fi
vs "\$HOME/.local/bin/illogical run --session work \"i=0; while :; do i=\\\$((i+1)); echo \\\$i > /tmp/count; sleep 0.2; done\"" >/dev/null
wait_for 10 vs 'test -s /tmp/count' || true
pane0=$(pane_pid /tmp/count)
echo "before: counter pane pid ${pane0:-none}"

# --- Arugula over it.
vs '/tmp/new/arugulad install' >"$work/new.log" 2>&1 || { cat "$work/new.log"; fail agent "arugulad install failed"; exit 1; }
sed 's/^/    /' "$work/new.log"
sleep 5
new_pid=$(svc_pid arugulad)
old_pid=$(svc_pid illogicald)
plists=$(vs 'ls ~/Library/LaunchAgents/ 2>/dev/null' | tr '\n' ' ')
if [ -n "$new_pid" ] && [ -z "$old_pid" ] && [[ "$plists" == *arugulad.plist* ]] && [[ "$plists" != *illogicald.plist* ]]; then
  pass agent "gui/UID/arugulad pid $new_pid; no illogicald agent; LaunchAgents: $plists"
else
  fail agent "arugulad pid ${new_pid:-none}, illogicald pid ${old_pid:-none}, LaunchAgents: $plists"; diag
fi

c0=$(count /tmp/count); sleep 2; c1=$(count /tmp/count)
pane1=$(pane_pid /tmp/count)
listed=$(vs '$HOME/.local/bin/arugula ls' 2>&1 || true)
if [ -n "$pane0" ] && [ "$pane1" = "$pane0" ] && [ "$c1" -gt "$c0" ] && [[ "$listed" == *work* ]]; then
  pass panes "the counter (pid $pane0) is still counting ($c0 -> $c1); arugula ls: $(echo "$listed" | head -1)"
else
  fail panes "counter pid ${pane0:-none} -> ${pane1:-none}, count $c0 -> $c1; arugula ls: $listed"; diag
fi

links=$(vs 'readlink ~/.local/bin/illogical; readlink ~/.local/bin/illogicald' | tr '\n' ' ')
old_ls=$(vs '$HOME/.local/bin/illogical ls' 2>&1 || true)
if [[ "$links" == *arugula*arugulad* ]] && [[ "$old_ls" == *work* ]]; then
  pass names "illogical, illogicald -> $links; illogical ls lists the pane"
else
  fail names "links: $links; illogical ls: $old_ls"
fi

state=$(vs 'readlink ~/.local/state/arugula; test -d ~/.local/state/illogical && test ! -L ~/.local/state/illogical && echo kept' | tr '\n' ' ')
if [[ "$state" == *illogical*kept* ]]; then
  pass state "the arugula state link -> $state"
else
  fail state "$state"
fi

exit "$failed"
