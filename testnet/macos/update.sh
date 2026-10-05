#!/usr/bin/env bash
#
# The desktop app updates itself (M46), in a fresh tart VM: an app built
# as 0.17.0 finds 0.17.1 in a manifest, downloads it, checks its signature
# and replaces itself, and the panes keep running through it.
#
#   testnet/macos/update.sh          (after `just build`)
#   KEEP=1 ...                       leave the clone running
#
# It makes a throwaway updater key and builds the app twice with it
# (`just desktop-macos aarch64 --config ...`): 0.17.1, archived and signed as a
# release would be, then 0.17.0, which the VM installs. The host serves the
# manifest and the archive on the tart network (port 7757).
#   update  the app replaces itself with 0.17.1 and starts again
#   panes   a running vim and a running build (a counter) carry on
#   bad     a manifest whose signature doesn't match is refused
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ILLOGICAL_MACOS_VM:-illogical-macos-l}"
PORT=7757
# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"

# The worktree's tauri-cli and desktop build directory, when there.
[ -d "$ROOT/target/tools/bin" ] && export PATH="$ROOT/target/tools/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target/desktop-mac}"
failed=0
v() { "$V" "$1" "$VM" "${@:2}"; }
vs() { v ssh "$@"; }
pass() { echo "[macos update $1] ok${2:+: $2}"; }
fail() { echo "[macos update $1] FAIL: $2" >&2; failed=1; }
wait_for() {
  local until=$((SECONDS + $1)); shift
  until "$@"; do
    [ $SECONDS -lt $until ] || return 1
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

# A throwaway updater key, no password.
cargo tauri signer generate --ci -p "" -w "$work/key" >/dev/null
pub=$(cat "$work/key.pub")
host_ip=192.168.64.1
config() {
  printf '{"version":"%s","plugins":{"updater":{"pubkey":"%s","dangerousInsecureTransportProtocol":true,"endpoints":["http://%s:%s/latest.json"]}}}' \
    "$1" "$pub" "$host_ip" "$PORT"
}
cd "$ROOT"
# The release: 0.17.1, archived and signed.
TAURI_SIGNING_PRIVATE_KEY=$(cat "$work/key") TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
  just desktop-macos aarch64 --config "'$(config 0.17.1)'" >"$work/build-new.log" 2>&1 || { tail -20 "$work/build-new.log"; exit 1; }
mkdir -p "$work/srv"
cp dist/illogical-desktop-macos-arm64.app.tar.gz dist/illogical-desktop-macos-arm64.app.tar.gz.sig "$work/srv/"
# What the VM installs: 0.17.0.
just desktop-macos aarch64 --config "'$(config 0.17.0)'" >"$work/build-old.log" 2>&1 || { tail -20 "$work/build-old.log"; exit 1; }
rm -f dist/illogical-desktop-macos-arm64.app.tar.gz dist/illogical-desktop-macos-arm64.app.tar.gz.sig

manifest() {
  python3 - "$1" "$2" "http://$host_ip:$PORT/illogical-desktop-macos-arm64.app.tar.gz" >"$work/srv/latest.json" <<'PY'
import json, sys
v, sig, url = sys.argv[1:]
print(json.dumps({"version": v, "platforms": {"darwin-aarch64": {"signature": sig, "url": url}}}))
PY
}

v down >/dev/null
v up >/dev/null
v push dist/illogical-desktop-macos-arm64.dmg /tmp/illogical.dmg
vs 'set -e; hdiutil attach -nobrowse -quiet -mountpoint /tmp/d /tmp/illogical.dmg; cp -R /tmp/d/illogical.app /Applications/; hdiutil detach -quiet /tmp/d'
version() { vs 'defaults read /Applications/illogical.app/Contents/Info.plist CFBundleShortVersionString'; }
[ "$(version)" = 0.17.0 ] || { echo "installed $(version), not 0.17.0" >&2; exit 1; }

# A signature from another key first: refused.
cargo tauri signer generate --ci -p "" -w "$work/other" >/dev/null
TAURI_SIGNING_PRIVATE_KEY=$(cat "$work/other") TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
  cargo tauri signer sign "$work/srv/illogical-desktop-macos-arm64.app.tar.gz" >/dev/null 2>&1
manifest 0.17.1 "$(cat "$work/srv/illogical-desktop-macos-arm64.app.tar.gz.sig")"
(cd "$work/srv" && exec python3 -m http.server "$PORT" --bind "$host_ip" >"$work/http.log" 2>&1) &
srv=$!
start_app() {
  vs "(ILLOGICAL_UPDATE_RESTART=1 nohup /Applications/illogical.app/Contents/MacOS/illogical-desktop >>/tmp/app.log 2>&1 &)"
}
start_app
wait_for 60 vs '~/.local/bin/illogical ls >/dev/null 2>&1' || { echo "the daemon never answered" >&2; vs 'cat /tmp/app.log'; exit 1; }
if wait_for 40 vs 'grep -q "checking for an update" /tmp/app.log'; then
  if [ "$(version)" = 0.17.0 ]; then
    pass bad "a signature from another key was refused ($(vs 'grep -m1 "checking for an update" /tmp/app.log'))"
  else
    fail bad "it installed an update with the wrong signature"
  fi
else
  fail bad "no update check failed: $(vs 'cat /tmp/app.log' | tail -3)"
fi
vs 'pkill -x illogical-desktop' || true

# Work that must survive: vim, and a build that counts.
vs '~/.local/bin/illogical run -- vim /tmp/notes >/dev/null; ~/.local/bin/illogical run -- sh -c "i=0; while :; do i=\$((i+1)); echo \$i > /tmp/count; sleep 0.5; done" >/dev/null'
sleep 2
vim_pid=$(vs 'pgrep -x vim')
count0=$(vs 'cat /tmp/count')

# The real signature.
TAURI_SIGNING_PRIVATE_KEY=$(cat "$work/key") TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
  cargo tauri signer sign "$work/srv/illogical-desktop-macos-arm64.app.tar.gz" >/dev/null 2>&1
manifest 0.17.1 "$(cat "$work/srv/illogical-desktop-macos-arm64.app.tar.gz.sig")"
start_app
wait_for 20 vs 'pgrep -x illogical-desktop >/dev/null' || true
first=$(vs 'pgrep -x illogical-desktop' || echo none)
updated() { [ "$(version)" = 0.17.1 ]; }
if wait_for 90 updated; then
  pass update "the app replaced itself with 0.17.1"
else
  fail update "still $(version): $(vs 'tail -5 /tmp/app.log')"
fi
restarted() { local p; p=$(vs 'pgrep -x illogical-desktop' || true); [ -n "$p" ] && [ "$p" != "$first" ]; }
if wait_for 30 restarted; then
  pass update "and started again ($(vs 'grep -m1 "is in place" /tmp/app.log'))"
else
  fail update "it didn't start again after the update (was $first, now $(vs 'pgrep -x illogical-desktop' || echo none))"
fi
sleep 2
if [ "$(vs 'pgrep -x vim')" = "$vim_pid" ]; then pass panes "vim (pid $vim_pid) is still running"; else fail panes "vim went away"; fi
count1=$(vs 'cat /tmp/count')
[ "$count1" -gt "$count0" ] && pass panes "the build kept counting ($count0 -> $count1)" || fail panes "the build stopped at $count1"
exit $failed
