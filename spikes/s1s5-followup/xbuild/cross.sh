#!/usr/bin/env bash
# Encode every fixture with the pinned and the main build, decode each
# snapshot with both builds (C harness), and decode the main-written ones
# with the Rust harness too (pinned build, the full cell-by-cell compare).
set -u
cd "$(dirname "$0")/.."
B=work/bin; S=work/xsnap; mkdir -p $S
for bin in fixtures/*.bin; do
  n=$(basename $bin .bin)
  read cols rows rs < <(python3 -c "
import json; m=json.load(open('fixtures/$n.json')); r=m['resizes']
print(m['cols'], m['rows'], ('%d:%d:%d' % (r[0]['offset'], r[0]['cols'], r[0]['rows'])) if r else '')")
  [ "$(python3 -c "import json; print(len(json.load(open('fixtures/$n.json'))['resizes']))")" -gt 1 ] && continue
  echo "== $n"
  $B/snapcheck-pinned encode $bin $cols $rows $S/$n.pinned.snap $rs
  $B/snapcheck-main encode $bin $cols $rows $S/$n.main.snap $rs
  echo " sizes: pinned $(stat -c%s $S/$n.pinned.snap) B, main $(stat -c%s $S/$n.main.snap) B, identical bytes: $(cmp -s $S/$n.pinned.snap $S/$n.main.snap && echo yes || echo no)"
  for w in pinned main; do for r in pinned main; do
    echo " written by $w, read by $r:"
    $B/snapcheck-$r decode $S/$n.$w.snap $bin $cols $rows $rs
  done; done
  echo " written by main, read by the Rust harness (pinned):"
  work/target/release/s1s5-followup decode $n $S/$n.main.snap | sed 's/^/  /'
done
