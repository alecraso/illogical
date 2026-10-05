#!/usr/bin/env bash
#
# macOS checks that need a whole Mac, each in a fresh tart VM clone
# (testnet/macos/vm.sh), with no person and no GUI on the host.
#
#   testnet/macos/test.sh launchd [claim...]   claims: install logout reboot
#   BREAK=1 testnet/macos/test.sh launchd      every claim must fail
#   testnet/macos/test.sh safari               web/safari against real Safari
#   testnet/macos/test.sh iterm2 [claim...]    M5 and M32 in iTerm2 (iterm2.sh)
#   KEEP=1 ...                                 leave the clone running
#
# launchd (S28 #153, M52 #155): a user made with sysadminctl who has never
# logged in to the GUI, reached only over ssh, installs the daemon the way
# the product does (`illogicald install`, or ILLOGICAL_MACOS_INSTALL below)
# and starts a pane.
#   install  the install exits 0 and the daemon answers on its socket
#   logout   after that ssh session ends, the daemon and its pane are still
#            there
#   reboot   after `tart stop` and `run`, with nobody logged in as that
#            user, the daemon is running and has the pane back
# BREAK=1 disables the daemon's launchd service and boots it out before
# each check.
# ILLOGICAL_MACOS_INSTALL picks how it's installed: `product` (default,
# `illogicald install` over ssh), `background` (that plist bootstrapped into
# user/UID with LimitLoadToSessionType Background, no sudo) or `system` (a
# LaunchDaemon with UserName, which needs sudo once). The last two are
# references for M52's install step: what works where.
#
# Binaries come from $ILLOGICAL_MACOS_BIN (default target/debug, as `cargo
# build -p illogicald -p illogical` leaves them). Exit codes: 0 every claim
# held (or no tart: a clean skip), 1 a claim failed, 2 usage.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
V="$HERE/vm.sh"
VM="${ILLOGICAL_MACOS_VM:-illogical-macos}"
BIN="${ILLOGICAL_MACOS_BIN:-$ROOT/target/debug}"
BREAK="${BREAK:-}"
MODE="${ILLOGICAL_MACOS_INSTALL:-product}"
USER_NAME=illo

command -v tart >/dev/null 2>&1 || { echo "SKIP: tart is not installed (brew install cirruslabs/cli/tart)"; exit 0; }

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
    mkdir -p /tmp/illogical"
  v push "$BIN/illogicald" "$BIN/illogical" /tmp/illogical/
  v ssh 'chmod 755 /tmp/illogical /tmp/illogical/*'
}

# Whether the user's daemon answers, asked as admin: no session of theirs.
answers() { v ssh "sudo -u $USER_NAME /Users/$USER_NAME/.local/bin/illogical --socket /Users/$USER_NAME/.local/state/illogical/sock ls" 2>/dev/null; }

knock_out() {
  [ -n "$BREAK" ] || return 0
  v ssh "u=\$(id -u $USER_NAME); for s in gui/\$u/illogicald user/\$u/illogicald system/illogicald.$USER_NAME; do sudo launchctl disable \$s 2>/dev/null; sudo launchctl bootout \$s 2>/dev/null; done; sleep 1; ! pgrep -u $USER_NAME illogicald >/dev/null || sudo pkill -9 -u $USER_NAME illogicald; true"
}

install_daemon() {
  case "$MODE" in
    product) as_user '/tmp/illogical/illogicald install' ;;
    background)
      # shellcheck disable=SC2016 # expands there
      as_user '/tmp/illogical/illogicald install --no-start >/dev/null
        p=~/Library/LaunchAgents/illogicald.plist
        plutil -replace LimitLoadToSessionType -string Background "$p"
        launchctl bootstrap user/$(id -u) "$p"'
      ;;
    system)
      as_user '/tmp/illogical/illogicald install --no-start >/dev/null'
      v ssh "set -e; p=/Library/LaunchDaemons/illogicald.$USER_NAME.plist
        sudo cp /Users/$USER_NAME/Library/LaunchAgents/illogicald.plist \$p
        sudo plutil -replace Label -string illogicald.$USER_NAME \$p
        sudo plutil -insert UserName -string $USER_NAME \$p
        sudo plutil -insert EnvironmentVariables.HOME -string /Users/$USER_NAME \$p
        sudo chown root:wheel \$p
        sudo launchctl bootstrap system \$p"
      ;;
    *) echo "ILLOGICAL_MACOS_INSTALL is product, background or system" >&2; exit 2 ;;
  esac
}

# A pane marked so it can be found again.
MARK=illogical-macos-launchd-mark

t_launchd() {
  local claims="${*:-install logout reboot}" out
  for c in $claims; do case $c in install | logout | reboot) ;; *) echo "unknown claim $c (install logout reboot)" >&2; exit 2 ;; esac; done
  fresh
  make_user
  # Never a GUI session for this user, only the one ssh login below.
  [ "$(v ssh "who | awk '\$1 == \"$USER_NAME\"' | wc -l | tr -d ' '")" = 0 ]

  # The install, and a pane, in one ssh login that then ends.
  if out=$(install_daemon 2>&1) && sleep 2 && as_user "/Users/$USER_NAME/.local/bin/illogical run -- sh -c 'echo $MARK; exec sleep 100000'" >/dev/null 2>&1; then
    installed=1
  else
    installed=
  fi
  knock_out
  case " $claims " in *" install "*)
    if [ -n "$installed" ] && answers >/dev/null; then pass install "$MODE"
    else fail install "$MODE: $(echo "$out" | tail -2 | tr '\n' ' ')"; fi ;;
  esac
  case " $claims " in *" logout "*)
    sleep 5
    if [ "$(v ssh "who | awk '\$1 == \"$USER_NAME\"' | wc -l | tr -d ' '")" = 0 ] && answers | grep -q "sleep 100000"; then pass logout
    else fail logout "no daemon or no pane once $USER_NAME's ssh session ended"; fi ;;
  esac
  case " $claims " in *" reboot "*)
    v restart
    knock_out
    local up=""
    for _ in $(seq 1 30); do answers >/dev/null && { up=1; break; }; sleep 2; done
    if [ -n "$up" ] && [ "$(v ssh "who | awk '\$1 == \"$USER_NAME\"' | wc -l | tr -d ' '")" = 0 ]; then pass reboot "$(answers | wc -l | tr -d ' ') pane(s) back"
    else fail reboot "no daemon for $USER_NAME after a restart with nobody logged in as them"; fi ;;
  esac
}

failed=
case "$test" in
  launchd) t_launchd "$@" ;;
  safari) exec "$HERE/safari.sh" "$@" ;;
  iterm2) exec "$HERE/iterm2.sh" "$@" ;;
  *) sed -n '3,30p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
[ -z "$failed" ]
