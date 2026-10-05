#!/usr/bin/env bash
#
# Remove everything the test stack started, every profile: containers, the
# stack's networks, and its state directory. Touches nothing outside the
# compose project (COMPOSE_PROJECT_NAME, default illogical-testnet).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
. "$HERE/env.sh"

# Docker is required: without it this fails. ILLOGICAL_SKIP_DOCKER=1 skips
# on purpose, and says loudly that nothing ran.
if ! docker info >/dev/null 2>&1; then
  if [ "${ILLOGICAL_SKIP_DOCKER:-}" = 1 ]; then
    echo "SKIP (ILLOGICAL_SKIP_DOCKER=1): Docker is not available, so the testnet did NOT run" >&2
    exit 0
  fi
  echo "FAIL: Docker is not available (\`docker info\` failed). The testnet needs it: start Docker, or set ILLOGICAL_SKIP_DOCKER=1 to skip on purpose." >&2
  exit 1
fi

docker compose -f "$HERE/compose.yaml" --profile ssh --profile control down -v --remove-orphans
rm -rf "$STATE"
echo "$TESTNET removed"
