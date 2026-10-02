#!/usr/bin/env python3
"""S17: what nvim can report cheaply, and how often, while someone types.

usage: work/venv/bin/python nvim_events.py [cps] [seconds] > work/nvim-<cps>.json

- Starts `nvim --embed --headless --clean` (0.12) with pylsp (pyflakes + pycodestyle, from work/venv)
  as a real language server, on work/nvproj/app.py.
- A Lua recorder on autocmds (TextChanged[I], CursorMoved[I], ModeChanged, BufEnter, WinScrolled,
  DiagnosticChanged, BufModifiedSet, BufWritePost) snapshots what an editor presence would carry:
  file, cursor, mode, visual selection, visible range (w0/w$), diagnostic counts, unsaved buffers. Each
  snapshot's own cost is timed.
- A driver types Python at <cps> characters a second (jittered), with bursts of normal-mode motion,
  a visual selection, scrolling, and a save every ~20 s, for <seconds>.
- Saves the raw events to work/nvim-events-<cps>.json, then replays them through reporter policies
  (replay.py: every event, trailing debounce, throttles up to M23's 1 s tick) and counts messages,
  bytes and added delay for the summary and follow streams.
"""
import json, os, random, sys, time

import pynvim

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(HERE, "work")
CPS = float(sys.argv[1]) if len(sys.argv) > 1 else 8
SECS = float(sys.argv[2]) if len(sys.argv) > 2 else 60
random.seed(17)

proj = os.path.join(WORK, "nvproj")
os.makedirs(proj, exist_ok=True)
path = os.path.join(proj, "app.py")
with open(path, "w") as f:
    f.write("\n".join(f"# line {i}" for i in range(1, 121)) + "\n")

LUA = r"""
_G.S17 = {ev = {}, cost = {}}
local t0 = vim.uv.hrtime()
local function snap(ev)
  local c0 = vim.uv.hrtime()
  local buf = vim.api.nvim_get_current_buf()
  local cur = vim.api.nvim_win_get_cursor(0)
  local mode = vim.api.nvim_get_mode().mode
  local sel = vim.NIL
  if mode:match('^[vV\22]') then local v = vim.fn.getpos('v'); sel = {v[2], v[3] - 1} end
  local n = {0, 0, 0, 0}
  for _, d in ipairs(vim.diagnostic.get(buf)) do n[d.severity] = n[d.severity] + 1 end
  local dirty = 0
  for _, b in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(b) and vim.bo[b].modified then dirty = dirty + 1 end
  end
  local s = {t = (c0 - t0) / 1e6, ev = ev, file = vim.api.nvim_buf_get_name(buf), line = cur[1], col = cur[2],
    mode = mode, sel = sel, top = vim.fn.line('w0'), bot = vim.fn.line('w$'), e = n[1], w = n[2], i = n[3] + n[4], dirty = dirty, text = vim.api.nvim_get_current_line()}
  table.insert(S17.ev, s)
  table.insert(S17.cost, (vim.uv.hrtime() - c0) / 1e3)
end
for _, ev in ipairs({'TextChanged', 'TextChangedI', 'CursorMoved', 'CursorMovedI', 'ModeChanged', 'BufEnter',
    'WinScrolled', 'DiagnosticChanged', 'BufModifiedSet', 'BufWritePost'}) do
  vim.api.nvim_create_autocmd(ev, {callback = function() snap(ev) end})
end
"""

nv = pynvim.attach("child", argv=["nvim", "--embed", "--headless", "--clean", "-n", "-i", "NONE"])
nv.exec_lua("vim.o.lines = 40; vim.o.columns = 120; vim.o.swapfile = false")
nv.exec_lua(LUA)
nv.command(f"edit {path}")
pylsp = os.path.join(WORK, "venv/bin/pylsp")
nv.exec_lua(f"vim.lsp.start({{name = 'pylsp', cmd = {{'{pylsp}'}}, root_dir = '{proj}'}})")
# Wait for the server's first diagnostics (pycodestyle has opinions about "# line" files).
for _ in range(100):
    if nv.exec_lua("return #vim.lsp.get_clients()") and any(e["ev"] == "DiagnosticChanged" for e in nv.exec_lua("return S17.ev")):
        break
    time.sleep(0.1)
nv.exec_lua("S17.ev = {}; S17.cost = {}")
nv.input("G")

CODE = '''
def parse_line(text):
    head, _, rest = text.partition(":")
    if not rest:
        return None
    return head.strip(), rest.strip()


class Store:
    def __init__(self, path):
        self.path = path
        self.items = {}

    def load(self):
        with open(self.path) as f:
            for line in f:
                kv = parse_line(line)
                if kv:
                    self.items[kv[0]] = kv[1]
        return len(self.items)

    def get(self, key, default=None):
        return self.items.get(key, default)
'''

start = time.time()
typed = 0
pos = 0
last_save = start
mean = 1.0 / CPS


def gap():
    return random.uniform(0.4, 1.6) * mean


def normal_burst():
    # Leave insert mode, look around (motions, scroll, a visual selection), come back.
    nv.input("<Esc>")
    time.sleep(0.3)
    for k in random.sample(["k", "k", "j", "w", "b", "}", "{", "<C-u>", "<C-d>", "gg", "G", "5k"], 6):
        nv.input(k)
        time.sleep(random.uniform(0.15, 0.4))
    nv.input("V")
    for _ in range(3):
        time.sleep(0.2)
        nv.input("j")
    time.sleep(0.4)
    nv.input("<Esc>G")
    time.sleep(0.2)
    nv.input("o")


nv.input("o")
while time.time() - start < SECS:
    ch = CODE[pos % len(CODE)]
    pos += 1
    nv.input("<CR>" if ch == "\n" else ("<lt>" if ch == "<" else ch))
    typed += 1
    time.sleep(gap())
    if ch == "\n" and random.random() < 0.12:
        normal_burst()
    if time.time() - last_save > 20:
        nv.input("<Esc>:w<CR>o")
        last_save = time.time()
nv.input("<Esc>")
time.sleep(2)  # let the last diagnostics land
events = nv.exec_lua("return S17.ev")
cost = sorted(nv.exec_lua("return S17.cost"))
elapsed = time.time() - start
try:
    nv.command("qa!")
except Exception:
    pass  # nvim exits before answering

from replay import all_policies, rates  # noqa: E402


def norm(st):
    if st.get("file"):
        st["file"] = os.path.relpath(st["file"], proj)
    return st


with open(os.path.join(WORK, f"nvim-events-{int(CPS)}.json"), "w") as f:
    json.dump(events, f)
out = {
    "nvim": "0.12.5", "cps_target": CPS, "seconds": round(elapsed, 1), "typed_chars": typed,
    "cps_actual": round(typed / elapsed, 2),
    "events_total": len(events), "events_per_s": round(len(events) / elapsed, 2),
    "by_event_per_s": rates(events, elapsed),
    "snapshot_cost_us": {"p50": round(cost[len(cost) // 2], 1), "p99": round(cost[int(len(cost) * 0.99)], 1), "max": round(cost[-1], 1)},
    "policies": all_policies(events, elapsed, norm),
}
print(json.dumps(out, indent=1))

