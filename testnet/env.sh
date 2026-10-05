# Sourced by the testnet scripts. COMPOSE_PROJECT_NAME (default
# illogical-testnet) names the stack: its containers, networks and state
# directory. Two stacks with different names don't touch each other.
# shellcheck shell=bash
TESTNET="${COMPOSE_PROJECT_NAME:-illogical-testnet}"
export COMPOSE_PROJECT_NAME="$TESTNET"
if [ "$TESTNET" = illogical-testnet ]; then STATE="$HERE/.state"; else STATE="$HERE/.state-$TESTNET"; fi
export ILLOGICAL_TESTNET_STATE="$STATE"
