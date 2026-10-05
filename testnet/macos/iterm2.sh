#!/usr/bin/env bash
#
# iTerm2 in a fresh tart VM, driven over AppleScript from ssh (no person,
# no GUI on the host). Called by `testnet/macos/test.sh iterm2 [claim...]`.
#
# M5, `illogical tmux -CC` as iTerm2's tmux:
#   attach  iTerm2 runs `illogical tmux -CC` and opens a native window for
#           the daemon's tab
#   type    text written into that iTerm2 session runs in the daemon's pane
#   output  what the daemon's pane prints shows in iTerm2's session
#   split   splitting the session in iTerm2 adds a pane to the daemon's tab
#   tab     a tab the daemon opens shows up as an iTerm2 tab
# M32, OSC 52 from the TUI to a real terminal's clipboard:
#   osc52   `illogical tui` in iTerm2 copies the line above the prompt in
#           copy mode (Ctrl-] [, k, V, y) and the Mac's clipboard (pbpaste)
#           has it
#
# BREAK=1: iTerm2 runs `illogical ls` instead of `tmux -CC`, the session
# checks act on a plain shell window instead of the tmux one, and iTerm2's
# clipboard access is off; every claim then fails.
#
# The VM's sshd is pre-authorized for Apple Events to iTerm2 (the image's
# TCC database already allows it for Safari and System Events; SIP is off
# there). iTerm2 comes from iterm2.com's latest stable zip.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ILLOGICAL_MACOS_VM:-illogical-macos}"
BIN="${ILLOGICAL_MACOS_BIN:-$ROOT/target/debug}"
BREAK="${BREAK:-}"
ALL="attach type output split tab osc52"
claims="${*:-$ALL}"
for c in $claims; do case " $ALL " in *" $c "*) ;; *) echo "unknown claim $c ($ALL)" >&2; exit 2 ;; esac; done

v() { "$V" "$1" "$VM" "${@:2}"; }
osa() { v ssh osascript -; }
failed=
check() {
  if [ "$2" = "$3" ]; then echo "[macos iterm2 $1] ok"; else echo "[macos iterm2 $1] FAIL: wanted '$3', got '$2'" >&2; failed=1; fi
}
want() { case " $claims " in *" $1 "*) return 0 ;; esac; return 1; }

v down >/dev/null
v up >/dev/null
[ -n "${KEEP:-}" ] || trap 'v down >/dev/null' EXIT

clip=true
[ -z "$BREAK" ] || clip=false
v ssh "set -e
  db=~/Library/Application\\ Support/com.apple.TCC/TCC.db
  for c in /usr/libexec/sshd-keygen-wrapper /usr/bin/osascript; do
    sqlite3 \"\$db\" \"insert or replace into access (service,client,client_type,auth_value,auth_reason,auth_version,indirect_object_identifier_type,indirect_object_identifier,flags,last_modified) values ('kTCCServiceAppleEvents','\$c',1,2,0,1,0,'com.googlecode.iterm2',0,cast(strftime('%s','now') as integer))\"
  done
  curl -sSL -o /tmp/iterm.zip https://iterm2.com/downloads/stable/latest
  ditto -x -k /tmp/iterm.zip /Applications
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f /Applications/iTerm.app
  defaults write com.googlecode.iterm2 SUEnableAutomaticChecks -bool false
  defaults write com.googlecode.iterm2 AllowClipboardAccess -bool $clip
  mkdir -p /tmp/illogical"
v push "$BIN/illogicald" "$BIN/illogical" /tmp/illogical/
# Two daemons: one for iTerm2's tmux, one with a single pane for the TUI.
# shellcheck disable=SC2016 # expands there
v ssh 'chmod 755 /tmp/illogical/*
  for d in cc tui; do
    (PS1="$ " nohup /tmp/illogical/illogicald --listen 127.0.0.1:0 --state-dir /tmp/ill-$d --shell "bash --norc --noprofile" >/tmp/illd-$d.log 2>&1 &)
  done
  for _ in $(seq 1 50); do [ -S /tmp/ill-cc/sock ] && [ -S /tmp/ill-tui/sock ] && break; sleep 0.2; done
  open -a /Applications/iTerm.app
  for _ in $(seq 1 50); do osascript -e "tell application \"iTerm\" to count windows" >/dev/null 2>&1 && break; sleep 0.5; done'
I() { v ssh "/tmp/illogical/illogical --socket /tmp/ill-cc/sock $*"; }

cc="tmux -CC"
[ -z "$BREAK" ] || cc="ls"
# A plain shell window (BREAK=1 acts on it), then the tmux gateway.
plain=$(echo 'tell application "iTerm" to id of (create window with default profile)' | osa)
osa <<EOF >/dev/null
tell application "iTerm"
  create window with default profile command "/tmp/illogical/illogical --socket /tmp/ill-cc/sock $cc"
end tell
EOF
# The window iTerm2 opens for the daemon's tab is named "↣ tmux ...".
target='(first window whose name starts with "↣ tmux")'
[ -z "$BREAK" ] || target="(window id $plain)"
tmux_windows() { echo 'tell application "iTerm" to count (windows whose name starts with "↣ tmux")' | osa; }
for _ in $(seq 1 40); do [ "$(tmux_windows)" -ge 1 ] 2>/dev/null && break; sleep 0.5; done

want attach && check attach "$(tmux_windows)" 1

if want type; then
  echo "tell application \"iTerm\" to tell current session of $target to write text \"echo ITERM-\$((6*7))\"" | osa >/dev/null || true
  sleep 2
  check type "$(I capture %1 | grep -c '^ITERM-42$' || true)" 1
fi

if want output; then
  I send %1 "'echo FROM-DAEMON-\$((7*7))
'" >/dev/null
  sleep 2
  check output "$(echo "tell application \"iTerm\" to get contents of current session of $target" | osa | grep -c '^FROM-DAEMON-49' || true)" 1
fi

if want split; then
  before=$(I ls | wc -l | tr -d ' ')
  echo "tell application \"iTerm\" to tell current session of $target to split vertically with default profile" | osa >/dev/null || true
  sleep 3
  check split "$(I ls | grep -c ' @1 ')" "$((before + 1))"
fi

if want tab; then
  tabs() { echo "tell application \"iTerm\" to count tabs of $target" | osa; }
  before=$(tabs)
  I run -- bash --norc --noprofile >/dev/null
  sleep 3
  check tab "$(tabs)" "$((before + 1))"
fi

if want osc52; then
  v ssh 'echo nothing-yet | pbcopy'
  tui=$(echo 'tell application "iTerm" to id of (create window with default profile command "env TERM=xterm-256color /tmp/illogical/illogical --socket /tmp/ill-tui/sock tui")' | osa || true)
  tui="(window id $tui)"
  sleep 4
  osa <<EOF >/dev/null || true
tell application "iTerm"
  tell current session of $tui
    write text "echo TUI-COPY-\$((8*8))"
    delay 1
    write text (ASCII character 29) newline NO
    write text "[" newline NO
    delay 0.5
    write text "k" newline NO
    write text "V" newline NO
    write text "y" newline NO
  end tell
end tell
EOF
  sleep 2
  check osc52 "$(v ssh pbpaste || true)" TUI-COPY-64
fi

[ -z "$failed" ]
