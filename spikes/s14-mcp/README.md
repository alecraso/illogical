# S14: MCP (rmcp, Claude Code and Codex as clients, VM reachability)

Run 2026-10-02 on geek, for #4 (before M16, #5). **Result: go on rmcp, with
four changes to M16's plan.** rmcp 3.5 does everything M16 asks for, over
both the new stateless protocol (which Claude Code speaks) and the older
session protocol (which Codex speaks), from one `/mcp` service. What
changes:

- **Over HTTP, Claude Code kills a tool call that is silent for 60s.**
  Progress notifications reset that timer, so `run --wait` and `wait` must
  send progress (every 15s or so). They're not optional. Separately,
  **interactive Claude Code moves any MCP call still running after 120s into
  a background task**, ends the turn, and wakes the model when it finishes.
  So the default wait cap should be under 120s, returning "still running"
  plus an offset.
- **When a result has `structuredContent`, Claude Code shows the model only
  that JSON and drops the text.** Codex shows both. The one-line summary has
  to go inside the structured object (a `summary` field), not only in the
  text.
- **Neither client subscribes to resources.** Claude Code reads them on
  request, but its `subscriptions/listen` only asks for resource-list
  changes. Codex (exec) never lists them. Follow a pane with `wait` and
  `read_output`. Subscribable pane resources aren't worth building in v1.
- **A wisp guest can't reach the host at all** (bridge IP, LAN or tailnet).
  This is by design in wisp's nftables. The bridge has to be opened from the
  host: a wisp exec that runs a small relay on a guest Unix socket worked,
  with a 1ms tool-call round trip and progress flowing through.

Also: `claude-agent-acp` takes `http` MCP servers with headers, so local
agent blocks can use `/mcp` over loopback with a per-block bearer token
instead of a stdio bridge. And rmcp's defaults need three settings for
illogical: allowed hosts, cache hints on resource results, and Origin.

## Setup

- `rmcp` **3.5.0** (2026-09-27) with axum 0.8.9 and tokio 1.53. Features:
  `server macros schemars transport-io transport-streamable-http-server
  transport-streamable-http-client-unix-socket
  transport-streamable-http-client-reqwest client`.
- Claude Code **2.1.287** (`~/.local/bin/claude`), `claude -p --model haiku
  --mcp-config <file> --strict-mcp-config --setting-sources local
  --output-format stream-json`, in `work/cc` (its own git repo), with the
  parent session's `CLAUDE*`/`AI_AGENT` env removed. The interactive test
  used S18's `tui.py` (120x40 PTY, pyte).
- Codex **codex-cli 0.155.1**, `codex exec --json`, model `gpt-6-astra`.
  The MCP server was added with `-c mcp_servers.s14.*` overrides, and
  `~/.codex/config.toml` wasn't edited.
- `@agentclientprotocol/claude-agent-acp` **0.85.1** (ACP SDK 1.6.0,
  claude-agent-sdk 0.3.286), driven by S13's `acp.ts`.
- Local wisp (wispd on `127.0.0.1:7788`). Throwaway sprite
  `illogical-s14-net` (Ubuntu 24.04, `10.209.0.91/16`, no network policy),
  deleted afterwards (DELETE 204, then GET 404). Existing sprites and the
  running illogicald weren't touched.

## Files

| file | what |
|---|---|
| `src/main.rs` | the throwaway rmcp server: tools `sleep` (optional progress), `big` (N chars of numbered lines), `pane_summary` (`Json<T>`, so an outputSchema), `summary_and_structured` (text plus a separate `structuredContent`), `close_pane` (`isError`, destructive), `bump` (fires resource updates); a resource, a template, legacy `resources/subscribe` and 2026-07-28 `subscriptions/listen`. Modes: `stdio`, `http ADDR`, `unix SOCKET`, plus the clients `bridge SOCKET` (rmcp over a Unix socket) and `listen URL` (2026-07-28 subscription). Logs every request to `$S14_LOG` |
| `serve.sh`, `stop.sh`, `show.py` | start or stop a server in the background; print its log |
| `rpc.sh` | a raw curl session: initialize, tools/list, calls with a progress token, subscribe, and a GET stream |
| `cc.sh`, `timeout.sh`, `outsize.sh`, `firstresult.py` | one `claude -p` run against one config; timeout trials; output-size trials; the first tool result a run saw |
| `tui-run.sh`, `tui.py` | interactive Claude Code with a long call (S18's driver) |
| `codex.sh` | one `codex exec` run, stdio or HTTP |
| `acp.ts`, `claude-acp.sh`, `acp-http.ts` | S13's ACP client; claude-agent-acp with http, stdio and `type:"stdio"` servers |
| `vmnet.ts`, `vm.sh`, `guest-probe.sh`, `guest-client.sh` | create, delete and exec in a sprite; probe host addresses from the guest; the host-side relay and an MCP client inside the guest |

```sh
cargo build                                  # in spikes/s14-mcp
./serve.sh http 127.0.0.1:7914 cc && ./rpc.sh
./timeout.sh t1 http 7921 400 0              # NAME transport PORT SECS PROGRESS_EVERY [ENV=..]
./serve.sh http 127.0.0.1:7930 out && ./outsize.sh o1 200000 false
./codex.sh h1 http 7970 "Use the s14 MCP server: ..."
./cc.sh basic mcp-http.json "Call the s14 tool summary_and_structured once, then reply with the exact text you received"
npm install --prefix work/node85 @agentclientprotocol/claude-agent-acp@0.85.1
./serve.sh http 127.0.0.1:7950 acp-http && mise exec bun@1.4.2 -- bun acp-http.ts
python3 -m venv work/venv && work/venv/bin/pip install pyte && ./tui-run.sh bg 7980 200 20
./vm.sh create illogical-s14-net && ./vm.sh runfile illogical-s14-net guest-probe.sh
./vm.sh relay illogical-s14-net /tmp/illogical-mcp.sock -- ./target/debug/s14-mcp stdio &
./vm.sh runfile illogical-s14-net guest-client.sh; ./vm.sh delete illogical-s14-net
```

## 1. rmcp maturity

**Everything M16 needs works, built and exercised against real clients, not
just read about.** One `StreamableHttpService` nested at `/mcp` in axum
served all of these:

| feature | result |
|---|---|
| Streamable HTTP, 2026-07-28 (stateless: `server/discover`, per-request `_meta`, no `initialize`) | works. Claude Code 2.1.287 uses it, and so does rmcp's own client with `ClientLifecycleMode::Discover` |
| Streamable HTTP, legacy sessions (`initialize`, `Mcp-Session-Id`, GET stream, DELETE) | works. Codex uses 2025-06-18, and curl was tested with 2025-11-25 |
| stdio | works with Claude Code (2026-07-28) and Codex (2025-06-18) |
| Streamable HTTP client over a **Unix socket** (`StreamableHttpClientTransport::from_unix_socket`) | works against the same service served by `axum::serve(UnixListener)`. This is `illogical mcp`'s bridge to the daemon socket, ready-made |
| progress notifications | a tool takes `meta: RequestMetaObject, peer: Peer<RoleServer>` and calls `peer.notify_progress(...)`. They arrive on the POST's SSE stream before the result, over HTTP, stdio and the VM relay |
| structured content | `Json<T>` returns generate an `outputSchema`, and the text block is the same JSON. You can also set `CallToolResult.structured_content` next to your own text |
| tool annotations | `#[tool(annotations(read_only_hint = true, destructive_hint = true, idempotent_hint = …, open_world_hint = …))]` appear in `tools/list` as `readOnlyHint` and so on |
| `isError` results | `CallToolResult::error(...)`. Claude Code gave it to the model as an `is_error` tool_result with the sentence intact |
| resource updates | legacy `resources/subscribe`: kept the peer and sent `notifications/resources/updated` on the session's GET stream. 2026-07-28: `accepted_subscription_filter` + `listen(SubscriptionContext)`, and `context.sink().notify_resource_updated(uri)` reached the rmcp client's `listen()` stream |

Three defaults illogical has to change:

- **`allowed_hosts` defaults to loopback only** (`localhost`, `127.0.0.1`,
  `::1`). The `Host` check is a DNS-rebinding guard, so `/mcp` on the
  tailnet needs the MagicDNS name and tailnet IP added. A Unix-socket
  client sends `Host: localhost`.
- **`allowed_origins` is empty by default, which means no Origin check.**
  rmcp does implement the exact-`(scheme, host, port)` match. M16 should set
  it to the app's own origins (and `enforce_origin_validation()` if it wants
  every present Origin checked).
- **Resource results need `ttlMs` and `cacheScope` for Claude Code.**
  rmcp leaves both unset. Claude Code 2.1.287 then rejects `resources/read`
  with `Invalid result for resources/read: … "ttlMs" … expected number,
  received undefined … "cacheScope" … values ["public","private"]`. It also
  treats `resources/list` as failed: it retried it four times in 2s and
  told the model "No resources found". Setting
  `.with_ttl_ms(0).with_cache_scope(CacheScope::Private)` on list, template
  and read results fixed it (`S14_CACHE_HINTS=0` reproduces the failure).
  `tools/list` was accepted without them.

Other notes: rmcp 3.x moves fast (3.1 to 3.5 in six weeks, with the
2026-07-28 rewrite). `resources/subscribe` is `#[deprecated]` in favour of
`subscriptions/listen`. A stateless 2026-07-28 tool call sends nothing on
its SSE stream until the result (no keep-alive comments were seen in 35s),
which is why the 60s limit in section 2 bites. Pin the version.

## 2. Claude Code 2.1.287 as a client

### What it speaks

Over both HTTP and stdio it opens with `server/discover` at **2026-07-28**,
then `subscriptions/listen` with `{"resourcesListChanged": true}`, then
`tools/list` and `resources/list`. Every request carries
`_meta.io.modelcontextprotocol/{protocolVersion,clientCapabilities,clientInfo}`
(capabilities: `elicitation {form, url}`, `roots {listChanged}`), and HTTP
adds `Mcp-Method`/`Mcp-Name` headers. **Every `tools/call` carries a
`progressToken`** and `_meta["claudecode/toolUseId"]`. The User-Agent is
`claude-code/2.1.287 (sdk-cli)`.

### Timeouts

There are three clocks. Trials used `sleep` with and without progress (`T`
is the wall time of the whole `claude -p` run, about 6s of which is
startup):

| trial | transport | env | call | result |
|---|---|---|---|---|
| t1 | HTTP | default | 400s, silent | `The operation timed out.` (isError) about **60s** in (T 67s) |
| t3 | HTTP | `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT=40000` | 120s, silent | the same, about 60s |
| t6 | HTTP | `CLAUDE_AUTO_BACKGROUND_TASKS=1 CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS=15000` | 60s, silent | the same, about 60s (nothing backgrounded under `-p`) |
| t4 | HTTP | `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT=40000` | 120s, progress every 10s | **ok**, `slept 120.0s` |
| t8 | HTTP | default | 150s, progress every **50s** | **ok** (progress resets the 60s timer) |
| t7 | HTTP | `MCP_TOOL_TIMEOUT=600000` | 100s, silent | **ok** (the 60s limit is `max(MCP_TOOL_TIMEOUT, 60s)`) |
| t2 | HTTP | `MCP_TOOL_TIMEOUT=20000` | 45s, progress every 5s | `MCP server "s14" tool "sleep" timed out after 20s`: **progress does not extend the hard limit** |
| t5 | stdio | default | 200s, silent | **ok** |
| t9 | stdio | `CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT=40000` | 120s, silent | **ok** (the idle watchdog never fired) |
| t10 | stdio | the same | 120s, progress every 10s | ok |
| tui | HTTP, **interactive** | `MCP_TOOL_TIMEOUT=600000` | 200s, progress every 20s | the call shows `slept 20s (10%)` (progress message and percentage). **At ~120s it became background task `ki6nl16f6`**, the turn ended ("1 MCP task still running"), and at 200s "MCP task … completed" woke the model, which reported `slept 200.0s` |

So:

- **Hard limit:** per-server `timeout` or `MCP_TOOL_TIMEOUT`. The default is
  `1e8` ms (about 27.8h, read from the binary). Progress doesn't extend it.
  The binary's own description says so too ("Hard wall-clock limit per
  call; progress notifications do not extend it").
- **HTTP silence limit: 60s**, or `MCP_TOOL_TIMEOUT` if that's larger. Any
  progress notification resets it. Its error text is a bare `The operation
  timed out.`, so an agent can't tell which tool or why. stdio has no such
  limit at 200s.
- **Idle watchdog:** the binary has one (`CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT`,
  defaults 300s HTTP and 1800s stdio, reset by progress). It never fired in
  these trials, even set to 40s over stdio. *Unverified* why (it may be
  gated off for `-p`). Don't rely on it either way.
- **Auto-background (interactive only):** `CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS`,
  default 120000 behind a feature flag, off in `-p` unless
  `CLAUDE_AUTO_BACKGROUND_TASKS` is set (it didn't trigger with it set, t6).
  Observed in the TUI at 120s. It's graceful, but it ends the agent's turn,
  which is usually not what "wait for my build" means.

### Output size

`big` returns N characters of 100-char numbered lines (mostly `x`, which
tokenizes very cheaply, so the token tier below is generous for this
content):

| result | Claude Code's model saw |
|---|---|
| 40,000 chars, 49,000 chars | the whole text inline |
| 51,000, 60,000 (structured), 90,000, 120,000 chars | `<persisted-output>Output too large (50.3KB). Full output saved to: ~/.claude/projects/…/tool-results/toolu_….json` plus `Preview (first 2KB)` |
| 200,000 chars (default) and 30,000 chars with `MAX_MCP_OUTPUT_TOKENS=5000` | not an error: `Error: result (200,000 characters across 2,000 lines) exceeds maximum allowed tokens. Output has been saved to …/mcp-s14-big-….txt. Format: Plain text`, then advice to grep it or Read it "in chunks of ~747 lines", ideally in a subagent |

- **About 50,000 characters is the real ceiling.** Past it, the model gets a
  2KB preview and a file path. Reading that path needs a Read permission
  under `~/.claude/projects`, which was denied in `-p` and in one run led
  haiku to invent the "last 300 characters". `MAX_MCP_OUTPUT_TOKENS`
  (default 25,000; the binary also checks a remote flag) is a second,
  higher tier.
- Nothing is cut silently. Both tiers say what happened. But a tool that
  pages itself (`read_output` with `next_offset`) beats both. **M16 should
  cap a page at about 16KB by default and 40,000 chars at most.**

### structuredContent

`summary_and_structured` returns text `pane %7 exited 2; last line: build
failed (SUMMARY-TEXT)` and structuredContent `{"exit_code":2, "pane":"%7",
"secret_marker":"STRUCTURED-ONLY-42"}`. **Claude Code's tool_result to the
model was only the structuredContent JSON.** The summary text never
reached it (`tool_use_result` in the stream JSON keeps both). The 60k
structured `big` call was the same: the persisted file held the
structuredContent object, not the text. Codex gave the model both (section 3).

So in M16, every result's structured object carries its own `summary`
string, and the text block is that same JSON (as the spec recommends and
rmcp's `Json<T>` already does).

### Resources and subscriptions

- Claude Code exposes resources through `ListMcpResourcesTool`,
  `ReadMcpResourceTool` and `ReadMcpResourceDirTool`. With cache hints set,
  list and read worked.
- It **never asked to be told about a resource changing.** Its only
  `subscriptions/listen` asks for `resourcesListChanged`. After `bump`, the
  server's attempt to notify about `s14://counter` on that stream was
  refused by rmcp (`listen.sent ok:false`), because the client hadn't asked
  for that URI. It never sent the legacy `resources/subscribe` either.
- *Unverified:* whether Claude Code uses `readOnlyHint` and the other
  annotations for its permission prompts. All tools here were allow-listed.

## 3. Codex 0.155.1 as a client

- **stdio and HTTP both work.** Codex sends `initialize` at **2025-06-18**
  with capabilities `elicitation {form, url}` (plus
  `experimental.codex/auth-change` on stdio). Over HTTP it uses a legacy
  session (`Mcp-Session-Id`, GET stream, DELETE at the end), User-Agent
  `codex-mcp-client/0.155.1`.
- Every `tools/call` carries a `progressToken` plus `callId`, `itemId`,
  `threadId` and `x-codex-turn-metadata` in `_meta`. Five progress
  notifications were accepted during `sleep 5`.
- **The model sees both `content` and `structuredContent`.** Its reply
  quoted the full result object with both.
- **Timeouts:** a silent 90s HTTP call, and one with progress every 10s,
  both completed with default settings. Codex also let the model say "still
  running" while the call was in flight. A silent 400s call failed at
  **300s**: `tool call failed for 's14/sleep' … timed out awaiting
  tools/call after 300s`, which is the default `tool_timeout_sec`.
  *Unverified:* whether progress extends it. M16's 100s cap stays well
  inside it.
- **Output:** 200,000 chars came back as a completed call. The model
  reported the first line and line `001999` as the last. *Unverified:*
  whether Codex elided the middle.
- It never called `resources/list` or `resources/templates/list`.

## 4. From inside a wisp VM

**Nothing on the host is reachable from a guest, and that's wisp's design.**
`scripts/setup-host.sh` in mini-sprites has
`iifname msbr0 drop comment "sprites may not talk to host services"` in
`input`, and drops `private4` (10/8, 172.16/12, 192.168/16, 100.64/10,
169.254/16, 127/8) in `forward`. `wisp-netd` only edits the `restricted4`
set, which adds a proxy for policy-restricted sprites. It opens nothing.
`docs/api.md` says the same: "a guest cannot reach the host at all".

Measured from `illogical-s14-net` (no policy) with TCP connects and a
real MCP POST, each with a 4–5s timeout:

| target | result |
|---|---|
| `10.209.0.1:7940` (the spike's `/mcp`, bound to the bridge IP; host-side curl got 200) | timed out; `curl` POST `000` |
| `10.209.0.1:8080` (a host service on 0.0.0.0), `10.209.0.1:7880` (wispd on the bridge) | timed out |
| `192.168.1.10:8080` (host LAN IP) | timed out |
| `100.64.0.10:7788` (host tailnet IP) | timed out |
| `1.1.1.1:443`, `https://example.com` | connected, 200 |

**A host-side bridge works.** `vm.sh relay` opens a **non-TTY wisp exec**
(host to guest, which is allowed) running a 20-line python3 relay. The
relay listens on a guest Unix socket (`/tmp/illogical-mcp.sock`) and joins
the one accepted connection to the exec's stdin/stdout. On the host, the
exec stream is piped to `s14-mcp stdio`. A client in the guest
(`guest-client.sh`, standing in for an agent's `mcpServers` stdio command
such as `nc -U /tmp/illogical-mcp.sock`) got `initialize` in 4ms, the
6-tool list, a `tools/call` round trip in 1.0ms, and three progress
notifications during a 3s `sleep`. The relay's exec stayed open until the
sprite was deleted, then closed with 1006.

A VM agent block already holds an exec, so the relay doesn't add the "an
attached exec keeps the sprite awake" cost from M3b. It does need to accept
again if the agent restarts its MCP client (the spike accepts once).

## 5. claude-agent-acp and `http` MCP servers

**Yes.** 0.85.1 advertises `agentCapabilities.mcpCapabilities: {"http": true,
"sse": true}`. One `session/new` with three servers:

- `{type:"http", name:"s14http", url, headers:[{name:"Authorization",
  value:"Bearer s14-block-token"}]}`: connected and listed six tools. The
  model called `mcp__s14http__summary_and_structured`, and **every HTTP
  request carried the `Authorization` header**. It speaks 2026-07-28, the
  same as Claude Code.
- `{name, command, args, env}` (no `type`): connected.
- `{type:"stdio", name, command, …}`: also connected. The adapter source
  (`acp-agent.js`) only treats a server with no `type` field as stdio, so
  something in between normalises it. Either form works today.

Side finding: the session's tool list also had the user's **claude.ai
connectors** (`mcp__claude_ai_*`) even with `settingSources: []`. An agent
block isn't limited to the MCP servers illogical passes.

## Recommendation for M16

1. **Build on rmcp 3.5** (pinned). Use one `StreamableHttpService` at `/mcp`
   for both protocols, `stdio()` for the bridge, and
   `from_unix_socket` for `illogical mcp` to the daemon. Set
   `allowed_hosts` (the tailnet name and IP), `allowed_origins` (the app's
   origins), and `ttlMs` + `cacheScope` on every resource result.
2. **Long calls:** `run --wait` and `wait` send progress every 15s with a
   message (Claude Code shows it as `msg (NN%)`). They return "still
   running" with the offset **by 100s by default**, before Claude Code's
   120s auto-background and well inside every limit measured. A caller can
   ask for a longer `timeout`. Over HTTP, progress isn't optional: 60s of
   silence kills the call.
3. **Results:** a page caps at ~16KB (hard max 40,000 chars) with
   `next_offset`. `structuredContent` always includes a `summary` string,
   and the text block is the same JSON, because Claude Code shows the model
   only the structured part.
4. **Resources:** keep read-only templates (`pane/%N/output`, `screen`,
   `history`). They're cheap and Claude Code can read them. **Drop
   "subscribable" from v1**, because no client tested subscribes.
5. **Agent blocks:** local blocks get an **`http` MCP server** pointing at
   the daemon's loopback `/mcp` with `Authorization: Bearer <per-block
   token>`, with no bridge process. **VM blocks** get a stdio entry
   (`nc -U <guest socket>`, or `illogical mcp --socket` if the binary is
   in the image), served by a **host-opened relay exec** that the daemon
   pipes into its MCP server, scoped by that block's token. Don't plan on
   the guest reaching any host address, and don't ask wisp to open one.
6. **Docs:** in the README's Claude Code setup, mention `MCP_TOOL_TIMEOUT`
   only as a fallback. The tools shouldn't need it.
