#!/usr/bin/env bash
#
# The S27 spike (spikes/s27-blocks/safari/safari.ts) against real Safari
# in a fresh tart VM. safaridriver runs in the VM's GUI session and its
# port comes here over ssh; the spike's control, daemon and dev server run
# here, and control's port goes back to the VM's loopback, where
# /etc/hosts sends control.test and the block names. The spike's test CA
# is trusted in the VM's System keychain.
#
#   testnet/macos/s27-safari.sh
#
# Build the spike first (in spikes/s27-blocks: `cargo build --release &&
# node build.mjs && target/release/s27 cert .run/cert`). S27_SAFARI_PORT
# (default 7753) is control's port on both ends; SAFARI_DRIVER_PORT
# (default 7744) the local end of safaridriver.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
SPIKE="$ROOT/spikes/s27-blocks"
V="$HERE/vm.sh"
VM="${ILLOGICAL_MACOS_VM:-illogical-macos}"
PORT="${SAFARI_DRIVER_PORT:-7744}"
CPORT="${S27_SAFARI_PORT:-7753}"

# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"

[ -f "$SPIKE/.run/cert/cert.pem" ] && [ -x "$SPIKE/target/release/s27" ] || { echo "build the spike first (see the top of $0)" >&2; exit 2; }

"$V" down "$VM" >/dev/null
"$V" up "$VM" >/dev/null
fwd=""
cleanup() {
  [ -z "$fwd" ] || kill "$fwd" 2>/dev/null || true
  [ -n "${KEEP:-}" ] || "$V" down "$VM" >/dev/null
}
trap cleanup EXIT

"$V" push "$VM" "$SPIKE/.run/cert/cert.pem" /tmp/s27-cert.pem
"$V" ssh "$VM" "set -e
  sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain /tmp/s27-cert.pem
  echo '127.0.0.1 control.test b-s27safari1.blocks.test b-s27safari2.blocks.test' | sudo tee -a /etc/hosts >/dev/null
  sudo dscacheutil -flushcache
  sudo safaridriver --enable
  (nohup safaridriver --port 4444 >/tmp/safaridriver.log 2>&1 &)
  sleep 1
  sw_vers -productVersion
  defaults read /Applications/Safari.app/Contents/Info CFBundleShortVersionString"
ssh_cmd=$("$V" sshcmd "$VM")
# shellcheck disable=SC2086 # a command line, split on purpose
$ssh_cmd -N -o ExitOnForwardFailure=yes -L "127.0.0.1:$PORT:127.0.0.1:4444" -R "127.0.0.1:$CPORT:127.0.0.1:$CPORT" &
fwd=$!
for _ in $(seq 1 50); do curl -sf "http://127.0.0.1:$PORT/status" >/dev/null && break; sleep 0.2; done
curl -sf "http://127.0.0.1:$PORT/status" >/dev/null || { echo "safaridriver in the VM never answered" >&2; exit 1; }

cd "$SPIKE"
SAFARIDRIVER_URL="http://127.0.0.1:$PORT" S27_SAFARI_PORT="$CPORT" node safari/safari.ts "$@"
