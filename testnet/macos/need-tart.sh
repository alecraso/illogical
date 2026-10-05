# shellcheck shell=bash
# Sourced by the macOS VM scripts. Without tart they fail: a test that
# didn't run isn't a pass. ILLOGICAL_SKIP_MACOS_VM=1 is the only way to
# skip, and it says loudly that nothing ran.
if [ "${ILLOGICAL_SKIP_MACOS_VM:-}" = 1 ]; then
  echo "!!! ILLOGICAL_SKIP_MACOS_VM=1: SKIPPED, no macOS VM test ran !!!" >&2
  exit 0
fi
command -v tart >/dev/null 2>&1 || {
  echo "tart is not installed, so no macOS VM test can run: brew install cirruslabs/cli/tart (docs/testing.md, "A fresh Mac"), then \`just macos base\`. ILLOGICAL_SKIP_MACOS_VM=1 skips, saying so." >&2
  exit 1
}
