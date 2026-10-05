#!/bin/sh
# safari/safari.ts against the desktop app's own WebKitGTK: Ubuntu 22.04's
# libwebkit2gtk-4.1, in the desktop app's Xvfb image (`just desktop-xvfb`
# builds it). WebKitWebDriver drives that library's MiniBrowser. The
# spike's control, daemon and dev server run here; in the container the
# test names are 127.0.0.1, where socat carries control's port to this
# machine (host.docker.internal), and the test CA is in the system store.
# Build the spike first (see safari/safari.ts).
#
#   ./webkitgtk.sh            S27_SAFARI_PORT (7753) is control's port,
#                             SAFARI_DRIVER_PORT (7744) WebKitWebDriver's
set -eu
cd "$(dirname "$0")"
engine=$(command -v docker || command -v podman)
image=${S27_WEBKITGTK_IMAGE:-$("$engine" images --format '{{.Repository}}:{{.Tag}}' | grep '^illogical-desktop-xvfb:jammy-' | head -1)}
[ -n "$image" ] || { echo "no illogical-desktop-xvfb image: run \`just desktop-xvfb\` once" >&2; exit 1; }
cport=${S27_SAFARI_PORT:-7753}
dport=${SAFARI_DRIVER_PORT:-7744}
arch=$(uname -m); [ "$arch" = arm64 ] && arch=aarch64
name=s27-webkitgtk-$$
trap '"$engine" rm -f "$name" >/dev/null 2>&1 || true' EXIT
hosts=""
for h in control.test b-s27safari1.blocks.test b-s27safari2.blocks.test; do hosts="$hosts --add-host $h:127.0.0.1"; done
# shellcheck disable=SC2086 # the --add-host flags, split on purpose
"$engine" run -d --name "$name" $hosts -p "127.0.0.1:$dport:4444" -v "$PWD/.run/cert/cert.pem:/s27-cert.pem:ro" \
  -e WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 -e WEBKIT_DISABLE_COMPOSITING_MODE=1 -e LIBGL_ALWAYS_SOFTWARE=1 \
  "$image" bash -c "set -e
    apt-get update -qq && apt-get install -y -qq --no-install-recommends webkit2gtk-driver socat >/dev/null
    cp /s27-cert.pem /usr/local/share/ca-certificates/s27.crt && update-ca-certificates >/dev/null
    dpkg-query -W -f '\${Package} \${Version}\n' libwebkit2gtk-4.1-0 webkit2gtk-driver
    # Control's port on this side goes to the same port on the host.
    socat TCP-LISTEN:$cport,fork,reuseaddr TCP:host.docker.internal:$cport &
    Xvfb :99 -screen 0 1280x900x24 -nolisten tcp >/dev/null 2>&1 &
    export DISPLAY=:99
    exec dbus-run-session -- WebKitWebDriver --port=4444 --host=all" >/dev/null
for _ in $(seq 1 300); do curl -sf "http://127.0.0.1:$dport/status" >/dev/null && break; sleep 1; done
curl -sf "http://127.0.0.1:$dport/status" >/dev/null || { "$engine" logs "$name"; echo "WebKitWebDriver never answered" >&2; exit 1; }
"$engine" logs "$name" 2>&1 | grep -E '(libwebkit2gtk-4.1-0|webkit2gtk-driver) [0-9]'
SAFARIDRIVER_URL="http://127.0.0.1:$dport" S27_SAFARI_PORT="$cport" \
  S27_MINIBROWSER="/usr/lib/$arch-linux-gnu/webkit2gtk-4.1/MiniBrowser" node safari/safari.ts --webkitgtk "$@"
