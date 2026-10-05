# Sourced by the testnet scripts. COMPOSE_PROJECT_NAME (default
# illogical-testnet) names the stack: its containers, networks and state
# directory. Two stacks with different names don't touch each other.
# shellcheck shell=bash
TESTNET="${COMPOSE_PROJECT_NAME:-illogical-testnet}"
export COMPOSE_PROJECT_NAME="$TESTNET"
if [ "$TESTNET" = illogical-testnet ]; then STATE="$HERE/.state"; else STATE="$HERE/.state-$TESTNET"; fi
export ILLOGICAL_TESTNET_STATE="$STATE"

# Docker is required: without it a script fails, so a run that tested
# nothing never looks green. ILLOGICAL_SKIP_DOCKER=1 skips instead, saying
# loudly that nothing ran.
need_docker() {
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then return 0; fi
  if [ "${ILLOGICAL_SKIP_DOCKER:-}" = 1 ]; then
    echo "################################################################" >&2
    echo "## SKIPPED: Docker is not available and ILLOGICAL_SKIP_DOCKER=1. ##" >&2
    echo "## NOTHING RAN: no testnet claim was checked.                   ##" >&2
    echo "################################################################" >&2
    echo "SKIPPED: nothing ran (no Docker, ILLOGICAL_SKIP_DOCKER=1)"
    exit 0
  fi
  echo "FAIL: Docker is not available. The testnet needs it; ILLOGICAL_SKIP_DOCKER=1 skips (and nothing runs)." >&2
  exit 1
}
