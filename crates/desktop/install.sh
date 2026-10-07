#!/bin/sh
# Builds the desktop app and installs it for this user, without sudo.
#   Linux: ~/.local/lib/arugula-desktop (the app and the arugulad and
#          arugula it carries), ~/.local/bin/arugula-desktop, its icons
#          and a launcher entry.
#   macOS: ~/Applications/Arugula.app (ad-hoc signed, carrying both).
# An install from before the rename (illogical-desktop, illogical.app) is
# replaced, not left beside it (#505).
# The daemon stays its own service: the app starts the installed one, or
# installs the one it carries (`arugulad install`) when there is none.
# The carried binaries come from ./sidecars.sh (built when missing; it needs
# web/dist and Zig), or put prebuilt ones in binaries/NAME-TRIPLE yourself.
# Needs cargo-tauri (`cargo install tauri-cli --version "^2"`), and on Linux
# libwebkit2gtk-4.1-dev, libayatana-appindicator3-dev, librsvg2-dev.
set -e
cd "$(dirname "$0")"
triple=$(rustc -vV | sed -n 's/^host: //p')
[ -f "binaries/arugulad-$triple" ] && [ -f "binaries/arugula-$triple" ] || ./sidecars.sh
case "$(uname -s)" in
Linux)
  cargo tauri build --bundles deb
  bin="$HOME/.local/bin"
  lib="$HOME/.local/lib/arugula-desktop"
  share="$HOME/.local/share"
  # The app from before the rename (#505): its files go, so there's one app.
  rm -rf "$HOME/.local/lib/illogical-desktop"
  rm -f "$bin/illogical-desktop" "$share/applications/illogical.desktop"
  for size in 32x32 128x128 512x512; do
    rm -f "$share/icons/hicolor/$size/apps/illogical-desktop.png"
  done
  mkdir -p "$bin" "$lib" "$share/applications"
  install -m 755 target/release/arugula-desktop "$lib/arugula-desktop"
  install -m 755 "binaries/arugulad-$triple" "$lib/arugulad"
  install -m 755 "binaries/arugula-$triple" "$lib/arugula"
  ln -sf "$lib/arugula-desktop" "$bin/arugula-desktop"
  for size in 32x32 128x128 512x512; do
    mkdir -p "$share/icons/hicolor/$size/apps"
  done
  install -m 644 icons/32x32.png "$share/icons/hicolor/32x32/apps/arugula-desktop.png"
  install -m 644 icons/128x128.png "$share/icons/hicolor/128x128/apps/arugula-desktop.png"
  install -m 644 icons/icon.png "$share/icons/hicolor/512x512/apps/arugula-desktop.png"
  cat > "$share/applications/arugula.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Arugula
Comment=Terminals that outlive their windows
Exec=$bin/arugula-desktop
Icon=arugula-desktop
StartupWMClass=arugula-desktop
Categories=Development;
Terminal=false
DESKTOP
  command -v update-desktop-database >/dev/null && update-desktop-database "$share/applications" || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$share/icons/hicolor" || true
  echo "installed: $bin/arugula-desktop and $share/applications/arugula.desktop"
  ;;
Darwin)
  cargo tauri build --bundles app
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/Arugula.app" "$HOME/Applications/illogical.app"
  cp -R target/release/bundle/macos/Arugula.app "$HOME/Applications/"
  echo "installed: $HOME/Applications/Arugula.app"
  ;;
*) echo "the desktop app is for Linux and macOS" >&2; exit 1 ;;
esac
