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
list=$(sed -n 's/^app_downloads="\(arugula-desktop-.*\)"$/\1/p' scripts/release)
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
    for a in "${arches[@]}"; do for e in $exts; do echo "arugula-desktop-linux-$a.$e"; done; done
    names=$(sed -n 's/.*) name=\(macos-[a-z0-9_]*\) ;;.*/\1/p' <<<"$macos")
    exts=$(grep -o '\$dist/arugula-desktop-\$name\.[a-z]*$' <<<"$macos" | sed 's/.*\.//' | sort -u)
    for n in $names; do for e in $exts; do echo "arugula-desktop-$n.$e"; done; done
    grep -o 'dist/arugula-desktop-windows-[A-Za-z0-9_.-]*\.exe' "$awf" | sed 's:^dist/::'
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
! grep -q 'arugula-desktop\|desktop-linux\|desktop-macos\|tauri' "$wf" || bad "$wf builds the desktop app; that's app-release.yml's (#393)"

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
    arugula-desktop-linux-*) a=${d#arugula-desktop-linux-}; step="just desktop-linux ${a%%.*}" ;;
    arugula-desktop-macos-arm64.*) step="just desktop-macos aarch64" ;;
    arugula-desktop-macos-*) a=${d#arugula-desktop-macos-}; step="just desktop-macos ${a%%.*}" ;;
    arugula-desktop-windows-*) step="scripts/release app-upload \"\\\$GITHUB_REF_NAME\" dist/$d\\*" ;;
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
formula=packaging/homebrew/arugula.rb.in
for t in "${targets[@]}"; do
  grep -q "arugula-@VERSION@-$t\.tar\.gz" "$formula" || bad "$formula has no URL for $t"
done
in_formula=$(grep -o '@SHA_[A-Z0-9_]*@' "$formula" | sort -u)
# "@PLACEHOLDER@ target", for each `s=$(sha target); sed+=(-e "s/@PLACEHOLDER@/$s/")`.
# shellcheck disable=SC2016 # a literal $(sha …) in scripts/release
in_release=$(sed -n 's/.*s=\$(sha \([a-z0-9_-]*\)); sed+=(-e "s\/\(@SHA_[A-Z0-9_]*@\)\/.*/\2 \1/p' scripts/release | sort -u)
[ -n "$in_release" ] || bad "can't find the checksums scripts/release fills in"
filled=$(cut -d' ' -f1 <<<"$in_release" | sort -u)
[ "$in_formula" = "$filled" ] || bad "checksum placeholders differ: formula has $(tr '\n' ' ' <<<"$in_formula"), scripts/release fills $(tr '\n' ' ' <<<"$filled")"
# Each placeholder sits under its own target's URL: the sha it's filled
# with is for the tarball on the line above it.
while read -r ph t; do
  [ -n "$ph" ] || continue
  above=$(grep -B1 "sha256 \"$ph\"" "$formula" | head -1)
  grep -q "arugula-@VERSION@-$t\.tar\.gz" <<<"$above" || bad "$ph is filled with $t's checksum but sits under another URL in $formula"
done <<<"$in_release"

# install.sh installs every target.
for t in "${targets[@]}"; do
  grep -q "target=$t\b" scripts/install.sh || bad "scripts/install.sh never picks $t"
done
# And on a Mac the app's zip, which the app's release makes (#318).
zips=$(grep -o 'zip=arugula-desktop-[A-Za-z0-9_-]*\.zip' scripts/install.sh | sed 's/^zip=//')
[ -n "$zips" ] || bad "scripts/install.sh fetches no Mac app zip"
for z in $zips; do
  printf '%s\n' "${downloads[@]}" | grep -qx "$z" || bad "scripts/install.sh fetches $z, which no app release makes"
done

# The site offers every download, and no page links to one that isn't made.
# Not yet on the site: the .rpm and .dmg and the Linux arm64 bundles (M46,
# #130); they're in each release. Linking one there drops it from here.
not_on_site='\.rpm$|\.dmg$|^arugula-desktop-linux-aarch64\.'
for dl in "${downloads[@]}"; do
  grep -Eq "$not_on_site" <<<"$dl" && ! grep -q "releases/download/app-latest/$dl\"" site/index.html && continue
  grep -q "releases/download/app-latest/$dl\"" site/index.html || bad "site/index.html doesn't link $dl from app-latest"
done
while IFS=: read -r f name; do
  printf '%s\n' "${downloads[@]}" | grep -qx "$name" || bad "$f links $name, which no release makes"
done < <(grep -oH 'arugula-desktop-[A-Za-z0-9_.-]*[A-Za-z0-9]' README.md site/index.html docs/*.md | sort -u)

# The old names (#505, drop in #508): daemons on 0.24 and 0.25 update
# themselves by fetching illogical-VERSION-TARGET.{tar.gz,zip}, and the
# site's older links point at app-latest's illogical-desktop-*. Every
# archive is packed by scripts/dist-pack, which makes both.
# shellcheck disable=SC2016 # literal $n, $name and ${f#…} in the scripts
sed -n '/^dist:/,/^[^ ]/p' justfile | grep -q 'scripts/dist-pack "dist/\$n"' || bad "just dist doesn't pack with scripts/dist-pack"
# shellcheck disable=SC2016
grep -q 'scripts/dist-pack "dist/\$name"' scripts/windows-dist || bad "scripts/windows-dist doesn't pack with scripts/dist-pack"
grep -q 'dist/illogical-\*-x86_64-pc-windows-msvc.zip' "$wf" || bad "$wf doesn't upload the Windows zip's old name"
grep -q '^    for p in arugula illogical; do$' scripts/release || bad "scripts/release sums doesn't count the old names"
grep -q 'sha256sum -- arugula-\[0-9\]\*.tar.gz arugula-\[0-9\]\*.zip illogical-\[0-9\]\*.tar.gz illogical-\[0-9\]\*.zip > SHA256SUMS' scripts/release \
  || bad "scripts/release's SHA256SUMS doesn't list the old names"
# shellcheck disable=SC2016
grep -q 'illogical-desktop-\${f#arugula-desktop-}' scripts/release || bad "scripts/release doesn't put app-latest's downloads under their old names"

# What scripts/dist-pack makes, from stand-in binaries: the old archive
# holds illogical-VERSION-TARGET/illogicald, where 0.24 looks (and 0.25
# first), beside the arugulad and arugula the new install copies.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for t in x86_64-unknown-linux-musl x86_64-pc-windows-msvc; do
  case "$t" in *-windows-*) x=.exe e=zip ;; *) x="" e=tar.gz ;; esac
  n=arugula-1.2.3-$t o=illogical-1.2.3-$t
  mkdir -p "$work/$n" "$work/$t"
  echo daemon >"$work/$n/arugulad$x"; echo cli >"$work/$n/arugula$x"; echo license >"$work/$n/LICENSE-MIT"
  scripts/dist-pack "$work/$n" >/dev/null || { bad "scripts/dist-pack failed for $t"; continue; }
  for a in "$n.$e" "$o.$e"; do [ -f "$work/$a" ] || bad "scripts/dist-pack made no $a"; done
  [ ! -e "$work/$o" ] || bad "scripts/dist-pack left its $o folder behind"
  if [ "$e" = zip ]; then
    python3 -I -m zipfile -e "$work/$n.$e" "$work/$t/new"
    python3 -I -m zipfile -e "$work/$o.$e" "$work/$t/old"
  else
    mkdir -p "$work/$t/new" "$work/$t/old"
    tar -xzf "$work/$n.$e" -C "$work/$t/new"
    tar -xzf "$work/$o.$e" -C "$work/$t/old"
  fi
  got=$(cd "$work/$t/new" && find . -type f | LC_ALL=C sort | tr '\n' ' ')
  [ "$got" = "./$n/LICENSE-MIT ./$n/arugula$x ./$n/arugulad$x " ] || bad "$n.$e holds $got"
  got=$(cd "$work/$t/old" && find . -mindepth 1 -maxdepth 1)
  [ "$got" = "./$o" ] || bad "$o.$e holds $got, not $o/"
  for f in illogicald arugulad; do
    [ "$(cat "$work/$t/old/$o/$f$x" 2>/dev/null)" = daemon ] || bad "$o.$e has no $o/$f$x that's arugulad"
  done
  for f in "illogical$x" "arugula$x" LICENSE-MIT; do
    [ "$(cat "$work/$t/old/$o/$f")" = "$(cat "$work/$n/$f" 2>/dev/null || echo cli)" ] || bad "$o.$e has no $o/$f, or not arugula's"
  done
done

# The Homebrew tap's files: the formula (named arugula now, every checksum
# filled), the old formula gone, and formula_renames.json moving `illogical`
# installs to `arugula` (keeping any other renames).
tap=$work/tap
mkdir -p "$tap/Formula"
echo old >"$tap/Formula/illogical.rb"
echo '{"other": "thing"}' >"$tap/formula_renames.json"
for t in "${targets[@]}"; do printf '%064d  arugula-1.2.3-%s.tar.gz\n' "${#t}" "$t"; done >"$work/SHA256SUMS"
if scripts/release tap-files v1.2.3 "$tap" "$work/SHA256SUMS" >/dev/null; then
  f=$tap/Formula/arugula.rb
  grep -q '^class Arugula < Formula$' "$f" || bad "the tap's formula isn't class Arugula"
  ! grep -q '@[A-Z_]*@' "$f" || bad "the tap's formula has placeholders left: $(grep -o '@[A-Z_]*@' "$f" | tr '\n' ' ')"
  [ "$(grep -c 'sha256 "[0-9a-f]\{64\}"' "$f")" = "${#targets[@]}" ] || bad "the tap's formula hasn't a checksum for each target"
  [ ! -e "$tap/Formula/illogical.rb" ] || bad "the tap keeps Formula/illogical.rb, so brew won't rename it"
  python3 -I -c 'import json, sys; r = json.load(open(sys.argv[1])); sys.exit(r != {"illogical": "arugula", "other": "thing"})' "$tap/formula_renames.json" \
    || bad "the tap's formula_renames.json is $(cat "$tap/formula_renames.json")"
else
  bad "scripts/release tap-files failed"
fi
# A release missing a target's tarball stops it.
grep -v aarch64-apple-darwin "$work/SHA256SUMS" >"$work/SHORT"
! scripts/release tap-files v1.2.3 "$work/tap2" "$work/SHORT" >/dev/null 2>&1 || bad "scripts/release tap-files wrote a formula with a checksum missing"

[ "$fail" = 0 ] && echo "release targets: all agree"
exit "$fail"
