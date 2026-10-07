# The guest end of a VM agent's MCP relay (#59). The daemon runs it as a
# non-TTY exec in the agent's VM (`python3 -c THIS arugula-mcp-relay
# SOCKET`): it listens on SOCKET and carries every connection over the
# exec's stdin and stdout, one line each way per message:
#
#   N+         connection N opened        (guest to host)
#   N:LINE     a line for connection N    (both ways)
#   N-         connection N closed        (both ways)
#
# The host serves each connection as its own MCP session, so an agent that
# restarts its MCP client just connects again. A newer relay for the same
# socket (the daemon restarted) replaces this one.
import os
import signal
import socket
import sys
import threading

path = sys.argv[-1]
pidfile = path + ".pid"

# The relay before us, if it's still there.
try:
    with open(pidfile) as f:
        old = int(f.read().strip())
    with open(f"/proc/{old}/cmdline", "rb") as f:
        # (Under the old name too: one a daemon from before the rename ran, #505.)
        cmdline = f.read()
        if (b"arugula-mcp-relay" in cmdline or b"illogical-mcp-relay" in cmdline) and old != os.getpid():
            os.kill(old, signal.SIGTERM)
except (OSError, ValueError):
    pass
try:
    os.unlink(path)
except FileNotFoundError:
    pass

os.umask(0o077)
listener = socket.socket(socket.AF_UNIX)
listener.bind(path)
listener.listen(16)
with open(pidfile, "w") as f:
    f.write(str(os.getpid()))
print("arugula: MCP relay on", path, file=sys.stderr, flush=True)

lock = threading.Lock()
conns = {}
out = sys.stdout.buffer


def emit(b):
    with lock:
        out.write(b)
        out.flush()


def up(n, c):
    try:
        for line in c.makefile("rb"):
            line = line.rstrip(b"\r\n")
            if line.strip():
                emit(b"%d:%s\n" % (n, line))
    except OSError:
        pass
    if conns.pop(n, None) is not None:
        emit(b"%d-\n" % n)
    c.close()


def accept():
    n = 0
    while True:
        c, _ = listener.accept()
        n += 1
        conns[n] = c
        emit(b"%d+\n" % n)
        threading.Thread(target=up, args=(n, c), daemon=True).start()


threading.Thread(target=accept, daemon=True).start()

for line in sys.stdin.buffer:
    i = 0
    while i < len(line) and line[i:i + 1].isdigit():
        i += 1
    if i == 0 or i == len(line):
        continue
    n, kind, rest = int(line[:i]), line[i:i + 1], line[i + 1:]
    c = conns.get(n)
    if c is None:
        continue
    try:
        if kind == b":":
            c.sendall(rest)
        elif kind == b"-":
            conns.pop(n, None)
            c.shutdown(socket.SHUT_RDWR)
    except OSError:
        pass
# The daemon let go of us for good.
os._exit(0)
