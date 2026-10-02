#!/bin/bash
# Runs a shell script in a sprite on geek's wispd, as studio's lobby does (plant.mjs's exec).
# Usage: ./box.sh <sprite> '<script>'
set -euo pipefail
name="$1"; script="$2"
token="$(cat ~/.local/share/wisp/token)"
curl -s -m600 -X POST -H "Authorization: Bearer $token" \
  --get --data-urlencode stdin=false --data-urlencode cmd=bash --data-urlencode cmd=-lc \
  --data-urlencode "cmd=( $script
) 2>&1; echo __EXIT:\$?" \
  "http://127.0.0.1:7789/v1/sprites/$name/exec" -d '' -o - | tr -d '\000-\010'
