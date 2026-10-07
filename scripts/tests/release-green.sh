#!/usr/bin/env bash
# `scripts/release check-green` (#503): a release tag on a commit with a
# ci/green tag passes; one with no green tag, or with a ci/revoked tag too,
# fails, and ARUGULA_RELEASE_SKIP_GREEN=1 lets it through. Against a local
# bare repository standing in for GitHub (GITHUB_SERVER_URL), so nothing
# reaches the network.
#
#   scripts/tests/release-green.sh
set -euo pipefail

release=$(cd "$(dirname "$0")/.." && pwd)/release
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { printf 'FAIL  %s\n' "$*"; fail=1; }

export GITHUB_SERVER_URL=file://$tmp GITHUB_REPOSITORY=forge/arugula
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.invalid GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.invalid
unset ARUGULA_RELEASE_SKIP_GREEN
git init -q --bare "$tmp/forge/arugula.git"
git init -q "$tmp/work"
cd "$tmp/work"
git commit -q --allow-empty -m one
git tag -a v1.0.0 -m one
git commit -q --allow-empty -m two
git tag -a app-v1.0.0 -m two
git commit -q --allow-empty -m three
git tag -a v1.0.1 -m three
one=$(git rev-parse 'v1.0.0^{commit}') two=$(git rev-parse 'app-v1.0.0^{commit}')
git tag -a "ci/green/$one" -m green "$one"
git tag -a "ci/green/$two" -m green "$two"
git tag -a "ci/revoked/$two" -m revoked "$two"
git push -q "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY.git" --tags

expect() { # WANT TAG [ENV...]: check-green's exit status is WANT
  local want=$1 tag=$2 got=0
  shift 2
  env "$@" "$release" check-green "$tag" >"$tmp/out" 2>&1 || got=$?
  if [ "$got" = "$want" ]; then printf 'ok    %s %s: %s\n' "$tag" "$*" "$(tail -1 "$tmp/out")"
  else bad "$tag $*: exit $got, wanted $want: $(cat "$tmp/out")"; fi
}
expect 0 v1.0.0
expect 1 app-v1.0.0
expect 1 v1.0.1
expect 0 v1.0.1 ARUGULA_RELEASE_SKIP_GREEN=1
expect 1 v9.9.9

exit $fail
