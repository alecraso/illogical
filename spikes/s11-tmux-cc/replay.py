#!/usr/bin/env python3
"""S11: replay iTerm2's tmux -CC conversation against a real tmux.

Runs `tmux -L illogical-s11 -CC ...` on a pty (as iTerm2's ssh/shell session
would), and plays the client side the way iTerm2 does, using the command
strings from iTerm2's sources (TmuxController.m, TmuxGateway.m,
TmuxWindowOpener.m, iTermInitialDirectory+Tmux.m, iTermTmuxOptionMonitor.m,
iTermTmuxClientTracker.swift; see README for the commit). Like iTerm2, it
waits for answers only where iTerm2 needs one to decide what to send next.

Writes, under work/:
  raw.log         every chunk both ways, escaped, with timestamps
  transcript.txt  line view: "> " client lines, "< " server lines, "# " notes

Usage: python3 replay.py   (kills the illogical-s11 server first and last)
"""

import os
import pty
import re
import select
import subprocess
import sys
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(HERE, "work")
SOCK = "illogical-s11"
CONF = os.path.join(HERE, "s11.tmux.conf")
NL = "\r"  # iTerm2's default line terminator (TmuxGateway.newline)
CLIENT_COLS, CLIENT_ROWS = 120, 40  # the "profile size" iTerm2 would use
MAX_HISTORY = 1000  # MAX(client height, scrollback lines); iTerm2 default 1000

# list-windows detailed format (TmuxController -listWindowsDetailedFormat), >= 2.4
DETAILED = '"' + "\t".join([
    "#{session_name}", "#{window_id}", "#{window_name}", "#{window_width}",
    "#{window_height}", "#{window_layout}", "#{window_flags}",
    "#{?window_active,1,0}", "#{window_visible_layout}", "#{pane-border-status}",
]) + '"'
# commandToListWindows, >= 2.4
LIST_WINDOWS = ('list-windows -F "#{window_id} #{window_layout} #{window_flags} '
                '#{window_visible_layout} #{pane-border-status}"')
# TmuxStateParser +format
STATE_KEYS = ["pane_id", "alternate_on", "alternate_saved_x", "alternate_saved_y",
              "cursor_x", "cursor_y", "scroll_region_upper", "scroll_region_lower",
              "pane_tabs", "cursor_flag", "insert_flag", "keypad_cursor_flag",
              "keypad_flag", "wrap_flag", "mouse_standard_flag", "mouse_button_flag",
              "mouse_any_flag", "mouse_utf8_flag", "mouse_sgr_flag",
              "bracket_paste_flag", "pane_key_mode"]
STATE_FMT = "\t".join(f"{k}=#{{{k}}}" for k in STATE_KEYS)


class Client:
    def __init__(self, argv, tag):
        self.tag = tag
        self.t0 = time.monotonic()
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            os.environ.update(TERM="xterm-256color", LANG="C.UTF-8")
            os.execvp(argv[0], argv)
        self.buf = b""
        self.lines = []          # decoded server lines, in order
        self.queue = []          # commands awaiting %begin
        self.current = None      # [cmd, [lines]] between %begin and %end
        self.done = []           # (cmd, ok, lines)
        self.notes = []          # notifications
        self.exited = False

    def log(self, direction, data):
        t = time.monotonic() - self.t0
        RAW.write(f"{self.tag} {t:9.3f} {direction} {data!r}\n")

    def tx(self, text, note=None):
        if note:
            TX.write(f"# {note}\n")
        self.log(">", text.encode())
        TX.write("> " + text.replace(NL, "").replace("\x03", "^C") + "\n")
        os.write(self.fd, text.encode())

    def send(self, *cmds, note=None):
        """One line; several commands are joined with '; ' like
        TmuxGateway -sendCommandList. Each gets its own %begin/%end."""
        self.queue.extend(cmds)
        self.tx("; ".join(cmds) + NL, note)

    def pump(self, timeout=0.2):
        end = time.monotonic() + timeout
        while True:
            left = end - time.monotonic()
            if left <= 0:
                return
            r, _, _ = select.select([self.fd], [], [], left)
            if not r:
                return
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                self.exited = True
                return
            if not data:
                self.exited = True
                return
            self.log("<", data)
            self.buf += data
            while b"\n" in self.buf:
                line, self.buf = self.buf.split(b"\n", 1)
                self.on_line(line.rstrip(b"\r"))
            if self.buf == b"\x1b\\":  # ST has no newline after it
                TX.write("< \\033\\   (ST: control mode ends)\n")
                self.buf = b""

    def on_line(self, raw):
        line = raw.decode("utf-8", "replace")
        # The DCS that starts control mode is glued to the first line.
        if line.startswith("\x1bP1000p"):
            TX.write("< \\033P1000p   (DCS: control mode starts)\n")
            line = line[len("\x1bP1000p"):]
        if line.startswith("\x1b\\"):
            TX.write("< \\033\\   (ST: control mode ends)\n")
            line = line[2:]
            if not line:
                return
        TX.write("< " + line.replace("\x1b", "\\033").replace("\t", "\\t") + "\n")
        self.lines.append(line)
        m = re.match(r"^%(begin|end|error) (\d+) (\d+) (\d+)$", line)
        if m:
            kind, flags = m.group(1), int(m.group(4))
            if kind == "begin":
                cmd = self.queue.pop(0) if flags & 1 and self.queue else "(server)"
                self.current = [cmd, []]
            else:
                cmd, body = self.current
                self.done.append((cmd, kind == "end", body))
                self.current = None
            return
        if self.current is not None:
            self.current[1].append(line)
            return
        self.notes.append(line)
        if line.startswith("%exit"):
            self.exited = True

    def wait_idle(self, quiet=0.3, limit=10):
        """Until every command is answered and the line goes quiet."""
        end = time.monotonic() + limit
        while time.monotonic() < end:
            n = len(self.lines)
            self.pump(quiet)
            if not self.queue and self.current is None and len(self.lines) == n:
                return
            if self.exited:
                return

    def answer(self, cmd_prefix):
        for cmd, ok, body in reversed(self.done):
            if cmd.startswith(cmd_prefix):
                return ok, body
        return None, []

    def wait_note(self, prefix, limit=5):
        end = time.monotonic() + limit
        while time.monotonic() < end:
            for n in self.notes:
                if n.startswith(prefix):
                    return n
            self.pump(0.1)
        return None


def panes_in_layout(layout):
    """Pane ids in a tmux layout string, depth-first (TmuxLayoutParser)."""
    return [int(p) for p in re.findall(r"\d+x\d+,\d+,\d+,(\d+)", layout)]


def pane_requests(c, wp, version):
    """TmuxWindowOpener -appendRequestsForWindowPane:."""
    n = "N" if version >= 3.1 else ""
    return [
        f'capture-pane -peqJ{n} -t "%{wp}" -S -{MAX_HISTORY}',
        f'capture-pane -peqJ{n} -a -t "%{wp}" -S -{MAX_HISTORY}',
        f'list-panes -t "%{wp}" -F "{STATE_FMT}"',
        f'capture-pane -p -P -C -t "%{wp}"',
        f"refresh-client -A '%{wp}:continue'",  # pause mode is on (>= 3.2)
        f"show-options -v -q -p -t %{wp} @uservars",  # >= 3.1
    ]


def attach_like_iterm2(c, label):
    """PTYSession -startTmuxMode / -kickOffTmuxForRestoration, TmuxController
    -guessVersion, -didGuessVersion, -openWindowsInitial, -openWindowsOfSize,
    -initialListWindowsResponse, TmuxWindowOpener -openWindows:."""
    TX.write(f"\n# ===== {label}: wait for tmux's unsolicited %begin/%end and %session-changed\n")
    # iTerm2 waits for the first %end/%error and holds writes until
    # %session-changed (TmuxGateway -parseSessionChangeCommand, enableWrites).
    note = c.wait_note("%session-changed")
    sid = int(re.match(r"%session-changed \$(\d+)", note).group(1))
    c.wait_idle()

    TX.write("# iTerm2 writes ^C when it sees the DCS, then the phony command\n")
    c.tx("\x03")
    c.send("phony-command", note="kickOffTmuxForRestoration (each command its own line, pipelined)")
    c.send("refresh-client -fpause-after=0,wait-exit")       # ping
    c.send("show-window-options -g aggressive-resize")       # validateOptions
    c.send("show-option -g -v status")
    c.send('list-sessions -F "\t"')                          # checkForUTF8 (a real tab)
    c.send("show-options -v -s default-terminal")            # loadDefaultTerminal
    c.send("list-keys")                                      # loadKeyBindings
    c.send("copy-mode -q")                                   # exitCopyMode
    c.send('display-message -p "#{version}"', note="guessVersion")
    c.send("show-window-options pane-border-format")
    c.send('list-windows -F "#{socket_path}"')
    c.send('list-windows -F "#{pid}"')
    c.send("show-options -g message-style")
    c.wait_idle()
    ok, body = c.answer('display-message -p "#{version}"')
    version = float(re.match(r"[\d.]+", body[0]).group(0)) if ok and body else 0
    TX.write(f"# version {body} -> min version {version}; variable window sizes and pause mode on\n")

    c.send("refresh-client -fpause-after=120", note="didGuessVersion: enablePauseModeIfPossible (age = TmuxPauseModeAgeLimit default 120)")
    c.send('display-message -p "#{socket_path},#{pid}"')     # loadServerPID
    c.send("display-message -p '#{client_name}'")             # loadTmuxClientName
    c.send("show-options -v -g set-titles")                   # loadTitleFormat (a list of one)
    c.wait_idle()
    ok, body = c.answer("display-message -p '#{client_name}'")
    client_name = body[0] if body else "?"

    # openWindowsInitial (>= 3.6): client tracker, set-clipboard monitor.
    c.send(f"list-clients -t '${sid}' -F '#{{client_name}}\t#{{client_control_mode}}'",
           note="openWindowsInitial: client tracker (3.6+), set-clipboard monitor (3.6+), @iterm2_size")
    c.send("refresh-client -B 'it2_1::#{T:set-clipboard}'")
    c.send("display-message -t '' -p '#{T:set-clipboard}'")
    c.send(f"show -v -q -t ${sid} @iterm2_size")
    c.wait_idle()
    ok, body = c.answer(f"show -v -q -t ${sid} @iterm2_size")
    cols, rows = CLIENT_COLS, CLIENT_ROWS
    if ok and body and re.match(r"^\d+,\d+$", body[0]):
        cols, rows = map(int, body[0].split(","))

    c.send(
        f"show -v -q -t ${sid} @iterm2_id",
        f"refresh-client -C {cols},{rows}",
        f"show -v -q -t ${sid} @hidden",
        f"show -v -q -t ${sid} @buried_indexes",
        f"show -v -q -t ${sid} @affinities",
        f"show -v -q -t ${sid} @per_window_settings",
        f"show -v -q -t ${sid} @per_tab_settings",
        f"show -v -q -t ${sid} @origins",
        f"show -v -q -t ${sid} @hotkeys",
        f"show -v -q -t ${sid} @tab_colors",
        'list-sessions -F "#{session_id} #{session_name}"',
        f"list-windows -F {DETAILED}",
        note="openWindowsOfSize: one command list, one line",
    )
    c.wait_idle()
    ok, body = c.answer(f"show -v -q -t ${sid} @iterm2_id")
    if not body or not body[0]:
        c.send(f'set -t ${sid} @iterm2_id "{str(uuid.uuid4()).upper()}"', note="no @iterm2_id yet: claim the session")
    ok, body = c.answer("list-windows -F \"#{session_name}")
    windows = []
    for row in body:
        f = row.split("\t")
        windows.append((int(f[1][1:]), f[5]))
    for i, (wid, layout) in enumerate(windows):
        reqs = []
        for wp in panes_in_layout(layout):
            reqs += pane_requests(c, wp, version)
        c.send(*reqs, note=f"TmuxWindowOpener for @{wid}: per pane, depth first (the last list is 'initial'; notifications accepted after it)")
    c.wait_idle()
    # After the tabs exist iTerm2 fits each tmux window to its tab
    # (-setWindowSizes:, refresh-client -C @id:WxH on >= 3.4).
    c.send(*[f"refresh-client -C @{wid}:{cols}x{rows}" for wid, _ in windows],
           note="tabs open: size each window (variable window sizes)")
    c.wait_idle()
    return sid, version, windows


def type_keys(c, wp, text):
    """TmuxGateway -sendCodePoints: one command list per keystroke when typed."""
    def enc(ch):
        o = ord(ch)
        if ch.isascii() and (ch.isalnum() or ch in "+/):,_"):
            return f"send -lt %{wp} {ch}"
        if o < 0x20:
            return f"send -H -t %{wp} {o:02x}"
        return f"send -t %{wp} 0x{o:x}"
    for ch in text:
        c.send(enc(ch))
        c.pump(0.03)


def paste(c, wp, text):
    """A paste or fast typing: run-length grouped into one list."""
    groups, cur, kind = [], "", None
    def k(ch):
        if ch.isascii() and (ch.isalnum() or ch in "+/):,_"):
            return "l"
        return "H" if ord(ch) < 0x20 else "x"
    for ch in text:
        if k(ch) != kind and cur:
            groups.append((kind, cur)); cur = ""
        kind = k(ch); cur += ch
    groups.append((kind, cur))
    cmds = []
    for kind, s in groups:
        if kind == "l":
            cmds.append(f"send -lt %{wp} {s}")
        elif kind == "H":
            cmds.append(f"send -H -t %{wp} " + " ".join(f"{ord(ch):02x}" for ch in s))
        else:
            cmds.append(f"send -t %{wp} " + " ".join(f"0x{ord(ch):x}" for ch in s))
    c.send(*cmds)


def on_layout_change(c, version, known):
    """TmuxController layout change -> TmuxWindowOpener -updateLayoutInTab:
    capture any pane iTerm2 has not seen."""
    new = []
    for n in c.notes:
        m = re.match(r"^%layout-change @(\d+) (\S+)", n)
        if m:
            for wp in panes_in_layout(m.group(2)):
                if wp not in known:
                    known.add(wp); new.append(wp)
    reqs = []
    for wp in new:
        reqs += pane_requests(c, wp, version)
    if reqs:
        c.send(*reqs, note=f"%layout-change shows new pane(s) {new}: fetch their state")
        c.wait_idle()


def main():
    os.makedirs(WORK, exist_ok=True)
    global RAW, TX
    RAW = open(os.path.join(WORK, "raw.log"), "w")
    TX = open(os.path.join(WORK, "transcript.txt"), "w")
    subprocess.run(["tmux", "-L", SOCK, "kill-server"], stderr=subprocess.DEVNULL)

    # ---- first attach: `tmux -CC new -s s11`
    c = Client(["tmux", "-L", SOCK, "-f", CONF, "-CC", "new", "-s", "s11"], "A")
    sid, version, windows = attach_like_iterm2(c, "attach 1: tmux -CC new -s s11")
    known = set()
    for _, layout in windows:
        known.update(panes_in_layout(layout))
    wid, p0 = windows[0][0], panes_in_layout(windows[0][1])[0]
    c.notes.clear()

    # ---- split (Shell > Split Vertically): list-panes, split, list-panes
    TX.write("\n# ===== split vertically (iTerm2 'vertical' = tmux -h)\n")
    c.send(f"list-panes -t %{p0} -F '#{{pane_id}}'", note="-splitWindowPane: panes before")
    c.send(f'split-window -h -t "%{p0}"')
    c.send(f"list-panes -t %{p0} -F '#{{pane_id}}'")
    c.wait_idle()
    on_layout_change(c, version, known)
    p1 = max(known)
    c.notes.clear()

    # ---- drag the divider: resize-pane + list-windows
    TX.write("\n# ===== drag the divider 5 cells right\n")
    c.send(f'resize-pane -R -t "%{p0}" 5', LIST_WINDOWS)
    c.wait_idle()

    # ---- iTerm2 window resized: refresh-client -C @w:WxH
    TX.write("\n# ===== the iTerm2 window gets smaller\n")
    c.send(f"refresh-client -C @{wid}:100x30")
    c.wait_idle()

    # ---- typing, keystroke by keystroke
    TX.write("\n# ===== type 'echo hi there' + Return into the new pane, key by key\n")
    type_keys(c, p1, "echo hi there\r")
    c.wait_idle()

    # ---- flow control by hand: pause and continue
    TX.write("\n# ===== pause/continue a pane by hand (refresh-client -A)\n")
    c.send(f"refresh-client -A '%{p1}:pause'", note="-pausePanes:")
    c.wait_idle()
    paste(c, p1, "seq 1 3\r")
    c.wait_idle()
    c.send(*pane_requests(c, p1, version), note="-unpausePanes: refetch history then continue")
    c.wait_idle()

    # ---- vi (vim-tiny; vim itself is not installed), left running across detach
    TX.write("\n# ===== run vi (vim-tiny) in the first pane, type a line, leave it open\n")
    paste(c, p0, "vi -u NONE -N /tmp/s11-vim.txt\r")
    c.pump(1.0)
    c.wait_idle()
    type_keys(c, p0, "ihello from s11\x1b")
    c.wait_idle()

    # ---- new window (tab) and close it
    TX.write("\n# ===== new tab, then close it\n")
    c.notes.clear()
    c.send(f"new-window -PF '#{{window_id}}' -a -t \"${sid}:+\"", note="-newWindowWithAffinity (iTermInitialDirectory+Tmux)")
    c.wait_idle()
    ok, body = c.answer("new-window")
    w2 = int(body[0][1:])
    if c.wait_note("%window-add"):
        c.send(f"display -p -F {DETAILED} -t @{w2}", note="%window-add -> -openWindowWithId:")
        c.wait_idle()
        ok, body = c.answer("display -p -F")
        layout = body[0].split("\t")[5]
        reqs = []
        for wp in panes_in_layout(layout):
            known.add(wp); reqs += pane_requests(c, wp, version)
        c.send(*reqs, note="TmuxWindowOpener for the new window")
        c.send(f"refresh-client -C @{w2}:{CLIENT_COLS}x{CLIENT_ROWS}")
        c.wait_idle()
    c.send(f"kill-window -t @{w2}", note="close the tab (-killWindow:)")
    c.wait_idle()

    # ---- detach
    TX.write("\n# ===== detach (Esc in the gateway, or Shell > tmux > Detach)\n")
    c.send("detach")
    c.wait_idle(limit=3)
    c.tx(NL, note="%exit: iTerm2 answers with an empty line on >= 3.2 (wait-exit), then tmux ends the DCS")
    c.pump(0.5)
    os.close(c.fd)
    os.waitpid(c.pid, 0)

    # ---- re-attach: vim must still be there, on the alternate screen
    c2 = Client(["tmux", "-L", SOCK, "-f", CONF, "-CC", "attach", "-t", "s11"], "B")
    attach_like_iterm2(c2, "attach 2: tmux -CC attach -t s11")
    TX.write("\n# ===== quit vi in the reattached client\n")
    paste(c2, p0, "\x1b:q!\r")
    c2.wait_idle()
    paste(c2, p0, "exit\r")
    c2.pump(1.0)
    c2.wait_idle()
    TX.write("\n# ===== detach again\n")
    c2.send("detach")
    c2.wait_idle(limit=3)
    c2.tx(NL, note="%exit: empty line (wait-exit)")
    c2.pump(0.5)
    os.close(c2.fd)
    os.waitpid(c2.pid, 0)

    subprocess.run(["tmux", "-L", SOCK, "kill-server"], stderr=subprocess.DEVNULL)
    RAW.close(); TX.close()
    print("wrote", os.path.join(WORK, "transcript.txt"))


if __name__ == "__main__":
    main()
