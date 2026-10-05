#!/usr/bin/env bash
#
# The desktop app updates itself (M46), in a fresh tart VM: an app built
# as 0.17.0 finds 0.17.1 in a manifest, downloads it, checks its signature
# and replaces itself; the new app carries a newer daemon and restarts its
# launch agent on it, and the panes keep running through both.
#
#   testnet/macos/update.sh          (after `just build`)
#   KEEP=1 ...                       leave the clone running
#
# It makes a throwaway updater key and builds the app twice with it
# (`just desktop-macos aarch64 --config ...`): 0.17.1, archived and signed as a
# release would be, carrying this tree's illogicald (`just build`); then
# 0.17.0, which the VM installs, carrying an illogicald one minor version
# older. That one builds from a copy of this tree with only the version
# changed (target/update-old, kept between runs). The host serves the
# manifest and the archive on the tart network (port 7757).
#   update  the app replaces itself with 0.17.1 and starts again
#   daemon  the new app finds the older daemon running, restarts its launch
#           agent on the bundle's newer one, and that one answers
#   panes   a running vim and a running build (a counter) carry on
#   bad     a manifest whose signature doesn't match is refused
# shellcheck disable=SC2016,SC2329 # strings run in the VM expand there; cleanup runs from the trap
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

# A throwaway updater key, no password.
cargo tauri signer generate --ci -p "" -w "$work/key" >/dev/null
pub=$(cat "$work/key.pub")
host_ip=192.168.64.1
config() {
  printf '{"version":"%s","plugins":{"updater":{"pubkey":"%s","dangerousInsecureTransportProtocol":true,"endpoints":["http://%s:%s/latest.json"]}}}' \
    "$1" "$pub" "$host_ip" "$PORT"
}
cd "$ROOT"
[ -x target/release/illogicald ] || { echo "no target/release/illogicald: just build" >&2; exit 2; }
# The daemon the release carries, and an older one for the installed app.
new=$(target/release/illogicald --version | awk '{print $2}')
old=$(echo "$new" | awk -F. '$2 > 0 { print $1 "." $2 - 1 ".0" }')
[ -n "$old" ] || { echo "can't make a version older than $new" >&2; exit 2; }
olddir=$ROOT/target/update-old
mkdir -p "$olddir/src"
rsync -a --delete --exclude /target --exclude /dist --exclude /.git --exclude /.claude --exclude node_modules \
  "$ROOT/" "$olddir/src/"
# The workspace's version, and the path dependencies that pin it.
for f in "$olddir/src/Cargo.toml" "$olddir"/src/crates/*/Cargo.toml; do
  sed -i '' -e "s/^version = \"$new\"/version = \"$old\"/" -e "s/\(illogical-[a-z0-9]* = { version = \)\"$new\"/\1\"$old\"/" "$f"
done
# mise runs from this tree (the copy isn't a trusted mise directory).
CARGO_TARGET_DIR=$olddir/target mise exec -- cargo build --release -q --manifest-path "$olddir/src/Cargo.toml" \
  -p illogicald -p illogical >"$work/build-daemon.log" 2>&1 || { tail -20 "$work/build-daemon.log"; exit 1; }
got=$("$olddir/target/release/illogicald" --version | awk '{print $2}')
[ "$got" = "$old" ] || { echo "the older daemon says $got, not $old" >&2; exit 1; }
echo "daemons: the installed app carries $old, the release $new"
# The release: 0.17.1, archived and signed.
TAURI_SIGNING_PRIVATE_KEY=$(cat "$work/key") TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" ILLOGICAL_DESKTOP_BINARIES=$ROOT/target/release \
  just desktop-macos aarch64 --config "'$(config 0.17.1)'" >"$work/build-new.log" 2>&1 || { tail -20 "$work/build-new.log"; exit 1; }
mkdir -p "$work/srv"
cp dist/illogical-desktop-macos-arm64.app.tar.gz dist/illogical-desktop-macos-arm64.app.tar.gz.sig "$work/srv/"
# What the VM installs: 0.17.0, with the older daemon.
ILLOGICAL_DESKTOP_BINARIES=$olddir/target/release \
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
wait_for 60 vs '$HOME/.local/bin/illogical ls >/dev/null 2>&1' || { echo "the daemon never answered" >&2; vs 'cat /tmp/app.log'; exit 1; }
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
vs '$HOME/.local/bin/illogical run -- vim /tmp/notes >/dev/null; $HOME/.local/bin/illogical run -- sh -c "i=0; while :; do i=\$((i+1)); echo \$i > /tmp/count; sleep 0.5; done" >/dev/null'
sleep 2
vim_pid=$(vs 'pgrep -x vim')
count0=$(vs 'cat /tmp/count')
# The daemon the launch agent runs, and the version it answers with.
daemon_pid() { vs 'pgrep -f "Contents/MacOS/illogicald$"' || true; }
answers() { vs 'curl -s -H "Authorization: Bearer $(cat $HOME/.local/state/illogical/local-token)" http://127.0.0.1:7681/api/host' | python3 -c 'import json, sys; print(json.load(sys.stdin).get("version", ""))' 2>/dev/null || true; }
daemon0=$(daemon_pid)
v0=$(answers)
[ "$v0" = "$old" ] || fail daemon "before the update the daemon answers ${v0:-nothing}, not $old"

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
on_new() { [ "$(answers)" = "$new" ]; }
if wait_for 60 on_new; then
  d=$(daemon_pid)
  if [ -n "$d" ] && [ "$d" != "$daemon0" ]; then
    pass daemon "the launch agent runs the bundle's $new now (pid $daemon0 -> $d): $(vs 'grep -m1 "updated the daemon" /tmp/app.log')"
  else
    fail daemon "it answers $new, but the daemon's pid is $daemon0 -> ${d:-none}"
  fi
else
  fail daemon "still ${v0:-nothing} after the update: $(vs 'grep "daemon" /tmp/app.log' | tail -3)"
fi
sleep 2
if [ "$(vs 'pgrep -x vim')" = "$vim_pid" ]; then pass panes "vim (pid $vim_pid) is still running"; else fail panes "vim went away"; fi
n=$(vs '$HOME/.local/bin/illogical --json ls' | python3 -c 'import json, sys; print(len(json.load(sys.stdin)))')
if [ "$n" -ge 2 ]; then pass panes "the new daemon lists the $n panes"; else fail panes "the new daemon lists $n panes"; fi
count1=$(vs 'cat /tmp/count')
if [ "$count1" -gt "$count0" ]; then pass panes "the build kept counting ($count0 -> $count1)"; else fail panes "the build stopped at $count1"; fi
exit $failed
