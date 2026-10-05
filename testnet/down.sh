#!/usr/bin/env bash
#
# Remove everything the test stack started, every profile: containers, the
# illogical-testnet networks, and testnet/.state. Touches nothing outside the
# illogical-testnet compose project.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

command -v docker >/dev/null 2>&1 || { echo "SKIP: Docker is not available"; exit 0; }
docker info >/dev/null 2>&1 || { echo "SKIP: Docker is not available"; exit 0; }

docker compose -f "$HERE/compose.yaml" --profile ssh down -v --remove-orphans
rm -rf "$HERE/.state"
echo "illogical testnet removed"
