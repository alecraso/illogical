#!/usr/bin/env bash
#
# #317 on macOS, in a fresh tart VM: the app never shows a daemon older
# than 0.19.0 (one that reports no protocol), and never updates it either
# (#392: the daemon updates itself). Its window stays on the setup page,
# which says which version runs and which the app needs.
#
#   testnet/macos/stale-daemon.sh [claim...]   (after `just desktop`)
#   KEEP=1 ...                                 leave the clone running
#
# A released illogicald (OLD, default 0.8.0, the one #317 saw; it must be
# below 0.19.0) is installed as the service with its own `illogicald
# install` (~/.local/bin and ~/Library/LaunchAgents/illogicald.plist, as
# install.sh does), then the app from $ILLOGICAL_DMG (default
# dist/illogical-desktop-macos-arm64.dmg) starts:
#   stopped  the service is installed but not running: the app starts it,
#            finds it too old, says so, and OLD still answers
#   running  the same with OLD already running
# shellcheck disable=SC2016,SC2329 # strings run in the VM expand there; claims run as claim_$c
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ILLOGICAL_MACOS_VM:-illogical-macos-l}"
DMG="${ILLOGICAL_DMG:-$ROOT/dist/illogical-desktop-macos-arm64.dmg}"
OLD="${OLD:-0.8.0}"
# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"
[ -f "$DMG" ] || { echo "no .dmg (ILLOGICAL_DMG, or build one: just desktop)" >&2; exit 2; }

claims=("$@")
[ ${#claims[@]} -gt 0 ] || claims=(stopped running)
failed=0
v() { "$V" "$1" "$VM" "${@:2}"; }
vs() { v ssh "$@"; }
pass() { echo "[macos stale $1] ok${2:+: $2}"; }
fail() { echo "[macos stale $1] FAIL: $2" >&2; failed=1; }
wait_for() {
  local until=$((SECONDS + $1)); shift
  until "$@"; do
    [ "$SECONDS" -lt "$until" ] || return 1
    sleep 1
  done
}

work=$(mktemp -d)
trap 'rm -rf "$work"; [ -n "${KEEP:-}" ] || v down >/dev/null' EXIT
# The released daemon, checked against its release's SHA256SUMS.
tgz=illogical-$OLD-aarch64-apple-darwin.tar.gz
for f in "$tgz" SHA256SUMS; do
  curl -fsSL -o "$work/$f" "https://github.com/arugula-salad/illogical/releases/download/v$OLD/$f"
done
(cd "$work" && grep " $tgz\$" SHA256SUMS | shasum -a 256 -c - >/dev/null) || { echo "$tgz doesn't match SHA256SUMS" >&2; exit 1; }
mkdir "$work/old"
tar -xzf "$work/$tgz" -C "$work/old" --strip-components 1

v down >/dev/null
v up >/dev/null
v push "$DMG" /tmp/illogical.dmg
vs 'set -e; hdiutil attach -nobrowse -quiet -mountpoint /tmp/d /tmp/illogical.dmg; cp -R /tmp/d/illogical.app /Applications/; hdiutil detach -quiet /tmp/d'
vs 'mkdir -p /tmp/old'
v push "$work/old/illogicald" /tmp/old/illogicald
v push "$work/old/illogical" /tmp/old/illogical
vs 'xattr -dr com.apple.quarantine /tmp/old 2>/dev/null; chmod +x /tmp/old/illogicald /tmp/old/illogical; true'

answers() { vs 'curl -s http://127.0.0.1:7681/api/host' | python3 -c 'import json, sys; print(json.load(sys.stdin).get("version", ""))' 2>/dev/null || true; }
answers_old() { [ "$(answers)" = "$OLD" ]; }
nothing_answers() { [ -z "$(answers)" ]; }
said_why() { vs "grep -q 'illogicald here is $OLD; this app needs 0.19.0 or newer' /tmp/app.log"; }
quit_app() { vs 'osascript -e "quit app \"illogical\"" 2>/dev/null; sleep 2; pkill -x illogical-desktop; : >/tmp/app.log; true'; }
start_app() { vs '(nohup /Applications/illogical.app/Contents/MacOS/illogical-desktop >>/tmp/app.log 2>&1 &)'; }
# The older daemon as the service, as install.sh leaves it.
install_old() {
  vs '/tmp/old/illogicald install >/dev/null'
  wait_for 30 answers_old || fail "$1" "$OLD didn't answer after its install ($(answers))"
}
setup_page() {
  if wait_for 60 said_why; then
    pass "$1" "the setup page says $OLD runs and 0.19.0 is needed"
  else
    fail "$1" "the app never said $OLD is too old: $(vs 'grep illogical /tmp/app.log | tail -3')"
  fi
  # Long enough for an update, if anything started one.
  sleep 20
  if answers_old; then
    pass "$1" "$OLD still answers: the app didn't update it"
  else
    fail "$1" "the daemon became '$(answers)'; the app must leave it alone (#392)"
  fi
}

claim_stopped() {
  quit_app
  install_old stopped
  vs 'launchctl bootout gui/$(id -u)/illogicald 2>/dev/null; true'
  wait_for 15 nothing_answers || fail stopped "$OLD still answers after launchctl bootout"
  start_app
  wait_for 60 answers_old || fail stopped "the app didn't start the stopped $OLD service"
  setup_page stopped
}

claim_running() {
  quit_app
  install_old running
  start_app
  setup_page running
}

for c in "${claims[@]}"; do
  "claim_$c"
done
exit $failed
