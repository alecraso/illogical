#!/usr/bin/env bash
# The Linux desktop bundles, inside the build container (`just
# desktop-linux ARCH`, packaging/desktop/Containerfile): a .deb, an .rpm
# and an AppImage for this container's architecture, carrying the static
# sidecars in crates/desktop/binaries. Writes them to /dist as
# illogical-desktop-linux-ARCH.{deb,rpm,AppImage}.
#
# TAURI_SIGNING_PRIVATE_KEY (and _PASSWORD), when set, also sign the
# AppImage for the updater (illogical-desktop-linux-ARCH.AppImage.sig).
# Extra arguments go to `cargo tauri build` (a test's --config).
set -euo pipefail
arch=$(uname -m)
host=$arch-unknown-linux-gnu
case "$arch" in
  x86_64) deb=amd64 ;;
  aarch64) deb=arm64 ;;
  *) echo "no desktop build for $arch" >&2; exit 1 ;;
esac
cd /src/crates/desktop
v=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
for b in illogicald illogical; do
  [ -s "binaries/$b-$host" ] || { echo "missing binaries/$b-$host (just static $arch)" >&2; exit 1; }
done
cargo tauri build --bundles deb,rpm,appimage "$@"
out=$CARGO_TARGET_DIR/release/bundle
# linuxdeploy patches an RPATH into every ELF in usr/bin, which breaks the
# static-pie sidecars (they segfault at start, so the app could never
# install its daemon). Put the originals back and pack the AppImage again
# with the same plugin.
cd "$out/appimage"
for b in illogicald illogical; do install -m 755 "/src/crates/desktop/binaries/$b-$host" "illogical.AppDir/usr/bin/$b"; done
for b in illogicald illogical; do "illogical.AppDir/usr/bin/$b" --version >/dev/null; done
appimage=$(ls illogical_"$v"_*.AppImage)
rm -f "$appimage"
plugin=$(ls /root/.cache/tauri/linuxdeploy-plugin-appimage*.AppImage | head -1)
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$arch OUTPUT=$appimage "$plugin" --appdir illogical.AppDir >/dev/null
mkdir -p /dist
name=illogical-desktop-linux-$arch
cp "$out/deb/illogical_${v}_$deb.deb" "/dist/$name.deb"
cp "$out/rpm/illogical-$v-1.$arch.rpm" "/dist/$name.rpm"
cp "$out/appimage/$appimage" "/dist/$name.AppImage"
# The sidecars as the app will run them: they must start.
x=$(mktemp -d)
(cd "$x" && "/dist/$name.AppImage" --appimage-extract >/dev/null \
  && for b in illogicald illogical; do squashfs-root/usr/bin/$b --version >/dev/null || { echo "the AppImage's $b doesn't run" >&2; exit 1; }; done)
rm -rf "$x"
if [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  cargo tauri signer sign "/dist/$name.AppImage" >/dev/null
  echo "signed /dist/$name.AppImage for the updater"
fi
ls -la /dist/"$name".*
