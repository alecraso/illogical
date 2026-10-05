#!/bin/sh
# Copies the client's own xterm.js build into probe/vendor (not committed).
set -e
here=$(cd "$(dirname "$0")" && pwd)
nm=${XTERM_FROM:-$here/../../web/node_modules}
[ -d "$nm/@xterm/xterm" ] || { echo "no $nm/@xterm: run pnpm install in web/ (or set XTERM_FROM)" >&2; exit 1; }
mkdir -p "$here/probe/vendor"
cp "$nm/@xterm/xterm/lib/xterm.js" "$nm/@xterm/xterm/css/xterm.css" "$nm/@xterm/addon-webgl/lib/addon-webgl.js" "$here/probe/vendor/"
