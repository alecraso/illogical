#!/usr/bin/env bash
# S17: build the four follow-view candidates and measure them (writes work/bundle.json).
set -eu
cd "$(dirname "$0")"
mkdir -p work/bundle
cp bundle/*.js bundle/*.mjs bundle/package.json work/bundle/
cd work/bundle
[ -d node_modules ] || npm install --no-audit --no-fund --loglevel=error
npm ls --depth=0 2>/dev/null | sed -n '2,20p' >../bundle-versions.txt
node build.mjs >../bundle.json
