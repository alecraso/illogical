#!/usr/bin/env python3
"""S18: copy recorded hook inputs into crates/daemon/tests/fixtures/, with the
machine's paths replaced the way S13's fixture has them (/work/tui, /home/user)."""
import json, os, re

HERE = os.path.dirname(os.path.abspath(__file__))
KEEP = os.path.join(HERE, "work", "keep")
OUT = os.path.join(HERE, "..", "..", "crates", "daemon", "tests", "fixtures")

PICK = {
    "s18-hook-permission.json": "rec1-hooks/02-PermissionRequest-record.json",
    "s18-hook-pretooluse-bash.json": "rec1-hooks/01-PreToolUse-record.json",
    "s18-hook-notification-permission.json": "rec1-hooks/03-Notification-record.json",
    "s18-hook-permission-accept-edits.json": "accept1-hooks/03-PermissionRequest-record.json",
    "s18-hook-permission-subagent.json": "accept1-hooks/21-PermissionRequest-record.json",
    "s18-hook-permission-ask.json": "ask1-hooks/02-PermissionRequest-record.json",
    "s18-hook-stop.json": "rec1-hooks/04-Stop-record.json",
    "s18-hook-sessionstart-resume.json": "inbox3-hooks/00-SessionStart-inbox.json",
    "s18-hook-rewake-prompt.json": "inbox2-hooks/06-UserPromptSubmit-record.json",
}


def scrub(s: str) -> str:
    s = re.sub(r"/home/me/\.claude/projects/[^/\"]+", "/home/user/.claude/projects/-work-tui", s)
    s = re.sub(r"/tmp/claude-1000/[^/\"]+", "/tmp/claude-1000/-work-tui", s)
    s = re.sub(r"/home/me/dev/jhgaylor/illogical/\.claude/worktrees/[^/\"]+/spikes/s18-team-answers/work/tui", "/work/tui", s)
    return s


for name, src in PICK.items():
    raw = open(os.path.join(KEEP, src)).read()
    j = json.loads(scrub(raw))
    open(os.path.join(OUT, name), "w").write(json.dumps(j, ensure_ascii=False) + "\n")
    assert "jake" not in json.dumps(j), name
    print(name)
