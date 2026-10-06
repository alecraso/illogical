# S24: Fountain, more deeply (agent catalog, agents worn locally, runners)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s24-fountain/<file>`.

Run 2026-10-03 on geek, against hosted Fountain (`managoat.com`, Jake's account, CLI v0.21.0).
Today illogical knows Fountain only as an ACP command: an agent block can run `fountain acp
--agent X` (S7), and that's all. This spike asks what else is worth building.
**Result: go on all three, with geek as the only runner (decided below). Before that decision it was two of three. A read-only agent catalog and "wear this agent locally" are cheap,
and they work from what Fountain already exposes (q1: a session wearing `pr-reviewer` works). Managing runners waits on one Fountain change:
an agent can't be pinned to a runner.**

## What's there

- **108 agents on the account, and most of them are machine-made.** 13 `Switchyard ·`, 8
  `Salon ·`, 5 `Mend:`, 4 `Rounds:`, planter, Paddock, probes and `adr-0047-prod-proof-2`. About
  23 carry `metadata.managed-by: chant`: those are the curated ones, declared in
  `~/dev/jhgaylor/agent-specs` (chant's fountain lexicon, `dist/fountain.yaml`, secrets as
  `infisical://` URIs). A handful more are hand-made (`games`, `ravioli-*`, `reflex-1`,
  `fountain-marketing`). A catalog that lists all 108 is noise. The filter is metadata:
  `managed-by`, `switchyard`, `salon`, `paddock`, `attemptId`, `part-of`.
- **An agent is a full recipe:** `system` (105 of 108 have one, up to 20 kB), `skills` (25; inline
  `{name, content}` or GitHub `{source, name?}`), `mcp_servers` (43; `${VAR}` in any string),
  model, runtime, environment, `sandbox_provider`. `fountain agent list --json` returns all of it.
- **Secrets are write-only.** No read returns a vault's or an environment's values
  (`docs/concepts/vault.md`, "Not returnable"). Anything that runs an agent off Fountain has to
  get its `${VAR}`s somewhere else.
- **3 runners registered:** `jake-air` (online, build `runner-d0b9597`), `jake-mbair` (last seen
  2026-09-01, v0.13.0), `fireball` (2026-08-27). 3 agents use `sandbox_provider: runner`.
  `GET /api/runners` gives name, os, arch, version, online and last seen. There's no CLI for it.
- **Fountain places a runner conversation on the most recently connected online runner.**
  Per-agent pinning (`agents.runner_id`) is named in ADR 0022 and not built.

## q1: a local Claude Code wearing a Fountain agent

`wear.py <agent> <out>` turns an agent into what a local `claude` takes:

| Fountain | Local Claude Code | ACP (`claude-agent-acp`, what an agent block uses) |
|---|---|---|
| `system` | `--append-system-prompt` | `session/new` `_meta.systemPrompt.append` |
| inline skill | `<plugin>/skills/<name>/SKILL.md` | `_meta.claudeCode.options.plugins` |
| GitHub skill | shallow clone, copy each dir with a `SKILL.md` (one `name`, or all) | same |
| `mcp_servers` | `--mcp-config`, `${VAR}` from local env | `session/new` `mcpServers` (illogical already sends these) |
| `model` | `--model` (strip `anthropic/`) | `_meta.claudeCode.options.model` |

- `games`: 3 inline skills, no MCP. Clean.
- `pr-reviewer`: 4 of `getsentry/skills` + all 15 of `obra/superpowers`, and `context7`, `mem0`,
  `github`. `github` needs `${GITHUB_TOKEN}`. It's left out unless set, and set from `gh auth
  token` it resolves.
- **Loaded into a real session through `claude-agent-acp`** (`q1-acp.mjs`, the path a block
  takes; 11 s). The session calls itself pr-reviewer running locally. It lists the plugin's
  skills as `fountain-pr-reviewer:code-review`, `…:iterate-pr` and so on, and has tools from
  `github` and `context7`.
  - `mem0` (HTTP, OAuth) can't connect headless.
  - The account's claude.ai connectors come along too.
  - **The client must send `settingSources: []`**, as illogical's Claude blocks do. Without it,
    the SessionStart hook `illogical inbox` (24 h timeout) holds the session. That is why headless
    `claude -p` never answered on geek; `--setting-sources project,local` answers in 4.6 s (#124).

**The prompts are written for the sandbox.** `captain-picard` clones into `/workspace`, spawns
specialists with `vault_id`, and talks about `/home/sprite`. `wear.py` puts a short preamble in
front ("you're local; those paths describe the sandbox"), which is fine for specialists
(`pr-reviewer`, `designer`, `games`) and wrong for orchestrators (`captain-picard`, `team-lead`,
`tech-lead`), whose whole job is Fountain's API. Those should be run on Fountain, not worn.
A flag in agent-specs (`metadata.illogical.local: false`) says so better than a heuristic.

## What it might look like

1. **Agent catalog (a block, plus an MCP tool).** Cards for the curated agents, grouped as
   agent-specs, hand-made and (collapsed) app-made: description, model, skills, MCP servers,
   environment, conversation count. Each card has *Run on Fountain* (today's `fountain` agent
   block), *Run here* (2) and *Spec* (the agent-specs file, for `managed-by: chant`).
   Read-only: agent-specs stays the one place an agent is edited, so illogical never fights
   chant's converge. `list_agents` on illogical's MCP server lets any local Claude see the team
   and hand a task to one through `start_agent` (which already takes a Fountain agent).
2. **Wear locally: `illogical agent --as <fountain agent>`.** A Claude agent block (or a
   terminal `claude`) launched with the agent's prompt, skills and MCP servers, in a worktree on
   this machine: local files, local tools, Jake's subscription, no sandbox. Bundles are cached by
   the agent's `updated_at`. `${VAR}`s come from the environment, a few known helpers
   (`GITHUB_TOKEN` from `gh auth token`), and Infisical, which already holds agent-specs'
   secrets. An unresolved server is left out and the block says which.
3. **Runners on illogical's machines.** illogicald supervises `fountain runner` on each machine
   (geek with `--backend firecracker`, jake-mini with `process`), as a pane with `rerun` after a
   reboot, and shows `/api/runners` (online, version drift, stale registrations) on the machine.
   On the process backend a runner sandbox is just a directory on that machine, so illogical
   can open a shell in a running Fountain conversation's sandbox, show its diff, and take over:
   watch-and-take-over for Fountain's own runs, the thing illogical is for.
   **Blocked on placement:** with geek and jake-mini both online, Fountain picks whichever
   connected last. Fountain needs `agents.runner_id` (or a runner name per conversation) first.
   firecracker on geek overlaps wisp; PLAN already keeps M3b on wisp.

Not worth it now: Fountain conversations in swarm (webhooks into attention) is real but follows
from 1; injecting the whole team as `--agents` subagents into every local Claude bloats every
session's context for the rare delegation.

## Decided (2026-10-03, Jake)

geek is the only runner (Jake adds runner pinning to Fountain going forward), on the process
backend as a dedicated `fountain` user; jake-air's runner stops, and all three other
registrations are deleted. The catalog shows every agent with a filter. Local `${VAR}`s come
from Infisical, then the environment, then helpers. The three agents on the runner provider today
move to geek. The plan is PLAN.md's Fountain track: M43 (#121), M44 (#122), M45 (#123).
