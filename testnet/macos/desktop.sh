#!/usr/bin/env bash
#
# The macOS desktop app (M46) in a fresh tart VM (testnet/macos/vm.sh),
# with nobody at the keyboard: System Events types for the person, over
# ssh into the VM's logged-in admin session.
#
#   testnet/macos/desktop.sh [claim...]
#   KEEP=1 ...     leave the clone running
#
# Claims, in this order (each needs the ones before it):
#   install   the .dmg mounts and the app copies to /Applications
#   agent     the first start registers the daemon's launch agent through
#             SMAppService (BTM lists it), the daemon answers, the CLI is
#             linked into ~/.local/bin, and no illogicald install plist exists
#   pane      the window shows a pane: typing a command into it runs it
#   keys      Ctrl-W, T, N, Q and Tab reach the pane as bytes; Cmd-W closes
#             the pane and not the window; Cmd-T opens a tab; Cmd-Q, H and M
#             leave the app running and its window up
#   links     `open illogical://open?cwd=DIR` opens a tab there and shows
#             it; `open illogical://pane/%N` shows pane N
#   tabs      Cmd-N opens a window as a native tab of the first
#   hotkey    off by default; on (desktop.json), Ctrl-Option-Space hides
#             the app and brings it back
#   restart   the app quits and starts again; the daemon and its panes
#             stay (pids unchanged)
#
# The .dmg is $ILLOGICAL_DMG, default dist/illogical-desktop-macos-arm64.dmg
# (`just desktop` on a Mac). The app
# is copied in without a quarantine flag: an ad-hoc signed app needs a
# person's right-click > Open past Gatekeeper, which only notarization
# (#177) removes. VM name: $ILLOGICAL_MACOS_VM (default illogical-macos-l).
# Exit codes: 0 every claim held (or no tart: a clean skip), 1 a claim
# failed, 2 usage.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ILLOGICAL_MACOS_VM:-illogical-macos-l}"
DMG="${ILLOGICAL_DMG:-$ROOT/dist/illogical-desktop-macos-arm64.dmg}"

# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"
[ -f "$DMG" ] || { echo "no .dmg (ILLOGICAL_DMG, or build one: just desktop)" >&2; exit 2; }

claims=("$@")
[ ${#claims[@]} -gt 0 ] || claims=(install agent pane keys links tabs hotkey restart)
failed=0
v() { "$V" "$1" "$VM" "${@:2}"; }
vs() { v ssh "$@"; }
pass() { echo "[macos desktop $1] ok${2:+: $2}"; }
fail() { echo "[macos desktop $1] FAIL: $2" >&2; failed=1; }
wait_for() {
  local until=$((SECONDS + $1)); shift
  until "$@"; do
    [ $SECONDS -lt $until ] || return 1
    sleep 1
  done
}
# AppleScript in the VM's GUI session.
osa() { vs "osascript -e $(printf %q "$1")"; }
keys() { osa "tell application \"System Events\" to $1"; }
il() { vs "~/.local/bin/illogical $*"; }
# Pane ids, and one pane's field.
panes() { il --json ls | python3 -c 'import json, sys; [print(p["id"]) for p in json.load(sys.stdin)]'; }
pane_cwd() { il --json ls | python3 -c 'import json, sys; [print(p["cwd"]) for p in json.load(sys.stdin) if p["id"] == int(sys.argv[1])]' "$1"; }
app_running() { vs 'pgrep -x illogical-desktop >/dev/null'; }
windows() { osa 'tell application "System Events" to count windows of process "illogical-desktop"' 2>/dev/null || echo 0; }
front() { osa 'tell application "System Events" to set frontmost of process "illogical-desktop" to true' >/dev/null; sleep 0.5; }
# Size (rows cols) of the pane that recorded into DIR in the VM.
recorder() {
  # A pane that records what's typed into it and its size, under $1.
  printf '%s' "mkdir -p $1; cd $1; stty size > size; (while :; do stty size < /dev/tty > size.n && mv size.n size; sleep 0.3; done) & stty raw -echo; exec dd bs=1 of=$1/keys 2>/dev/null"
}
size_of() { vs "cat $1/size 2>/dev/null" || true; }
shown() { local z; z=$(size_of "$1"); [ -n "$z" ] && [ "$z" != "24 80" ]; }

v down >/dev/null
v up >/dev/null
[ -n "${KEEP:-}" ] || trap 'v down >/dev/null' EXIT

claim_install() {
  v push "$DMG" /tmp/illogical.dmg
  if vs 'set -e; hdiutil attach -nobrowse -quiet -mountpoint /tmp/illogical-dmg /tmp/illogical.dmg
      test -L /tmp/illogical-dmg/Applications
      cp -R /tmp/illogical-dmg/illogical.app /Applications/
      hdiutil detach -quiet /tmp/illogical-dmg
      codesign --verify --deep --strict /Applications/illogical.app'; then
    pass install "the .dmg (with its Applications link) installed a validly signed app"
  else
    fail install "installing from the .dmg failed"
  fi
}

claim_agent() {
  vs 'open -a /Applications/illogical.app'
  if wait_for 60 vs 'launchctl print gui/$(id -u)/wtf.widgets.illogical.daemon 2>/dev/null | grep -q "state = running"'; then
    pass agent "the launch agent runs: $(vs 'launchctl print gui/$(id -u)/wtf.widgets.illogical.daemon | grep -m1 "program ="' | xargs)"
  else
    fail agent "the launch agent isn't running: $(vs 'launchctl print gui/$(id -u)/wtf.widgets.illogical.daemon 2>&1 | grep -E "state|exit"' | xargs)"
  fi
  local s; s=$(vs '/Applications/illogical.app/Contents/MacOS/illogical-desktop --agent status' || true)
  [ "$s" = enabled ] && pass agent "SMAppService: $s" || fail agent "SMAppService: $s"
  if vs 'sudo sfltool dumpbtm 2>/dev/null | grep -q "8.wtf.widgets.illogical.daemon"'; then
    pass agent "Login Items (BTM) lists it"
  else
    fail agent "BTM has no record of the agent"
  fi
  if wait_for 30 vs '~/.local/bin/illogical ls >/dev/null 2>&1'; then
    pass agent "the daemon answers the linked CLI ($(vs 'readlink ~/.local/bin/illogical'))"
  else
    fail agent "~/.local/bin/illogical can't reach a daemon"
  fi
  vs 'test ! -e ~/Library/LaunchAgents/illogicald.plist' && pass agent "no illogicald install plist beside it" \
    || fail agent "an illogicald install plist exists too"
  vs 'pgrep -fl "Contents/MacOS/illogicald$" >/dev/null' && pass agent "the daemon is the bundle's" \
    || fail agent "the running daemon isn't the bundle's: $(vs 'pgrep -fl illogicald' | head -1)"
}

claim_pane() {
  wait_for 60 test "$(windows)" -ge 1 || { fail pane "no window"; return; }
  sleep 3
  front
  # Getting started opens on a new profile; Escape closes it.
  keys 'key code 53'
  sleep 1
  local marker=m46-$RANDOM
  # The first key after Escape can be lost to the dialog's exit: start
  # with a Return.
  keys 'key code 36'
  sleep 0.5
  keys "keystroke \"echo $marker > /tmp/typed\""
  keys 'key code 36'
  if wait_for 10 vs "grep -qx $marker /tmp/typed"; then
    pass pane "a command typed into the window ran in its pane"
  else
    fail pane "typing into the window ran nothing"
  fi
}

claim_keys() {
  local dir=/tmp/rec-keys
  local p; p=$(il --json run -- sh -c "$(printf %q "$(recorder $dir)")" | python3 -c 'import json, sys; print(json.load(sys.stdin)["pane"])')
  vs "open 'illogical://pane/%25$p'"
  wait_for 15 shown $dir || { fail keys "the window never showed the recorder %$p"; return; }
  front
  vs ": > $dir/keys"
  # ^W ^T ^N ^Q Tab, then Option-x (with Option as Meta off, a character).
  keys 'keystroke "w" using control down'
  keys 'keystroke "t" using control down'
  keys 'keystroke "n" using control down'
  keys 'keystroke "q" using control down'
  keys 'key code 48'
  sleep 1.5
  local got want=17140e1109
  got=$(vs "od -An -tx1 -v $dir/keys" | tr -d ' \n')
  [ "$got" = "$want" ] && pass keys "Ctrl-W, T, N, Q and Tab reached the pane ($got)" || fail keys "the pane got '$got', want $want"
  # Cmd-W: the pane goes, the window stays.
  local n; n=$(panes | wc -l)
  keys 'keystroke "w" using command down'
  if wait_for 10 test "$(panes | wc -l)" -lt "$n" && ! panes | grep -qx "$p"; then
    pass keys "Cmd-W closed %$p"
  else
    fail keys "Cmd-W didn't close %$p ($(panes | xargs))"
  fi
  sleep 1
  [ "$(windows)" -ge 1 ] && app_running && pass keys "and the window stayed" || fail keys "Cmd-W closed the window"
  n=$(panes | wc -l)
  front
  keys 'keystroke "t" using command down'
  wait_for 10 test "$(panes | wc -l)" -gt "$n" && pass keys "Cmd-T opened a tab" || fail keys "Cmd-T opened no tab"
  for k in q h m; do
    front
    keys "keystroke \"$k\" using command down"
    sleep 1.5
    local vis; vis=$(osa 'tell application "System Events" to get visible of process "illogical-desktop"' 2>/dev/null || echo gone)
    local mini; mini=$(osa 'tell application "System Events" to get value of attribute "AXMinimized" of window 1 of process "illogical-desktop"' 2>/dev/null || echo none)
    if app_running && [ "$vis" = true ] && [ "$mini" = false ]; then
      pass keys "Cmd-$(echo $k | tr a-z A-Z) reached the page: the app runs, shown, not minimized"
    else
      fail keys "after Cmd-$(echo $k | tr a-z A-Z): running=$(app_running && echo yes || echo no) visible=$vis minimized=$mini"
      vs 'open -a /Applications/illogical.app'; sleep 3
    fi
  done
}

claim_links() {
  local dir=/tmp/linked-dir
  vs "mkdir -p $dir"
  local n; n=$(panes | wc -l)
  vs "open 'illogical://open?cwd=%2Ftmp%2Flinked-dir'"
  if wait_for 15 test "$(panes | wc -l)" -gt "$n"; then
    local p; p=$(panes | tail -1)
    local cwd; cwd=$(pane_cwd "$p")
    case "$cwd" in
      */tmp/linked-dir) pass links "illogical://open?cwd= opened %$p in $cwd" ;;
      *) fail links "the new pane %$p is in $cwd" ;;
    esac
  else
    fail links "illogical://open?cwd= made no pane"
  fi
  local odir=/tmp/rec-link
  local q; q=$(il --json run -- sh -c "$(printf %q "$(recorder $odir)")" | python3 -c 'import json, sys; print(json.load(sys.stdin)["pane"])')
  sleep 2
  if shown $odir; then
    fail links "%$q was shown before its link"
  else
    vs "open 'illogical://pane/%25$q'"
    wait_for 15 shown $odir && pass links "illogical://pane/%$q showed it ($(size_of $odir))" || fail links "illogical://pane/%$q didn't show it"
  fi
}

claim_tabs() {
  front
  keys 'keystroke "n" using command down'
  sleep 3
  # Native tabs: one window in front, its tab bar with two tabs.
  local tabs; tabs=$(osa 'tell application "System Events" to tell process "illogical-desktop" to count (radio buttons of tab group 1 of window 1)' 2>/dev/null || echo 0)
  if [ "$tabs" -ge 2 ]; then
    pass tabs "Cmd-N opened a window as a tab ($tabs tabs)"
  else
    fail tabs "Cmd-N: $tabs tabs, $(windows) windows"
  fi
  vs 'screencapture -x /tmp/tabs.png' && v ssh 'cat /tmp/tabs.png' >"${TMPDIR:-/tmp}/illogical-macos-tabs.png" || true
}

claim_hotkey() {
  front
  keys 'key code 49 using {control down, option down}'
  sleep 2
  [ "$(osa 'tell application "System Events" to get visible of process "illogical-desktop"')" = true ] \
    && pass hotkey "off by default: Ctrl-Option-Space left it alone" || fail hotkey "with no settings the hotkey hid the app"
  vs 'osascript -e "quit app \"illogical\""; sleep 2; pkill -x illogical-desktop; d=~/Library/Application\ Support/wtf.widgets.illogical; mkdir -p "$d"; echo "{\"hotkey_on\": true}" > "$d/desktop.json"; open -a /Applications/illogical.app'
  wait_for 30 test "$(windows)" -ge 1 || { fail hotkey "the app didn't come back"; return; }
  sleep 3
  front
  keys 'key code 49 using {control down, option down}'
  sleep 2
  [ "$(osa 'tell application "System Events" to get visible of process "illogical-desktop"')" = false ] \
    && pass hotkey "on: Ctrl-Option-Space hid the app" || fail hotkey "on: the app stayed visible"
  keys 'key code 49 using {control down, option down}'
  sleep 2
  [ "$(osa 'tell application "System Events" to get visible of process "illogical-desktop"')" = true ] \
    && pass hotkey "and brought it back" || fail hotkey "a second press didn't bring it back"
}

claim_restart() {
  local before; before=$(vs 'pgrep -f "Contents/MacOS/illogicald" | sort | xargs')
  local n; n=$(panes | wc -l)
  vs 'pkill -x illogical-desktop; sleep 2; open -a /Applications/illogical.app'
  wait_for 30 test "$(windows)" -ge 1 || { fail restart "the app didn't start again"; return; }
  sleep 3
  local after; after=$(vs 'pgrep -f "Contents/MacOS/illogicald" | sort | xargs')
  [ "$before" = "$after" ] && [ "$(panes | wc -l)" = "$n" ] \
    && pass restart "the daemon and its $n panes outlived the app" || fail restart "daemon pids $before -> $after, panes $n -> $(panes | wc -l)"
}

for c in "${claims[@]}"; do
  "claim_$c"
done
exit $failed
