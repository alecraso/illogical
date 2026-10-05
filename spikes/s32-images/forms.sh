#!/bin/sh
# S32: which pasted strings Claude Code turns into [Image #N]. Start `claude`
# in pane $1 (at its prompt), make the files with `python3 images.py`, then run this.
P=${1:?pane}
try() {
  illogical keys %$P C-c >/dev/null; sleep 0.6
  printf "$2" | illogical send %$P - >/dev/null; sleep 1.6
  printf '%-36s %s\n' "$1" "$(illogical capture %$P | grep '^❯' | tail -1)"
}
D=/tmp/illogical-s32
try "bracketed absolute path"        "\033[200~$D/img-red.png\033[201~"
try "typed (not bracketed)"          "$D/img-red.png"
try "bracketed, under \$TMPDIR"       "\033[200~${TMPDIR}illogical-uploads/70/img-20261005-1200.png\033[201~"
try "bracketed, single-quoted"       "\033[200~'$D/img-red.png'\033[201~"
try "bracketed, spaces escaped"      "\033[200~$D/with\\\\ space/img\\\\ red.png\033[201~"
try "bracketed, spaces raw"          "\033[200~$D/with space/img red.png\033[201~"
try "two, space-separated"           "\033[200~$D/img-red.png $D/img-blue.png\033[201~"
try "two, newline-separated"         "\033[200~$D/img-red.png\n$D/img-blue.png\033[201~"
try "no extension"                   "\033[200~$D/noext\033[201~"
try "tilde"                          "\033[200~~/../../tmp/illogical-s32/img-red.png\033[201~"
try "file:// URL"                    "\033[200~file://$D/img-red.png\033[201~"
try "text and path in one paste"     "\033[200~look at $D/img-red.png\033[201~"
try "path after typed text"          "describe \033[200~$D/img-red.png\033[201~"
try "missing file"                   "\033[200~$D/nope.png\033[201~"
for f in img.jpg img.gif img.heic img.tiff IMG.PNG doc.pdf notes.txt; do try "$f" "\033[200~$D/$f\033[201~"; done
