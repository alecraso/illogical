#!/usr/bin/env bash
# Build the wasm package and print its sizes.
#   ./build-wasm.sh [wasm|wasm-fast] [rustcrypto|libcrux]
#   (cargo profile, default wasm = opt-level z; crypto provider, default rustcrypto)
set -euo pipefail
cd "$(dirname "$0")"
profile=${1:-wasm}
provider=${2:-rustcrypto}
export CARGO_TARGET_DIR=$PWD/target-$provider
cargo build --profile "$profile" --target wasm32-unknown-unknown --lib --no-default-features --features "$provider"
raw=$CARGO_TARGET_DIR/wasm32-unknown-unknown/$profile/s30_mls.wasm
out=pkg/$profile-$provider
rm -rf "$out" && mkdir -p "$out/node" "$out/web"
wasm-bindgen "$raw" --target nodejs --out-dir "$out/node"
wasm-bindgen "$raw" --target web --out-dir "$out/web"
for d in node web; do
  f=$out/$d/s30_mls_bg.wasm
  if command -v wasm-opt >/dev/null; then
    if [ "$profile" = wasm ]; then lvl=-Oz; else lvl=-O3; fi
    wasm-opt "$lvl" --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
      --enable-mutable-globals --enable-reference-types --enable-multivalue "$f" -o "$f.opt" && mv "$f.opt" "$f"
  fi
done
f=$out/web/s30_mls_bg.wasm
size() { wc -c <"$1" | tr -d ' '; }
echo "profile=$profile provider=$provider"
echo "cargo output (before wasm-bindgen): $(size "$raw") bytes"
echo "after wasm-bindgen + wasm-opt:     $(size "$f") bytes"
echo "gzip -9:                           $(gzip -9c "$f" | wc -c | tr -d ' ') bytes"
if command -v brotli >/dev/null; then echo "brotli -q 11:                      $(brotli -q 11 -c "$f" | wc -c | tr -d ' ') bytes"; fi
echo "JS glue (web):                     $(size "$out/web/s30_mls.js") bytes"
