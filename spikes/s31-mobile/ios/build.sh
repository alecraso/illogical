#!/bin/sh
# Builds the S31 iOS probe for a device and (with a device id) installs it.
# Run on a Mac with Xcode, rustup's aarch64-apple-ios target and Zig 0.16.
# ./build.sh [DEVICE]
set -eu
cd "$(dirname "$0")"
HERE=$PWD
OUT=$HERE/build
mkdir -p "$OUT"
export PATH="$HOME/.local/opt/zig-aarch64-macos-0.16.0:$HOME/.cargo/bin:$PATH"
export IPHONEOS_DEPLOYMENT_TARGET=17.0
CC="xcrun -sdk iphoneos clang -arch arm64 -miphoneos-version-min=17.0 -O2"

# Rust: tokio, axum, reqwest, tungstenite and lib-vt in a staticlib.
CARGO_TARGET_DIR=$OUT/cargo cargo +1.98 build --release --target aarch64-apple-ios --manifest-path rt/Cargo.toml
cp "$OUT/cargo/aarch64-apple-ios/release/libs31rt.a" "$OUT/"

# wasm3 (the spike ran its main branch at 28ecb9a). Guarded memory is off:
# it reserves 8 GiB of address space per memory, more than an iOS app gets.
if [ ! -d "$OUT/wasm3" ]; then
  git clone -q https://github.com/wasm3/wasm3 "$OUT/wasm3"
fi
git -C "$OUT/wasm3" log -1 --format='wasm3 %h %cs' 
rm -f "$OUT"/m3_*.o
(cd "$OUT" && for f in wasm3/source/*.c; do $CC -Dd_m3HasWASI -Dd_m3GuardedMemory=0 -c "$f" -o "m3_$(basename "$f" .c).o"; done)
rm -f "$OUT/libm3.a" && ar rcs "$OUT/libm3.a" "$OUT"/m3_*.o

# A separate executable and a framework-wrapped command.
$CC helpers/hello.c -o "$OUT/hello"
rm -rf "$OUT/S31Cmd.framework" && mkdir -p "$OUT/S31Cmd.framework"
$CC -dynamiclib -install_name @rpath/S31Cmd.framework/S31Cmd helpers/cmd.c -o "$OUT/S31Cmd.framework/S31Cmd"
cp helpers/Info-cmd.plist "$OUT/S31Cmd.framework/Info.plist"

xcodebuild -project app/S31Probe.xcodeproj -configuration Release -sdk iphoneos -target S31Probe DEVELOPMENT_TEAM="${DEVELOPMENT_TEAM:?set DEVELOPMENT_TEAM to your Apple team ID}" \
  SYMROOT="$OUT/xb" OBJROOT="$OUT/xb/obj" -allowProvisioningUpdates build | grep -E '^\*\* BUILD|error:' || true

APP=$OUT/xb/Release-iphoneos/S31Probe.app
if [ $# -gt 0 ]; then
  xcrun devicectl device install app --device "$1" "$APP"
fi
