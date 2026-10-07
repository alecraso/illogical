#!/usr/bin/env bash
#
# web/safari (the key probe, #94, and the one-click invite, #137) against
# real Safari in a fresh tart VM: safaridriver runs in the VM's GUI session
# (the image logs admin in), its port comes here over ssh, and the servers
# the spec starts here go back to the same ports on the VM's loopback.
#
#   testnet/macos/safari.sh [playwright args]
#
# Needs `cargo build -p arugula-control` and web/dist (`just web`).
# SAFARI_DRIVER_PORT (default 7744) is the local end of safaridriver.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ARUGULA_MACOS_VM:-arugula-macos}"
PORT="${SAFARI_DRIVER_PORT:-7744}"

# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"

"$V" down "$VM" >/dev/null
"$V" up "$VM" >/dev/null
fwd=""
cleanup() {
  [ -z "$fwd" ] || kill "$fwd" 2>/dev/null || true
  [ -n "${KEEP:-}" ] || "$V" down "$VM" >/dev/null
}
trap cleanup EXIT

# --enable turns on Develop > Allow Remote Automation and authorizes
# safaridriver; it needs an admin's sudo once per machine.
"$V" ssh "$VM" "sudo safaridriver --enable && (nohup safaridriver --port 4444 >/tmp/safaridriver.log 2>&1 &) && sleep 1 && sw_vers -productVersion && defaults read /Applications/Safari.app/Contents/Info CFBundleShortVersionString"
ssh_cmd=$("$V" sshcmd "$VM")
# shellcheck disable=SC2086 # a command line, split on purpose
$ssh_cmd -N -o ExitOnForwardFailure=yes -L "127.0.0.1:$PORT:127.0.0.1:4444" &
fwd=$!
for _ in $(seq 1 50); do curl -sf "http://127.0.0.1:$PORT/status" >/dev/null && break; sleep 0.2; done
curl -sf "http://127.0.0.1:$PORT/status" >/dev/null || { echo "safaridriver in the VM never answered" >&2; exit 1; }

cd "$ROOT/web"
SAFARIDRIVER_URL="http://127.0.0.1:$PORT" SAFARI_TUNNEL="$ssh_cmd" pnpm exec playwright test -c safari.config.ts "$@"
