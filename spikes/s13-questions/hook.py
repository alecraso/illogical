#!/usr/bin/env python3
"""Q5: PreToolUse hook for AskUserQuestion. usage: hook.py <mode> [arg]
Records stdin to work/hooks/<n>-<mode>.json and the reply to work/hooks/<n>-<mode>.out.
modes: answer        allow + updatedInput with answers (first option; multi: first two, comma-joined;
                     a question whose header is 'Pet' gets free text 'a parrot'; first question gets a note)
       silent        exit 0, no output (the picker should appear)
       sleep <s>     sleep s seconds, then behave like 'answer'
       wait          block until work/hooks/release exists (then 'answer'), for Ctrl-C/Esc tests
"""
import json, os, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
D = os.path.join(HERE, "work", "hooks"); os.makedirs(D, exist_ok=True)
mode = sys.argv[1]
raw = sys.stdin.read()
n = len([f for f in os.listdir(D) if f.endswith(".json")])
tag = f"{n:02d}-{mode}"
open(os.path.join(D, tag + ".json"), "w").write(raw)
log = open(os.path.join(D, tag + ".log"), "a")
log.write(f"{time.time():.1f} start pid={os.getpid()} ppid={os.getppid()}\n"); log.flush()
def bye(sig, _):
    log.write(f"{time.time():.1f} signal {sig}\n"); log.flush(); sys.exit(1)
import signal
for s in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP): signal.signal(s, bye)
if mode == "silent":
    log.write("silent exit\n"); sys.exit(0)
if mode == "sleep":
    time.sleep(float(sys.argv[2]))
if mode == "wait":
    while not os.path.exists(os.path.join(D, "release")): time.sleep(0.5)
inp = json.loads(raw)
ti = inp["tool_input"]
answers, annotations = {}, {}
for i, q in enumerate(ti.get("questions", [])):
    labels = [o["label"] for o in q.get("options", [])]
    if q.get("header") == "Pet":
        answers[q["question"]] = "a parrot"
    elif q.get("multiSelect"):
        answers[q["question"]] = ", ".join(labels[:2])
    else:
        answers[q["question"]] = labels[0]
    if i == 0 and len(ti["questions"]) > 1:
        annotations[q["question"]] = {"notes": "dark shade please"}
upd = {**ti, "answers": answers}
if annotations: upd["annotations"] = annotations
out = {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": upd}}
open(os.path.join(D, tag + ".out"), "w").write(json.dumps(out))
log.write(f"{time.time():.1f} answered\n"); log.flush()
print(json.dumps(out))
