#!/usr/bin/env bash
# vm.sh ARGS... — bun vmnet.ts with the local wisp token
here="$(cd "$(dirname "$0")" && pwd)"
SPRITE_TOKEN="$(cat ~/.local/share/wisp/token)" exec mise exec bun@1.4.2 -- bun "$here/vmnet.ts" "$@"
