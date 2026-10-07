#!/usr/bin/env bash
#
# macOS checks that need a whole Mac, each in a fresh tart VM clone
# (testnet/macos/vm.sh), with no person and no GUI on the host.
#
#   testnet/macos/test.sh launchd [claim...]   claims below
#   BREAK=1 testnet/macos/test.sh launchd      every claim must fail
#   testnet/macos/test.sh safari               web/safari against real Safari
#   testnet/macos/test.sh iterm2 [claim...]    M5 and M32 in iTerm2 (iterm2.sh)
#   testnet/macos/test.sh app                  #178, the app in cloud mode
#                                              (app-cloud.ts)
#   KEEP=1 ...                                 leave the clone running
#
# launchd (S28 #153, M52 #155): a user made with sysadminctl who has never
# logged in to the GUI, reached only over ssh, installs the daemon with
# `arugulad install` and starts a pane, then switches to --system and
# uninstalls. In order, all in one VM:
#   install   `arugulad install` over ssh exits 0 and the daemon answers
#   warning   it said the daemon won't start after a reboot by itself and
#             named `arugulad install --system`
#   logout    after that ssh session ends, the daemon and its pane are there
#   uninstall-agent  `arugulad uninstall` leaves no plist, no service in
#             gui/UID or user/UID, and no arugulad running as the user
#   ssh       `arugula --ssh illo@vm ls` from the host (M52's path) starts
#             the daemon there and passes the warning through
#   system    `arugulad install --system` (sudo) switches cleanly: a
#             LaunchDaemon in system/arugulad.illo, no agent plist, no
#             user/UID service, and the daemon answers
#   reboot    after a clean shutdown and `tart run`, with nobody logged in as that
#             user, the daemon is running with the pane made under --system
#   uninstall `arugulad uninstall` removes the LaunchDaemon: no plist, no
#             service, no arugulad running as the user
# BREAK=1: the daemon's launchd service is disabled and booted out before
# install, logout, system and reboot are checked; the install's `note:`
# line is dropped before warning; each uninstall is followed by an install
# (something left behind); and the daemon is already running when the ssh
# check starts it, so nothing is said.
#
# Binaries come from $ARUGULA_MACOS_BIN (default target/debug, as `cargo
# build -p arugulad -p arugula` leaves them). Exit codes: 0 every claim
# held (or no tart: a clean skip), 1 a claim failed, 2 usage.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ARUGULA_MACOS_VM:-arugula-macos}"
BIN="${ARUGULA_MACOS_BIN:-$ROOT/target/debug}"
BREAK="${BREAK:-}"
USER_NAME=illo
D=/Users/$USER_NAME/.local/bin/arugulad

# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"

test="${1:-}"; shift || true

v() { "$V" "$1" "$VM" "${@:2}"; }
as_user() { v ssh --as "$USER_NAME" "$@"; }
pass() { echo "[macos $test $1] ok${2:+: $2}"; }
fail() { echo "[macos $test $1] FAIL: $2" >&2; failed=1; }

fresh() {
  v down >/dev/null
  v up >/dev/null
  [ -n "${KEEP:-}" ] || trap 'v down >/dev/null' EXIT
}

# A user who has never had a GUI session, allowed in over ssh with the
# harness key, and the binaries where they can run them.
make_user() {
  v ssh "set -e
    sudo sysadminctl -addUser $USER_NAME -fullName $USER_NAME -password \"\$(openssl rand -hex 16)\" -home /Users/$USER_NAME -shell /bin/zsh >/dev/null 2>&1
    sudo createhomedir -c -u $USER_NAME >/dev/null
    sudo dseditgroup -o edit -a $USER_NAME -t user com.apple.access_ssh
    sudo mkdir -p /Users/$USER_NAME/.ssh
    sudo cp ~/.ssh/authorized_keys /Users/$USER_NAME/.ssh/
    sudo chown -R $USER_NAME:staff /Users/$USER_NAME/.ssh
    sudo chmod 700 /Users/$USER_NAME/.ssh
    mkdir -p /tmp/arugula"
  v push "$BIN/arugulad" "$BIN/arugula" /tmp/arugula/
  v ssh 'chmod 755 /tmp/arugula /tmp/arugula/*'
}

# Whether the user's daemon answers, asked as admin: no session of theirs.
answers() { v ssh "sudo -u $USER_NAME /Users/$USER_NAME/.local/bin/arugula --socket /Users/$USER_NAME/.local/state/arugula/sock ls" 2>/dev/null; }

knock_out() {
  [ -n "$BREAK" ] || return 0
  v ssh "u=\$(id -u $USER_NAME); for s in gui/\$u/arugulad user/\$u/arugulad system/arugulad.$USER_NAME; do sudo launchctl disable \$s 2>/dev/null; sudo launchctl bootout \$s 2>/dev/null; done; sleep 1; ! pgrep -u $USER_NAME arugulad >/dev/null || sudo pkill -9 -u $USER_NAME arugulad; true"
}

# Panes marked so they can be found again: one under the agent, one under
# the LaunchDaemon.
MARK=arugula-macos-launchd-mark
pane() { as_user "/Users/$USER_NAME/.local/bin/arugula run -- sh -c 'echo $MARK-\$(($1 + 1)); exec sleep $1'" >/dev/null 2>&1; }
# Every pane's output (a restored pane runs a shell again, with the old
# output above it).
outputs() {
  v ssh sh -s 2>/dev/null <<EOF
c="sudo -u $USER_NAME /Users/$USER_NAME/.local/bin/arugula --socket /Users/$USER_NAME/.local/state/arugula/sock"
for p in \$(\$c ls | awk '{print \$1}'); do \$c tail --text "\$p"; done
EOF
}
logged_in() { v ssh "who | awk '\$1 == \"$USER_NAME\"' | wc -l | tr -d ' '"; }
# What launchd and the disk hold for the user's daemon, one word per thing
# found: agent-plist daemon-plist gui user system process. Pane shims
# (`arugulad _shim`) don't count: with no daemon they end their panes a
# minute later, as after any stop.
leftovers() {
  # shellcheck disable=SC2016 # expands there
  v ssh "u=\$(id -u $USER_NAME)
    [ ! -e /Users/$USER_NAME/Library/LaunchAgents/arugulad.plist ] || echo agent-plist
    [ ! -e /Library/LaunchDaemons/arugulad.$USER_NAME.plist ] || echo daemon-plist
    ! sudo launchctl print gui/\$u/arugulad >/dev/null 2>&1 || echo gui
    ! sudo launchctl print user/\$u/arugulad >/dev/null 2>&1 || echo user
    ! sudo launchctl print system/arugulad.$USER_NAME >/dev/null 2>&1 || echo system
    ! ps -U $USER_NAME -o args= | grep -v ' _shim' | grep -q '^[^ ]*arugulad' || echo process" | tr '\n' ' '
}
# Nothing left, given a few seconds for the daemon to exit.
clean() {
  local left
  for _ in $(seq 1 10); do left=$(leftovers); [ -z "$left" ] && return 0; sleep 1; done
  echo "$left"; return 1
}
want() { case " $claims " in *" $1 "*) return 0 ;; esac; return 1; }

t_launchd() {
  claims="${*:-install warning logout uninstall-agent ssh system reboot uninstall}"
  local out left c
  for c in $claims; do case $c in install | warning | logout | uninstall-agent | ssh | system | reboot | uninstall) ;;
    *) echo "unknown claim $c (install warning logout uninstall-agent ssh system reboot uninstall)" >&2; exit 2 ;; esac; done
  fresh
  make_user
  # Never a GUI session for this user, only ssh logins.
  [ "$(logged_in)" = 0 ]

  # The default install, and a pane, in one ssh login that then ends.
  local ok=
  if out=$(as_user '/tmp/arugula/arugulad install' 2>&1) && sleep 2 && pane 100000; then ok=1; fi
  [ -z "$BREAK" ] || out=$(echo "$out" | grep -v '^note:')
  knock_out
  if want install; then
    if [ -n "$ok" ] && answers >/dev/null; then pass install "no GUI login, background agent"
    else fail install "$(echo "$out" | tail -2 | tr '\n' ' ')"; fi
  fi
  if want warning; then
    if echo "$out" | grep "^note:" | grep "after a reboot it won't start" | grep -q "arugulad install --system"; then pass warning
    else fail warning "no reboot warning in: $(echo "$out" | tail -3 | tr '\n' ' ')"; fi
  fi
  if want logout; then
    sleep 5
    if [ "$(logged_in)" = 0 ] && answers | grep -q "sleep 100000"; then pass logout
    else fail logout "no daemon or no pane once $USER_NAME's ssh session ended"; fi
  fi

  as_user "$D uninstall" >/dev/null 2>&1 || true
  [ -z "$BREAK" ] || as_user "$D install" >/dev/null 2>&1 || true
  if want uninstall-agent; then
    if left=$(clean); then pass uninstall-agent
    else fail uninstall-agent "left behind: $left"; fi
  fi

  # M52's path: the host's CLI over ssh, which starts the daemon there.
  local h=/tmp/arugula-macos-ssh
  rm -rf /tmp/arugula-macos-ssh; mkdir -p "$h"
  out=$(HOME=$h XDG_RUNTIME_DIR=$h XDG_CONFIG_HOME=$h/config XDG_CACHE_HOME=$h/cache ARUGULA_SSH_INSTALL=yes \
    ARUGULA_SSH="ssh -i $HERE/.state/id_ed25519 -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR" \
    "$BIN/arugula" --ssh "$USER_NAME@$(v ip)" ls </dev/null 2>&1) || true
  for sock in "$h"/arugula-ssh/*; do [ -S "$sock" ] && ssh -o ControlPath="$sock" -O exit x >/dev/null 2>&1; done
  rm -rf /tmp/arugula-macos-ssh
  if want ssh; then
    if echo "$out" | grep -q "^arugula: $USER_NAME@.*: $USER_NAME has no GUI login.*arugulad install --system" && answers >/dev/null; then pass ssh
    else fail ssh "no daemon, or the warning didn't come through: $(echo "$out" | tail -3 | tr '\n' ' ')"; fi
  fi

  # --system, by the user, who may sudo (as an admin would).
  v ssh "echo '$USER_NAME ALL=(ALL) NOPASSWD: ALL' | sudo tee /etc/sudoers.d/$USER_NAME >/dev/null"
  ok=
  if out=$(as_user "$D install --system" 2>&1) && sleep 2 && pane 200000; then ok=1; fi
  knock_out
  if want system; then
    left=$(leftovers)
    if [ -n "$ok" ] && answers >/dev/null && [ "$left" = "daemon-plist system process " ] && echo "$out" | grep -q "runs sudo"; then pass system
    else fail system "launchd and disk hold: $left; said: $(echo "$out" | tail -2 | tr '\n' ' ')"; fi
  fi

  if want reboot; then
    v restart
    knock_out
    local up=""
    for _ in $(seq 1 30); do outputs | grep -q "$MARK-200001" && { up=1; break; }; sleep 2; done
    if [ -n "$up" ] && [ "$(logged_in)" = 0 ]; then pass reboot "$(answers | wc -l | tr -d ' ') pane(s) back"
    else fail reboot "no daemon with its pane for $USER_NAME after a restart with nobody logged in as them"; fi
  fi

  if want uninstall; then
    as_user "$D uninstall" >/dev/null 2>&1 || true
    [ -z "$BREAK" ] || as_user "$D install --system" >/dev/null 2>&1 || true
    if left=$(clean); then pass uninstall
    else fail uninstall "left behind: $left"; fi
  fi
}

failed=
case "$test" in
  launchd) t_launchd "$@" ;;
  safari) exec "$HERE/safari.sh" "$@" ;;
  iterm2) exec "$HERE/iterm2.sh" "$@" ;;
  app) cd "$ROOT" && exec node --experimental-strip-types --no-warnings testnet/macos/app-cloud.ts "$@" ;;
  *) sed -n '3,40p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
[ -z "$failed" ]
