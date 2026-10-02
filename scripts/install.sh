#!/bin/sh
# Install illogical: download a release for this machine, check it, and run
# `illogicald install`, which puts illogicald and illogical in ~/.local/bin
# and starts the daemon as a service (systemd user unit on Linux, launchd
# agent on macOS). Run it again to upgrade; the daemon's flags are kept.
#
#   curl -fsSL https://illogical.widgets.wtf/install.sh | sh
#
# ILLOGICAL_VERSION=v0.1.0  a release tag (default: the latest)
# ILLOGICAL_NO_START=1      install the service without starting it
set -eu

repo=https://git.inevitable.fyi/jhgaylor/illogical
api=https://git.inevitable.fyi/api/v1/repos/jhgaylor/illogical

say() { printf '%s\n' "$*"; }
die() { printf 'illogical: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "needs $1"; }

need curl
need tar
need uname

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-musl ;;
  Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
  Darwin/arm64) target=aarch64-apple-darwin ;;
  *) die "no release for $(uname -s) $(uname -m); build from source: $repo/src/branch/main/docs/development.md" ;;
esac

version=${ILLOGICAL_VERSION:-}
if [ -z "$version" ]; then
  version=$(curl -fsSL "$api/releases/latest" | sed -n 's/.*"tag_name":"\([^"]*\)".*/\1/p')
  [ -n "$version" ] || die "couldn't find the latest release at $repo/releases"
fi

name="illogical-${version#v}-$target"
base="$repo/releases/download/$version"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "downloading $name"
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" || die "no $name.tar.gz in release $version"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "no SHA256SUMS in release $version"

want=$(grep " $name.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
[ -n "$want" ] || die "$name.tar.gz isn't in SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
  got=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
else
  got=$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)
fi
[ "$want" = "$got" ] || die "checksum mismatch for $name.tar.gz"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
if [ "$(uname -s)" = Linux ] && ! command -v systemctl >/dev/null 2>&1; then
  # No systemd (a container, a sandbox): the binaries, but no service.
  mkdir -p "$HOME/.local/bin"
  for b in illogicald illogical; do
    cp "$tmp/$name/$b" "$HOME/.local/bin/.$b.new" && mv "$HOME/.local/bin/.$b.new" "$HOME/.local/bin/$b"
  done
  say "installed ~/.local/bin/illogicald and ~/.local/bin/illogical"
  say "No systemd here, so no service: start the daemon with  ~/.local/bin/illogicald &"
  nosystemd=1
elif [ -n "${ILLOGICAL_NO_START:-}" ]; then
  "$tmp/$name/illogicald" install --no-start
else
  "$tmp/$name/illogicald" install
fi

say ""
say "illogical $version is installed."
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) say "Add ~/.local/bin to your PATH to use the illogical CLI." ;;
esac
say "Open http://127.0.0.1:7681"
say ""
say "From your phone and other machines on your tailnet:"
say "  tailscale serve --bg --https=443 http://127.0.0.1:7681"
if [ "$(uname -s)" = Linux ] && [ -z "${nosystemd:-}" ]; then
  say "To start it at boot, before you log in:"
  say "  loginctl enable-linger \$USER"
fi
