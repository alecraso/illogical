#!/usr/bin/env python3
"""S11: illogical split tree (weights) <-> tmux layout string.

`to_tmux(tree, cols, rows)` derives cells exactly as crates/core/src/layout.rs
does (distribute(): floor, largest remainder, one-cell dividers) and writes
tmux's v1 layout string with layout-custom.c's checksum. `from_tmux(s)`
parses one back into a tree whose weights reproduce the same cells.

Trees use illogical's JSON shape:
  {"type": "pane", "pane": 3}
  {"type": "split", "id": 1, "dir": "row"|"column",
   "children": [{"weight": 0.5, "node": {...}}, ...]}
row = side by side = tmux {...} (LEFTRIGHT); column = stacked = tmux [...].

Run it to self-test, and with --tmux to check every string against a real
tmux (select-layout, then read #{window_layout} back) on the illogical-s11
socket.
"""

import itertools
import math
import random
import re
import subprocess
import sys


def distribute(total, weights):
    """Port of crates/core/src/layout.rs distribute()."""
    n = len(weights)
    if n == 0:
        return []
    avail = max(total - (n - 1), n)
    s = sum(weights)
    exact = [w / s * avail for w in weights]
    sizes = [max(math.floor(e), 1) for e in exact]
    # Rust's sort_by is stable; ties keep index order.
    order = sorted(range(n), key=lambda i: -(exact[i] - math.floor(exact[i])))
    used, i = sum(sizes), 0
    while used < avail:
        sizes[order[i % n]] += 1
        used += 1
        i += 1
    while used > avail:
        big = max(j for j in range(n) if sizes[j] == max(sizes))  # Rust max_by_key: last max
        if sizes[big] <= 1:
            break
        sizes[big] -= 1
        used -= 1
    return sizes


def checksum(body):
    """layout-custom.c layout_checksum()."""
    csum = 0
    for ch in body.encode():
        csum = (csum >> 1) + ((csum & 1) << 15)
        csum = (csum + ch) & 0xFFFF
    return f"{csum:04x}"


def _body(node, x, y, cols, rows):
    if node["type"] == "pane":
        return f"{cols}x{rows},{x},{y},{node['pane']}"
    row = node["dir"] == "row"
    ext = distribute(cols if row else rows, [c["weight"] for c in node["children"]])
    parts, at = [], 0
    for child, n in zip(node["children"], ext):
        if row:
            parts.append(_body(child["node"], x + at, y, n, rows))
        else:
            parts.append(_body(child["node"], x, y + at, cols, n))
        at += n + 1
    o, c = ("{", "}") if row else ("[", "]")
    return f"{cols}x{rows},{x},{y}{o}{','.join(parts)}{c}"


def to_tmux(tree, cols, rows):
    body = _body(tree, 0, 0, cols, rows)
    return f"{checksum(body)},{body}"


def from_tmux(layout):
    """Parse a v1 layout string. Weights are the cell extents (normalized),
    so to_tmux(from_tmux(s), W, H) == s for the same W, H."""
    m = re.match(r"^([0-9a-f]{4}),(.*)$", layout)
    if not m or checksum(m.group(2)) != m.group(1):
        raise ValueError(f"bad checksum: {layout}")
    s, i = m.group(2), 0
    ids = itertools.count(1)

    def cell():
        nonlocal i
        mm = re.compile(r"(\d+)x(\d+),(\d+),(\d+)(?:,(\d+))?").match(s, i)
        i = mm.end()
        w, h = int(mm.group(1)), int(mm.group(2))
        if mm.group(5) is not None:
            return {"type": "pane", "pane": int(mm.group(5))}, w, h
        o = s[i]
        i += 1
        kids = []
        while True:
            node, cw, ch = cell()
            kids.append((node, cw if o == "{" else ch))
            if s[i] == ",":
                i += 1
                continue
            i += 1  # closing bracket
            break
        total = sum(e for _, e in kids)
        return {"type": "split", "id": next(ids), "dir": "row" if o == "{" else "column",
                "children": [{"weight": e / total, "node": n} for n, e in kids]}, w, h

    node, w, h = cell()
    return node, w, h


def fits(layout):
    """True if every split's children plus one-cell dividers fill it exactly.
    illogical's distribute() gives each child at least one cell, so a tab
    smaller than its tree overflows (layout.rs: "the last children overflow
    and get clipped by the client"); tmux refuses such a layout."""
    t, w, h = from_tmux(layout)
    ok = True
    def go(node, cols, rows):
        nonlocal ok
        if node["type"] == "pane":
            return
        row = node["dir"] == "row"
        ext = distribute(cols if row else rows, [c["weight"] for c in node["children"]])
        if sum(ext) + len(ext) - 1 != (cols if row else rows):
            ok = False
        for c, e in zip(node["children"], ext):
            go(c["node"], e if row else cols, rows if row else e)
    go(t, w, h)
    return ok


def pane(p):
    return {"type": "pane", "pane": p}


def split(d, *kids):
    return {"type": "split", "id": 0, "dir": d,
            "children": [{"weight": w, "node": n} for w, n in kids]}


def label(tree):
    """Renumber panes 0..n-1 in reading order (tmux assigns panes to cells in order)."""
    c = itertools.count()
    def go(n):
        if n["type"] == "pane":
            return pane(next(c))
        return {**n, "children": [{"weight": k["weight"], "node": go(k["node"])} for k in n["children"]]}
    return go(tree)


def npanes(tree):
    return 1 if tree["type"] == "pane" else sum(npanes(k["node"]) for k in tree["children"])


def random_tree(rng, depth=0, d=None):
    if depth > 2 or (depth > 0 and rng.random() < 0.4):
        return pane(0)
    d = rng.choice(["row", "column"]) if d is None else d
    other = "column" if d == "row" else "row"
    n = rng.randint(2, 3)
    ws = [rng.uniform(0.1, 1) for _ in range(n)]
    s = sum(ws)
    return split(d, *[(w / s, random_tree(rng, depth + 1, other)) for w in ws])


EXAMPLES = [
    ("one pane", pane(0), 120, 40),
    ("50/50 side by side", split("row", (0.5, pane(0)), (0.5, pane(1))), 120, 40),
    ("50/50 side by side, odd width", split("row", (0.5, pane(0)), (0.5, pane(1))), 81, 25),
    ("left + right stacked (layout.rs test)",
     split("row", (0.5, pane(1)), (0.5, split("column", (0.5, pane(2)), (0.5, pane(3))))), 81, 25),
    ("thirds 0.5/0.25/0.25", split("row", (0.5, pane(0)), (0.25, pane(1)), (0.25, pane(2))), 100, 30),
    ("S11 transcript after drag (65|54)", split("row", (65 / 119, pane(0)), (54 / 119, pane(1))), 120, 40),
]


def self_test():
    print("examples:")
    for name, tree, w, h in EXAMPLES:
        s = to_tmux(tree, w, h)
        print(f"  {name:42} {w}x{h}  {s}")
    # The transcript's own strings parse and re-derive byte for byte.
    for s in ["aafd,120x40,0,0,0",
              "f91d,120x40,0,0{60x40,0,0,0,59x40,61,0,1}",
              "2a7e,120x40,0,0{65x40,0,0,0,54x40,66,0,1}",
              "9ceb,100x30,0,0{55x30,0,0,0,44x30,56,0,1}"]:
        t, w, h = from_tmux(s)
        assert to_tmux(t, w, h) == s, (s, to_tmux(t, w, h))
    print("  transcript layouts round-trip: ok")
    # Round trip on random trees and sizes: parse(derive(tree)) re-derives
    # the same string, i.e. cell extents -> weights -> cells is exact.
    rng = random.Random(11)
    n = 0
    for _ in range(3000):
        t = label(random_tree(rng))
        w, h = rng.randint(20, 400), rng.randint(10, 150)
        s = to_tmux(t, w, h)
        t2, w2, h2 = from_tmux(s)
        assert (w2, h2) == (w, h) and to_tmux(t2, w, h) == s, s
        n += 1
    print(f"  {n} random trees: derive -> parse -> derive is identical")


def tmux(*args):
    return subprocess.run(["tmux", "-L", "illogical-s11", "-f", "/dev/null", *args],
                          capture_output=True, text=True)


def against_tmux():
    """Feed derived strings to tmux and read them back."""
    rng = random.Random(5)
    cases = [(n, t, w, h) for n, t, w, h in EXAMPLES]
    for k in range(60):
        cases.append((f"random {k}", label(random_tree(rng)), rng.randint(40, 300), rng.randint(15, 100)))
    ok = overflow = 0
    for name, tree, w, h in cases:
        tree = label(tree)
        tmux("kill-server")
        for _ in range(5):
            if tmux("new", "-d", "-s", "lay", "-x", str(w), "-y", str(h), "cat").returncode == 0:
                break
        tmux("set", "-g", "window-size", "manual")
        tmux("resize-window", "-t", "lay:0", "-x", str(w), "-y", str(h))
        for k in range(npanes(tree) - 1):
            # Split the last pane in the list so list order stays id order.
            tmux("select-layout", "-t", "lay:0", "tiled")
            tmux("split-window", "-d", "-t", f"lay:0.{k}", "cat")
        want = to_tmux(tree, w, h)
        r = tmux("select-layout", "-t", "lay:0", want)
        got = tmux("display", "-p", "-t", "lay:0", "#{window_layout}").stdout.strip()
        ids = tmux("list-panes", "-t", "lay:0", "-F", "#{pane_id}").stdout.split()
        # tmux puts its own pane ids in; ours are 0..n-1 in reading order.
        mapped = want
        if ids != [f"%{i}" for i in range(len(ids))]:
            print(f"  {name}: pane ids {ids}, comparing shape only")
        same = re.sub(r"^[0-9a-f]{4},", "", got) == re.sub(r"^[0-9a-f]{4},", "", mapped)
        if r.returncode == 0 and got == want:
            ok += 1
        elif not fits(want):
            overflow += 1
            print(f"  overflow (tab too small for its tree; tmux refuses: {r.stderr.strip()[:40]}...) {name} {w}x{h}")
        else:
            print(f"  MISMATCH {name} {w}x{h}\n    want {want}\n    got  {got}\n    rc={r.returncode} {r.stderr.strip()} same-body={same}")
    tmux("kill-server")
    print(f"against tmux {subprocess.run(['tmux', '-V'], capture_output=True, text=True).stdout.strip()}: "
          f"{ok}/{len(cases)} accepted by select-layout and read back byte for byte, "
          f"{overflow} overflowing (expected refusals), {len(cases) - ok - overflow} other failures")


if __name__ == "__main__":
    self_test()
    if "--tmux" in sys.argv:
        against_tmux()
