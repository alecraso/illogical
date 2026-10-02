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

# Static musl binaries (daemon and CLI) for sandboxes, machines without
# systemd and releases: target/ARCH-unknown-linux-musl/release/. ARCH is
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
    {{cargo}} build --release --target "$t" -p illogicald -p illogical
    file {{target_dir}}/$t/release/illogicald {{target_dir}}/$t/release/illogical

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

# THIRD_PARTY.md: notices for the Rust crates (cargo-about) and the npm
# packages bundled into the web client.
notices:
    cargo about generate about.hbs > THIRD_PARTY.md
    scripts/web-notices >> THIRD_PARTY.md

# All tests.
test: web
    {{cargo}} test --workspace
    cd web && pnpm run typecheck

# Browser tests in system Chrome; pass a URL to test a running daemon.
e2e url="":
    {{cargo}} build -p illogicald
    cd web && pnpm run build && E2E_BASE_URL="{{url}}" pnpm exec playwright test

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
