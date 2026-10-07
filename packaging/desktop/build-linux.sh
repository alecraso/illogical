#!/usr/bin/env bash
# The Linux desktop bundles, inside the build container (`just
# desktop-linux ARCH`, packaging/desktop/Containerfile): a .deb, an .rpm
# and an AppImage for this container's architecture, carrying the static
# sidecars in crates/desktop/binaries. Writes them to /dist as
# arugula-desktop-linux-ARCH.{deb,rpm,AppImage}.
#
# TAURI_SIGNING_PRIVATE_KEY (and _PASSWORD), when set, also sign the
# AppImage for the updater (arugula-desktop-linux-ARCH.AppImage.sig).
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
# The bundles are named after the app's productName.
product=$(sed -n 's/^ *"productName": "\(.*\)",$/\1/p' tauri.conf.json)
for b in arugulad arugula; do
  [ -s "binaries/$b-$host" ] || { echo "missing binaries/$b-$host (just static $arch)" >&2; exit 1; }
done
# An AppImage marks itself with "AI\2" in the ELF header's padding, which
# the binfmt_misc entries for emulation (qemu-user, Docker Desktop's
# Rosetta) require to be zero: under emulation the kernel refuses to run
# linuxdeploy's plugins ("Exec format error"). Clear the mark on the tools
# tauri caches; the AppImage runtime doesn't need it. A first build
# downloads them mid-way, so it runs again once they're there.
runnable() {
  for f in "$@"; do
    [ "$(dd if="$f" bs=1 skip=8 count=3 2>/dev/null | od -An -c | tr -d ' ')" = 'AI002' ] || continue
    printf '\0\0\0' | dd of="$f" bs=1 seek=8 count=3 conv=notrunc 2>/dev/null
  done
}
tools() { runnable /root/.cache/tauri/*.AppImage; }
tools 2>/dev/null || true
# Huddles run in Rust here (M63): Opus linked in, not a library to install.
export LIBOPUS_STATIC=1 LIBOPUS_NO_PKG=1 LIBOPUS_LIB_DIR=/opt/opus
build() { cargo tauri build --bundles deb,rpm,appimage --features native-calls "$@"; }
build "$@" || { tools; build "$@"; }
out=$CARGO_TARGET_DIR/release/bundle
# linuxdeploy patches an RPATH into every ELF in usr/bin, which breaks the
# static-pie sidecars (they segfault at start, so the app could never
# install its daemon). Put the originals back and pack the AppImage again
# with the same plugin.
cd "$out/appimage"
for b in arugulad arugula; do install -m 755 "/src/crates/desktop/binaries/$b-$host" "$product.AppDir/usr/bin/$b"; done
for b in arugulad arugula; do "$product.AppDir/usr/bin/$b" --version >/dev/null; done
appimage=$(ls "$product"_"$v"_*.AppImage)
rm -f "$appimage"
plugin=$(ls /root/.cache/tauri/linuxdeploy-plugin-appimage*.AppImage | head -1)
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$arch OUTPUT=$appimage "$plugin" --appdir "$product.AppDir" >/dev/null
mkdir -p /dist
name=arugula-desktop-linux-$arch
cp "$out/deb/${product}_${v}_$deb.deb" "/dist/$name.deb"
cp "$out/rpm/$product-$v-1.$arch.rpm" "/dist/$name.rpm"
cp "$out/appimage/$appimage" "/dist/$name.AppImage"
# The sidecars as the app will run them: they must start (from a copy
# without the mark, which the shipped AppImage keeps).
x=$(mktemp -d)
cp "/dist/$name.AppImage" "$x/app.AppImage"
runnable "$x/app.AppImage"
(cd "$x" && ./app.AppImage --appimage-extract >/dev/null \
  && for b in arugulad arugula; do squashfs-root/usr/bin/$b --version >/dev/null || { echo "the AppImage's $b doesn't run" >&2; exit 1; }; done)
rm -rf "$x"
if [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
  cargo tauri signer sign "/dist/$name.AppImage" >/dev/null
  echo "signed /dist/$name.AppImage for the updater"
fi
ls -la /dist/"$name".*
