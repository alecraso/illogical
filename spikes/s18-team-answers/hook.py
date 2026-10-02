#!/usr/bin/env python3
"""S18: one hook script for every event. usage: hook.py <mode> [arg]

Records stdin to work/hooks/<n>-<event>-<mode>.json, the reply to .out, and timings to .log.
modes (by event):
  record          exit 0, no output: Claude Code carries on as if there were no hook
  allow           PermissionRequest: {"behavior": "allow"}
  always          PermissionRequest: allow, and apply the first permission suggestion (updatedPermissions)
  deny [msg]      PermissionRequest: {"behavior": "deny", "message": msg}
  wait            block until work/hooks/release exists; its text picks the reply (allow|deny|always|exit0)
  pre-allow       PreToolUse: permissionDecision allow
  pre-ask         PreToolUse: permissionDecision ask (forces the dialog)
  queue           Stop: if work/queue.txt has text, block with it as the reason (a queued follow-up), and empty it
  inbox           Stop with asyncRewake: wait in the background for work/queue.txt, then exit 2 with it (wakes the model)
  submit-add      UserPromptSubmit: add work/queue.txt as additionalContext
"""
import json, os, sys, time, signal
HERE = os.path.dirname(os.path.abspath(__file__))
D = os.path.join(HERE, "work", "hooks"); os.makedirs(D, exist_ok=True)
mode = sys.argv[1]
raw = sys.stdin.read()
try:
    inp = json.loads(raw)
except Exception:
    inp = {}
ev = inp.get("hook_event_name", "unknown")
n = len([f for f in os.listdir(D) if f.endswith(".json")])
tag = f"{n:02d}-{ev}-{mode}"
open(os.path.join(D, tag + ".json"), "w").write(raw)
log = open(os.path.join(D, tag + ".log"), "a")
log.write(f"{time.time():.2f} start pid={os.getpid()} ppid={os.getppid()}\n"); log.flush()


def bye(sig, _):
    log.write(f"{time.time():.2f} signal {sig}\n"); log.flush(); sys.exit(1)


for s in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
    signal.signal(s, bye)


def reply(out):
    open(os.path.join(D, tag + ".out"), "w").write(json.dumps(out))
    log.write(f"{time.time():.2f} reply {json.dumps(out)[:300]}\n"); log.flush()
    print(json.dumps(out)); sys.exit(0)


def perm(decision):
    return {"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}}


if mode == "record":
    log.write("record exit 0\n"); sys.exit(0)
if mode == "wait":
    rel = os.path.join(D, "release")
    while not os.path.exists(rel):
        time.sleep(0.25)
    what = open(rel).read().strip() or "allow"
    os.unlink(rel)
    log.write(f"{time.time():.2f} released: {what}\n"); log.flush()
    if what == "exit0":
        sys.exit(0)
    mode = what
if mode == "allow":
    reply(perm({"behavior": "allow"}))
if mode == "always":
    sugg = inp.get("permission_suggestions") or []
    reply(perm({"behavior": "allow", "updatedPermissions": sugg[:1]}))
if mode == "deny":
    msg = sys.argv[2] if len(sys.argv) > 2 else "Sam said no: do a dry run first."
    reply(perm({"behavior": "deny", "message": msg}))
if mode == "pre-allow":
    reply({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "permissionDecisionReason": "allowed by a teammate"}})
if mode == "pre-ask":
    reply({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "ask", "permissionDecisionReason": "a teammate wants you to look"}})
q = os.path.join(HERE, "work", "queue.txt")
if mode == "queue":
    text = open(q).read().strip() if os.path.exists(q) else ""
    if text:
        open(q, "w").close()
        reply({"decision": "block", "reason": f"A follow-up from Sam (sent through illogical): {text}"})
    log.write("nothing queued\n"); sys.exit(0)
if mode == "inbox":
    # Stop hook with asyncRewake: runs in the background after every turn and waits for a follow-up.
    # The newest waiter owns work/inbox.pid; an older one exits quietly when it sees it lost.
    pidf = os.path.join(HERE, "work", "inbox.pid")
    open(pidf, "w").write(str(os.getpid()))
    while True:
        try:
            if int(open(pidf).read().strip() or 0) != os.getpid():
                log.write(f"{time.time():.2f} superseded\n"); sys.exit(0)
        except (OSError, ValueError):
            pass
        text = open(q).read().strip() if os.path.exists(q) else ""
        if text:
            open(q, "w").close()
            log.write(f"{time.time():.2f} wake: {text}\n"); log.flush()
            sys.stderr.write(f"A follow-up from Sam (sent through illogical): {text}\n")
            sys.exit(2)
        time.sleep(0.25)
if mode == "submit-add":
    text = open(q).read().strip() if os.path.exists(q) else ""
    if text:
        open(q, "w").close()
        reply({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": f"A follow-up from Sam (sent through illogical): {text}"}})
    sys.exit(0)
log.write(f"unknown mode {mode}\n"); sys.exit(0)
