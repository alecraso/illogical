#!/bin/sh
# Builds the macOS apps: s26-mac (B2), s26-tabs (level A and the hybrid) and,
# when GHOSTTYKIT points at a GhosttyKit.xcframework/macos-arm64, s26-b1.
set -e
cd "$(dirname "$0")/.."
cargo +1.98 build --release -p s26-ffi
mkdir -p target/mac
F="-O -import-objc-header mac/s26.h -Ltarget/release -ls26_ffi -framework AppKit -framework CoreText -framework QuartzCore"
swiftc $F mac/term.swift mac/main.swift -o target/mac/s26-mac
swiftc $F -framework WebKit mac/term.swift mac/tabs/main.swift -o target/mac/s26-tabs
if [ -n "$GHOSTTYKIT" ]; then
  swiftc -O mac/b1.swift -import-objc-header "$GHOSTTYKIT/Headers/ghostty.h" -L"$GHOSTTYKIT" -lghostty-internal -lc++ \
    -framework AppKit -framework Carbon -framework Metal -framework MetalKit -framework QuartzCore -framework CoreText \
    -framework IOSurface -framework UniformTypeIdentifiers -framework CoreVideo -o target/mac/s26-b1
fi
ls -la target/mac
