#!/usr/bin/env bash
#
# The daemon updates itself (#391), on a fresh box-systemd: illogical OLD
# installed as the lingering systemd user service, a pane running a
# counter, and a fake release of NEW served on the box (python3, with
# `releases/latest` redirecting to NEW's tag as GitHub's does). Then:
#   check   the daemon finds NEW and offers Update now (apply: true)
#   apply   POST /api/update/apply: it downloads NEW, checks it against
#           SHA256SUMS and runs NEW's install outside the service
#           (systemd-run); the service restarts onto NEW, which answers,
#           and the old one stopped when asked (systemd didn't kill it)
#   panes   the counter is the same process, and still counting
#   bad     `illogicald update -y` against a release whose SHA256SUMS
#           doesn't match refuses it; the daemon stays NEW
#
#   testnet/selfupdate.sh OLD_DIR NEW_DIR
#
# Each directory holds `illogicald` and `illogical` from `just static` at a
# different version (NEW newer). Its own stack, COMPOSE_PROJECT_NAME
# selfupdate unless set, so it doesn't touch another's boxes.
# shellcheck disable=SC2016 # strings run on the box expand there
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-selfupdate}"
export ILLOGICAL_TESTNET_SSH_PORT="${ILLOGICAL_TESTNET_SSH_PORT:-22991}"
export ILLOGICAL_TESTNET_INNER_NET="${ILLOGICAL_TESTNET_INNER_NET:-10.229.71}"
# shellcheck source-path=SCRIPTDIR source=env.sh
. "$HERE/env.sh"
need_docker

OLD_DIR="${1:?usage: testnet/selfupdate.sh OLD_DIR NEW_DIR}"
NEW_DIR="${2:?usage: testnet/selfupdate.sh OLD_DIR NEW_DIR}"
OLD="$("$OLD_DIR/illogicald" --version | cut -d' ' -f2)"
NEW="$("$NEW_DIR/illogicald" --version | cut -d' ' -f2)"
# Newer than NEW, so it is offered; its sum lies.
BAD="${NEW%.*}.$((${NEW##*.} + 1))"
TARGET="$(uname -m)-unknown-linux-musl"
PORT=7758
failed=0
pass() { echo "[selfupdate $1] ok${2:+: $2}"; }
fail() { echo "[selfupdate $1] FAIL: $2" >&2; failed=1; }

"$HERE/up.sh" ssh >/dev/null
docker compose -f "$HERE/compose.yaml" --profile ssh up -d --force-recreate --wait box-systemd >/dev/null 2>&1
s() { ssh -F "$STATE/ssh_config" box-systemd "$@"; }
until s true 2>/dev/null; do sleep 1; done

# The fake release: NEW's archive and SHA256SUMS, and BAD, whose sum lies.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for v in "$NEW" "$BAD"; do
  d="$work/rel/releases/download/v$v"
  name="illogical-$v-$TARGET"
  mkdir -p "$d" "$work/$name"
  cp "$NEW_DIR/illogicald" "$NEW_DIR/illogical" "$work/$name/"
  tar -czf "$d/$name.tar.gz" -C "$work" "$name"
  if [ "$v" = "$BAD" ]; then sum=$(printf '0%.0s' $(seq 64)); else sum=$(sha256sum "$d/$name.tar.gz" | cut -d' ' -f1); fi
  echo "$sum  $name.tar.gz" > "$d/SHA256SUMS"
done
echo "$NEW" > "$work/rel/latest"
cat > "$work/rel/serve.py" <<'PY'
import http.server, sys
root, port = sys.argv[1], int(sys.argv[2])
class H(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **k):
        super().__init__(*a, directory=root, **k)
    def do_GET(self):
        if self.path.rstrip("/") == "/releases/latest":
            v = open(root + "/latest").read().strip()
            self.send_response(302)
            self.send_header("Location", f"/releases/tag/v{v}")
            self.end_headers()
            return
        super().do_GET()
http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
PY
# The owner's API over the daemon's socket.
cat > "$work/rel/api.py" <<'PY'
import http.client, json, os, socket, sys
class C(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(os.path.expanduser("~/.local/state/illogical/sock"))
c = C("localhost", timeout=10)
c.request(sys.argv[1], sys.argv[2])
r = c.getresponse()
print(r.status, r.read().decode())
PY
tar -C "$work" -cf - rel | s 'rm -rf rel old && tar -xf - && mkdir old'
scp -q -F "$STATE/ssh_config" "$OLD_DIR/illogicald" "$OLD_DIR/illogical" box-systemd:old/
s "nohup setsid python3 rel/serve.py \$HOME/rel $PORT >rel/serve.log 2>&1 </dev/null &"

# OLD as the service, looking for releases on the fake.
s "loginctl enable-linger && old/illogicald install -- --update-url http://127.0.0.1:$PORT/releases/latest" >/dev/null
api() { s "python3 rel/api.py $1 $2"; }
until api GET /api/update 2>/dev/null | grep -q "\"current\":\"$OLD\""; do sleep 1; done
s '.local/bin/illogical run --session work "i=0; while :; do i=\$((i+1)); echo \$i > /tmp/count; sleep 0.2; done"' >/dev/null
sleep 2
counter_pid=$(s 'pgrep -f "[/]tmp/count" | head -1')
daemon_pid=$(s 'systemctl --user show -p MainPID --value illogicald.service')

# check
for _ in $(seq 30); do
  st=$(api GET /api/update)
  grep -q '"newer":true' <<<"$st" && break
  sleep 1
done
if grep -q "\"latest\":\"$NEW\"" <<<"$st" && grep -q '"apply":true' <<<"$st"; then pass check "$OLD finds $NEW, apply: true"; else fail check "$st"; fi

# apply
out=$(api POST /api/update/apply)
grep -q '^200 .*"stage":"downloading"' <<<"$out" || fail apply "POST said: $out"
ok=0
for _ in $(seq 90); do
  if api GET /api/update 2>/dev/null | grep -q "\"current\":\"$NEW\""; then ok=1; break; fi
  sleep 1
done
new_pid=$(s 'systemctl --user show -p MainPID --value illogicald.service')
# It stopped when asked: systemd didn't have to kill it after its timeout.
# (The systemd-run client it waited on is killed with the old cgroup; the
# install itself runs on in its own unit.)
killed=$(s 'journalctl --user -u illogicald.service --no-pager 2>/dev/null | grep -i "stop-sigterm.*timed out" || true')
[ -z "$killed" ] || fail apply "systemd had to kill the old daemon: $killed"
if [ "$ok" = 1 ] && [ "$new_pid" != "$daemon_pid" ] && [ "$(s '.local/bin/illogicald --version')" = "illogicald $NEW" ]; then
  pass apply "$OLD -> $NEW, service pid $daemon_pid -> $new_pid"
else
  fail apply "not $NEW after 90s: $(api GET /api/update 2>&1); update.log: $(s 'cat .local/state/illogical/update.log' 2>&1)"
fi

# panes
a=$(s 'cat /tmp/count'); sleep 1; b=$(s 'cat /tmp/count')
now_pid=$(s 'pgrep -f "[/]tmp/count" | head -1')
if [ -n "$counter_pid" ] && [ "$now_pid" = "$counter_pid" ] && [ "$b" -gt "$a" ]; then
  pass panes "counter pid $counter_pid still counting ($a -> $b)"
else
  fail panes "counter pid $counter_pid -> ${now_pid:-gone}, $a -> $b"
fi

# bad
s "echo $BAD > rel/latest"
out=$(s "ILLOGICAL_UPDATE_URL=http://127.0.0.1:$PORT/releases/latest .local/bin/illogicald update -y" 2>&1 || true)
if grep -q "doesn't match" <<<"$out" && api GET /api/update | grep -q "\"current\":\"$NEW\""; then
  pass bad "refused, still $NEW"
else
  fail bad "$out"
fi

[ -n "${KEEP:-}" ] || "$HERE/down.sh" >/dev/null 2>&1
exit "$failed"
