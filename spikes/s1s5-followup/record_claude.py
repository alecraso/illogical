#!/usr/bin/env python3
"""Record Claude Code's TUI in a real PTY as a fixture.

Writes fixtures/claude.bin + .json (the session while it is still running,
after an answer and a resize 100x30 -> 80x24) and fixtures/claude_exit.bin
+ .json (the same bytes plus /exit and what is left on screen afterwards).

The nested claude runs with every CLAUDE* session variable of the parent
removed, `--model haiku`, and the tools that change anything disallowed.
"""
import fcntl, json, os, pty, select, struct, sys, termios, time

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures")
PROMPT = "Reply with exactly these words and nothing else: hello from haiku"
DENY = "Bash Edit Write NotebookEdit WebFetch WebSearch Agent Task"


def set_size(fd, cols, rows):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def drain(fd, buf, seconds, until=None):
    """Read for `seconds`, or until `until` (bytes) appears in new output."""
    start = len(buf)
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return False
            if not data:
                return False
            buf.extend(data)
            if until and until in buf[start:]:
                return True
    return False


def main():
    cols, rows = 100, 30
    removed = sorted(k for k in os.environ if k.startswith("CLAUDE"))
    print("unset:", " ".join(removed))
    cwd = os.path.join(HERE, "work")
    pid, fd = pty.fork()
    if pid == 0:
        for k in removed:
            os.environ.pop(k, None)
        os.environ.update(TERM="xterm-256color", COLUMNS=str(cols), LINES=str(rows))
        os.chdir(cwd)
        os.execvp("claude", ["claude", "--model", "haiku", "--disallowedTools", DENY])
    set_size(fd, cols, rows)
    buf, resizes, log = bytearray(), [], []

    def step(msg):
        log.append(f"{len(buf):7d} {msg}")
        print(log[-1], flush=True)

    drain(fd, buf, 8)
    step("started")
    if b"trust" in buf.lower():
        os.write(fd, b"\r")
        drain(fd, buf, 4)
        step("trust prompt answered")
    for ch in PROMPT:
        os.write(fd, ch.encode())
        drain(fd, buf, 0.02)
    drain(fd, buf, 0.5)
    os.write(fd, b"\r")
    got = drain(fd, buf, 40, until=b"hello from haiku")
    drain(fd, buf, 4)
    step(f"answered={got}")
    resizes.append(dict(offset=len(buf), cols=80, rows=24))
    set_size(fd, 80, 24)
    drain(fd, buf, 3)
    step("resized to 80x24")
    running = len(buf)
    os.write(fd, b"/exit")
    drain(fd, buf, 0.8)
    os.write(fd, b"\r")
    drain(fd, buf, 5)
    step("after /exit")
    try:
        os.kill(pid, 9)
    except ProcessLookupError:
        pass
    os.waitpid(pid, 0)

    meta = dict(cols=cols, rows=rows, resizes=resizes)
    for name, data in [("claude", buf[:running]), ("claude_exit", buf)]:
        with open(os.path.join(OUT, name + ".bin"), "wb") as f:
            f.write(data)
        with open(os.path.join(OUT, name + ".json"), "w") as f:
            json.dump(meta, f)
        print(f"{name}: {len(data)} bytes")
    with open(os.path.join(OUT, "claude.log"), "w") as f:
        f.write("\n".join(log) + "\n")


if __name__ == "__main__":
    main()
