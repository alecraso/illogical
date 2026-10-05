#!/bin/sh
# Install illogical: download a release for this machine, check it, and run
# `illogicald install`, which puts illogicald and illogical in ~/.local/bin
# and starts the daemon as a service (systemd user unit on Linux, launchd
# agent on macOS). Run it again to upgrade; the daemon's flags are kept.
#
#   curl -fsSL https://illogical.widgets.wtf/install.sh | sh
#
# ILLOGICAL_VERSION=vX.Y.Z  a release tag (default: the latest)
# ILLOGICAL_NO_START=1      install the service without starting it
# ILLOGICAL_DOWNLOAD_URL=…  where the release's files are, instead of GitHub
#                           (a mirror, or a local build: file:///path/to/dist)
set -eu

repo=https://github.com/arugula-salad/illogical

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
  *) die "no release for $(uname -s) $(uname -m); build from source: $repo/blob/main/docs/development.md" ;;
esac

version=${ILLOGICAL_VERSION:-}
if [ -z "$version" ]; then
  # releases/latest redirects to releases/tag/<tag>. Not GitHub's API: its
  # unauthenticated limit (60 an hour) is shared by everyone behind an IP.
  latest=$(curl -fsSL -o /dev/null -w '%{url_effective}' "$repo/releases/latest") || latest=""
  version=${latest##*/releases/tag/}
  case "$version" in
    v[0-9]*) ;;
    *) die "couldn't find the latest release at $repo/releases" ;;
  esac
fi

name="illogical-${version#v}-$target"
base=${ILLOGICAL_DOWNLOAD_URL:-$repo/releases/download/$version}
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
os=$(uname -s)
started=""
if [ "$os" = Linux ] && ! command -v systemctl >/dev/null 2>&1; then
  # No systemd (a container, a sandbox): the binaries, but no service.
  mkdir -p "$HOME/.local/bin"
  for b in illogicald illogical; do
    cp "$tmp/$name/$b" "$HOME/.local/bin/.$b.new" && mv "$HOME/.local/bin/.$b.new" "$HOME/.local/bin/$b"
  done
  say "installed ~/.local/bin/illogicald and ~/.local/bin/illogical"
  nosystemd=1
elif [ -n "${ILLOGICAL_NO_START:-}" ]; then
  "$tmp/$name/illogicald" install --no-start
else
  "$tmp/$name/illogicald" install
  started=1
fi

# Where the service listens: its --listen, kept from an earlier install,
# or the default.
listen=127.0.0.1:7681
if [ "$os" = Darwin ]; then
  args=$(sed -n 's:.*<string>\(.*\)</string>.*:\1:p' "$HOME/Library/LaunchAgents/illogicald.plist" 2>/dev/null || true)
else
  args=$(sed -n 's/^ExecStart=[^ ]*//p' "$HOME/.config/systemd/user/illogicald.service" 2>/dev/null || true)
fi
prev=""
for a in $args; do
  case "$a" in --listen=*) listen=${a#--listen=} ;; esac
  [ "$prev" = --listen ] && listen=$a
  prev=$a
done
url="http://$listen"

# The commands below, as you can type them now.
bin=""
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) bin="$HOME/.local/bin/" ;;
esac

say ""
if [ -n "$started" ]; then
  # It answers within a second or two; a crash loop never does.
  up=""
  i=0
  while [ $i -lt 20 ]; do
    if curl -s -o /dev/null -m 1 "$url/"; then up=1; break; fi
    sleep 0.5
    i=$((i + 1))
  done
  if [ -n "$up" ]; then
    say "illogical $version is installed and running."
  else
    say "illogical $version is installed, but it isn't answering at $url yet."
    if [ "$os" = Darwin ]; then
      say "  Its logs:  tail -n 50 ~/Library/Logs/illogicald.log"
    else
      say "  Its logs:  journalctl --user -u illogicald -e"
    fi
  fi
elif [ -n "${nosystemd:-}" ]; then
  if curl -s -o /dev/null -m 1 "$url/"; then
    # An upgrade: the old daemon is still the one running.
    say "illogical $version is installed. A daemon is already running here (the old one): restart it"
    say "to run this version. Panes started with --keep-panes keep running:"
    say "  kill \$(pgrep -f '^$HOME/.local/bin/illogicald --keep-panes')"
  else
    say "illogical $version is installed. No systemd here, so no service: start the daemon with"
  fi
  say "  nohup $HOME/.local/bin/illogicald --keep-panes >>~/illogicald.log 2>&1 &"
  say "(--keep-panes: panes outlive its restarts)"
else
  say "illogical $version is installed, not started (ILLOGICAL_NO_START)."
fi

if [ -n "$bin" ]; then
  say ""
  say "$HOME/.local/bin isn't on your PATH. Add it:"
  case "${SHELL:-}" in
    */fish) say "  fish_add_path ~/.local/bin" ;;
    */zsh) say "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.zshrc && exec zsh" ;;
    */bash)
      rc=~/.bashrc
      [ "$os" = Darwin ] && rc=~/.bash_profile
      say "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> $rc && exec bash"
      ;;
    *) say "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.profile  (then log in again)" ;;
  esac
fi

# The phone: this machine's tailnet name, if tailscale is here and up.
ts=""
if command -v tailscale >/dev/null 2>&1; then
  ts=tailscale
elif [ -x /Applications/Tailscale.app/Contents/MacOS/Tailscale ]; then
  ts=/Applications/Tailscale.app/Contents/MacOS/Tailscale
fi
dns=""
if [ -n "$ts" ]; then
  # Self comes before Peer in the status; its DNSName ends in a dot.
  dns=$("$ts" status --json 2>/dev/null | awk '/"Self":/ { s = 1 } s && /"DNSName":/ { gsub(/[",]/, "", $2); sub(/\.$/, "", $2); print $2; exit }' || true)
fi

say ""
if [ -n "$started" ]; then
  say "  Open      illogical web   ($url, with your browser signed in)"
else
  say "  Open      illogical web   ($url signed in, once it's running)"
fi
serve="tailscale serve --bg --https=443 $url"
if [ -n "$dns" ]; then
  if "$ts" serve status 2>/dev/null | grep -q "$listen"; then
    say "  Phone     https://$dns"
  else
    say "  Phone     $serve"
    say "            then https://$dns"
    if [ "$os" = Linux ] && [ "$(id -u)" != 0 ]; then
      say "            (first time: sudo tailscale set --operator=\$USER, and HTTPS on"
      say "            at https://login.tailscale.com/admin/dns)"
    else
      say "            (first time: HTTPS on at https://login.tailscale.com/admin/dns)"
    fi
  fi
elif [ -n "$ts" ]; then
  say "  Phone     tailscale up, then $serve"
else
  say "  Phone     install Tailscale (https://tailscale.com/download) to reach it"
  say "            from your phone, or use illogical control (Anywhere)"
fi
say "  Anywhere  ${bin}illogicald join https://control.illogical.widgets.wtf"
say "            (also how you add this machine to a team: pick it when you approve)"
say "  Agents    ${bin}illogical agent --help · claude mcp add illogical -- ${bin}illogical mcp"
say "  Docs      https://illogical.widgets.wtf/#install"
