#!/bin/sh
# Builds arugulad and arugula for this machine and puts them where the
# bundle picks them up (binaries/NAME-TRIPLE, Tauri's sidecar naming).
# Needs web/dist (`just web`): release daemons embed the web client.
set -e
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
[ -f "$root/web/dist/index.html" ] || { echo "build the web client first: just web" >&2; exit 1; }
triple=$(rustc -vV | sed -n 's/^host: //p')
target=${CARGO_TARGET_DIR:-$root/target}
(cd "$root" && cargo build --release -p arugulad -p arugula)
mkdir -p "$here/binaries"
for bin in arugulad arugula; do
  cp "$target/release/$bin" "$here/binaries/$bin-$triple"
done
ls -la "$here/binaries"
