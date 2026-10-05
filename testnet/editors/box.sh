#!/usr/bin/env bash
#
# The box for M28's Remote-SSH test.
#
#   testnet/editors/box.sh up      keys, ssh_config, the box, and illogicald
#                                  running there as illo
#   testnet/editors/box.sh down
#
# `up` writes testnet/editors/.state/ssh_config, which reaches the box as
# `m28-box` with strict host key checking (VS Code's Remote-SSH reads it).
# Needs the static build for this machine and ILLOGICAL_LOCAL_TOKEN_FILE.
#
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
STATE="$HERE/.state"
COMPOSE=(docker compose -f "$HERE/compose.yaml")
SSH_PORT="${ILLOGICAL_TESTNET_EDITORS_SSH_PORT:-17751}"
DAEMON_PORT="${ILLOGICAL_TESTNET_EDITORS_DAEMON_PORT:-17752}"

# These tests need Docker: without it they fail, unless ILLOGICAL_SKIP_DOCKER=1
# asks to skip them, which says loudly that nothing ran.
if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  if [ "${ILLOGICAL_SKIP_DOCKER:-}" = 1 ]; then
    echo "!!! ILLOGICAL_SKIP_DOCKER=1 and no Docker: NOTHING RAN (testnet/editors) !!!" >&2
    exit 0
  fi
  echo "FAIL: Docker is not available, and testnet/editors needs it (ILLOGICAL_SKIP_DOCKER=1 skips, running nothing)" >&2
  exit 1
fi

arch="$(uname -m)"; [ "$arch" = arm64 ] && arch=aarch64
export ILLOGICAL_TESTNET_BIN="${ILLOGICAL_TESTNET_BIN:-${CARGO_TARGET_DIR:-$ROOT/target}/$arch-unknown-linux-musl/release}"
ILLOGICAL_TESTNET_TOKEN_DIR="$(dirname "${ILLOGICAL_LOCAL_TOKEN_FILE:-/nonexistent/x}")"
export ILLOGICAL_TESTNET_TOKEN_DIR

case "${1:-}" in
  up)
    [ -x "$ILLOGICAL_TESTNET_BIN/illogicald" ] || { echo "no $ILLOGICAL_TESTNET_BIN/illogicald: just static $arch" >&2; exit 1; }
    [ -n "${ILLOGICAL_LOCAL_TOKEN_FILE:-}" ] || { echo "ILLOGICAL_LOCAL_TOKEN_FILE is not set" >&2; exit 1; }
    mkdir -p "$STATE"
    [ -f "$STATE/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C illogical-testnet-editors -f "$STATE/id_ed25519"
    [ -f "$STATE/box_host_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C m28-box -f "$STATE/box_host_ed25519"
    echo "[127.0.0.1]:$SSH_PORT $(cut -d' ' -f1,2 "$STATE/box_host_ed25519.pub")" > "$STATE/known_hosts"
    cat > "$STATE/ssh_config" <<CFG
# Written by testnet/editors/box.sh.
Host m28-box
  HostName 127.0.0.1
  Port $SSH_PORT
  User illo
  IdentityFile "$STATE/id_ed25519"
  IdentitiesOnly yes
  UserKnownHostsFile "$STATE/known_hosts"
  StrictHostKeyChecking yes
  BatchMode yes
  ConnectTimeout 10
CFG
    "${COMPOSE[@]}" up -d --build --force-recreate >&2
    for _ in $(seq 1 40); do ssh -F "$STATE/ssh_config" m28-box true 2>/dev/null && break; sleep 0.5; done
    # The daemon, as illo, in its usual state dir: the extension finds its
    # socket there as it would on any machine.
    # shellcheck disable=SC2029 # expanded here on purpose
    ssh -F "$STATE/ssh_config" m28-box "ILLOGICAL_LOCAL_TOKEN_FILE=/token/local-token nohup /opt/illogical/illogicald --listen 127.0.0.1:$DAEMON_PORT --name m28-box --shell 'bash --norc --noprofile' --no-manager-env --tailscale-socket /nonexistent/tailscaled.sock >/tmp/illogicald.log 2>&1 </dev/null &"
    for _ in $(seq 1 40); do
      ssh -F "$STATE/ssh_config" m28-box "test -S ~/.local/state/illogical/sock || test -s ~/.local/state/illogical/sock.path" && exit 0
      sleep 0.5
    done
    ssh -F "$STATE/ssh_config" m28-box cat /tmp/illogicald.log >&2 || true
    echo "illogicald didn't start on the box" >&2; exit 1
    ;;
  down)
    "${COMPOSE[@]}" down -v --remove-orphans >&2
    rm -rf "$STATE"
    ;;
  *) echo "usage: testnet/editors/box.sh up|down" >&2; exit 2 ;;
esac
