# S20: Claude Code conversations as blocks
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s20-conversations/<file>`.

Run 2026-10-02 on geek. **Result: go, with three changes to M33.** A session
the CLI made resumes through the adapter the daemon already installs, with its
whole context, and the CLI sees the new turns afterwards. Fork works. What
changes:

- **The transcript is shown in file order, not by walking `parentUuid`.**
  Claude Code's links aren't a clean tree: compactions re-link a preserved
  tail after the boundary, side lines (`away_summary`, `turn_duration`) become
  parents of the next prompt, and parallel tool calls hang results off the
  chain. File order never puts a tool result before its call, and real
  rewinds are rare (5 in 389 sessions).
- **Imported blocks use `settingSources: ["user","project","local"]` with
  `settings: {disableAllHooks: true}`.** `[]` (what agent blocks use) drops
  `CLAUDE.md`; `project` is what loads it, and it brings the project's hooks
  with it. `disableAllHooks` keeps `CLAUDE.md`, skills and permissions and
  stops every hook (M3's too).
- **Fork is required while a session is live elsewhere, not advice.** Two
  writers each see only their own turns, and a later resume follows the
  newest `last-prompt` leaf, so the other writer's turns silently drop out of
  the conversation.

The desktop app's Code tab (Q6) is the same thing: a Claude Code process
writing the same jsonl, with `entrypoint: claude-desktop`. It forks like a
terminal session.

## Setup

- `@agentclientprotocol/claude-agent-acp` **0.85.0**, the one the daemon
  installs (`~/.local/share/illogical/agents/claude`), with
  `@anthropic-ai/claude-agent-sdk` 0.3.286 (bundled Claude Code 2.1.286). The
  CLI was **2.1.288**, so every resume here crossed versions.
- `acp.ts` is S7's client, unchanged. `claude-acp.sh` launches the daemon's
  adapter with the parent session's env removed (as `CLAUDE_ENV_REMOVE`).
- `work/scratch` is a git repo with a `CLAUDE.md` canary and `UserPromptSubmit`
  hooks in `.claude/settings.json` and `.claude/settings.local.json` that
  append to `project-hook.fired` and `local-hook.fired`.
- The interactive CLI ran in its own tmux server (`tmux -L s20`) with the
  Claude and illogical env removed (`work/clean.env`). Haiku throughout.

| file | what |
|---|---|
| `survey.py` | Q1: line types, content blocks, versions, entrypoints, sizes |
| `chain.py`, `chain2.py` | Q1: what walking `parentUuid` back from the last line leaves out |
| `fileorder.py` | Q1: file order checks (results after calls) and rewinds |
| `q2.ts` | Q2/Q3: resume a CLI session (`SID`, `SOURCES`, `SETTINGS`) and ask what it knows |
| `q4.ts` | Q4: fork, prompt the fork, resume it in a fresh process, hash the original |
| `q5.ts` | Q5: two writers (`STEP=write\|check`) |
| `fixtures.py` | writes `fixtures/` (redacted) |

```sh
python3 survey.py
SID=… SOURCES=user,project,local SETTINGS='{"disableAllHooks":true}' mise exec bun@1.4.2 -- bun q2.ts
SID=… SOURCES=user,project,local mise exec bun@1.4.2 -- bun q4.ts
```

## Q1: what's on disk

Geek, 2026-10-02 (`work/survey.txt`):

- **608 files, 931 MiB:** 389 sessions at `~/.claude/projects/<slug>/<id>.jsonl`
  and 219 subagent runs at `<slug>/<id>/subagents/agent-<x>.jsonl`, each with
  an `agent-<x>.meta.json`. Median session 528 KiB, p95 6.9 MiB, largest
  31 MiB. Parsing all of it in Python takes 2.5 s; reading the first and last
  64 KiB of the 389 sessions (what the index needs) takes 9 ms warm.
- **Versions:** every line is 2.1.x. **Entrypoints** of sessions: `cli` 211,
  `sdk-cli` 109 (`claude -p` and SDK tools; many are `/tmp/ilg-*` test repos
  and Claude scratchpads), `sdk-ts` 69 (agent blocks and other TypeScript SDK
  clients).
- **Line types**, by count: `assistant`, `attachment`, `user`, then
  bookkeeping: `atis-latch`, `last-prompt`, `mode`, `ai-title`,
  `permission-mode`, `queue-operation`, `system`, `pr-link`,
  `file-history-snapshot`/`-delta`, `cost-state`, `frame-link`, `relocated`,
  `worktree-state`, `agent-name`, `custom-title`, `fork-context-ref`,
  `continued-in`, and a few others. `system` subtypes: `turn_duration`,
  `away_summary`, `stop_hook_summary`, `scheduled_task_fire`, `api_error`,
  `local_command`, `informational`, `compact_boundary`, `agents_killed`.
- **Content blocks:** assistant `tool_use`, `thinking` (the text is often
  empty, with only a signature), `text`; user `tool_result`, a plain string,
  `text`, `image`. One API response is written as one line per block, sharing
  `message.id`.
- **Metadata the index needs:**
  - title: `custom-title.customTitle` (a rename, or "… (fork)") or
    `agent-name.agentName`, else the newest `ai-title.aiTitle`, else the
    first prompt;
  - `last-prompt {lastPrompt, leafUuid}`: the latest prompt, and the leaf a
    resume continues from (Q5);
  - `relocated.relocatedCwd`: the session moved (into a worktree);
  - `continued-in.continuedInSessionId`: the conversation goes on in another
    session;
  - every line of a fork has `forkedFrom {sessionId, messageUuid}`;
  - `isSidechain`, `isMeta`, `isCompactSummary`, `toolUseResult`
    (structured tool output, beside the `tool_result` block).
- **The chain isn't a clean tree** (`chain.py`, `chain2.py`). Walking back
  from the last user/assistant line drops 8,095 of 63,400 lines in 41 files.
  With compactions followed through `logicalParentUuid`, missing parents
  bridged and parallel calls added back by `message.id` and `tool_use_id`, it
  still drops 7,863 in 18:
  - a compaction keeps a "preserved segment" and re-links it after the
    boundary, so the walk loops;
  - a `system` line written while you were away (`away_summary`) becomes the
    parent of the next prompt, so a whole exchange falls off the chain;
  - a parent can be missing from the file.
- **File order works** (`fileorder.py`): no `tool_result` comes before its
  `tool_use` in any file. A prompt re-sent from an earlier point (a rewind,
  or a second root) happens 5 times in 389 sessions; the converter marks it
  with a note.
- **Live sessions:** `~/.claude/sessions/<pid>.json` holds `pid`,
  `sessionId`, `cwd`, `kind` (`interactive`), `entrypoint`, `status`
  (`idle`, `busy`), `procStart` and `updatedAt`. `procStart` is field 22 of
  `/proc/<pid>/stat`, so a reused pid is caught. `/proc/<pid>/cgroup` names
  the scope: `illogical-pane-76-<n>.scope` for a process in pane 76,
  `tmux-spawn-….scope` for one in plain tmux.

## Q2: resuming a CLI session through the adapter: works

An interactive CLI session (2.1.288): "remember 4817, tell me the canary
word, run `echo hello-s20 > note.txt`" (it saved 4817 to auto-memory), then
"the courier's name is Odalys Brenner; don't write it anywhere", then
`/exit`. `CLAUDE.md`'s canary was then changed from PERIWINKLE to VERMILION.

`session/resume` with the CLI's session id, from a fresh adapter: **1,010 ms**
(577–672 ms on later runs), no replay. The next turn answered
"Odalys Brenner, NONE, 4817": the context is all there.

- `claude --resume <id>` in a terminal afterwards shows the adapter's turns,
  in the same file (`entrypoint: sdk-ts`, `version: 2.1.286` on those lines).
- **The adapter writes command lines into the transcript.** Every resume
  plus `set_config_option` adds `/model` lines (`<local-command-caveat>`,
  `<command-name>`, `<local-command-stdout>`). The CLI writes the same shape
  for slash commands. The converter shows them as one note, or not at all.
- **A resume resets the model** to the adapter's default (Opus) until
  `set_config_option`. *Continue* should set the session's last model (the
  last assistant line's `message.model`).

## Q3: `settingSources`

| sources | `CLAUDE.md` | project hook | local hook |
|---|---|---|---|
| `[]` (agent blocks today) | no (NONE) | no | no |
| `project` | yes (VERMILION) | **fires** | no |
| `project,local` | yes | fires | **fires** |
| `project,local` + `settings: {disableAllHooks: true}` | yes | no | no |
| `user,project,local` + `disableAllHooks` | yes | no | no |

The SDK says it plainly: `'project'` must be in `settingSources` for
`CLAUDE.md` to load. `settings` (the `--settings` flag layer) passes through
`_meta.claudeCode.options`. **M33 uses `user,project,local` with
`disableAllHooks`**: the session gets your skills (they showed up in
`available_commands_update`), permissions and `CLAUDE.md`, as it had in the
terminal, and no hook fires. Geek has no `~/.claude/CLAUDE.md`, so the user
one wasn't checked; the user layer is the same mechanism.

## Q4: fork: works

`session/fork {sessionId, cwd, mcpServers}` takes **22–24 ms**. It writes a
new transcript and returns its id, and **doesn't open it**: prompting it
right away fails with `Session not found`. `session/resume` on the new id
first, then it works.

- The fork answered "Odalys Brenner", took a new fact ("quince"), and a fresh
  process resumed it (661 ms) and still had both.
- The original's sha256 was unchanged after the fork, after the fork's turns
  and after its resume.
- Every line of the fork carries `forkedFrom {sessionId, messageUuid}`, and
  it gets a `custom-title` of the original's title plus " (fork)".

## Q5: two writers: one side's turns disappear

The CLI reopened the session (`claude --resume`) and stayed open. Then:

1. Over ACP (resume, no fork): "the vault code is 2468" → "OK".
2. In the CLI: "the lighthouse is teal; what's the vault code?" →
   **"UNKNOWN"**. The CLI doesn't see turns another process wrote.
3. A fresh ACP resume: "vault code, lighthouse colour?" → **"2468,
   UNKNOWN"**. The CLI's turn is gone.

The file has both branches: the CLI's prompt (line 166) is a child of line
140, beside the ACP branch (151–158). A resume continues from the newest
`last-prompt.leafUuid`, which the CLI hadn't written for its turn (it writes
one when the session ends), so the CLI's turns fell off without a warning.
Resuming a live session therefore loses someone's turns. **M33 forks** when
the session is live in another process, and offers *Continue* once that
process is gone.

## Q6: the desktop app's Code tab: the same, with its own record

Jake started one Code tab session on geek ("Hello", no folder chosen).

- **The process** is the desktop app's bundled Claude Code
  (`~/.config/Claude/claude-code/2.1.275/claude`), run as `--output-format
  stream-json --input-format stream-json --model claude-opus-5 --effort high
  --permission-prompt-tool stdio …`: the adapter's mode, driven by the app.
- **The transcript** is an ordinary `~/.claude/projects/<slug>/<id>.jsonl`,
  `entrypoint: claude-desktop`, `version: 2.1.275`, with `custom-title` and
  `agent-name` lines holding the app's title.
- **Live:** `~/.claude/sessions/<pid>.json` lists it (`kind: interactive`,
  `entrypoint: claude-desktop`, `name`: the title), like a terminal session.
- **The app's own record** is
  `~/.config/Claude/claude-code-sessions/<account>/<org>/local_<uuid>.json`:
  `cliSessionId` (the jsonl's id), `cwd`, `title` and `titleSource`, `model`,
  `effort`, `permissionMode`, `isArchived`, `createdAt`, `lastActivityAt`, and
  its MCP tool choices and connector configs. On a Mac it's under
  `~/Library/Application Support/Claude/` (not checked).
- **No folder chosen** means a scratch workspace,
  `~/.config/Claude/scratch-workspaces/<account>/<org>/scratch-<date>-<x>`,
  which "is removed once the session is gone" (the app's own reminder, which
  it puts at the top of the first prompt in a `<system-reminder>`). So a
  desktop session's cwd can vanish while the conversation is still worth
  continuing.
- **Fork** of the live desktop session through the adapter (2.1.286 against
  2.1.275): 20 ms, the original's sha256 unchanged, the fork resumed in a fresh
  process in 621 ms.

## Fixtures

`fixtures.py` writes `fixtures/`; this repo is public, so:

- `shapes/` holds lines from real transcripts with the structure kept
  (types, keys, uuids and their links, tool names and ids, flags, models)
  and every other string replaced by `<N chars>`. Bash, Edit,
  AskUserQuestion, an image, thinking then text, parallel tool calls, a
  compaction, an API error, a local command, a rewind, a subagent call and
  its transcript and `.meta.json`, the metadata lines, a live-session file,
  the desktop app's record (`desktop-session.json`, connector configs
  dropped) and the start of its transcript.
- `scratch/` holds this spike's own session (`a683c96a…`: the CLI turns,
  then the adapter's, then the CLI's branch from Q5) and its fork
  (`d1ccc1e4…`), verbatim but with the home directory rewritten, attachment
  bodies dropped and meta and system lines redacted.

Both were checked for names, hosts, emails and tokens before committing.
