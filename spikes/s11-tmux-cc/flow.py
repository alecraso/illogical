#!/usr/bin/env python3
"""S11: what tmux does when a pause-after client stops reading.

Attach with -CC, set pause-after=1 (seconds), start a flood in the pane, stop
reading the pty for 3 s, then read. Expect `%pause %0` if a block ages past 1 s; but with only control clients
attached tmux stops reading the pane PTY instead (server_client_check_pane_buffer),
so usually nothing ages and no %pause comes. `%continue %0` follows
`refresh-client -A '%0:continue'` either way. Appends to
work/flow.txt.
"""

import os
import subprocess
import time

import replay
from replay import Client, NL, SOCK, CONF, WORK


def main():
    os.makedirs(WORK, exist_ok=True)
    replay.RAW = open(os.path.join(WORK, "flow-raw.log"), "w")
    replay.TX = open(os.path.join(WORK, "flow.txt"), "w")
    subprocess.run(["tmux", "-L", SOCK, "kill-server"], stderr=subprocess.DEVNULL)
    c = Client(["tmux", "-L", SOCK, "-f", CONF, "-CC", "new", "-s", "flow"], "F")
    c.wait_note("%session-changed")
    c.wait_idle()
    c.send("refresh-client -fpause-after=1", "refresh-client -C 80,24")
    c.wait_idle()
    c.send("send -lt %0 'yes s11-flood | head -c 2000000; echo done'", "send -H -t %0 0d",
           note="2 MB flood, then stop reading for 4 s")
    time.sleep(4)
    c.wait_note("%pause", limit=3)
    c.wait_idle(quiet=0.5)
    c.send("capture-pane -p -t %0 -S -3", "refresh-client -A '%0:continue'",
           note="iTerm2 refetches the screen, then continues")
    c.wait_idle()
    c.send("detach")
    c.wait_idle(limit=3)
    c.tx(NL)
    c.pump(0.5)
    os.close(c.fd)
    os.waitpid(c.pid, 0)
    subprocess.run(["tmux", "-L", SOCK, "kill-server"], stderr=subprocess.DEVNULL)
    replay.TX.close()
    replay.RAW.close()


if __name__ == "__main__":
    main()
