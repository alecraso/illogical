#!/bin/sh
# Builds S30Probe.app (with a microphone usage string, as the desktop app
# would need) and ad-hoc signs it.
set -e
cd "$(dirname "$0")"
rm -rf S30Probe.app && mkdir -p S30Probe.app/Contents/MacOS
swiftc -O Probe.swift -o S30Probe.app/Contents/MacOS/S30Probe
cat > S30Probe.app/Contents/Info.plist <<P
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>wtf.widgets.illogical.s30probe</string>
<key>CFBundleExecutable</key><string>S30Probe</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSMicrophoneUsageDescription</key><string>S30 checks that calls can use the microphone.</string>
</dict></plist>
P
codesign -s - --force S30Probe.app
