#!/usr/bin/env python3
"""Q5: drive the Claude Code TUI in a PTY at a fixed size (120x40).

usage: work/venv/bin/python tui.py <name> <settings.json> [prompt]
- Runs `claude --model haiku --setting-sources local --settings <file>` in work/tui (its own git repo),
  with the parent Claude Code session's env removed.
- Renders the screen with pyte. Every second writes work/tui-<name>.screen (current screen) and appends
  changed screens to work/tui-<name>.screens; raw bytes go to work/tui-<name>.raw.
- Keys: anything written to the FIFO work/tui-<name>.keys is sent to the PTY. Escapes: \\r \\e \\x03,
  <down> <up> <tab> <space> <enter> <esc> <ctrl-c>.
- Types the prompt once the input box is shown, answers the folder-trust dialog with Enter.
"""
import os, pty, sys, time, select, re, signal, json
import pyte

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(HERE, "work")
name, settings = sys.argv[1], os.path.abspath(sys.argv[2])
prompt = sys.argv[3] if len(sys.argv) > 3 else None
COLS, ROWS = 120, 40
base = os.path.join(WORK, f"tui-{name}")
keys_fifo = base + ".keys"
if os.path.exists(keys_fifo):
    os.unlink(keys_fifo)
os.mkfifo(keys_fifo)

env = {k: v for k, v in os.environ.items()
       if not (k == "CLAUDECODE" or k.startswith("CLAUDE_CODE_") or k in ("CLAUDE_PID", "CLAUDE_EFFORT", "AI_AGENT"))}
env["TERM"] = "xterm-256color"
env["COLUMNS"], env["LINES"] = str(COLS), str(ROWS)

pid, fd = pty.fork()
if pid == 0:
    os.chdir(os.path.join(WORK, "tui"))
    import fcntl, termios, struct
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    os.execvpe("claude", ["claude", "--model", "haiku", "--setting-sources", "local", "--settings", settings], env)

import fcntl, termios, struct
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
screen = pyte.Screen(COLS, ROWS)
stream = pyte.ByteStream(screen)
raw = open(base + ".raw", "wb")
screens = open(base + ".screens", "w")
kfd = os.open(keys_fifo, os.O_RDONLY | os.O_NONBLOCK)
t0 = time.time()
last_dump, last_text = 0, ""
typed = False
trusted = False
KEYMAP = {"<down>": "\x1b[B", "<up>": "\x1b[A", "<tab>": "\t", "<space>": " ", "<enter>": "\r", "<esc>": "\x1b", "<ctrl-c>": "\x03", "<right>": "\x1b[C", "<left>": "\x1b[D"}


def text():
    return "\n".join(line.rstrip() for line in screen.display)


def send(s):
    for k, v in KEYMAP.items():
        s = s.replace(k, v)
    s = s.replace("\\r", "\r").replace("\\e", "\x1b").replace("\\x03", "\x03")
    screens.write(f"\n### {time.time()-t0:.1f}s SEND {s!r}\n")
    screens.flush()
    os.write(fd, s.encode())


while True:
    r, _, _ = select.select([fd, kfd], [], [], 0.2)
    if fd in r:
        try:
            d = os.read(fd, 65536)
        except OSError:
            break
        if not d:
            break
        raw.write(d); raw.flush()
        stream.feed(d)
    if kfd in r:
        k = os.read(kfd, 4096)
        if k:
            send(k.decode())
        else:
            os.close(kfd)
            kfd = os.open(keys_fifo, os.O_RDONLY | os.O_NONBLOCK)
    now = time.time()
    if now - last_dump > 1:
        last_dump = now
        cur = text()
        with open(base + ".screen", "w") as f:
            f.write(f"{now-t0:.1f}s\n{cur}\n")
        if cur != last_text:
            screens.write(f"\n### {now-t0:.1f}s\n{cur}\n"); screens.flush()
            last_text = cur
        if not trusted and re.search(r"Yes, I trust this folder", cur):
            trusted = True
            time.sleep(0.5); send("<down>"); time.sleep(0.3); send("\r")
        elif not typed and prompt and re.search(r"^\s*[>❯]\s", cur, re.M) and now - t0 > 3:
            typed = True
            os.write(fd, prompt.encode()); time.sleep(0.8); send("\r")
    try:
        p, st = os.waitpid(pid, os.WNOHANG)
        if p:
            break
    except ChildProcessError:
        break
screens.write(f"\n### {time.time()-t0:.1f}s EXIT\n"); screens.close()
os.unlink(keys_fifo)
