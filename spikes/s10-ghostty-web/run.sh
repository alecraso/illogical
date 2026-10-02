#!/bin/sh
# Runs a harness script with the browsers installed under work/ (WebKit
# needs a few host libraries that were unpacked into its lib dir; see README).
cd "$(dirname "$0")"
export PLAYWRIGHT_BROWSERS_PATH="$PWD/work/pw-browsers" PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=1 NODE_PATH="$PWD/work/node_modules"
exec node "$@"
