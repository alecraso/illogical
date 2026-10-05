#!/usr/bin/env bash
# Tests scripts/notices with a stand-in cargo: without cargo-about it must
# leave both THIRD_PARTY.md files as they were and say how to install it;
# when a step fails partway it must also leave them; on success it replaces
# them.
#
#   scripts/tests/notices.sh
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail=0
bad() { printf 'FAIL  %s\n' "$*"; fail=1; }

# A copy of the tree's shape: the script, a stand-in web-notices, and old
# notices to keep or replace.
tree=$work/tree
mkdir -p "$tree/scripts" "$tree/crates/desktop"
cp "$root/scripts/notices" "$tree/scripts/notices"
printf '#!/bin/sh\necho web-notices\n' >"$tree/scripts/web-notices"
chmod +x "$tree/scripts/web-notices"
reset() {
  echo old-root >"$tree/THIRD_PARTY.md"
  echo old-desktop >"$tree/crates/desktop/THIRD_PARTY.md"
}

# Stand-in cargo. FAKE_ABOUT: missing (no such subcommand), fail (generate
# fails), or ok.
stubs=$work/stubs
mkdir -p "$stubs"
cat >"$stubs/cargo" <<'STUB'
#!/bin/sh
[ "$1" = about ] || exit 101
case "$FAKE_ABOUT" in
  missing) echo "error: no such command: \`about\`" >&2; exit 101 ;;
  fail) [ "$2" = --version ] && { echo "cargo-about 0.9.2"; exit 0; }; exit 1 ;;
  ok) [ "$2" = --version ] && { echo "cargo-about 0.9.2"; exit 0; }; echo "generated $PWD"; exit 0 ;;
esac
STUB
chmod +x "$stubs/cargo"

run() { FAKE_ABOUT=$1 PATH="$stubs:$PATH" "$tree/scripts/notices" 2>"$work/err"; }

for mode in missing fail; do
  reset
  if run "$mode"; then bad "$mode: notices succeeded"; fi
  [ "$(cat "$tree/THIRD_PARTY.md")" = old-root ] || bad "$mode: THIRD_PARTY.md changed"
  [ "$(cat "$tree/crates/desktop/THIRD_PARTY.md")" = old-desktop ] || bad "$mode: desktop THIRD_PARTY.md changed"
  [ ! -e "$tree/THIRD_PARTY.md.tmp" ] || bad "$mode: left THIRD_PARTY.md.tmp"
  [ ! -e "$tree/crates/desktop/THIRD_PARTY.md.tmp" ] || bad "$mode: left desktop THIRD_PARTY.md.tmp"
done
reset
run missing || true
grep -q 'cargo install cargo-about --version 0.9.2' "$work/err" || bad "missing: no install hint: $(cat "$work/err")"

reset
run ok || bad "ok: notices failed: $(cat "$work/err")"
grep -q '^web-notices$' "$tree/THIRD_PARTY.md" || bad "ok: THIRD_PARTY.md lacks the web notices"
grep -q '^generated ' "$tree/THIRD_PARTY.md" || bad "ok: THIRD_PARTY.md lacks the crate notices"
grep -q "^generated .*crates/desktop$" "$tree/crates/desktop/THIRD_PARTY.md" || bad "ok: desktop THIRD_PARTY.md not generated in crates/desktop"

[ "$fail" = 0 ] && echo "notices: ok"
exit "$fail"
