#!/bin/sh
# The spike's tests on Linux, in Playwright's container: its WebKit is the
# GTK/WPE port, the engine the desktop app's WebKitGTK webview runs. Needs
# Docker and Zig (for the static s27, as `just static` builds the daemon).
# Usage: ./linux-webkit.sh [playwright args], e.g. ./linux-webkit.sh basics vite
set -eu
cd "$(dirname "$0")"
root=$(cd ../.. && pwd)
t=aarch64-unknown-linux-musl
[ "$(uname -m)" = x86_64 ] && t=x86_64-unknown-linux-musl
arch=${t%%-*}
T=$(echo "$t" | tr a-z- A-Z_)
rustup target add "$t" >/dev/null
export ZIG_MUSL_ARCH=$arch
export "CC_$(echo "$t" | tr - _)=$root/scripts/zig-cc-musl" "AR_$(echo "$t" | tr - _)=$root/scripts/zig-ar"
export "CARGO_TARGET_${T}_LINKER=$root/scripts/zig-cc-musl" "CARGO_TARGET_${T}_RUSTFLAGS=-C link-self-contained=no"
cargo build --release --target "$t"
cargo build --release
node build.mjs
[ -f .run/cert/spki.txt ] || target/release/s27 cert .run/cert
# The tree is copied into the container (the host's node_modules are for
# macOS); results are printed as JSON lines, as on the host.
docker run --rm --ipc=host -v "$PWD:/src:ro" -e S27_BUILT=1 -e CI=1 \
  mcr.microsoft.com/playwright:v1.63.0-noble sh -c "
    set -e
    mkdir -p /work/target/$t/release /work/.run && cd /src
    tar cf - --exclude=./node_modules --exclude=./target --exclude=./.run --exclude=./test-results . | tar xf - -C /work
    cp target/$t/release/s27 /work/target/$t/release/ && cp -r .run/cert /work/.run/
    cd /work && npx -y pnpm@11 install --frozen-lockfile --store-dir /tmp/pnpm-store >/tmp/install.log 2>&1 || { cat /tmp/install.log; exit 1; }
    S27_BIN=target/$t/release/s27 npx playwright test --project webkit --reporter=list $*
  "
