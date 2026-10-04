# S6: non-terminal blocks (agent blocks and browser blocks)

Run 2026-10-01 on geek. **Result: both block types are feasible.** Three
assumptions in the M6 plan need to change:

- the iframe should be `allow-same-origin`;
- the browser-block port should not be 8443;
- the proxy must be HTTP-aware and enforce its own Origin rule.

Details are under "Consequences for M6".

## Setup

- **Claude Code 2.1.286** (`~/.local/bin/claude`), `--model haiku`, which
  resolves to `claude-haiku-4-5-20251001`.
  - The nested `claude` runs through `agent/claude.sh`, which unsets
    `CLAUDECODE`, `CLAUDE_CODE_*`, `CLAUDE_PID`, `CLAUDE_EFFORT` and
    `AI_AGENT`.
  - Every run also used `--strict-mcp-config --setting-sources ""`, so
    none of Jake's settings or hooks loaded.
  - Its cwd was `work/claude/`.
- **wisp:** local API on `127.0.0.1:7788` (7789 serves the same API under
  `--api-host wisp.widgets.wtf`).
  - Sprite `illogical-s6-vite`: Ubuntu 24.04, no Node, internet works.
  - Node 22.20.0 was installed from the nodejs.org tarball.
  - **Vite 8.3.2** (`npm create vite@latest -- --template vanilla`) and
    **Next.js 16.3.8** (Turbopack, `create-next-app`) ran as `sprite-env`
    services on 5173 and 3000.
- **Bun 1.4.2** (`mise exec bun@1.4.2 --`), Node 22.23.2 on the host.
- **Browsers:** Playwright 1.63.0 from `web/node_modules`.
  - Chromium is system Chrome 153 (`channel: "chrome"`).
  - WebKit is Playwright's build 2359, installed into `work/` with six
    missing system libs unpacked from .debs (no sudo); deleted afterwards.
- tailscale 1.102.4.

**Code** (all in this directory):

| file | what |
|---|---|
| `agent/claude.sh` | launches the nested `claude` with the session env removed |
| `agent/lib.ts`, `multiturn.ts` | stream-json driver; Q2 |
| `agent/perm-mcp.ts` | stdio MCP server with an `approve` tool. Modes: auto policy, hold N seconds, or relay to a Unix socket with reconnect and resend |
| `agent/perm.ts`, `eof.ts`, `restart.ts`, `fake-daemon.ts`, `held-pipes.sh`, `resume.ts` | Q3, Q4a, Q4b, the fd-store stand-in, Q5 |
| `agent/samples/*.ndjson` | raw event streams (`*.perm.ndjson` is the MCP server's view) |
| `proxy.ts` | local TCP port to Sprites proxy WebSocket, one per connection. `REWRITE=1` rewrites Host/Origin |
| `wrapper.ts` | stand-in app page that frames `?src=` in a sandboxed iframe (`SANDBOX=` to change) |
| `hmr.mjs`, `next-hmr.mjs` | Playwright: load, edit in the sprite, measure HMR, tap/type/scroll |
| `origin-matrix.sh` | WebSocket upgrade with chosen Host/Origin/identity against a test daemon |
| `attack/evil.html` | the "malicious dev server page" used in Q8 |

```sh
SPRITE_TOKEN="$(cat ~/.local/share/wisp/token)" bun proxy.ts <sprite> 5173 18173
tailscale serve --bg --https=10000 http://127.0.0.1:18173
SPRITE_TOKEN=... [BROWSER=webkit DEVICE="iPhone 15" FRAME=1 FRAME_HOST=http://127.0.0.1:18174] node hmr.mjs https://geek.tail1234.ts.net:10000/b/7/
```

## A. Claude Code headless as an agent block

### 1. Flags (2.1.286)

- **`-p/--print`** is required for all of the following.
- **`--output-format stream-json` requires `--verbose`.** Without it you get
  `Error: When using --print, --output-format=stream-json requires --verbose`
  and exit 1.
- **`--input-format stream-json`** takes one JSON user message per stdin line.
- **`--include-partial-messages`** needs `-p` and stream-json output. It adds
  token-level `stream_event`s.
- **`--replay-user-messages`** needs stream-json both ways. It echoes each
  stdin message back on stdout as a `user` event, which is useful as an ack.
- **`--permission-prompt-tool <mcp tool>`** is accepted and works, but is
  **hidden from `--help`**. Because it is undocumented, treat it as an API we
  have to re-check on upgrades.
- **`--permission-prompts host|none`** (new):
  - `host`: the SDK host or the permission-prompt tool answers;
  - `none`: auto-deny anything that would prompt.
- **`--permission-mode`:** `acceptEdits | auto | bypassPermissions | manual |
  dontAsk | plan`. The init event reports `"default"` when none is given.
- **Sessions:**
  - `-r/--resume [id]`, `-c/--continue`;
  - `--session-id <uuid>`, so the daemon can choose the id up front;
  - `--fork-session` (with resume/continue);
  - `--no-session-persistence` (print mode).
- **MCP:** `--mcp-config <files or inline JSON...>`; `--strict-mcp-config`
  ignores every other MCP source.
- **Tools:** `--tools` (the built-in set; `""` means none),
  `--allowedTools`/`--allowed-tools`, `--disallowedTools`.
- **Other flags that matter for a daemon:**
  - `--setting-sources`, `--settings`, `--bare`, `--restricted`;
  - `--include-hook-events`;
  - `--max-budget-usd`;
  - `--append-system-prompt`, `--fallback-model`, `-n/--name`.

### 2. One long-lived, multi-turn process: yes

- One pid answered turn 1 ("remember PELICAN, run `echo turn1`"). Then a
  second line on the same stdin got "PELICAN".
- Each turn ends with a `result` event and the process keeps waiting for
  input.
- Stdin EOF after the last result gives exit 0 within about 360ms.

Stdin line:

```json
{"type":"user","message":{"role":"user","content":"<text>"}}
```

**Events** (the agent block's event mapping; sample in
`agent/samples/multiturn.ndjson`):

| event | key fields | block use |
|---|---|---|
| `system/init` (again at the start of every turn) | `session_id`, `cwd`, `model`, `permissionMode`, `tools[]`, `mcp_servers[{name,status}]`, `claude_code_version`, `apiKeySource` | store session_id in config; `working` |
| `system/thinking_tokens` | `estimated_tokens`, `estimated_tokens_delta` | progress |
| `assistant` | `message.content[]`: one event per block (`thinking`, `text`, `tool_use{id,name,input}`); `usage`; `parent_tool_use_id` (subagents) | transcript, current tool |
| `user` | the replayed prompt, or `message.content[{type:"tool_result",tool_use_id,content,is_error}]` plus `tool_use_result{stdout,stderr,interrupted}` | tool output |
| `rate_limit_event` | `rate_limit_info{status,resetsAt,rateLimitType,unifiedWindows}` | show when limited |
| `result/success` (also error subtypes) | `is_error`, `result` (final text), `num_turns`, `duration_ms`, `ttft_ms`, `stop_reason`, `terminal_reason`, `session_id`, `total_cost_usd`, `usage`, `modelUsage`, `permission_denials[]`, `queued_turn_count` | `idle`/`done`, cost |

**`total_cost_usd` and `modelUsage` are cumulative for the process**, not
per turn: $0.0134 after turn 1, $0.0151 after turn 2. Per-turn cost is the
difference. After a resume the count starts again.

### 3. Permission prompts through an MCP tool: yes, and it blocks

Wiring (inline config works):

```
--mcp-config '{"mcpServers":{"perm":{"type":"stdio","command":"bun","args":["perm-mcp.ts"]}}}'
--permission-prompt-tool mcp__perm__approve
```

**Plain `echo hello` never reaches the tool.** Claude Code treats read-only
Bash as allowed without asking. The tests used commands that write a file
(`echo hello > hello.txt`), and those prompt.

**Request** (`tools/call` params):

```json
{"name":"approve",
 "arguments":{"tool_name":"Bash",
              "input":{"command":"echo hello > hello.txt","description":"…"},
              "tool_use_id":"toolu_…"},
 "_meta":{"claudecode/toolUseId":"toolu_…","progressToken":2}}
```

**Response:** a normal MCP tool result whose text content is JSON.

```json
{"content":[{"type":"text","text":"{\"behavior\":\"allow\",\"updatedInput\":{…the input…}}"}]}
{"content":[{"type":"text","text":"{\"behavior\":\"deny\",\"message\":\"spike policy: only echo/ls/pwd are allowed\"}"}]}
```

- **Allow:** the command ran. `updatedInput` can rewrite the input.
- **Deny:**
  - Claude gets a `tool_result` with `is_error:true` and the message as its
    content;
  - the turn still ends in `result/success`, with
    `permission_denials:[{tool_name,tool_use_id,tool_input}]`;
  - haiku relayed the reason and didn't retry.
- **Blocking:** the tool held the answer for 60s, then 420s (7 min), with
  `MCP_TOOL_TIMEOUT` unset.
  - Claude waited both times, with no timeout and no error. It ran the command
    after the late allow (result `duration_ms` 62,656 and 422,481).
  - The binary describes `MCP_TOOL_TIMEOUT` and a per-server `timeout` as a
    hard wall-clock limit that progress notifications don't extend. The
    default could not be read from the minified code, and nothing fired
    within 7 minutes.
- **`--allowedTools 'Bash(echo:*)'` still asked** for
  `echo again > hello.txt`, because the redirect isn't covered by that rule.

### 4. Daemon restarts: an agent can survive one

**(a) Losing the pipes**:

| what happened | claude did |
|---|---|
| stdin EOF between turns | exit 0 straight away |
| stdin EOF mid-turn, with a permission request held 10s | got the allow, ran the command, wrote "Done.", emitted `result`, exit 0 |
| pipe owner SIGKILLed mid-turn (stdin EOF and stdout reader gone together) | **no SIGPIPE death.** It got the late allow, ran the command, wrote the whole turn to its transcript, and exited about 14s later. Whatever it wrote to stdout in that window was lost |

So without extra work a daemon restart ends the process after the current
turn. The turn's side effects and transcript are complete; only the live
stream is lost.

**(b) A pending permission request across a daemon restart**
(`restart.ts`, `fake-daemon.ts`):

- **The path:** claude → `perm-mcp` (stdio, a child of claude) → Unix socket
  → fake daemon.
- **The test:** daemon #1 received the request and was SIGKILLed without
  answering. The MCP server retried every 500ms. 30s later daemon #2 came up;
  the MCP server reconnected and **resent the pending request**, and daemon #2
  allowed it.
- **The result:** claude, which had been waiting the whole time, ran the
  command and finished the turn.

**Held pipes** (`held-pipes.sh`, standing in for the systemd fd store):

- claude's stdin and stdout were FIFOs that a keeper process held open
  read-write.
- **Daemon #1** sent turn 1 and went away while a permission request was
  pending.
- **For 20s** nothing read stdout. claude stayed alive; its output waited in
  the pipe buffer.
- **Daemon #2** reopened the FIFOs, drained the backlog (tool_result,
  "Done.", the result), and sent turn 2 on the **same process**. It answered
  with the word from turn 1.

**Conclusion:** an agent block can survive a daemon restart with no lost turn,
if three things hold:

1. claude runs in its own scope, like a pane's shell (S3), so the unit's
   `KillMode=mixed` doesn't kill it;
2. the daemon stores claude's stdin write end and stdout read end in the
   systemd fd store, next to the PTY masters;
3. the permission MCP server is a relay with its own retry loop, which
   reconnects to the daemon socket and resends pending requests. It is a
   child of claude, so it outlives the daemon.

If the daemon is down long enough, more than 64KB of unread stdout blocks
claude. That is harmless; it resumes when drained.

### 5. Resume after SIGKILL: works

- **The test:** turn 1 set the code word HERON. Turn 2 was SIGKILLed (exit
  137) while its permission request was pending. A new process with
  `--resume <id>` and the same stream-json and MCP flags answered "HERON".
- **The resumed process keeps the same `session_id`** and appends to the same
  transcript. `--resume <id> --fork-session` gives a new id with the context
  intact.
- **The killed turn is patched on resume.** Claude adds a synthetic
  `tool_result`: `[Tool call interrupted: the session ended before this
  call's result was recorded, so its outcome is unknown. …]`. The model then
  said the outcome was unknown, which was correct: the command never ran.
- **Transcripts** are in
  `~/.claude/projects/<cwd with / replaced by ->/<session_id>.jsonl`.
  - Besides messages, the file holds queue-operation, attachment, last-prompt
    and cost-state records.
  - After a reboot, it is the source of truth for whatever stdout the daemon
    missed.
- When claude dies, the MCP server exits on stdin EOF, so nothing is
  orphaned.

## B. Browser blocks

### 6. Vite through the Sprites proxy: works, including HMR

**The bridge** (`proxy.ts`): each local TCP connection opens one
`wss://…/v1/sprites/{name}/proxy` and sends `{"host":"localhost","port":5173}`.
After `{"status":"connected","target":"localhost:5173"}`, raw bytes flow
both ways.

- Connect time is 1–4ms on local wisp.
- HTTP keep-alive and the HMR WebSocket upgrade pass through untouched.

**HMR test** (Chrome, `http://127.0.0.1:18173/`):

- `src/main.js` was replaced with a self-accepting module
  (`import.meta.hot.accept()`).
- `style.css` and `main.js` were then rewritten inside the sprite with the
  Sprites `fs/write` API.

| | page load | CSS HMR | JS HMR | full reload? |
|---|---|---|---|---|
| bridge, local | 871ms | 45ms | 16ms | no (`window` marker kept) |

The vanilla template's own `main.js` has no `hot.accept`, so editing it
triggers a full reload, which is Vite's normal behaviour. CSS edits are true
HMR either way.

**Vite settings:**

- **Without a rewriting proxy,** Vite answers any Host other than localhost
  with 403 `Blocked request. This host ("geek.tail1234.ts.net") is not
  allowed`. That needs `server.allowedHosts: ["geek.tail1234.ts.net"]`.
- **HMR needs no `hmr.clientPort/host/protocol` config.** The client builds
  its socket URL from `import.meta.url`, so through `:10000` it uses
  `wss://geek.tail1234.ts.net:10000/?token=…`.
- **The `?token=` is Vite's guard against cross-site WebSocket hijacking.**
  It is in the served HTML, so other origins can't read it.
- **Base path:** `--base /b/7/` works through a non-stripping proxy, with HMR
  at `wss://…:10000/b/7/?token=…`. Stripping the prefix in the proxy instead
  can't work: the dev server emits root paths like `/@vite/client`. **So a
  `/b/%N/` scheme needs every dev server told its base**: Vite `--base`, Next
  `basePath`, and so on.

**Next.js 16.3.8 (Turbopack):**

- Fast Refresh works through the bridge. Edits to `app/page.js` showed up in
  28ms and to `globals.css` in 31ms, and React state was kept (a counter
  stayed at 2) with no reload.
- Through the tailnet name it needs `allowedDevOrigins:
  ["geek.tail1234.ts.net"]`. Without it Next logs `Cross-origin access to
  Next.js dev resources is blocked by default`, the HMR socket fails (serve
  returns 502), and edits only arrive by full reload (9.3s).

**Zero-config variant** (`REWRITE=1`): the proxy rewrites `Host` to
`localhost:<port>` and a non-null `Origin` to `http://localhost:<port>`, and
drops `Tailscale-*` and `X-Forwarded-*`.

- Both servers then worked through `:10000` with **no config at all**: Vite
  with no `vite.config.js`, Next with `next.config.mjs` set to `{}`.
- Vite: CSS 26ms, JS 15ms. Next: 28ms and 30ms, with state kept.

### 7. Separate origin on `tailscale serve --https=10000`: works

- **Certificate:** `curl https://geek.tail1234.ts.net:10000/` verified with
  no `-k` (`ssl_verify_result=0`). It is the node's existing Let's Encrypt
  cert: `CN=geek.tail1234.ts.net`, issuer `Let's Encrypt YE2`, valid to
  2026-12-30.
- **HMR through serve:**
  - Vite: CSS 15ms, JS 16ms, no reload, socket
    `wss://geek.tail1234.ts.net:10000/?token=…`.
  - Next: Fast Refresh 28ms.
- **Serve passes the upgrade through.** It adds `X-Forwarded-*` but keeps
  `Origin`.
- **Serve adds identity on this port too.** Every request on :10000, including
  WebSocket upgrades, carried `Tailscale-User-Login`, `Tailscale-User-Name`,
  `Tailscale-User-Profile-Pic`, `Tailscale-Headers-Info`, `X-Forwarded-For`,
  `X-Forwarded-Host` and `X-Forwarded-Proto`. Without stripping, the dev
  server (agent-written code) sees Jake's login, name and avatar.
- **Host includes the port:** `Host: geek.tail1234.ts.net:10000`.
- **8443 is taken on geek:** `wispd` listens on `*:8443` (`--public-listen
  :8443`). The plan's example port would collide; use 10000.

### 8. Isolation: what a script on :10000 can do to illogicald

**Setup:**

- A copy of the release `illogicald` ran on `127.0.0.1:17699` with its own
  state dir and `--allow-origin http://localhost:5173`. It auto-detected the
  tailnet name and owner.
- The live daemons were not contacted.
- `origin-matrix.sh` sends the WebSocket upgrade headers that serve or a
  browser would send.

| request to `/ws` | Host | Origin | result |
|---|---|---|---|
| CLI on loopback | `127.0.0.1:17699` | none | 101 |
| the app through serve :443 | `geek…ts.net` | `https://geek…ts.net` | 101 |
| **page on :10000** | `geek…ts.net` | `https://geek…ts.net:10000` | **403** |
| sandboxed (opaque) iframe | `geek…ts.net` | `null` | 403 |
| page on `http://` (port 80) of the same name | `geek…ts.net` | `http://geek…ts.net` | **101 (scheme ignored)** |
| explicit `:443` in Origin | `geek…ts.net` | `https://geek…ts.net:443` | 403 (browsers never send it) |
| suffix trick | `geek…ts.net` | `https://geek…ts.net.evil.com` | 403 |
| `--allow-origin http://localhost:5173` | `geek…ts.net` | `http://localhost:5173` | 101 (any app on that port) |
| other tailnet user | `geek…ts.net` | app's | 403 |
| :10000 mistakenly routed to the daemon | `geek…ts.net:10000` | `…:10000` | 421 (Host check) |

**In a real browser** (Chrome, page `https://geek…ts.net:10000/b/7/evil.html`
served by the dev server; `attack/evil.html`):

- **`new WebSocket("ws://127.0.0.1:17699/ws")`:** handshake 403. The daemon
  logged `rejected websocket why=origin https://geek.tail1234.ts.net:10000
  not allowed`.
- **Chrome's Private Network Access did not block it**, so don't count on it.
- **`fetch(…, {mode:"no-cors"})`:** the GET is sent and the response is
  opaque. A POST to `/ws` is sent and gets 405. No side effects, because the
  daemon has no state-changing HTTP endpoint today; every state change goes
  over the WebSocket.
- **A CORS read of the app page** is blocked (no `Access-Control-Allow-Origin`).
- **`<iframe src=http://127.0.0.1:17699/>`** **loads the full app, and it
  connects.** The daemon logged `client connected`; the framed client's
  Origin is the app's own. The daemon sends no `X-Frame-Options` or CSP
  `frame-ancestors`.
  - Since serve authenticates by source, **any** site Jake visits, not only
    :10000, can frame `https://geek…ts.net/` with a live terminal and overlay
    it (clickjacking).
  - This is a current issue, independent of M6.

**Verdict:**

- **Today's WebSocket Origin check is sufficient against a :10000 page.**
  Origin carries the port, and `hosts` holds the bare MagicDNS name, so
  `…:10000` never matches.
- There is no CSRF surface today.
- It is not airtight. The rules the daemon must enforce:

1. **WebSocket Origin must match exactly**:
   - `https://<magicdns>` (scheme included, no port);
   - or `http://127.0.0.1|localhost|[::1]:<listen port>`;
   - or an explicit `--allow-origin`;
   - otherwise reject, including `null`.
   - Today the scheme is ignored, so a future plain-HTTP listener on port 80
     of the same name would be trusted.
2. **Keep `--allow-origin` dev-only.** It must never be in the installed unit,
   because any app on that origin can drive every terminal. The live daemon
   runs without it.
3. **Never let the app be framed:** send `Content-Security-Policy:
   frame-ancestors 'none'` and `X-Frame-Options: DENY` on every app response.
4. **Any future HTTP API (M3)** needs:
   - no side effects on GET;
   - state changes only with `Content-Type: application/json`, so they are
     never a "simple" request;
   - `Origin` absent or exactly the app's;
   - `Sec-Fetch-Site` absent, `same-origin` or `none`;
   - no CORS headers, ever.

   Note that :443 and :10000 are **same-site**: same host, different port.
   SameSite cookies and `Sec-Fetch-Site: same-site` therefore don't separate
   them. Only the exact origin does.
5. **The browser-block proxy (on :10000) needs its own gate, because it
   rewrites away the dev servers' own guards.** If it rewrites Origin to
   localhost, Next's `allowedDevOrigins` and Vite's Host check stop
   protecting anything, and any site Jake visits could open the dev server's
   HMR socket through his tailnet. So the proxy must:
   - require `Tailscale-User-Login` = owner, as `access.rs` does;
   - check Host;
   - for WebSocket upgrades and non-GET requests, require `Origin` to be the
     block origin itself (or `null` only if the opaque-sandbox variant is
     used);
   - strip `Tailscale-*` and `X-Forwarded-*` before forwarding.

**One origin for every browser block means no isolation between blocks.** A
page in block A can read block B's pages and HMR token, and share its
localStorage. That is acceptable for one user whose blocks all run agent code.
Per-block origins need one serve port per block (see "Still open").

### 9. Phones (emulated) and the sandboxed iframe

**Setup:**

- `wrapper.ts` plays the app page: a 44px header and an iframe of the dev
  server under `/b/7/` through :10000.
- Two sandbox variants:
  - **opaque:** `sandbox="allow-scripts allow-forms"`;
  - **same-origin:** `sandbox="allow-scripts allow-forms allow-same-origin"`.
    Here the frame gets its own origin (`https://geek…ts.net:10000`), which
    is still not the app's.

Every run:

- edited CSS and JS through HMR;
- tapped a button twice (Playwright `tap()` with touch emulation);
- typed `héllo 123` into an input;
- scrolled the frame by 800px.

| browser / device | sandbox | frame origin | localStorage | CSS / JS HMR (ms) | reload? | taps | typed | scroll |
|---|---|---|---|---|---|---|---|---|
| WebKit / iPhone 15 (393x615 @3) | opaque | `null` | SecurityError | 24 / 14 | no | 2 | ok | 800 (scrollBy) |
| WebKit / iPhone 15 | same-origin | :10000 | ok | 26 / 14 | no | 2 | ok | 800 (scrollBy) |
| WebKit / Pixel 7 (412x795 @2.625) | opaque | `null` | SecurityError | 26 / 16 | no | 2 | ok | 800 (scrollBy) |
| WebKit / Pixel 7 | same-origin | :10000 | ok | 24 / 15 | no | 2 | ok | 800 (scrollBy) |
| Chrome / iPhone 15 | opaque | `null` | SecurityError | 26 / 16 | no | 2 | ok | 800 (wheel) |
| Chrome / iPhone 15 | same-origin | :10000 | ok | 19 / 15 | no | 2 | ok | 800 (wheel) |
| Chrome / Pixel 7 | opaque | `null` | SecurityError | 14 / 16 | no | 2 | ok | 800 (wheel) |
| Chrome / Pixel 7 | same-origin | :10000 | ok | 21 / 15 | no | 2 | ok | 800 (wheel) |

Page load was about 770–810ms everywhere. The iframe could never reach
`parent.document` (SecurityError) in either variant.

**The opaque sandbox (the plan's choice) breaks real apps:**

- **Module scripts become CORS requests from origin `null`.** Vite 6+
  allows only localhost origins by default, so the page stays blank:
  `Access to script at '…/@vite/client' from origin 'null' has been blocked
  by CORS policy`. It works only with `server.cors: true`. Once loaded, Vite's
  HMR socket accepts `Origin: null` (its token check is what guards it).
- **Next.js fails in the opaque sandbox** even with `allowedDevOrigins`: 403s
  plus CORS-blocked fonts, with no hydration and no HMR.
- **`localStorage`, `sessionStorage` and `document.cookie` throw
  SecurityError** in the opaque variant, and plenty of apps use them at
  startup.

**With `allow-same-origin`,** Vite and Next both work with no CORS config and
storage works.

- This is safe here. That attribute is only dangerous when the frame is
  same-origin with the parent (it can then remove its own sandbox), and
  browser blocks are on :10000, never :443.
- In the same-origin run, a cookie set in the frame was silently dropped:
  third-party cookies are blocked, because the stand-in parent was
  `http://127.0.0.1`.

**Limits of emulation:**

- Playwright can't wheel-scroll mobile WebKit, so WebKit scrolling was
  `window.scrollBy`. That only proves the frame scrolls, not that touch drags
  reach it.
- Playwright doesn't model on-screen keyboards, viewport resizing on focus, or
  iOS's iframe sizing quirks.
- Chrome reported `devicePixelRatio` 1 inside the frame under emulation.
- Playwright's WebKit is not iOS Safari.

**Manual checklist for Jake** (real iPhone Safari as the installed PWA, and
Android Chrome), with a dev server in a browser block next to a terminal:

1. The page renders at the block's width, not at 980px (the dev app's own
   viewport meta is honoured inside the frame).
2. A one-finger drag scrolls the frame, not the app. Scrolling to the end
   doesn't rubber-band the whole PWA, and pinch-zoom stays inside the frame.
3. Tap a button in the frame: one tap, no 300ms delay, no double-fire.
4. Focus an input in the frame:
   - the keyboard opens;
   - the field scrolls into view above it;
   - the app's layout (header, extra-keys bar) doesn't jump or stay offset
     after the keyboard closes;
   - typing, autocorrect and dictation reach the field.
5. Edit `style.css` and `main.js` in the terminal block. The frame updates in
   about a second with no reload. Then lock the phone for 1 minute and unlock:
   HMR reconnects (Vite logs `[vite] connected.` again) and an edit still
   applies.
6. Rotate the phone: the frame resizes and the dev app reflows.
7. An app that uses `localStorage` or a login cookie works (same-origin
   variant). On iOS, check whether Safari's tracking prevention partitions or
   drops the frame's cookies when the parent is the :443 app.
8. A link with `target=_blank` in the frame is either blocked (no
   `allow-popups`) or opens a new tab, by our choice, and the app's own tab is
   never navigated (no `allow-top-navigation`).
9. Kill the dev server: the block shows the load error and the attention
   badge (M6a `needs-input`).

## Consequences for M6

**M6a (browser blocks):**

- **Origin: use :10000, not 8443.** wispd owns `*:8443` on geek. The
  certificate is the node's normal one, and nothing else is needed.
- **Sandbox: `allow-scripts allow-forms allow-same-origin`.** That gives the
  frame its own :10000 origin.
  - Also add `allow-popups` only if we want links to open new tabs. Never add
    `allow-top-navigation`.
  - The plan's "no `allow-same-origin`" would break storage for most apps, and
    Next entirely. What actually protects the app is "never the app's origin",
    and the separate port provides that.
  - The plan should say this explicitly.
- **The proxy in the daemon must be HTTP-aware** (hyper), not a raw byte pipe,
  so it can:
  - rewrite `Host` (and same-origin `Origin`) to `localhost:<port>`, so dev
    servers need no config (`allowedHosts`, `allowedDevOrigins`). This was
    shown for Vite 8 and Next 16;
  - strip `Tailscale-*` and `X-Forwarded-*`, so agent code doesn't learn who
    you are;
  - enforce its own owner, Host and Origin gate (rule 5 above), since the
    rewrite disables the dev servers' own;
  - pass WebSocket upgrades through. One Sprites proxy WebSocket per TCP
    connection is fine: 1–4ms to connect on wisp.
- **Path routing is the weak point.** `/b/%N/` works only when the dev server
  is told its base: Vite `--base /b/%N/`, Next `basePath`. Proxy-side prefix
  stripping can't work, because pages emit root paths. Options, in order of
  preference:
  1. **One serve port per open browser block,** so each block's dev server
     sits at `/` on its own origin. This is zero-config and isolates blocks
     from each other. It needs the daemon to run `tailscale serve
     --https=<port>` at runtime (operator permission) and serve to accept
     arbitrary ports (unverified here).
  2. **One shared :10000** where `illogical open` passes the base to the dev
     command and the docs say "set base".
- **Add to the daemon now, independent of M6:**
  - `frame-ancestors 'none'` and `X-Frame-Options: DENY`;
  - an exact Origin match, including scheme;
  - and plan the M3 HTTP API with the CSRF rules above.

**M6b (agent blocks):**

- **The planned command needs `--verbose`:**
  `claude -p --verbose --input-format stream-json --output-format stream-json
  [--replay-user-messages] --mcp-config <inline> --strict-mcp-config
  --permission-prompt-tool mcp__illogical__approve [--session-id <uuid> |
  --resume <id>]`.
  - `--permission-prompt-tool` is undocumented; pin and re-check it on Claude
    Code upgrades.
  - Choose `--setting-sources` on purpose. With user settings loaded, Jake's
    M3 attention hooks would also fire inside agent blocks.
- **The permission tool is not called for read-only tools and commands.** So
  `needs-input` only fires for real prompts, and "always allow" policies stack
  on top of Claude Code's own rules.
- **The permission MCP server should be a small separate relay** (spawned by
  claude through `--mcp-config`). It talks to the daemon over a Unix socket,
  reconnects, and resends pending requests by `tool_use_id`. Answers are
  keyed by `tool_use_id`, which the block also sees in its `assistant`
  `tool_use` event, so approve/deny maps directly onto the transcript.
- **Restarts.** The plan's "is a pending request still answerable if the
  daemon restarts" is **yes**, if:
  - claude is in its own scope;
  - its pipe fds are in the fd store, as for PTYs;
  - the relay above is used.

  An upgrade (M2b) then loses nothing.
- **Reboot:** `--resume <session_id>` works and keeps the id. The transcript
  JSONL (not our log) is authoritative for anything said while the daemon was
  down; backfill the block log from it. An interrupted tool call comes back as
  a synthetic "outcome unknown" `tool_result`, and the block should show it
  as interrupted.
- **Cost:** `total_cost_usd` is cumulative per process. Store per-turn deltas,
  and reset after resume.
- **No timeout surfaced in 7 minutes** of a held permission request. Hours-long
  waits (approval from a phone overnight) are untested, so set
  `MCP_TOOL_TIMEOUT` explicitly (it is in ms) and handle a timeout as "deny,
  ask again".

## Still open

- **Real devices:** run the checklist above on iOS Safari (as a PWA) and
  Android Chrome. Emulation can't cover touch scrolling into the frame, the
  on-screen keyboard, or ITP cookie behaviour.
- **Per-block ports:** does `tailscale serve` accept arbitrary HTTPS ports
  (Funnel is limited to 443/8443/10000)? And can the daemon add and remove
  them at runtime without root?
- **Cookies in the frame with the real parent** (:443 framing :10000,
  same-site): not tested, because :443 could not be changed.
- **The `MCP_TOOL_TIMEOUT` default,** and whether permission waits of hours
  survive.
- **Whether an `--allowedTools` rule that covers the whole command** skips the
  permission tool.
- **Next.js under a `basePath`,** and other dev servers (SvelteKit, Astro,
  Remix).
- **An agent block on a remote host or VM** (claude inside a sprite, with the
  relay socket reached over the proxy).
- **Backpressure in the bridge:** `proxy.ts` ignores it.

## Cleanup

- **Sprites:** `illogical-s6-vite` was deleted. `GET /v1/sprites/illogical-s6-vite`
  returns 404, and no `illogical-s6-*` sprite is listed. No other sprite was
  touched.
- **tailscale serve:** the :10000 serve was removed (`--https=10000 off`).
  `tailscale serve status --json` matches the recorded original
  (`work/serve-orig.json` vs `work/serve-after.json`, `jq -S` diff empty):
  only `:443 → http://127.0.0.1:7681`.
- **Processes:** the bridges, wrappers and the test daemon on 17699 were
  killed, along with its pane. No claude, `perm-mcp`, fake-daemon or keeper
  processes remain (checked with `pgrep`). The live daemons on 7681/7690 were
  never contacted.
- **Disk:** WebKit (277MB), the unpacked .debs, the daemon binary copy and its
  state dir were deleted from `work/`.
- **Left in place:**
  - Playwright screenshots in `work/shots/`, plus logs;
  - 13 spike sessions under
    `~/.claude/projects/-home-jake-dev-jhgaylor-illogical-spikes-s6-blocks-work-claude/`.
- **Token:** the wisp token was only passed inline. A grep of this directory
  for it finds nothing.
- **Claude cost:** about **$0.10** across 14 haiku runs (from
  `total_cost_usd`; SIGKILLed turns not reported). `apiKeySource` was
  `"none"` (subscription login), so this may not be billed at all.
