#!/usr/bin/env bash
#
# Run the forge blocks' tests against the running forges (#93).
#
#   testnet/forges/test.sh forgejo     crates/daemon/tests/integration/forges_real.rs, forgejo_*
#   testnet/forges/test.sh gitlab      ... gitlab_*
#   testnet/forges/test.sh all         every forge that's up
#
# Needs `testnet/forges/up.sh <forge>` first, and Docker.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
FORGE="${1:-forgejo}"

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
  forgejo | gitlab) filter="${FORGE}_"; files="$FORGE.json" ;;
  all) filter=""; files="" ;;
  *) echo "usage: testnet/forges/test.sh forgejo|gitlab|all" >&2; exit 2 ;;
esac
for f in $files; do
  [ -f "$HERE/.state/$f" ] || { echo "no $HERE/.state/$f: run 'just forges up $FORGE' first" >&2; exit 1; }
done

cd "$ROOT"
ILLOGICAL_TESTNET_FORGES="$HERE/.state" mise exec -- cargo test -p illogicald --test integration -- --ignored "forges_real::$filter"
