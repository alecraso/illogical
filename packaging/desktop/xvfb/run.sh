#!/usr/bin/env bash
# Runs the desktop tests named in its arguments, in the Xvfb container
# (`just desktop-xvfb`): `join`, `m46 [claim...]`, `m47` (Nautilus), `stale
# [claim...]` (#317). APP is the app, BIN the
# directory with the static arugulad and arugula, HOST their triple.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tests=()
for w in "$@"; do
  case "$w" in
    join | m46 | m47 | stale) tests+=("$w") ;;
    *) [ ${#tests[@]} -gt 0 ] || { echo "no desktop test $w (join, m46, m47, stale)" >&2; exit 2; }
       tests[${#tests[@]}-1]+=" $w" ;;
  esac
done
failed=()
for t in "${tests[@]}"; do
  set -- $t
  name=$1; shift
  echo "=== $name $*"
  case "$name" in
    join) "$here/test.sh" "$APP" "$BIN/arugulad-$HOST" ;;
    m46) "$here/m46.sh" "$APP" "$BIN/arugulad-$HOST" "$BIN/arugula-$HOST" "$@" ;;
    m47) "$here/m46.sh" "$APP" "$BIN/arugulad-$HOST" "$BIN/arugula-$HOST" nautilus ;;
    stale) "$here/stale.sh" "$APP" "$@" ;;
  esac || failed+=("$name")
done
[ ${#failed[@]} -eq 0 ] || { echo "failed: ${failed[*]}" >&2; exit 1; }
