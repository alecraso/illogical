#!/usr/bin/env bash
# M46 on Linux, in the Xvfb container (`just desktop-xvfb m46`): the app
# ($1, a debug build), a daemon ($2) and its CLI ($3), under Openbox (a
# window manager, for minimize, maximize and moving the window).
#
#   keys      every chord reaches the page: Ctrl-W, T, N, Q, Tab, F1, F10
#             (GTK's menu key) and Alt as Meta arrive in the pane as bytes
#   titlebar  the window has no decorations; the client's bar moves it
#             (drag), and its own buttons maximize and minimize it
#   links     `illogical-desktop 'illogical://open?cwd=DIR'` (what the
#             .desktop file's x-scheme-handler runs) opens a tab in DIR in
#             the running app and shows it; `illogical://pane/%N` shows pane N
#   hotkey    off by default (Ctrl+Alt+Space does nothing); on
#             (desktop.json), it hides the focused window and brings it back
#   nautilus  M47 (`just desktop-xvfb m47`): with the packages' .desktop
#             file claiming illogical:// and their Nautilus extension, a
#             right-click on a folder in Nautilus, *Open in illogical*,
#             opens a tab there in the running app and shows it; so does
#             the same item on a folder's background
#
# Claims run in that order; name some to run only those (nautilus runs
# only when named). KEEP=1 leaves everything running afterwards (for a
# look with xdotool and xwd).
set -euo pipefail
app=$1
daemon=$2
cli=$3
shift 3
claims=("$@")
[ ${#claims[@]} -gt 0 ] || claims=(keys titlebar links hotkey)
here=$(cd "$(dirname "$0")" && pwd)
desktop_dir=$(cd "$here/../../../crates/desktop" && pwd)
work=$(mktemp -d)
state=$work/state
export HOME=$work/home REC_DIR=$work/rec
mkdir -p "$state" "$HOME" "$REC_DIR"
pids=()
app_pid=
failed=0
trap 'kill "${pids[@]}" $app_pid 2>/dev/null || true; [ "$failed" = 0 ] && [ "${done_:-}" = 1 ] || { echo "--- app"; tail -60 "$work/app.log"; echo "--- daemon"; tail -40 "$work/daemon.log"; }' EXIT

ok() { echo "[m46 $1] ok${2:+: $2}"; }
bad() { echo "[m46 $1] FAIL: $2" >&2; failed=1; }
wait_for() {
  local until=$((SECONDS + $1)); shift
  until "$@"; do
    [ $SECONDS -lt $until ] || return 1
    sleep 0.2
  done
}

Xvfb :99 -screen 0 1280x900x24 -nolisten tcp >/dev/null 2>&1 &
pids+=($!)
export DISPLAY=:99
wait_for 10 xdpyinfo >/dev/null 2>&1 || { echo "no X server" >&2; exit 1; }
# One session bus for every launch: single-instance hands a second
# launch's link to the first over it.
eval "$(dbus-launch --sh-syntax)"
pids+=("$DBUS_SESSION_BUS_PID")
openbox >/dev/null 2>&1 &
pids+=($!)

chmod +x "$here/recorder.sh" 2>/dev/null || true
RUST_LOG=illogicald=info "$daemon" --listen 127.0.0.1:0 --state-dir "$state" --shell "$here/recorder.sh" \
  --no-manager-env --tailscale-socket /nonexistent/sock >"$work/daemon.log" 2>&1 &
pids+=($!)
wait_for 20 test -s "$state/listen" || { echo "the daemon didn't start" >&2; exit 1; }
il() { "$cli" --socket "$state/sock" "$@"; }

start_app() {
  ILLOGICAL_STATE_DIR=$state ILLOGICAL_NO_DAEMON_UPGRADE=1 ILLOGICAL_DESKTOP_SETTINGS=$work/desktop.json \
    WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 \
    "$app" >>"$work/app.log" 2>&1 &
  app_pid=$!
  wait_for 90 window >/dev/null || { echo "the app's window never showed" >&2; exit 1; }
  # The client is up once it has opened a tab and sized it.
  wait_for 60 sized_panes_at_least 1 || { echo "the window never showed a pane" >&2; exit 1; }
  sleep 1
  # A new profile opens Getting started over everything: close it (a
  # click on it, then Escape).
  xdotool windowactivate --sync "$(window)"
  click_terminal
  sleep 0.3
  xdotool key Escape
  sleep 0.5
}
stop_app() {
  kill "$app_pid" 2>/dev/null || true
  wait "$app_pid" 2>/dev/null || true
  app_pid=
}
# The app's window (visible), by its title.
window() { xdotool search --onlyvisible --name '^illogical$' 2>/dev/null | head -1; }
# Pane ids.
panes() { il --json ls | python3 -c 'import json, sys; [print(p["id"]) for p in json.load(sys.stdin)]'; }
# The recorder that started in $1, and its size ("rows cols").
rec_in() { grep -lxF "$1" "$REC_DIR"/*/cwd 2>/dev/null | head -1 | xargs -r dirname; }
# Conditions for wait_for, which runs its command again each time.
more_panes_than() { [ "$(panes | wc -l)" -gt "$1" ]; }
sized_in() { [ -n "$(size_in "$1")" ]; }
no_window() { [ -z "$(window)" ]; }
has_window() { [ -n "$(window)" ]; }
started_in() { [ -n "$(rec_in "$1")" ]; }
size_in() { local r; r=$(rec_in "$1"); [ -n "$r" ] && cat "$r/size" 2>/dev/null; }
# A window has shown it: a client sized its tab (a new pane is 24 80).
shown_in() { local z; z=$(size_in "$1"); [ -n "$z" ] && [ "$z" != "24 80" ]; }
sized_panes_at_least() { [ "$(cat "$REC_DIR"/*/size 2>/dev/null | grep -vcx '24 80')" -ge "$1" ]; }
newest_recording() { ls -td "$REC_DIR"/*/ 2>/dev/null | head -1; }
click_terminal() {
  local w; w=$(window)
  eval "$(xdotool getwindowgeometry --shell "$w")"
  xdotool mousemove --window "$w" $((WIDTH / 2)) $((HEIGHT / 2)) click 1
}

claim_keys() {
  local rec; rec=$(newest_recording)
  [ -n "$rec" ] || { bad keys "no recording pane"; return; }
  click_terminal
  sleep 0.5
  : >"$rec/keys"
  xdotool key --delay 80 ctrl+w ctrl+t ctrl+n ctrl+q Tab F1 F10 alt+x
  sleep 1.5
  # ^W ^T ^N ^Q, Tab, F1 (ESC O P), F10 (ESC [ 2 1 ~), Alt-x (ESC x).
  local got want=17140e11091b4f501b5b32317e1b78
  got=$(od -An -tx1 -v "$rec/keys" | tr -d ' \n')
  if [ "$got" = "$want" ]; then
    ok keys "Ctrl-W, T, N, Q, Tab, F1, F10 and Alt-x reached the pane ($got)"
  else
    bad keys "the pane got $got, want $want"
  fi
}

claim_titlebar() {
  local w; w=$(window)
  local hints; hints=$(xprop -id "$w" _MOTIF_WM_HINTS 2>/dev/null || true)
  # flags, functions, decorations, ...: decorations 0 means none.
  if echo "$hints" | grep -qE '= 0x2, 0x0, 0x0|= 2, 0, 0'; then
    ok titlebar "no decorations ($hints)"
  else
    bad titlebar "the window has decorations: ${hints:-no _MOTIF_WM_HINTS}"
  fi
  xdotool windowsize "$w" 1000 700 windowmove "$w" 100 100
  sleep 1
  eval "$(xdotool getwindowgeometry --shell "$w")"
  local x0=$X y0=$Y
  # Drag the bar's empty middle (.bar-fill) 150 right and 80 down.
  xdotool mousemove --window "$w" $((WIDTH - 260)) 16 mousedown 1 sleep 0.3 \
    mousemove_relative 50 30 sleep 0.2 mousemove_relative 100 50 sleep 0.3 mouseup 1
  sleep 1
  eval "$(xdotool getwindowgeometry --shell "$w")"
  if [ $((X - x0)) -ge 100 ] && [ $((Y - y0)) -ge 50 ]; then
    ok titlebar "dragging the bar moved the window from $x0,$y0 to $X,$Y"
  else
    bad titlebar "dragging the bar left the window at $X,$Y (was $x0,$y0)"
  fi
  # The client's buttons: minimize, maximize, close from the right, 40 wide.
  xdotool mousemove --window "$w" $((WIDTH - 60)) 16 click 1
  sleep 1.5
  eval "$(xdotool getwindowgeometry --shell "$w")"
  if [ "$WIDTH" -ge 1270 ] && [ "$HEIGHT" -ge 850 ]; then
    ok titlebar "its maximize button filled the screen (${WIDTH}x$HEIGHT)"
  else
    bad titlebar "after its maximize button the window is ${WIDTH}x$HEIGHT"
  fi
  xdotool mousemove --window "$w" $((WIDTH - 60)) 16 click 1
  sleep 1
  eval "$(xdotool getwindowgeometry --shell "$w")"
  xdotool mousemove --window "$w" $((WIDTH - 100)) 16 click 1
  sleep 1.5
  if xprop -id "$w" WM_STATE | grep -qi iconic; then
    ok titlebar "its minimize button minimized it"
  else
    bad titlebar "after its minimize button: $(xprop -id "$w" WM_STATE | tr '\n' ' ')"
  fi
  xdotool windowactivate --sync "$w" 2>/dev/null || xdotool windowmap "$w"
  sleep 1
}

claim_links() {
  local dir=$work/linked\ dir
  mkdir -p "$dir"
  local before; before=$(panes | wc -l)
  local enc; enc=$(python3 -c 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$dir")
  ILLOGICAL_STATE_DIR=$state "$app" "illogical://open?cwd=$enc" >>"$work/app.log" 2>&1 || true
  if ! wait_for 20 more_panes_than "$before"; then
    bad links "illogical://open?cwd=… made no pane"
    return
  fi
  local new; new=$(panes | tail -1)
  if wait_for 10 started_in "$dir"; then
    ok links "illogical://open?cwd=… opened %$new in $dir"
  else
    bad links "the new pane %$new didn't start in $dir: $(cat "$REC_DIR"/*/cwd | tr '\n' ' ')"
  fi
  if wait_for 15 shown_in "$dir"; then
    ok links "the window shows %$new ($(size_in "$dir"))"
  else
    bad links "the window didn't show %$new (size $(size_in "$dir"))"
  fi
  # A pane no window has shown yet, by link.
  local odir=$work/other
  mkdir -p "$odir"
  local other; other=$(il --json run --cwd "$odir" | python3 -c 'import json,sys; print(json.load(sys.stdin)["pane"])')
  wait_for 10 sized_in "$odir" || true
  sleep 1
  if shown_in "$odir"; then
    bad links "%$other was shown before its link ($(size_in "$odir")), so the link proves nothing"
  else
    ILLOGICAL_STATE_DIR=$state "$app" "illogical://pane/%25$other" >>"$work/app.log" 2>&1 || true
    if wait_for 15 shown_in "$odir"; then
      ok links "illogical://pane/%$other showed it ($(size_in "$odir"))"
    else
      bad links "illogical://pane/%$other didn't show it (size $(size_in "$odir"))"
    fi
  fi
  [ -n "$(window)" ] && [ "$(xdotool search --name '^illogical$' | wc -l)" -eq 1 ] \
    || bad links "the links opened another window: $(xdotool search --name '^illogical$' | wc -l)"
}

# The app as the packages install it: their .desktop file (from
# crates/desktop/linux/illogical.desktop, as tauri's bundler fills it in)
# claiming illogical://, and the Nautilus extension.
install_like_a_package() {
  local apps=$HOME/.local/share/applications bin=$work/bin
  mkdir -p "$apps" "$bin" "$HOME/.local/share/nautilus-python/extensions"
  printf '#!/bin/sh\nexec "%s" "$@"\n' "$app" >"$bin/illogical-desktop"
  chmod +x "$bin/illogical-desktop"
  sed -e '/{{[#/]if/d' -e "s|{{exec}}|$bin/illogical-desktop|g" -e 's|{{icon}}|illogical-desktop|' \
    -e 's|{{name}}|illogical|' -e 's|{{categories}}|Development;|' -e 's|{{comment}}|Terminals|' \
    -e 's|{{mime_type}}|x-scheme-handler/illogical|' "$desktop_dir/linux/illogical.desktop" >"$apps/illogical.desktop"
  desktop-file-validate "$apps/illogical.desktop" || bad nautilus "the .desktop file doesn't validate"
  update-desktop-database "$apps"
  xdg-mime default illogical.desktop x-scheme-handler/illogical
  cp "$desktop_dir/linux/nautilus/illogical.py" "$HOME/.local/share/nautilus-python/extensions/"
}

nautilus_window() { xdotool search --onlyvisible --class '[Nn]autilus' >/dev/null 2>&1; }

# Right-click DIR's entry in Nautilus (shown in its parent, selected) or,
# with `background`, an empty spot inside DIR, with the mouse; then click
# *Open in illogical* in the menu that opens.
nautilus_open() {
  local dir=$1 where=${2:-}
  # One window at a time: Nautilus is one process, whatever starts it.
  nautilus -q >/dev/null 2>&1 || true
  sleep 1
  if [ "$where" = background ]; then
    nautilus --new-window "$dir" >>"$work/nautilus.log" 2>&1 &
  else
    nautilus --new-window --select "$dir" >>"$work/nautilus.log" 2>&1 &
  fi
  pids+=($!)
  wait_for 60 nautilus_window || { echo "no Nautilus window: $(tail -3 "$work/nautilus.log")"; return 1; }
  sleep 3
  xdotool windowactivate --sync "$(xdotool search --onlyvisible --class '[Nn]autilus' | tail -1)"
  sleep 0.5
  local xy
  if [ "$where" = background ]; then
    xy=$("$here/click.py" --at --corner nautilus "Icon View" 10) || { echo "$xy"; return 1; }
  else
    xy=$("$here/click.py" --at nautilus "$(basename "$dir")" 10) || { echo "$xy"; return 1; }
  fi
  # shellcheck disable=SC2086 # "x y"
  xdotool mousemove $xy click 3
  sleep 1
  "$here/click.py" nautilus "Open in illogical" 10
}

claim_nautilus() {
  install_like_a_package
  local dir=$work/projects/some\ project
  mkdir -p "$dir"
  local before; before=$(panes | wc -l)
  if ! out=$(nautilus_open "$dir" 2>&1); then
    bad nautilus "right-click > Open in illogical: $out"
    return
  fi
  if wait_for 20 started_in "$dir"; then
    ok nautilus "right-click on a folder, $out: a new pane started in $dir"
  else
    bad nautilus "no pane started in $dir (panes $before -> $(panes | wc -l)): $(tail -5 "$work/app.log")"
  fi
  if wait_for 15 shown_in "$dir"; then
    ok nautilus "and the running app's window shows it ($(size_in "$dir"))"
  else
    bad nautilus "the window didn't show the new pane (size $(size_in "$dir"))"
  fi
  local inside=$work/projects/inside
  mkdir -p "$inside"
  if ! out=$(nautilus_open "$inside" background 2>&1); then
    bad nautilus "right-click inside a folder > Open in illogical: $out"
  elif wait_for 20 started_in "$inside"; then
    ok nautilus "right-click inside a folder: a new pane started in $inside"
  else
    bad nautilus "no pane started in $inside"
  fi
  if [ "$(xdotool search --name '^illogical$' | wc -l)" -eq 1 ]; then
    ok nautilus "one app, one window: the links went to the running app"
  else
    bad nautilus "$(xdotool search --name '^illogical$' | wc -l) windows"
  fi
}

claim_hotkey() {
  # Off: nothing happens.
  stop_app
  rm -f "$work/desktop.json"
  start_app
  local w; w=$(window)
  xdotool windowactivate --sync "$w"
  xdotool key ctrl+alt+space
  sleep 1.5
  if [ -n "$(window)" ]; then
    ok hotkey "off by default: Ctrl+Alt+Space left the window alone"
  else
    bad hotkey "with no settings Ctrl+Alt+Space hid the window"
  fi
  stop_app
  echo '{"hotkey_on": true}' >"$work/desktop.json"
  start_app
  w=$(window)
  xdotool windowactivate --sync "$w"
  sleep 1.5
  xdotool key ctrl+alt+space
  if wait_for 15 no_window; then
    ok hotkey "on: Ctrl+Alt+Space hid the focused window"
  else
    bad hotkey "on: Ctrl+Alt+Space didn't hide the window"
  fi
  sleep 1
  xdotool key ctrl+alt+space
  if wait_for 15 has_window; then
    ok hotkey "and brought it back"
  else
    bad hotkey "a second Ctrl+Alt+Space didn't bring the window back"
  fi
}

start_app
for c in "${claims[@]}"; do
  "claim_$c"
done
done_=1
if [ -n "${KEEP:-}" ]; then
  echo "KEEP: DISPLAY=$DISPLAY state=$state work=$work"
  echo "$work" >/tmp/m46-work
  sleep infinity
fi
exit $failed
