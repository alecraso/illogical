#!/usr/bin/env bash
#
# Bring the two hosts up or down, or take one off the network and back.
#
#   testnet/hosts/net.sh up            build nothing; start home and mac
#   testnet/hosts/net.sh offline mac   docker network disconnect
#   testnet/hosts/net.sh online mac    docker network connect, same address
#   testnet/hosts/net.sh down
#
# Needs the static build for this machine's architecture (`just static
# aarch64` on Apple silicon, `just static` on x86_64) and
# ARUGULA_LOCAL_TOKEN_FILE (the e2e tests' local token, which the
# daemons in the boxes take too).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
COMPOSE=(docker compose -f "$HERE/compose.yaml")
PROJECT="${COMPOSE_PROJECT_NAME:-arugula-testnet-hosts}"
NET="${PROJECT}_hosts"

# These tests need Docker: without it they fail, unless ARUGULA_SKIP_DOCKER=1
# asks to skip them, which says loudly that nothing ran.
if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  if [ "${ARUGULA_SKIP_DOCKER:-}" = 1 ]; then
    echo "!!! ARUGULA_SKIP_DOCKER=1 and no Docker: NOTHING RAN (testnet/hosts) !!!" >&2
    exit 0
  fi
  echo "FAIL: Docker is not available, and testnet/hosts needs it (ARUGULA_SKIP_DOCKER=1 skips, running nothing)" >&2
  exit 1
fi

arch="$(uname -m)"; [ "$arch" = arm64 ] && arch=aarch64
export ARUGULA_TESTNET_BIN="${ARUGULA_TESTNET_BIN:-${CARGO_TARGET_DIR:-$ROOT/target}/$arch-unknown-linux-musl/release}"
if [ -n "${ARUGULA_LOCAL_TOKEN_FILE:-}" ]; then
  ARUGULA_TESTNET_TOKEN_DIR="$(dirname "$ARUGULA_LOCAL_TOKEN_FILE")"
  export ARUGULA_TESTNET_TOKEN_DIR
fi

ip_of() {
  case "$1" in
    home) echo "${ARUGULA_TESTNET_HOME_IP:-172.31.77.10}" ;;
    mac) echo "${ARUGULA_TESTNET_MAC_IP:-172.31.77.11}" ;;
    *) echo "no host $1 (home, mac)" >&2; exit 2 ;;
  esac
}
container() { "${COMPOSE[@]}" ps -q "$1"; }

case "${1:-}" in
  up)
    [ -x "$ARUGULA_TESTNET_BIN/arugulad" ] || { echo "no $ARUGULA_TESTNET_BIN/arugulad: just static $arch" >&2; exit 1; }
    [ -n "${ARUGULA_TESTNET_TOKEN_DIR:-}" ] || { echo "ARUGULA_LOCAL_TOKEN_FILE is not set" >&2; exit 1; }
    "${COMPOSE[@]}" up -d --force-recreate >&2
    ;;
  offline) docker network disconnect "$NET" "$(container "$2")" ;;
  online) docker network connect --ip "$(ip_of "$2")" "$NET" "$(container "$2")" ;;
  down)
    export ARUGULA_TESTNET_BIN ARUGULA_TESTNET_TOKEN_DIR="${ARUGULA_TESTNET_TOKEN_DIR:-/nonexistent}"
    "${COMPOSE[@]}" down -v --remove-orphans >&2
    ;;
  *) echo "usage: testnet/hosts/net.sh up|offline HOST|online HOST|down" >&2; exit 2 ;;
esac
