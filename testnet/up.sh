#!/usr/bin/env bash
#
# Bring up one profile of the test stack.
#
#   testnet/up.sh ssh       bastion, box-bare, box-systemd, git (see README.md)
#   testnet/up.sh tailnet   headscale, ts-box and ts-client (S28's comparison)
#
# Makes the stack's keys in testnet/.state (.state-<name> for another
# COMPOSE_PROJECT_NAME) (a client key and one host key per
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
# shellcheck source-path=SCRIPTDIR source=env.sh
. "$HERE/env.sh"
PORT="${ILLOGICAL_TESTNET_SSH_PORT:-22922}"

log() { echo "[testnet up $PROFILE] $*" >&2; }
die() { log "FAIL: $*"; exit 1; }

command -v docker >/dev/null 2>&1 || { echo "SKIP: Docker is not available"; exit 0; }
docker info >/dev/null 2>&1 || { echo "SKIP: Docker is not available"; exit 0; }

case "$PROFILE" in
  ssh) boxes="bastion box-bare box-systemd git" ;;
  tailnet) boxes="ts-box ts-client" ;;
  *) echo "usage: testnet/up.sh ssh|tailnet" >&2; exit 2 ;;
esac

mkdir -p "$STATE"
[ -f "$STATE/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C illogical-testnet -f "$STATE/id_ed25519"
for b in $boxes; do
  [ -f "$STATE/${b}_host_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C "$b" -f "$STATE/${b}_host_ed25519"
done
# Every profile's hosts, so bringing up one profile doesn't drop another's.
: > "$STATE/known_hosts.new"
for k in "$STATE"/*_host_ed25519.pub; do
  b="$(basename "$k" _host_ed25519.pub)"
  echo "$b $(cut -d' ' -f1,2 "$k")" >> "$STATE/known_hosts.new"
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

if [ "$PROFILE" = tailnet ]; then
  # headscale first, for a reusable, ephemeral key the nodes join with.
  docker compose -f "$HERE/compose.yaml" --profile tailnet up -d --wait headscale >&2
  hs() { docker exec "$TESTNET-headscale" headscale "$@"; }
  for _ in $(seq 1 40); do hs users list >/dev/null 2>&1 && break; sleep 0.5; done
  hs users list -o json | grep -q '"name": *"illo"' || hs users create illo >/dev/null
  uid="$(hs users list -o json | tr -d ' \t\n' | sed 's/.*"id":\([0-9]*\),"name":"illo".*/\1/')"
  hs preauthkeys create --user "$uid" --reusable --ephemeral --expiration 24h > "$STATE/tailnet-authkey.new"
  tail -n1 "$STATE/tailnet-authkey.new" > "$STATE/tailnet-authkey" && rm "$STATE/tailnet-authkey.new"
fi

docker compose -f "$HERE/compose.yaml" --profile "$PROFILE" up -d --build --wait >&2

if [ "$PROFILE" = tailnet ]; then
  for _ in $(seq 1 60); do
    if docker exec "$TESTNET-ts-client" tailscale ping -c 1 ts-box >/dev/null 2>&1; then
      log "up: docker exec $TESTNET-ts-client tailscale status"
      exit 0
    fi
    sleep 0.5
  done
  docker compose -f "$HERE/compose.yaml" --profile tailnet logs --tail=40 >&2 || true
  die "ts-client can't reach ts-box over the tailnet"
fi

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
