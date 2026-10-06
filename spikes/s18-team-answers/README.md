# S18: team answers (permission hooks, follow-ups, notification answers)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s18-team-answers/<file>`.

Run 2026-10-02 on geek, for #45 (before M29, #46). **Result: go on all three,
with one change of method and two gaps for M29 to fill.**

- **(a) Permission prompts: go, with Claude Code's `PermissionRequest`
  hook.** It fires only when Claude Code is about to show a permission
  dialog, carries the tool, its input and Claude's own "always allow"
  suggestions, and can allow, allow always, or deny with a message. **But the
  dialog shows at the same time**, and a "Yes" typed in the terminal doesn't
  tell the hook: it keeps waiting and its later answer is silently dropped.
  The card has to be closed by other signals.
- **(b) Follow-ups: go, but not by typing.** Typing text and Enter into the
  pane works when the prompt is empty, but it merges with whatever the driver
  has half typed and submits both. Instead, a **background `Stop` hook with
  `asyncRewake`** (plus the same at `SessionStart`) waits for a follow-up and
  exits 2 with it, which wakes an idle Claude Code and runs it as the next
  instruction. The driver's draft stays where it was. Mid-turn, it's
  delivered at the next model step.
- **(c) Answering from a notification through control: go on desktop and
  Android, as far as headless Chrome can show; iOS likely falls back to
  opening the card.** A service worker can load the non-extractable device
  key from IndexedDB, check a certificate chain, open a WebSocket, run the
  Noise IK handshake and send one `approve`: 20 ms inside the worker, cold or
  warm, against a local stand-in for the daemon. The app can't do it yet,
  because the directory and pins it needs are in `localStorage`, which a
  worker can't read, and control-routed pushes carry no approve/ask payload.

## Setup

- Claude Code **2.1.287** (`~/.local/bin/claude`), `--model haiku` (Haiku
  4.5), driven in a 120x40 PTY by `tui.py` (S13's driver, plus extra
  arguments) with pyte 0.8 in `work/venv`.
- Each run: `claude --model haiku --setting-sources local --settings
  work/settings-<name>.json [args]` in `work/tui` (its own git repo). The
  settings files only add hooks, all pointing at `hook.py`. Nothing was
  written to `~/.claude/settings.json`.
- Google Chrome **153.0.8010.52** headless through Playwright 1.63 (the
  repo's `@playwright/test`), Node 22.23.2, and `vite` to bundle the
  prototype. The daemon stand-in is `crates/e2e/examples/interop responder`.
- No illogicald was run: none of the three questions needed one.

## Files

| file | what it is |
|---|---|
| `hook.py` | one hook for every event: records stdin to `work/hooks/<n>-<event>-<mode>.json`, replies by mode (`record`, `allow`, `always`, `deny`, `wait`, `inbox`, …) |
| `settings.py` | writes `work/settings-<name>.json`, one per hook setup |
| `tui.py`, `run.sh`, `key.sh`, `waitfor.sh` | start a TUI run, type into it (prompts come from `prompts/`), wait for a screen |
| `show.py`, `fixtures.py` | print recorded hook inputs; copy chosen ones (paths scrubbed) into `crates/daemon/tests/fixtures/` |
| `sw-proto/` | Q3: `page.ts` makes device keys and registers `sw.ts`; `build.mjs` bundles both; `run.mjs` delivers pushes over the DevTools protocol |
| `crates/daemon/tests/fixtures/s18-hook-*.json` | recorded hook inputs (below) |

To rerun: `python3 -m venv work/venv && work/venv/bin/pip install pyte`,
`python3 settings.py`, then for example `./run.sh rec1 record touch` and
`./waitfor.sh rec1 ask`. For Q3, from `web/`: `cargo build -p illogical-e2e
--example interop`, `node ../spikes/s18-team-answers/sw-proto/build.mjs`,
then copy `interop fixtures` to `work/sw-dist/fixtures.json` and run
`node ../spikes/s18-team-answers/sw-proto/run.mjs`.

## Q1: permission prompts in a terminal

### Which hook

2.1.287 has `PermissionRequest` ("Run before permission prompt", matched by
tool name). Recorded in one default-mode run (`rec1`) that asked to `touch
a.txt`:

| event | when | what it gets |
|---|---|---|
| `PreToolUse` | every tool call, before Claude Code decides whether to ask | `tool_name`, `tool_input`, `tool_use_id` |
| `PermissionRequest` | only when a dialog is about to show (0.05 s after `PreToolUse`) | `tool_name`, `tool_input`, `permission_suggestions`, `permission_mode`; **no `tool_use_id`** |
| `Notification` | 6.0 s later (both runs) | `notification_type: "permission_prompt"`, `message: "Claude needs your permission"`, nothing about the tool |

- **`PreToolUse` is the wrong place.** It fires for tools that never ask: in
  `acceptEdits` it fired for a `Write` that ran with no dialog (`accept1`
  01–02). Blocking there would hold every tool call, and its `ask` decision
  only forces the dialog.
- **`Notification` is what today's `needs_input` comes from.** It names no
  tool, so it can't make a card.
- **`PermissionRequest` fires in the cases that matter:**
  - **default** mode (Bash);
  - **`acceptEdits`**: the Write ran without one, the Bash asked;
  - **`--continue`**: same `session_id`, and the mode came back (`cont1`);
  - **subagents**: a Task subagent's Bash asked with `agent_id` and
    `agent_type` in the input (`accept1` 21);
  - **AskUserQuestion**, when no `PreToolUse` hook answered it (`ask1`).
    M6c's hook answers with `permissionDecision: "allow"`, which should skip
    this step; M29's tests should confirm it, and the matcher should leave
    `AskUserQuestion` to M6c.
- **It doesn't fire in `bypassPermissions`**, where nothing asks (`bypass1`).
- **No `tool_use_id`.** To tie a card to its tool call (and to the
  `PostToolUse` that ends it), match it to the `PreToolUse` just before it
  from the same session (and `agent_id`) with the same `tool_name` and
  `tool_input`.

### Answering

The reply is `{"hookSpecificOutput": {"hookEventName": "PermissionRequest",
"decision": …}}`:

| decision | what happened |
|---|---|
| `{"behavior": "allow"}` | the dialog closed, the tool ran; the transcript says "Allowed by PermissionRequest hook" (`wait1`) |
| `{"behavior": "allow", "updatedPermissions": [<a suggestion>]}` | as above, and the rule was written to `.claude/settings.local.json`; running the same command again asked nothing (`always1`) |
| `{"behavior": "deny", "message": "Sam said no: do a dry run first."}` | "Error: Sam said no: …" and "Denied by PermissionRequest hook"; the turn carried on and the model read the message (`deny1`) |

- **The suggestions aren't the dialog's options.** For `python3 -c
  "print(5*5)"` the dialog offered "don't ask again for: `python3 *`", but
  the hook's suggestion was an `addRules` for the exact command. A card should
  offer the hook's suggestions as given. A broader rule is possible (an
  `addRules` entry is plain data), but it's a choice we'd be making for the
  user.
- **The schema error message** spells the shape out: `PermissionRequest
  decision must be {"behavior": "allow"} or {"behavior": "deny", "message":
  "..."}`.

### When someone also answers in the terminal

**The dialog doesn't wait for the hook.** It showed while the `wait` hook was
still blocked (`wait1`), so the terminal and the card are both live, and the
first answer wins:

| who answered first | what happened to the other |
|---|---|
| the hook (allow) | the dialog closed |
| the terminal, **Yes** | the tool ran; **the hook was not signalled** and kept waiting; when released 45 s later, its `deny` was ignored without a word |
| the terminal, **No** | the hook got SIGTERM |
| the terminal, **Esc** | the hook got SIGTERM, and the turn was interrupted |

So `illogical ask` can't rely on Claude Code to tell it that it lost. M29 has
to close the card when any of these arrive for the same session:
`PostToolUse` or `PostToolUseFailure` for the matched `tool_use_id`, the next
`PreToolUse`, `Stop`, `UserPromptSubmit`, or SIGTERM to the hook. Then the
hook exits 0. Hook timeouts have no maximum (S13), so set a long one.

## Q2: follow-ups into a terminal agent

### Typing into the prompt

- **Text and Enter in one write submit,** when the prompt is empty: a short
  line (`oneshot`) and a bracketed paste of three lines followed by `\r`
  (`paste`) both went through.
- **But typing fights the driver.** With "jake was typing this and has not
  pressed enter" in the box, the follow-up was appended and both were
  submitted as one prompt: `❯ jake was typing this and has not pressed
  enterReply with just the word oneshot.` (`inbox2`).
- **No-go as the method.** At best a fallback for agents with no hooks.

### A background hook that wakes the model

The hook schema in 2.1.287 has `asyncRewake`: "runs in background and wakes
the model on exit code 2 (blocking error)". With it on a `Stop` hook,
`hook.py inbox` waits after every turn for `work/queue.txt` (in M29: for the
daemon to hand it a follow-up), then writes it to stderr and exits 2.

- **Idle: it wakes Claude Code.** With the driver's half-typed text in the
  box, writing the queue file started a new turn. The tool ran ("sam done"),
  and the draft was still there afterwards, untouched (`inbox1`).
- **Busy: delivered within the turn.** A follow-up queued during a `sleep
  15` tool call went in right after that tool's result, at the next model
  step. The turn finished the original request and the follow-up (`inbox2`).
  The waiter that fires is the one from the previous turn's `Stop`, which
  stays alive until a newer one replaces it.
- **Before the first turn:** the same hook on `SessionStart` (`source:
  "resume"` under `--continue`) woke a resumed session that hadn't had a
  turn yet (`inbox3`). That covers panes M2 restores with `claude
  --continue`.
- **What the model sees** is a `UserPromptSubmit` whose prompt is a
  `<task-notification>` plus a `<system-reminder>` (fixture
  `s18-hook-rewake-prompt.json`). By default it reads `Stop hook blocking
  error from command "Stop": <text>`, and the transcript line says "Stop
  hook feedback". Two fields marked `@internal` in the schema change both:
  `rewakeMessage` ("A teammate sent a follow-up through illogical. Treat it
  as the user's next message:") and `rewakeSummary` ("Follow-up from Sam").
  They worked. Haiku followed the instruction in all four runs, with and
  without them.
- **Waiters:** one starts after every turn. Each one exits when a newer one
  replaces it (a pid file here; in M29, the daemon keeps one waiter per pane),
  and all of them died with Claude Code.

Not needed: a synchronous `Stop` hook that returns `decision: "block"` with
the follow-up only helps while the agent is busy. `asyncRewake` covers both
idle and busy.

## Q3: answering from a notification through control

### What the app does today (from the code)

- `web/public/sw.js` answers a notification's Approve, Deny or one-tap answer
  with `fetch("/api/blocks/<pane>/call/…")` on its own origin. That only
  works when the page came from the daemon. Through control, the origin is
  control, which has no such API.
- **Control-routed payloads** carry `daemon` and `pane` but no `approve` or
  `ask` (M21's gap). They only open the pane.
- **Device keys and enrollment are in IndexedDB** (`illogical-device`:
  `keys`, `enrollment:<control>`), which a service worker can open.
- **The directory and the pins are in `localStorage`** (`DIR_KEY`,
  `PINS_KEY` in `control.ts`), which a service worker can't. Those hold the
  daemon's Noise key from a certificate this browser checked, and the roots
  pinned for other accounts (teammates' daemons).

### The prototype

`sw-proto/` bundles the app's own `web/src/e2e` code into a classic service
worker (18 KB unminified). On a push, the worker:

1. loads the device keys the page made (non-extractable X25519 and Ed25519,
   from IndexedDB);
2. fetches `fixtures.json`, Rust's certificate fixtures standing in for
   control's `/api/devices`, and runs `evaluate()` on it;
3. opens a WebSocket from the worker to the Rust responder, a stand-in for
   relay plus daemon, and runs the Noise IK handshake;
4. sends `POST /api/blocks/5/call/approve` over the channel and reads the
   reply.

Pushes were delivered with `ServiceWorker.deliverPushMessage`. "Cold" means
the worker was stopped first (`ServiceWorker.stopAllWorkers`). Five of each
(times from the push event; wall is from the CDP call to the page hearing
back):

| | keys loaded | chain checked | channel up | answered | wall |
|---|---|---|---|---|---|
| cold (median) | 0.6 ms | 3.9 ms | 19.6 ms | **19.9 ms** | 46 ms |
| warm (median) | 0.5 ms | 3.7 ms | 18.5 ms | **18.8 ms** | 22 ms |

- **Everything worked:**
  - the key came back with `extractable: false`;
  - the daemon's certificate chained to the root;
  - every request got its 201 with the right method and path.
- **The relay adds round trips.** The handshake and the request are one
  round trip each, and S15 measured 18.7 ms for one through the hosted relay
  from geek. Expect roughly 60–100 ms on a good network, and more on
  cellular.

### What M29 needs for it

- **Keep what the worker needs in IndexedDB:** the checked directory (daemon
  id, Noise key, relay URL) and the pins. The worker could also refetch
  `/api/devices` from control with its cookie and re-check the chain, which
  took 4 ms here. Teammates' daemons still need the pins.
- **Put `approve` / `ask` (and the daemon id) in control-routed payloads.**
  They're encrypted to the subscription, so control still can't read them.
- **Build `sw.js`** from a bundled entry, as the prototype does, instead of
  a hand-written file in `public/`.
- **On the daemon,** the answer arrives over the device's own channel, so
  authz and "answered by" use the device's principal, the same as any page
  request.

### Pending (needs real phones)

- **Android Chrome:** cold start of a killed worker, on cellular, through
  the relay.
- **iOS Safari:**
  - whether Web Push shows notification `actions` at all; if it doesn't,
    iOS opens the card instead;
  - whether a WebSocket and X25519 work inside a service worker there.

## Fixtures

Paths scrubbed to S13's form (`/work/tui`, `/home/user/...`), written by
`fixtures.py`:

| file | event |
|---|---|
| `s18-hook-permission.json` | `PermissionRequest`, Bash, default mode (suggestions: `addDirectories`, `setMode`) |
| `s18-hook-pretooluse-bash.json` | the `PreToolUse` just before it (has `tool_use_id`) |
| `s18-hook-notification-permission.json` | the `Notification` 6 s later |
| `s18-hook-permission-accept-edits.json` | `PermissionRequest` in `acceptEdits` (suggestion: `addRules` for the command) |
| `s18-hook-permission-subagent.json` | `PermissionRequest` from a Task subagent (`agent_id`, `agent_type`) |
| `s18-hook-permission-ask.json` | `PermissionRequest` for AskUserQuestion, with no `PreToolUse` hook |
| `s18-hook-stop.json` | `Stop` (`last_assistant_message`, `background_tasks`) |
| `s18-hook-sessionstart-resume.json` | `SessionStart` under `--continue` (`source: "resume"`) |
| `s18-hook-rewake-prompt.json` | the `UserPromptSubmit` an `asyncRewake` follow-up becomes |

## What this changes in M29 (#46)

- **The hook is `PermissionRequest`,** matching every tool but
  `AskUserQuestion` (M6c's). Pair it with `PreToolUse` for the
  `tool_use_id`.
- **Close cards without being told:**
  - on `PostToolUse` / `PostToolUseFailure` for the tool call;
  - on the session's next `PreToolUse`, `Stop` or `UserPromptSubmit`;
  - on SIGTERM.

  A terminal "Yes" doesn't reach the hook.
- **"Always allow" offers Claude Code's suggestions** (often the exact
  command), not the dialog's broader wording.
- **Follow-ups use an `asyncRewake` inbox hook** on `Stop` and
  `SessionStart`, not typing. Drop "typed when the agent is idle". A
  follow-up no longer touches the PTY, so it never fights the driver. Who may
  send one is still a policy question, because an instruction to an agent on
  someone's machine runs code there. #46's rule (drive rights, or a trust
  grant) still fits.
- **The inbox fields `rewakeMessage` / `rewakeSummary` are `@internal`.**
  Use them, but don't depend on them: the default wording also works.
- **Answering from a notification:**
  - move the directory and pins to IndexedDB;
  - add `approve`/`ask` to control-routed payloads;
  - bundle the service worker;
  - plan for iOS opening the card.

## Leftovers

- Claude Code wrote its usual state for the scratch folder:
  - transcripts under `~/.claude/projects/-home-jake-dev-jhgaylor-illogical--claude-worktrees-agent-a13dc8ae465e3b3fb-spikes-s18-team-answers-work-tui/`;
  - a folder-trust entry for `…/spikes/s18-team-answers/work/tui` in `~/.claude.json`.
- `work/` (gitignored) holds the venv, the runs' screens and hook records
  (`work/keep/`), the scratch repo, and the prototype's build.
- All runs, hooks and the responder were stopped.
