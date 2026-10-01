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

# All tests.
test:
    {{cargo}} test --workspace
    cd web && pnpm run typecheck

# Browser tests in system Chrome; pass a URL to test a running daemon.
e2e url="":
    {{cargo}} build -p illogicald
    cd web && pnpm run build && E2E_BASE_URL="{{url}}" pnpm exec playwright test

# What CI runs.
check: test
    {{cargo}} fmt --all --check
    {{cargo}} clippy --workspace --all-targets -- -D warnings

# Run the daemon the way it runs for real (port 7681, behind `tailscale serve`).
run *args: build
    ./target/release/illogicald {{args}}

# Dev loop: separate daemon on 7682 + Vite on 5173; leaves the real one alone.
dev:
    {{cargo}} build -p illogicald
    trap 'kill 0' EXIT; \
      ./target/debug/illogicald --listen 127.0.0.1:7682 --allow-origin http://localhost:5173 & \
      (cd web && pnpm run dev)

# Re-record snapshot fixtures (crates/vt/fixtures).
fixtures *names:
    python3 crates/vt/fixtures/record.py {{names}}
