#!/usr/bin/env python3
"""Record raw PTY output of scripted sessions as S1 fixtures.

Each fixture is fixtures/<name>.bin (raw output bytes) plus
fixtures/<name>.json ({cols, rows, resizes: [{offset, cols, rows}]}).
"""
import fcntl, json, os, pty, select, struct, sys, termios, time

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures")

ESC = "\x1b"
SCENARIOS = {
    # Primary screen with deep scrollback.
    "seq": dict(cmd="seq 1 5000; echo done", steps=[(1.0, None)]),
    # Styles, palette, hyperlink, cursor shape, mouse modes, bracketed paste,
    # left in place for the snapshot to carry over.
    "modes": dict(
        cmd=(
            "printf '\\e[1;31mbold red\\e[0m \\e[38;5;208m256\\e[0m \\e[38;2;10;200;90mtruecolor\\e[0m\\n';"
            "printf '\\e]8;;https://example.com\\e\\\\link\\e]8;;\\e\\\\\\n';"
            "printf '\\e]4;1;rgb:ff/00/ff\\e\\\\';"
            "printf '\\e[5 q\\e[?1000h\\e[?1006h\\e[?2004h\\e[?1h\\e=';"
            "printf 'wide: 漢字 emoji: 👍🏽 combining: e\\xcc\\x81\\n';"
            "printf 'title\\e]0;my title\\a\\e]7;file://geek/tmp\\a';"
            "sleep 30"
        ),
        steps=[(1.0, None)],
    ),
    # Full-screen app on top of primary scrollback: the alt-screen gap case.
    "nvim": dict(
        cmd="seq 1 300; exec nvim -u NONE -i NONE /etc/services",
        steps=[
            (1.0, ":set number\r"),
            (0.3, ":vsplit\r"),
            (0.3, "200G"),
            (0.3, ":set cursorline\r"),
            (0.5, None),
        ],
    ),
    "top": dict(cmd="seq 1 100; exec top -d 0.5", steps=[(2.0, None)]),
    "less": dict(cmd="seq 1 2000 | less", steps=[(0.8, " "), (0.3, " "), (0.3, None)]),
    # Narrow mid-run: reflow on the primary screen.
    "resize": dict(
        cmd="for i in $(seq 1 60); do printf 'line %03d %s\\n' $i $(printf 'x%.0s' $(seq 1 90)); done; sleep 30",
        steps=[(1.0, ("resize", 60, 30)), (0.5, None)],
    ),
    # Resize while a full-screen app runs.
    "nvim_resize": dict(
        cmd="seq 1 50; exec nvim -u NONE -i NONE /etc/services",
        steps=[(1.0, ":set number\r"), (0.3, ("resize", 70, 20)), (0.5, None)],
    ),
}


def set_size(fd, cols, rows):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def drain(fd, buf, seconds):
    end = time.time() + seconds
    while True:
        left = end - time.time()
        if left <= 0:
            return
        r, _, _ = select.select([fd], [], [], left)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            if not data:
                return
            buf.extend(data)


def record(name, spec, cols=100, rows=30):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ.update(TERM="xterm-256color", COLUMNS=str(cols), LINES=str(rows))
        os.execvp("bash", ["bash", "--noprofile", "--norc", "-c", spec["cmd"]])
    set_size(fd, cols, rows)
    buf, resizes = bytearray(), []
    for delay, action in spec["steps"]:
        drain(fd, buf, delay)
        if isinstance(action, str):
            os.write(fd, action.encode())
        elif isinstance(action, tuple) and action[0] == "resize":
            _, c, r = action
            resizes.append(dict(offset=len(buf), cols=c, rows=r))
            set_size(fd, c, r)
    drain(fd, buf, 0.3)
    os.kill(pid, 9)
    os.waitpid(pid, 0)
    with open(os.path.join(OUT, name + ".bin"), "wb") as f:
        f.write(buf)
    with open(os.path.join(OUT, name + ".json"), "w") as f:
        json.dump(dict(cols=cols, rows=rows, resizes=resizes), f)
    print(f"{name}: {len(buf)} bytes, {len(resizes)} resizes")


if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    for name in sys.argv[1:] or SCENARIOS:
        record(name, SCENARIOS[name])
