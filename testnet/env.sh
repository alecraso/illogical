# Sourced by the testnet scripts. COMPOSE_PROJECT_NAME (default
# arugula-testnet) names the stack: its containers, networks and state
# directory. Two stacks with different names don't touch each other.
# shellcheck shell=bash
TESTNET="${COMPOSE_PROJECT_NAME:-arugula-testnet}"
export COMPOSE_PROJECT_NAME="$TESTNET"
if [ "$TESTNET" = arugula-testnet ]; then STATE="$HERE/.state"; else STATE="$HERE/.state-$TESTNET"; fi
export ARUGULA_TESTNET_STATE="$STATE"

# Docker is required: without it a script fails, so a run that tested
# nothing never looks green. ARUGULA_SKIP_DOCKER=1 skips instead, saying
# loudly that nothing ran.
need_docker() {
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then return 0; fi
  if [ "${ARUGULA_SKIP_DOCKER:-}" = 1 ]; then
    echo "################################################################" >&2
    echo "## SKIPPED: Docker is not available and ARUGULA_SKIP_DOCKER=1. ##" >&2
    echo "## NOTHING RAN: no testnet claim was checked.                   ##" >&2
    echo "################################################################" >&2
    echo "SKIPPED: nothing ran (no Docker, ARUGULA_SKIP_DOCKER=1)"
    exit 0
  fi
  echo "FAIL: Docker is not available. The testnet needs it; ARUGULA_SKIP_DOCKER=1 skips (and nothing runs)." >&2
  exit 1
}
