#!/usr/bin/env bash
#
# Bring up one profile of the test stack.
#
#   testnet/up.sh ssh       bastion, box-bare, box-systemd and git (see README.md)
#   testnet/up.sh control   the same, and illogical-control with its fakes;
#                           needs this tree's static binaries (`just static`)
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

need_docker

case "$PROFILE" in
  ssh) boxes="bastion box-bare box-systemd git"; profiles="--profile ssh" ;;
  control) boxes="bastion box-bare box-systemd git"; profiles="--profile ssh --profile control" ;;
  tailnet) boxes="ts-box ts-client"; profiles="--profile tailnet" ;;
  *) echo "usage: testnet/up.sh ssh|control|tailnet" >&2; exit 2 ;;
esac

if [ "$PROFILE" = control ]; then
  # The static binaries for Docker's architecture, mounted into control
  # (and installed on the boxes by the tests).
  arch="$(docker info --format '{{.Architecture}}')"
  case "$arch" in arm64) arch=aarch64 ;; amd64) arch=x86_64 ;; esac
  export ILLOGICAL_TESTNET_BINARIES="${ILLOGICAL_TESTNET_BINARIES:-$(cd "$HERE/.." && pwd)/target/$arch-unknown-linux-musl/release}"
  [ -x "$ILLOGICAL_TESTNET_BINARIES/illogical-control" ] || die "no $ILLOGICAL_TESTNET_BINARIES/illogical-control: run 'just static $arch'"
fi

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

# shellcheck disable=SC2086 # two words for the control profile
docker compose -f "$HERE/compose.yaml" $profiles up -d --build --wait >&2

if [ "$PROFILE" = control ]; then
  # What the control profile's tests need (test.sh reads it): control's
  # public URL and where the host reaches it and the fake GitHub.
  net="${ILLOGICAL_TESTNET_INNER_NET:-10.229.80}"
  cport="${ILLOGICAL_TESTNET_CONTROL_PORT:-22980}"
  cat > "$STATE/control.env" <<ENV
# Written by testnet/up.sh control. The stack's settings, so a test that
# recreates a box gets the same networks.
CONTROL_URL=http://$net.10:8080
CONTROL_VIA="--via http://$net.10:8080=http://127.0.0.1:$cport --via http://fakes:9001=http://127.0.0.1:${ILLOGICAL_TESTNET_FAKES_PORT:-22981}"
export ILLOGICAL_TESTNET_BINARIES="$ILLOGICAL_TESTNET_BINARIES"
export ILLOGICAL_TESTNET_INNER_NET=$net
export ILLOGICAL_TESTNET_SSH_PORT=$PORT
export ILLOGICAL_TESTNET_CONTROL_PORT=$cport
export ILLOGICAL_TESTNET_FAKES_PORT=${ILLOGICAL_TESTNET_FAKES_PORT:-22981}
export ILLOGICAL_TESTNET_GUEST_SSH_PORT=${ILLOGICAL_TESTNET_GUEST_SSH_PORT:-22982}
ENV
  for _ in $(seq 1 40); do
    curl -fsS "http://127.0.0.1:$cport/control.json" >/dev/null 2>&1 && break
    sleep 0.5
  done
  curl -fsS "http://127.0.0.1:$cport/control.json" >/dev/null || die "control did not answer on 127.0.0.1:$cport"
  log "control: http://$net.10:8080 (127.0.0.1:$cport from here)"
fi

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
# shellcheck disable=SC2086
docker compose -f "$HERE/compose.yaml" $profiles logs --tail=40 >&2 || true
die "box-bare did not answer through the bastion"
