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

# The macOS daemon and CLI for Intel Macs, cross-compiled on Apple silicon
# (or built natively on an Intel Mac): target/x86_64-apple-darwin/release/.
# Then `just dist` and `just desktop x86_64`.
build-macos-x86_64: web
    rustup target add x86_64-apple-darwin >/dev/null
    {{cargo}} build --release --target x86_64-apple-darwin -p illogicald -p illogical

# Release tarballs in dist/: illogical-VERSION-TARGET.tar.gz with both
# binaries and the licenses, for the targets already built (`just static`,
# `just static aarch64`, `just build` and `just build-macos-x86_64` on a Mac).
dist:
    #!/usr/bin/env bash
    set -euo pipefail
    v=$({{cargo}} pkgid -p illogicald | sed 's/.*[#@]//')
    host=$(rustc -vV | sed -n 's/^host: //p')
    mkdir -p dist
    for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin x86_64-apple-darwin; do
      d={{target_dir}}/$t/release
      # The Mac's own architecture builds without --target (`just build`).
      if [ "$t" = "$host" ] && [ -x {{target_dir}}/release/illogicald ]; then d={{target_dir}}/release; fi
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
# latest release. Linux: `just desktop-linux ARCH` (x86_64 by default).
# macOS: `just desktop-macos ARCH`, this Mac's own arch by default;
# `just desktop x86_64` after `just build-macos-x86_64` makes the Intel app.
desktop arch="":
    #!/usr/bin/env bash
    set -euo pipefail
    a="{{arch}}"
    case "$(uname -s)" in
      Linux) just desktop-linux "${a:-x86_64}" ;;
      Darwin) just desktop-macos "$a" ;;
    esac

# The Linux desktop app for ARCH (x86_64 or aarch64): a .deb, an .rpm and
# an AppImage, after `just static ARCH`. It builds in an Ubuntu 22.04
# container (podman or docker, packaging/desktop/Containerfile) so it runs
# on glibc 2.35 and newer, and fails if anything in the bundles needs more
# (#170). Another architecture than the host's runs under emulation
# (qemu-user binfmt on the host).
desktop-linux arch="x86_64" *tauri_args="":
    #!/usr/bin/env bash
    set -euo pipefail
    # An unset GitHub secret arrives as "": treat it as unset, or Tauri
    # takes APPLE_CERTIFICATE="" for a certificate to import.
    for v in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD; do
      [ -n "${!v:-}" ] || unset "$v"
    done
    root={{justfile_directory()}}
    dist=$root/dist
    mkdir -p "$dist"
    # The oldest glibc the app runs on: Ubuntu 22.04's, the container's.
    floor=2.35
    arch={{arch}}
    case "$arch" in x86_64) platform=linux/amd64 ;; aarch64) platform=linux/arm64 ;; *) echo "arch: x86_64 or aarch64" >&2; exit 2 ;; esac
    src={{target_dir}}/$arch-unknown-linux-musl/release
    mkdir -p crates/desktop/binaries
    for b in illogicald illogical; do install -m 755 "$src/$b" "crates/desktop/binaries/$b-$arch-unknown-linux-gnu"; done
    engine=$(command -v podman || command -v docker) || { echo "the Linux desktop build needs podman or docker" >&2; exit 1; }
    toolchain=$(sed -n 's/^channel = "\(.*\)"/\1/p' crates/desktop/rust-toolchain.toml)
    image=illogical-desktop-build:jammy-$toolchain-$arch
    "$engine" build -q --platform "$platform" -t "$image" -f packaging/desktop/Containerfile --build-arg RUST_TOOLCHAIN="$toolchain" --build-arg TAURI_CLI=2.12.1 packaging/desktop
    # Its own target dir: build scripts built against 22.04's glibc
    # don't mix with the host's.
    target={{target_dir}}/desktop-jammy-$arch
    mkdir -p "$target"
    # Docker Desktop's file sharing (a Mac) refuses linuxdeploy's copies
    # into a shared directory: build in a volume there.
    [ "$(uname -s)" = Darwin ] && target=illogical-desktop-target-$arch
    "$engine" run --rm --platform "$platform" --security-opt label=disable \
      -v "$root:/src" -v "$target:/target" -v "$dist:/dist" \
      -v illogical-desktop-cargo-$arch:/opt/cargo/registry -v illogical-desktop-tauri-$arch:/root/.cache/tauri \
      -e CARGO_TARGET_DIR=/target -e TAURI_SIGNING_PRIVATE_KEY -e TAURI_SIGNING_PRIVATE_KEY_PASSWORD \
      "$image" /src/packaging/desktop/build-linux.sh {{tauri_args}}
    name=$dist/illogical-desktop-linux-$arch
    if command -v dpkg-deb >/dev/null; then
      scripts/glibc-floor "$floor" "$name.deb" "$name.AppImage"
      dpkg-deb -f "$name.deb" Depends | grep -q "libc6 (>= $floor)" \
        || { echo "the .deb should depend on libc6 (>= $floor): crates/desktop/tauri.conf.json" >&2; exit 1; }
    else
      echo "no dpkg-deb here: skipped the glibc floor check" >&2
    fi
    ls -la "$name".*

# The Linux packages from `just desktop-linux ARCH` install on a fresh
# Ubuntu 22.04 (.deb) and Fedora (.rpm) and claim illogical:// links
# (packaging/desktop/packages.sh).
desktop-packages arch="x86_64":
    packaging/desktop/packages.sh {{arch}}

# The macOS app for ARCH (aarch64 or x86_64; this Mac's own by default),
# after `just build` (or `just build-macos-x86_64` for the Intel app,
# cross-compiled on Apple silicon): the app, a zip of it and a .dmg in
# dist/, named macos-arm64 or macos-x86_64. scripts/macos-sign signs,
# notarizes and staples them when the Developer ID secrets are set (#177)
# and says which is missing when not; the app is ad-hoc signed either way.
# With TAURI_SIGNING_PRIVATE_KEY set, also the updater's .app.tar.gz and
# its signature.
desktop-macos arch="" *tauri_args="":
    #!/usr/bin/env bash
    set -euo pipefail
    # An unset GitHub secret arrives as "": treat it as unset, or Tauri
    # takes APPLE_CERTIFICATE="" for a certificate to import.
    for v in APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD; do
      [ -n "${!v:-}" ] || unset "$v"
    done
    root={{justfile_directory()}}
    dist=$root/dist
    mkdir -p "$dist"
    command -v cargo-tauri >/dev/null || cargo install tauri-cli --version "^2" --locked
    cd crates/desktop
    host=$(rustc -vV | sed -n 's/^host: //p')
    a="{{arch}}"; t=${a:-${host%%-*}}-apple-darwin
    case "$t" in
      aarch64-apple-darwin) name=macos-arm64 ;;
      x86_64-apple-darwin) name=macos-x86_64 ;;
      *) echo "no macOS app for $t (arch: aarch64 or x86_64)" >&2; exit 2 ;;
    esac
    # The Mac's own arch builds without --target (`just build`).
    src={{target_dir}}/$t/release
    if [ "$t" = "$host" ] && [ -x {{target_dir}}/release/illogicald ]; then src={{target_dir}}/release; fi
    # A test's own daemon and CLI (testnet/macos/update.sh's older ones).
    src=${ILLOGICAL_DESKTOP_BINARIES:-$src}
    out=${CARGO_TARGET_DIR:-$PWD/target}
    flags=()
    if [ "$t" = "$host" ]; then out=$out/release; else
      rustup target add "$t" >/dev/null
      flags=(--target "$t"); out=$out/$t/release
    fi
    mkdir -p binaries
    for b in illogicald illogical; do install -m 755 "$src/$b" "binaries/$b-$t"; done
    # ${flags[@]+…}: macOS bash 3.2 calls an empty array unbound.
    cargo tauri build --bundles app ${flags[@]+"${flags[@]}"} {{tauri_args}}
    app=$out/bundle/macos/illogical.app
    "$root/scripts/macos-sign" app "$app"
    # A zip of the app: ditto keeps its signature and symlinks.
    # --norsrc: no ._* AppleDouble files for xattrs like
    # com.apple.provenance, which a command-line unzip leaves in the
    # bundle (#177). The signature lives in the bundle, not in xattrs.
    zip=$dist/illogical-desktop-$name.zip
    rm -f "$zip"
    ditto -c -k --norsrc --keepParent "$app" "$zip"
    if zipinfo -1 "$zip" | grep -E '(^|/)\._'; then echo "AppleDouble files in $zip" >&2; exit 1; fi
    # The .dmg: the app beside a link to /Applications.
    stage=$(mktemp -d)
    ditto "$app" "$stage/illogical.app"
    ln -s /Applications "$stage/Applications"
    dmg=$dist/illogical-desktop-$name.dmg
    rm -f "$dmg"
    hdiutil create -quiet -volname illogical -srcfolder "$stage" -fs HFS+ -format UDZO "$dmg"
    rm -rf "$stage"
    "$root/scripts/macos-sign" dmg "$dmg"
    # The updater's archive of the app, signed with the updater key.
    if [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
      tgz=$dist/illogical-desktop-$name.app.tar.gz
      tar -C "$(dirname "$app")" -czf "$tgz" illogical.app
      cargo tauri signer sign "$tgz" >/dev/null
      echo "signed $tgz for the updater"
    else
      echo "no updater archive: TAURI_SIGNING_PRIVATE_KEY isn't set"
    fi
    ls -la "$dist"/illogical-desktop-"$name".*

# The Linux desktop app under Xvfb, in a container: builds the app (debug,
# no bundle) in its build image (packaging/desktop/Containerfile) and runs
# packaging/desktop/xvfb's tests against a static daemon: `join` (#204,
# test.sh, with a stand-in control), `m46` (m46.sh: keys, the titlebar,
# illogical:// links, the global hotkey) and `m47` (a right-click in
# Nautilus opens a tab). Needs podman or docker; the
# container runs the host's architecture (aarch64 under Docker Desktop on a
# Mac). `just desktop-xvfb m46 keys` runs one claim.
desktop-xvfb *tests="join m46 m47":
    #!/usr/bin/env bash
    set -euo pipefail
    root={{justfile_directory()}}
    engine=$(command -v podman || command -v docker) || { echo "the desktop test needs podman or docker" >&2; exit 1; }
    arch=$(uname -m); [ "$arch" = arm64 ] && arch=aarch64
    just static "$arch"
    host=$arch-unknown-linux-gnu
    mkdir -p crates/desktop/binaries
    for b in illogicald illogical; do install -m 755 "{{target_dir}}/$arch-unknown-linux-musl/release/$b" "crates/desktop/binaries/$b-$host"; done
    toolchain=$(sed -n 's/^channel = "\(.*\)"/\1/p' crates/desktop/rust-toolchain.toml)
    base=illogical-desktop-build:jammy-$toolchain
    "$engine" build -q -t "$base" -f packaging/desktop/Containerfile --build-arg RUST_TOOLCHAIN="$toolchain" --build-arg TAURI_CLI=2.12.1 packaging/desktop
    "$engine" build -q -t illogical-desktop-xvfb:jammy-$toolchain -f packaging/desktop/xvfb/Containerfile --build-arg BASE="$base" packaging/desktop/xvfb
    target={{target_dir}}/desktop-xvfb-$arch
    mkdir -p "$target"
    "$engine" run --rm --security-opt label=disable \
      -v "$root:/src" -v "$target:/target" -v illogical-desktop-cargo:/opt/cargo/registry \
      -e CARGO_TARGET_DIR=/target -w /src/crates/desktop \
      illogical-desktop-xvfb:jammy-$toolchain bash -c 'set -euo pipefail
        cargo tauri build --debug --no-bundle
        APP=/target/debug/illogical-desktop BIN=/src/crates/desktop/binaries HOST='"$host"' \
          /src/packaging/desktop/xvfb/run.sh {{tests}}'

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
# Needs cargo-about 0.9.2; leaves both files alone when anything fails.
notices:
    scripts/notices

# All tests. The Rust ones run under cargo-nextest (.config/nextest.toml),
# which `just bootstrap` installs; the doctests, which it can't run, under
# cargo test.
test: web
    {{cargo}} nextest run --workspace
    {{cargo}} test --workspace --doc
    cd web && pnpm run typecheck
    just e2e-interop control-smoke

# Control end to end without a browser: sign in (fake GitHub), enroll,
# join a daemon, reach it through the relay and directly.
control-smoke:
    {{cargo}} build -p illogical-control -p illogicald -p illogical
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
e2e url="": web e2e-build
    cd web && E2E_BASE_URL="{{url}}" pnpm exec playwright test

# Only the WebKit specs (`*.webkit.spec.ts`), as the macOS runner runs them.
e2e-webkit: web e2e-build
    cd web && pnpm exec playwright test --project=webkit

# What the specs run, from ../target/debug: with CARGO_TARGET_DIR elsewhere
# (CI), target is a link to it.
[private]
e2e-build:
    #!/usr/bin/env bash
    set -euo pipefail
    {{cargo}} build -p illogicald -p illogical -p illogical-control
    t="{{target_dir}}"
    if [ "$t" != "{{justfile_directory()}}/target" ]; then
      if [ -L target ] || [ ! -e target ]; then ln -sfn "$t" target
      else echo "target/ is a directory but CARGO_TARGET_DIR is $t: the specs would run stale binaries" >&2; exit 1; fi
    fi

# Playwright's browsers (Chromium and WebKit by default). On Linux their
# system libraries come too when sudo needs no password; otherwise a spec
# that can't start its browser says which are missing.
browsers *which="chromium webkit":
    #!/usr/bin/env bash
    set -euo pipefail
    cd web
    if [ "$(uname -s)" = Linux ] && sudo -n true 2>/dev/null; then
      pnpm exec playwright install --with-deps {{which}}
    else
      pnpm exec playwright install {{which}}
    fi

# illogical's VS Code extension as a VSIX in target/ (M28), for Open VSX
# (`npx ovsx publish FILE`) and the Marketplace (`npx @vscode/vsce publish
# --packagePath FILE`).
vsix:
    {{cargo}} build -p illogicald
    {{target_dir}}/debug/illogicald _vsix {{target_dir}}

# The images in site/img/, from a throwaway daemon with a demo HOME and a
# scripted agent (web/screenshots/). Needs nvim for the editor pane.
# The web client first: the daemon build picks up web/dist.
screenshots:
    cd web && pnpm run build
    {{cargo}} build -p illogicald -p illogical
    cd web && pnpm exec playwright test -c screenshots.config.ts
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

# The local Docker test stack (testnet/README.md): up|test|break|measure|down [profile] [claim...].
# The control profile builds what it runs: the static binaries for Docker's
# architecture (unless ILLOGICAL_TESTNET_BINARIES names others) and the CLI.
testnet cmd="test" profile="ssh" *claims:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ {{profile}} = control ] && docker info >/dev/null 2>&1; then
      case "{{cmd}}" in
        up) arch=$(docker info --format '{{{{.Architecture}}')
            case "$arch" in arm64) arch=aarch64 ;; amd64) arch=x86_64 ;; esac
            [ -n "${ILLOGICAL_TESTNET_BINARIES:-}" ] || just static "$arch" >&2 ;;
        test|break) [ -n "${ILLOGICAL_CLI:-}" ] || {{cargo}} build -q -p illogical ;;
      esac
    fi
    case "{{cmd}}" in
      up) testnet/up.sh {{profile}} ;;
      test) testnet/test.sh {{profile}} {{claims}} ;;
      break) testnet/test.sh {{profile}} --break ;;
      measure) testnet/measure-{{profile}}.sh {{claims}} ;;
      down) testnet/down.sh ;;
      *) echo "usage: just testnet up|test|break|measure|down [profile] [claim...]" >&2; exit 2 ;;
    esac

# #17 on two Docker machines, one dropping off the network (testnet/hosts/README.md).
testnet-hosts:
    #!/usr/bin/env bash
    set -euo pipefail
    # No Docker: a failure, or with ILLOGICAL_SKIP_DOCKER=1 a loud skip.
    if ! docker info >/dev/null 2>&1; then testnet/hosts/net.sh check; exit $?; fi
    a=$(uname -m); [ "$a" = arm64 ] && a=aarch64
    just static "$a"
    {{cargo}} build -p illogicald
    cd web && ILLOGICAL_TESTNET_HOSTS=1 pnpm exec playwright test e2e/testnet-hosts.spec.ts

# M28 for real: VS Code over Remote-SSH into a Docker box (testnet/editors/README.md).
testnet-editors:
    #!/usr/bin/env bash
    set -euo pipefail
    # No Docker: a failure, or with ILLOGICAL_SKIP_DOCKER=1 a loud skip.
    if ! docker info >/dev/null 2>&1; then testnet/editors/box.sh check; exit $?; fi
    a=$(uname -m); [ "$a" = arm64 ] && a=aarch64
    just static "$a"
    {{cargo}} build -p illogicald
    cd web
    # Electron needs a display: a virtual one where there's none (Linux CI).
    x=(); if [ "$(uname -s)" = Linux ] && [ -z "${DISPLAY:-}" ]; then x=(xvfb-run -a); fi
    ILLOGICAL_TESTNET_EDITORS=1 ${x[@]+"${x[@]}"} pnpm exec playwright test e2e/editor-remote-ssh.spec.ts

# Real Forgejo and GitLab in Docker (testnet/forges/README.md): up|test|down [forgejo|gitlab|all].
forges cmd="test" forge="forgejo":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{cmd}}" in
      up) testnet/forges/up.sh {{forge}} ;;
      test) testnet/forges/test.sh {{forge}} ;;
      down) testnet/forges/down.sh {{forge}} ;;
      *) echo "usage: just forges up|test|down [forgejo|gitlab|all]" >&2; exit 2 ;;
    esac

# macOS checks in a throwaway tart VM (testnet/macos/README.md):
# `just macos launchd`, `just macos safari`, or base|up|down|ssh for the VM.
# Fails without tart or the base VM; ILLOGICAL_SKIP_MACOS_VM=1 skips loudly.
macos cmd="launchd" *args:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{cmd}}" in
      base|up|down|ssh|ip|push|restart) exec testnet/macos/vm.sh {{cmd}} {{args}} ;;
    esac
    {{cargo}} build -p illogicald -p illogical -p illogical-control
    exec testnet/macos/test.sh {{cmd}} {{args}}

# Tests for the shell side of releases: install.sh picks the right release
# per machine, and the ratchet that what a release ships is named the same
# everywhere (release.yml, scripts/release, Homebrew, install.sh, the site);
# and `just notices` keeping THIRD_PARTY.md when cargo-about fails.
test-scripts:
    scripts/tests/install.sh
    scripts/tests/release-targets.sh
    scripts/tests/notices.sh

# What CI runs.
check: test
    {{cargo}} fmt --all --check
    {{cargo}} clippy --workspace --all-targets -- -D warnings

# Type-check and lint the macOS build from Linux: ARCH is aarch64 (Apple
# silicon) or x86_64 (Intel). Zig is the C compiler; this compiles but
# doesn't link, so build and test on a Mac too.
check-macos arch="aarch64":
    rustup target add {{arch}}-apple-darwin >/dev/null
    ZIG_MACOS_ARCH={{arch}} CC_{{arch}}_apple_darwin="$PWD/scripts/zig-cc-macos" AR_{{arch}}_apple_darwin="$PWD/scripts/zig-ar" \
      {{cargo}} clippy --target {{arch}}-apple-darwin --workspace --all-targets -- -D warnings

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
