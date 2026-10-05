#!/usr/bin/env bash
#
# Run claims against a running profile of the test stack.
#
#   testnet/test.sh ssh                 every claim of the ssh profile
#   testnet/test.sh ssh jump stdio      just those
#   BREAK=1 testnet/test.sh ssh jump    must fail
#   testnet/test.sh ssh --break         each claim under BREAK=1; passes only
#                                       if every one of them fails
#
# The ssh profile's claims, and how BREAK=1 breaks each one:
#
#   login  The stack's key logs into the bastion, with strict host key
#          checking. Broken: a fresh key the boxes have never seen.
#   jump   box-bare is reached through the bastion with ProxyJump.
#          Broken: the bastion's AllowTcpForwarding is turned off (and back
#          on afterwards).
#   inner  box-bare has no route out; only ssh through the bastion reaches
#          it. Broken: the check runs on the bastion, which has one.
#   bare   box-bare has no illogical: no binary on the login PATH, no state
#          or config dir. Broken: a stub illogical is put in /usr/local/bin
#          (and removed afterwards).
#   stdio  ssh carries a binary stream both ways unchanged: 1 MiB of random
#          bytes through `cat` on box-bare comes back identical. This is
#          what S28's bridge relies on. Broken: a forced tty (-tt).
#   agent  A key in the client's agent is usable on box-bare when the agent
#          is forwarded through the bastion (M51's `git push`). Broken:
#          ForwardAgent=no.
#   push   `git push` from box-bare to the git server (a bare repository
#          over ssh) with the client's key only in the forwarded agent
#          (M51). Broken: ForwardAgent=no.
#   linger On box-systemd, a user turns on lingering for themselves from an
#          ssh login with no sudo (S28's lifetime question). Broken: polkit
#          masked (and unmasked afterwards).
#
# Needs `testnet/up.sh <profile>` first. Exit codes: 0 every claim held (or
# Docker is unavailable, a clean skip), 1 a claim failed, 2 usage.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROFILE="${1:-ssh}"; shift || true
# shellcheck source-path=SCRIPTDIR source=env.sh
. "$HERE/env.sh"
CFG="$STATE/ssh_config"
BREAK="${BREAK:-}"
SSH_CLAIMS="login jump inner bare stdio agent push linger"

command -v docker >/dev/null 2>&1 || { echo "SKIP: Docker is not available"; exit 0; }
docker info >/dev/null 2>&1 || { echo "SKIP: Docker is not available"; exit 0; }

[ "$PROFILE" = ssh ] || { echo "usage: testnet/test.sh ssh [--break | claim...]   (claims: $SSH_CLAIMS)" >&2; exit 2; }
[ -f "$CFG" ] || { echo "no $CFG; run 'just testnet up ssh' first" >&2; exit 1; }

if [ "${1:-}" = --break ]; then
  held=""
  for c in $SSH_CLAIMS; do
    if BREAK=1 "$0" "$PROFILE" "$c" >/dev/null 2>&1; then held="$held $c"; else echo "[testnet ssh $c] caught under BREAK=1"; fi
  done
  [ -z "$held" ] || { echo "FAIL: these claims held under BREAK=1, so they can't catch what they check:$held" >&2; exit 1; }
  exit 0
fi

claims="${*:-$SSH_CLAIMS}"
WORK="$(mktemp -d)"
cleanup() {
  [ -n "${OUR_AGENT:-}" ] && kill "$OUR_AGENT" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

s() { ssh -F "$CFG" "$@"; }
sha() { if command -v sha256sum >/dev/null; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi; }

claim_login() {
  # IdentityFile on the command line adds to the config's rather than
  # replacing it, so the broken run gets a config naming only the stranger.
  local cfg="$CFG"
  if [ -n "$BREAK" ]; then
    ssh-keygen -q -t ed25519 -N '' -f "$WORK/stranger"
    sed "s|IdentityFile .*|IdentityFile \"$WORK/stranger\"|" "$CFG" > "$WORK/ssh_config"
    cfg="$WORK/ssh_config"
  fi
  [ "$(ssh -F "$cfg" bastion hostname)" = bastion ]
}

claim_jump() {
  if [ -n "$BREAK" ]; then
    docker exec "$TESTNET-bastion" sed -i 's/^AllowTcpForwarding yes/AllowTcpForwarding no/' /etc/ssh/sshd_config
    docker exec "$TESTNET-bastion" kill -HUP 1
    sleep 1
    local ok=0; [ "$(s box-bare hostname 2>/dev/null)" = box-bare ] && ok=1
    docker exec "$TESTNET-bastion" sed -i 's/^AllowTcpForwarding no/AllowTcpForwarding yes/' /etc/ssh/sshd_config
    docker exec "$TESTNET-bastion" kill -HUP 1
    sleep 1
    [ "$ok" = 1 ]
  else
    [ "$(s box-bare hostname)" = box-bare ]
  fi
}

claim_inner() {
  # A default route shows in /proc/net/route as destination 00000000.
  local box=box-bare; [ -n "$BREAK" ] && box=bastion
  ! s "$box" "awk 'NR > 1 && \$2 == \"00000000\"' /proc/net/route | grep -q ."
}

claim_bare() {
  if [ -n "$BREAK" ]; then
    docker exec "$TESTNET-box-bare" sh -c 'printf "#!/bin/sh\n" > /usr/local/bin/illogical && chmod +x /usr/local/bin/illogical'
  fi
  local rc=0
  s box-bare 'bash -lc "! command -v illogical && ! command -v illogicald && [ ! -e ~/.local/state/illogical ] && [ ! -e ~/.config/illogical ]"' >/dev/null || rc=1
  [ -z "$BREAK" ] || docker exec "$TESTNET-box-bare" rm -f /usr/local/bin/illogical
  return "$rc"
}

claim_stdio() {
  local tty="-T"; [ -n "$BREAK" ] && tty="-tt"
  head -c 1048576 /dev/urandom > "$WORK/sent"
  s "$tty" box-bare cat < "$WORK/sent" > "$WORK/back" 2>/dev/null || true
  [ "$(sha < "$WORK/sent")" = "$(sha < "$WORK/back")" ]
}

# An agent of our own holding the stack's key, started once.
use_agent() {
  [ -n "${OUR_AGENT:-}" ] && return 0
  eval "$(ssh-agent -s)" >/dev/null
  OUR_AGENT="$SSH_AGENT_PID"
  ssh-add -q "$STATE/id_ed25519"
}

claim_agent() {
  use_agent
  local want fwd="-o ForwardAgent=yes"
  want="$(ssh-keygen -lf "$STATE/id_ed25519.pub" | cut -d' ' -f2)"
  [ -n "$BREAK" ] && fwd="-o ForwardAgent=no"
  # shellcheck disable=SC2086
  s $fwd box-bare ssh-add -l 2>/dev/null | grep -qF "$want"
}

claim_push() {
  use_agent
  local fwd="-o ForwardAgent=yes" branch="claim-$$-$RANDOM"
  [ -n "$BREAK" ] && fwd="-o ForwardAgent=no"
  # shellcheck disable=SC2086,SC2016 # options split on purpose; $(...) runs on the box
  s $fwd box-bare 'cd "$(mktemp -d)" && git init -q && git -c user.name=illo -c user.email=illo@box-bare commit -q --allow-empty -m claim && git push -q git@git:/srv/git/repo.git HEAD:refs/heads/'"$branch" 2>/dev/null || return 1
  docker exec "$TESTNET-git" git --git-dir=/srv/git/repo.git rev-parse -q --verify "refs/heads/$branch" >/dev/null
}

claim_linger() {
  local b="$TESTNET-box-systemd" rc=0
  docker exec "$b" loginctl disable-linger illo
  if [ -n "$BREAK" ]; then docker exec "$b" systemctl mask --now polkit.service >/dev/null 2>&1; fi
  # shellcheck disable=SC2016 # expanded on the box
  s box-systemd 'loginctl enable-linger 2>/dev/null && [ "$(loginctl show-user "$USER" -p Linger --value)" = yes ]' || rc=1
  if [ -n "$BREAK" ]; then docker exec "$b" sh -c 'systemctl unmask polkit.service && systemctl start polkit.service' >/dev/null 2>&1; fi
  return "$rc"
}

failed=""
for c in $claims; do
  case " $SSH_CLAIMS " in *" $c "*) ;; *) echo "unknown claim '$c' (claims: $SSH_CLAIMS)" >&2; exit 2 ;; esac
  if "claim_$c"; then echo "[testnet ssh $c] PASS${BREAK:+ (BREAK=1: not caught)}"; else echo "[testnet ssh $c] FAIL${BREAK:+ (BREAK=1: caught)}"; failed="$failed $c"; fi
done
[ -z "$failed" ] || exit 1
