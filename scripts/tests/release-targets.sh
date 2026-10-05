#!/usr/bin/env bash
# A ratchet on what a release ships. The targets `just dist` packs and the
# downloads `just desktop` makes are named in several other places: how
# release.yml builds them, how many scripts/release expects, the Homebrew
# formula, install.sh and the download links. Adding (or dropping) one in
# some of those places and not the others fails here, not at release time.
#
#   scripts/tests/release-targets.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
fail=0
bad() { printf 'FAIL  %s\n' "$*"; fail=1; }

# The source of truth: `just dist`'s targets and `just desktop`'s outputs,
# plus the Windows installer, which release.yml builds itself on GitHub's
# runner (no just there).
wf=.github/workflows/release.yml
read -ra targets <<<"$(sed -n '/^dist:/,/^[^ ]/p' justfile | sed -n 's/^ *for t in \(.*\); do$/\1/p')"
[ "${#targets[@]}" -gt 0 ] || { echo "FAIL  can't find just dist's targets in the justfile"; exit 1; }
desktop=$(sed -n '/^desktop /,/^[^ ]/p' justfile)
downloads=()
while read -r d; do downloads+=("$d"); done < <(
  {
    grep -o 'illogical-desktop-linux-[A-Za-z0-9_]*\.[A-Za-z]*' <<<"$desktop"
    sed -n 's/.*) name=\(macos-[a-z0-9_]*\) ;;.*/illogical-desktop-\1.zip/p' <<<"$desktop"
    grep -o 'dist/illogical-desktop-windows-[A-Za-z0-9_.-]*\.exe' "$wf" | sed 's:^dist/::'
  } | sort -u
)
[ "${#downloads[@]}" -gt 0 ] || { echo "FAIL  can't find just desktop's outputs in the justfile"; exit 1; }
echo "targets:   ${targets[*]}"
echo "downloads: ${downloads[*]}"

# release.yml builds every target and every download.
for t in "${targets[@]}"; do
  case "$t" in
    *-unknown-linux-musl) step="just static ${t%%-*}" ;;
    aarch64-apple-darwin) step="just build" ;; # the macOS runner's own arch
    x86_64-apple-darwin) step="just build-macos-x86_64" ;;
    *) bad "$t: no rule for how release.yml builds it; add one here"; continue ;;
  esac
  grep -qx " *- run: $step" "$wf" || bad "$wf doesn't build $t (expected: run: $step)"
done
for d in "${downloads[@]}"; do
  case "$d" in
    illogical-desktop-linux-*) step="just desktop" ;;
    illogical-desktop-macos-arm64.zip) step="just desktop" ;; # native
    illogical-desktop-macos-*.zip) a=${d#illogical-desktop-macos-}; step="just desktop ${a%.zip}" ;;
    illogical-desktop-windows-*) step="scripts/release upload \"\$GITHUB_REF_NAME\" dist/$d" ;;
    *) bad "$d: no rule for how release.yml makes it; add one here"; continue ;;
  esac
  grep -qx " *- run: $step" "$wf" || bad "$wf doesn't make $d (expected: run: $step)"
done

# scripts/release counts exactly these before it writes SHA256SUMS.
# shellcheck disable=SC2016 # literal \$n and \$d in scripts/release
n=$(sed -n 's/.*\[ "\$n" = \([0-9]*\) \].*/\1/p' scripts/release)
d=$(sed -n 's/.*\[ "\$d" = \([0-9]*\) \].*/\1/p' scripts/release)
[ "$n" = "${#targets[@]}" ] || bad "scripts/release expects ${n:-?} tarballs; just dist makes ${#targets[@]}"
[ "$d" = "${#downloads[@]}" ] || bad "scripts/release expects ${d:-?} desktop downloads; just desktop makes ${#downloads[@]}"

# The Homebrew formula has a URL for each target, and scripts/release fills
# in every checksum placeholder the formula has (and no others).
formula=packaging/homebrew/illogical.rb.in
for t in "${targets[@]}"; do
  grep -q "illogical-@VERSION@-$t\.tar\.gz" "$formula" || bad "$formula has no URL for $t"
done
in_formula=$(grep -o '@SHA_[A-Z0-9_]*@' "$formula" | sort -u)
# shellcheck disable=SC2016 # a literal $(sha …) in scripts/release
in_release=$(grep -o 's/@SHA_[A-Z0-9_]*@/\$(sha [a-z0-9_-]*)' scripts/release | sort -u)
# shellcheck disable=SC2001 # one per line; sed reads clearer here
filled=$(sed 's:^s/\(@SHA_[A-Z0-9_]*@\)/.*:\1:' <<<"$in_release" | sort -u)
[ "$in_formula" = "$filled" ] || bad "checksum placeholders differ: formula has $(tr '\n' ' ' <<<"$in_formula"), scripts/release fills $(tr '\n' ' ' <<<"$filled")"
# Each placeholder sits under its own target's URL: the sha it's filled
# with is for the tarball on the line above it.
while read -r line; do
  [ -n "$line" ] || continue
  ph=${line#s/}; ph=${ph%%/*}; t=${line##*(sha }; t=${t%)}
  above=$(grep -B1 "sha256 \"$ph\"" "$formula" | head -1)
  grep -q "illogical-@VERSION@-$t\.tar\.gz" <<<"$above" || bad "$ph is filled with $t's checksum but sits under another URL in $formula"
done <<<"$in_release"

# install.sh installs every target.
for t in "${targets[@]}"; do
  grep -q "target=$t\b" scripts/install.sh || bad "scripts/install.sh never picks $t"
done

# The site offers every download, and no page links to one that isn't made.
for dl in "${downloads[@]}"; do
  grep -q "releases/latest/download/$dl\"" site/index.html || bad "site/index.html doesn't link $dl"
done
while IFS=: read -r f name; do
  printf '%s\n' "${downloads[@]}" | grep -qx "$name" || bad "$f links $name, which no release makes"
done < <(grep -oH 'illogical-desktop-[A-Za-z0-9_.-]*[A-Za-z0-9]' README.md site/index.html docs/*.md | sort -u)

[ "$fail" = 0 ] && echo "release targets: all agree"
exit "$fail"
