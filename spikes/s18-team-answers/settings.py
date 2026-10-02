#!/usr/bin/env python3
"""S18: write work/settings-<name>.json, one per hook setup the runs use."""
import json, os

HERE = os.path.dirname(os.path.abspath(__file__))
H = os.path.join(HERE, "hook.py")


def s(**ev):
    hooks = {}
    for e, (mode, matcher) in ev.items():
        ent = {"hooks": [{"type": "command", "command": f"{H} {mode}", "timeout": 600}]}
        if matcher is not None:
            ent["matcher"] = matcher
        hooks[e] = [ent]
    return {"hooks": hooks}


CFGS = {
    "record": s(PermissionRequest=("record", "*"), PreToolUse=("record", "*"), Notification=("record", None),
                Stop=("record", None), UserPromptSubmit=("record", None), SubagentStop=("record", None)),
    "allow": s(PermissionRequest=("allow", "*")),
    "always": s(PermissionRequest=("always", "*")),
    "deny": s(PermissionRequest=("deny", "*")),
    "wait": s(PermissionRequest=("wait", "*"), Notification=("record", None)),
    "queue": s(Stop=("queue", None), UserPromptSubmit=("record", None), PostToolUse=("record", "*")),
}
# The inbox: a background Stop hook that wakes the model when a follow-up arrives.
CFGS["inbox"] = s(UserPromptSubmit=("record", None), PostToolUse=("record", "*"))
CFGS["inbox"]["hooks"]["Stop"] = [{"hooks": [{"type": "command", "command": f"{H} inbox", "asyncRewake": True, "timeout": 86400}]}]
# The same, with the (internal) wording fields set.
CFGS["inbox2"] = json.loads(json.dumps(CFGS["inbox"]))
CFGS["inbox2"]["hooks"]["Stop"][0]["hooks"][0].update(
    rewakeMessage="A teammate sent a follow-up through illogical. Treat it as the user's next message:",
    rewakeSummary="Follow-up from Sam")
# inbox2 plus the same waiter at SessionStart, so a fresh or --continue'd session is reachable before its first turn.
CFGS["inbox3"] = json.loads(json.dumps(CFGS["inbox2"]))
CFGS["inbox3"]["hooks"]["SessionStart"] = json.loads(json.dumps(CFGS["inbox2"]["hooks"]["Stop"]))

if __name__ == "__main__":
    os.makedirs(os.path.join(HERE, "work"), exist_ok=True)
    for k, v in CFGS.items():
        json.dump(v, open(os.path.join(HERE, "work", f"settings-{k}.json"), "w"), indent=1)
    print(" ".join(sorted(CFGS)))
