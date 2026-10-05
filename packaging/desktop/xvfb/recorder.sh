#!/bin/sh
# A pane for the desktop tests (m46.sh, the daemon's --shell): it records
# where it started, its size while it runs, and every byte typed into it,
# under $REC_DIR/<pid>/.
d=$REC_DIR/$$
mkdir -p "$d"
pwd >"$d/cwd"
(while :; do stty size </dev/tty >"$d/size.new" 2>/dev/null && mv "$d/size.new" "$d/size"; sleep 0.3; done) &
stty raw -echo
exec dd bs=1 of="$d/keys" 2>/dev/null
