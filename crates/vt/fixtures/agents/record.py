#!/usr/bin/env python3
"""Record an agent's terminal session for the replay agent (`replay.py`).

    record.py claude_turn [OUT_DIR]     run the real `claude` and record it
    record.py codex_turn [OUT_DIR]      synthesize Codex from ../screens

Writes OUT_DIR/<name>.cast (default: next to this file): asciicast v2, a
JSON header line and then `[seconds, kind, text]` events. Kind "o" is
output, "i" is what was typed, and "m" is a marker naming the state the
screen shows at that moment (`working`, `blocked`, `idle`), which the tests
check detection against.

A recorded session runs the real agent in a pty at 80x24, in a new scratch
git repo under /tmp (shown as ~/src/demo), typing what the scenario says once the screen (drawn by
pyte: `pip install pyte`) shows what it waits for. It runs without the
user's settings (`--setting-sources project`), so none of their hooks fire,
with an `illogical` on PATH that does nothing, and without the environment
of any Claude Code it was started from. $HOME and the user name are
replaced, emails blanked, and the session's transcript is removed after.
Read the result before committing it.

Codex isn't installed where these were made, so `codex_turn` is drawn from
the screens captured from Codex 0.155 (`../screens/codex_*.txt`) with
plausible timing, and says so in its header.
"""
import codecs, fcntl, json, os, pty, re, select, shutil, signal, struct, subprocess, sys, tempfile, termios, time

COLS, ROWS = 80, 24
HERE = os.path.dirname(os.path.abspath(__file__))

# Each step: ("expect", regex, marker or None, timeout), ("type", text),
# ("sleep", seconds).
SCENARIOS = {
    # Claude Code from a fresh folder: trust it; ask for a command, approve
    # it, wait through it, look at the transcript view; again, with the
    # transcript view open while it works; leave.
    "claude_turn": dict(
        cmd=["claude", "--model", "haiku", "--setting-sources", "project", "--strict-mcp-config"],
        steps=[
            ("expect", r"trust this folder", "blocked", 30),
            ("sleep", 0.5),
            # "No, exit" comes first: down to "Yes, I trust this folder".
            ("type", "\x1b[B"),
            ("sleep", 0.3),
            ("type", "\r"),
            ("expect", r"\? for shortcuts", "idle", 30),
            ("sleep", 1.0),
            ("type", "Run this shell command: sleep 4 && touch made-by-claude.txt\r"),
            ("expect", r"esc to interrupt", "working", 30),
            ("expect", r"Do you want to proceed\?", "blocked", 90),
            ("sleep", 1.0),
            ("type", "\r"),
            ("expect", r"esc to interrupt", "working", 30),
            ("expect", r"\? for shortcuts", "idle", 90),
            ("sleep", 1.5),
            ("type", "\x0f"),
            ("expect", r"(?i)showing detailed transcript", "idle", 10),
            ("sleep", 1.0),
            ("type", "\x0f"),
            ("expect", r"\? for shortcuts", "idle", 10),
            ("sleep", 1.0),
            ("type", "Run this shell command: sleep 6 && touch made-again.txt\r"),
            ("expect", r"esc to interrupt", "working", 30),
            ("expect", r"Do you want to proceed\?", "blocked", 90),
            ("sleep", 1.0),
            ("type", "\r"),
            ("expect", r"esc to interrupt", "working", 30),
            ("sleep", 0.5),
            ("type", "\x0f"),
            ("expect", r"(?i)showing detailed transcript", "working", 10),
            ("sleep", 1.0),
            ("type", "\x0f"),
            ("expect", r"\? for shortcuts", "idle", 90),
            ("sleep", 1.0),
        ],
    ),
}


class Session:
    def __init__(self, cmd, cwd, env):
        import pyte

        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.chdir(cwd)
            os.execvpe(cmd[0], cmd, env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.t0 = time.time()
        self.events = []
        self.screen = pyte.Screen(COLS, ROWS)
        self.stream = pyte.Stream(self.screen)
        # A character can be split across reads.
        self.utf8 = codecs.getincrementaldecoder("utf-8")("replace")

    def at(self):
        return round(time.time() - self.t0, 4)

    def pump(self, seconds):
        end = time.time() + seconds
        while (left := end - time.time()) > 0:
            r, _, _ = select.select([self.fd], [], [], left)
            if not r:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return False
            if not data:
                return False
            text = self.utf8.decode(data)
            if not text:
                continue
            self.events.append([self.at(), "o", text])
            self.stream.feed(text)
            self.answer(text)
        return True

    def answer(self, text):
        # Answer the queries a real terminal would, so the agent doesn't
        # wait out its timeouts. (Not recorded: they aren't typed.)
        if "\x1b[6n" in text:
            os.write(self.fd, f"\x1b[{ROWS};1R".encode())
        if "\x1b[c" in text or "\x1b[0c" in text:
            os.write(self.fd, b"\x1b[?62;22c")

    def text(self):
        return "\n".join(line.rstrip() for line in self.screen.display)

    def expect(self, pattern, marker, timeout):
        rx = re.compile(pattern)
        end = time.time() + timeout
        while not rx.search(self.text()):
            if time.time() > end:
                sys.exit(f"timed out waiting for {pattern!r}; the screen:\n{self.text()}")
            if not self.pump(0.05):
                sys.exit(f"it exited waiting for {pattern!r}")
        if marker:
            # Let the frame that showed it finish drawing.
            self.pump(0.3)
            self.events.append([self.at(), "m", marker])

    def type(self, text):
        self.events.append([self.at(), "i", text])
        os.write(self.fd, text.encode())

    def close(self):
        os.kill(self.pid, signal.SIGTERM)
        self.pump(0.5)
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(self.pid, 0)


def scrub(events, scratch):
    pairs = [("/private" + scratch, "~/src/demo"), (scratch, "~/src/demo"), (os.path.expanduser("~"), "~")]
    if user := os.environ.get("USER"):
        pairs.append((user, "me"))
    out = []
    for t, kind, text in events:
        for old, new in pairs:
            # Same width, so a box drawn around it still lines up.
            text = text.replace(old, new.ljust(len(old)))
        text = re.sub(r"[\w.+-]+@[\w-]+(\.[\w-]+)+", lambda m: "x" * len(m.group(0)), text)
        text = re.sub(r"[\w.+'’ -]+'s Organization", lambda m: " " * len(m.group(0)), text)
        out.append([t, kind, text])
    return out


def record(name):
    spec = SCENARIOS[name]
    # A new folder each time: Claude Code remembers trusting one.
    top = tempfile.mkdtemp(prefix="rec-", dir="/tmp")
    scratch = os.path.join(top, "demo")
    os.makedirs(scratch)
    subprocess.run(["git", "init", "-q", scratch], check=True)
    stub = tempfile.mkdtemp(prefix="agent-rec-bin-")
    with open(os.path.join(stub, "illogical"), "w") as f:
        f.write("#!/bin/sh\nexit 0\n")
    os.chmod(os.path.join(stub, "illogical"), 0o755)
    env = {k: v for k, v in os.environ.items() if not (k.startswith("CLAUDE") and k != "CLAUDE_CONFIG_DIR")}
    env = {k: v for k, v in env.items() if not k.startswith(("ILLOGICAL", "TERM_PROGRAM"))}
    env.update(PATH=f"{stub}:{env['PATH']}", TERM="xterm-256color", COLUMNS=str(COLS), LINES=str(ROWS))
    s = Session(spec["cmd"], scratch, env)
    try:
        for step in spec["steps"]:
            match step:
                case ("expect", pattern, marker, timeout):
                    s.expect(pattern, marker, timeout)
                case ("type", text):
                    s.type(text)
                case ("sleep", secs):
                    s.pump(secs)
    finally:
        s.close()
        shutil.rmtree(top, ignore_errors=True)
        shutil.rmtree(stub, ignore_errors=True)
        # The conversation it left in the user's Claude Code.
        config = os.environ.get("CLAUDE_CONFIG_DIR") or os.path.expanduser("~/.claude")
        for d in (scratch, "/private" + scratch):
            shutil.rmtree(os.path.join(config, "projects", re.sub(r"[^A-Za-z0-9]", "-", d)), ignore_errors=True)
    return dict(), scrub(s.events, scratch)


def frame(title, screen):
    """Codex draws in place: a title, then the screen from the top."""
    body = screen.strip("\n").replace("\n", "\r\n")
    return f"\x1b]0;{title}\x07\x1b[2J\x1b[H{body}"


def screen(name):
    with open(os.path.join(HERE, "..", "screens", f"{name}.txt")) as f:
        title, body = f.read().split("\n", 1)
    return title.removeprefix("title: "), body


def synth_codex():
    """Trust the folder, prompt, work (its spinner in the title), ask to
    run a command, approve it, work, finish."""
    spinner = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
    prompt = "Run exactly this shell command: curl -sI https://example.com"
    events, t = [], 0.4

    def out(title, body, dt=0.0):
        nonlocal t
        t += dt
        events.append([round(t, 4), "o", frame(title, body)])

    def mark(state):
        events.append([round(t + 0.05, 4), "m", state])

    def typed(text, dt=1.0):
        nonlocal t
        t += dt
        events.append([round(t, 4), "i", text])

    out(*screen("codex_trust"))
    mark("blocked")
    typed("\r")
    out(*screen("codex_idle"), 0.3)
    mark("idle")
    typed(prompt + "\r", 1.5)
    working = (
        f"› {prompt}\n\n• Working ({{}}s • esc to interrupt)\n\n\n"
        "› Ask Codex to do anything\n\n  some-model default · ~/src/demo"
    )
    # Frames every 80 ms, the elapsed seconds once a second.
    for i in range(30):
        s = spinner[i % 10]
        out(f"{s} running curl {s} | demo", working.format(int(i * 0.08)), 0.08 if i else 0.1)
        if i == 6:
            mark("working")
    out(*screen("codex_approval"), 0.1)
    mark("blocked")
    typed("\r", 2.0)
    for i in range(20):
        s = spinner[i % 10]
        out(f"{s} running curl {s} | demo", working.format(3 + int(i * 0.08)), 0.08)
        if i == 6:
            mark("working")
    out(*screen("codex_done"), 0.2)
    mark("idle")
    return dict(synthesized=True, source="crates/vt/fixtures/screens/codex_*.txt (Codex 0.155)"), events


def main():
    name = sys.argv[1]
    out_dir = sys.argv[2] if len(sys.argv) > 2 else HERE
    extra, events = synth_codex() if name == "codex_turn" else record(name)
    header = dict(version=2, width=COLS, height=ROWS, title=name, env=dict(TERM="xterm-256color"), **extra)
    path = os.path.join(out_dir, f"{name}.cast")
    with open(path, "w") as f:
        f.write(json.dumps(header, ensure_ascii=False) + "\n")
        for e in events:
            f.write(json.dumps(e, ensure_ascii=False) + "\n")
    print(f"{path}: {len(events)} events over {events[-1][0]:.1f}s")


if __name__ == "__main__":
    main()
