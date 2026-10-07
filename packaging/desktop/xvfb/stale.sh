#!/usr/bin/env bash
# #317 on Linux, in the Xvfb container (`just desktop-xvfb stale`): the app
# ($1, a debug build) never shows a daemon older than 0.19.0, and never
# updates it either (#392: the daemon updates itself). The older daemon is
# a stand-in (old-daemon.py: 0.8.0, no protocol) installed as the service:
# a ~/.local/bin/arugulad that runs it, and a unit that systemctl (a
# stand-in too, on PATH) runs.
#
#   stopped  the service is installed but not running: the app starts it,
#            finds 0.8.0 too old and stays on its setup page
#   running  the same with 0.8.0 already running
#
# Each asserts the app said why (0.8.0 runs, 0.19.0 is needed), that 0.8.0
# still answers (the app didn't replace it) and that it served no page,
# only the app's calls under /api and the watcher's /ws.
# shellcheck disable=SC2329 # claims run as claim_$c
set -euo pipefail
app=$1
shift
claims=("$@")
[ ${#claims[@]} -gt 0 ] || claims=(stopped running)
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
state=$work/state
export HOME=$work/home
mkdir -p "$state" "$HOME/.local/bin" "$HOME/.config/systemd/user" "$work/bin"
cp "$here/systemctl" "$work/bin/systemctl"
export PATH=$work/bin:$PATH
pids=()
app_pid=
failed=0
trap 'kill "${pids[@]}" $app_pid 2>/dev/null || true; systemctl --user stop arugulad.service || true; [ "$failed" = 0 ] || { echo "--- app"; tail -40 "$work/app.log"; echo "--- daemon"; tail -20 "$HOME/arugulad.log"; }' EXIT

ok() { echo "[stale $1] ok${2:+: $2}"; }
bad() { echo "[stale $1] FAIL: $2" >&2; failed=1; }
wait_for() {
  local until=$((SECONDS + $1)); shift
  until "$@"; do
    [ $SECONDS -lt $until ] || return 1
    sleep 0.5
  done
}

Xvfb :99 -screen 0 1280x900x24 -nolisten tcp >/dev/null 2>&1 &
pids+=($!)
export DISPLAY=:99
wait_for 10 xdpyinfo >/dev/null 2>&1 || { echo "no X server" >&2; exit 1; }
eval "$(dbus-launch --sh-syntax)"
pids+=("$DBUS_SESSION_BUS_PID")

listen=127.0.0.1:7781
echo "$listen" >"$state/listen"

# The older daemon as the service, stopped.
install_old() {
  systemctl --user stop arugulad.service
  rm -f "$state/old-requests.log" "$state/local-token"
  : >"$work/app.log"
  # Written beside it and renamed over it, as `arugulad install` does.
  cat >"$HOME/.local/bin/.arugulad.old" <<SH
#!/bin/sh
case "\$1" in
  --version) echo "arugulad 0.8.0" ;;
  install) exec systemctl --user restart arugulad.service ;;
  *) exec python3 $here/old-daemon.py "\$@" ;;
esac
SH
  chmod +x "$HOME/.local/bin/.arugulad.old"
  mv -f "$HOME/.local/bin/.arugulad.old" "$HOME/.local/bin/arugulad"
  printf '[Service]\nExecStart=%%h/.local/bin/arugulad --listen %s --state-dir %s\n' \
    "$listen" "$state" >"$HOME/.config/systemd/user/arugulad.service"
}
# The version answering at $listen.
answers() {
  python3 - "$listen" <<'PY' 2>/dev/null || true
import json, sys, urllib.request
print(json.load(urllib.request.urlopen(f"http://{sys.argv[1]}/api/host", timeout=3)).get("version", ""))
PY
}
answers_old() { [ "$(answers)" = 0.8.0 ]; }
said_why() { grep -q 'arugulad here is 0.8.0; this app needs 0.19.0 or newer' "$work/app.log"; }
start_app() {
  env ARUGULA_STATE_DIR="$state" WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    LIBGL_ALWAYS_SOFTWARE=1 "$app" >>"$work/app.log" 2>&1 &
  app_pid=$!
}
stop_app() {
  kill "$app_pid" 2>/dev/null || true
  wait "$app_pid" 2>/dev/null || true
  app_pid=
}
# The app's setup page said why, 0.8.0 was left alone, and it served no
# page: only /api (host, and the setup page's /api/update) and /ws.
setup_page() {
  if wait_for 60 said_why; then
    ok "$1" "the setup page says 0.8.0 runs and 0.19.0 is needed"
  else
    bad "$1" "the app never said 0.8.0 is too old: $(grep -i arugula "$work/app.log" | tail -3)"
  fi
  # Long enough for an update, if anything started one.
  sleep 15
  if answers_old; then
    ok "$1" "0.8.0 still answers: the app didn't update it"
  else
    bad "$1" "the daemon became '$(answers)'; the app must leave it alone (#392)"
  fi
  local pages
  pages=$(grep -vE '^(GET|POST) /(api/|ws)' "$state/old-requests.log" 2>/dev/null || true)
  if [ -z "$pages" ]; then
    ok "$1" "the window never loaded 0.8.0's page"
  else
    bad "$1" "0.8.0 served $(echo "$pages" | tr '\n' ' ')"
  fi
  grep -q '^GET /api/host' "$state/old-requests.log" 2>/dev/null || bad "$1" "the app never asked 0.8.0 its version"
}

claim_stopped() {
  install_old
  start_app
  wait_for 60 test -s "$state/old-requests.log" || bad stopped "the app never started the installed 0.8.0"
  setup_page stopped
  stop_app
}

claim_running() {
  install_old
  systemctl --user start arugulad.service
  wait_for 20 answers_old || { bad running "the stand-in didn't start"; return; }
  start_app
  setup_page running
  stop_app
}

for c in "${claims[@]}"; do
  "claim_$c"
done
exit $failed
