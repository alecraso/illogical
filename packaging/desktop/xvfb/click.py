#!/usr/bin/env python3
"""Click a showing item by its label in an app, through AT-SPI (m46.sh's
nautilus claim): `click.py APP LABEL [SECONDS]`. A menu that's open shows
its items to AT-SPI the way a screen reader sees them, so this finds the
right-click menu's entry without knowing where it is on the screen.
`click.py --at APP LABEL` prints where LABEL is instead ("x y", its
middle; with `--corner`, 30 px inside its bottom right), for a right-click
there. Exits 1, listing the labels it saw, when there's no such item."""
import sys
import time

import pyatspi

args = sys.argv[1:]
at = "--at" in args
corner = "--corner" in args
args = [a for a in args if a not in ("--at", "--corner")]
app_name, label = args[0], args[1]
until = time.time() + float(args[2] if len(args) > 2 else 15)


def walk(node, depth=0):
    if node is None or depth > 60:
        return
    yield node
    try:
        children = list(node)
    except Exception:
        return
    for c in children:
        yield from walk(c, depth + 1)


def showing(node):
    try:
        return node.getState().contains(pyatspi.STATE_SHOWING)
    except Exception:
        return False


seen = set()
while True:
    desktop = pyatspi.Registry.getDesktop(0)
    for app in desktop:
        if app is None or app_name not in (app.name or "").lower():
            continue
        for node in walk(app):
            name = (node.name or "").strip()
            if not name or not showing(node):
                continue
            seen.add(name)
            if name != label:
                continue
            if at:
                e = node.queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
                if e.width <= 1:
                    continue
                if corner:
                    print(e.x + e.width - 30, e.y + e.height - 30)
                else:
                    print(e.x + e.width // 2, e.y + e.height // 2)
                sys.exit(0)
            try:
                action = node.queryAction()
            except NotImplementedError:
                continue
            names = [action.getName(i) for i in range(action.nActions)]
            i = next((names.index(n) for n in ("click", "press", "activate") if n in names), 0)
            action.doAction(i)
            print(f"clicked {label!r} ({node.getRoleName()}, action {names[i] if names else i})")
            sys.exit(0)
    if time.time() > until:
        break
    time.sleep(0.5)
print(f"no showing {label!r} in {app_name}; saw: {sorted(seen)[:80]}", file=sys.stderr)
sys.exit(1)
