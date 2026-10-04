# illogical tasks. Cargo runs under mise so libghostty-vt-sys finds the Zig
# it needs (.mise.toml).

set shell := ["bash", "-euo", "pipefail", "-c"]

# rustup's default location, for shells that haven't sourced ~/.cargo/env.
export PATH := env("HOME") / ".cargo/bin" + ":" + env("PATH")

cargo := "mise exec -- cargo"

# Where cargo builds (CI keeps one per runner, outside the checkout).
target_dir := env("CARGO_TARGET_DIR", justfile_directory() / "target")

default:
    @just --list

# Install toolchains (Zig via mise) and web dependencies.
bootstrap:
    mise install
    cd web && pnpm install --frozen-lockfile

# Build the web client into web/dist (embedded into the daemon).
web:
    cd web && pnpm run build

# Release build of everything.
build: web
    {{cargo}} build --release

# Static musl binaries (daemon, CLI and illogical-control) for sandboxes,
# machines without systemd, hosting control and releases:
# target/ARCH-unknown-linux-musl/release/. ARCH is
# x86_64 or aarch64. Zig, already here for libghostty, is the C compiler and
# brings musl; for aarch64 it links too.
static arch="x86_64": web
    #!/usr/bin/env bash
    set -euo pipefail
    t={{arch}}-unknown-linux-musl; T=$(echo "$t" | tr a-z- A-Z_)
    rustup target add "$t" >/dev/null
    export ZIG_MUSL_ARCH={{arch}} "CC_${t//-/_}=$PWD/scripts/zig-cc-musl" "AR_${t//-/_}=$PWD/scripts/zig-ar"
    # Cross: Zig links too, with its own musl and startup files, not rustc's.
    if [ {{arch}} != "$(uname -m)" ]; then export "CARGO_TARGET_${T}_LINKER=$PWD/scripts/zig-cc-musl" "CARGO_TARGET_${T}_RUSTFLAGS=-C link-self-contained=no"; fi
    {{cargo}} build --release --target "$t" -p illogicald -p illogical -p illogical-control
    file {{target_dir}}/$t/release/illogicald {{target_dir}}/$t/release/illogical {{target_dir}}/$t/release/illogical-control

# Deploy the hosted illogical control to Fly (packaging/control/fly.toml):
# the static x86_64 binary in a distroless image, from a small build context.
control-deploy: static
    #!/usr/bin/env bash
    set -euo pipefail
    ctx=$(mktemp -d)
    trap 'rm -rf "$ctx"' EXIT
    cp {{target_dir}}/x86_64-unknown-linux-musl/release/illogical-control {{target_dir}}/x86_64-unknown-linux-musl/release/illogicald packaging/control/Dockerfile packaging/control/fly.toml "$ctx"/
    cd "$ctx" && fly deploy --local-only --ha=false

# Release tarballs in dist/: illogical-VERSION-TARGET.tar.gz with both
# binaries and the licenses, for the targets already built (`just static`,
# `just static aarch64`, `just build` on a Mac).
dist:
    #!/usr/bin/env bash
    set -euo pipefail
    v=$({{cargo}} pkgid -p illogicald | sed 's/.*[#@]//')
    mkdir -p dist
    for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin; do
      d={{target_dir}}/$t/release
      if [ "$t" = aarch64-apple-darwin ] && [ "$(uname -s)" = Darwin ]; then d={{target_dir}}/release; fi
      [ -x "$d/illogicald" ] || continue
      n=illogical-$v-$t; s=$(mktemp -d)/$n; mkdir -p "$s"
      cp "$d/illogicald" "$d/illogical" LICENSE-MIT LICENSE-APACHE THIRD_PARTY.md README.md "$s/"
      tar -C "$(dirname "$s")" -czf "dist/$n.tar.gz" "$n"
      echo "dist/$n.tar.gz"
    done
    (cd dist && (sha256sum *.tar.gz 2>/dev/null || shasum -a 256 *.tar.gz) > SHA256SUMS)

# The desktop app (crates/desktop, M46), carrying this build's illogicald
# and illogical: run after `just static x86_64` (Linux; the static binaries
# run anywhere) or `just build` (macOS). Writes dist/illogical-desktop-*,
# named without the version so the site's download links always find the
# latest release.
# On Linux the app builds in an Ubuntu 22.04 container (podman or docker,
# packaging/desktop/Containerfile) so it runs on glibc 2.35 and newer, and
# the build fails if anything in the bundles needs more (#170).
desktop:
    #!/usr/bin/env bash
    set -euo pipefail
    root={{justfile_directory()}}
    dist=$root/dist
    mkdir -p "$dist"
    v=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/desktop/Cargo.toml | head -1)
    case "$(uname -s)" in
      Linux)
        # The oldest glibc the app runs on: Ubuntu 22.04's, the container's.
        floor=2.35
        src={{target_dir}}/x86_64-unknown-linux-musl/release
        host=x86_64-unknown-linux-gnu
        mkdir -p crates/desktop/binaries
        for b in illogicald illogical; do install -m 755 "$src/$b" "crates/desktop/binaries/$b-$host"; done
        engine=$(command -v podman || command -v docker) || { echo "the Linux desktop build needs podman or docker" >&2; exit 1; }
        toolchain=$(sed -n 's/^channel = "\(.*\)"/\1/p' crates/desktop/rust-toolchain.toml)
        image=illogical-desktop-build:jammy-$toolchain
        "$engine" build -q -t "$image" --build-arg RUST_TOOLCHAIN="$toolchain" --build-arg TAURI_CLI=2.12.1 packaging/desktop
        # Its own target dir: build scripts built against 22.04's glibc
        # don't mix with the host's.
        target={{target_dir}}/desktop-jammy
        mkdir -p "$target"
        "$engine" run --rm --security-opt label=disable \
          -v "$root:/src" -v "$target:/target" -v illogical-desktop-cargo:/opt/cargo/registry -v illogical-desktop-tauri:/root/.cache/tauri \
          -e CARGO_TARGET_DIR=/target -w /src/crates/desktop \
          "$image" cargo tauri build --bundles deb,appimage
        out=$target/release/bundle
        # This version's bundles: a kept target dir (CI) holds older ones too.
        cp "$out/deb/illogical_${v}_amd64.deb" "$dist/illogical-desktop-linux-x86_64.deb"
        cp "$out/appimage/illogical_${v}_amd64.AppImage" "$dist/illogical-desktop-linux-x86_64.AppImage"
        scripts/glibc-floor "$floor" "$dist/illogical-desktop-linux-x86_64.deb" "$dist/illogical-desktop-linux-x86_64.AppImage"
        dpkg-deb -f "$dist/illogical-desktop-linux-x86_64.deb" Depends | grep -q "libc6 (>= $floor)" \
          || { echo "the .deb should depend on libc6 (>= $floor): crates/desktop/tauri.conf.json" >&2; exit 1; } ;;
      Darwin)
        command -v cargo-tauri >/dev/null || cargo install tauri-cli --version "^2" --locked
        cd crates/desktop
        host=$(rustc -vV | sed -n 's/^host: //p')
        mkdir -p binaries
        for b in illogicald illogical; do install -m 755 "{{target_dir}}/release/$b" "binaries/$b-$host"; done
        cargo tauri build --bundles app
        out=${CARGO_TARGET_DIR:-$PWD/target}/release/bundle
        # A zip of the app: ditto keeps its signature and symlinks.
        # --norsrc: no ._* AppleDouble files for xattrs like
        # com.apple.provenance, which a command-line unzip leaves in the
        # bundle (#177). The signature lives in the bundle, not in xattrs.
        zip=$dist/illogical-desktop-macos-arm64.zip
        rm -f "$zip"
        ditto -c -k --norsrc --keepParent "$out/macos/illogical.app" "$zip"
        if zipinfo -1 "$zip" | grep -E '(^|/)\._'; then echo "AppleDouble files in $zip" >&2; exit 1; fi ;;
    esac
    ls -la "$dist"/illogical-desktop-*

# Lint the desktop app (its own workspace) without building its sidecars.
desktop-check:
    #!/usr/bin/env bash
    set -euo pipefail
    cd crates/desktop
    host=$(rustc -vV | sed -n 's/^host: //p')
    mkdir -p binaries
    # tauri-build wants the sidecars to exist; empty stand-ins do for a lint.
    for b in illogicald illogical; do [ -e "binaries/$b-$host" ] || : > "binaries/$b-$host"; done
    cargo fmt --check
    cargo clippy -- -D warnings

# THIRD_PARTY.md: notices for the Rust crates (cargo-about) and the npm
# packages bundled into the web client; crates/desktop/THIRD_PARTY.md for
# the desktop app's own crates (its about.toml also accepts MPL-2.0).
notices:
    cargo about generate about.hbs > THIRD_PARTY.md
    scripts/web-notices >> THIRD_PARTY.md
    cd crates/desktop && cargo about generate -c about.toml ../../about.hbs > THIRD_PARTY.md

# All tests.
test: web
    {{cargo}} test --workspace
    cd web && pnpm run typecheck
    just e2e-interop control-smoke

# Control end to end without a browser: sign in (fake GitHub), enroll,
# join a daemon, reach it through the relay and directly.
control-smoke:
    {{cargo}} build -p illogical-control -p illogicald
    cd web && TARGET_DIR="{{target_dir}}/debug" node --experimental-strip-types --no-warnings control-smoke.ts

# The swarm (M26) by hand: three throwaway daemons with scripted work on
# 7730-7732 (t: make trouble, a: an agent asks, x: quit).
fake-fleet:
    {{cargo}} build -p illogicald -p illogical
    cd web && pnpm run build && node --experimental-strip-types --no-warnings fake-fleet.ts

# The browser's end-to-end crypto (web/src/e2e) against Rust's (crates/e2e).
e2e-interop:
    {{cargo}} build -p illogical-e2e --example interop
    cd web && INTEROP_BIN="{{target_dir}}/debug/examples/interop" node --experimental-strip-types --no-warnings e2e-interop.ts

# Browser tests in system Chrome; pass a URL to test a running daemon.
e2e url="":
    {{cargo}} build -p illogicald
    cd web && pnpm run build && E2E_BASE_URL="{{url}}" pnpm exec playwright test

# illogical's VS Code extension as a VSIX in target/ (M28), for Open VSX
# (`npx ovsx publish FILE`) and the Marketplace (`npx @vscode/vsce publish
# --packagePath FILE`).
vsix:
    {{cargo}} build -p illogicald
    {{target_dir}}/debug/illogicald _vsix {{target_dir}}

# The images in site/img/, from a throwaway daemon with a demo HOME and a
# scripted agent (web/screenshots/). Needs nvim for the editor pane.
screenshots:
    {{cargo}} build -p illogicald -p illogical
    cd web && pnpm run build && pnpm exec playwright test -c screenshots.config.ts
    scripts/webp

# The project page (site/) with install.sh beside it, in target/site.
site:
    rm -rf target/site && mkdir -p target/site
    cp -r site/. target/site/
    cp scripts/install.sh target/site/install.sh

# Publish the page (wrangler.jsonc: static assets on Cloudflare, at
# illogical.widgets.wtf). Uses wrangler's login, or CLOUDFLARE_API_TOKEN.
site-deploy: site
    pnpm dlx wrangler@4 deploy

# M4a for real: a wisp sprite installs the static daemon on the tailnet and
# joins a throwaway home daemon's list; the phone gets vim there. Needs
# ILLOGICAL_E2E_TAILNET_AUTHKEY_FILE (an ephemeral tag:sandbox key) and wispd.
e2e-sandbox: static
    {{cargo}} build -p illogicald
    cd web && pnpm exec playwright test e2e/sandbox.spec.ts

# What CI runs.
check: test
    {{cargo}} fmt --all --check
    {{cargo}} clippy --workspace --all-targets -- -D warnings

# Type-check and lint the macOS (Apple silicon) build from Linux. Zig is the
# C compiler; this compiles but doesn't link, so build and test on a Mac too.
check-macos:
    rustup target add aarch64-apple-darwin >/dev/null
    CC_aarch64_apple_darwin="$PWD/scripts/zig-cc-macos" AR_aarch64_apple_darwin="$PWD/scripts/zig-ar" \
      {{cargo}} clippy --target aarch64-apple-darwin --workspace --all-targets -- -D warnings

# Run the daemon the way it runs for real (port 7681, behind `tailscale serve`).
run *args: build
    {{target_dir}}/release/illogicald {{args}}

# Install as a systemd user service (starts at boot with lingering).
install: build
    {{target_dir}}/release/illogicald install

# Dev loop: separate daemon on 7682 + Vite on 5173; leaves the real one alone.
dev:
    {{cargo}} build -p illogicald
    trap 'kill 0' EXIT; \
      {{target_dir}}/debug/illogicald --listen 127.0.0.1:7682 --allow-origin http://localhost:5173 --state-dir ~/.local/state/illogical-dev & \
      (cd web && pnpm run dev)

# Re-record snapshot fixtures (crates/vt/fixtures).
fixtures *names:
    python3 crates/vt/fixtures/record.py {{names}}
