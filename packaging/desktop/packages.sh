#!/usr/bin/env bash
# The Linux packages install on a fresh system (`just desktop-packages
# ARCH`): the .deb on Ubuntu 22.04 and the .rpm on Fedora, each in a
# container of ARCH. After the install: the app and both sidecars are in
# /usr/bin and run, every library the app needs resolves, and the desktop
# file claims illogical:// links (x-scheme-handler/illogical), which
# xdg-mime then hands to it.
set -euo pipefail
arch=${1:-$(uname -m)}
[ "$arch" = arm64 ] && arch=aarch64
dist=${DIST:-$(cd "$(dirname "$0")/../.." && pwd)/dist}
case "$arch" in x86_64) platform=linux/amd64 ;; aarch64) platform=linux/arm64 ;; *) echo "arch: x86_64 or aarch64" >&2; exit 2 ;; esac
engine=$(command -v podman || command -v docker) || { echo "needs podman or docker" >&2; exit 1; }
name=illogical-desktop-linux-$arch
failed=0

check='set -e
for b in illogical-desktop illogicald illogical; do test -x /usr/bin/$b || { echo "no /usr/bin/$b"; exit 1; }; done
/usr/bin/illogicald --version >/dev/null && /usr/bin/illogical --version >/dev/null
missing=$(ldd /usr/bin/illogical-desktop | grep "not found" || true)
[ -z "$missing" ] || { echo "missing libraries: $missing"; exit 1; }
desktop=$(grep -l "^Exec=illogical-desktop" /usr/share/applications/*.desktop)
grep -q "^MimeType=.*x-scheme-handler/illogical" "$desktop" || { echo "$desktop has no x-scheme-handler/illogical"; exit 1; }
update-desktop-database /usr/share/applications 2>/dev/null || true
handler=$(HOME=/root xdg-mime query default x-scheme-handler/illogical)
[ "$handler" = "$(basename "$desktop")" ] || { echo "xdg-mime picks \"$handler\" for illogical://"; exit 1; }
echo "installed: $(basename "$desktop") handles illogical://"'

run() {
  local what=$1 image=$2 install=$3 file=$4
  if out=$("$engine" run --rm --platform "$platform" -v "$dist:/dist:ro" "$image" bash -c "$install /dist/$file >/tmp/install.log 2>&1 || { tail -20 /tmp/install.log; exit 1; }
$check" 2>&1); then
    echo "[packages $what] ok: $(echo "$out" | tail -1)"
  else
    echo "[packages $what] FAIL: $out" >&2
    failed=1
  fi
}
run "deb $arch" docker.io/library/ubuntu:22.04 \
  'export DEBIAN_FRONTEND=noninteractive; apt-get update -q && apt-get install -y -q xdg-utils desktop-file-utils && apt-get install -y -q' "$name.deb"
run "rpm $arch" docker.io/library/fedora:42 'dnf install -y -q xdg-utils desktop-file-utils && dnf install -y -q' "$name.rpm"
exit $failed
