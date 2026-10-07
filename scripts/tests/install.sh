#!/usr/bin/env bash
# Tests scripts/install.sh without a network or a real machine: uname,
# sysctl, curl and tailscale are stand-ins, and the "release" is a local
# tarball whose arugulad records how it was run. Each case checks which
# release it fetched and that it installed (or refused) as it should.
#
#   scripts/tests/install.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
version=v1.2.3
fail=0

# The release: a tarball per target with stand-in binaries, and SHA256SUMS.
rel=$work/release
mkdir -p "$rel"
for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin; do
  n=arugula-${version#v}-$t
  mkdir -p "$work/src/$n"
  # shellcheck disable=SC2016 # expands in the stand-in, not here
  printf '#!/bin/sh\necho "arugulad $*" >>"$HOME/calls"\n' >"$work/src/$n/arugulad"
  printf '#!/bin/sh\necho arugula\n' >"$work/src/$n/arugula"
  chmod +x "$work/src/$n/arugulad" "$work/src/$n/arugula"
  tar -C "$work/src" -czf "$rel/$n.tar.gz" "$n"
done
(cd "$rel" && { sha256sum -- *.tar.gz 2>/dev/null || shasum -a 256 -- *.tar.gz; } >SHA256SUMS)

# The app's release (app-latest, #393): a stand-in zip per Mac, which the
# ditto stand-in "unpacks" into an arugula.app that names it.
apprel=$work/app-release
mkdir -p "$apprel"
for a in arm64 x86_64; do echo "app $a" >"$apprel/arugula-desktop-macos-$a.zip"; done
(cd "$apprel" && { sha256sum -- *.zip 2>/dev/null || shasum -a 256 -- *.zip; } >SHA256SUMS)

# Stand-ins. FAKE_OS/FAKE_ARCH are uname's answers; FAKE_ARM64 is
# sysctl's hw.optional.arm64 (unset: no such key, as on an Intel Mac).
stubs=$work/stubs
mkdir -p "$stubs"
cat >"$stubs/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) echo "$FAKE_OS" ;; -m) echo "$FAKE_ARCH" ;; *) echo "$FAKE_OS" ;; esac
EOF
cat >"$stubs/sysctl" <<'EOF'
#!/bin/sh
[ "$2" = hw.optional.arm64 ] && [ -n "${FAKE_ARM64:-}" ] && { echo "$FAKE_ARM64"; exit 0; }
exit 1
EOF
# curl: serves the release's files by name, and app-latest's, and logs each
# URL; anything else (the daemon's address, the GitHub API) fails.
cat >"$stubs/curl" <<EOF
#!/bin/sh
out="" url=""
while [ \$# -gt 0 ]; do
  case "\$1" in -o) out=\$2; shift ;; -*) ;; *) url=\$1 ;; esac
  shift
done
echo "\$url" >>"\$HOME/fetched"
case "\$url" in
  */releases/download/$version/*) f="$rel/\${url##*/}" ;;
  */releases/download/app-latest/*) f="$apprel/\${url##*/}" ;;
  */releases/download/*) exit 22 ;;
  *) exit 7 ;;
esac
[ -f "\$f" ] || exit 22
if [ -n "\$out" ]; then cp "\$f" "\$out"; else cat "\$f"; fi
EOF
printf '#!/bin/sh\nexit 1\n' >"$stubs/tailscale"
# ditto -x -k ZIP DIR makes DIR/Arugula.app with the zip's contents in
# it; ditto SRC DST copies. stat answers who owns /dev/console
# (FAKE_CONSOLE); open logs; no app is running (pgrep).
cat >"$stubs/ditto" <<'EOF'
#!/bin/sh
if [ "$1" = -x ]; then mkdir -p "$4/Arugula.app" && cp "$3" "$4/Arugula.app/zip"; else cp -R "$1" "$2"; fi
EOF
cat >"$stubs/stat" <<'EOF'
#!/bin/sh
[ -n "${FAKE_CONSOLE:-}" ] && echo "$FAKE_CONSOLE"
EOF
# shellcheck disable=SC2016 # expands in the stand-in, not here
printf '#!/bin/sh\necho "open $*" >>"$HOME/calls"\n' >"$stubs/open"
printf '#!/bin/sh\nexit 1\n' >"$stubs/pgrep"
chmod +x "$stubs"/*

# run NAME OS ARCH [ARM64] [ARG...]: install.sh as that machine, in a
# fresh HOME, over ssh (so no Mac app unless an ARG or $with says so),
# with the app put in $home/Apps, never the real /Applications. $with is
# more VAR=value for env; $release is how it names the release and asks
# not to start (the old ILLOGICAL_ names work too, #505); $path is PATH.
# Sets $home, $status and $out.
with=()
release=("ARUGULA_VERSION=$version" "ARUGULA_NO_START=1")
path=$stubs:$PATH
run() {
  home=$work/home-$1
  mkdir -p "$home"
  status=0
  out=$(env -u ARUGULA_VERSION -u ARUGULA_NO_START -u ILLOGICAL_VERSION -u ILLOGICAL_NO_START \
    HOME="$home" PATH="$path" FAKE_OS="$2" FAKE_ARCH="$3" FAKE_ARM64="${4:-}" \
    SSH_CONNECTION="10.0.0.1 22 10.0.0.2 22" ARUGULA_APP_DIR="$home/Apps" \
    "${release[@]}" ${with[@]+"${with[@]}"} \
    sh "$root/scripts/install.sh" "${@:5}" 2>&1) || status=$?
}

ok() { printf 'ok    %s\n' "$1"; }
bad() { printf 'FAIL  %s: %s\n%s\n' "$1" "$2" "$out" | sed '3,$s/^/      /'; fail=1; }

# expect NAME OS ARCH ARM64 TARGET: it fetches TARGET's tarball and installs.
expect() {
  run "$1" "$2" "$3" "$4"
  local want=arugula-${version#v}-$5.tar.gz
  if [ "$status" != 0 ]; then bad "$1" "exited $status"; return; fi
  if ! grep -qx ".*/$want" "$home/fetched"; then bad "$1" "didn't fetch $want: $(tr '\n' ' ' <"$home/fetched")"; return; fi
  if [ "$(grep -c '\.tar\.gz$' "$home/fetched")" != 1 ]; then bad "$1" "fetched more than one tarball"; return; fi
  if grep -q '\.zip$' "$home/fetched"; then bad "$1" "fetched the app over ssh"; return; fi
  # The service install (Darwin, or Linux with systemd), else a plain copy.
  if [ ! -f "$home/calls" ] && [ ! -x "$home/.local/bin/arugulad" ]; then bad "$1" "installed nothing"; return; fi
  ok "$1 → $5"
}

# refuse NAME OS ARCH: it stops before downloading anything.
refuse() {
  run "$1" "$2" "$3"
  if [ "$status" = 0 ]; then bad "$1" "installed on an unsupported machine"; return; fi
  if grep -q '\.tar\.gz$' "$home/fetched" 2>/dev/null; then bad "$1" "downloaded a release"; return; fi
  if ! grep -q "no release for $2 $3" <<<"$out"; then bad "$1" "didn't say why"; return; fi
  ok "$1 refused"
}

expect linux-x86_64 Linux x86_64 "" x86_64-unknown-linux-musl
expect linux-amd64 Linux amd64 "" x86_64-unknown-linux-musl
expect linux-aarch64 Linux aarch64 "" aarch64-unknown-linux-musl
expect linux-arm64 Linux arm64 "" aarch64-unknown-linux-musl
expect mac-apple-silicon Darwin arm64 "" aarch64-apple-darwin
expect mac-intel Darwin x86_64 "" x86_64-apple-darwin
expect mac-intel-arm64-key-0 Darwin x86_64 0 x86_64-apple-darwin
# A shell under Rosetta says x86_64 on Apple silicon; it gets the native build.
expect mac-rosetta Darwin x86_64 1 aarch64-apple-darwin
refuse freebsd FreeBSD amd64
refuse linux-riscv Linux riscv64

# A tarball that doesn't match SHA256SUMS is refused, not installed.
good=$rel/arugula-${version#v}-x86_64-unknown-linux-musl.tar.gz
cp "$good" "$work/good.tar.gz"
printf 'tampered' >>"$good"
run tampered Linux x86_64
if [ "$status" != 0 ] && grep -q 'checksum mismatch' <<<"$out" && [ ! -f "$home/calls" ] && [ ! -e "$home/.local/bin/arugulad" ]; then
  ok "tampered tarball refused"
else
  bad tampered "installed a tarball that doesn't match SHA256SUMS"
fi
cp "$work/good.tar.gz" "$good"

# app NAME OS ARCH ARM64 ZIP [ARG...]: the daemon as above, then the Mac
# app ZIP from app-latest, checked against app-latest's SHA256SUMS, in
# $home/Apps.
app() {
  run "$1" "$2" "$3" "$4" "${@:6}"
  local want=arugula-desktop-macos-$5.zip
  if [ "$status" != 0 ]; then bad "$1" "exited $status"; return; fi
  if [ ! -f "$home/calls" ]; then bad "$1" "didn't install the daemon"; return; fi
  if ! grep -qx ".*/releases/download/app-latest/$want" "$home/fetched"; then bad "$1" "didn't fetch $want from app-latest: $(tr '\n' ' ' <"$home/fetched")"; return; fi
  if ! grep -qx ".*/releases/download/app-latest/SHA256SUMS" "$home/fetched"; then bad "$1" "didn't fetch app-latest's SHA256SUMS"; return; fi
  if [ "$(cat "$home/Apps/Arugula.app/zip" 2>/dev/null)" != "app $5" ]; then bad "$1" "no app from $want in $home/Apps"; return; fi
  ok "$1 → $want"
}

app mac-app Darwin arm64 "" arm64 --app
app mac-app-intel Darwin x86_64 "" x86_64 --app
app mac-app-rosetta Darwin x86_64 1 arm64 --app
with=(ARUGULA_APP=1)
app mac-app-env Darwin arm64 "" arm64
# At the screen (the console is this user's, no ssh): the app by default.
with=(SSH_CONNECTION= SSH_TTY= FAKE_CONSOLE="$(id -un)")
app mac-at-screen Darwin arm64 "" arm64
with=()

# --no-app (or ssh) leaves it out; on Linux --app is an error.
for c in "mac-no-app Darwin arm64 --no-app" "mac-ssh Darwin arm64"; do
  read -r n o a f <<<"$c"
  # shellcheck disable=SC2086 # no flag, or one
  run "$n" "$o" "$a" "" $f
  if [ "$status" = 0 ] && [ -f "$home/calls" ] && ! grep -q '\.zip$' "$home/fetched" && [ ! -e "$home/Apps" ]; then
    ok "$n: no app"
  else
    bad "$n" "installed the app, or not the daemon"
  fi
done
run linux-app Linux x86_64 "" --app
if [ "$status" != 0 ] && grep -q -- '--app is for macOS' <<<"$out" && [ ! -e "$home/fetched" ]; then
  ok "linux --app refused"
else
  bad linux-app "didn't refuse --app on Linux before downloading"
fi

# No app release (app-v9.9.9 isn't there): at the screen the daemon goes in
# alone; with --app it stops before installing anything.
with=(SSH_CONNECTION= SSH_TTY= FAKE_CONSOLE="$(id -un)" ARUGULA_APP_VERSION=app-v9.9.9)
run no-app-release Darwin arm64 ""
if [ "$status" = 0 ] && [ -f "$home/calls" ] && [ ! -e "$home/Apps" ] && grep -q 'installing without the app' <<<"$out"; then
  ok "no app release: the daemon alone"
else
  bad no-app-release "didn't install the daemon alone"
fi
run no-app-release-asked Darwin arm64 "" --app
if [ "$status" != 0 ] && [ ! -f "$home/calls" ] && [ ! -e "$home/Apps" ]; then
  ok "no app release, --app: refused"
else
  bad no-app-release-asked "installed without the app it was asked for"
fi
with=()

# An app zip that doesn't match app-latest's SHA256SUMS: nothing installed.
zip=$apprel/arugula-desktop-macos-arm64.zip
cp "$zip" "$work/good.zip"
printf 'tampered' >>"$zip"
run tampered-app Darwin arm64 "" --app
if [ "$status" != 0 ] && grep -q 'checksum mismatch for arugula-desktop-macos-arm64.zip' <<<"$out" && [ ! -f "$home/calls" ] && [ ! -e "$home/Apps" ]; then
  ok "tampered app zip refused"
else
  bad tampered-app "installed with an app zip that doesn't match SHA256SUMS"
fi
cp "$work/good.zip" "$zip"

# The names from before the rename (#505). ILLOGICAL_VERSION and
# ILLOGICAL_NO_START still pick the release and keep it from starting.
release=("ILLOGICAL_VERSION=$version" "ILLOGICAL_NO_START=1")
run old-env Linux x86_64
if [ "$status" = 0 ] && grep -qx ".*/releases/download/$version/arugula-${version#v}-x86_64-unknown-linux-musl.tar.gz" "$home/fetched" \
  && grep -qx 'arugulad install --no-start' "$home/calls"; then
  ok "ILLOGICAL_VERSION and ILLOGICAL_NO_START"
else
  bad old-env "didn't honour ILLOGICAL_VERSION and ILLOGICAL_NO_START: $(cat "$home/calls" 2>/dev/null)"
fi
# The new name wins over the old.
release=("ARUGULA_VERSION=$version" "ILLOGICAL_VERSION=v0.0.1" "ARUGULA_NO_START=1")
run both-env Linux x86_64
if [ "$status" = 0 ] && ! grep -q 'v0.0.1' "$home/fetched"; then ok "ARUGULA_VERSION over ILLOGICAL_VERSION"; else bad both-env "took ILLOGICAL_VERSION over ARUGULA_VERSION"; fi
release=("ARUGULA_VERSION=$version" "ARUGULA_NO_START=1")

# A Mac with the app from before the rename: illogical.app goes, Arugula.app
# takes its place (#505).
mkdir -p "$work/home-mac-old-app/Apps/illogical.app"
app mac-old-app Darwin arm64 "" arm64 --app
if [ -e "$home/Apps/illogical.app" ]; then bad mac-old-app "left illogical.app beside Arugula.app"; else ok "illogical.app replaced"; fi

# No systemd, over an install from before the rename: illogicald and
# illogical in ~/.local/bin lead to the new binaries (#505). PATH without
# systemctl: the stubs and the few tools install.sh uses.
nosd=$work/nosd
mkdir -p "$nosd"
for c in sh mktemp rm mkdir cp mv ln grep cut sed tar gzip sha256sum shasum id cat tr basename dirname sleep chmod; do
  w=$(command -v "$c" 2>/dev/null) && ln -sf "$w" "$nosd/$c"
done
path=$stubs:$nosd
mkdir -p "$work/home-nosystemd-old/.local/bin"
printf 'old\n' >"$work/home-nosystemd-old/.local/bin/illogicald"
printf 'old\n' >"$work/home-nosystemd-old/.local/bin/illogical"
run nosystemd-old Linux x86_64
b=$home/.local/bin
if [ "$status" = 0 ] && [ -x "$b/arugulad" ] && [ "$(readlink "$b/illogicald")" = arugulad ] && [ "$(readlink "$b/illogical")" = arugula ]; then
  ok "no systemd: the old names lead to the new binaries"
else
  bad nosystemd-old "illogicald → $(readlink "$b/illogicald"), illogical → $(readlink "$b/illogical")"
fi
# A fresh one gets no old names.
run nosystemd-new Linux x86_64
if [ "$status" = 0 ] && [ -x "$home/.local/bin/arugula" ] && [ ! -e "$home/.local/bin/illogical" ]; then ok "no systemd, fresh: no old names"; else bad nosystemd-new "made old names on a fresh install"; fi
path=$stubs:$PATH

# #532: nothing is unpacked in the temp dir (often noexec): with TMPDIR
# somewhere that can't be written, it installs all the same.
with=(TMPDIR=/nonexistent/tmp)
run tmp-noexec Linux x86_64
with=()
left=$(find "$home/.cache/arugula" -maxdepth 1 -name 'install.*' 2>/dev/null || true)
if [ "$status" = 0 ] && [ -d "$home/.cache/arugula" ] && [ -z "$left" ]; then
  ok "unpacked under the cache, not the temp dir"
else
  bad tmp-noexec "with TMPDIR unusable it didn't install, or left its unpacked files"
fi

[ "$fail" = 0 ] && echo "install.sh: all passed"
exit "$fail"
