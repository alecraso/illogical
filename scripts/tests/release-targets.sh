#!/usr/bin/env bash
# A ratchet on what a release ships. The targets `just dist` packs and the
# downloads the desktop recipes make are named in several other places: how
# release.yml builds them, how many scripts/release expects, the Homebrew
# formula, install.sh and the download links. Adding (or dropping) one in
# some of those places and not the others fails here, not at release time.
#
#   scripts/tests/release-targets.sh
set -euo pipefail

cd "$(dirname "$0")/../.."
fail=0
bad() { printf 'FAIL  %s\n' "$*"; fail=1; }

# The sources of truth: `just dist`'s targets, and the desktop downloads
# scripts/release requires before it publishes an app release. Each download
# must be one the build makes (`just desktop-linux ARCH` through
# packaging/desktop/build-linux.sh, `just desktop-macos ARCH`, and the
# Windows installer, which app-release.yml builds itself on GitHub's
# runner), and each one the build makes must be required. The daemon's
# release (release.yml, v*) builds the targets; the app's (app-release.yml,
# app-v*) the downloads (#393).
wf=.github/workflows/release.yml
awf=.github/workflows/app-release.yml
read -ra targets <<<"$(sed -n '/^dist:/,/^[^ ]/p' justfile | sed -n 's/^ *for t in \(.*\); do$/\1/p')"
[ "${#targets[@]}" -gt 0 ] || { echo "FAIL  can't find just dist's targets in the justfile"; exit 1; }
list=$(sed -n 's/^app_downloads="\(illogical-desktop-.*\)"$/\1/p' scripts/release)
[ -n "$list" ] || { echo "FAIL  can't find scripts/release's desktop downloads"; exit 1; }
downloads=()
# Brace expansion of scripts/release's own list; it names files only.
while read -r d; do downloads+=("$d"); done < <(eval "printf '%s\n' $list" | sort -u)
echo "targets:   ${targets[*]}"
echo "downloads: ${downloads[*]}"

linux=$(sed -n '/^desktop-linux /,/^[^ ]/p' justfile)
macos=$(sed -n '/^desktop-macos /,/^[^ ]/p' justfile)
# shellcheck disable=SC2016 # literal $arch, $name and $dist in the recipes
made=$(
  {
    read -ra arches <<<"$(sed -n 's/^ *case "\$arch" in \(.*\) esac$/\1/p' <<<"$linux" | grep -o '[a-z0-9_]*) platform' | sed 's/) platform//' | tr '\n' ' ')"
    exts=$(grep -o '"/dist/\$name\.[A-Za-z]*"' packaging/desktop/build-linux.sh | sed 's/.*\.\([A-Za-z]*\)"$/\1/' | sort -u)
    for a in "${arches[@]}"; do for e in $exts; do echo "illogical-desktop-linux-$a.$e"; done; done
    names=$(sed -n 's/.*) name=\(macos-[a-z0-9_]*\) ;;.*/\1/p' <<<"$macos")
    exts=$(grep -o '\$dist/illogical-desktop-\$name\.[a-z]*$' <<<"$macos" | sed 's/.*\.//' | sort -u)
    for n in $names; do for e in $exts; do echo "illogical-desktop-$n.$e"; done; done
    grep -o 'dist/illogical-desktop-windows-[A-Za-z0-9_.-]*\.exe' "$awf" | sed 's:^dist/::'
  } | sort -u
)
[ -n "$made" ] || { echo "FAIL  can't find what the desktop recipes make in the justfile"; exit 1; }
for m in $made; do
  printf '%s\n' "${downloads[@]}" | grep -qx "$m" || bad "the build makes $m but scripts/release doesn't require it"
done
for d in "${downloads[@]}"; do
  grep -qx "$d" <<<"$made" || bad "scripts/release requires $d but no recipe makes it"
done

# A release.yml step runs it, alone or in an `a && b` chain.
runs() { grep -Eq "^ *- run: (.* && )?$1( && .*)?$" "$wf"; }
# An app-release.yml step (or a line of one) runs it, maybe with VAR=value
# in front.
app_runs() { grep -Eq "^ *(- run: )?([A-Z_]+=[^ ]+ )*$1\$" "$awf"; }

# The daemon's release makes no app.
! grep -q 'illogical-desktop\|desktop-linux\|desktop-macos\|tauri' "$wf" || bad "$wf builds the desktop app; that's app-release.yml's (#393)"

# release.yml builds every target and every download.
for t in "${targets[@]}"; do
  case "$t" in
    *-unknown-linux-musl) step="just static ${t%%-*}" ;;
    aarch64-apple-darwin) step="just build" ;; # the macOS runner's own arch
    x86_64-apple-darwin) step="just build-macos-x86_64" ;;
    *) bad "$t: no rule for how release.yml builds it; add one here"; continue ;;
  esac
  runs "$step" || bad "$wf doesn't build $t (expected: run: $step)"
done
for d in "${downloads[@]}"; do
  case "$d" in
    illogical-desktop-linux-*) a=${d#illogical-desktop-linux-}; step="just desktop-linux ${a%%.*}" ;;
    illogical-desktop-macos-arm64.*) step="just desktop-macos aarch64" ;;
    illogical-desktop-macos-*) a=${d#illogical-desktop-macos-}; step="just desktop-macos ${a%%.*}" ;;
    illogical-desktop-windows-*) step="scripts/release app-upload \"\\\$GITHUB_REF_NAME\" dist/$d\\*" ;;
    *) bad "$d: no rule for how app-release.yml makes it; add one here"; continue ;;
  esac
  app_runs "$step" || bad "$awf doesn't make $d (expected: run: $step)"
done

# scripts/release counts exactly these tarballs before it writes SHA256SUMS.
# shellcheck disable=SC2016 # literal \$n in scripts/release
n=$(sed -n 's/.*\[ "\$n" = \([0-9]*\) \].*/\1/p' scripts/release)
[ "$n" = "${#targets[@]}" ] || bad "scripts/release expects ${n:-?} tarballs; just dist makes ${#targets[@]}"

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
# And on a Mac the app's zip, which the app's release makes (#318).
zips=$(grep -o 'zip=illogical-desktop-[A-Za-z0-9_-]*\.zip' scripts/install.sh | sed 's/^zip=//')
[ -n "$zips" ] || bad "scripts/install.sh fetches no Mac app zip"
for z in $zips; do
  printf '%s\n' "${downloads[@]}" | grep -qx "$z" || bad "scripts/install.sh fetches $z, which no app release makes"
done

# The site offers every download, and no page links to one that isn't made.
# Not yet on the site: the .rpm and .dmg and the Linux arm64 bundles (M46,
# #130); they're in each release. Linking one there drops it from here.
not_on_site='\.rpm$|\.dmg$|^illogical-desktop-linux-aarch64\.'
for dl in "${downloads[@]}"; do
  grep -Eq "$not_on_site" <<<"$dl" && ! grep -q "releases/download/app-latest/$dl\"" site/index.html && continue
  grep -q "releases/download/app-latest/$dl\"" site/index.html || bad "site/index.html doesn't link $dl from app-latest"
done
while IFS=: read -r f name; do
  printf '%s\n' "${downloads[@]}" | grep -qx "$name" || bad "$f links $name, which no release makes"
done < <(grep -oH 'illogical-desktop-[A-Za-z0-9_.-]*[A-Za-z0-9]' README.md site/index.html docs/*.md | sort -u)

[ "$fail" = 0 ] && echo "release targets: all agree"
exit "$fail"
