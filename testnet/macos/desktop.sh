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
#             linked into ~/.local/bin, and no arugulad install plist exists
#   pane      the window shows a pane: typing a command into it runs it
#   keys      Ctrl-W, T, N, Q and Tab reach the pane as bytes; Cmd-W closes
#             the pane and not the window; Cmd-T opens a tab; Cmd-M leaves
#             the app running and its window up; Cmd-H hides the app and
#             Cmd-Q quits it (#320), and its panes keep running
#   links     `open arugula://open?cwd=DIR` opens a tab there and shows
#             it; `open arugula://pane/%N` shows pane N
#   finder    M47: the app's service is registered; right-clicking a
#             folder in Finder and picking *New Arugula Tab Here* opens a
#             tab there in the running app and shows it; a .command file
#             opened with the app runs in a new tab
#   this      #323: the tray's *This machine*, clicked again and again,
#             brings forward the window already showing this machine: no
#             new window, no native tab
#   drag      #316: the page's bar moves the window as before, and so does
#             the top strip of a page with no drag markup (as an old
#             daemon's); a double-click there zooms it
#   tabs      Cmd-N opens a window as a native tab of the first; with two
#             and three tabs the page's bar sits below AppKit's tab bar
#             (#323); Cmd-Shift-W closes tabs back to one, and the strip
#             goes and the bar is back in the titlebar
#   hotkey    off by default; on (desktop.json), Ctrl-Option-Space hides
#             the app and brings it back
#   restart   the app quits and starts again; the daemon and its panes
#             stay (pids unchanged)
#
# And one not run by default, as it fetches a published release from
# GitHub instead of the .dmg (#318, #319):
#   installsh this tree's scripts/install.sh --app on the fresh account:
#             the app it puts in /Applications has no quarantine flag,
#             opens with no Gatekeeper window, and adopts install.sh's
#             daemon (no SMAppService agent); then, with /Applications not
#             writable, the same into ~/Applications. Gatekeeper's verdict
#             (spctl) is printed, not judged. ARUGULA_VERSION picks the
#             daemon's release (default: the latest), ARUGULA_APP_VERSION
#             the app's (default: app-latest).
#
# The .dmg is $ARUGULA_DMG, default dist/arugula-desktop-macos-arm64.dmg
# (`just desktop` on a Mac). The app
# is copied in without a quarantine flag: an ad-hoc signed app needs a
# person's right-click > Open past Gatekeeper, which only notarization
# (#177) removes. VM name: $ARUGULA_MACOS_VM (default arugula-macos-l).
# Exit codes: 0 every claim held (or no tart: a clean skip), 1 a claim
# failed, 2 usage.
# shellcheck disable=SC2016,SC2329 # strings run in the VM expand there; claims run as claim_$c
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ARUGULA_MACOS_VM:-arugula-macos-l}"
DMG="${ARUGULA_DMG:-$ROOT/dist/arugula-desktop-macos-arm64.dmg}"

# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"
claims=("$@")
[ ${#claims[@]} -gt 0 ] || claims=(install agent pane keys links finder this drag tabs hotkey restart)
if [[ " ${claims[*]} " == *" install "* ]]; then
  [ -f "$DMG" ] || { echo "no .dmg (ARUGULA_DMG, or build one: just desktop)" >&2; exit 2; }
fi
failed=0
v() { "$V" "$1" "$VM" "${@:2}"; }
vs() { v ssh "$@"; }
pass() { echo "[macos desktop $1] ok${2:+: $2}"; }
fail() { echo "[macos desktop $1] FAIL: $2" >&2; failed=1; }
wait_for() {
  local until=$((SECONDS + $1)); shift
  until "$@"; do
    [ "$SECONDS" -lt "$until" ] || return 1
    sleep 1
  done
}
# AppleScript in the VM's GUI session.
osa() { vs "osascript -e $(printf %q "$1")"; }
keys() { osa "tell application \"System Events\" to $1"; }
il() { vs "\$HOME/.local/bin/arugula $*"; }
# Pane ids, and one pane's field.
panes() { il --json ls | python3 -c 'import json, sys; [print(p["id"]) for p in json.load(sys.stdin)]'; }
pane_cwd() { il --json ls | python3 -c 'import json, sys; [print(p["cwd"]) for p in json.load(sys.stdin) if p["id"] == int(sys.argv[1])]' "$1"; }
# Conditions for wait_for, which runs its command again each time.
has_window() { [ "$(windows)" -ge 1 ]; }
more_panes_than() { [ "$(panes | wc -l)" -gt "$1" ]; }
fewer_panes_than() { [ "$(panes | wc -l)" -lt "$1" ]; }
app_running() { vs 'pgrep -x arugula-desktop >/dev/null'; }
app_gone() { ! app_running; }
windows() { osa 'tell application "System Events" to count windows of process "arugula-desktop"' 2>/dev/null || echo 0; }
visible() { osa 'tell application "System Events" to get visible of process "arugula-desktop"'; }
front() { osa 'tell application "System Events" to set frontmost of process "arugula-desktop" to true' >/dev/null; sleep 0.5; }
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
  v push "$DMG" /tmp/arugula.dmg
  if vs 'set -e; hdiutil attach -nobrowse -quiet -mountpoint /tmp/arugula-dmg /tmp/arugula.dmg
      test -L /tmp/arugula-dmg/Applications
      cp -R /tmp/arugula-dmg/arugula.app /Applications/
      hdiutil detach -quiet /tmp/arugula-dmg
      codesign --verify --deep --strict /Applications/arugula.app'; then
    pass install "the .dmg (with its Applications link) installed a validly signed app"
  else
    fail install "installing from the .dmg failed"
  fi
}

claim_agent() {
  vs 'open -a /Applications/arugula.app'
  if wait_for 60 vs 'launchctl print gui/$(id -u)/wtf.widgets.illogical.daemon 2>/dev/null | grep -q "state = running"'; then
    pass agent "the launch agent runs: $(vs 'launchctl print gui/$(id -u)/wtf.widgets.illogical.daemon | grep -m1 "program ="' | xargs)"
  else
    fail agent "the launch agent isn't running: $(vs 'launchctl print gui/$(id -u)/wtf.widgets.illogical.daemon 2>&1 | grep -E "state|exit"' | xargs)"
  fi
  local s; s=$(vs '/Applications/arugula.app/Contents/MacOS/arugula-desktop --agent status' || true)
  if [ "$s" = enabled ]; then pass agent "SMAppService: $s"; else fail agent "SMAppService: $s"; fi
  if vs 'sudo sfltool dumpbtm 2>/dev/null | grep -q "8.wtf.widgets.illogical.daemon"'; then
    pass agent "Login Items (BTM) lists it"
  else
    fail agent "BTM has no record of the agent"
  fi
  if wait_for 30 vs '$HOME/.local/bin/arugula ls >/dev/null 2>&1'; then
    pass agent "the daemon answers the linked CLI ($(vs 'readlink $HOME/.local/bin/arugula'))"
  else
    fail agent "the linked CLI in ~/.local/bin can't reach a daemon"
  fi
  if vs 'test ! -e $HOME/Library/LaunchAgents/arugulad.plist'; then
    pass agent "no arugulad install plist beside it"
  else
    fail agent "an arugulad install plist exists too"
  fi
  if vs 'pgrep -fl "Contents/MacOS/arugulad$" >/dev/null'; then
    pass agent "the daemon is the bundle's"
  else
    fail agent "the running daemon isn't the bundle's: $(vs 'pgrep -fl arugulad' | head -1)"
  fi
}

claim_pane() {
  wait_for 60 has_window || { fail pane "no window"; return; }
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
  local p; p=$(il --json run -- sh -c "$(printf %q "$(recorder "$dir")")" | python3 -c 'import json, sys; print(json.load(sys.stdin)["pane"])')
  vs "open 'arugula://pane/%25$p'"
  wait_for 15 shown "$dir" || { fail keys "the window never showed the recorder %$p"; return; }
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
  if [ "$got" = "$want" ]; then
    pass keys "Ctrl-W, T, N, Q and Tab reached the pane ($got)"
  else
    fail keys "the pane got '$got', want $want"
  fi
  # Cmd-W: the pane goes, the window stays.
  local n; n=$(panes | wc -l)
  keys 'keystroke "w" using command down'
  if wait_for 10 fewer_panes_than "$n" && ! panes | grep -qx "$p"; then
    pass keys "Cmd-W closed %$p"
  else
    fail keys "Cmd-W didn't close %$p ($(panes | xargs))"
  fi
  sleep 1
  if [ "$(windows)" -ge 1 ] && app_running; then pass keys "and the window stayed"; else fail keys "Cmd-W closed the window"; fi
  n=$(panes | wc -l)
  front
  keys 'keystroke "t" using command down'
  if wait_for 10 more_panes_than "$n"; then pass keys "Cmd-T opened a tab"; else fail keys "Cmd-T opened no tab"; fi
  # Cmd-M is the page's: the app stays up, shown, not minimized.
  front
  keys 'keystroke "m" using command down'
  sleep 1.5
  local vis mini
  vis=$(visible 2>/dev/null || echo gone)
  mini=$(osa 'tell application "System Events" to get value of attribute "AXMinimized" of window 1 of process "arugula-desktop"' 2>/dev/null || echo none)
  if app_running && [ "$vis" = true ] && [ "$mini" = false ]; then
    pass keys "Cmd-M reached the page: the app runs, shown, not minimized"
  else
    fail keys "after Cmd-M: running=$(app_running && echo yes || echo no) visible=$vis minimized=$mini"
  fi
  # Cmd-H and Cmd-Q are the Mac's (#320): the app menu hides and quits.
  front
  keys 'keystroke "h" using command down'
  sleep 1.5
  if [ "$(visible 2>/dev/null || echo gone)" = false ] && app_running; then
    pass keys "Cmd-H hid the app"
  else
    fail keys "after Cmd-H: running=$(app_running && echo yes || echo no) visible=$(visible 2>/dev/null || echo gone)"
  fi
  vs 'open -a /Applications/arugula.app'
  sleep 2
  front
  keys 'keystroke "q" using command down'
  if wait_for 10 app_gone; then
    pass keys "Cmd-Q quit the app"
  else
    fail keys "Cmd-Q left the app running"
    vs 'pkill -x arugula-desktop' || true
  fi
  # The daemon's panes outlive it; the app comes back for the next claims.
  n=$(panes | wc -l)
  vs 'open -a /Applications/arugula.app'
  wait_for 30 has_window || fail keys "the app didn't start again after Cmd-Q"
  sleep 3
  if [ "$(panes | wc -l)" = "$n" ]; then pass keys "and its $n panes kept running"; else fail keys "panes $n -> $(panes | wc -l) across Cmd-Q"; fi
}

claim_links() {
  local dir=/tmp/linked-dir
  vs "mkdir -p $dir"
  local n; n=$(panes | wc -l)
  vs "open 'arugula://open?cwd=%2Ftmp%2Flinked-dir'"
  if wait_for 15 more_panes_than "$n"; then
    local p; p=$(panes | tail -1)
    local cwd; cwd=$(pane_cwd "$p")
    case "$cwd" in
      */tmp/linked-dir) pass links "arugula://open?cwd= opened %$p in $cwd" ;;
      *) fail links "the new pane %$p is in $cwd" ;;
    esac
  else
    fail links "arugula://open?cwd= made no pane"
  fi
  local odir=/tmp/rec-link
  local q; q=$(il --json run -- sh -c "$(printf %q "$(recorder "$odir")")" | python3 -c 'import json, sys; print(json.load(sys.stdin)["pane"])')
  sleep 2
  if shown "$odir"; then
    fail links "%$q was shown before its link"
  else
    vs "open 'arugula://pane/%25$q'"
    if wait_for 15 shown "$odir"; then
      pass links "arugula://pane/%$q showed it ($(size_of "$odir"))"
    else
      fail links "arugula://pane/%$q didn't show it"
    fi
  fi
}

# The newest pane, once there are more than $1.
newest_after() { wait_for 20 more_panes_than "$1" && panes | tail -1; }

claim_finder() {
  vs '/System/Library/CoreServices/pbs -update; sleep 1'
  if vs '/System/Library/CoreServices/pbs -dump 2>/dev/null | grep -q "New Arugula Tab Here"'; then
    pass finder "the services list has New Arugula Tab Here"
  else
    fail finder "pbs doesn't list the app's service"
  fi
  local dir=/Users/admin/m47/some-project
  vs "mkdir -p $dir"
  local n; n=$(panes | wc -l)
  # Finder on ~/m47 in a list view, the folder selected by typing its name
  # (no Apple Events to Finder: those need a person's yes); then a
  # right-click on it and *New Arugula Tab Here* (finder-menu.js).
  vs 'open /Users/admin/m47'
  sleep 2
  keys 'keystroke "2" using command down'
  sleep 0.5
  keys 'keystroke "some-project"'
  sleep 1
  v push "$HERE/finder-menu.js" /tmp/finder-menu.js
  local how
  how=$(vs "osascript -l JavaScript /tmp/finder-menu.js some-project 'New Arugula Tab Here'" 2>&1) \
    || { fail finder "right-click > New Arugula Tab Here: $how"; return; }
  local p; p=$(newest_after "$n") || { fail finder "$how, but no new pane"; return; }
  local cwd; cwd=$(pane_cwd "$p")
  if [ "$cwd" = "$dir" ]; then
    pass finder "$how: %$p opened in $cwd"
  else
    fail finder "$how: the new pane %$p is in $cwd, not $dir"
  fi
  local front; front=$(osa 'tell application "System Events" to get name of first process whose frontmost is true')
  if [ "$front" = arugula-desktop ]; then
    pass finder "and the app came to the front with it"
  else
    fail finder "the front app is $front"
  fi
  # A .command file opened with the app (Open With, as a person picks it).
  vs "printf '#!/bin/sh\\necho ran > /tmp/m47-command\\nexec sleep 600\\n' > /Users/admin/m47/hello.command; chmod +x /Users/admin/m47/hello.command; rm -f /tmp/m47-command"
  n=$(panes | wc -l)
  vs 'open -a /Applications/arugula.app /Users/admin/m47/hello.command'
  if wait_for 20 vs 'grep -qx ran /tmp/m47-command'; then
    pass finder "a .command file opened with the app ran in a new pane ($(newest_after "$n" | sed 's/^/%/'))"
  else
    fail finder "opening hello.command with the app ran nothing"
  fi
}

# The tray's menu: click ITEM in it (tray.js, a mouse click on the icon).
tray() { vs "osascript -l JavaScript /tmp/tray.js $(printf %q "$1")" >/dev/null; }
# WINDOW_TOP STRIP_BOTTOM BAR_TOP TABS of the front window (tab-bar.js).
tab_bar() { vs 'osascript -l JavaScript /tmp/tab-bar.js'; }

claim_this() {
  v push "$HERE/tab-bar.js" "$HERE/tray.js" /tmp/
  front
  local before; before=$(windows)
  for _ in 1 2 3; do
    tray "This machine" || { fail this "no This machine in the tray's menu"; return; }
    sleep 2
  done
  local t tabs; t=$(tab_bar || echo "? ? ? ?"); tabs=$(echo "$t" | awk '{print $4}')
  if [ "$(windows)" = "$before" ] && [ "$tabs" = 0 ]; then
    pass this "three clicks on This machine left $before window, no native tabs"
  else
    fail this "after three clicks: $(windows) windows (was $before), tab bar $t"
  fi
  local front; front=$(osa 'tell application "System Events" to get name of first process whose frontmost is true')
  if [ "$front" = arugula-desktop ]; then pass this "and brought the app forward"; else fail this "the front app is $front"; fi
}

# The page's bar starts below the tab bar, with N native tabs.
bars_apart() {
  local t top strip bar tabs
  t=$(tab_bar) || { fail tabs "$1 tabs: $t"; return; }
  read -r top strip bar tabs <<<"$t"
  if [ "$tabs" -ne "$1" ]; then
    fail tabs "want $1 native tabs, the tab bar shows $tabs"
  elif [ "$bar" -ge "$strip" ]; then
    pass tabs "$1 tabs: the page's bar starts at $bar, below the tab bar's end at $strip (window top $top)"
  else
    fail tabs "$1 tabs: the page's bar starts at $bar, under the tab bar (ends at $strip, window top $top)"
  fi
}

# Moved: drag.js's frames (X0 Y0 W0 H0 X1 Y1 W1 H1) moved >= 100, 50.
moved() { awk '{ exit !($5 - $1 >= 100 && $6 - $2 >= 50) }' <<<"$1"; }
# Room to move down: AppKit keeps a window that exactly fills the screen
# above the Dock (1024x678 on the VM's screen) from going lower, so a drag
# moves it sideways only. Shorter, at the top left, it can.
room() { osa 'tell application "System Events" to tell window 1 of process "arugula-desktop" to set {position, size} to {{0, 30}, {1024, 560}}' >/dev/null; sleep 1; }

claim_drag() {
  v push "$HERE/drag.js" "$HERE/banners.js" /tmp/
  # The app's "App Background Activity" banner covers the bar's right end
  # for minutes after its first start.
  vs 'osascript -l JavaScript /tmp/banners.js' >/dev/null || true
  front
  room
  # The client's bar, its empty middle (.bar-fill): the page's own region,
  # found between its buttons (by now there are tabs enough to reach a
  # fixed spot).
  local f; f=$(vs 'osascript -l JavaScript /tmp/drag.js gap 16' || echo "")
  if moved "$f"; then pass drag "the page's bar moved the window ($f)"; else fail drag "dragging the page's bar: $f"; fi
  # A page with no drag markup: a stand-in for an old daemon, served in the
  # VM (nc, one answer at a time) at ARUGULA_URL. It has no /api/host,
  # so the app doesn't judge its protocol and shows it.
  vs 'osascript -e "quit app \"arugula\""; sleep 2; pkill -x arugula-desktop; true'
  vs 'printf "HTTP/1.0 200 OK\r\nContent-Type: text/html\r\n\r\n<!doctype html><title>bare</title><body style=\"margin:0;background:#444\"><p style=\"margin:80px\">no drag markup</p></body>\n" > /tmp/bare.http
      (nohup sh -c "while :; do nc -l 127.0.0.1 7799 < /tmp/bare.http >/dev/null; done" >/dev/null 2>&1 &)
      (ARUGULA_URL=http://127.0.0.1:7799 ARUGULA_LOCAL_TOKEN_FILE=/nonexistent \
        nohup /Applications/arugula.app/Contents/MacOS/arugula-desktop >/tmp/bare-app.log 2>&1 &)'
  if wait_for 30 has_window; then
    sleep 3
    front
    room
    f=$(vs 'osascript -l JavaScript /tmp/drag.js 400 12' || echo "")
    if moved "$f"; then
      pass drag "a page with no drag markup moved by its top strip ($f)"
    else
      fail drag "dragging a plain page's top strip: $f"
    fi
    f=$(vs 'osascript -l JavaScript /tmp/drag.js 400 200' || echo "")
    if [ -n "$f" ] && ! moved "$f"; then pass drag "below the strip the page keeps its mouse"; else fail drag "a drag in the page's body: $f"; fi
    f=$(vs 'osascript -l JavaScript /tmp/drag.js 400 12 double' || echo "")
    # Zoomed: a new frame, no smaller. A window that already fills the
    # VM's small screen keeps its size and moves into the zoomed frame.
    if [ -n "$f" ] && awk '{ exit !($7 * $8 >= $3 * $4 && ($1 != $5 || $2 != $6 || $3 != $7 || $4 != $8)) }' <<<"$f"; then
      pass drag "a double-click on the strip zoomed it ($f)"
    else
      fail drag "a double-click on the strip: $f"
    fi
  else
    fail drag "the app didn't show the plain page: $(vs 'tail -3 /tmp/bare-app.log')"
  fi
  vs 'pkill -x arugula-desktop; pkill -f "nc -l 127.0.0.1 7799"; pkill -f "while :; do nc"; sleep 1; open -a /Applications/arugula.app' || true
  wait_for 30 has_window || fail drag "the app didn't come back on the daemon"
  sleep 3
}

claim_tabs() {
  v push "$HERE/tab-bar.js" /tmp/tab-bar.js
  front
  keys 'keystroke "n" using command down'
  sleep 3
  # Native tabs: one window in front, its tab bar with two tabs.
  local tabs; tabs=$(osa 'tell application "System Events" to tell process "arugula-desktop" to count (radio buttons of tab group 1 of window 1)' 2>/dev/null || echo 0)
  if [ "$tabs" -ge 2 ]; then
    pass tabs "Cmd-N opened a window as a tab ($tabs tabs)"
  else
    fail tabs "Cmd-N: $tabs tabs, $(windows) windows"
  fi
  bars_apart 2
  front
  keys 'keystroke "n" using command down'
  sleep 3
  bars_apart 3
  # Cmd-Shift-W closes the window (Cmd-W is a pane's), back to one tab.
  for _ in 1 2; do
    front
    keys 'keystroke "w" using {command down, shift down}'
    sleep 2
  done
  local t top strip bar n
  t=$(tab_bar || echo "? ? ? ?")
  read -r top strip bar n <<<"$t"
  if [ "$n" = 0 ] && [ "$strip" = 0 ] && [ "$bar" -lt $((top + 40)) ] 2>/dev/null; then
    pass tabs "Cmd-Shift-W closed tabs back to one: no strip, the bar is in the titlebar again ($bar, window top $top)"
  else
    fail tabs "after closing back to one tab: $t (window top, strip end, bar top, tabs)"
  fi
  [ "$(windows)" -ge 1 ] || fail tabs "Cmd-Shift-W closed every window"
}

claim_hotkey() {
  front
  keys 'key code 49 using {control down, option down}'
  sleep 2
  if [ "$(visible)" = true ]; then
    pass hotkey "off by default: Ctrl-Option-Space left it alone"
  else
    fail hotkey "with no settings the hotkey hid the app"
  fi
  vs 'osascript -e "quit app \"arugula\""; sleep 2; pkill -x arugula-desktop; d=~/Library/Application\ Support/wtf.widgets.illogical; mkdir -p "$d"; echo "{\"hotkey_on\": true}" > "$d/desktop.json"; open -a /Applications/arugula.app'
  wait_for 30 has_window || { fail hotkey "the app didn't come back"; return; }
  sleep 3
  front
  keys 'key code 49 using {control down, option down}'
  sleep 2
  if [ "$(visible)" = false ]; then pass hotkey "on: Ctrl-Option-Space hid the app"; else fail hotkey "on: the app stayed visible"; fi
  keys 'key code 49 using {control down, option down}'
  sleep 2
  if [ "$(visible)" = true ]; then pass hotkey "and brought it back"; else fail hotkey "a second press didn't bring it back"; fi
}

claim_restart() {
  local before; before=$(vs 'pgrep -f "Contents/MacOS/arugulad" | sort | xargs')
  local n; n=$(panes | wc -l)
  vs 'pkill -x arugula-desktop; sleep 2; open -a /Applications/arugula.app'
  wait_for 30 has_window || { fail restart "the app didn't start again"; return; }
  sleep 3
  local after; after=$(vs 'pgrep -f "Contents/MacOS/arugulad" | sort | xargs')
  if [ "$before" = "$after" ] && [ "$(panes | wc -l)" = "$n" ]; then
    pass restart "the daemon and its $n panes outlived the app"
  else
    fail restart "daemon pids $before -> $after, panes $n -> $(panes | wc -l)"
  fi
}

# installsh: one install.sh --app run into DIR's arugula.app.
installsh_into() {
  local dir=$1 out
  out=$(vs "ARUGULA_VERSION=${ARUGULA_VERSION:-} ARUGULA_APP_VERSION=${ARUGULA_APP_VERSION:-} sh /tmp/install.sh --app" 2>&1) || {
    fail installsh "install.sh into $dir exited non-zero: $(tail -3 <<<"$out" | xargs)"; return 1; }
  if ! vs "test -d $dir/arugula.app"; then
    fail installsh "no app in $dir: $(grep -i app <<<"$out" | xargs)"; return 1
  fi
  if vs "! xattr -r $dir/arugula.app | grep -q com.apple.quarantine"; then
    pass installsh "$dir/arugula.app has no quarantine flag"
  else
    fail installsh "$dir/arugula.app is quarantined"
  fi
  echo "[macos desktop installsh] spctl, $dir: $(vs "spctl -a -vv $dir/arugula.app 2>&1" | xargs)"
  # Hung in Gatekeeper (#315), a binary never gets past dyld: give it 10s.
  if vs "$dir/arugula.app/Contents/MacOS/arugulad --version & p=\$!; for i in \$(seq 20); do kill -0 \$p 2>/dev/null || { wait \$p; exit \$?; }; sleep 0.5; done; kill \$p; exit 1" >/dev/null; then
    pass installsh "the bundle's arugulad runs from $dir"
  else
    fail installsh "the bundle's arugulad hangs (or fails) in $dir"
  fi
  if wait_for 60 has_window; then
    pass installsh "install.sh opened the app from $dir: $(vs 'pgrep -fl arugula-desktop' | head -1)"
  else
    fail installsh "no window from $dir/arugula.app within 60s"
  fi
  local gk; gk=$(osa 'tell application "System Events" to count windows of process "CoreServicesUIAgent"' 2>/dev/null || echo 0)
  if [ "${gk:-0}" = 0 ]; then pass installsh "no Gatekeeper window"; else fail installsh "Gatekeeper shows $gk window(s)"; fi
}

claim_installsh() {
  v push "$ROOT/scripts/install.sh" /tmp/install.sh
  # An admin can write /Applications, so the first run goes there.
  installsh_into /Applications || return
  if vs 'launchctl print gui/$(id -u)/arugulad 2>/dev/null | grep -q "state = running"' \
    && ! vs 'launchctl print gui/$(id -u)/wtf.widgets.illogical.daemon >/dev/null 2>&1'; then
    pass installsh "the app adopted install.sh's daemon (no agent of its own)"
  else
    fail installsh "the app didn't adopt install.sh's daemon: $(vs 'launchctl list | grep -i arugula' | xargs)"
  fi
  if vs '$HOME/.local/bin/arugula ls >/dev/null 2>&1'; then
    pass installsh "the CLI reaches it ($(vs '$HOME/.local/bin/arugulad --version'))"
  else
    fail installsh "the CLI can't reach a daemon"
  fi
  # A user who can't write /Applications: install.sh puts it in ~/Applications.
  vs 'pkill -x arugula-desktop; sleep 2; sudo chmod 755 /Applications'
  installsh_into '$HOME/Applications'
  vs 'pkill -x arugula-desktop; sudo chmod 775 /Applications'
}

for c in "${claims[@]}"; do
  "claim_$c"
done
exit $failed
