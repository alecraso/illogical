"""S17: replay an editor's raw event stream through reporter policies.

Each event is a snapshot dict with a time `t` (ms). A stream is a set of fields; a message carries
only the fields that changed since the last one sent (as M23's deltas do), plus an editor id.

policies:
  raw           every event that changes something is a message
  trailing-N    debounce: send once nothing has changed for N ms (starves while typing)
  tick-N        throttle: at most one message per N ms, the latest state (M23's 1 s tick is tick-1000)
"""
import json

# What the swarm's summary carries (M23's once-a-second deltas): cheap, coarse.
SUMMARY = ("file", "e", "w", "i", "dirty", "debug")
# What a follower needs live (only while someone follows): cursor, selection, view, the edited line.
FOLLOW = ("file", "line", "col", "sel", "top", "bot", "mode", "text")

POLICIES = (("raw", 0), ("trailing", 50), ("trailing", 100), ("trailing", 250), ("tick", 100), ("tick", 250), ("tick", 1000))


def _pct(xs, p):
    if not xs:
        return None
    s = sorted(xs)
    return round(s[min(len(s) - 1, int(p / 100 * len(s)))])


def replay(events, fields, policy, window_ms, elapsed_s, norm=lambda st: st):
    msgs, size, last_sent = 0, 0, {}
    pending, pending_since, due = None, None, None
    delays = []

    def send(state, now, since):
        nonlocal msgs, size, last_sent
        delta = {k: v for k, v in state.items() if last_sent.get(k) != v}
        if delta:
            msgs += 1
            size += len(json.dumps({"editor": 1, **delta}, separators=(",", ":")))
            last_sent = dict(state)
            delays.append(now - since)

    tick = window_ms
    for e in events:
        t = e["t"]
        st = norm({k: e.get(k) for k in fields})
        if policy == "raw":
            send(st, t, t)
        elif policy == "trailing":
            if pending is not None and t >= due:
                send(pending, due, pending_since)
                pending = None
            if pending is None:
                pending_since = t
            pending, due = st, t + window_ms
        elif policy == "tick":
            while t >= tick:
                if pending is not None:
                    send(pending, tick, pending_since)
                    pending = None
                tick += window_ms
            if pending is None:
                pending_since = t
            pending = st
    if pending is not None:
        send(pending, (due if policy == "trailing" else tick), pending_since)
    return {
        "msgs_per_s": round(msgs / elapsed_s, 2),
        "bytes_per_s": round(size / elapsed_s, 1),
        "avg_msg_bytes": round(size / max(msgs, 1), 1),
        "delay_ms_p50": _pct(delays, 50),
        "delay_ms_max": _pct(delays, 100),
    }


def all_policies(events, elapsed_s, norm=lambda st: st):
    out = {}
    for name, fields in (("summary", SUMMARY), ("follow", FOLLOW)):
        for pol, w in POLICIES:
            key = f"{name}/{pol}" + ("" if pol == "raw" else f"-{w}ms")
            out[key] = replay(events, fields, pol, w, elapsed_s, norm)
    return out


def rates(events, elapsed_s):
    by = {}
    for e in events:
        by[e["ev"]] = by.get(e["ev"], 0) + 1
    return {k: round(v / elapsed_s, 2) for k, v in sorted(by.items(), key=lambda kv: -kv[1])}
