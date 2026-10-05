#!/usr/bin/env bash
# #204: the desktop app follows a join made while it's open. Runs inside
# the Xvfb container (`just desktop-xvfb`): the app ($1), a daemon ($2,
# the static illogicald) and a stand-in control (fake-control.py).
#
# 1. The app opens on the daemon's own page (a client connects to it).
# 2. The machine joins control: the daemon's state directory gets what
#    `illogicald join` writes (its key and control.json), and the daemon
#    picks it up as it does after a real join. The window moves to the
#    app's sign-in, which (ILLOGICAL_SIGNIN_AUTO) asks control for a ticket
#    and opens control's page in the browser (ILLOGICAL_OPEN_LOG). Before
#    #204 the window stayed on the local page until a restart.
# 3. The machine leaves: the window goes back to the daemon's page.
set -euo pipefail
app=$1
daemon=$2
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
state=$work/state
export HOME=$work/home
mkdir -p "$state" "$HOME"
pids=()
trap 'kill "${pids[@]}" 2>/dev/null || true; [ "${ok:-}" = 1 ] || { echo "--- app"; cat "$work/app.log"; echo "--- daemon"; tail -40 "$work/daemon.log"; echo "--- control"; cat "$work/control.log" 2>/dev/null; }' EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
# wait_for SECONDS COMMAND...: until COMMAND succeeds.
wait_for() {
  local until=$((SECONDS + $1)); shift
  until "$@"; do
    [ $SECONDS -lt $until ] || return 1
    sleep 0.2
  done
}
clients() { grep -c "client connected" "$work/daemon.log" || true; }
more_clients_than() { [ "$(clients)" -gt "$1" ]; }

Xvfb :99 -screen 0 1280x900x24 -nolisten tcp >/dev/null 2>&1 &
pids+=($!)
export DISPLAY=:99

python3 "$here/fake-control.py" "$work/control-port" "$work/control.log" &
pids+=($!)
wait_for 10 test -s "$work/control-port" || fail "the stand-in control didn't start"
control="http://127.0.0.1:$(cat "$work/control-port")"

RUST_LOG=illogicald=info "$daemon" --listen 127.0.0.1:0 --state-dir "$state" --shell "bash --norc --noprofile" \
  --no-manager-env --tailscale-socket /nonexistent/sock >"$work/daemon.log" 2>&1 &
pids+=($!)
wait_for 20 test -s "$state/listen" || fail "the daemon didn't start"

# WebKitGTK's sandbox needs user namespaces a container doesn't give;
# Xvfb has no GPU.
ILLOGICAL_STATE_DIR=$state ILLOGICAL_OPEN_LOG=$work/opened ILLOGICAL_SIGNIN_AUTO=1 ILLOGICAL_NO_DAEMON_UPGRADE=1 \
  WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1 \
  dbus-run-session -- "$app" >"$work/app.log" 2>&1 &
pids+=($!)

# 1. The daemon's page.
wait_for 90 grep -q "client connected" "$work/daemon.log" || fail "the window never opened the daemon's page"
echo "ok: the window shows the daemon's page"
sleep 3
[ ! -s "$work/opened" ] || fail "it opened $(cat "$work/opened") before the machine joined"

# 2. Joined.
umask 077
{
  echo "illogical-device-key 1"
  echo "noise $(head -c32 /dev/urandom | od -An -tx1 | tr -d ' \n') $(head -c32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  echo "sign $(head -c32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
} >"$state/daemon.key"
zeros=$(printf '0%.0s' $(seq 64))
cat >"$state/control.json.tmp" <<JSON
{"url": "$control", "trust": {"account": "acct-test", "root": "root-test"},
 "cert": {"v": 1, "account": "acct-test", "device": "dev-test", "kind": "daemon", "name": "xvfb",
          "noise": "$zeros", "sign": "$zeros", "created": 1, "approver": "root-test", "sig": ""}}
JSON
mv "$state/control.json.tmp" "$state/control.json"
joined=$SECONDS
wait_for 30 grep -q "^$control/#app=t1" "$work/opened" 2>/dev/null || fail "the window didn't move to sign in after the join"
grep -q "^POST /auth/app " "$work/control.log" || fail "control wasn't asked for a ticket"
echo "ok: joined; the window moved to the app's sign-in and opened control's page in $((SECONDS - joined))s"

# 3. Left.
before=$(clients)
rm "$state/control.json"
wait_for 30 more_clients_than "$before" || fail "the window didn't go back to the daemon's page after leaving (clients: $before, then $(clients))"
echo "ok: left; the window is back on the daemon's page"
ok=1
