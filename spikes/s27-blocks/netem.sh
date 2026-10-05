#!/usr/bin/env bash
# The relay's extra hop on a network with real delay (#262): the latency
# spec (tests/latency.spec.ts, Chromium) with control, the box and the
# browser in three containers, each pair on a network of its own, and
# `tc netem` delaying every packet each container sends on each of them:
#
#   client ── a ── control ── b ── box        relayed: client, control, box
#      └────────── c ──────────────┘          direct: client to the box
#
# DELAY_MS (default 10) on every interface makes each link's round trip
# 2 x DELAY_MS, so the relayed path is two links and the direct one is one.
# Needs Docker and the static s27 (see linux-webkit.sh, which builds it).
# COMPOSE_PROJECT_NAME names the containers and networks (default
# illogical-s27-netem); ILLOGICAL_TESTNET_INNER_NET (default 10.231.7) is
# the first three octets of their subnets.
set -euo pipefail
cd "$(dirname "$0")"
P=${COMPOSE_PROJECT_NAME:-illogical-s27-netem}
NET=${ILLOGICAL_TESTNET_INNER_NET:-10.231.7}
DELAY=${DELAY_MS:-10}
arch=$(uname -m); [ "$arch" = arm64 ] && arch=aarch64
BIN=target/$arch-unknown-linux-musl/release/s27
command -v docker >/dev/null || { echo "netem.sh needs Docker" >&2; exit 1; }
[ -x "$BIN" ] && [ -f .run/cert/spki.txt ] && [ -d dist ] || { echo "build the spike first: the static $BIN, dist/ and .run/cert" >&2; exit 2; }

image=$P-image
docker build -q -t "$image" -f netem/Containerfile netem >/dev/null
cleanup() {
  docker rm -f "$P-client" "$P-control" "$P-box" >/dev/null 2>&1 || true
  docker network rm "$P-a" "$P-b" "$P-c" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup
docker network create --subnet "$NET.0/26" "$P-a" >/dev/null
docker network create --subnet "$NET.64/26" "$P-b" >/dev/null
docker network create --subnet "$NET.128/26" "$P-c" >/dev/null

# name, first network and address, second network and address
start() {
  docker run -d --name "$P-$1" --cap-add NET_ADMIN --network "$P-$2" --ip "$3" -v "$PWD:/src:ro" "$image" sleep infinity >/dev/null
  docker network connect --ip "$5" "$P-$4" "$P-$1"
  docker exec "$P-$1" sh -c "set -e
    mkdir -p /work/target && cd /src
    tar cf - --exclude=./node_modules --exclude=./target --exclude=./test-results . | tar xf - -C /work
    cp $BIN /work/target/s27
    for i in \$(ls /sys/class/net | grep '^eth'); do tc qdisc add dev \$i root netem delay ${DELAY}ms; done"
}
start control a "$NET.10" b "$NET.74"
start box b "$NET.75" c "$NET.138"
start client a "$NET.11" c "$NET.139"

docker exec -d -w /work "$P-control" sh -c "target/s27 control --listen 0.0.0.0:7767 --dial-listen 0.0.0.0:7770 --dir .run/cert --web dist --marker S27-PLAINTEXT-MARKER >/tmp/out 2>/tmp/err"
docker exec -d -w /work -e S27_BIN=target/s27 "$P-box" sh -c "node netem/box.ts ws://$NET.74:7770/relay/dial >/tmp/out 2>/tmp/err"
for _ in $(seq 1 100); do docker exec "$P-box" test -s /tmp/out && break; sleep 0.2; done
box=$(docker exec "$P-box" head -1 /tmp/out)
[ -n "$box" ] || { docker exec "$P-box" cat /tmp/err; echo "the box didn't start" >&2; exit 1; }
id=$(node -e 'console.log(JSON.parse(process.argv[1]).id)' "$box")
noise=$(node -e 'console.log(JSON.parse(process.argv[1]).noise)' "$box")
site=$(node -e 'console.log(JSON.parse(process.argv[1]).site)' "$box")
stack=$(printf '{"controlPort":7767,"origin":"https://control.test:7767","daemon":{"id":"%s","noise":"%s"},"admin":"%s:7771","directPort":7772,"site":%s}' "$id" "$noise" "$NET.138" "$site")
resolve="MAP control.test $NET.10, MAP *.blocks.test $NET.10, MAP daemon.test $NET.138, MAP *.direct.test $NET.138"

echo "round trips from the client (ms, ${DELAY} ms netem on every interface):"
for to in "control $NET.10:7767" "box $NET.138:7772"; do
  # shellcheck disable=SC2086 # a name and an address, split on purpose
  set -- $to
  docker exec "$P-client" node -e '
    const [h, p] = process.argv[1].split(":"); const n = require("net"); const xs = [];
    (async () => { for (let i = 0; i < 5; i++) { const t = performance.now();
      await new Promise((r, j) => { const s = n.connect(+p, h, () => { s.destroy(); r(); }); s.on("error", j); });
      xs.push(performance.now() - t); } console.log(process.argv[2], "tcp connect", xs.map((x) => x.toFixed(1)).join(" ")); })();' "$2" "$1"
done

docker exec -w /work -e S27_BUILT=1 -e "S27_STACK=$stack" -e "S27_RESOLVE=$resolve" -e CI=1 "$P-client" sh -c "
  # Only Playwright: the spike's package.json would bring the dev servers.
  echo '{\"type\": \"module\"}' >package.json
  npm i --no-save --no-audit --no-fund @playwright/test@1.63.0 >/tmp/npm.log 2>&1 || { cat /tmp/npm.log; exit 1; }
  npx playwright test -c netem/playwright.config.ts"
