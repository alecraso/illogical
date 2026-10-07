#!/usr/bin/env bash
#
# The daemon updates itself (#391), in a fresh tart VM: `POST
# /api/update/apply` and `arugulad update` download a release from a
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
#   check-agent  OLD, installed with `arugulad install -- --update-url
#                <fake>`, finds NEW: GET /api/update has latest NEW, apply
#   apply-agent  POST /api/update/apply: the daemon answers as NEW within
#                90 s, under a new pid in gui/UID/arugulad, and
#                ~/.local/bin/arugulad says NEW
#   panes-agent  a counter pane started before is the same process and
#                still counting
#   bad          `arugulad update -y` against BAD fails saying the
#                checksum doesn't match; the daemon is still NEW
#   check-app    after `arugulad uninstall`, a fake app bundle holding OLD
#                (/Applications/arugula.app/Contents/MacOS) and its agent
#                (wtf.widgets.illogical.daemon, as the app's plist) answer
#                as OLD and offer apply
#   apply-app    POST apply: it answers as NEW, run from ~/.local/bin under
#                the app's label, and there's no arugulad agent (plist or
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
VM="${ARUGULA_MACOS_VM:-arugula-macos-su}"
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
    -p arugulad -p arugula) >"$work/build-$ver.log" 2>&1 || { tail -20 "$work/build-$ver.log"; exit 1; }
  mkdir -p "$sudir/$ver"
  cp "$sudir/target/debug/arugulad" "$sudir/target/debug/arugula" "$sudir/$ver/"
  got=$("$sudir/$ver/arugulad" --version | awk '{print $2}')
  [ "$got" = "$ver" ] || { echo "the $ver build says $got" >&2; exit 1; }
done
echo "daemons: OLD $OLD, NEW $NEW (debug builds)"

# The fake releases.
srvdir=$work/srv
# Packed as a release is (scripts/dist-pack), so under the old name too: a
# daemon from before the rename fetches illogical-VERSION-TARGET (#505).
release() { # VERSION FROM-DIR [lie]
  local stem="arugula-$1-$TARGET" d="$srvdir/releases/download/v$1" f sum
  mkdir -p "$d/$stem"
  cp "$2/arugulad" "$2/arugula" "$d/$stem/"
  "$ROOT/scripts/dist-pack" "$d/$stem" >/dev/null
  rm -rf "${d:?}/$stem"
  : >"$d/SHA256SUMS"
  for f in "$stem.tar.gz" "illogical-$1-$TARGET.tar.gz"; do
    sum=$(shasum -a 256 "$d/$f" | awk '{print $1}')
    [ -z "${3:-}" ] || sum=$(printf '0%.0s' $(seq 64))
    printf '%s  %s\n' "$sum" "$f" >>"$d/SHA256SUMS"
  done
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
v push "$sudir/$OLD/arugulad" "$sudir/$OLD/arugula" /tmp/old/
v push "$sudir/$NEW/arugulad" "$sudir/$NEW/arugula" /tmp/new/
wait_for 60 vs "curl -sf -o /dev/null http://$host_ip:$PORT/releases/download/v$NEW/SHA256SUMS" \
  || { echo "the VM can't reach the fake releases" >&2; cat "$work/http.log"; kill -0 "$srv" || echo "(the server exited)"; exit 1; }

SOCK='$HOME/.local/state/arugula/sock'
api() { vs "curl -s --max-time 5 --unix-socket $SOCK http://arugula$1" 2>/dev/null || true; }
field() { python3 -c 'import json, sys
try: d = json.load(sys.stdin)
except Exception: sys.exit(0)
for k in sys.argv[1].split("."): d = d.get(k) if isinstance(d, dict) else None
print("" if d is None else json.dumps(d) if not isinstance(d, str) else d)' "$1"; }
answers() { api /api/host | field version; }
update_status() { api /api/update; }
is() { [ "$(answers)" = "$1" ]; }
offers() { local s; s=$(update_status); [ "$(echo "$s" | field latest)" = "$1" ] && [ "$(echo "$s" | field apply)" = true ]; }
apply() { vs "curl -s --max-time 10 -X POST --unix-socket $SOCK http://arugula/api/update/apply"; }
svc_pid() { vs "launchctl print gui/\$(id -u)/$1 2>/dev/null | awk '\$1 == \"pid\" { print \$3 }'" || true; }
exe_of() { vs "ps -o comm= -p $1" 2>/dev/null || true; }
counter() { # SESSION FILE: a pane that counts into FILE
  vs "$2 run --session $1 \"i=0; while :; do i=\\\$((i+1)); echo \\\$i > $3; sleep 0.2; done\"" >/dev/null
}
# [>]: not the shell running pgrep, whose command line has this too.
pane_pid() { vs "pgrep -f '[>] $1; sleep'" | head -1 || true; }
count() { vs "cat $1" 2>/dev/null || echo 0; }
diag() { vs 'tail -15 ~/Library/Logs/arugulad.log; echo "--- update.log"; cat ~/.local/state/arugula/update.log 2>/dev/null' | sed 's/^/    /' >&2 || true; }

# --- The install script's launch agent.
vs "/tmp/old/arugulad install -- --update-url $URL" >"$work/install.log" 2>&1 \
  || { cat "$work/install.log"; exit 1; }
wait_for 30 is $OLD || { echo "the installed daemon never answered as $OLD: $(answers)" >&2; diag; exit 1; }
if wait_for 30 offers $NEW; then
  pass check-agent "$OLD finds $NEW: $(update_status)"
else
  fail check-agent "GET /api/update: $(update_status)"
fi

counter work '$HOME/.local/bin/arugula' /tmp/count
wait_for 10 vs 'test -s /tmp/count' || true
pane0=$(pane_pid /tmp/count)
pid0=$(svc_pid arugulad)
echo "before: daemon pid $pid0, counter pane pid ${pane0:-none}"
echo "POST /api/update/apply: $(apply)"
if wait_for 90 is $NEW; then
  pid1=$(svc_pid arugulad)
  bin=$(vs '$HOME/.local/bin/arugulad --version')
  if [ -n "$pid1" ] && [ "$pid1" != "$pid0" ] && [ "$bin" = "arugulad $NEW" ]; then
    pass apply-agent "answers as $NEW, gui/UID/arugulad pid $pid0 -> $pid1, ~/.local/bin/arugulad says $bin"
  else
    fail apply-agent "answers as $NEW, but pid $pid0 -> ${pid1:-none}, ~/.local/bin/arugulad says $bin"
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
out=$(vs "ARUGULA_UPDATE_URL=$URL \$HOME/.local/bin/arugulad update -y" 2>&1) && rc=0 || rc=$?
if [ "$rc" -ne 0 ] && grep -q "doesn't match" <<<"$out" && is $NEW; then
  pass bad "refused (exit $rc): $(grep "doesn't match" <<<"$out" | head -1 | cut -c1-160); still $NEW"
else
  fail bad "exit $rc, answers $(answers): $out"
fi
latest $NEW

# --- The app's launch agent, faked: its bundle holding OLD, its plist.
vs '$HOME/.local/bin/arugulad uninstall' >/dev/null
vs 'pkill -f "[>] /tmp/count; sleep" || true; rm -f $HOME/.local/bin/arugulad* $HOME/.local/bin/arugula $HOME/.local/state/arugula/update-check.json'
vs 'set -e; sudo mkdir -p /Applications/arugula.app/Contents/MacOS; sudo chown -R $(id -un) /Applications/arugula.app
  cp /tmp/old/arugulad /tmp/old/arugula /Applications/arugula.app/Contents/MacOS/'
vs "cat > /tmp/wtf.widgets.illogical.daemon.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>wtf.widgets.illogical.daemon</string>
  <key>Program</key>
  <string>/Applications/arugula.app/Contents/MacOS/arugulad</string>
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
    <key>ARUGULA_KEEP_PANES</key>
    <string>true</string>
    <key>ARUGULA_LOG_FILE</key>
    <string>~/Library/Logs/arugulad.log</string>
    <key>ARUGULA_UPDATE_URL</key>
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

counter app /Applications/arugula.app/Contents/MacOS/arugula /tmp/count2
wait_for 10 vs 'test -s /tmp/count2' || true
pane0=$(pane_pid /tmp/count2)
pid0=$(svc_pid $APP)
echo "before: app agent pid $pid0 ($(exe_of "$pid0")), counter pane pid ${pane0:-none}"
echo "POST /api/update/apply: $(apply)"
if wait_for 90 is $NEW; then
  pid1=$(svc_pid $APP)
  exe=$(exe_of "$pid1")
  second=$(vs 'test -e $HOME/Library/LaunchAgents/arugulad.plist && echo plist; launchctl print gui/$(id -u)/arugulad >/dev/null 2>&1 && echo service; ps -axo command= | grep -cE "/arugulad$"')
  if [ -n "$pid1" ] && [ "$pid1" != "$pid0" ] && [[ "$exe" == */.local/bin/arugulad ]] && [ "$second" = 1 ]; then
    pass apply-app "answers as $NEW, $APP pid $pid0 -> $pid1 runs $exe; no arugulad agent, one daemon running"
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
vs 'set -e; cd /Applications/arugula.app/Contents/MacOS; rm -f arugulad arugula; cp /tmp/new/arugulad /tmp/new/arugula .
  cp /tmp/old/arugulad $HOME/.local/bin/.arugulad.old && mv $HOME/.local/bin/.arugulad.old $HOME/.local/bin/arugulad'
pid0=$(svc_pid $APP)
vs "launchctl kickstart -k gui/\$(id -u)/$APP"
moved() { local p; p=$(svc_pid "$APP"); [ -n "$p" ] && [ "$p" != "$pid0" ] && is "$NEW"; }
if wait_for 60 moved; then
  pid1=$(svc_pid $APP)
  exe=$(exe_of "$pid1")
  if [ "$exe" = /Applications/arugula.app/Contents/MacOS/arugulad ]; then
    pass downgrade "the bundle's $NEW runs (pid $pid1, $exe), not ~/.local/bin's $OLD"
  else
    fail downgrade "it answers $NEW from $exe"
  fi
else
  fail downgrade "after kickstart it answers $(answers) (pid $pid0 -> $(svc_pid $APP))"; diag
fi
exit $failed
