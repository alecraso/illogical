#!/usr/bin/env python3
"""Drives the S25 probe with real key presses, through a uinput keyboard.

    S25_BIN=shell/target/release/s25-desktop ./drive.py results/probe-geek.jsonl

Starts the probe with S25_AUTO=1, waits for it to say it has focus, then
presses each chord and checks the page logged it before the next one. Keys go
to whichever window has focus, so a chord that doesn't arrive is followed by
a harmless `z`; if that doesn't arrive either, the run stops rather than
typing Ctrl-W/Q into someone else's window. Needs write access to /dev/uinput
(logind grants it to the seat's user) and `ibus` for the IME part.
"""

import fcntl, json, os, struct, subprocess, sys, time

# linux/input-event-codes.h
K = dict(ESC=1, MINUS=12, BACKSPACE=14, TAB=15, ENTER=28, LEFTCTRL=29, LEFTSHIFT=42, LEFTALT=56, SPACE=57,
         LEFTBRACE=26, RIGHTBRACE=27, BACKSLASH=43, COMMA=51, DOT=52, PAGEUP=104, PAGEDOWN=109, LEFT=105,
         LEFTMETA=125, HANGEUL=122, ZENKAKUHANKAKU=85, F11=87, F12=88)
for i, c in enumerate("1234567890"):
    K[c] = 2 + i
for row, start in (("QWERTYUIOP", 16), ("ASDFGHJKL", 30), ("ZXCVBNM", 44)):
    for i, c in enumerate(row):
        K[c] = start + i
for i in range(10):
    K[f"F{i + 1}"] = 59 + i
MODS = {"Ctrl": K["LEFTCTRL"], "Shift": K["LEFTSHIFT"], "Alt": K["LEFTALT"], "Meta": K["LEFTMETA"]}
# DOM `code` -> key name above
CODE = {"Tab": "TAB", "PageUp": "PAGEUP", "PageDown": "PAGEDOWN", "Comma": "COMMA", "Period": "DOT",
        "Backspace": "BACKSPACE", "ArrowLeft": "LEFT", "Backslash": "BACKSLASH", "BracketLeft": "LEFTBRACE",
        "Space": "SPACE", "Escape": "ESC"}


class Keyboard:
    def __init__(self):
        self.fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
        fcntl.ioctl(self.fd, 0x40045564, 1)  # UI_SET_EVBIT EV_KEY
        for code in range(1, 200):
            fcntl.ioctl(self.fd, 0x40045565, code)  # UI_SET_KEYBIT
        setup = struct.pack("HHHH80sI", 0x03, 0x1234, 0x5678, 1, b"s25-probe-keyboard", 0)
        fcntl.ioctl(self.fd, 0x405C5503, setup)  # UI_DEV_SETUP
        fcntl.ioctl(self.fd, 0x5501)  # UI_DEV_CREATE
        time.sleep(1.0)  # the compositor picks the device up

    def _ev(self, typ, code, value):
        os.write(self.fd, struct.pack("llHHi", 0, 0, typ, code, value))

    def _key(self, code, down):
        self._ev(1, code, 1 if down else 0)
        self._ev(0, 0, 0)
        time.sleep(0.012)

    def press(self, *codes):
        for c in codes:
            self._key(c, True)
        for c in reversed(codes):
            self._key(c, False)
        time.sleep(0.03)

    def chord(self, chord):
        """`Ctrl-Shift-KeyT`, `F5`, `Alt-Period`: the probe's own spelling."""
        parts = chord.split("-")
        mods, code = parts[:-1], parts[-1]
        name = code[3:] if code.startswith("Key") else code[5:] if code.startswith("Digit") else CODE.get(code, code)
        self.press(*[MODS[m] for m in mods], K[name])

    def type(self, text):
        for ch in text:
            self.press(K["SPACE"] if ch == " " else K[ch.upper()])

    def close(self):
        fcntl.ioctl(self.fd, 0x5502)  # UI_DEV_DESTROY
        os.close(self.fd)


class Log:
    def __init__(self, path):
        self.path, self.pos, self.lines = path, 0, []

    def poll(self):
        if os.path.exists(self.path):
            with open(self.path) as f:
                f.seek(self.pos)
                for line in f:
                    self.lines.append(json.loads(line))
                self.pos = f.tell()

    def wait(self, pred, timeout=1.5):
        end = time.time() + timeout
        while time.time() < end:
            self.poll()
            for o in self.lines:
                if pred(o):
                    return o
            time.sleep(0.05)
        return None

    def mark(self):
        self.poll()
        return len(self.lines)

    def since(self, n):
        self.poll()
        return self.lines[n:]


def got_key(log, n, chord, timeout=1.0):
    end = time.time() + timeout
    while time.time() < end:
        if any(o.get("key") == chord for o in log.since(n)):
            return True
        time.sleep(0.05)
    return False


CHORDS = (
    [f"F{i}" for i in range(1, 13)]
    + ["Alt-KeyB", "Alt-KeyF", "Alt-KeyX", "Alt-Period", "Alt-Backspace", "Alt-ArrowLeft"]
    + ["Ctrl-KeyC", "Ctrl-KeyD", "Ctrl-KeyZ", "Ctrl-Backslash", "Ctrl-BracketLeft", "Ctrl-Space", "Escape"]
    + [f"Ctrl-Shift-Key{k}" for k in "TWNCVPI"]
    + ["Ctrl-Tab", "Ctrl-Shift-Tab", "Ctrl-PageUp", "Ctrl-PageDown"]
    + [f"Ctrl-{k}" for k in ("KeyL", "KeyR", "KeyF", "KeyP", "Comma", "Digit1", "Digit9", "KeyT", "KeyN", "KeyH", "KeyM", "KeyW", "KeyQ")]
)


def ibus(*args):
    return subprocess.run(["ibus", *args], capture_output=True, text=True).stdout.strip()


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "results/probe.jsonl"
    if os.path.exists(out):
        os.remove(out)
    env = dict(os.environ, S25_AUTO="1", S25_LOG=os.path.abspath(out))
    app = subprocess.Popen([os.environ.get("S25_BIN", "shell/target/release/s25-desktop"), "probe"], env=env,
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    log = Log(out)
    kb = None
    summary = {"received": [], "missing": [], "aborted": None}
    try:
        ready = log.wait(lambda o: o.get("ready"), 20)
        if not ready:
            summary["aborted"] = "the probe never logged ready"
            return summary
        log.wait(lambda o: o.get("autodone"), 20)
        kb = Keyboard()
        n = log.mark()
        kb.type("z")
        if not got_key(log, n, "KeyZ"):
            summary["aborted"] = f"no focus: a test z didn't reach the page (page said focus={ready['focus']})"
            return summary
        for chord in CHORDS:
            n = log.mark()
            kb.chord(chord)
            if got_key(log, n, chord):
                summary["received"].append(chord)
                continue
            summary["missing"].append(chord)
            n = log.mark()
            kb.type("z")
            if not got_key(log, n, "KeyZ"):
                summary["aborted"] = f"focus lost after {chord}"
                return summary

        # IME: set the engine directly, type, and see what xterm got.
        before = ibus("engine")
        for engine, keys, toggle in (("mozc-on", "nihongo", None), ("hangul", "gksrnr", "HANGEUL")):
            n = log.mark()
            ibus("engine", engine)
            time.sleep(0.5)
            if toggle:
                kb.press(K[toggle])
                time.sleep(0.2)
            kb.type(keys)
            kb.press(K["SPACE"])
            time.sleep(0.3)
            kb.press(K["ENTER"])
            time.sleep(0.5)
            if toggle:
                kb.press(K[toggle])
            summary[engine] = [o for o in log.since(n) if "ime" in o or "onData" in o]
        ibus("engine", before or "xkb:us::eng")
        summary["ibus_restored"] = ibus("engine")

        # Clipboard with a gesture (Ctrl-Alt-G), then a notification click through
        # the same D-Bus signal the notification server sends.
        n = log.mark()
        kb.press(K["LEFTCTRL"], K["LEFTALT"], K["G"])
        log.wait(lambda o: o.get("clip") == "restored", 10)
        # Paste: Rust puts text on the clipboard, then the engine's own paste chords.
        for chord in ("Ctrl-Shift-KeyV", "Ctrl-KeyV"):
            kb.press(K["LEFTCTRL"], K["LEFTALT"], K["P"])
            log.wait(lambda o: o.get("paste") == "armed", 3)
            n = log.mark()
            kb.chord(chord)
            time.sleep(0.8)
            summary[f"paste {chord}"] = [o.get("paste") for o in log.since(n) if "paste" in o] or [o.get("onData") for o in log.since(n) if "onData" in o]
        # Put the user's clipboard back (the gesture test ends by restoring it).
        n = log.mark()
        kb.press(K["LEFTCTRL"], K["LEFTALT"], K["G"])
        end = time.time() + 10
        while time.time() < end and not any(o.get("clip") == "restored" for o in log.since(n)):
            time.sleep(0.1)
        note = next((o for o in log.lines if o.get("note") == "native shown"), None)
        if note and note.get("id"):
            subprocess.run(["gdbus", "emit", "--session", "--object-path", "/org/freedesktop/Notifications",
                            "--signal", "org.freedesktop.Notifications.ActionInvoked", str(note["id"]), "default"])
            summary["notification_click"] = log.wait(lambda o: o.get("note") == "clicked", 3)
        kb.press(K["LEFTCTRL"], K["LEFTALT"], K["O"])
        summary["new_window"] = log.wait(lambda o: "window" in o, 5)
        time.sleep(1.5)
        return summary
    finally:
        if kb:
            kb.close()
        app.terminate()
        try:
            stdout, _ = app.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            app.kill()
            stdout, _ = app.communicate()
        summary["stdout"] = [l for l in stdout.splitlines() if l.startswith("s25:")]
        log.poll()
        summary["log"] = log.lines
        with open(out.replace(".jsonl", ".summary.json"), "w") as f:
            json.dump(summary, f, indent=1, ensure_ascii=False)


if __name__ == "__main__":
    s = main()
    print(json.dumps({k: v for k, v in s.items() if k != "log"}, indent=1, ensure_ascii=False))
