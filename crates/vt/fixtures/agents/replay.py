#!/usr/bin/env python3
"""A fake agent: plays back a recorded session (`record.py`) in its
terminal, with the recording's timing, waiting wherever someone typed.

Tests copy this file to a bin directory as `claude` (or `codex`) and put
the recording beside it as `claude.cast`, so the daemon sees a program
named `claude` and reads its screen as Claude Code's. `$ILLOGICAL_REPLAY`
names another recording.

- Output is written as recorded, `$ILLOGICAL_REPLAY_SPEED` times as fast
  (default 1).
- Where the recording has input ("i"), it waits for its last key: Enter
  (`\\r` or `\\n`) after a prompt, Ctrl-O, an arrow. Other keys, and what
  a terminal sends back on its own (device attributes), are passed over.
  Then the recording carries on from there.
- Each marker ("m") it reaches is appended to `<self>.log` (`m working`),
  with `start` first and `end` last, so a test can wait for a point in the
  recording rather than sleep.
- Every start appends its argv and working directory to `<self>.argv` as a
  JSON line: `{"argv": [...], "cwd": "..."}` (the resume tests read it).
- With `--resume <id>`, the id must name a transcript in
  `<self>.sessions/<id>`; if it doesn't, it says so and exits 1, as Claude
  Code does.
- `$ILLOGICAL_REPLAY_PAUSE=working=8` stops for 8 seconds at the first
  `working` marker, printing nothing: a long think, or a tool call that's
  slow to say anything.
- At the end it waits, as an agent at its prompt would, until its input
  closes, Ctrl-C or Ctrl-D.

Standard library only.
"""
import json, os, re, sys, termios, time, tty

ME = os.path.abspath(sys.argv[0])
REPORT = re.compile(rb"\x1b(\[[0-9;?<>=]*[ -/]*[@-~]|\][^\x07\x1b]*(\x07|\x1b\\)|P[^\x1b]*\x1b\\|O.|.)")


def log(line):
    with open(ME + ".log", "a") as f:
        f.write(line + "\n")


def tokens(raw):
    """Keys, one by one: an escape sequence is one (an arrow, or something
    the terminal sent back on its own), anything else a byte. `ESC O B` (an
    arrow in application cursor mode) is `ESC [ B`, and `\\n` is `\\r`."""
    out, i = [], 0
    while i < len(raw):
        m = REPORT.match(raw, i)
        if m:
            out.append(m.group(0).replace(b"\x1bO", b"\x1b[", 1))
            i = m.end()
        else:
            out.append(raw[i : i + 1].replace(b"\n", b"\r"))
            i += 1
    return out


def main():
    with open(ME + ".argv", "a") as f:
        f.write(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd()}) + "\n")
    args = sys.argv[1:]
    if "--resume" in args:
        i = args.index("--resume")
        sid = args[i + 1] if i + 1 < len(args) else ""
        if not os.path.isfile(os.path.join(ME + ".sessions", sid)):
            print(f"No conversation found with session ID: {sid}")
            return 1
    cast = os.environ.get("ILLOGICAL_REPLAY") or os.path.splitext(ME)[0] + ".cast"
    speed = float(os.environ.get("ILLOGICAL_REPLAY_SPEED") or 1)
    pause = dict(p.split("=", 1) for p in (os.environ.get("ILLOGICAL_REPLAY_PAUSE") or "").split(",") if "=" in p)
    with open(cast) as f:
        events = [json.loads(line) for line in f.read().splitlines()[1:] if line.strip()]
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd) if os.isatty(fd) else None
    if saved:
        tty.setraw(fd)
    out = sys.stdout.buffer
    log("start")
    # Keys typed and not yet used: two can come at once.
    typed = []
    # Recording time `t` plays at `base + t / speed`.
    base = time.monotonic()
    try:
        for t, kind, data in events:
            if kind == "i":
                # Its last key (Enter after a prompt); what came before it,
                # and anything the terminal said, is passed over.
                want = tokens(data.encode())[-1]
                while True:
                    while not typed:
                        chunk = os.read(fd, 4096)
                        if not chunk:
                            return 0
                        typed.extend(tokens(chunk))
                    if typed.pop(0) == want:
                        break
                base = time.monotonic() - t / speed
                continue
            delay = base + t / speed - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            if kind == "o":
                out.write(data.encode())
                out.flush()
            elif kind == "m":
                log(f"m {data}")
                if data in pause:
                    time.sleep(float(pause.pop(data)))
                    base = time.monotonic() - t / speed
        log("end")
        while True:
            chunk = os.read(fd, 4096)
            if not chunk or b"\x03" in chunk or b"\x04" in chunk:
                return 0
    finally:
        if saved:
            termios.tcsetattr(fd, termios.TCSADRAIN, saved)


if __name__ == "__main__":
    sys.exit(main())
