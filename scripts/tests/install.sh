#!/usr/bin/env bash
# Tests scripts/install.sh without a network or a real machine: uname,
# sysctl, curl and tailscale are stand-ins, and the "release" is a local
# tarball whose illogicald records how it was run. Each case checks which
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
  n=illogical-${version#v}-$t
  mkdir -p "$work/src/$n"
  # shellcheck disable=SC2016 # expands in the stand-in, not here
  printf '#!/bin/sh\necho "illogicald $*" >>"$HOME/calls"\n' >"$work/src/$n/illogicald"
  printf '#!/bin/sh\necho illogical\n' >"$work/src/$n/illogical"
  chmod +x "$work/src/$n/illogicald" "$work/src/$n/illogical"
  tar -C "$work/src" -czf "$rel/$n.tar.gz" "$n"
done
(cd "$rel" && { sha256sum -- *.tar.gz 2>/dev/null || shasum -a 256 -- *.tar.gz; } >SHA256SUMS)

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
# curl: serves the release's files by name and logs each URL; anything
# else (the daemon's address, the GitHub API) fails.
cat >"$stubs/curl" <<EOF
#!/bin/sh
out="" url=""
while [ \$# -gt 0 ]; do
  case "\$1" in -o) out=\$2; shift ;; -*) ;; *) url=\$1 ;; esac
  shift
done
echo "\$url" >>"\$HOME/fetched"
f="$rel/\${url##*/}"
case "\$url" in */releases/download/$version/*) [ -f "\$f" ] || exit 22 ;; *) exit 7 ;; esac
if [ -n "\$out" ]; then cp "\$f" "\$out"; else cat "\$f"; fi
EOF
printf '#!/bin/sh\nexit 1\n' >"$stubs/tailscale"
chmod +x "$stubs"/*

# run NAME OS ARCH [ARM64]: install.sh as that machine, in a fresh HOME.
# Sets $home, $status and $out.
run() {
  home=$work/home-$1
  mkdir -p "$home"
  status=0
  out=$(env HOME="$home" PATH="$stubs:$PATH" FAKE_OS="$2" FAKE_ARCH="$3" FAKE_ARM64="${4:-}" \
    ILLOGICAL_VERSION=$version ILLOGICAL_NO_START=1 sh "$root/scripts/install.sh" 2>&1) || status=$?
}

ok() { printf 'ok    %s\n' "$1"; }
bad() { printf 'FAIL  %s: %s\n%s\n' "$1" "$2" "$out" | sed '3,$s/^/      /'; fail=1; }

# expect NAME OS ARCH ARM64 TARGET: it fetches TARGET's tarball and installs.
expect() {
  run "$1" "$2" "$3" "$4"
  local want=illogical-${version#v}-$5.tar.gz
  if [ "$status" != 0 ]; then bad "$1" "exited $status"; return; fi
  if ! grep -qx ".*/$want" "$home/fetched"; then bad "$1" "didn't fetch $want: $(tr '\n' ' ' <"$home/fetched")"; return; fi
  if [ "$(grep -c '\.tar\.gz$' "$home/fetched")" != 1 ]; then bad "$1" "fetched more than one tarball"; return; fi
  # The service install (Darwin, or Linux with systemd), else a plain copy.
  if [ ! -f "$home/calls" ] && [ ! -x "$home/.local/bin/illogicald" ]; then bad "$1" "installed nothing"; return; fi
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
good=$rel/illogical-${version#v}-x86_64-unknown-linux-musl.tar.gz
cp "$good" "$work/good.tar.gz"
printf 'tampered' >>"$good"
run tampered Linux x86_64
if [ "$status" != 0 ] && grep -q 'checksum mismatch' <<<"$out" && [ ! -f "$home/calls" ] && [ ! -e "$home/.local/bin/illogicald" ]; then
  ok "tampered tarball refused"
else
  bad tampered "installed a tarball that doesn't match SHA256SUMS"
fi
cp "$work/good.tar.gz" "$good"

[ "$fail" = 0 ] && echo "install.sh: all passed"
exit "$fail"
