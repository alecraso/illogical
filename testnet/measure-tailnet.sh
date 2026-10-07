#!/usr/bin/env bash
#
# S28's comparison (#153): the same box's daemon reached from the same
# client over ssh (`arugula --ssh`, the M51 bridge over a ControlMaster)
# and over the tailnet (`arugula --host http://<tailnet IP>:7681`, the
# daemon's own port, which ts-box's userspace tailscaled forwards to
# loopback). Runs in the tailnet profile:
#
#   testnet/measure-tailnet.sh [samples]   (brings the profile up if needed)
#
# Needs Docker: without it this fails. ARUGULA_SKIP_DOCKER=1 skips it and
# says that nothing ran.
#
# Puts this tree's static binaries on ts-client (`just static <arch>`, or
# ARUGULA_SSH_BINARIES), installs Arugula on ts-box over ssh, then times
# on ts-client, so docker exec isn't in the numbers:
#
#   request   a whole `arugula ls` (process start, connect, one request),
#             N times
#   export    `arugula export` of a pane holding about 8 MiB of output
#             (a download through the path), 20 times
#
# Prints the median, p90 and slowest in ms. Fails if either path doesn't
# reach the daemon or the two disagree about its panes.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR source=env.sh
. "$HERE/env.sh"
N="${1:-30}"
C="$TESTNET-ts-client"

need_docker
docker exec "$C" true 2>/dev/null || "$HERE/up.sh" tailnet

arch="$(docker exec "$C" uname -m)"
BIN="${ARUGULA_SSH_BINARIES:-$HERE/../target/$arch-unknown-linux-musl/release}"
{ [ -x "$BIN/arugula" ] && [ -x "$BIN/arugulad" ]; } || { echo "no binaries in $BIN; run 'just static $arch'" >&2; exit 1; }
docker exec "$C" mkdir -p /opt/arugula
docker cp -q "$BIN/arugula" "$C:/opt/arugula/arugula"
docker cp -q "$BIN/arugulad" "$C:/opt/arugula/arugulad"

# shellcheck disable=SC2016 # expanded in the container
docker exec -i -e N="$N" "$C" bash -s <<'IN'
set -euo pipefail
export ARUGULA_SSH_BINARIES=/opt/arugula ARUGULA_SSH_INSTALL=yes XDG_RUNTIME_DIR=/run/ilg
mkdir -p "$XDG_RUNTIME_DIR"
I=/opt/arugula/arugula
ip="$(tailscale ip -4 ts-box)"
ssh_=(--ssh illo@ts-box)
tail_=(--host "http://$ip:7681")

"$I" "${ssh_[@]}" ls >/dev/null 2>&1            # installs, starts the daemon, opens the master
ids() { "$I" "$@" --json ls | grep -o '"id": *[0-9]*' | tr -d ' ' | sort; }
a="$(ids "${ssh_[@]}")"
b="$(ids "${tail_[@]}")"
[ -n "$a" ] || { echo "FAIL: no panes over ssh" >&2; exit 1; }
[ "$a" = "$b" ] || { echo "FAIL: ssh and the tailnet see different panes" >&2; exit 1; }

# A pane with about 8 MiB of output, done before timing.
p="$("$I" "${ssh_[@]}" run --wait 'head -c 6291456 /dev/urandom | base64')"
"$I" "${ssh_[@]}" run --wait true >/dev/null

ms() { local s e; s=$(date +%s%N); "$@" >/dev/null; e=$(date +%s%N); echo $(( (e - s) / 1000 )); }
stats() { sort -n | awk '{v[NR]=$1} END {m=v[int((NR+1)/2)]; q=v[int(NR*0.9+0.5)]; printf "%8.1f %8.1f %8.1f\n", m/1000, q/1000, v[NR]/1000}'; }
row() {
  local what=$1 n=$2; shift 2
  printf '%-28s' "$what"
  for _ in $(seq 1 "$n"); do ms "$@"; done | stats
}

bytes="$("$I" "${ssh_[@]}" export "$p" | wc -c)"
echo "ts-box over ssh and over the tailnet, from ts-client (ms)"
printf '%-28s %8s %8s %8s\n' "" median p90 max
row "request, ssh" "$N" "$I" "${ssh_[@]}" ls
row "request, tailnet" "$N" "$I" "${tail_[@]}" ls
row "export $((bytes / 1048576)) MiB, ssh" 20 "$I" "${ssh_[@]}" export "$p"
row "export $((bytes / 1048576)) MiB, tailnet" 20 "$I" "${tail_[@]}" export "$p"
IN
