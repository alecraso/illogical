#!/usr/bin/env python3
"""Make the Fountain fixtures from raw API answers (M43).

    scrub.py AGENTS.json [RUNNERS.json] [ENVIRONMENTS.json]

Each input is what `GET /api/<thing>` (or `fountain agent list --json`)
returned: `{"data": [...]}` or a bare list. Writes agents.json, runners.json
and environments.json beside this script, as `{"data": [...]}`.

What's kept: names, ids, models, runtimes, metadata keys and values,
skill names and sources, MCP server names and types, counts and times.
What's cut:
  - `system`, an inline skill's `content` and `runtime_command`: their first
    80 characters;
  - URL hosts anywhere (MCP servers' first), unless a well-known public
    service's (GitHub, Codeberg, public MCP services) or loopback: each
    other host becomes mcp-N.example.com;
  - home directories become /srv/someone (/srv/sprite for the sandbox's);
  - every `${VAR}` becomes `${X}` (anywhere);
  - MCP `headers` and `env` values that aren't references are dropped;
  - email addresses, home directories, and runners' names and roots;
  - environments: only `id` and `name`.
Then it refuses to write anything that still looks like a secret.
"""

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
CUT = 80
VAR = re.compile(r"\$\{[^}]*\}")
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
HOME = re.compile(r"/(Users|home)/([A-Za-z0-9._-]+)")
SECRETS = [
    re.compile(p)
    for p in [
        r"ftn_[A-Za-z0-9]{8,}",
        r"gh[pousr]_[A-Za-z0-9]{20,}",
        r"github_pat_[A-Za-z0-9_]{20,}",
        r"sk-[A-Za-z0-9_-]{20,}",
        r"xox[abpr]-[A-Za-z0-9-]{10,}",
        r"AKIA[0-9A-Z]{16}",
        r"Bearer (?!\$\{X\})[A-Za-z0-9._-]{12,}",
        r"-----BEGIN [A-Z ]*PRIVATE KEY",
        r"\.ts\.net",
        r"tail[0-9a-f]{6}",
    ]
]


# Public MCP services anyone can name; any other host is someone's own.
PUBLIC_HOSTS = {
    "api.githubcopilot.com",
    "mcp.context7.com",
    "mcp.mem0.ai",
    "mcp.posthog.com",
    "mcp.honeycomb.io",
    "mcp.render.com",
    "127.0.0.1",
    "localhost",
    "github.com",
    "codeberg.org",
}
URL = re.compile(r"(https?://)([^/\s:\"']+)")
HOSTS = {}


def host(m):
    h = m.group(2).lower()
    if h in PUBLIC_HOSTS:
        return m.group(0)
    if h not in HOSTS:
        HOSTS[h] = f"mcp-{len(HOSTS) + 1}.example.com"
    return m.group(1) + HOSTS[h]


def hosts(v):
    if isinstance(v, dict):
        return {k: hosts(x) for k, x in v.items()}
    if isinstance(v, list):
        return [hosts(x) for x in v]
    return URL.sub(host, v) if isinstance(v, str) else v


def rows(path):
    v = json.loads(Path(path).read_text())
    return v["data"] if isinstance(v, dict) else v


def text(s):
    if not isinstance(s, str):
        return s
    s = VAR.sub("${X}", s)
    s = EMAIL.sub("someone@example.com", s)
    s = HOME.sub(lambda m: "/srv/" + (m.group(2) if m.group(2) in ("sprite", "fountain") else "someone"), s)
    return URL.sub(host, s)


def deep(v):
    if isinstance(v, dict):
        return {k: deep(x) for k, x in v.items()}
    if isinstance(v, list):
        return [deep(x) for x in v]
    return text(v)


def cut(s):
    if not isinstance(s, str):
        return s
    s = text(s)[:CUT]
    # A link cut short would name half a host: drop it.
    at = s.rfind("http")
    if at >= 0 and not re.search(r"\s", s[at:]):
        s = s[:at].rstrip()
    return s


def refs_only(m):
    # A value that is (or holds) a reference stays, as `${X}`; anything typed
    # in literally is dropped.
    return {k: text(v) for k, v in (m or {}).items() if isinstance(v, str) and VAR.search(v)}


def agent(a):
    a = dict(a)
    # MCP servers' hosts are numbered first, so they're mcp-1, mcp-2, ...
    for s in (a.get("mcp_servers") or {}).values():
        if isinstance(s, dict) and isinstance(s.get("url"), str):
            URL.sub(host, s["url"])
    a["system"] = cut(a.get("system"))
    a["runtime_command"] = cut(a.get("runtime_command"))
    skills = []
    for s in a.get("skills") or []:
        s = dict(s)
        if "content" in s:
            s["content"] = cut(s["content"])
        skills.append(deep(s))
    a["skills"] = skills
    servers = {}
    for name, s in (a.get("mcp_servers") or {}).items():
        s = dict(s)
        if "headers" in s:
            s["headers"] = refs_only(s["headers"])
        if "env" in s:
            s["env"] = refs_only(s["env"])
        servers[name] = deep(s)
    a["mcp_servers"] = servers
    rest = {k: v for k, v in a.items() if k not in ("system", "runtime_command", "skills", "mcp_servers")}
    return {**deep(rest), "system": a["system"], "runtime_command": a["runtime_command"], "skills": skills, "mcp_servers": servers}


def runner(r, n):
    r = deep(dict(r))
    r["name"] = f"runner-{n}"
    r["hostname"] = None
    if r.get("root"):
        r["root"] = f"/srv/fountain/runner-{n}/sandboxes"
    return r


def write(name, data):
    out = json.dumps({"data": data}, indent=1, ensure_ascii=False, sort_keys=True) + "\n"
    for p in SECRETS:
        m = p.search(out)
        if m:
            sys.exit(f"{name}: still looks secret at {m.start()}: {p.pattern}")
    # Every URL's host is a public one or an example; every address an example.
    for m in URL.finditer(out):
        h = m.group(2).lower()
        if h not in PUBLIC_HOSTS and not h.endswith("example.com"):
            sys.exit(f"{name}: a host that isn't public at {m.start()}")
    if HOME.search(out.replace("/srv/", "")):
        sys.exit(f"{name}: a home directory")
    for m in EMAIL.finditer(out):
        if not m.group(0).endswith("@example.com"):
            sys.exit(f"{name}: an address at {m.start()}")
    (HERE / name).write_text(out)
    print(f"wrote {name}: {len(data)} rows, {len(out)} bytes")


def main():
    args = sys.argv[1:]
    if not args:
        sys.exit(__doc__)
    write("agents.json", [agent(a) for a in rows(args[0])])
    if len(args) > 1:
        write("runners.json", [runner(r, n) for n, r in enumerate(rows(args[1]), 1)])
    if len(args) > 2:
        write("environments.json", [{"id": e["id"], "name": text(e["name"])} for e in rows(args[2])])


if __name__ == "__main__":
    main()
