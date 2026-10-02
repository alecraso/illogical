#!/usr/bin/env python3
"""Kill this spike's daemons and shims, and any shell they left behind.

Ours: an illogicald run from work/, and processes whose environment names a
work/state-* directory (the shell integration's rc path) or whose parent is
one of ours. Prints what it kills."""
import os, signal
W = os.path.join(os.path.dirname(os.path.abspath(__file__)), "work")
me = os.getpid()

def read(p, f):
    try:
        with open(f"/proc/{p}/{f}", "rb") as fh:
            return fh.read()
    except OSError:
        return b""

procs = {}
for d in os.listdir("/proc"):
    if d.isdigit() and int(d) != me:
        s = read(d, "stat").decode(errors="replace")
        if not s:
            continue
        ppid = int(s[s.rindex(")") + 2:].split()[1])
        procs[int(d)] = (ppid, read(d, "cmdline"), read(d, "environ"))
ours = {p for p, (_, cmd, env) in procs.items()
        if cmd.startswith(W.encode() + b"/illogicald") or (W.encode() + b"/state-") in env}
changed = True
while changed:
    changed = False
    for p, (pp, _, _) in procs.items():
        if pp in ours and p not in ours:
            ours.add(p); changed = True
for p in sorted(ours):
    print("kill", p, procs[p][1].replace(b"\0", b" ")[:100].decode(errors="replace"))
    try:
        os.kill(p, signal.SIGKILL)
    except ProcessLookupError:
        pass
print(len(ours), "killed")
