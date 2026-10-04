# S17: editors in the swarm (Claude Code's IDE protocol, remote extensions, editor events, servers)

Run 2026-10-02 on geek, for #41 (before M27, #42, and M28, #43). **Result: go on
illogicald as a Claude Code IDE (as a complement to M29's hook, not a
replacement); code-server goes in M27; the M28 schema is below.**

- **illogicald as a Claude Code IDE: go.** A 260-line fake IDE (lockfile +
  MCP over a WebSocket) was enough for the real `claude` 2.1.287 to send it
  every Edit and Write as `openDiff`, and to apply what it answered: accept,
  reject, accept-with-changes. The terminal dialog shows at the same time, and
  when the terminal answers first Claude Code **tells the IDE** (`close_tab`),
  which the `PermissionRequest` hook never does (S18). Two limits: only Edit
  and Write go to the IDE (Bash, AskUserQuestion and everything else stay
  with M29's hook and M6c), and Claude Code **does not reconnect** after the
  IDE goes away: a daemon restart costs every running Claude its IDE until
  someone types `/ide`.
- **A real IDE beside it: no clash, if illogicald registers with no
  workspace folders** and sets `CLAUDE_CODE_SSE_PORT` in its panes. Claude
  Code in an illogical pane then always picks illogicald; Claude Code
  anywhere else never sees it and picks the real IDE as before. Registering
  with folders the way an extension does makes two valid IDEs, and `--ide`
  then connects to **neither**.
- **Extensions on remote machines: go for Remote-SSH-style servers, mount
  needed for containers.** A workspace extension ran in the server's
  extension host and reached a unix socket on the server's machine under
  Microsoft's own VS Code Server 1.140 (the build Remote-SSH installs, via
  `code serve-web`), code-server 1.140 and openvscode-server 1.109. In a
  Docker container it ran inside the container and could reach the host's
  socket only when the socket's directory was bind-mounted. nvim reaches the
  socket with core `vim.uv`. Zed's extensions can't (from its docs: WASM, no
  editor events), so Zed is out of M28.
- **What an editor reports, and how often:** typing at 7.4 characters a
  second fires 16 events a second in nvim and 23 in VS Code. Everything the
  swarm's summary needs (file, diagnostic counts, unsaved buffers, debugger)
  changes about 0.1 times a second, so M23's once-a-second tick costs about
  2 B/s per editor. A follower's stream (cursor, selection, view, the edited
  line) needs a **throttle, not a debounce**: a 100 ms tick sends 7 messages
  and 360–570 B/s with at most 100 ms of lag; a 250 ms trailing debounce
  starved for up to 52 s while someone typed.
- **M27's server: code-server.** It's current (VS Code 1.140, released
  yesterday; openvscode-server's latest is 1.109.5 from February), MIT, Open
  VSX, brotli (6.1 MB per page load against openvscode-server's 17 MB
  uncompressed), and has the flags M27 needs (`--auth none`,
  `--disable-workspace-trust`, `--socket`, `--idle-timeout-seconds`). It costs
  more memory: 135 MB idle and 417 MB with a client once its chat features are
  off (openvscode-server: 84 and 259). Both ran inside an M6a browser block
  with no auth of their own, a file shown 1.7–2.0 s after the block opened.
- **Follow mode: CodeMirror 6, not Monaco.** Read-only CM6 with three
  languages, lint underlines and the terminal's colours is 182 KB gzipped
  and drew in 66 ms; Monaco cut down to the same is 780 KB (4x today's whole
  app) and 189 ms, and Monaco as usually bundled is 3.3 MB.

## Setup

- geek: 32 cores, 124 GB. Claude Code **2.1.287** (`~/.local/bin/claude`,
  `--model haiku`), driven in a 120x40 PTY by `tui.py` (S18's driver) with
  pyte 0.8 and `websockets` 17.1 in `work/venv`.
- **openvscode-server v1.109.5** (2026-02-20) and **code-server 4.140.0**
  (VS Code 1.140.0, 2026-10-01) from their GitHub releases, unpacked outside
  the repo. Microsoft's VS Code Server 1.140.0 through `code serve-web` from
  the installed VS Code 1.139 CLI. Anthropic's Claude Code extension 2.1.287
  from Open VSX.
- Headless Google Chrome through the repo's Playwright 1.63; Node 22.23.2;
  nvim 0.12.5 with pylsp 1.15 (pyflakes, pycodestyle) as a real language
  server; Docker 29.8 with `debian:bookworm-slim`; wisp on geek.
- A throwaway illogicald (`target/debug`, its own state dir, loopback ports
  7861/7862 in M6a's dev scheme) for the browser-block runs. The user's
  illogicald, editors and VMs were not touched. Every lockfile written to
  `~/.claude/ide/` was removed; the directory didn't exist before and was
  removed too.

## Files

| file | what it is |
|---|---|
| `fake_ide.py` | Q1: a fake IDE. Writes `~/.claude/ide/<port>.lock`, speaks MCP over a WebSocket, logs every frame to `work/ide-<name>.jsonl`; `openDiff` is answered from `work/ide-<name>.mode` or a line in `work/ide-<name>.cmd` (`accept`, `reject`, `edit`, `close`); `sel …` and `at …` lines send `selection_changed` and `at_mentioned` |
| `tui.py`, `run.sh`, `key.sh`, `waitfor.sh` | S18's driver: start Claude Code in a PTY (`S17_ENV` adds variables such as `CLAUDE_CODE_SSE_PORT`; `S17_SETTINGS` picks `work/settings-<name>.json`), type into it, wait for a screen |
| `turn.sh`, `probe.sh` | one edit turn answered from the IDE or the terminal, printing the IDE frames; start Claude Code and report which fake IDE it connected to |
| `realide.mjs` | a real IDE beside the fake: code-server with Anthropic's extension, opened in Chrome, with a button-clicker |
| `probe-ext/`, `pack_vsix.py`, `listen.py` | Q2/Q3: a workspace extension that sends what a presence carries (raw, every event) to a unix socket; its VSIX; the socket's listener |
| `remote_probe.mjs` | Q2: the probe under each server, on the host or in a container with or without the socket mounted |
| `vscode_events.mjs`, `nvim_events.py`, `replay.py` | Q3: type for a minute and record every event (VS Code through the probe, nvim through autocmds); replay through reporter policies |
| `nvim_sock.lua` | Q2: nvim to a unix socket with core `vim.uv` |
| `servers.mjs`, `block.mjs`, `wisp.mjs`, `wisp-inner.sh`, `sprite.sh` | Q4: start and memory on geek; inside a browser block behind a throwaway illogicald; inside a wisp sandbox |
| `bundle/`, `bundle.sh` | Q5: Monaco (full and cut down), CodeMirror 6 and Shiki builds, sized and timed |
| `results/` | the JSON the tables below come from (paths scrubbed) |

To rerun: `python3 -m venv work/venv && work/venv/bin/pip install pyte
websockets pynvim "python-lsp-server[pyflakes,pycodestyle]"`. Q1: `echo hold >
work/ide-a.mode; work/venv/bin/python fake_ide.py a work/proj &`, then
`S17_ENV='{"CLAUDE_CODE_SSE_PORT":"<port>"}' ./run.sh r1 edit` and `echo
accept >> work/ide-a.cmd`. Q3: `work/venv/bin/python nvim_events.py 8 90`,
`node spikes/s17-editors/vscode_events.mjs code-server <dir> 8 60`. Q4: `node
spikes/s17-editors/servers.mjs <ovs-dir> <cs-dir> 3`, `block.mjs`, `wisp.mjs
<cs.tgz>`. Q5: `./bundle.sh`. Unix socket paths are limited to 108 bytes, so
the probe's socket lives in `/tmp/s17-sock/`.

## Q1: Claude Code's IDE protocol

### Finding an IDE (2.1.287, read from the binary, then run)

- **Lockfiles:** `~/.claude/ide/<port>.lock` (also `$CLAUDE_CONFIG_DIR/ide`,
  and Windows profiles under WSL), newest first. JSON: `pid`,
  `workspaceFolders`, `ideName`, `transport` (`"ws"`; anything else means the
  older SSE transport), `runningInWindows`, `authToken`. The real extension
  in code-server wrote `{"pid", "workspaceFolders": [<folder>], "ideName":
  "code-server", "transport": "ws", "runningInWindows": false, "authToken":
  <36 chars>}`, mode 0600, 1.25 s after the workbench opened.
- **Stale lockfiles:** at startup Claude Code deletes lockfiles whose `pid`
  is dead (seen: a `kill -9`'d fake's lockfile was gone after the next start).
  The real extension left its lockfile behind when code-server was stopped,
  so stale files are normal.
- **When it connects on its own** (`MNe`): `CLAUDE_CODE_AUTO_CONNECT_IDE=false`
  stops it; otherwise it auto-connects when `autoConnectIde` is set in
  `~/.claude.json`, with `--ide`, inside a VS Code or JetBrains terminal, when
  `CLAUDE_CODE_SSE_PORT` is set, or with `CLAUDE_CODE_AUTO_CONNECT_IDE=true`.
  It polls for up to 30 s at startup.
- **Which one:** a lockfile is valid when its port equals
  `CLAUDE_CODE_SSE_PORT`, or one of its folders contains the cwd. It picks
  only when exactly one is valid; `CLAUDE_CODE_SSE_PORT` narrows the list to
  that port first. Inside a VS Code terminal it also requires the IDE's pid to
  be an ancestor; outside one it doesn't check.

### Connecting

- `ws://127.0.0.1:<port>`, subprotocol `mcp`, headers `User-Agent:
  claude-code/2.1.287 (cli)` and `X-Claude-Code-Ide-Authorization: <token>`.
  **No `Origin`**, so an IDE server can refuse any upgrade that has one (a
  browser), which closes the cross-site WebSocket hole the VS Code extension
  had in 2025 (from memory, not rechecked).
- MCP `initialize` with `protocolVersion: "2025-11-25"` and capabilities
  `roots` and `elicitation`, then `notifications/initialized`, an
  `ide_connected` notification with Claude Code's pid, and `tools/list`.

### What it calls

| when | tool | what the fake answered | what Claude Code did |
|---|---|---|---|
| each turn's first tool call | `closeAllDiffTabs` | `TAB_CLOSED` | |
| an Edit or Write in default mode | `openDiff {old_file_path, new_file_path, new_file_contents, tab_name}`; waits | `FILE_SAVED` + contents | applied them; with **changed contents**, applied the changes ("Added 2 lines", the IDE's extra line in the file) |
| | | `DIFF_REJECTED` | "User rejected update to hello.py"; the turn ended as for "No" |
| | | `TAB_CLOSED` | applied its own proposal |
| | (pending; terminal answered **Yes**) | | applied it, then `close_tab {tab_name}` to the IDE |
| | (pending; terminal answered **No**) | | rejected it, then `close_tab` |
| after each edit | `getDiagnostics {uri}`, then `getDiagnostics {}` | `[]` | (new diagnostics would be shown to the model) |

- **Only Edit and Write.** A Bash permission prompt (`touch bash-made.txt`)
  sent nothing to the IDE; the code that calls `openDiff` takes only those
  two tools, and only when the `diffTool` setting is `auto` (the default).
  AskUserQuestion doesn't go to an IDE (from the code; not run).
- **acceptEdits:** no `openDiff`, the edit ran (`r3`). bypassPermissions was
  not run; nothing asks there (S18).
- **Beside M29's hook:** with a `PermissionRequest` hook configured and the
  IDE connected, the same Edit fired both: the hook got `tool_name: "Edit"`
  and the IDE got `openDiff`. Write and Bash fired the hook; only Write
  reached the IDE.
- **The dialog shows at the same time** ("Opened changes in illogical ⧉" above
  "Do you want to make this edit"), as with the hook, and the first answer
  wins. Unlike the hook, the IDE is told when it lost (`close_tab` with the
  same `tab_name`).
- **The other direction:** a `selection_changed` notification put "⧉ 2 lines
  selected" in the prompt (sent with the next message), and `at_mentioned`
  typed `@hello.py#L1-2` into the prompt box. illogicald could send either
  from the web: "ask Claude about these lines" from a follow view.

### When the IDE goes away

The fake was stopped and restarted on the same port and token (`rc1`).
Claude Code noticed the close, **did not reconnect** in 18 s or on the next
edit (which used the terminal dialog only), and reconnected only when
someone ran `/ide` and picked it. That `/ide` then offers to turn on
auto-connect for good (declined here; it would have written
`~/.claude.json`). A daemon restart (M2, upgrades) therefore has to keep the
IDE connections themselves, not just the listener.

### Two IDEs at once

| registered | Claude Code started with | connected to |
|---|---|---|
| fake "illogical" + fake "Visual Studio Code", both with work/proj | nothing | none (no auto-connect) |
| | `--ide` or `CLAUDE_CODE_AUTO_CONNECT_IDE=true` | **none** (two valid) |
| | `CLAUDE_CODE_SSE_PORT=<illogical>` | illogical |
| fake "illogical" + **real code-server** with Claude Code's extension | `--ide` | none |
| | `CLAUDE_CODE_SSE_PORT=<illogical>` | illogical |
| | `CLAUDE_CODE_SSE_PORT=<code-server>` | code-server: its diff tab opened, its "Accept Proposed Changes" applied the edit |
| **"illogical" with `workspaceFolders: []`** + fake "Visual Studio Code" | `--ide` | Visual Studio Code |
| | `CLAUDE_CODE_SSE_PORT=<illogical>` | illogical |
| one left after the other's `kill -9` | `--ide` | the live one (stale lockfile deleted) |

### Go/no-go: go, as a complement

- **What it adds over M29's hook:**
  - edit before accepting (`FILE_SAVED` with other contents);
  - Claude Code says when the terminal answered (`close_tab`), so the card
    closes without guessing;
  - no hook settings to install for diffs; an environment variable is enough;
  - selection and @-mentions from the web.
- **What it doesn't cover:** Bash, MCP tools, WebFetch and AskUserQuestion.
  Those stay with M29's hook and M6c, so the IDE never replaces them.
- **How (for M28):**
  - one loopback listener per daemon, and its lockfile with **no workspace
    folders**, mode 0600;
  - `CLAUDE_CODE_SSE_PORT` in every pane's environment;
  - check the token, and refuse any upgrade with an `Origin`;
  - `openDiff` becomes a diff card. Keep the request open until someone
    answers, or Claude Code calls `close_tab` / `closeAllDiffTabs`;
  - when the user would rather have diffs in their real IDE, pass the call
    on. The daemon can read that IDE's lockfile (same user) and call its
    `openDiff` with its token. "Which one gets diffs" is then a daemon
    setting, changeable at any time, instead of an environment variable
    fixed when the pane started;
  - restarts must keep the WebSocket connections (S3's fd store, or a small
    process that outlives the daemon), or every Claude loses its IDE until
    `/ide`.
- **Not run:** JetBrains, the SSE transport, `executeCode`, Windows/WSL, and
  what Claude Code does with diagnostics the IDE returns (the fake always
  returned none).

## Q2: extensions on remote machines

The probe (`probe-ext`, `extensionKind: ["workspace"]`) connects to
`$ILLOGICAL_PROBE_SOCK` from its extension host, says hello (hostname, pid,
`vscode.env.remoteName`, whether `/.dockerenv` exists), then streams events.

| server | where | extension ran on | reached the socket |
|---|---|---|---|
| Microsoft VS Code Server 1.140.0 (`code serve-web`) | geek | geek (`appName: "Visual Studio Code"`, `remoteName: 127.0.0.1:7851`) | yes, 14 events |
| code-server 1.140.0 | geek | geek | yes, 10 events |
| openvscode-server 1.109.5 | geek | geek | yes |
| code-server 1.140.0 | Docker (`debian:bookworm-slim`), socket dir **not** mounted | the container (hostname `8e8a17d2ea27`) | **no**: `connect ENOENT` |
| code-server 1.140.0 | Docker, socket dir bind-mounted | the container (`inContainer: true`) | yes, 10 events |
| nvim 0.12.5 | geek | geek | yes (`vim.uv.new_pipe`, `nvim_sock.lua`) |

- **Remote-SSH (VS Code, Cursor):** the browser here plays the desktop UI,
  and the server is the same build Remote-SSH installs, so the extension's
  placement and socket access are shown. **Not run:** the SSH transport
  itself (no desktop session was driven), and Cursor (not installed; from its
  docs it's VS Code's API and remote model with its own Remote-SSH).
- **Dev containers:** the workspace extension lives in the container. M28
  needs the socket's directory mounted (a dev container Feature adding a
  `mounts` entry and `ILLOGICAL_SOCK`), or a fallback. **Not run:** the Dev
  Containers extension itself; Docker plus a server stood in for it.
- **The extension must be `workspace` kind.** A `ui` extension would run on
  the laptop and reach the laptop's illogicald, not the machine the files are
  on.
- **Zed: out of M28 (from docs, Zed not installed).** Zed's extensions are
  WASM with an API for languages, language servers, themes, slash commands,
  MCP servers and debug adapters. There are no editor events (cursor,
  selection, active file), and no sockets. Under remote development the UI
  runs locally and a headless server runs language servers on the remote.
- **nvim:** a plugin talks to the local socket directly. Over ssh, nvim runs
  on the remote machine, so it's local there.

## Q3: what an editor reports, and how often

### What's available

| | VS Code API (verified with the probe) | nvim (verified with autocmds) |
|---|---|---|
| active file | `window.onDidChangeActiveTextEditor`, `tabGroups.onDidChangeTabs` | `BufEnter` |
| cursor, selection | `window.onDidChangeTextEditorSelection` | `CursorMoved`, `CursorMovedI`, `ModeChanged` + `getpos('v')` |
| visible range | `window.onDidChangeTextEditorVisibleRanges` | `WinScrolled` + `line('w0')`, `line('w$')` |
| edits | `workspace.onDidChangeTextDocument` (with the changed ranges) | `TextChanged`, `TextChangedI` (`nvim_buf_attach` for ranges; not used here) |
| diagnostics counts | `languages.onDidChangeDiagnostics` + `getDiagnostics()` | `DiagnosticChanged` + `vim.diagnostic.get()` |
| unsaved buffers | `TextDocument.isDirty`, `onDidSaveTextDocument` | `BufModifiedSet`, `BufWritePost` |
| debugger | `debug.activeDebugSession`, `onDidChangeActiveStackItem`, a debug adapter tracker's `stopped` event | none in core (nvim-dap's listeners) |

**Debugger state was not measured** in either (no debug session was run); the
rows come from the API docs and the probe's code compiling against 1.140.

### Event rates while typing

TypeScript typed into `app.ts` (VS Code: code-server 1.140, the probe, the
TypeScript server warm) and Python into `app.py` (nvim with pylsp), with
jittered keystrokes, motion and a visual selection every few lines, page
scrolling and a save every 20 s.

| | typed | events/s | by kind (per second) |
|---|---|---|---|
| nvim, 90 s | 7.35 cps | 16.0 | CursorMovedI 7.35, TextChangedI 7.35, WinScrolled 0.51, CursorMoved 0.29, ModeChanged 0.28, BufModifiedSet 0.10, TextChanged 0.07, BufWritePost 0.04, DiagnosticChanged 0.03 |
| nvim, 60 s | 13.0 cps | 27.8 | CursorMovedI 13.0, TextChangedI 13.0, WinScrolled 0.82, … DiagnosticChanged 0.05 |
| VS Code, 60 s | 7.36 cps | 23.0 | visibleRanges 7.72, textChange 7.56, selection 7.49, tabs 0.15, save 0.07, diagnostics 0.05 |

- **Diagnostics change rarely while typing** (2–3 a minute): both pylsp and
  the TypeScript server wait for a pause before they lint.
- **Taking a snapshot is cheap:** in nvim the recorder (file, cursor, mode,
  selection, visible range, diagnostic counts, unsaved buffers, the current
  line) took 30 µs at the median and 204 µs at p99, about 0.05% of a core at
  16 events a second.

### Debouncing for M23's budget

`replay.py` sends only changed fields, as M23's deltas do. The **summary**
stream is file, diagnostic counts, unsaved buffers and debugger. The
**follow** stream is file, cursor, selection, visible range, mode and the
edited line.

| stream / policy | nvim 7.4 cps | nvim 13 cps | VS Code 7.4 cps | lag (p50 / max) |
|---|---|---|---|---|
| summary, every event | 0.12 msg/s, 3 B/s | 0.11, 3 B/s | 0.17, 4 B/s | 0 |
| summary, 1 s tick (M23) | 0.02 msg/s, 1 B/s | 0.03, 1 B/s | 0.07, 2 B/s | 0.6–0.95 s / 1 s |
| summary, 250 ms trailing debounce | 0.02, 1 B/s | 0.03, 1 B/s | 0.05, 2 B/s | **52 s** / 28 s / 7 s max |
| follow, every event | 7.9 msg/s, 629 B/s | 13.7, 1134 B/s | 15.0, 475 B/s | 0 |
| follow, 100 ms tick | **7.0 msg/s, 572 B/s** | **9.0, 756 B/s** | **7.0, 361 B/s** | 51–65 ms / 100 ms |
| follow, 250 ms tick | 3.9, 317 B/s | 3.8, 326 B/s | 3.9, 206 B/s | 184–218 ms / 250 ms |
| follow, 100 ms trailing debounce | 5.0, 399 B/s | 1.7, 143 B/s | 5.3, 276 B/s | 100–411 ms / **2.3 s** |
| follow, 250 ms trailing debounce | 0.1, 10 B/s | 0.2, 16 B/s | 0.1, 6 B/s | **51–52 s** max |

- **Use throttles (send the latest state at most every N ms), not trailing
  debounces.** Keystrokes 60–200 ms apart never leave a 250 ms gap, so a
  trailing debounce sends nothing until the typist pauses.
- **Summary:** M23's 1 s tick. An editor costs about 2 B/s, so 50 typing
  editors add 0.1 KB/s against M23's 20 KB/s.
- **Follow:** a 100 ms tick while someone follows (0.4–0.8 KB/s per followed
  editor, under 100 ms of lag). Nothing when nobody does.
- **Attention bypasses the tick**, as M23's attention does: a debugger
  stopping, or errors appearing after a save, goes at once.

### The editor event schema for M28

An editor presence is one entry in M23's summary, with `kind: "editor"`,
and the same delta rules (changed fields only, `null` for gone):

```jsonc
// summary (M23's once-a-second deltas; everyone with a role on it)
{
  "id": "e3",                      // the daemon's, like a pane id
  "kind": "editor",
  "editor": "vscode",              // vscode | cursor | code-server | nvim
  "host": "geek",                  // the daemon it connected to (its socket)
  "remote": "ssh-remote",          // vscode.env.remoteName: null, ssh-remote, dev-container, …
  "workspace": "/home/me/dev/x", // the first workspace folder
  "project": { "root": "/home/me/dev/x", "name": "x" },   // M23's
  "file": "crates/control/src/auth.rs",                     // relative to project, or null
  "diag": { "e": 3, "w": 0, "i": 1 },                       // counts across the workspace
  "dirty": 1,                      // unsaved buffers
  "debug": null,                   // or { "state": "paused", "reason": "breakpoint", "file": "…", "line": 42 }
  "followers": 1                   // so the editor can say someone is following
}
```

```jsonc
// follow (only to clients following it; content, so E2E only; 100 ms tick)
{ "editor": "e3", "file": "crates/control/src/auth.rs", "line": 88, "col": 12,
  "sel": [86, 0, 90, 4],            // or null
  "view": [60, 110],                // visible lines
  "mode": "i" }                     // nvim only
// on opening a file (only files open in that editor; capped in size):
{ "editor": "e3", "open": { "file": "…", "version": 7, "text": "…" } }
// each change (VS Code contentChanges; nvim nvim_buf_attach on_lines):
{ "editor": "e3", "edit": { "file": "…", "version": 8, "changes": [{ "range": [88, 4, 88, 4], "text": "x" }] } }
// diagnostics for the followed file, when they change:
{ "editor": "e3", "diagnostics": { "file": "…", "items": [{ "range": [12, 4, 12, 9], "severity": "error", "message": "…" }] } }
```

M24 reasons from editors: **paused** (`debug.state` turns `paused`),
**errors** (`diag.e` goes from 0 to more on a save), **conflict** (a file with
conflict markers is opened), and **diff** (a pending `openDiff`, from
illogicald as an IDE).

## Q4: openvscode-server or code-server

### Licence and extensions

| | openvscode-server | code-server | VS Code Server (`code serve-web`) |
|---|---|---|---|
| licence | MIT (Gitpod), over VS Code OSS (MIT) | MIT (Coder), over VS Code OSS | Microsoft's licence; downloaded from Microsoft at first use, not redistributable (from its terms, not rechecked) |
| extensions | Open VSX (`product.json`: `open-vsx.org/vscode/gallery`) | Open VSX (built into its server, `out/server-main.js`; `EXTENSIONS_GALLERY` overrides); `--install-extension anthropic.claude-code` got Open VSX's 2.1.287 | Microsoft Marketplace |
| latest | **1.109.5, 2026-02-20** (repo pushed 2026-09-24) | **4.140.0 = VS Code 1.140.0, 2026-10-01**, about weekly | 1.140.0 |
| Claude Code's extension | (not tried) | installed from Open VSX and worked (Q1) | |

### Start and memory on geek

Three warm runs (the server's data dir reused) after one cold (fresh) each.
The page is a fresh headless Chrome context, so no browser cache. Times are
from `goto` (workbench, explorer, file) or from spawning (HTTP). PSS is
summed over the server's process tree.

| | HTTP up | workbench | explorer | file shown | page bytes | idle PSS | with a client | 15 s after it left |
|---|---|---|---|---|---|---|---|---|
| openvscode-server | 110 ms | 772 ms | 1.63 s | 2.07 s | **17.2 MB** (no compression) | **84 MB** | **259 MB** | 95 MB |
| code-server | 335 ms | 1.29 s | 1.55 s | 2.02 s | 6.3 MB (brotli) | 135 MB | 530 MB | 513 MB |
| code-server, `chat.disableAIFeatures` | 328 ms | 1.03 s | 1.27 s | 1.72 s | 6.1 MB | 135 MB | **417 MB** | 394 MB |

(medians of three; cold runs were within the same range: openvscode-server
file 1.59 s, code-server 1.98 s.)

- **Where code-server's memory goes:** its own server process (206–216 MB
  with a client, against openvscode-server's 97–106), and VS Code 1.140's
  agent host plus `copilot-runtime` (85 + 32 MB). `chat.disableAIFeatures`
  removes those two.
- **After the client leaves:** openvscode-server's extension host exits.
  code-server keeps it for its reconnection grace time, which should be short
  in M27 (not measured).
- openvscode-server's `workbench.js` is 13.1 MB with no `Content-Encoding`;
  code-server's is 5.1 MB brotli. That matters over control's relay and on
  phones.
- openvscode-server has no flag to turn workspace trust off, and didn't take
  `security.workspace.trust.enabled: false` from the settings files tried, so
  every new folder asks "Do you trust the authors". code-server has
  `--disable-workspace-trust`.
- **code-server writes `~/.config/code-server/config.yaml` on any invocation,
  even `--help`**, unless `--config` and the XDG dirs point elsewhere (it
  happened twice here and was removed). M27 must pass them. openvscode-server
  does the same with `~/.openvscode-server` (also removed).

### In a wisp sandbox

code-server 4.140 in a fresh sprite (2 GB, 8 vCPU) as a sprite service on
:8080 with `--auth none`, reached through wispd's proxy with the token
(`wisp.mjs`; the sprite was destroyed afterwards):

| | |
|---|---|
| push the 229 MB tarball | 1.2 s |
| first start, to HTTP | 5.4 s (a fresh VM; geek: 0.3 s) |
| PSS, server only / with a client | 130 MB / 429 MB (VM: 308 / 624 MB used of 1990) |
| first page: workbench / file | 1.25 s / 1.86 s |
| again | 1.29 s / 1.78 s |
| idle until the sprite went `warm` | 34 s, with code-server in it |
| page after that: workbench / file | 1.36 s / 2.32 s |

A 2 GB sprite holds code-server with room to spare; the AI features weren't
turned off there (they are 117 MB of the 429).

### Inside a browser block, with no auth of its own

`block.mjs`: a throwaway illogicald, the server on 127.0.0.1:7863 with no
token or password (warmed once), `illogical open :7863/?folder=…`, then
everything inside the block's frame:

| | workbench | file shown | typed and saved | frame origin | sockets |
|---|---|---|---|---|---|
| code-server | 1.48 s | **1.95 s** | yes | `http://b-2-<key>.localhost:7862` | the app's, and `ws://b-2-<key>.localhost:7862/stable-ccc19adc…` |
| openvscode-server | 1.20 s | **1.70 s** (after the trust dialog) | yes | same | `…/stable-07258626…` |

- **Both work through M6a's proxy unchanged:** its Host/Origin rewrite
  satisfies their own origin checks, and their WebSockets go through it.
  M27's "under 3 s once warm" held on the desktop. The phone wasn't run.
- **But the port answers anyone local:** a plain `fetch` of
  `127.0.0.1:7863` from another process got 200. "The daemon's checks are its
  only auth" needs the server off TCP. code-server has `--socket` and
  `--socket-mode`, and openvscode-server has `--socket-path`. The daemon's
  port proxy would then need a unix-socket target, which it doesn't have
  today (`ports.rs` reaches TCP ports). Otherwise: a connection token only
  the daemon knows.
- **Tile previews:** a cross-origin block can't be screenshotted from the
  page, so "the lines around its cursor" (from the illogical extension
  pre-installed in M27's server, the same stream as Q3) is the cheap one. Not
  measured; reasoned from the origin rules.

### Verdict: code-server for M27

- It's current, the AI features can be turned off, pages are compressed, and
  it has flags for no auth, no trust prompt, a unix socket and an idle
  timeout.
- openvscode-server is lighter (about 160 MB less with a client) but eight
  months behind VS Code and uncompressed.
- Microsoft's server is the most compatible (its Marketplace) but can't be
  shipped.
- M27 defaults: `--auth none --disable-workspace-trust --socket <0600 path>
  --disable-telemetry --disable-update-check`, `--config`/`--user-data-dir`/
  `--extensions-dir` under illogical's state, `chat.disableAIFeatures: true`,
  and the illogical extension (M28) pre-installed, so every M27 block is also
  an editor presence.

## Q5: follow-mode rendering

`bundle/` builds each with Vite 8.3 (production), showing 20 KB of Rust from
this repo read-only. "Draw" is page load until the text is on screen in
headless Chrome (median of 5). Heap is after a GC.

| | files | raw | gzip | brotli | draw | heap |
|---|---|---|---|---|---|---|
| Monaco 0.57, everything (languages, 4 language workers) | 95 | 13.9 MB | 3.3 MB | 2.5 MB | 241 ms | 11.9 MB |
| Monaco, cut down (editor API, Monarch for 3 languages, base worker, a theme) | 6 | 3.0 MB | **780 KB** | 620 KB | 189 ms | 9.6 MB |
| **CodeMirror 6** (read-only, line numbers, Lezer for Rust/JS-TS/Python, lint underlines, selection) | 1 | 524 KB | **182 KB** | 149 KB | **66 ms** | 3.4 MB |
| Shiki 4.5 (3 TextMate grammars, Catppuccin Mocha, JS regex engine), static HTML | 5 | 470 KB | 88 KB | 75 KB | 88 ms | 3.7 MB |
| today's app (`web/dist`, for scale) | 5 | 690 KB | 191 KB | | | |

- **Monaco** would add four times today's whole app to the page. Its
  diagnostics, selection and theming are what M27's block already shows.
- **CodeMirror 6** does what follow mode needs: a selection, diagnostic
  underlines, a highlighted line for the debugger, and applying `edit`
  messages incrementally. Its theme takes the terminal's colours directly
  (`EditorView.theme` with `theme.ts`'s background, foreground and
  selection).
- Load it as its own chunk, only when someone follows.
- **Shiki** is the lightest. But it renders static HTML: every edit would
  re-render, and selection and diagnostics would be hand-made decorations.
  It's a fit for tile previews (the lines around a cursor), not for following.
- **Theming was checked only by building with the palette;** no screenshots
  were compared.

## Not covered

- The phone (M27's 3 s target, follow mode on a phone), as for S16 and S18.
- Remote-SSH's SSH leg, Cursor, Zed, the Dev Containers extension, JetBrains
  (each marked above with what stood in for it).
- Debugger events (no debug session was run in either editor).
- How code-server behaves behind a unix socket through the daemon (the
  daemon can't proxy to one yet), and its reconnection grace time.
- Claude Code with `diffTool: terminal`, `executeCode`, and IDE-returned
  diagnostics.
