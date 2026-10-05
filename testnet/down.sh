#!/usr/bin/env bash
#
# Remove everything the test stack started, every profile: containers, the
# illogical-testnet networks, and testnet/.state. Touches nothing outside the
# illogical-testnet compose project.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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

docker compose -f "$HERE/compose.yaml" --profile ssh down -v --remove-orphans
rm -rf "$HERE/.state"
echo "illogical testnet removed"
