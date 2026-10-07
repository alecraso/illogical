#!/usr/bin/env bash
#
# Remove everything the test stack started, every profile: containers, the
# stack's networks, and its state directory. Touches nothing outside the
# compose project (COMPOSE_PROJECT_NAME, default arugula-testnet).
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR source=env.sh
. "$HERE/env.sh"

need_docker

docker compose -f "$HERE/compose.yaml" --profile ssh --profile control --profile tailnet down -v --remove-orphans
rm -rf "$STATE"
echo "$TESTNET removed"
