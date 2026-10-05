#!/bin/bash
# Runs a command inside the logged-in user's GUI session, where the login
# keychain is unlocked, so codesign works from an ssh shell (ssh sessions get
# errSecInternalComponent). Prints the command's output when it's done.
# ./gui.sh COMMAND...
set -eu
label=dev.illogical.s31.gui
dir=$(mktemp -d)
plist=$dir/job.plist
cmd=$(printf '%q ' "$@")
cat > "$plist" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>$label</string>
<key>ProgramArguments</key><array><string>/bin/bash</string><string>-c</string>
<string>cd $PWD; $cmd &gt; $dir/out 2&gt;&amp;1; echo \$? &gt; $dir/rc</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
PL
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$plist"
while [ ! -f "$dir/rc" ]; do sleep 1; done
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
cat "$dir/out"
exit "$(cat "$dir/rc")"
