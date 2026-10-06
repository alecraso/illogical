# S13: questions and forms from agents
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s13-questions/<file>`.

Run 2026-10-01 on geek. **Result: the M6c design works, with four
corrections.** AskUserQuestion arrives as a clean form elicitation from
claude-agent-acp, survives a held-pipes restart, and the Claude Code hook
skips the picker. What changes:

- **The capability shape is `elicitation: {form: {}, url: {}}`, not
  `{form: true, url: true}`.** The SDK's parser silently drops a boolean
  (`defaultOnError(… → undefined)`), so `true` means "not supported".
- **Without the capability, AskUserQuestion doesn't exist at all.** Both
  0.85.0 and 0.81.2 put it in `disallowedTools`. It never arrives as a
  permission request, and the model says "I don't have access to an
  AskUserQuestion tool" and asks in plain text. Today's illogical blocks
  therefore never see the tool (the "Approve/Deny card for AskUserQuestion" in
  PLAN.md doesn't happen).
- **Stop is `session/cancel`, and nothing else.** The adapter withdraws the
  open request itself with `$/cancel_request` and the turn ends `cancelled`
  in ~10ms. Answering the elicitation with `{action:"cancel"}` on its own
  does *not* stop the turn: the tool fails and the model carries on.
- **The `_meta._askUserQuestionCustomAnswer` marker is gone in 0.85.0** for
  non-JetBrains clients. Recognise the form by the tool call instead.

Fountain doesn't offer AskUserQuestion at all. Codex does elicit, but only in
its plan collaboration mode; in default mode its question goes nowhere. Claude
Code hook timeouts have no maximum.

## Setup

- `@agentclientprotocol/claude-agent-acp` **0.85.0** (npm latest; ACP SDK
  1.5.1, claude-agent-sdk 0.3.286) and **0.81.2** (Fountain's pin; ACP SDK
  1.5.0, claude-agent-sdk 0.3.280), each with its bundled Claude Code.
- `@agentclientprotocol/sdk` 1.5.1 schema, read for `ClientCapabilities`.
- `@agentclientprotocol/codex-acp` **2.1.1** (`--omit=optional`) on the
  installed `codex-cli 0.155.1` via `CODEX_PATH`, ChatGPT login, default
  model `gpt-6-astra`.
- fountain CLI **v0.21.0**, profile `default`, the existing agent `Arena
  anthropic/claude-haiku-4-5` (`f0577ee9-…`).
- Claude Code **2.1.286** (`~/.local/bin/claude`) for the TUI tests.
- `@modelcontextprotocol/sdk` 1.31.0 for the MCP server in Q6.
- Bun 1.4.2, Node 22.23.2, Python 3 with pyte (in `work/venv`).
- The adapters ran in `work/scratch/` and the TUI in `work/tui/`, each its own
  git repo. Every ACP session used `settingSources: []` and was switched to
  haiku with `session/set_config_option`. The TUI ran
  `claude --model haiku --setting-sources local --settings work/hook-settings-*.json`.
  `CLAUDECODE`, `CLAUDE_CODE_*`, `CLAUDE_PID`, `CLAUDE_EFFORT` and `AI_AGENT`
  were removed from the environment.

**Code** (logs in `work/*.ndjson` and `work/*.out`):

| file | what |
|---|---|
| `acp.ts` | S7's client, plus `custom` handlers for agent→client requests and logging of notifications |
| `claude-acp.sh` | launches claude-agent-acp `85` or `81` with the parent session's env removed |
| `q1.ts` | Q1: `V=85\|81 CAPS=objects\|bools CASE=single\|multi\|preview\|other\|decline\|cancelaction\|stop\|nocaps` |
| `q1load.ts` | Q1: how `session/load` replays an answered question |
| `held-pipes.sh`, `held-client.ts` | Q2 (`V=85`): kill the client with an elicitation open, answer it from a new client |
| `q3.ts` | Q3: `fountain acp` with elicitation declared |
| `q4.ts` | Q4: codex-acp (`CONFIG=collaboration_mode=plan,…`) |
| `tui.py`, `hook.py` | Q5: Claude Code in a 120x40 PTY (pyte screen dumps, keys through a FIFO) and the PreToolUse hook |
| `mcp-elicit.mjs`, `q6.ts` | Q6: a stdio MCP server that elicits a form and a URL |

```sh
npm install --prefix work/node85 @agentclientprotocol/claude-agent-acp@0.85.0
npm install --prefix work/node81 @agentclientprotocol/claude-agent-acp@0.81.2
npm install --prefix work/codex --omit=optional @agentclientprotocol/codex-acp@2.1.1
npm install --prefix work/mcp @modelcontextprotocol/sdk@1.31.0
python3 -m venv work/venv && work/venv/bin/pip install pyte
V=85 CASE=multi mise exec bun@1.4.2 -- bun q1.ts
./held-pipes.sh
work/venv/bin/python tui.py answer work/hook-settings-answer.json "<prompt>"
```

## 1. ACP capability and shapes

### The capability

The SDK schema (`ClientCapabilities.elicitation: ElicitationCapabilities`):

```ts
elicitation?: { form?: {_meta?}|null, url?: {_meta?}|null, _meta? } | null
// "Supplying `{}` explicitly advertises form support."
```

The adapter tests `this.clientCapabilities?.elicitation?.form` for truthiness,
but it gets the capabilities after the SDK's zod parse, which has
`form: defaultOnError(zElicitationFormCapabilities.nullish(), () => undefined)`.

| `clientCapabilities.elicitation` | 0.85.0 | 0.81.2 |
|---|---|---|
| `{form:{}, url:{}}` | elicitation | elicitation |
| `{form:true, url:true}` | **dropped**; tool disabled | **dropped**; tool disabled |
| absent (illogical today) | tool disabled | tool disabled |

"Tool disabled": `disallowedTools = elicitationSupport.form ? [] :
["AskUserQuestion"]`. With the tool gone, the model answered "I don't have
access to an AskUserQuestion tool" (once adding "I'll ask directly: red or
blue?"), `end_turn`, no tool call, no permission request.

`url` gates MCP URL elicitations, `elicitation/complete` and MCP OAuth
(`startMcpAuthentication`); `form` gates AskUserQuestion, MCP form
elicitations and the refusal-fallback dialog ("retry with <model>?").

### One single-select question (0.85.0)

Around it: a `tool_call` (`name:"AskUserQuestion"`, `title:"Asking for your
input"`, `kind:"other"`, `rawInput:{}`, `status:"pending"`), then a
`tool_call_update` carrying the full `rawInput.questions` and
`title:"<question>"`, then the request 4ms later:

```json
{"jsonrpc":"2.0","id":0,"method":"elicitation/create","params":{
 "mode":"form","sessionId":"27307874-…","toolCallId":"toolu_016SiCMRLiS5dvvk9WmusQqb",
 "message":"Which color do you prefer?",
 "requestedSchema":{"type":"object","properties":{
  "question_0":{"type":"string","title":"Color","oneOf":[
    {"const":"Red","title":"Red","description":"Warm, energetic, bold"},
    {"const":"Blue","title":"Blue","description":"Cool, calm, serene"}]},
  "question_0_custom":{"type":"string","title":"Other",
    "description":"Type your own answer, or add a note to the option you chose above (optional)."}}}}}
```

Our response and what followed (8ms):

```json
{"jsonrpc":"2.0","id":0,"result":{"action":"accept","content":{"question_0":"Red"}}}
{"sessionUpdate":"tool_call_update","toolCallId":"toolu_016S…","_meta":{"claudeCode":{"toolName":"AskUserQuestion",
  "toolResponse":{"questions":[…],"answers":{"Which color do you prefer?":"Red"}}}}}
{"sessionUpdate":"tool_call_update","toolCallId":"toolu_016S…","status":"completed",
  "rawOutput":"Your questions have been answered: \"Which color do you prefer?\"=\"Red\". You can now continue with these answers in mind.", "content":[…same text…]}
```

Reply: "You prefer red." Turn 5.5s including our 1.5s delay.

- `message` is the question text for one question, and the generic "Please
  answer the following questions." for several. `title` is the question's
  `header`; with several questions each field's `description` is its
  question text.
- No `required`: everything is optional, as in the CLI.
- `toolCallId` is the tool-use id, so the card attaches to the tool call.

### Several questions with a multi-select, notes and "Other"

```json
"question_0":{"type":"string","title":"Colour","description":"Which colour do you prefer?","oneOf":[{"const":"Red","title":"Red","description":"…"},{"const":"Blue",…}]},
"question_0_custom":{"type":"string","title":"Other","description":"Type your own answer, or add a note to the option you chose above (optional)."},
"question_1":{"type":"array","title":"Fruit","description":"Which fruits do you like?","items":{"anyOf":[{"const":"Apple","title":"Apple","description":"…"},{"const":"Pear",…},{"const":"Plum",…}]}},
"question_1_custom":{"type":"string","title":"Other","description":"Type your own answer to add to your selection above (optional)."},
"question_2":{"type":"string","title":"Pet",…"oneOf":[…Cat, Dog…]},
"question_2_custom":{…"title":"Other"…}
```

Response, and what the tool got:

```json
{"action":"accept","content":{"question_0":"Red","question_0_custom":"dark red please",
  "question_1":["Apple","Plum"],"question_1_custom":"kiwi","question_2_custom":"a parrot"}}

"answers":{"Which colour do you prefer?":"Red","Which fruits do you like?":"Apple, Plum, kiwi","Which pet do you prefer?":"a parrot"},
"annotations":{"Which colour do you prefer?":{"notes":"dark red please"}}
```

- A single-select pick plus text: the pick is the answer, the text becomes
  `annotations[q].notes`.
- Text alone: the text is the answer (the CLI's "Other").
- Multi-select: picks and text are comma-joined (items containing `, ` or `"`
  are JSON-quoted).

The model got `"Which colour do you prefer?"="Red" notes: dark red please,
"Which fruits do you like?"="Apple, Plum, kiwi", "Which pet do you
prefer?"="a parrot". Read the answers carefully…` and replied "You chose dark
red for colour, apple and plum for fruits (plus kiwi, which wasn't an
option), and a parrot for pet".

### A preview

```json
{"const":"Sidebar","title":"Sidebar","description":"Navigation on the left side",
 "_meta":{"_claude/askUserQuestionOption":{"preview":"┌──────┬───────────────┐\n│ Nav  │ Content Area  │\n│ ║    │ goes here     │"}}}
```

Only options that have a preview carry `_meta`. Answering `Sidebar` → "You
prefer a Sidebar layout."

### "Other" alone

`{"action":"accept","content":{"question_0_custom":"green, actually"}}` →
`answers {"Which colour do you prefer?":"green, actually"}` → "You prefer
green."

### Skip (`decline`)

`{"action":"decline"}` → `answers: {}`, tool `completed` with `rawOutput:"The
user did not answer the questions."`, turn `end_turn`, reply "You didn't
answer the question." The turn continues.

### Answering `cancel` (without `session/cancel`)

`{"action":"cancel"}` → `tool_call_update status:"failed"`, `rawOutput:"Tool
permission request failed: Error: Tool use aborted"`,
`_meta.claudeCode.nonExecutionKind:"permission-rule"`. **The turn carries on**
(`end_turn`), and the model concluded "I cannot use the AskUserQuestion tool
in this non-interactive session". So `cancel` is not a Stop button.

### Stop (`session/cancel` while the elicitation is open), 0.85.0 and 0.81.2

```
5.345 -> session/cancel {sessionId}
5.349 <- {"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":0}}
5.357 <- usage_update
5.359 <- {"id":4,"result":{"stopReason":"cancelled",…}}
8.346 -> {"id":0,"result":{"action":"cancel"}}      (late; ignored, no error)
```

- The agent withdraws its own request with the JSON-RPC `$/cancel_request`
  notification. The client should close the card on it; an answer is not
  needed, and a late one is silently dropped.
- No final `tool_call_update` for the tool call: it stays at the last
  pending update. Close it on `cancelled`.
- Transcript: `tool_result` `is_error:true`, "The user doesn't want to
  proceed with this tool use. The tool use was rejected…"; the next turn the
  model said "No, you rejected the question before answering it."
- 0.81.2 identical (14ms → `$/cancel_request`, `cancelled`).

### 0.81.2 vs 0.85.0

Same request shape, field names, answers and result text, with one
difference: **0.81.2 marks every `question_<n>_custom` with
`"_meta":{"_askUserQuestionCustomAnswer":{"questionId":"question_0","isCustomAnswer":true}}`;
0.85.0 sends that marker only to JetBrains AIR clients**
(`toolCallCapabilities.air.client`). 0.81.2 also repeats `kind:"other"` on the
`tool_call_update`.

### The transcript

- Live: the `tool_call_update` before the request has `rawInput.questions`;
  the one after has `_meta.claudeCode.toolResponse.{questions,answers,annotations}`
  (structured), then `completed` with the `rawOutput` text.
- Claude Code's JSONL: `tool_use` `AskUserQuestion` with the questions;
  `tool_result` with the same text, and `toolUseResult` holding
  `{questions, answers, annotations}`.
- `session/load` replay (`q1load.ts`): `user_message_chunk`, one `tool_call`
  with the full `rawInput.questions` (`status:"pending"`), one
  `tool_call_update` `completed` with the `rawOutput` text (no structured
  `toolResponse`), then the reply.

## 2. Restart with held pipes: works

`held-pipes.sh` (0.85.0): client #1 initializes with elicitation, asks turn 1
("my favourite bird is the merlin"), starts turn 2 (AskUserQuestion), receives
`elicitation/create` id 0, saves `{elicitationRequestId, params, nextId}` and
SIGKILLs itself.

- The adapter stayed alive for 20s with nobody attached (`Ssl`).
- Client #2 attached: backlog 0 frames. It sent
  `{"jsonrpc":"2.0","id":0,"result":{"action":"accept","content":{"question_0":"Blue"}}}`.
- 11ms later the tool completed with `"answers":{"Which colour do you
  prefer?":"Blue"}`, and the orphaned prompt (client #1's id 5) returned
  `end_turn` at 1.9s: "Your colour is blue and your favourite bird is the
  merlin."
- Turn 3 on the same process: "Blue is your colour, and the merlin is your
  favourite bird."

So the daemon persists the request id and `params` (the whole form) in the
block log, exactly as for a permission request in S7.

## 3. Fountain: no questions at all

`fountain acp --agent f0577ee9-…` with `elicitation:{form:{},url:{}}`:

- `initialize` is the same as in S7 (no new capabilities). stderr logs the
  client's elicitation capability and nothing more.
- The prompt took 16.9s (sandbox provision) and ended with "I don't have
  access to an \"AskUserQuestion\" tool. However, I can ask you directly:
  **Which color do you prefer: red or blue?**". No `elicitation/create`, no
  permission request, no tool call.
- The sandboxed claude-agent-acp's own client (Fountain's runner) doesn't
  declare form elicitation, so the tool is disabled inside the sandbox. The
  CLI binary contains no "elicitation" string, and `fountain acp --help`
  mentions only approval prompts (`--permission ask`, denied after the
  server's timeout).
- No time limit to measure, since nothing is ever pending.

## 4. Codex: elicits in plan mode, loses the question in default mode

codex-acp 2.1.1 bridges Codex's `item/tool/requestUserInput` to
`elicitation/create` when `elicitation.form != null`.

**Plan collaboration mode** (`session/set_config_option collaboration_mode=plan`,
`reasoning_effort=low`): the model called `request_user_input`.

```json
{"jsonrpc":"2.0","id":0,"method":"elicitation/create","params":{
 "sessionId":"01a0f7f4-…","toolCallId":"call_H5MmjBN0VwZREK9Gh4N3MLOB","mode":"form",
 "message":"Codex needs your input to continue.",
 "requestedSchema":{"type":"object","properties":{
  "colour":{"title":"Which colour do you prefer, red or blue?","description":"Colour",
    "_meta":{"codex":{"isOther":true,"isSecret":false}},"type":"string","oneOf":[
      {"const":"Red","title":"Red","description":"Choose red."},
      {"const":"Blue","title":"Blue","description":"Choose blue."},
      {"const":"None of the above","title":"None of the above","description":"Provide a different answer in the note field."}]},
  "colour_note":{"type":"string","title":"Additional answer or note",
    "_meta":{"codex":{"questionId":"colour","role":"user_note","isSecret":false}}}},
  "required":["colour"]},
 "_meta":{"codex":{"autoResolutionMs":null}}}}
```

- `{"action":"accept","content":{"colour":"Blue"}}` → "You prefer blue." in
  12.2s.
- Different from Claude: fields are named by Codex's question id; the
  question text is the field `title` and the header its `description`;
  "Other" is a "None of the above" option plus a `<id>_note` field;
  `required` is set; `isSecret` exists; the message is generic.
- No `tool_call` update was sent for `toolCallId`. The only other signal was
  `session_info_update` `_meta.codex.threadStatus.activeFlags:["waitingOnUserInput"]`.
- `autoResolutionMs` (null here) is a Codex-side deadline: when set, codex-acp
  gives up and returns no answers when it expires.
- Decline or cancel → `{answers:{}}`.

**Default mode** (twice, medium and low effort): the model called
`request_user_input_async` (seen in `~/.codex/sessions/…/rollout-*.jsonl`),
which returned `{"accepted":true}` straight away. codex-acp doesn't bridge it,
so nothing reached the client. The model then called `sleep 60000` in a loop
and the turn never ended (killed at 300s both times).

## 5. Claude Code TUI hook

`work/hook-settings-*.json`, e.g.:

```json
{"hooks":{"PreToolUse":[{"matcher":"AskUserQuestion","hooks":[{"type":"command","command":"…/hook.py answer","timeout":900}]}]}}
```

**(a) Hook stdin** (2.1.286):

```json
{"session_id":"47f67b40-…","transcript_path":"/home/me/.claude/projects/…/47f67b40-….jsonl",
 "cwd":"…/work/tui","scratchpad_dir":"/tmp/claude-1000/…/scratchpad","prompt_id":"d765cc07-…",
 "permission_mode":"default","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion",
 "tool_input":{"questions":[
   {"question":"Which colour do you prefer?","header":"Colour","options":[{"label":"Red","description":"A warm, bold colour"},{"label":"Blue","description":"A cool, calming colour"}],"multiSelect":false},
   {"question":"Which fruits do you like?","header":"Fruit","options":[…Apple, Pear, Plum…],"multiSelect":true},
   {"question":"Which pet do you prefer?","header":"Pet","options":[…Cat, Dog…],"multiSelect":false}]},
 "tool_use_id":"toolu_01VjPcYqkckCw26ZfpWqGFQf"}
```

Yes, `tool_input.questions` is there, in the tool's own shape (options with
`preview` when the model gives one).

**(b) Answering from the hook skips the picker.** The hook printed:

```json
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{
  "questions":[…unchanged…],
  "answers":{"Which colour do you prefer?":"Red","Which fruits do you like?":"Apple, Pear","Which pet do you prefer?":"a parrot"},
  "annotations":{"Which colour do you prefer?":{"notes":"dark shade please"}}}}}
```

The PTY never showed the picker (no "Enter to select" in any of the 1s screen
dumps). The screen showed:

```
● User answered Claude's questions:
  ⎿  · Which colour do you prefer? → Red
     · Which fruits do you like? → Apple, Pear
     · Which pet do you prefer? → a parrot

● You prefer red (dark shade), like apple and pear, and chose a parrot as your pet preference.
```

The `tool_result` was the same text as through ACP (`…="Red" notes: dark shade
please, …`), so the note reached the model although the screen doesn't show
it.

**(c) The `answers` shape** is the adapter's: `answers: {[question text]:
string}`, multi-select comma-joined, free text as the value; per-question
`annotations: {[question text]: {notes}}`. Keep `questions` in
`updatedInput` (the tool's `call()` reads both).

**(d) A silent hook** (exit 0, no output): the normal picker appeared at once:

```
 ☐ Colour
Which colour do you prefer, red or blue?
❯ 1. Red
     The colour red
  2. Blue
     The colour blue
  3. Type something.
  4. Chat about this
Enter to select · ↑/↓ to navigate · Esc to cancel
```

Down + Enter → "User answered Claude's questions: … → Blue", "You prefer
blue."

**(e) Timeouts.**

- The settings schema is `timeout: number().positive().optional()` ("Timeout
  in seconds for this specific command"): **no maximum**. The default for
  command hooks is `600000` ms (10 minutes).
- `timeout: 900` with a hook that slept **660s**: the answer was used, no
  picker, "Worked for 11m 3s". Values above the 10-minute default are honoured.
- `timeout: 5` with a hook that sleeps 10s: the hook got **SIGTERM at exactly
  5.0s**. Claude Code then showed the normal picker, with no error on screen
  (the spinner said "running PreToolUse hook · 6s" and then the picker
  replaced it). Answering the picker worked. The transcript records
  `{"type":"hook_cancelled","hookName":"PreToolUse:AskUserQuestion","durationMs":5022,"timedOut":true,"timeoutMs":5000}`.
  So a timed-out hook falls back to the picker by itself.

**(f) Responsiveness while a hook blocks** (`timeout: 3600`, hook waits for a
file):

- The spinner says "running PreToolUse hook · Ns · esc to interrupt". Typing
  into the prompt box works.
- **Esc** interrupted the turn at once. The hook got **SIGTERM**. The screen
  said "User declined to answer questions · Which color do you prefer? (Red /
  Blue)", and the transcript `tool_result` was the usual "The user doesn't
  want to proceed with this tool use…" plus "[Request interrupted by user for
  tool use]".
- **Ctrl-C** (with an empty prompt box) did the same: interrupt, SIGTERM to the
  hook, "User declined to answer questions".

## 6. MCP form and URL elicitations through claude-agent-acp: work

`mcp-elicit.mjs` (stdio, passed in `session/new` `mcpServers`) has two tools
that call `elicitInput`. Each tool call first got an ordinary
`session/request_permission` (allowed), then:

```json
{"id":1,"method":"elicitation/create","params":{"mode":"form","sessionId":"d424b8b9-…","message":"Order details",
 "requestedSchema":{"type":"object","properties":{
   "size":{"type":"string","title":"Size","enum":["S","M","L"],"enumNames":["Small","Medium","Large"]},
   "qty":{"type":"integer","title":"Quantity","minimum":1,"maximum":9},
   "gift":{"type":"boolean","title":"Gift wrap"}},"required":["size"]}}}
→ {"action":"accept","content":{"size":"M","qty":2,"gift":true}}

{"id":3,"method":"elicitation/create","params":{"mode":"url","sessionId":"d424b8b9-…","message":"Sign in to S13",
 "url":"https://example.com/s12-signin","elicitationId":"s12-signin-1"}}
→ {"action":"accept"}
<- {"method":"elicitation/complete","params":{"elicitationId":"s12-signin-1"}}
```

- The MCP server received exactly our content, and `{action:"accept",
  content:{}}` for the URL.
- **No `toolCallId`** on MCP elicitations (unlike AskUserQuestion), so the
  card hangs off the session, not a tool call.
- The schema is passed through as the server wrote it: here the legacy
  `enum` + `enumNames`, not `oneOf`. (codex-acp converts `enumNames` to
  `oneOf`; claude-agent-acp doesn't.)
- `elicitation/complete` came 4ms after our accept, because the server
  completes immediately. A real OAuth flow would complete later.
- Not tried: a real OAuth sign-in (`startMcpAuthentication` for HTTP MCP
  servers needing auth).

## Consequences for M6c

**Declare** `clientCapabilities.elicitation: {form: {}, url: {}}` in
`crates/daemon/src/agent/mod.rs` (objects, not booleans). Until that ships,
agent blocks on claude-agent-acp can't ask questions at all; the plan's
"approval card for AskUserQuestion" never happens.

**Recognise AskUserQuestion** by its tool call, not by `_meta`:

- `elicitation/create` with `mode:"form"` and a `toolCallId` whose
  `tool_call` has `_meta.claudeCode.toolName == "AskUserQuestion"` (or
  `name`). The preceding `tool_call_update` has `rawInput.questions`, which
  is the clean source for the card (question, header, options with
  description and preview, multiSelect).
- Map fields: `question_<n>` ↔ `questions[n]`, `question_<n>_custom` is its
  "Other" box. Don't rely on `_meta._askUserQuestionCustomAnswer`: 0.85.0
  dropped it for non-AIR clients.
- Answer with labels: single → `question_<n>: "<label>"`; multi →
  `question_<n>: ["<label>", …]`; Other → `question_<n>_custom: "<text>"`
  (alone = the answer; with a single-select pick = a note; with a
  multi-select = an extra item).
- Previews: `oneOf[i]._meta["_claude/askUserQuestionOption"].preview`, or
  `rawInput.questions[n].options[i].preview`.

**Buttons:**

| card action | send | effect |
|---|---|---|
| Submit | `{action:"accept", content}` | tool `completed`, answers used |
| Skip | `{action:"decline"}` | tool `completed`, "The user did not answer the questions.", the turn goes on |
| Stop | `session/cancel` only | agent sends `$/cancel_request {requestId}`, prompt ends `cancelled` in ~10ms; no answer needed, a late one is ignored |

- Do **not** map Stop to `{action:"cancel"}`: on its own that fails the tool
  and the turn continues with a confused model. The spec's "answer pending
  requests on cancel" applies to permissions; for elicitations the agent
  withdraws them itself.
- Handle `$/cancel_request` generally: close any open card whose JSON-RPC id
  matches. Also close it on the prompt's `cancelled` response.

**Restart:** works like a pending approval. Persist the request's id and full
`params` in the block log; after a held-pipes restart, any client answers it
by that id and the turn continues.

**Generic forms:** MCP forms arrive unmodified from the server, without
`toolCallId`. The renderer must accept `enum`/`enumNames` as well as
`oneOf`/`anyOf`, plus integer/number/boolean with min/max and `required`. URL
mode: show `message` + `url`, answer `accept` when opened (or `decline`), and
close on `elicitation/complete` with that `elicitationId`.

**Fountain:** no AskUserQuestion and no elicitations; the agent asks in plain
text. Nothing to render. Leave it, and note it in the docs; ask Fountain to
declare form elicitation for its sandboxed adapter and relay
`elicitation/create` if questions are wanted there.

**Codex:** handle its form too, as a generic form (it is one: `oneOf` with a
"None of the above" option and a `<id>_note` field, `required`, a generic
message). Recognising it as a question card is optional; `_meta.codex.isOther`
and `role:"user_note"` identify the parts. Only plan collaboration mode asks
this way. In default mode Codex uses `request_user_input_async`, which
codex-acp drops, and the turn spins on `sleep` until cancelled. That is a
codex-acp gap; the block can only show it as a long-running turn.

**The Claude Code hook design works:**

- `PreToolUse` matcher `AskUserQuestion`; stdin has `tool_input.questions`,
  `tool_use_id`, `session_id`, `transcript_path`, `cwd`.
- Print `allow` + `updatedInput {questions, answers, annotations?}` and the
  picker never appears; the answers and notes reach the model.
- Exit 0 silently ("Answer in terminal", or not in illogical) and the picker
  works normally.
- **No maximum timeout.** Set the hook's `timeout` very large (e.g. `86400`)
  to get "waits indefinitely". The 10-minute default would otherwise cut it.
  When a timeout does expire, Claude Code SIGTERMs the hook and shows the
  picker by itself, so "give up just before the limit" is unnecessary; the
  hook only has to tell the daemon on SIGTERM.
- **The hook must handle SIGTERM.** Esc and Ctrl-C in the TUI kill it with
  SIGTERM and record "User declined to answer questions". `illogical ask`
  should then withdraw the card (like `$/cancel_request`).
- The TUI stays responsive while the hook waits (typing, Esc, Ctrl-C).

## Still open

- **Hours-long hook waits.** 11 minutes with `timeout: 900` worked; the
  schema has no maximum, but nothing longer was run.
- **Pending elicitation after Fountain's or Codex's restarts.** Not
  applicable to Fountain (no elicitations); codex-acp on held pipes not
  tried.
- **Codex `autoResolutionMs` non-null** (when Codex sets a deadline for its
  own question) was never seen.
- **A real MCP OAuth sign-in** through `elicitation.url` (the adapter's
  `startMcpAuthentication` for HTTP servers), and a URL elicitation whose
  `elicitation/complete` arrives later than the accept.
- **The refusal-fallback form** ("<model> declined… Retry with <fallback>?",
  field `choice`, values `retry_fallback`/`cancelled`) is also sent when
  `form` is declared. Not triggered here; the generic renderer covers it.
- **Several clients answering at once** ("first answer wins"): a second
  answer to an already-answered id was not sent. A late answer after
  `$/cancel_request` was ignored without an error.
- **AskUserQuestion inside a subagent** (the adapter attributes it to the
  parent tool call) was not tried.

## Cleanup

- **Processes:** all adapters, keepers, TUIs and hook processes exited or were
  killed (two orphaned `codex-acp`/`codex app-server` from the default-mode
  runs were killed by pid). `ps` shows no `claude-agent-acp`, `codex-acp`,
  `fountain acp`, `tui.py`, `hook.py` or keeper from this spike. The running
  illogicald instances and their own `claude-agent-acp` and `fountain acp
  --agent captain-picard` were never contacted or signalled.
- **Fountain:** no agent created. One conversation, `b2a565e2-…`, was created
  and deleted with `fountain conv delete`; `conv show` returns 404.
- **Repo:** only `spikes/s13-questions/` was written. `git status` shows the
  same entries as at the start plus `?? spikes/s13-questions/`. The
  pre-existing untracked `.claude/` holds only other agents' `worktrees/`; no
  `settings.local.json` appeared there or anywhere else. `work/scratch` and
  `work/tui` are their own git repos and have no `.claude/` directory (no
  allow-always was chosen).
- **Claude Code settings:** `~/.claude/settings.json` untouched (mtime 07:05,
  before the spike), no `~/.claude/settings.local.json`. Hooks came only from
  `--settings work/hook-settings-*.json` with `--setting-sources local`.
- **Left in place:**
  - Claude session transcripts under
    `~/.claude/projects/-home-jake-dev-jhgaylor-illogical-spikes-s13-questions-work-{scratch,tui}/`;
  - Codex rollouts in `~/.codex/sessions/2026/10/01/` (three runs);
  - a trust entry for `…/s13-questions/work/tui` in `~/.claude.json`
    (`hasTrustDialogAccepted`), written by answering the folder-trust dialog;
  - the logs in `work/`. They contain the account e-mail from
    `_auth/status_update` notifications, but no credentials.
  - `work/node85`, `work/node81`, `work/codex`, `work/mcp` and `work/venv`
    (~650MB) were deleted after the run.
- **Credentials:** none stored. A `grep -F` of this directory for the 18
  token-like values in `~/.fountain/credentials`, `~/.codex/auth.json` and
  `~/.claude/.credentials.json` found 0 hits.
- **Spend:**
  - claude-agent-acp, from `usage_update.cost`: **$0.32** over 16 haiku
    sessions;
  - Fountain: **$0.02** (one haiku turn);
  - the TUI: six haiku turns in five sessions, not metered; about $0.10 at
    the same per-turn cost;
  - Codex: three runs on the ChatGPT login (two spun on `sleep` for 5
    minutes), cost not reported.
