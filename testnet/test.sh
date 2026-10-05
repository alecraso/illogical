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
# The control profile's claims (it needs node, and the CLI built:
# `cargo build -p illogical`, or ILLOGICAL_CLI):
#
#   signin  A person signs in to control with (the fake) GitHub, from the
#           host, and their first device is trusted on enrollment
#           (web/fixtures/device.ts). Broken: GitHub is down (the fakes
#           stopped, and started afterwards).
#   reach   box-systemd, on the inner network with no route out, reaches
#           control at its address there. Broken: control taken off the
#           inner network (and put back).
#   m52     M52 end to end: on a fresh box-systemd, `illogical --ssh
#           box-systemd join` installs illogical and starts its daemon, and
#           its code is approved by a headless device; the box is then on
#           the account's device list and online, and with the ssh master
#           closed and the bastion paused, a marker round-trips through a
#           pane over control's relay. After `docker restart` it comes back
#           to the relay by itself and the pane answers again. Broken:
#           polkit masked on the box, so lingering can't be turned on and
#           the daemon doesn't start again after the restart.
#   unreachable
#           A box that can't reach control (box-bare joining the hosted
#           control, with no route out) says so, naming the box, control
#           and `illogical --ssh box tui`, and is still reachable over
#           --ssh. Broken: it joins the stack's control, which it can reach.
#   m49     M49 end to end: box-systemd and box-bare join the stack's
#           control (approved by the headless device); box-bare's daemon
#           also listens on the inner network and lists that URL, and
#           box-systemd lists none. On the bastion, which has the CLI and no
#           daemon (so no hosts.json), `illogical login` shows a code the
#           device approves; then `illogical hosts` lists both machines
#           from control, and `--host box-bare` (direct) and `--host
#           box-systemd` (relayed) each `run`, `ls` and `capture`. Broken:
#           the CLI isn't logged in, so neither name resolves.
#
# Needs `testnet/up.sh <profile>` first. Exit codes: 0 every claim held, 1 a
# claim failed or there's no Docker (ILLOGICAL_SKIP_DOCKER=1 makes that a
# loud skip with exit 0: nothing runs), 2 usage.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROFILE="${1:-ssh}"; shift || true
# shellcheck source-path=SCRIPTDIR source=env.sh
. "$HERE/env.sh"
CFG="$STATE/ssh_config"
BREAK="${BREAK:-}"
SSH_CLAIMS="login jump inner bare stdio agent push linger"
CONTROL_CLAIMS="signin reach m52 unreachable m49"

need_docker

case "$PROFILE" in
  ssh) CLAIMS="$SSH_CLAIMS" ;;
  control) CLAIMS="$CONTROL_CLAIMS" ;;
  *) echo "usage: testnet/test.sh ssh|control [--break | claim...]   (ssh: $SSH_CLAIMS; control: $CONTROL_CLAIMS)" >&2; exit 2 ;;
esac
[ -f "$CFG" ] || { echo "no $CFG; run 'just testnet up $PROFILE' first" >&2; exit 1; }
if [ "$PROFILE" = control ]; then
  [ -f "$STATE/control.env" ] || { echo "no $STATE/control.env; run 'just testnet up control' first" >&2; exit 1; }
  # shellcheck source=/dev/null
  . "$STATE/control.env"
fi

if [ "${1:-}" = --break ]; then
  held=""
  for c in $CLAIMS; do
    if BREAK=1 "$0" "$PROFILE" "$c" >/dev/null 2>&1; then held="$held $c"; else echo "[testnet $PROFILE $c] caught under BREAK=1"; fi
  done
  [ -z "$held" ] || { echo "FAIL: these claims held under BREAK=1, so they can't catch what they check:$held" >&2; exit 1; }
  exit 0
fi

claims="${*:-$CLAIMS}"
WORK="$(mktemp -d)"
# The CLI's ssh masters, in a short directory (a socket path is at most
# 104 bytes on macOS).
RT="/tmp/ilg-$TESTNET-$$"
cleanup() {
  [ -z "${OUR_AGENT:-}" ] || kill "$OUR_AGENT" 2>/dev/null || true
  [ -z "${PAUSED:-}" ] || docker unpause "$TESTNET-bastion" >/dev/null 2>&1 || true
  if [ -d "$RT" ]; then
    for b in box-bare box-systemd; do ssh -F "$CFG" -o ControlPath="$RT/illogical-ssh/%C" -O exit "$b" >/dev/null 2>&1 || true; done
    rm -rf "$RT"
  fi
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

# The control profile's helpers.
ROOT="$(cd "$HERE/.." && pwd)"
CLI="${ILLOGICAL_CLI:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/illogical}"
note() { echo "[testnet $PROFILE] $*" >&2; }
# The headless approving device (web/fixtures/device-cli.ts), one per run.
dev() { node --experimental-strip-types --no-warnings "$ROOT/web/fixtures/device-cli.ts" --state "$WORK/device.json" "$@"; }
# shellcheck disable=SC2086 # CONTROL_VIA is several words
signin() { dev signin --control "$CONTROL_URL" $CONTROL_VIA --login "$1"; }
field() { sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }
# The CLI on the host, as a person runs it, with the stack's ssh config.
cli() {
  mkdir -p "$RT"
  ILLOGICAL_SSH="ssh -F $CFG" ILLOGICAL_SSH_BINARIES="$ILLOGICAL_TESTNET_BINARIES" ILLOGICAL_SSH_INSTALL=yes \
    XDG_RUNTIME_DIR="$RT" "$CLI" "$@"
}
# A box as new: recreated, and answering ssh.
fresh() {
  docker compose -f "$HERE/compose.yaml" --profile ssh up -d --force-recreate --wait "$1" >/dev/null 2>&1 || return 1
  for _ in $(seq 1 40); do s "$1" true 2>/dev/null && return 0; sleep 0.5; done
  return 1
}

claim_signin() {
  local rc=0
  if [ -n "$BREAK" ]; then docker stop "$TESTNET-fakes" >/dev/null; fi
  signin "signin-$$" > "$WORK/signin.json" 2>/dev/null || rc=1
  grep -q '"approved":true' "$WORK/signin.json" || rc=1
  if [ -n "$BREAK" ]; then docker start "$TESTNET-fakes" >/dev/null; sleep 1; fi
  return "$rc"
}

claim_reach() {
  local ip="${CONTROL_URL#http://}" rc=0
  ip="${ip%%:*}"
  if [ -n "$BREAK" ]; then docker network disconnect "$TESTNET-inner" "$TESTNET-control"; fi
  s box-systemd bash -s > "$WORK/reach" 2>/dev/null <<SH || rc=1
exec 3<>/dev/tcp/$ip/8080
printf 'GET /control.json HTTP/1.0\r\n\r\n' >&3
head -1 <&3
SH
  grep -q ' 200 ' "$WORK/reach" || rc=1
  if [ -n "$BREAK" ]; then docker network connect --ip "$ip" "$TESTNET-inner" "$TESTNET-control"; fi
  return "$rc"
}

claim_m52() {
  local box=box-systemd fp code="" pid started ok=0
  fresh "$box" || { note "m52: couldn't recreate $box"; return 1; }
  if [ -n "$BREAK" ]; then docker exec "$TESTNET-$box" systemctl mask --now polkit.service >/dev/null 2>&1; fi
  fp="$(signin "m52-$$-$RANDOM" | field fingerprint)"
  [ -n "$fp" ] || { note "m52: no device signed in"; return 1; }
  # One command, as a person types it; its code shows in this terminal.
  cli --ssh "$box" join "$CONTROL_URL" --account "$fp" < /dev/null > "$WORK/join.out" 2>&1 &
  pid=$!
  for _ in $(seq 1 120); do
    code="$(sed -n 's/.*#join=\([A-Z0-9]*-[A-Z0-9]*\).*/\1/p' "$WORK/join.out" | head -1)"
    [ -n "$code" ] && break
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.5
  done
  if [ -z "$code" ]; then note "m52: no join code"; cat "$WORK/join.out" >&2; kill "$pid" 2>/dev/null; return 1; fi
  dev approve "$code" > /dev/null || { kill "$pid" 2>/dev/null; return 1; }
  wait "$pid" || { note "m52: join failed"; cat "$WORK/join.out" >&2; return 1; }
  grep -q "Joined." "$WORK/join.out" || return 1
  dev devices | grep -q "\"kind\":\"daemon\",\"name\":\"$box\"" || { note "m52: $box isn't on the device list"; return 1; }
  dev online "$box" 60 > /dev/null || return 1
  # ssh is out of the picture: the CLI's master closed, the bastion paused.
  ssh -F "$CFG" -o ControlPath="$RT/illogical-ssh/%C" -O exit "$box" >/dev/null 2>&1 || true
  docker pause "$TESTNET-bastion" > /dev/null && PAUSED=1
  dev pane "$box" "M52-RELAY-$$" 30 > /dev/null || { note "m52: no pane over the relay"; return 1; }
  # A reboot: it comes back to the relay by itself.
  started="$(docker inspect -f '{{.State.StartedAt}}' "$TESTNET-$box")"
  docker restart "$TESTNET-$box" > /dev/null
  [ "$(docker inspect -f '{{.State.StartedAt}}' "$TESTNET-$box")" != "$started" ] || return 1
  for _ in $(seq 1 45); do
    if dev pane "$box" "M52-REBOOT-$$" 10 > /dev/null 2>&1; then ok=1; break; fi
    sleep 2
  done
  docker unpause "$TESTNET-bastion" > /dev/null && PAUSED=
  [ "$ok" = 1 ] || { note "m52: no pane over the relay after the restart"; return 1; }
}

claim_unreachable() {
  local box=box-bare url=https://control.illogical.widgets.wtf pid rc=0
  fresh "$box" || return 1
  [ -z "$BREAK" ] || url="$CONTROL_URL"
  cli --ssh "$box" join "$url" < /dev/null > "$WORK/unreachable.out" 2>&1 &
  pid=$!
  for _ in $(seq 1 60); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null && rc=1
  grep -qF "$box can't reach control at $url" "$WORK/unreachable.out" || rc=1
  grep -qF "illogical --ssh $box tui" "$WORK/unreachable.out" || rc=1
  cli --ssh "$box" ls > /dev/null || { note "unreachable: $box isn't reachable over --ssh"; rc=1; }
  # Leave it bare for the ssh profile's claims.
  fresh "$box" || true
  return "$rc"
}

# Join a box to the stack's control over ssh, approved by the device
# (signed in already). Its daemon is up when this returns.
join_box() {
  local box="$1" fp="$2" code="" pid
  cli --ssh "$box" join "$CONTROL_URL" --account "$fp" < /dev/null > "$WORK/join-$box.out" 2>&1 &
  pid=$!
  for _ in $(seq 1 120); do
    code="$(sed -n 's/.*#join=\([A-Z0-9]*-[A-Z0-9]*\).*/\1/p' "$WORK/join-$box.out" | head -1)"
    [ -n "$code" ] && break
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.5
  done
  if [ -z "$code" ]; then note "join $box: no code"; cat "$WORK/join-$box.out" >&2; kill "$pid" 2>/dev/null; return 1; fi
  dev approve "$code" > /dev/null || { kill "$pid" 2>/dev/null; return 1; }
  wait "$pid" || { note "join $box failed"; cat "$WORK/join-$box.out" >&2; return 1; }
  dev online "$box" 60 > /dev/null
}

claim_m49() {
  local fp code pid b out pane ok
  # The bastion is the CLI's machine: on the inner network, with no daemon.
  # Leave it as it was, whatever happens.
  # shellcheck disable=SC2329,SC2317 # run by the trap below
  m49_tidy() { s bastion 'rm -rf ~/.local/bin/illogical ~/.config/illogical' >/dev/null 2>&1 || true; }
  m49_tidy
  if ! fresh box-bare || ! fresh box-systemd; then note "m49: couldn't recreate the boxes"; return 1; fi
  fp="$(signin "m49-$$-$RANDOM" | field fingerprint)"
  [ -n "$fp" ] || { note "m49: no device signed in"; return 1; }
  join_box box-systemd "$fp" || return 1
  join_box box-bare "$fp" || return 1
  # box-bare: reachable on the inner network too, and says so to control.
  # shellcheck disable=SC2016 # expanded on the box
  s box-bare 'pkill -x illogicald; for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -x illogicald >/dev/null || break; sleep 0.5; done
    ILLOGICAL_DIRECT_URL=http://box-bare:7681 nohup setsid ~/.local/bin/illogicald --keep-panes --listen 0.0.0.0:7681 \
      </dev/null >"$HOME/.local/state/illogicald.log" 2>&1 &' || return 1
  for _ in $(seq 1 60); do
    dev online box-bare 2 2>/dev/null | grep -q 'box-bare:7681' && break
    sleep 1
  done
  dev online box-bare 2 | grep -q 'box-bare:7681' || { note "m49: box-bare doesn't list its direct URL"; return 1; }
  # The CLI on the bastion, as the boxes got theirs.
  s bastion 'mkdir -p ~/.local/bin && cat > ~/.local/bin/illogical && chmod 755 ~/.local/bin/illogical' \
    < "$ILLOGICAL_TESTNET_BINARIES/illogical" || return 1
  if [ -z "$BREAK" ]; then
    s bastion ".local/bin/illogical login $CONTROL_URL --name bastion --account $fp" < /dev/null > "$WORK/login.out" 2>&1 &
    pid=$!
    code=""
    for _ in $(seq 1 60); do
      code="$(sed -n 's/.*#join=\([A-Z0-9]*-[A-Z0-9]*\).*/\1/p' "$WORK/login.out" | head -1)"
      [ -n "$code" ] && break
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.5
    done
    [ -n "$code" ] || { note "m49: illogical login showed no code"; cat "$WORK/login.out" >&2; m49_tidy; return 1; }
    dev approve "$code" > /dev/null || { kill "$pid" 2>/dev/null; m49_tidy; return 1; }
    if ! wait "$pid" || ! grep -q "Logged in." "$WORK/login.out"; then
      note "m49: login failed"; cat "$WORK/login.out" >&2; m49_tidy; return 1
    fi
    dev devices | grep -q '"kind":"cli","name":"bastion"' || { note "m49: the CLI isn't on the device list"; m49_tidy; return 1; }
  fi
  ok=1
  s bastion '.local/bin/illogical hosts' > "$WORK/hosts.out" 2>&1 || ok=0
  grep -q '^box-bare .*direct http://box-bare:7681.*(control: ' "$WORK/hosts.out" || ok=0
  grep -q '^box-systemd .*relayed.*(control: ' "$WORK/hosts.out" || ok=0
  [ "$ok" = 1 ] || { note "m49: illogical hosts on the bastion:"; cat "$WORK/hosts.out" >&2; }
  for b in box-bare:direct box-systemd:relayed; do
    local how="${b#*:}"; b="${b%%:*}"
    # shellcheck disable=SC2016 # $((...)) is for the pane's shell
    out="$(s bastion "ILLOGICAL_VERBOSE=1 .local/bin/illogical --host $b --json run -- 'echo M49-$b-\$((6*7))'" 2>&1)" || { note "m49: run on $b: $out"; ok=0; continue; }
    grep -q "$b: $how" <<< "$out" || { note "m49: $b wasn't reached $how: $out"; ok=0; }
    pane="$(sed -n 's/.*"pane": *\([0-9]*\).*/\1/p' <<< "$out" | head -1)"
    [ -n "$pane" ] || { note "m49: no pane from $b: $out"; ok=0; continue; }
    s bastion ".local/bin/illogical --host $b ls" | grep -q "^%$pane " || { note "m49: ls on $b doesn't show %$pane"; ok=0; }
    local seen=0
    for _ in $(seq 1 30); do
      if s bastion ".local/bin/illogical --host $b capture $pane" 2>/dev/null | grep -q "M49-$b-42"; then seen=1; break; fi
      sleep 0.5
    done
    [ "$seen" = 1 ] || { note "m49: capture on $b never showed M49-$b-42"; ok=0; }
  done
  m49_tidy
  # Leave box-bare bare for the ssh profile's claims.
  fresh box-bare || true
  [ "$ok" = 1 ]
}

failed=""
for c in $claims; do
  case " $CLAIMS " in *" $c "*) ;; *) echo "unknown claim '$c' (claims: $CLAIMS)" >&2; exit 2 ;; esac
  if "claim_$c"; then echo "[testnet $PROFILE $c] PASS${BREAK:+ (BREAK=1: not caught)}"; else echo "[testnet $PROFILE $c] FAIL${BREAK:+ (BREAK=1: caught)}"; failed="$failed $c"; fi
done
[ -z "$failed" ] || exit 1
