#!/usr/bin/env bash
#
# The daemon updates itself (#391), in a fresh tart VM: `POST
# /api/update/apply` and `illogicald update` download a release from a
# fake GitHub on the host, check it against SHA256SUMS and run its
# `install`, for the install script's launch agent and for the app's.
#
#   testnet/macos/selfupdate.sh      builds both daemons itself (debug)
#   KEEP=1 ...                       leave the clone running
#
# It builds this tree twice, as 0.23.98 (OLD, what the VM installs) and
# 0.23.99 (NEW, the release), from a copy with only the version changed
# (target/selfupdate, kept between runs). The host fakes the releases on
# the tart network (port 7758): `releases/latest` redirects to
# `releases/tag/vV` for the V in its `latest` file, and
# `releases/download/vV/` holds the archives and SHA256SUMS laid out as the
# release workflow lays them out. 0.24.0 is BAD: its SHA256SUMS lies.
# In order, all in one VM, as admin (logged in to the GUI):
#   check-agent  OLD, installed with `illogicald install -- --update-url
#                <fake>`, finds NEW: GET /api/update has latest NEW, apply
#   apply-agent  POST /api/update/apply: the daemon answers as NEW within
#                90 s, under a new pid in gui/UID/illogicald, and
#                ~/.local/bin/illogicald says NEW
#   panes-agent  a counter pane started before is the same process and
#                still counting
#   bad          `illogicald update -y` against BAD fails saying the
#                checksum doesn't match; the daemon is still NEW
#   check-app    after `illogicald uninstall`, a fake app bundle holding OLD
#                (/Applications/illogical.app/Contents/MacOS) and its agent
#                (wtf.widgets.illogical.daemon, as the app's plist) answer
#                as OLD and offer apply
#   apply-app    POST apply: it answers as NEW, run from ~/.local/bin under
#                the app's label, and there's no illogicald agent (plist or
#                service): no second daemon
#   panes-app    a counter pane started before carries on
#   downgrade    NEW in the bundle and OLD in ~/.local/bin: the agent runs
#                the bundle's NEW (it doesn't hand on to an older one)
# Exit codes: 0 every claim held, 1 a claim failed (or setup did), 2 usage.
# shellcheck disable=SC2016,SC2329 # strings run in the VM expand there; cleanup runs from the trap
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ILLOGICAL_MACOS_VM:-illogical-macos-su}"
PORT=7758
OLD=0.23.98
NEW=0.23.99
BAD=0.24.0
TARGET=aarch64-apple-darwin
host_ip=192.168.64.1
URL="http://$host_ip:$PORT/releases/latest"
# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"

failed=0
v() { "$V" "$1" "$VM" "${@:2}"; }
vs() { v ssh "$@"; }
pass() { echo "[macos selfupdate $1] ok${2:+: $2}"; }
fail() { echo "[macos selfupdate $1] FAIL: $2" >&2; failed=1; }
wait_for() {
  local until=$((SECONDS + $1)); shift
  until "$@"; do
    [ "$SECONDS" -lt "$until" ] || return 1
    sleep 1
  done
}

work=$(mktemp -d)
srv=
cleanup() {
  [ -z "$srv" ] || kill "$srv" 2>/dev/null || true
  rm -rf "$work"
  [ -n "${KEEP:-}" ] || v down >/dev/null
}
trap cleanup EXIT

# Both daemons, from a copy of this tree with only the version changed.
# One target dir, so the second build only rebuilds our crates.
sudir=$ROOT/target/selfupdate
mkdir -p "$sudir/src"
rsync -a --delete --exclude /target --exclude /dist --exclude /.git --exclude /.claude --exclude node_modules \
  "$ROOT/" "$sudir/src/"
mkdir -p "$sudir/src/web/dist"
for ver in $OLD $NEW; do
  sed -i '' -e "/^\[workspace.package\]/,/^\[/ s/^version = \".*\"/version = \"$ver\"/" "$sudir/src/Cargo.toml"
  (cd "$ROOT" && CARGO_TARGET_DIR=$sudir/target mise exec -- cargo build -q --manifest-path "$sudir/src/Cargo.toml" \
    -p illogicald -p illogical) >"$work/build-$ver.log" 2>&1 || { tail -20 "$work/build-$ver.log"; exit 1; }
  mkdir -p "$sudir/$ver"
  cp "$sudir/target/debug/illogicald" "$sudir/target/debug/illogical" "$sudir/$ver/"
  got=$("$sudir/$ver/illogicald" --version | awk '{print $2}')
  [ "$got" = "$ver" ] || { echo "the $ver build says $got" >&2; exit 1; }
done
echo "daemons: OLD $OLD, NEW $NEW (debug builds)"

# The fake releases.
srvdir=$work/srv
release() { # VERSION FROM-DIR [lie]
  local stem="illogical-$1-$TARGET" d="$srvdir/releases/download/v$1"
  mkdir -p "$d" "$work/stage/$stem"
  cp "$2/illogicald" "$2/illogical" "$work/stage/$stem/"
  tar -czf "$d/$stem.tar.gz" -C "$work/stage" "$stem"
  local sum; sum=$(shasum -a 256 "$d/$stem.tar.gz" | awk '{print $1}')
  [ -z "${3:-}" ] || sum=$(printf '0%.0s' $(seq 64))
  printf '%s  %s\n' "$sum" "$stem.tar.gz" >"$d/SHA256SUMS"
}
release $NEW "$sudir/$NEW"
release $BAD "$sudir/$NEW" lie
latest() { echo "$1" >"$srvdir/latest"; }
latest $NEW
cat >"$work/fake.py" <<'PY'
import http.server, os, sys
root, host, port = sys.argv[1], sys.argv[2], int(sys.argv[3])
class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k): super().__init__(*a, directory=root, **k)
    def do_GET(self):
        if self.path.rstrip("/") == "/releases/latest":
            v = open(os.path.join(root, "latest")).read().strip()
            self.send_response(302)
            self.send_header("Location", f"http://{host}:{port}/releases/tag/v{v}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        super().do_GET()
http.server.ThreadingHTTPServer((host, port), H).serve_forever()
PY

v down >/dev/null
v up >/dev/null
# The tart bridge is there once a VM runs; the server binds to it.
wait_for 60 sh -c "ifconfig | grep -q 'inet $host_ip '" || { echo "no $host_ip on this host" >&2; exit 1; }
(exec python3 -I "$work/fake.py" "$srvdir" "$host_ip" "$PORT" >"$work/http.log" 2>&1) &
srv=$!
vs 'mkdir -p /tmp/old /tmp/new'
v push "$sudir/$OLD/illogicald" "$sudir/$OLD/illogical" /tmp/old/
v push "$sudir/$NEW/illogicald" "$sudir/$NEW/illogical" /tmp/new/
wait_for 60 vs "curl -sf -o /dev/null http://$host_ip:$PORT/releases/download/v$NEW/SHA256SUMS" \
  || { echo "the VM can't reach the fake releases" >&2; cat "$work/http.log"; kill -0 "$srv" || echo "(the server exited)"; exit 1; }

SOCK='$HOME/.local/state/illogical/sock'
api() { vs "curl -s --max-time 5 --unix-socket $SOCK http://illogical$1" 2>/dev/null || true; }
field() { python3 -c 'import json, sys
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for k in sys.argv[1].split("."): d = d.get(k) if isinstance(d, dict) else None
print("" if d is None else json.dumps(d) if not isinstance(d, str) else d)' "$1"; }
answers() { api /api/host | field version; }
update_status() { api /api/update; }
is() { [ "$(answers)" = "$1" ]; }
offers() { local s; s=$(update_status); [ "$(echo "$s" | field latest)" = "$1" ] && [ "$(echo "$s" | field apply)" = true ]; }
apply() { vs "curl -s --max-time 10 -X POST --unix-socket $SOCK http://illogical/api/update/apply"; }
svc_pid() { vs "launchctl print gui/\$(id -u)/$1 2>/dev/null | awk '\$1 == \"pid\" { print \$3 }'" || true; }
exe_of() { vs "ps -o comm= -p $1" 2>/dev/null || true; }
counter() { # SESSION FILE: a pane that counts into FILE
  vs "$2 run --session $1 \"i=0; while :; do i=\\\$((i+1)); echo \\\$i > $3; sleep 0.2; done\"" >/dev/null
}
# [>]: not the shell running pgrep, whose command line has this too.
pane_pid() { vs "pgrep -f '[>] $1; sleep'" | head -1 || true; }
count() { vs "cat $1" 2>/dev/null || echo 0; }
diag() { vs 'tail -15 ~/Library/Logs/illogicald.log; echo "--- update.log"; cat ~/.local/state/illogical/update.log 2>/dev/null' | sed 's/^/    /' >&2 || true; }

# --- The install script's launch agent.
vs "/tmp/old/illogicald install -- --update-url $URL" >"$work/install.log" 2>&1 \
  || { cat "$work/install.log"; exit 1; }
wait_for 30 is $OLD || { echo "the installed daemon never answered as $OLD: $(answers)" >&2; diag; exit 1; }
if wait_for 30 offers $NEW; then
  pass check-agent "$OLD finds $NEW: $(update_status)"
else
  fail check-agent "GET /api/update: $(update_status)"
fi

counter work '$HOME/.local/bin/illogical' /tmp/count
wait_for 10 vs 'test -s /tmp/count' || true
pane0=$(pane_pid /tmp/count)
pid0=$(svc_pid illogicald)
echo "before: daemon pid $pid0, counter pane pid ${pane0:-none}"
echo "POST /api/update/apply: $(apply)"
if wait_for 90 is $NEW; then
  pid1=$(svc_pid illogicald)
  bin=$(vs '$HOME/.local/bin/illogicald --version')
  if [ -n "$pid1" ] && [ "$pid1" != "$pid0" ] && [ "$bin" = "illogicald $NEW" ]; then
    pass apply-agent "answers as $NEW, gui/UID/illogicald pid $pid0 -> $pid1, ~/.local/bin/illogicald says $bin"
  else
    fail apply-agent "answers as $NEW, but pid $pid0 -> ${pid1:-none}, ~/.local/bin/illogicald says $bin"
  fi
else
  fail apply-agent "still $(answers) after 90 s: $(update_status)"; diag
fi
c0=$(count /tmp/count); sleep 2; c1=$(count /tmp/count)
pane1=$(pane_pid /tmp/count)
if [ -n "$pane0" ] && [ "$pane1" = "$pane0" ] && [ "$c1" -gt "$c0" ]; then
  pass panes-agent "the counter (pid $pane0) is still counting ($c0 -> $c1)"
else
  fail panes-agent "counter pid ${pane0:-none} -> ${pane1:-none}, count $c0 -> $c1"
fi

latest $BAD
out=$(vs "ILLOGICAL_UPDATE_URL=$URL \$HOME/.local/bin/illogicald update -y" 2>&1) && rc=0 || rc=$?
if [ "$rc" -ne 0 ] && grep -q "doesn't match" <<<"$out" && is $NEW; then
  pass bad "refused (exit $rc): $(grep "doesn't match" <<<"$out" | head -1 | cut -c1-160); still $NEW"
else
  fail bad "exit $rc, answers $(answers): $out"
fi
latest $NEW

# --- The app's launch agent, faked: its bundle holding OLD, its plist.
vs '$HOME/.local/bin/illogicald uninstall' >/dev/null
vs 'pkill -f "[>] /tmp/count; sleep" || true; rm -f $HOME/.local/bin/illogicald* $HOME/.local/bin/illogical $HOME/.local/state/illogical/update-check.json'
vs 'set -e; sudo mkdir -p /Applications/illogical.app/Contents/MacOS; sudo chown -R $(id -un) /Applications/illogical.app
  cp /tmp/old/illogicald /tmp/old/illogical /Applications/illogical.app/Contents/MacOS/'
vs "cat > /tmp/wtf.widgets.illogical.daemon.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>wtf.widgets.illogical.daemon</string>
  <key>Program</key>
  <string>/Applications/illogical.app/Contents/MacOS/illogicald</string>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>ILLOGICAL_KEEP_PANES</key>
    <string>true</string>
    <key>ILLOGICAL_LOG_FILE</key>
    <string>~/Library/Logs/illogicald.log</string>
    <key>ILLOGICAL_UPDATE_URL</key>
    <string>$URL</string>
  </dict>
  <key>ExitTimeOut</key>
  <integer>15</integer>
</dict>
</plist>
PLIST
APP=wtf.widgets.illogical.daemon
vs "launchctl bootstrap gui/\$(id -u) /tmp/$APP.plist"
wait_for 30 is $OLD || { echo "the app's agent never answered as $OLD: $(answers)" >&2; diag; exit 1; }
if wait_for 30 offers $NEW; then
  pass check-app "the bundle's $OLD finds $NEW: $(update_status)"
else
  fail check-app "GET /api/update: $(update_status)"
fi

counter app /Applications/illogical.app/Contents/MacOS/illogical /tmp/count2
wait_for 10 vs 'test -s /tmp/count2' || true
pane0=$(pane_pid /tmp/count2)
pid0=$(svc_pid $APP)
echo "before: app agent pid $pid0 ($(exe_of "$pid0")), counter pane pid ${pane0:-none}"
echo "POST /api/update/apply: $(apply)"
if wait_for 90 is $NEW; then
  pid1=$(svc_pid $APP)
  exe=$(exe_of "$pid1")
  second=$(vs 'test -e $HOME/Library/LaunchAgents/illogicald.plist && echo plist; launchctl print gui/$(id -u)/illogicald >/dev/null 2>&1 && echo service; ps -axo command= | grep -cE "/illogicald$"')
  if [ -n "$pid1" ] && [ "$pid1" != "$pid0" ] && [[ "$exe" == */.local/bin/illogicald ]] && [ "$second" = 1 ]; then
    pass apply-app "answers as $NEW, $APP pid $pid0 -> $pid1 runs $exe; no illogicald agent, one daemon running"
  else
    fail apply-app "answers as $NEW, but $APP pid $pid0 -> ${pid1:-none} runs ${exe:-?}; agent/service/count: $(echo "$second" | tr '\n' ' ')"
  fi
else
  fail apply-app "still $(answers) after 90 s: $(update_status)"; diag
fi
c0=$(count /tmp/count2); sleep 2; c1=$(count /tmp/count2)
pane1=$(pane_pid /tmp/count2)
if [ -n "$pane0" ] && [ "$pane1" = "$pane0" ] && [ "$c1" -gt "$c0" ]; then
  pass panes-app "the counter (pid $pane0) is still counting ($c0 -> $c1)"
else
  fail panes-app "counter pid ${pane0:-none} -> ${pane1:-none}, count $c0 -> $c1"
fi

# --- A newer bundle than ~/.local/bin: no handing on to an older one.
# Replaced, not written over: macOS kills a signed binary changed in place
# (OS_REASON_CODESIGNING).
vs 'set -e; cd /Applications/illogical.app/Contents/MacOS; rm -f illogicald illogical; cp /tmp/new/illogicald /tmp/new/illogical .
  cp /tmp/old/illogicald $HOME/.local/bin/.illogicald.old && mv $HOME/.local/bin/.illogicald.old $HOME/.local/bin/illogicald'
pid0=$(svc_pid $APP)
vs "launchctl kickstart -k gui/\$(id -u)/$APP"
moved() { local p; p=$(svc_pid "$APP"); [ -n "$p" ] && [ "$p" != "$pid0" ] && is "$NEW"; }
if wait_for 60 moved; then
  pid1=$(svc_pid $APP)
  exe=$(exe_of "$pid1")
  if [ "$exe" = /Applications/illogical.app/Contents/MacOS/illogicald ]; then
    pass downgrade "the bundle's $NEW runs (pid $pid1, $exe), not ~/.local/bin's $OLD"
  else
    fail downgrade "it answers $NEW from $exe"
  fi
else
  fail downgrade "after kickstart it answers $(answers) (pid $pid0 -> $(svc_pid $APP))"; diag
fi
exit $failed
