#!/usr/bin/env python3
"""Types into s26-gtk with real keys (S25's uinput keyboard) and checks the
dev daemon's pane got them. Stops if the first key doesn't land.

    drive_gtk.py SOCK OUTDIR
"""
import os, subprocess, sys, time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "../../s25-desktop"))
from drive import K, Keyboard  # noqa: E402

sock, out = sys.argv[1], sys.argv[2]
cli = os.path.expanduser("~/.local/bin/illogical")
here = os.path.dirname(os.path.abspath(__file__))
binary = os.path.join(here, "../target/release/s26-gtk")


def screen():
    return subprocess.run([cli, "--socket", sock, "capture", "%1"], capture_output=True, text=True).stdout


def wait_for(text, timeout=3.0):
    end = time.time() + timeout
    while time.time() < end:
        if text in screen():
            return True
        time.sleep(0.1)
    return False


def type_line(kb, s):
    for ch in s:
        if ch == " ":
            kb.press(K["SPACE"])
        elif ch == "-":
            kb.press(K["MINUS"])
        elif ch == "'":
            kb.press(K["LEFTSHIFT"] if False else 40)
        else:
            kb.press(K[ch.upper()])
    kb.press(K["ENTER"])


def shot(name, delay_ms):
    """A fresh window that saves what it draws, then exits."""
    env = dict(os.environ, S26_SHOT=os.path.join(out, name), S26_SHOT_MS=str(delay_ms))
    subprocess.run(["timeout", str(delay_ms / 1000 + 3), binary, "--socket", sock, "--pane", "1"], env=env, capture_output=True)


result = {}
kb = Keyboard()
app = subprocess.Popen([binary, "--socket", sock, "--pane", "1"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(2.5)
try:
    kb.press(K["LEFTCTRL"], K["U"])
    subprocess.run([cli, "--socket", sock, "send", "%1", "clear\n"])
    time.sleep(0.5)
    type_line(kb, "echo typed ok")
    result["typing"] = wait_for("typed ok\n") or wait_for("typed ok")
    if not result["typing"]:
        result["aborted"] = "typing didn't reach the pane (focus elsewhere?)"
        result["screen"] = screen()[-400:]
        raise RuntimeError(result)
    # Ctrl-C at a prompt, Ctrl-L clears: control keys through libghostty's encoder.
    type_line(kb, "sleep 30")
    time.sleep(0.4)
    kb.press(K["LEFTCTRL"], K["C"])
    result["ctrl_c"] = wait_for("^C")
    # IME: Mozc then Hangul, committed into the pane.
    before = subprocess.run(["ibus", "engine"], capture_output=True, text=True).stdout.strip()
    type_line(kb, "echo")  # an echo line to commit text into
    time.sleep(0.3)
    kb.press(K["LEFTCTRL"], K["C"])
    for word in ("echo ",):
        for ch in word:
            kb.press(K["SPACE"] if ch == " " else K[ch.upper()])
    subprocess.run(["ibus", "engine", "mozc-on"])
    time.sleep(0.5)
    for ch in "nihongo":
        kb.press(K[ch.upper()])
    kb.press(K["SPACE"])
    time.sleep(0.3)
    kb.press(K["ENTER"])  # commits the conversion
    time.sleep(0.3)
    subprocess.run(["ibus", "engine", "hangul"])
    time.sleep(0.5)
    kb.press(K["HANGEUL"])
    for ch in "gksrnr":
        kb.press(K[ch.upper()])
    kb.press(K["HANGEUL"])
    time.sleep(0.3)
    subprocess.run(["ibus", "engine", before or "xkb:us::eng"])
    time.sleep(0.3)
    kb.press(K["ENTER"])
    result["ime_mozc"] = wait_for("日本語")
    result["ime_hangul"] = wait_for("한국")
    result["ibus_restored"] = subprocess.run(["ibus", "engine"], capture_output=True, text=True).stdout.strip()
finally:
    kb.close()
    app.terminate()
    app.wait()

# vim and htop, seen by a fresh window each.
subprocess.run([cli, "--socket", sock, "send", "%1", "clear; vim -u NONE -c 'syntax on' -c 'set number cursorline' /etc/ssh/sshd_config\n"])
time.sleep(1.5)
shot("vim.png", 1500)
subprocess.run([cli, "--socket", sock, "send", "%1", ":q!\n"])
time.sleep(0.5)
subprocess.run([cli, "--socket", sock, "send", "%1", "clear; htop\n"])
time.sleep(1.5)
shot("htop.png", 2500)
subprocess.run([cli, "--socket", sock, "send", "%1", "q"])
print(result)
