# S7: the agent block as an ACP client
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s7-acp/<file>`.

Run 2026-10-01 on geek. **Result: ACP works for the agent block, with
changes.** One small hand-rolled client drove `claude-agent-acp`, `codex-acp`
and `fountain acp` with the same code. Permissions, cancel, `session/load`
and cost all work. Four findings change the M6b design:

- **No adapter uses client terminals or client fs.** `claude-agent-acp` and
  `codex-acp` never call `terminal/*` or `fs/*`, even when the client offers
  them. They run commands themselves. They report output through Zed's
  `_meta.terminal_output` extension instead, and only once the command ends.
  So "each agent command as a live terminal block" is not available from
  these adapters.
- **`claude-agent-acp` exits on stdin EOF and aborts the turn.** It treats
  the pending permission as rejected and kills a running command.
  `claude -p` (S6) finished the turn; this adapter does not. Holding the pipe
  fds (S6's fd-store approach) is therefore required, not optional, and it
  works (Q4c).
- **"Allow always" writes `.claude/settings.local.json` at the git root of
  the cwd.** It does this even with `settingSources: []`. The rule did not
  stop the next prompt for the same redirecting command.
- **Fountain's `session/load` is a snapshot, not an attach.** A client that
  reconnects mid-turn gets the replay up to that point and no live updates
  after it. A permission request raised to a dead client is not re-sent; it
  waits for the 5-minute refusal.

## Setup

- **`@agentclientprotocol/claude-agent-acp` 0.81.2**, the version Fountain
  pins in `managoat_runtimes` (`lib/managoat/runtimes/acp.ex`,
  `priv/acp/claude-agent-acp@0.81.2.tsv`). npm latest is 0.84.0. The old
  `@zed-industries/claude-code-acp` (0.16.2) is deprecated and renamed.
  - It depends on `@anthropic-ai/claude-agent-sdk` 0.3.280. It runs the
    **bundled Claude Code 2.1.280**, not Jake's 2.1.286, unless
    `CLAUDE_CODE_EXECUTABLE` is set.
  - It runs claude as `--output-format stream-json --verbose --input-format
    stream-json --permission-prompt-tool stdio …`. This is the same
    stream-json mode as S6, wrapped.
  - `claude-acp.sh` unsets `CLAUDECODE`, `CLAUDE_CODE_*`, `CLAUDE_PID`,
    `CLAUDE_EFFORT` and `AI_AGENT`, as S6's `claude.sh` did.
  - Every session used `_meta.claudeCode.options.settingSources: []`, so
    Jake's hooks didn't load, and was switched to haiku
    (`claude-haiku-4-5-20251001`).
- **`@agentclientprotocol/sdk` 1.5.1** is the official TypeScript SDK and is
  current. It was installed for reference only. The client here is
  hand-rolled newline-delimited JSON-RPC, about 200 lines, so every frame
  could be logged.
- **`@agentclientprotocol/codex-acp` 2.1.0** (Fountain pins 1.10.0),
  installed with `--omit=optional` (18MB) and run against the installed
  `codex-cli 0.155.1` via `CODEX_PATH`.
- **fountain CLI v0.21.0**, **profile `default`** (`https://managoat.com`).
  `qs-hosted` works too; `selfhost` and `selfhost2` return 404. The agent was
  the existing `Arena anthropic/claude-haiku-4-5`
  (`f0577ee9-a870-433e-a866-5f4e96e93826`): runtime claude, ephemeral
  sandbox, empty permission policy. No agent was created.
- Bun 1.4.2 (`mise exec bun@1.4.2 --`), Node 22.23.2. The agents' cwd was
  `work/scratch/`, which was turned into its own git repo after the first
  allow-always test (see Q2).

**Code** (all in this directory; logs in `work/*.ndjson`, one line per
frame with `t` in seconds):

| file | what |
|---|---|
| `acp.ts` | the client: spawn or attach to FIFOs, JSON-RPC, a permission policy (allow / allow_always / reject / cancelled / hold, with a delay), `fs/*`, `terminal/*` with a plain child process |
| `claude-acp.sh` | launches the pinned adapter with the parent session's env removed |
| `q1.ts` | Q1: initialize, new, two turns, cancel |
| `q23.ts` | Q2/Q3: permissions, terminal/fs capabilities (`CAPS=plain\|meta\|delta`, `PERM=…`, `DELAY=ms`) |
| `q4.ts` | Q4a/b: kill the client mid-turn; `session/load`, `session/resume` |
| `held-pipes.sh`, `held-client.ts` | Q4c: the adapter on FIFOs held by a keeper |
| `q5.ts` | Q5: `fountain acp` (`STEP=basic\|kill\|load\|hold`) |
| `q6.ts` | Q6: one-prompt smoke test of any ACP agent |

```sh
mise exec bun@1.4.2 -- bun q1.ts
CAPS=delta PERM=allow PROMPT=bash mise exec bun@1.4.2 -- bun q23.ts
STEP=basic PERM=execute=ask mise exec bun@1.4.2 -- bun q5.ts
```

`work/node` is reinstalled with `npm install --prefix work/node
@agentclientprotocol/claude-agent-acp@0.81.2 @agentclientprotocol/sdk@1.5.1`.

## 1. A minimal ACP client against claude-agent-acp: works

**initialize** (client sends `protocolVersion: 1`, `clientCapabilities:
{fs:{readTextFile,writeTextFile}, terminal:true}`). The agent answers:

```json
{"protocolVersion":1,
 "agentCapabilities":{"loadSession":true,
   "promptCapabilities":{"image":true,"embeddedContext":true},
   "mcpCapabilities":{"http":true,"sse":true},
   "sessionCapabilities":{"additionalDirectories":{},"close":{},"delete":{},"fork":{},"list":{},"resume":{},"subagents":{}},
   "_meta":{"claudeCode":{"promptQueueing":true},"authStatus":{}}},
 "agentInfo":{"name":"@agentclientprotocol/claude-agent-acp","title":"Claude Agent","version":"0.81.2"},
 "authMethods":[],
 "_meta":{"steering":{"supported":true},"goal":{…},"jetbrains":{"air":{…}}}}
```

**session/new** `{cwd, mcpServers:[], _meta:{claudeCode:{options:{settingSources:[]}}}}`
returns `sessionId`, `modes` (default/acceptEdits/plan/auto/bypassPermissions)
and `configOptions`: `mode`, `model`, `effort` and `fast`.

- **The default model was `opus[1m]`.** `_meta.claudeCode.options.model:
  "haiku"` was silently overridden; the first Q1 run went to Opus (about
  $0.13).
- What works is `session/set_config_option {sessionId, configId:"model",
  value:"haiku"}`. Its response carries the updated `configOptions`.

**Turns** (haiku, same process and session):

| turn | ms | stopReason | updates |
|---|---|---|---|
| "Remember PELICAN, reply OK" | 1016 | `end_turn` | `available_commands_update` 1, `usage_update` 3, `agent_message_chunk` 1 |
| "What was the code word?" → `PELICAN` | 1198 | `end_turn` | `session_info_update` 1 (title "Code word Pelican"), `usage_update` 3, `agent_message_chunk` 1 |
| 400-word story, `session/cancel` after 4s | response **19ms after cancel** | `cancelled` | `agent_message_chunk` 122, `usage_update` 2 |
| "Code word again?" after the cancel | ~1s | `end_turn` | answered `PELICAN`; the process and session are fine |

`session/cancel` is a notification. The pending `session/prompt` then
resolves with `{"stopReason":"cancelled"}`.

**Usage and cost.**

- The `session/prompt` response carries **per-turn** usage:
  `{"stopReason":"end_turn","usage":{"inputTokens":10,"outputTokens":53,"cachedReadTokens":13689,"cachedWriteTokens":7859,"totalTokens":21611},"_meta":{"quota":{"model_usage":[{"model":"claude-haiku-4-5-20251001",…}]}}}`.
  A cancelled turn reports zeros.
- **Cost arrives in `usage_update`, and it is cumulative per session:**
  `{"sessionUpdate":"usage_update","used":16567,"size":1000000,"cost":{"amount":0.0183,"currency":"USD"}}`.
  The amounts went 0.0183 → 0.0239 → 0.0239 (cancelled) → 0.0273. Store
  per-turn deltas, as in S6.
- Other `usage_update`s carry `used`/`size` (the context window),
  `_meta["_claude/model"]` and `_meta["_claude/rateLimit"]`
  (`five_hour`/`seven_day` utilization). These are the same fields as S6's
  `rate_limit_event`.

**Update kinds seen** across all runs: `agent_message_chunk`,
`agent_thought_chunk` (only through Fountain; local haiku sent no thought
chunks), `tool_call`, `tool_call_update`, `usage_update`,
`available_commands_update`, `session_info_update`, and on load
`user_message_chunk`.

**How they map to the block's attention state:**

| signal | state |
|---|---|
| `session/prompt` sent, until its response | `working` |
| incoming `session/request_permission` (until answered) | `needs-input` |
| `session/prompt` response `end_turn` / `max_tokens` / `refusal` | `done` (turn finished; the block is idle until the next send) |
| `cancelled` | `idle` |
| JSON-RPC error on the prompt, or the process exits | `needs-input` with an error |
| `tool_call` / `tool_call_update` `status: pending→in_progress→completed\|failed` | "current tool" in state |

The block owns the turn boundary: it is the response to its own request, so
no event parsing is needed for `working`/`done`. With stream-json the block
had to watch for `result` events instead.

## 2. Permissions

**A side-effecting command asks; read-only does not.** `ls`, and `echo`
without a redirect, ran without a request. `echo hi > x.txt`, `touch`,
`mkdir`, a `for … sleep` loop, and `Write` all asked. This matches S6.

**Request** (agent → client, JSON-RPC request with an `id` the client must
answer):

```json
{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{
  "sessionId":"e5544303-…",
  "toolCall":{"toolCallId":"toolu_01Pe5s…","name":"Bash","status":"pending",
    "rawInput":{"command":"echo hi > x.txt","description":"Create file x.txt with content \"hi\""},
    "title":"echo hi > x.txt","kind":"execute",
    "content":[{"type":"content","content":{"type":"text","text":"Create file x.txt with content \"hi\""}}],
    "locations":[{"path":"/home/me/…/work/scratch/x.txt"}]},
  "_meta":{"permission":{"version":1,"title":"echo hi > x.txt"}},
  "options":[
    {"optionId":"allow-once","name":"Yes","kind":"allow_once"},
    {"optionId":"allow-with-updates","name":"Yes, and allow access to scratch/ and echo hi * commands","kind":"allow_always"},
    {"optionId":"reject","name":"No","kind":"reject_once"}]}}
```

- **Options differ per tool and command:**
  - `Write`: `allow_always` is "Yes, allow all edits during this session",
    which switches the session mode.
  - `touch a.txt`, `mkdir d1`: only `allow-once` and `reject` (no
    `allow_always`).
  - No `reject_always` was ever offered.
- Option ids are the adapter's own. codex uses `allow_once`/`reject_once`
  and gemini `proceed_once`/`cancel` (Fountain's notes). **Always answer with
  an id from the request,** chosen by `kind`.
- The `toolCallId` equals the Claude `tool_use` id and matches the
  `tool_call` update, so approve/deny maps onto the transcript directly.

**Response** (the spec's two shapes):

```json
{"jsonrpc":"2.0","id":0,"result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}}
{"jsonrpc":"2.0","id":0,"result":{"outcome":{"outcome":"cancelled"}}}
```

- **Reject:** the tool call ends `status:"failed"`, the model is told
  "User refused permission", and the turn still ends `end_turn`.
- The spec says the client **MUST** answer pending requests with
  `cancelled` when it cancels the turn.

**How long it waits:**

| hold before allow | result |
|---|---|
| 60s | ran; turn 63,324ms, `end_turn` |
| 330s (5.5 min) | ran; turn 333,049ms, `end_turn` |
| never answered | **no timeout in 25 min** (my client's own 1500s limit). No update and no error the whole time |

The adapter has no deadline of its own. Unlike S6's MCP tool, no
`MCP_TOOL_TIMEOUT` applies: the permission travels on the SDK's stdio
control channel.

**"Allow always" persists to a file**:

- **Where.** Choosing `allow-with-updates` for `echo hi > x.txt` wrote
  `{"permissions":{"allow":["Bash(echo hi *)"]}}` to
  **`<git root of the cwd>/.claude/settings.local.json`**.
  - The first time, cwd `work/scratch` was inside the illogical repo, so it
    created `/home/me/dev/jhgaylor/illogical/.claude/settings.local.json`.
    That file is untracked and was deleted straight away.
  - After `git init work/scratch` it wrote `work/scratch/.claude/…`.
  - It writes **even with `settingSources: []`**. The "access to scratch/"
    half is session-only (`addDirectories`).
- **It didn't help.** The next `echo hi > w.txt` asked again in all three
  cases: in the same session, in a new process with
  `settingSources:["local"]` (which reads that file), and with none. The rule
  doesn't cover output redirection (S6 saw the same with `--allowedTools`).
  The other "always" commands offered none.
- **So the agent block should keep its own "always allow" policy** in the
  block config and answer from it, and not pick `allow_always`. Otherwise it
  writes into the user's repo.

## 3. Client terminals and fs: not used

**With `terminal: true` and `fs:{readTextFile:true, writeTextFile:true}`,**
the only agent → client request in any run was
`session/request_permission`.

- `Bash` ran inside Claude Code.
- `Write` and `Read` touched the disk directly. `y.txt` appeared and the
  read came back with no `fs/*` call.
- In the adapter source, `readTextFile`/`writeTextFile` exist as methods with
  no callers, and there is no `createTerminal` at all.
- `codex-acp` 2.1.0 is the same. Its SDK defines `terminal/create`, but it
  never calls it, and Q6's command ran with no client request.

So the client side of `terminal/*` and `fs/*` in `acp.ts` was never
exercised. The questions about a slow client or a killed terminal have no
answer for these adapters.

**What these adapters offer instead** is Zed's `_meta` extension, opted
into with `clientCapabilities._meta.terminal_output: true` (or
`terminal_output_delta: true`):

```json
{"sessionUpdate":"tool_call","toolCallId":"toolu_01Cd…","kind":"execute","title":"Terminal",
 "content":[{"type":"terminal","terminalId":"toolu_01Cd…"}],
 "_meta":{"terminal_info":{"terminal_id":"toolu_01Cd…"}}}
{"sessionUpdate":"tool_call_update","toolCallId":"toolu_01Cd…","_meta":{"terminal_output":{"terminal_id":"toolu_01Cd…","data":"x.txt"}}}
{"sessionUpdate":"tool_call_update","toolCallId":"toolu_01Cd…","status":"completed",
 "content":[{"type":"terminal","terminalId":"toolu_01Cd…"}],
 "_meta":{"terminal_exit":{"terminal_id":"toolu_01Cd…","exit_code":0,"signal":null}}}
```

- **The output is not live.** With `terminal_output_delta`, `for i in 1 2 3;
  do echo line$i; sleep 2; done` produced **one** delta of
  `"line1\nline2\nline3"` at the end (8.83s), not three chunks two seconds
  apart.
- The `terminalId` is the tool-use id, not a client terminal.
- Codex sends the same shape (`terminal_info` with `cwd`, `terminal_exit`).
- **Without the `_meta` flag,** output arrives as `rawOutput` plus a
  ```` ```console ```` text block, and stdout/stderr come in
  `_meta.claudeCode.toolResponse`.

**No setting turns client terminals on** in claude-agent-acp 0.81.2:

- `CLAUDE_CODE_EXECUTABLE` and `_meta.claudeCode.options.*` pass through to
  the SDK, which has no client-terminal backend;
- `disableBuiltInTools`/`tools: []` remove Bash entirely.

The only route would be to remove Bash and give the agent an MCP "run"
tool served by illogical. That is our own tool and not ACP terminals, and it
is untested.

## 4. Restarts

### (a) Client killed while a turn runs

The adapter was started `detached`, so only its pipes died.

| killed while | adapter did |
|---|---|
| a permission request was unanswered | **exited at once** (gone within 5s; its claude child too). Transcript: `tool_result` "The user doesn't want to proceed with this tool use. The tool use was rejected…" then `[Request interrupted by user for tool use]` |
| the allowed `sleep 15; echo done > k-cmd.txt` was running (3s in) | **exited at once and killed the command:** `k-cmd.txt` was never written. Same "rejected" + interrupted record |

This is by design. `index.js` does `connection.closed.then(shutdown)`
("Exit cleanly when the ACP connection closes (e.g. stdin EOF…)"), and
`shutdown` aborts every session. **It is the opposite of `claude -p` in S6,**
which finished the turn after stdin EOF. Nothing was orphaned.

### (b) Fresh client: `session/load` and `session/resume`

Both are advertised and both restore context.

- **`session/load {sessionId, cwd, mcpServers}`:** 560ms. It replays before
  the response: `user_message_chunk` 3 (both prompts and the
  "[Request interrupted…]" marker), `agent_message_chunk` 1, `tool_call`, and
  `tool_call_update` `status:"failed"` with the rejection text. The next
  prompt answered "Your favourite bird is the kestrel; the command was
  rejected and didn't run."
- **`session/resume`:** 542ms, **no replay**, context kept. It knew the Bash
  call had been rejected.
- **The id is the Claude session id.** Transcripts stay in
  `~/.claude/projects/<cwd>/<id>.jsonl`, as in S6. `session/load` needs the
  same `cwd`.

### (c) Held pipe fds: works, and is required

`held-pipes.sh`: a keeper holds `in` and `out` FIFOs open read-write; the
adapter runs in its own session on them.

1. Client #1 sends initialize, new, turn 1 ("merlin"), then turn 2
   ("`echo held > held.txt`"). It receives `request_permission` id 0, saves
   `{permissionRequestId:0, options, nextId}` (what the daemon would keep in
   the block log), and SIGKILLs itself.
2. For 20s nothing is attached. **The adapter stays alive**, because it never
   sees EOF.
3. Client #2 opens the FIFOs. The backlog is 0 frames, since nothing was
   written while blocked. It sends `{"id":0,"result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}}`.
   The command runs, the updates stream, and the response to client #1's
   prompt (id 5) arrives. Turn 3 on the same process answers "merlin, and
   yes, the command ran successfully".

Compared with S6:

- **The fd store is now the only way an agent survives a daemon restart**
  mid-turn. Without it, the turn is aborted, where S6 merely lost the stream.
- **The permission relay is gone.** The permission is a JSON-RPC request on
  the same pipe, so no MCP server or socket reconnect is needed. But the
  daemon must persist the pending request's **JSON-RPC id and options**, and
  its own outstanding request ids. The response to a request sent before the
  restart arrives on the new daemon, so new ids must not collide (client #2
  started at 100).
- **The daemon's reader must not lose a partial line.** A killed daemon may
  have read bytes it never processed. Here nothing was in flight, because the
  agent was blocked. In general, persist the read offset in the log, or read
  whole lines only.
- **Bun quirk:** inside a Bun-spawned `sh -c`, a backgrounded `cat` FIFO
  bridge delivered nothing (Node was fine). Opening the FIFOs directly with
  `fs.openSync` works, and that is what `acp.ts` `fifo:` mode does. The Rust
  daemon won't hit this.

## 5. Fountain as an agent host (`fountain acp`)

```
fountain acp --agent f0577ee9-… --log-level debug [--permission execute=ask]
```

**initialize:**

```json
{"agentCapabilities":{"loadSession":true,"promptCapabilities":{"audio":false,"embeddedContext":false,"image":true}},
 "agentInfo":{"name":"fountain","title":"Fountain","version":"v0.21.0"},"authMethods":[],"protocolVersion":1}
```

- stderr confirms it logs the client's `fs`/`terminal` capabilities and uses
  neither: `ignoring client-side session parameters cwd=… mcpServers=0
  why="the agent runs in a sandbox, not on this machine"`.
- No session capabilities (no resume, list, close or fork) and no
  `configOptions`.

**session/new:** 263ms.

- It returns
  `{"sessionId":"<conversation id>","models":{"currentModelId":"anthropic/claude-haiku-4-5","availableModels":[…one…]}}`.
- `cwd` and `mcpServers` are ignored.

**Turns:**

| turn | ms | notes |
|---|---|---|
| "Remember HERON, reply OK" (`_meta.clientRequestId:"s7-basic-1"`) | 10,729 | includes ~7s sandbox provision (stderr `stage=provision`, `broker`, `network`, `model`, `turn`). `agent_thought_chunk` 37 |
| "`echo hi > x.txt && ls`, then the code word" with `execute=ask` | 6,763 | permission request → my allow after 2s → ran in the sprite → "HERON" |

**The stream is claude-agent-acp's own, passed through unchanged**: the same
`tool_call` shapes, `_meta.claudeCode`, `usage_update.cost` (cumulative per
conversation: 0.0166 → 0.0237) and `session_info_update` titles. The
differences:

- **The `session/prompt` response is only `{"stopReason":"end_turn"}`**, with
  no `usage`. Cost comes only from `usage_update`.
- **The permission request is the sandbox agent's own:** same options, ids
  and `_meta.permission`. `toolCall.locations` is moved to
  `toolCall._meta["fountain.sandboxLocations"]` (`/home/sprite/x.txt`), and
  the allow-always label says "sprite/".
- **No `fs/*` or `terminal/*` request ever arrived.**

**Unanswered permission** (`STEP=hold`, client connected, never answers):

- The request arrived at 10.06s.
- At **310.17s (300.1s later)** this arrived:
  `{"sessionUpdate":"tool_call_update","status":"failed","rawOutput":"User refused permission to run tool","_meta":{"claudeCode":{"nonExecutionKind":"permission-rule","toolName":"Bash"}}}`.
  The agent continued ("Permission denied — the command wasn't executed.")
  and the prompt returned `end_turn` at 311.8s.
- **The client gets no cancel for its open request id 0.** The client has to
  infer from the `failed` update, keyed by `toolCallId`, that the card is
  stale. It also has to cope with sending a late answer to a request that no
  longer waits (not tried).

**Killing the client mid-turn, then `session/load` from a new process:**

| case | server side | after `session/load` |
|---|---|---|
| default policy, killed during `sleep 20; echo done > z.txt` | **the turn continued** and completed (z.txt written; the next turn "yes, sleep ran for 20 seconds and created z.txt") | load (110ms) replayed everything up to the moment of load. **No live updates for the rest of the turn** arrived in the next 30s, although the turn finished during them. A new prompt then worked and kept context |
| `execute=ask`, killed with the request unanswered | the turn stayed `running`, blocked on the request. It was **not re-sent** to the new client after load | a prompt 45s later failed: `{"code":-32603,"message":"could not send the prompt: http 400: conversation_busy"}`. After the 5-minute refusal, a second load replayed the whole turn (refused tool) and a prompt answered "OSPREY; no, permission was refused." |

- **Replays contain no `user_message_chunk`.** The user's own prompts are
  missing from Fountain's replay, but present in claude-agent-acp's local one.
- `agent_thought_chunk` is replayed (133 chunks in one load).
- The editors doc says "close the laptop mid-turn, and the turn continues"
  and that is true. But a reopened client sees the end of that turn only by
  loading again after it ends, or by watching the conversation outside ACP
  (`fountain conv stream`).

**`_meta.fountain.*` and related extensions worth using:**

- `_meta.clientRequestId` on `session/prompt`: correlate a block's `send`
  with the Fountain turn. Accepted; nothing echoed it back over ACP.
- `toolCall._meta["fountain.sandboxLocations"]`: show sandbox paths as text,
  never as local links.
- `usage_update.cost` and `_claude/rateLimit` pass through.
- `session/new` `_meta`: `channelId` (+ `freshSession`) would let a block
  re-find its conversation without storing the id; `sandboxMode`/`sandboxId`.
  Not tried; the block can just store the session id, which *is* the
  conversation id.
- Detached waits (`stopReason:"waiting"`, `_meta.fountain.timeout`) are for
  long approvals. They are agent-initiated, and claude-agent-acp doesn't do
  it, so not tried.

**Fountain offers the agent no terminal and no fs.** The docs and the stderr
log say so, and nothing arrived.

## 6. Other local agents

| agent | here? | result |
|---|---|---|
| Codex: `@agentclientprotocol/codex-acp` 2.1.0 (+ installed `codex-cli 0.155.1` via `CODEX_PATH`, ChatGPT login) | installable (18MB without the optional `@openai/codex` binary) | **works.** `authMethods` api-key/chat-gpt, `loadSession`, the same session capabilities as claude. Default model `gpt-6-astra`. "Reply with just OK" → `OK` in 5.7s. `echo hi > x.txt` ran **without a permission request** (inside codex's own workspace-write sandbox), with no `terminal/create` and with `terminal_info`/`terminal_exit` `_meta`. No `cost` in `usage_update`; the prompt response carries `usage` |
| Gemini CLI (`gemini --acp`; formerly `--experimental-acp`) | not installed (`@google/gemini-cli` 0.62.0 on npm), no `~/.gemini` | not tested |
| opencode (`opencode acp`) | not installed (`opencode-ai` 1.18.34 on npm) | not tested. Fountain notes it never sends `session/request_permission` |

## Consequences for M6b

**What the ACP block needs:**

- **An `AcpAdapter`** (the plan's `AgentAdapter`, specialised) that spawns
  any ACP agent command with a `cwd` and env. The per-agent config is a
  command line plus a few `_meta` defaults:
  - `claude-acp`: `settingSources: []`, then `set_config_option model`;
  - `codex-acp`;
  - `fountain acp --agent X [--permission execute=ask]`.
- **Turn state from the protocol itself:**
  - `working` while our `session/prompt` is outstanding;
  - `needs-input` while a `session/request_permission` is open;
  - `done`/`idle` from `stopReason`;
  - the current tool from `tool_call`/`tool_call_update` by `toolCallId`.
- **Permissions:**
  - the incoming request goes to the UI, the push and `illogical call %N
    approve`;
  - the answer is an `optionId` picked by `kind` from the request;
  - `deny` = `reject_once`; `interrupt` = `session/cancel` plus `cancelled`
    for every open request (the spec's MUST);
  - "always allow" lives in the block config and is answered by the daemon,
    not with `allow_always`, which writes into the user's repo.
  - Clear the card when a `tool_call_update` for that `toolCallId` goes
    `completed`/`failed` (Fountain's timeout sends nothing else).
- **Cost:** per-turn deltas of `usage_update.cost.amount` (cumulative per
  session). Per-turn tokens come from the prompt response where present
  (claude, codex; not Fountain).
- **Log:** the block log is the ACP frame stream.
  - `capture --text` renders `user_message_chunk` (our own sends, logged by
    us), `agent_message_chunk` and tool calls.
  - Reboot: store the session id. Then `session/load` if the transcript is
    wanted again (claude replays the user prompts too), or `session/resume`
    without replay when our log already has it.
- **Daemon restarts (M2b): the fd store is mandatory.** claude-agent-acp
  aborts the turn on EOF. Put the pipe fds in the store, persist the open
  permission request ids/options and our next request id, and read whole
  lines. The S6 MCP relay is not needed.
- **Fountain:** the same block works with `fountain acp`. Note that:
  - turns survive the block closing;
  - after a reattach mid-turn, the block must `session/load` again when it
    sees the conversation go idle (poll, or a later prompt), because load
    doesn't stream the rest;
  - a permission raised to a dead daemon is not re-sent. With `ask`, a
    daemon restart that loses the pipe costs a refused tool after 5 minutes.
    The fd store avoids this here too, since `fountain acp` is a local child
    like any other.

**What changes compared with the stream-json design:**

- Gone:
  - `--permission-prompt-tool` (undocumented),
  - the permission MCP server and relay,
  - `--verbose` and `--replay-user-messages`,
  - parsing `result` for turn ends.
  The adapter wraps all of that, and the same code drives codex and Fountain.
- New dependencies:
  - an npm adapter with its own pinned Claude Code (2.1.280, behind the
    installed CLI). Pin it, as Fountain does, or set
    `CLAUDE_CODE_EXECUTABLE`;
  - its defaults: Opus, all setting sources, and exit-on-EOF.
- Lost:
  - the adapter does **not** finish a turn after EOF;
  - live tool output (stream-json gave `tool_result` at the end too, so this
    is no worse).
- Gained: `session/cancel` with a clean `cancelled`, a standard permission
  shape with option kinds, `session/load` replay, titles, and model and mode
  config options.

**ACP terminals as terminal blocks: not viable with today's adapters.**
Neither claude-agent-acp nor codex-acp calls `terminal/create`, whatever the
client offers. Their `terminal_output` `_meta` arrives in one piece after the
command exits, so a "terminal block" for a tool call could only be a
read-only replay. Show the command and its output inside the agent block.
Revisit if an adapter starts calling client terminals; the client side is
small (`acp.ts` has it, unexercised).

## Still open

- **Client terminals with a real agent.** None here uses them. Gemini CLI
  reportedly uses client `fs`; it is not installed.
- **Answering after Fountain's 5-minute refusal**, and whether a late answer
  to the stale id errors.
- **Following a Fountain turn live after reattach** (SSE/`fountain conv
  stream` alongside ACP), and whether `channelId` resume helps.
- **Agent blocks in a VM:** `claude-acp` inside a sprite with the pipes over
  exec.
- **`session/cancel` with a permission request open** (the client answers
  `cancelled`) was not run.
- **Permission waits of hours** with claude-agent-acp. 25 minutes showed no
  timeout.
- **Whether `CLAUDE_CODE_EXECUTABLE=~/.local/bin/claude`** (2.1.286) behaves
  the same as the bundled 2.1.280.

## Cleanup

- **Processes:** every adapter, keeper and bridge was killed. These include
  two orphaned adapters and keepers from failed FIFO attempts, and a stale
  `cat`. `ps` shows no `claude-agent-acp`, bundled `claude`, `codex
  app-server`, `codex-acp`, `fountain acp`, or `sleep 600`/`3600` keepers. The
  live illogicald on 7681/7690 was never contacted.
- **Fountain** (profile `default`):
  - No agent was created. The existing `Arena anthropic/claude-haiku-4-5` was
    used.
  - Four conversations were created and then deleted with `fountain conv
    delete`: 57467309, 363cd176, 44f13873 and 47851482. `conv show` now
    returns 404.
  - Their four ephemeral sandboxes show `terminated`.
  - Nothing else was touched; reads were `agent list` per profile and
    `conv show`/`list`, `sandbox list`.
- **Repo:** the stray `/home/me/dev/jhgaylor/illogical/.claude/` (created
  by allow-always, holding only `Bash(echo hi *)`) was deleted. No other file
  outside `spikes/s7-acp/` was changed. `work/scratch` has its own
  `.git` and `.claude/settings.local.json` (git-ignored scratch).
- **Left in place:**
  - spike Claude sessions under
    `~/.claude/projects/-home-jake-dev-jhgaylor-illogical-spikes-s7-acp-work-*`;
  - the logs in `work/`.
  - `work/node` (290MB) and `work/codex` (18MB) were deleted after the run.
- **Credentials:** none stored. The Fountain debug log prints only
  `credentials="https://managoat.com (profile default)"`. A scan of this
  directory for every token-like value in `~/.fountain/credentials`,
  `~/.codex/auth.json` and `~/.claude/.credentials.json` found 0 hits.
- **Spend** (from `usage_update.cost`):
  - local claude-agent-acp about **$0.57** across ~25 sessions, including
    one accidental Opus run (~$0.13). `_claude/rateLimit` shows the
    subscription five-hour window at 13–14%;
  - Fountain about **$0.07** (haiku);
  - codex: two tiny prompts, cost not reported (ChatGPT login).
