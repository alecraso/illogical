#!/usr/bin/env python3
"""S16: a first cut of M23's `kind` and `project`, checked two ways.

1. A fixture of commands people run in panes, each labelled by hand.
2. The real history on geek and jake-mini (`illogical history --json`,
   read-only, saved under work/ by hand; never committed): what the
   heuristic says, and how much of it lands in a project.

    python3 spikes/s16-swarm/classify.py [work/history-geek.json ...]
"""

import collections
import json
import os
import re
import subprocess
import sys

AGENTS = {"claude", "codex", "aider", "gemini", "opencode", "goose", "amp", "cursor-agent"}
EDITORS = {"vim", "nvim", "vi", "hx", "helix", "emacs", "nano", "micro", "kak"}
LOGS = {"tail", "journalctl", "less", "kubectl logs", "docker logs", "fly logs", "flyctl logs", "stern", "lnav"}
SERVERS = re.compile(r"\b(run dev|serve|server|uvicorn|gunicorn|rails s|runserver|vite(?! build)|next dev|"
                     r"illogicald --foreground|caddy run|nginx|watch|cargo watch|air\b|npm start|pnpm dev|hugo server)\b")
TESTS = re.compile(r"\b(test|tests|pytest|nextest|jest|vitest|playwright test|go test|rspec|bats|ctest)\b")
BUILDS = re.compile(r"\b(build|make|ninja|cmake|clippy|check|tsc|webpack|esbuild|bazel|gradle|mvn|zig build|"
                    r"docker build|just build|cargo b\b)\b")
WRAPPERS = {"sudo", "time", "nice", "env", "nohup", "uv", "npx", "pnpm", "npm", "yarn", "bunx", "exec"}


def kind(argv0_text: str) -> str:
    """kind from a command line: argv from /proc when we have it, else the
    text shell integration reported."""
    t = argv0_text.strip()
    if not t:
        return "shell"
    words = t.split()
    first = words[0].rsplit("/", 1)[-1]
    # Look through wrappers (`uv run`, `npx`, `sudo`).
    i = 0
    while i < len(words) - 1 and words[i].rsplit("/", 1)[-1] in WRAPPERS:
        i += 1
        if words[i] == "run":
            i = min(i + 1, len(words) - 1)
    head = words[i].rsplit("/", 1)[-1]
    two = " ".join(w.rsplit("/", 1)[-1] for w in words[i:i + 2])
    if head in AGENTS or first in AGENTS or any(head.startswith(a + "-") for a in AGENTS):
        return "agent"
    if head in EDITORS:
        return "editor"
    if two in LOGS or (head in LOGS and ("-f" in words or "-F" in words or head == "journalctl")):
        return "logs"
    if SERVERS.search(t):
        return "server"
    if TESTS.search(t):
        return "test"
    if BUILDS.search(t):
        return "build"
    return "shell"


def project(cwd):
    """The git root's name, with no `git` subprocess: walk up for `.git`."""
    if not cwd:
        return None
    p = cwd
    while p and p != "/":
        if os.path.exists(os.path.join(p, ".git")):
            return os.path.basename(p)
        p = os.path.dirname(p)
    return None


FIXTURE = [
    ("cargo build --release", "build"), ("cargo build -p illogical-control", "build"),
    ("cargo clippy --all-targets -- -D warnings", "build"), ("just build", "build"), ("make -j32", "build"),
    ("npm run build", "build"), ("pnpm run build", "build"), ("vite build", "build"), ("tsc --noEmit", "build"),
    ("docker build -t api .", "build"), ("zig build", "build"),
    ("cargo test", "test"), ("cargo nextest run", "test"), ("npx playwright test passkey", "test"),
    ("pytest -x", "test"), ("uv run pytest -k slots", "test"), ("go test ./...", "test"), ("pnpm vitest", "test"),
    ("just test", "test"), ("bats tests/", "test"),
    ("claude", "agent"), ("claude --continue", "agent"), ("claude --dangerously-skip-permissions", "agent"),
    ("codex", "agent"), ("aider --model sonnet", "agent"), ("/home/user/.local/bin/claude", "agent"),
    ("npm run dev", "server"), ("pnpm dev", "server"), ("uvicorn app:main --reload", "server"),
    ("python manage.py runserver", "server"), ("illogicald --foreground", "server"), ("hugo server", "server"),
    ("cargo watch -x check", "server"), ("caddy run", "server"),
    ("tail -f /var/log/caddy.log", "logs"), ("journalctl -fu illogicald", "logs"), ("kubectl logs -f deploy/api", "logs"),
    ("fly logs -a illogical-control", "logs"), ("docker logs -f web", "logs"),
    ("nvim src/main.rs", "editor"), ("vim /tmp/x.txt", "editor"), ("hx .", "editor"), ("emacs -nw", "editor"),
    ("", "shell"), ("git status", "shell"), ("ls -la", "shell"), ("ssh geek", "shell"), ("htop", "shell"),
    ("git push origin main", "shell"), ("sudo apt upgrade", "shell"), ("python3", "shell"),
    # Known misses the heuristic gets wrong, kept to show its limits:
    ("c", "agent"),                       # an alias for claude: typed text hides it, /proc argv shows it
    ("./scripts/release.sh", "build"),    # a script's name says little
    ("npm start", "server"),
]


def main():
    wrong = [(c, want, kind(c)) for c, want in FIXTURE if kind(c) != want]
    print(f"fixture: {len(FIXTURE) - len(wrong)}/{len(FIXTURE)} right")
    for c, want, got in wrong:
        print(f"  {c!r}: want {want}, got {got}")
    for f in sys.argv[1:]:
        rows = json.load(open(f))
        kinds = collections.Counter(kind(r.get("text") or "") for r in rows)
        projects = collections.Counter(project(r.get("cwd")) for r in rows)
        with_project = sum(n for p, n in projects.items() if p)
        print(f"{os.path.basename(f)}: {len(rows)} commands; kinds {dict(kinds)}; "
              f"in a git project {with_project}/{len(rows)}; projects {dict((k, v) for k, v in projects.items() if k)}")


if __name__ == "__main__":
    main()
