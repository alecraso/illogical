#!/usr/bin/env bash
#
# Bring up one profile of the test stack.
#
#   testnet/up.sh ssh     bastion, box-bare and box-systemd (see README.md)
#
# Makes the stack's keys in testnet/.state (a client key and one host key per
# box) and writes testnet/.state/ssh_config, which reaches every box by name
# with strict host key checking:
#
#   ssh -F testnet/.state/ssh_config box-bare
#
# Re-running it is safe: existing keys are kept and the containers are
# recreated only if their config changed.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROFILE="${1:-ssh}"
STATE="$HERE/.state"
PORT="${ILLOGICAL_TESTNET_SSH_PORT:-22922}"

log() { echo "[testnet up $PROFILE] $*" >&2; }
die() { log "FAIL: $*"; exit 1; }

# Docker is required: without it this fails. ILLOGICAL_SKIP_DOCKER=1 skips
# on purpose, and says loudly that nothing ran.
if ! docker info >/dev/null 2>&1; then
  if [ "${ILLOGICAL_SKIP_DOCKER:-}" = 1 ]; then
    echo "SKIP (ILLOGICAL_SKIP_DOCKER=1): Docker is not available, so the testnet did NOT run" >&2
    exit 0
  fi
  echo "FAIL: Docker is not available (\`docker info\` failed). The testnet needs it: start Docker, or set ILLOGICAL_SKIP_DOCKER=1 to skip on purpose." >&2
  exit 1
fi

case "$PROFILE" in
  ssh) boxes="bastion box-bare box-systemd" ;;
  *) echo "usage: testnet/up.sh ssh   (the only profile so far)" >&2; exit 2 ;;
esac

mkdir -p "$STATE"
[ -f "$STATE/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C illogical-testnet -f "$STATE/id_ed25519"
: > "$STATE/known_hosts.new"
for b in $boxes; do
  [ -f "$STATE/${b}_host_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C "$b" -f "$STATE/${b}_host_ed25519"
  echo "$b $(cut -d' ' -f1,2 "$STATE/${b}_host_ed25519.pub")" >> "$STATE/known_hosts.new"
done
mv "$STATE/known_hosts.new" "$STATE/known_hosts"

cat > "$STATE/ssh_config" <<CFG
# Written by testnet/up.sh. Every box by name, keys only, strict host keys.
Host bastion
  HostName 127.0.0.1
  Port $PORT
  HostKeyAlias bastion

Host box-bare
  HostName box-bare
  ProxyJump bastion

Host box-systemd
  HostName box-systemd
  ProxyJump bastion

Host *
  User illo
  IdentityFile "$STATE/id_ed25519"
  IdentitiesOnly yes
  UserKnownHostsFile "$STATE/known_hosts"
  StrictHostKeyChecking yes
  BatchMode yes
  ConnectTimeout 10
CFG

docker compose -f "$HERE/compose.yaml" --profile "$PROFILE" up -d --build --wait >&2

# sshd answers as soon as its container is up, but give it a few tries.
for _ in $(seq 1 20); do
  if ssh -F "$STATE/ssh_config" box-bare true 2>/dev/null; then
    log "up: ssh -F $STATE/ssh_config box-bare"
    exit 0
  fi
  sleep 0.5
done
ssh -F "$STATE/ssh_config" -v box-bare true || true
docker compose -f "$HERE/compose.yaml" --profile "$PROFILE" logs --tail=40 >&2 || true
die "box-bare did not answer through the bastion"
