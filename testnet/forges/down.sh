#!/usr/bin/env bash
#
# Remove the forges' containers and their volumes, and the tokens up.sh
# wrote. Touches nothing outside the illogical-testnet-forges project.
#
#   testnet/forges/down.sh [forgejo|gitlab|all]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FORGE="${1:-all}"

# These tests need Docker: without it they fail, unless ILLOGICAL_SKIP_DOCKER=1
# asks to skip them, which says loudly that nothing ran.
if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  if [ "${ILLOGICAL_SKIP_DOCKER:-}" = 1 ]; then
    echo "!!! ILLOGICAL_SKIP_DOCKER=1 and no Docker: NOTHING RAN (testnet/forges) !!!" >&2
    exit 0
  fi
  echo "FAIL: Docker is not available, and testnet/forges needs it (ILLOGICAL_SKIP_DOCKER=1 skips, running nothing)" >&2
  exit 1
fi

case "$FORGE" in
  forgejo | gitlab) profiles=("--profile" "$FORGE") ;;
  all) profiles=("--profile" forgejo "--profile" gitlab) ;;
  *) echo "usage: testnet/forges/down.sh [forgejo|gitlab|all]" >&2; exit 2 ;;
esac
docker compose -f "$HERE/compose.yaml" "${profiles[@]}" down -v --remove-orphans
case "$FORGE" in
  all) rm -rf "$HERE/.state" ;;
  gitlab) rm -f "$HERE/.state/gitlab.json" "$HERE/.state/gitlab-root-password" ;;
  *) rm -f "$HERE/.state/$FORGE.json" ;;
esac
echo "illogical testnet forges removed ($FORGE)"
