#!/bin/sh
# Install arugula: download a release for this machine, check it, and run
# `arugulad install`, which puts arugulad and arugula in ~/.local/bin
# and starts the daemon as a service (systemd user unit on Linux, launchd
# agent on macOS). Run it again to upgrade; the daemon's flags are kept.
#
#   curl -fsSL https://illogical.widgets.wtf/install.sh | sh
#   curl -fsSL https://illogical.widgets.wtf/install.sh | sh -s -- --no-app
#
# On a Mac with someone logged in at the screen, it installs the desktop app
# too, from the app's own releases (app-latest, #393): in /Applications if
# you can write there, else ~/Applications, then opens it. The app adopts
# the daemon this set up (#392). curl leaves no quarantine flag, so until
# the app is notarized (#177) this is the way to it without a Gatekeeper
# step (#318).
#
# --app                     install the Mac app even with no one at the
#                           screen (over ssh, say)
# --no-app                  don't install the Mac app
# ARUGULA_VERSION=vX.Y.Z  a release tag (default: the latest)
# ARUGULA_NO_START=1      install the service without starting it
# ARUGULA_DOWNLOAD_URL=…  where the release's files are, instead of GitHub
#                           (a mirror, or a local build: file:///path/to/dist)
# ARUGULA_APP=1 (or 0)    the same as --app (or --no-app)
# ARUGULA_APP_VERSION=app-vX.Y.Z  the app's release (default: app-latest)
# ARUGULA_APP_DOWNLOAD_URL=…      where the app's zip and its SHA256SUMS
#                           are, instead of GitHub
# ARUGULA_APP_DIR=DIR     where Arugula.app goes, instead of
#                           /Applications or ~/Applications
# Each ARUGULA_X can also be ILLOGICAL_X, its name before the rename.
set -eu

# The names from before the rename, for scripts that set them (#505, drop
# in #508). The new one wins when both are set.
for v in VERSION NO_START DOWNLOAD_URL APP APP_VERSION APP_DOWNLOAD_URL APP_DIR; do
  if eval "[ -z \"\${ARUGULA_$v+x}\" ] && [ -n \"\${ILLOGICAL_$v+x}\" ]"; then
    eval "ARUGULA_$v=\$ILLOGICAL_$v"
  fi
done

repo=https://github.com/arugula-salad/illogical

say() { printf '%s\n' "$*"; }
die() { printf 'arugula: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "needs $1"; }

# The Mac app: by default when this is someone's own session at the screen
# (not ssh), since that's who the site's one-liner is for; a server or an
# ssh session gets the daemon alone unless it says --app.
app=${ARUGULA_APP:-auto}
for a in "$@"; do
  case "$a" in
    --app) app=1 ;;
    --no-app) app=0 ;;
    *) die "unknown option $a (--app, --no-app)" ;;
  esac
done

need curl
need tar
need uname

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-musl ;;
  Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
  Darwin/arm64) target=aarch64-apple-darwin ;;
  # An Intel Mac, or a shell under Rosetta on Apple silicon (which gets the
  # native build).
  Darwin/x86_64)
    if [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then target=aarch64-apple-darwin; else target=x86_64-apple-darwin; fi ;;
  *) die "no release for $(uname -s) $(uname -m); build from source: $repo/blob/main/docs/development.md" ;;
esac

version=${ARUGULA_VERSION:-}
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

# --app said so (or ARUGULA_APP=1): a missing app is an error, not a skip.
asked=$app
if [ "$(uname -s)" != Darwin ]; then
  [ "$app" != 1 ] || die "--app is for macOS; on Linux and Windows, the app is at $repo/releases/tag/app-latest"
  app=0
elif [ "$app" = auto ]; then
  # Logged in at the screen as this user, and not over ssh.
  if [ -z "${SSH_CONNECTION:-}${SSH_TTY:-}" ] && [ "$(stat -f %Su /dev/console 2>/dev/null)" = "$(id -un)" ]; then
    app=1
  else
    app=0
  fi
fi

name="arugula-${version#v}-$target"
base=${ARUGULA_DOWNLOAD_URL:-$repo/releases/download/$version}
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

# The Mac app's zip, from the app's own releases (#393; v* releases don't
# carry it), checked against their SHA256SUMS before anything is installed.
# With curl, not a browser: no com.apple.quarantine, so no Gatekeeper step
# for an app that isn't notarized yet.
if [ "$app" = 1 ]; then
  case "$target" in
    aarch64-*) zip=arugula-desktop-macos-arm64.zip ;;
    *) zip=arugula-desktop-macos-x86_64.zip ;;
  esac
  app_version=${ARUGULA_APP_VERSION:-app-latest}
  app_base=${ARUGULA_APP_DOWNLOAD_URL:-$repo/releases/download/$app_version}
  mkdir "$tmp/app"
  say "downloading $zip ($app_version)"
  if curl -fsSL -o "$tmp/app/$zip" "$app_base/$zip" && curl -fsSL -o "$tmp/app/SHA256SUMS" "$app_base/SHA256SUMS"; then
    want=$(grep " $zip\$" "$tmp/app/SHA256SUMS" | cut -d' ' -f1)
    [ -n "$want" ] || die "$zip isn't in $app_version's SHA256SUMS"
    if command -v sha256sum >/dev/null 2>&1; then
      got=$(sha256sum "$tmp/app/$zip" | cut -d' ' -f1)
    else
      got=$(shasum -a 256 "$tmp/app/$zip" | cut -d' ' -f1)
    fi
    [ "$want" = "$got" ] || die "checksum mismatch for $zip"
  elif [ "$asked" = 1 ]; then
    die "no $zip and SHA256SUMS in $app_version (--no-app installs without the app)"
  else
    # No app release yet, or GitHub didn't answer: the daemon alone.
    say "no $zip in $app_version: installing without the app (it's at $repo/releases/tag/app-latest)"
    app=0
  fi
fi

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
os=$(uname -s)
started=""
if [ "$os" = Linux ] && ! command -v systemctl >/dev/null 2>&1; then
  # No systemd (a container, a sandbox): the binaries, but no service.
  mkdir -p "$HOME/.local/bin"
  for b in arugulad arugula; do
    cp "$tmp/$name/$b" "$HOME/.local/bin/.$b.new" && mv "$HOME/.local/bin/.$b.new" "$HOME/.local/bin/$b"
  done
  say "installed ~/.local/bin/arugulad and ~/.local/bin/arugula"
  # An install from before the rename: its names now lead to the new
  # binaries, for the panes and hooks that call them (#505, drop in #508).
  # `arugulad install` does this where there's a service.
  for b in arugulad arugula; do
    o=illogical${b#arugula}
    if [ -e "$HOME/.local/bin/$o" ] || [ -L "$HOME/.local/bin/$o" ]; then
      ln -sf "$b" "$HOME/.local/bin/.$o.new" && mv -f "$HOME/.local/bin/.$o.new" "$HOME/.local/bin/$o"
    fi
  done
  nosystemd=1
elif [ -n "${ARUGULA_NO_START:-}" ]; then
  "$tmp/$name/arugulad" install --no-start
else
  "$tmp/$name/arugulad" install
  started=1
fi

# Where the service listens: its --listen, kept from an earlier install,
# or the default.
listen=127.0.0.1:7681
if [ "$os" = Darwin ]; then
  plist="$HOME/Library/LaunchAgents/arugulad.plist"
  # Or the LaunchDaemon `arugulad install --system` wrote, or (not started,
  # on a machine from before the rename) the old one's (#505).
  for p in "/Library/LaunchDaemons/arugulad.$(id -un).plist" "$HOME/Library/LaunchAgents/illogicald.plist" "/Library/LaunchDaemons/illogicald.$(id -un).plist"; do
    [ -f "$plist" ] || plist=$p
  done
  args=$(sed -n 's:.*<string>\(.*\)</string>.*:\1:p' "$plist" 2>/dev/null || true)
else
  unit="$HOME/.config/systemd/user/arugulad.service"
  [ -f "$unit" ] || unit="$HOME/.config/systemd/user/illogicald.service"
  args=$(sed -n 's/^ExecStart=[^ ]*//p' "$unit" 2>/dev/null || true)
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
    say "Arugula $version is installed and running."
  else
    say "Arugula $version is installed, but it isn't answering at $url yet."
    if [ "$os" = Darwin ]; then
      say "  Its logs:  tail -n 50 ~/Library/Logs/arugulad.log"
    else
      say "  Its logs:  journalctl --user -u arugulad -e"
    fi
  fi
elif [ -n "${nosystemd:-}" ]; then
  if curl -s -o /dev/null -m 1 "$url/"; then
    # An upgrade: the old daemon is still the one running.
    say "Arugula $version is installed. A daemon is already running here (the old one): restart it"
    say "to run this version. Panes started with --keep-panes keep running:"
    say "  kill \$(pgrep -f '^$HOME/.local/bin/(arugulad|illogicald) --keep-panes')"
  else
    say "Arugula $version is installed. No systemd here, so no service: start the daemon with"
  fi
  say "  nohup $HOME/.local/bin/arugulad --keep-panes >>~/arugulad.log 2>&1 &"
  say "(--keep-panes: panes outlive its restarts)"
else
  say "Arugula $version is installed, not started (ARUGULA_NO_START)."
fi

# The Mac app, after the daemon: it finds that one (its plist in
# ~/Library/LaunchAgents, or the --system LaunchDaemon) and adopts it
# rather than starting its own (#392). /Applications if this user can
# write there, else ~/Applications.
if [ "$app" = 1 ]; then
  ditto -x -k "$tmp/app/$zip" "$tmp/app/x" || die "couldn't unpack $zip"
  # The bundle's name is the app's productName (Arugula.app).
  bundle=""
  for b in "$tmp/app/x"/*.app; do [ -d "$b" ] && bundle=$(basename "$b") && break; done
  [ -n "$bundle" ] || die "$zip has no app in it"
  if [ -n "${ARUGULA_APP_DIR:-}" ]; then
    appdir=$ARUGULA_APP_DIR
  elif [ -w /Applications ] && { [ ! -e "/Applications/$bundle" ] || [ -w "/Applications/$bundle" ]; }; then
    appdir=/Applications
  else
    appdir=$HOME/Applications
  fi
  mkdir -p "$appdir"
  # Side by side, then swapped in, so a failed copy leaves the old app.
  rm -rf "$appdir/.$bundle.new"
  ditto "$tmp/app/x/$bundle" "$appdir/.$bundle.new" || die "couldn't copy $bundle to $appdir"
  rm -rf "${appdir:?}/$bundle"
  mv "$appdir/.$bundle.new" "$appdir/$bundle"
  say ""
  say "The app is in $appdir/$bundle."
  # The app from before the rename, illogical.app, is this one now: it
  # goes, so Spotlight and the Dock don't keep opening it (#505, drop in
  # #508).
  if [ "$bundle" != illogical.app ] && [ -d "$appdir/illogical.app" ]; then
    if rm -rf "${appdir:?}/illogical.app" 2>/dev/null; then
      say "  It replaces $appdir/illogical.app (illogical is Arugula now)."
    else
      say "  Remove $appdir/illogical.app (illogical is Arugula now): it couldn't be removed from here."
    fi
  fi
  other=""
  case "$appdir" in
    /Applications) other=$HOME/Applications ;;
    "$HOME/Applications") other=/Applications ;;
  esac
  for o in "$bundle" illogical.app; do
    if [ -n "$other" ] && [ -e "$other/$o" ]; then
      say "  There's an older copy in $other/$o: remove it, so Spotlight and the Dock open this one."
    fi
  done
  if pgrep -f '/(Arugula|arugula|illogical)\.app/Contents/MacOS/' >/dev/null 2>&1; then
    say "  An earlier one is running: quit it (its menu > Quit) and open the app again."
  elif [ -n "$started" ]; then
    open "$appdir/$bundle" || say "  Open it from $appdir."
  fi
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
  say "  Open      arugula web   ($url, with your browser signed in)"
else
  say "  Open      arugula web   ($url signed in, once it's running)"
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
  say "            from your phone, or use Arugula control (Anywhere)"
fi
say "  Anywhere  ${bin}arugulad join https://control.arugula.io"
say "            (also how you add this machine to a team: pick it when you approve)"
say "  Agents    ${bin}arugula agent --help · claude mcp add arugula -- ${bin}arugula mcp"
say "  Hooks     ${bin}arugula hooks install   (Claude Code's questions and approvals as cards)"
say "  Docs      https://illogical.widgets.wtf/#install"
