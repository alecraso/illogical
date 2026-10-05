#!/usr/bin/env bash
#
# A throwaway macOS VM (tart) that tests drive over ssh.
#
#   testnet/macos/vm.sh base             make the base VM (once; ~30 GB)
#   testnet/macos/vm.sh up [NAME]        clone the base VM, boot the clone
#                                        headless, wait for ssh
#   testnet/macos/vm.sh ssh [NAME] [--as USER] CMD
#                                        run CMD in it, as admin unless USER
#   testnet/macos/vm.sh push [NAME] SRC... DEST   copy files in (scp)
#   testnet/macos/vm.sh restart [NAME]   shut the guest down, then run again
#   testnet/macos/vm.sh down [NAME]      stop and delete the clone
#   testnet/macos/vm.sh ip [NAME]
#   testnet/macos/vm.sh sshcmd [NAME]    an ssh command line into it, quoted
#
# NAME defaults to illogical-macos. Every test VM is an APFS clone of one
# local base VM, illogical-macos-base, which is never booted; `down`
# deletes the clone, so tests always start from a fresh Mac. `base` makes
# it from $ILLOGICAL_MACOS_IMAGE (default
# ghcr.io/cirruslabs/macos-tahoe-base:latest: no Xcode) and then empties
# tart's OCI cache, so the disk holds one ~30 GB copy, not two. Keep it to
# the base plus one running clone (CI on the runner too). The image's
# admin user's password is admin. `up` puts the key in testnet/macos/.state
# into admin's authorized_keys through the tart guest agent, so nothing
# after that needs the password. Runs on any Apple silicon Mac with tart,
# the self-hosted macos-arm64 runner included.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="$HERE/.state"
IMAGE="${ILLOGICAL_MACOS_IMAGE:-ghcr.io/cirruslabs/macos-tahoe-base:latest}"
BASE=illogical-macos-base
# Never let a clone prune tart's cache (other images) to make room.
export TART_NO_AUTO_PRUNE=1

usage() { sed -n '3,27p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }

# shellcheck source=testnet/macos/need-tart.sh
. "$HERE/need-tart.sh"

cmd="${1:-}"; shift || true
name="illogical-macos"
case "${1:-}" in illogical-*) name=$1; shift ;; esac
[ "$name" != illogical-macos-base ] || { echo "illogical-macos-base is the base; use a clone" >&2; exit 2; }

key() {
  mkdir -p "$STATE"
  [ -f "$STATE/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C illogical-macos-test -f "$STATE/id_ed25519"
}

ip() { tart ip --wait 120 "$name"; }
have() { tart list -q 2>/dev/null | grep -qx "$1"; }

# Fresh VMs get fresh host keys, so none are kept.
SSH_OPTS=(-i "$STATE/id_ed25519" -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=no
  -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=5 -o ServerAliveInterval=5)

vssh() {
  local user=admin
  if [ "${1:-}" = --as ]; then user=$2; shift 2; fi
  # shellcheck disable=SC2029 # the command is meant to run there
  ssh "${SSH_OPTS[@]}" "$user@$(ip)" "$@"
}

wait_ssh() {
  for _ in $(seq 1 90); do
    vssh true 2>/dev/null && return 0
    sleep 2
  done
  echo "the VM $name never answered ssh" >&2
  return 1
}

boot() {
  mkdir -p "$STATE"
  # tart run stays in the foreground for the VM's lifetime.
  nohup tart run --no-graphics --no-audio --no-clipboard "$name" >"$STATE/$name.log" 2>&1 &
  ip >/dev/null
}

case "$cmd" in
  base)
    if have "$BASE"; then echo "$BASE is there"; exit 0; fi
    have "$IMAGE" || tart pull "$IMAGE"
    tart clone "$IMAGE" "$BASE"
    tart prune --entries=caches --older-than=0 >/dev/null
    echo "made $BASE"
    ;;
  up)
    key
    have "$BASE" || { echo "no base VM $BASE, so no macOS VM test can run: make it once with \`just macos base\` (pulls $IMAGE, about 30 GB)" >&2; exit 1; }
    have "$name" || tart clone "$BASE" "$name"
    tart list 2>/dev/null | awk -v n="$name" '$2 == n && $NF == "running" { r = 1 } END { exit !r }' || boot
    # The guest agent starts a little after the network does.
    for _ in $(seq 1 60); do tart exec "$name" true 2>/dev/null && break; sleep 2; done
    tart exec -i "$name" sh -c 'mkdir -p ~/.ssh && chmod 700 ~/.ssh && cat >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys' <"$STATE/id_ed25519.pub"
    wait_ssh
    echo "$name is up at $(ip)"
    ;;
  ssh) vssh "$@" ;;
  push)
    [ $# -ge 2 ] || usage
    dest="${*: -1}"
    scp -q "${SSH_OPTS[@]}" "${@:1:$#-1}" "admin@$(ip):$dest"
    ;;
  restart)
    # A clean shutdown, as a person's restart is (launchd stops daemons and
    # they save), then tart's stop if the guest doesn't go within a minute.
    vssh 'sudo shutdown -h now' >/dev/null 2>&1 || true
    for _ in $(seq 1 30); do
      tart list 2>/dev/null | awk -v n="$name" '$2 == n && $NF == "running" { r = 1 } END { exit r }' && break
      sleep 2
    done
    tart stop "$name" >/dev/null 2>&1 || true
    boot
    wait_ssh
    ;;
  down)
    tart stop "$name" >/dev/null 2>&1 || true
    tart delete "$name" 2>/dev/null || true
    rm -f "$STATE/$name.log"
    ;;
  ip) ip ;;
  sshcmd) printf '%q ' ssh "${SSH_OPTS[@]}" "admin@$(ip)"; echo ;;
  *) usage ;;
esac
