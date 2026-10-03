#!/bin/sh
# S23: fetch a PR's head into a scratch repo, diff merge-base..head, and compare the file list with the forge's.
#
#   ./fetch-pr.sh DIR REMOTE_URL HEAD_REF BASE_REF FORGE_FILES_JSON JQ_PATHS [MERGE_BASE]
#
# HEAD_REF is refs/pull/N/head (GitHub, Forgejo) or refs/merge-requests/N/head (GitLab).
# BASE_REF is the target branch (refs/heads/main). MERGE_BASE, when given, is the forge's own
# (Forgejo's merge_base, GitLab's diff_refs.base_sha; not start_sha, which is the target's tip), for a PR whose base has moved on since it merged,
# or from:SHA to take the merge-base of SHA (GitHub's base.sha) and the head.
# JQ_PATHS turns the forge's file list into one path per line.
# Read-only: fetches only. Times each step on stderr.
set -eu
dir=$1 url=$2 head=$3 base=$4 files=$5 jqpaths=$6 mb=${7:-}
t() { s=$(date +%s%N); "$@"; e=$(date +%s%N); echo "  $(( (e - s) / 1000000 )) ms: $*" >&2; }
rm -rf "$dir"; git init -q "$dir"; cd "$dir"
git remote add origin "$url"
# A treeless partial fetch: commits only; trees and blobs come on demand when diff needs them.
t git fetch -q --filter=tree:0 origin "+$base:refs/remotes/origin/base" "+$head:refs/pr/head"
case $mb in
  "") mb=$(git merge-base refs/remotes/origin/base refs/pr/head) ;;
  from:*) mb=$(git merge-base "${mb#from:}" refs/pr/head) ;;   # GitHub: merge-base of the PR's base.sha and head
esac
echo "  merge-base $mb, head $(git rev-parse refs/pr/head)" >&2
t git diff --name-only "$mb" refs/pr/head > ../git-files.txt
jq -r "$jqpaths" "$files" | sort > ../forge-files.txt
sort -o ../git-files.txt ../git-files.txt
if cmp -s ../git-files.txt ../forge-files.txt; then
  echo "  MATCH: $(wc -l < ../git-files.txt) files" >&2
else
  echo "  DIFFER:" >&2; diff ../git-files.txt ../forge-files.txt >&2 || true
fi
echo "  objects: $(git count-objects -vH | grep size-pack)" >&2
