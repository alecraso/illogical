#!/bin/sh
# Builds the desktop app and installs it for this user, without sudo.
#   Linux: ~/.local/lib/illogical-desktop (the app and the illogicald and
#          illogical it carries), ~/.local/bin/illogical-desktop, its icons
#          and a launcher entry.
#   macOS: ~/Applications/illogical.app (ad-hoc signed, carrying both).
# The daemon stays its own service: the app starts the installed one, or
# installs the one it carries (`illogicald install`) when there is none.
# The carried binaries come from ./sidecars.sh (built when missing; it needs
# web/dist and Zig), or put prebuilt ones in binaries/NAME-TRIPLE yourself.
# Needs cargo-tauri (`cargo install tauri-cli --version "^2"`), and on Linux
# libwebkit2gtk-4.1-dev, libayatana-appindicator3-dev, librsvg2-dev.
set -e
cd "$(dirname "$0")"
triple=$(rustc -vV | sed -n 's/^host: //p')
[ -f "binaries/illogicald-$triple" ] && [ -f "binaries/illogical-$triple" ] || ./sidecars.sh
case "$(uname -s)" in
Linux)
  cargo tauri build --bundles deb
  bin="$HOME/.local/bin"
  lib="$HOME/.local/lib/illogical-desktop"
  share="$HOME/.local/share"
  mkdir -p "$bin" "$lib" "$share/applications"
  install -m 755 target/release/illogical-desktop "$lib/illogical-desktop"
  install -m 755 "binaries/illogicald-$triple" "$lib/illogicald"
  install -m 755 "binaries/illogical-$triple" "$lib/illogical"
  ln -sf "$lib/illogical-desktop" "$bin/illogical-desktop"
  for size in 32x32 128x128 512x512; do
    mkdir -p "$share/icons/hicolor/$size/apps"
  done
  install -m 644 icons/32x32.png "$share/icons/hicolor/32x32/apps/illogical-desktop.png"
  install -m 644 icons/128x128.png "$share/icons/hicolor/128x128/apps/illogical-desktop.png"
  install -m 644 icons/icon.png "$share/icons/hicolor/512x512/apps/illogical-desktop.png"
  cat > "$share/applications/illogical.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=illogical
Comment=Terminals that outlive their windows
Exec=$bin/illogical-desktop
Icon=illogical-desktop
StartupWMClass=illogical-desktop
Categories=Development;
Terminal=false
DESKTOP
  command -v update-desktop-database >/dev/null && update-desktop-database "$share/applications" || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$share/icons/hicolor" || true
  echo "installed: $bin/illogical-desktop and $share/applications/illogical.desktop"
  ;;
Darwin)
  cargo tauri build --bundles app
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/illogical.app"
  cp -R target/release/bundle/macos/illogical.app "$HOME/Applications/"
  echo "installed: $HOME/Applications/illogical.app"
  ;;
*) echo "the desktop app is for Linux and macOS" >&2; exit 1 ;;
esac
