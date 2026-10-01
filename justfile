# illogical tasks. Cargo runs under mise so libghostty-vt-sys finds the Zig
# it needs (.mise.toml).

set shell := ["bash", "-euo", "pipefail", "-c"]

# rustup's default location, for shells that haven't sourced ~/.cargo/env.
export PATH := env("HOME") / ".cargo/bin" + ":" + env("PATH")

cargo := "mise exec -- cargo"

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

# Static x86_64 musl binaries (daemon and CLI) for sandboxes and machines
# without systemd: target/x86_64-unknown-linux-musl/release/. Zig, already
# here for libghostty, is the C compiler and brings musl.
static: web
    rustup target add x86_64-unknown-linux-musl >/dev/null
    CC_x86_64_unknown_linux_musl="$PWD/scripts/zig-cc-musl" AR_x86_64_unknown_linux_musl="$PWD/scripts/zig-ar" \
      {{cargo}} build --release --target x86_64-unknown-linux-musl -p illogicald -p illogical
    file target/x86_64-unknown-linux-musl/release/illogicald target/x86_64-unknown-linux-musl/release/illogical

# All tests.
test: web
    {{cargo}} test --workspace
    cd web && pnpm run typecheck

# Browser tests in system Chrome; pass a URL to test a running daemon.
e2e url="":
    {{cargo}} build -p illogicald
    cd web && pnpm run build && E2E_BASE_URL="{{url}}" pnpm exec playwright test

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

# Run the daemon the way it runs for real (port 7681, behind `tailscale serve`).
run *args: build
    ./target/release/illogicald {{args}}

# Install as a systemd user service (starts at boot with lingering).
install: build
    ./target/release/illogicald install

# Dev loop: separate daemon on 7682 + Vite on 5173; leaves the real one alone.
dev:
    {{cargo}} build -p illogicald
    trap 'kill 0' EXIT; \
      ./target/debug/illogicald --listen 127.0.0.1:7682 --allow-origin http://localhost:5173 --state-dir ~/.local/state/illogical-dev & \
      (cd web && pnpm run dev)

# Re-record snapshot fixtures (crates/vt/fixtures).
fixtures *names:
    python3 crates/vt/fixtures/record.py {{names}}
