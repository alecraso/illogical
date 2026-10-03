#!/usr/bin/env python3
"""S24 q1: a local Claude Code that wears a Fountain agent.

    wear.py <agent name or id> <out dir>

Reads the agent from Fountain (`fountain agent list --json`) and writes what a
local `claude` needs to behave as that agent, without a Fountain sandbox:

- <out>/plugin/: a Claude Code plugin whose skills/ are the agent's skills
  (inline ones written out, GitHub ones shallow-cloned and copied);
- <out>/mcp.json: its MCP servers, ${VAR} resolved from this environment;
- <out>/system.md: its system prompt under a short "you are local" preamble;
- <out>/report.json: what didn't carry over (unset variables, missing skills).

Then: claude --plugin-dir <out>/plugin --mcp-config <out>/mcp.json \
             --append-system-prompt "$(cat <out>/system.md)"
"""
import json, os, re, shutil, subprocess, sys, tempfile
from pathlib import Path

PREAMBLE = """You are running as the Fountain agent "{name}", but locally, in Claude Code
on the user's own machine, not in a Fountain sandbox. Where the instructions
below mention /home/sprite, /workspace, vaults or spawning Fountain
conversations, they describe the sandbox; here, work in the current directory
with the user's own tools and credentials.

---

"""

def agent(key):
    out = subprocess.run(["fountain", "agent", "list", "--json"], check=True, capture_output=True, text=True).stdout
    rows = json.loads(out)
    rows = rows if isinstance(rows, list) else rows["data"]
    hit = [a for a in rows if key in (a["name"], a["id"])]
    if not hit:
        sys.exit(f"no agent {key!r}")
    return hit[0]

def subst(v, missing):
    """Fountain's ${VAR} / $$ rules (docs/primitives.md#substitution), from os.environ."""
    if isinstance(v, str):
        def one(m):
            if m.group(0) == "$$":
                return "$"
            k = m.group(1)
            if k not in os.environ:
                missing.add(k)
                return m.group(0)
            return os.environ[k]
        return re.sub(r"\$\$|\$\{([A-Za-z_][A-Za-z0-9_]*)\}", one, v)
    if isinstance(v, list):
        return [subst(x, missing) for x in v]
    if isinstance(v, dict):
        return {k: subst(x, missing) for k, x in v.items()}
    return v

def github_skills(source, name, dest, cache):
    """Copy skill dirs (those with a SKILL.md) from owner/repo; one name, or all."""
    repo = cache / source.replace("/", "__")
    if not repo.exists():
        subprocess.run(["git", "clone", "-q", "--depth", "1", f"https://github.com/{source}", str(repo)], check=True)
    found = []
    for md in repo.rglob("SKILL.md"):
        d = md.parent
        if name and d.name != name:
            continue
        if (dest / d.name).exists():
            continue
        shutil.copytree(d, dest / d.name, ignore=shutil.ignore_patterns(".git"))
        found.append(d.name)
    return found

def main():
    key, out = sys.argv[1], Path(sys.argv[2])
    a = agent(key)
    if out.exists():
        shutil.rmtree(out)
    skills = out / "plugin" / "skills"
    skills.mkdir(parents=True)
    (out / "plugin" / ".claude-plugin").mkdir()
    slug = re.sub(r"[^a-z0-9-]+", "-", a["name"].lower()).strip("-")
    (out / "plugin" / ".claude-plugin" / "plugin.json").write_text(json.dumps(
        {"name": f"fountain-{slug}", "description": f"Skills of the Fountain agent {a['name']}"}, indent=2))

    report = {"agent": a["name"], "runtime": a["runtime"], "model": a["model"], "skills": [], "skills_missing": [], "mcp": [], "unset": []}
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "illogical-s24" / "skills"
    cache.mkdir(parents=True, exist_ok=True)
    for s in a.get("skills") or []:
        if "content" in s:
            (skills / s["name"]).mkdir()
            (skills / s["name"] / "SKILL.md").write_text(s["content"])
            report["skills"].append(s["name"])
        else:
            got = github_skills(s["source"], s.get("name"), skills, cache)
            report["skills"] += got
            if not got:
                report["skills_missing"].append(f'{s["source"]}:{s.get("name") or "*"}')

    missing = set()
    servers = a.get("mcp_servers") or {}
    if isinstance(servers, list):
        servers = {s.pop("name"): s for s in servers}
    mcp = {}
    for n, cfg in servers.items():
        before = set(missing)
        resolved = subst(cfg, missing)
        if missing - before:
            continue  # a server with an unset secret would fail to connect; leave it out
        mcp[n] = resolved
    report["mcp"] = sorted(mcp)
    report["unset"] = sorted(missing)
    (out / "mcp.json").write_text(json.dumps({"mcpServers": mcp}, indent=2))
    (out / "system.md").write_text(PREAMBLE.format(name=a["name"]) + (a.get("system") or ""))
    (out / "report.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))

main()
