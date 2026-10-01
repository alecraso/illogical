# illogical: plan

Written 2026-10-01 from [BRIEF.md](BRIEF.md) and [docs/research.md](docs/research.md).
Scope: v1 is M0 to M2 (with M2b) plus enough of M3 to `run`/`tail`/`wait`.
Beyond v1, M3b and M3c (throwaway machines per pane and per tab), M4 (reach)
and M6 (non-terminal blocks) are planned with their shape decisions made, and
M5 is kept cheap. Decisions made after the first draft are dated inline.

## Decisions on the brief's open questions

| Question | Decision | Why |
|---|---|---|
| Rust or Go | **Rust** (tokio, axum) | Best VT-engine options, best PTY/fd control, and one binary for the daemon and CLI. Go's best option is the same libghostty through cgo. |
| VT engine | **libghostty-vt** through the `libghostty-vt` crate, behind a `VtEngine` trait | It already ships a formatter that turns terminal state back into VT sequences, and it keeps more state than xterm's serialize addon. `alacritty_terminal` would mean writing that serializer ourselves. Spike S1 has to pass before M0 depends on it; `@xterm/headless` in a sidecar is the fallback. |
| Log format | **Raw byte segments + sidecar index**; export asciicast v3 on demand | Byte offsets make `tail`, resume-from-offset and scrollback restore a seek. asciicast inflates the data and has no offsets. |
| Size reconciliation | **Per pane, last input wins.** Other viewers letterbox. | Single user moving between devices. "Smallest" would make the desktop suffer whenever the phone is open. |
| fd holding before M3 | **No; do it as M2b**, right before daily use | During development, run a separate dev daemon (own state dir and port) so restarts don't touch the daily one. Daily use is when upgrades start costing shells. |
| Local-only panes | **No.** Every pane is a daemon pane. | One model. Each machine can run its own daemon and the client can list several (M4). |
| Snapshot format | **Checkpoints: GHOSTSNP, zstd-compressed, tagged with the Ghostty commit; discarded and rebuilt from the log on mismatch. Wire: formatter VT bytes + S1 fix-ups until a ghostty-web client exists.** (Decided by S5.) | S5: exact round trip with no fix-ups (including the saved cursor), READY in 0.3 ms for 64k rows, 75x with zstd. But version 1 has already changed incompatibly without a version bump, so checkpoints can only be a cache, and xterm.js still needs VT bytes. |

## Architecture

```
web client (React + react-mosaic + xterm.js 6)      illogical CLI
        |  HTTPS/WSS via `tailscale serve`                |  Unix socket
        v                                                 v
illogicald  127.0.0.1:7681 (+ $XDG_RUNTIME_DIR/illogical/sock)
  auth: Tailscale-User-Login == config.owner (+ Host check); socket: SO_PEERCRED uid
  core: session tree   $s > @tab > split tree > %pane   (IDs never reused)
  pane: PTY master, VtEngine (libghostty-vt), log writer, OSC tap (133/7/633)
  store: ~/.local/state/illogical/
           layout.json                     atomic write, debounced on change
           blocks/%N/log/000001.seg ...    raw output bytes (other block types: their event stream)
           blocks/%N/index                 (offset, ts, resize | osc133 | osc7 | exit)
           blocks/%N/meta.json             cmd, cwd, policy, exit status
           blocks/%N/checkpoint            periodic VT snapshot + log offset
```

**Terms.** A *block* is a leaf of the tree. It has a `type`; only `terminal`
exists today, and a pane (`%N`) is a terminal block. This follows
Superlogical's model ([docs/superlogical.md](docs/superlogical.md)). Inside a
terminal, OSC 133 command ranges are *command marks*, never "blocks".

### Cargo workspace

- `crates/proto`: wire types (serde) and the byte-frame codec, shared by the daemon, CLI and tests. TypeScript types are generated from it (`ts-rs` or `specta`).
- `crates/vt`: the `VtEngine` trait (`feed`, `resize`, `snapshot() -> Vec<u8>`, `plain_text()`, `cursor`, `alt_active`). It has a `ghostty` implementation (formatter + S1 fix-ups), plus a test-only `xterm` oracle driven through Node. libghostty types are `!Send`, so the engines live on a dedicated VT thread fed by channels.
- `crates/core`: session tree, layout operations as pure functions over intents, restart policies, size arbitration. No I/O, unit-tested heavily.
- `crates/daemon`: the `illogicald` binary. Handles PTYs (rustix `openpty`, with the child doing `setsid` + `TIOCSCTTY`), the log store, the axum HTTP/WS server, the Unix socket, and systemd notify/FDSTORE.
- `crates/cli`: the `illogical` binary.
- `web/`: Vite + TypeScript + React 19. Built assets are embedded into the daemon with `rust-embed`, so there is one artifact to install.

### Protocol (one protocol, two transports)

- **Framing.** WebSocket binary frames, or length-prefixed frames on the Unix socket.
- **Message kinds.**
  - Control messages are JSON: `{id?, type, ...}`, and requests carry an `id` for correlation.
  - Output is binary: `[u8 kind][u32 pane][u64 offset][bytes]`.
- **Server to client.**
  - `hello`: the full tree plus a `rev` number.
  - `layout`: full tree and `rev` on every change. Trees are small, so the server sends whole trees, not diffs.
  - `output`: pane, offset, bytes.
  - `snapshot`: pane, offset, VT bytes. If visible-first attach is ever built (see below), it gains `part: screen|history`.
  - `ready`: pane, offset. Only if visible-first attach is built: the visible screen is complete, so the client can draw and accept input.
  - `event`: either pane events (`cmd_start|cmd_end{exit}|cwd|title|exit|bell|notify{title, body}|attention{state}`) or tree events (block closed, layout changed, client connected or gone).
    - The web client gets every event for the panes it has attached.
    - CLI and API clients get only what they `subscribe` to.
  - `size`: pane, cols, rows, owner client.
- **Client to server.**
  - `attach{panes: {id: last_offset}}`.
  - `input{pane, bytes}`.
  - `resize{pane, cols, rows}`, which also claims the size.
  - `ack{pane, offset}`.
  - Layout intents: `split{pane, dir, ratio}`, `move{pane|tab, target, edge}`, `resize_split{node, ratios}`, `close{id}`, `new_tab{session, cwd?}`, `rename`, `set_policy`.
  - Block methods (M3), which are requests with an `id` and a JSON reply:
    - `process{pane}`: the child and foreground process (pid, argv, cwd, start time);
    - `capture{pane, format: text|ansi|html, range: screen|scrollback|command}`;
    - `keys{pane, keys}`: named keys (`C-c`, `Up`, `F5`) encoded for the pane's current modes;
    - `mouse{pane, x, y, button, action}`: only delivered if the app has mouse reporting on;
    - `subscribe{events, panes?}`: opts in to event types, optionally for some panes only.
- **Attach and resume.**
  - If the client's `last_offset` is within the log and the gap is ≤ 1MB, the server replays from the log.
  - Otherwise it sends a full `snapshot`, then live output. The same path is used after a flow-control overrun.
  - After attach, the pane gets one SIGWINCH nudge.
  - **Visible-first attach is gated on measurement (decided 2026-10-01).**
    - Measure time-to-first-draw on the phone over serve, with deep scrollback.
    - Build visible-first only if that is slow. ghostty-web plus GHOSTSNP makes it native later anyway.
    - If it is built, it follows Superlogical's order:
      1. a `snapshot` with `part: screen` (the visible screen, modes and cursor), then `ready`;
      2. live output;
      3. scrollback as `snapshot` with `part: history`, newest first, within the flow-control window.
    - xterm.js can only append, so the web client does it in this order:
      1. draw the screen into the live terminal;
      2. **buffer** live output from `ready` onwards, as well as drawing it live;
      3. when history ends, build an offscreen xterm from history, then the screen, then the buffered output;
      4. swap the offscreen xterm in.

      The pane is usable from `ready`. The buffer is bounded by the flow-control window; if it overflows, fall back to a full snapshot.
- **Flow control.**
  - Each client has a per-pane unacked window of 512KB. The client ACKs about every 64KB once xterm's write callback fires.
  - A client that exceeds its window stops receiving. When it ACKs again, it gets a fresh snapshot rather than the backlog.
  - The PTY is never paused for a slow client; the log absorbs the output.
- **M5 rule.** Every server event must map onto a tmux `%` notification, and split sizes must convert to cells deterministically for a given tab size. Ratios are stored; cells are derived.

### Size arbitration

- Each pane has a size owner: the client that last sent input or focused it.
- The owner's measured cols×rows becomes the PTY size.
- Other viewers render at the pane's real size, centered with a subtle letterbox (desktop) or scaled to fit (phone).
- The phone's single-pane view claims the size only while you type there.

## Restart policies (M2)

Per pane, stored in `meta.json`, settable from the right-click menu and the CLI.

| Policy | After reboot |
|---|---|
| `none` | Scrollback restored, pane shows "exited". Click to start a shell. |
| `shell` (default) | New `$SHELL -l` in the last OSC 7 cwd. |
| `rerun` | Re-run the last command in its cwd. `confirm: true` by default, which shows a "Press Enter / click to re-run" banner, as Zellij does. |
| `hook` | Run a stored command, e.g. `claude --continue`, in the cwd. |

### Restoring scrollback

1. Create a fresh engine.
2. Feed it the last checkpoint plus the log since that point, capped at about 8MB.
3. Then write a reset (leave the alt screen, soft reset of modes, SGR 0) and a dim `── restored <time> ──` rule.
4. Then start the new process.

Old and new output share one continuous log with a `restore` index entry.

## Milestones

Each milestone ends with a demo against the acceptance list.

### S: spikes (each about half a day, before the milestone named)

- **S1 libghostty-vt: done 2026-10-01, passed.** See [spikes/s1-ghostty](spikes/s1-ghostty/README.md).
  - Seven recorded fixtures round-trip exactly, Ghostty to Ghostty (every cell, cursor, 20 modes, title, palette, scrollback) and into `@xterm/headless` 6. The fixtures are nvim on top of scrollback, nvim resized, less, top, reflow, deep scrollback, and a colours/modes/Unicode set.
  - A snapshot takes 1–3 ms.
  - This needed a fix-up layer around the formatter, which carries over into `crates/vt`:
    - set the cursor position again (the tab stops extra clobbers it);
    - emit the title and cursor shape;
    - put back dropped trailing rows and their backgrounds;
    - close the alt-screen gap with a mode-47 flip, with modes emitted separately.
  - Upstream issues to file: the cursor clobbered by tab stops, the missing title and cursor shape, the dropped trailing rows, a NUL inside the OSC 7 it emits, and a C API request to format a chosen screen and read the saved cursor.
  - Still to cover: a `claude` fixture, images, origin mode and DECSLRM, and the saved cursor.
- **S2 tailnet: done 2026-10-01, passed (from geek itself).**
  - Operator set to jake.
  - `tailscale serve --bg --https=443 http://127.0.0.1:7681` is configured and persists in tailscaled.
  - `https://geek.tailb2e8f2.ts.net` serves a valid cert.
  - A WSS echo worked through serve.
  - HTTP and WebSocket upgrade requests both carry `Tailscale-User-Login`, `-Name` and `-Profile-Pic`, plus `X-Forwarded-*`. A client-sent `Tailscale-User-Login` was replaced by the real one.
  - Still to do: open it from the phone once M0 serves a page.
- **S3 fd store: done 2026-10-01, passed.** See [spikes/s3-fdstore](spikes/s3-fdstore/README.md).
  - The shell runs in its own `systemd-run --user --scope`, with the PTY master in the FD store.
  - Restarts and a `kill -9` with `Restart=on-failure` both keep the same shell attached.
  - `stop` ends the panes. That follows from the default `FileDescriptorStorePreserve=restart`, so upgrades must use `restart`.
  - Must-dos for M2b:
    - set `O_CLOEXEC` on masters (openpty doesn't);
    - give each pane a unique scope name;
    - use the exit-status shim, because a restarted daemon isn't the shell's parent.

- **S4 reach: done 2026-10-01 (before M4), against a Fly sprite and a local
  wisp sprite.** See [spikes/s4-reach](spikes/s4-reach/README.md).
  - Both providers run x86_64, and a static musl daemon opens PTYs and runs as
    a `sprite-env` service.
  - Idle detached exec shells and an idle `tailscaled` do **not** keep a sprite
    awake; output, held-open proxy connections and attached panes do.
  - A paused sprite can only be woken through the provider (proxy, exec or a
    URL hit). Tailnet packets don't wake it.
  - Exec replay on reattach: about 6.5KB on Fly, 1 MiB on wisp. (The M3b
    spike found wisp replays the whole ring from the start of the session,
    with no end marker.) Ownership on
    reattach differs: `is_owner:true` on Fly, `false` on wisp.
  - Proxy round trip is about 50ms on both, the same as the tailnet from geek.
  - Cold wake: on Fly, processes survived about 5 min `cold`. On wisp it is a
    real cold boot (first byte 305ms): the service restarts, and the proxy
    holds the connection until the daemon listens.
  - Still pending: tailscaled on wisp; an ephemeral node surviving 60 min
    cold.

- **S5 upstream snapshot: done 2026-10-01, passed.** See [spikes/s5-snapshot](spikes/s5-snapshot/README.md).
  - GHOSTSNP, through libghostty-rs `master` (`8953a74`, which pins Ghostty `22d13172` and needs Zig 0.16), round-trips every S1 fixture with **no fix-ups**: alt and primary screens, scrollback, the saved cursor, title, modes.
  - It is about as fast as the formatter. An 11 MB snapshot (64k rows) reaches READY in 0.31 ms, and its history follows in 70 ms.
  - zstd -3 shrinks it 75x.
  - A snapshot taken mid-escape-sequence resumes exactly, as long as continuation tracking is on. Corrupted or truncated snapshots are rejected.
  - **Catch:** the format changed incompatibly after the pinned commit (BLAKE3 removed) without bumping `version = 1`.
  - **Outcome:**
    - Checkpoints are GHOSTSNP, zstd-compressed, and tagged with the Ghostty commit that wrote them.
    - A mismatched or undecodable checkpoint is discarded, and the log tail is replayed. The log is the truth; checkpoints are a cache.
    - Wire snapshots stay formatter VT bytes until a ghostty-web client exists.
    - Moving the daemon to libghostty-rs `master` (and Zig 0.16) happens at the start of M2.

### M0: the loop

- Workspace skeleton, CI (`cargo test`, `clippy`, `pnpm build`), `just` recipes, and a `dev` profile that runs a second daemon instance.
- Daemon: one PTY, VtEngine, axum WS on loopback, `rust-embed` page.
- Page: one xterm, fit, WebGL. It reconnects with backoff and sends `attach` with the last offset.
- **Done when:** run vim, close the tab, reopen it (and open it on the phone via serve), and vim is drawn correctly.

### M1: multiplexer

- `core`: the tree, the intents, and property tests. Random intent sequences must keep the tree valid: ratios sum to 1, no empty splits, IDs unique.
- Many panes, tabs and sessions. Per-pane attach and resume, per-client flow control, size arbitration.
- Web client:
  - **Layout:** react-mosaic 7 in controlled mode. Its `onChange` meta is translated to intents and never applied locally, apart from previewing a divider drag until `onRelease`.
  - **Terminals:** xterm instances live outside React in a `TerminalView` pool and are re-parented by ref, so moving a pane never remounts it. WebGL runs only on visible panes; the rest use DOM. Context loss falls back to DOM, and the code calls `loseContext()` on dispose.
  - **Mouse actions:**
    - Tab bar: click, drag to reorder, drag a tab onto a pane edge to dock it, middle-click to close, double-click to rename, a `+` button.
    - Split edges: drag to resize.
    - Right-click menus (Radix) for split right/down, move to new tab, close, rename, restart policy, and copy cwd.
  - **Keyboard:** no layout chords are required. Optional ones can come later.
  - **Desktop app:** PWA manifest with `window-controls-overlay`, so tabs sit in the titlebar.
  - **Phone view:** under 700px wide. One pane full screen, a tabs/panes sheet, and a Termux-style extra-keys bar (Esc, Tab, sticky Ctrl/Alt, arrows, `| ~ / -`). Uses `visualViewport` sizing.
- **Done when:** two machines and a phone all show the same layout live; dragging on one moves it everywhere; vim inside a moved pane stays intact.

### M2: durability

- **Log store:** segments of 4MB, plus the index and checkpoints (on idle 5s or every 2MB). Checkpoints follow S5: GHOSTSNP, zstd-compressed, tagged with the Ghostty commit, and a cache over the log. Retention defaults to 256MB per pane and is configurable.
- **Scrollback at rest (decided 2026-10-01).** Logs and checkpoints hold secrets (tokens pasted or echoed). The state dir is `0700` and the files are `0600`. Retention is enforced (above). `illogical purge %p` deletes a pane's history. Encryption is decided before M4c, below.
- **Layout persistence:** `layout.json` is written atomically (temp file + rename + fsync dir), debounced to 250ms, with a schema version.
- **Restore on start:** restart policies, scrollback replay as described above.
- **Pane environment:** spawn `$SHELL -l`. Merge environment variables live from the systemd user manager (`systemctl --user show-environment` via zbus) at spawn time, so panes started after login get `WAYLAND_DISPLAY` and `SSH_AUTH_SOCK`. Write `~/.config/environment.d/` guidance into the README.
- **Install:** systemd user unit (`Type=notify`, `WantedBy=default.target`) and `illogical install` to write and enable it.
- **Done when:** reboot geek, open the PWA, and tabs, splits, cwd and scrollback are back; the `rerun` panes show their banner or are running.

### M2b: in-place daemon upgrade (start of daily use)

- **Scopes:** each pane runs in its own transient scope (`StartTransientUnit` over zbus), so `systemctl --user restart illogicald` no longer kills shells.
- **Shim:** `illogicald _shim` wraps each child. It does `setsid`, opens the slave, execs the command, and writes the exit status to `meta.json`, because a restarted daemon is no longer the parent. Pid-reuse guards use the process start time.
- **FD store:** masters go into the FD store (`FDSTORE=1`, `FDNAME=pane-%N`, `FDPOLL=0`). On start, read `LISTEN_FDNAMES` and rebuild each VT from checkpoint + log tail.
- **Done when:** `systemctl --user restart illogicald` leaves vim and a running build untouched, and clients reconnect on their own.

### M3: structure and CLI

- **Shell integration:** auto-inject the way Ghostty does (bash `ENV`, zsh `ZDOTDIR`, fish `XDG_DATA_DIRS`), with a per-pane switch to turn it off. Parse OSC 133/7/633, and pass them through to clients.
- **Command marks in the UI:** exit-code gutter marks, click a mark to select a command's output, "re-run".
- **CLI over the socket.** Everything prints JSON with `--json`, so agents can script it.
  - `ls`
  - `run [--session s] [--tab] [--cwd] [--policy] -- cmd`, which prints the pane id
  - `send %p "text"` (`--enter`)
  - `tail %p [-f] [--from offset|--last-command]`
  - `wait %p [--command-end|--exit|--match re] [--timeout]`
  - `attach %p` (raw TTY passthrough for when you're in a terminal)
  - `export %p --cast`
  - `process %p`: the foreground process as JSON
  - `capture %p [--text|--ansi|--html] [--scrollback|--last-command]`
  - `keys %p C-c Up Enter …`: named keys, as opposed to `send`'s literal text
  - `mouse %p x y [--button] [--action]`
  - `events [-f] [--pane %p] [--type …]`: a stream of NDJSON events
- **The CLI inside every pane.** Each pane gets `ILLOGICAL_PANE=%N` and
  `ILLOGICAL_SOCK`, with `illogical` on `PATH`. A command (or an agent) running
  in a pane can drive its own pane and its siblings without being told where
  it is.
- **Agent attention (decided 2026-10-01).** This is the cheap version of
  Superlogical's guessed "agent block": know when an agent needs you, without
  a new block type.
  - **Sources:**
    - the notification sequences (OSC 9, OSC 777 `notify`, OSC 99) and BEL;
    - Claude Code hooks (`Notification`, `Stop`), which call
      `illogical attention %p needs-input|done` through the in-pane CLI;
    - a quiet-output heuristic as a fallback.
  - **State:** each pane has `idle | working | needs-input | done`, sent as
    `event` `attention{state}` and `notify{title, body}`.
  - **UI:** a badge on the tab and pane, and a "needs you" list in the phone
    sheet.
  - **Web Push** to the phone for `needs-input` and `done` when no client is
    focused on that pane. VAPID keys live on the home daemon, and the PWA
    service worker shows the notification.
  - Answering an agent's prompt from the phone is then attention plus `keys`.
- **Queryable history (decided 2026-10-01).** The per-pane index already
  records commands (OSC 133 with exit codes), cwd and timestamps. Make it
  queryable across panes, including panes that are closed but still retained:
  - `illogical history [--pane] [--failed] [--since 1h] [--cwd dir] [--match re]`
    lists commands with their pane, exit code, duration and log range;
  - `illogical search re [--since]` searches the text of all logs (with escape
    sequences stripped) and prints the matching pane, offset and command;
  - each result can feed `tail --from` or `capture`.
  - **Storage:** start with a scan over the indexes. Add a small SQLite index
    only if that is slow.
- **HTTP API:** the same requests over the WS/HTTP API for remote agents.

### M3b: ephemeral machines (a fresh VM owned by a pane)

"New VM pane" creates a throwaway wisp sprite (a Firecracker microVM on geek)
and opens a login shell in it. The sprite is deleted when the pane closes.
It's for agents and untrusted builds: `illogical run --vm -- claude …`. The
session log stays on the host after the machine is gone.

**Two separate axes: what a block is, and where it runs.** Superlogical treats
a terminal as one block *type* among many (see
[docs/superlogical.md](docs/superlogical.md)). A VM is not a block type. A
terminal in a VM is still a terminal, with the same methods, events, snapshots
and `tail`. So the VM is modelled as *placement*, not as a kind of pane:

- Each block in the tree has a `type`. Only `terminal` exists today; don't
  name anything in a way that assumes every block is a terminal.
- Each block also has a `host`: `local`, or a machine id.
- A **machine** is its own entity in `core`:
  `Machine { id, provider, image, cpus, mem, owner: NodeId, sprite }`.
  - Any block under its owner node can run on it, and no block outside can.
  - When the owner node closes, the machine is deleted.
- **M3b ships pane-owned machines only.** Tab-owned machines ("this tab is a
  throwaway box", where splits inherit the host) are M3c, below.

**Build on the Sprites API, not Firecracker directly.**

- wisp already does create, exec TTY with resize, kill and delete (S4). Going
  straight to Firecracker would mean rewriting wisp's image, network and
  teardown handling.
- Only drop down if wisp turns out to be missing something we need (for
  example snapshot-to-suspend).
- This pulls the exec-TTY half of M4b's Sprites adapter forward: create, exec,
  resize, kill, delete. M4b then adds listing, wake, the proxy and promotion
  on top of it. Anything built here has to fit the `Provider` trait.

**Terminal I/O.**

- Today a terminal is "host PTY plus a child". Add a second backend whose
  bytes come from an exec TTY WebSocket, behind a narrow `TerminalIo`
  (input, resize, output, exit). The backend is chosen by `host`, not by type.
- `Spawn` keeps describing the command (`program`, `args`, `cwd`), and `host`
  says where it runs.
- The host daemon still runs the VtEngine and log writer over those bytes.
  Scrollback and snapshots come from the host, not wisp's 1 MiB replay ring.
- **Protocol:** the tree carries `type` and `host` on each block, and
  `machines` alongside the panes. That is enough for the UI to show a badge
  and the machine's state. OSC 7 reports the guest's hostname anyway.

**Lifecycle.**

- **Create.** Create the sprite for the machine.
  - Create returns in about 14ms and doesn't boot anything. The first exec
    boots it.
  - For each terminal on the machine, exec a login shell with the starting
    size in the URL (`&cols=…&rows=…`; otherwise it starts at 80x24), with
    `max_run_after_disconnect` set to hours so a slow daemon restart doesn't
    find its shells killed. Then send a resize after `session_info`.
  - The spike measured 328ms median from create to a visible prompt
    (284–602ms).
- **Close.** When the owner node closes, `DELETE` the sprite (204 in about
  35ms). Don't kill the execs first: an interactive bash ignores SIGTERM, so
  wisp waits 10s before SIGKILL. If a terminal's process exits but its owner
  node stays open, the machine stays too.
- **Machine gone.** When a sprite disappears, an attached exec closes with
  WebSocket code 1006 and no exit frame. A 1006 alone could be a network drop,
  so the daemon then fetches the sprite:
  - 404 means "machine gone";
  - 200 means reattach.

  A process that ends normally always sends an exit frame, then close 1000.
- **Persistence.**
  - Machines are tree state, so they live in `layout.json`.
  - Exec ids, and a running count of exec bytes received, live in each pane's
    `meta.json`.
  - On a restart (M2b), the daemon reattaches the execs and doesn't create
    new sprites. Then it sends a resize; any attached client can resize,
    `is_owner:false` or not.
- **Replay on reattach (the one awkward bit).**
  - wisp resends its whole ring (up to 1 MiB, from the start of the session)
    on every reattach. That includes bytes the daemon already logged, and
    nothing marks where the replay ends.
  - The daemon skips as many bytes as its stored count. That works until a
    session has produced more than 1 MiB. After that the ring has wrapped and
    can't be aligned by counting.
  - **Fallback:** match the tail of the daemon's log against the replay. If
    that fails, drop the replay and write a `── reattached; output while
    detached may be missing ──` rule.
  - **Ask wisp upstream** for a stream offset in `session_info` or a
    `since=` parameter; that removes the problem.
- **Panes stay attached (for now).**
  - An attached idle exec keeps the sprite `running`, even with no output. A
    detached one pauses after about 31s, and resumes in about 33ms from
    reattach to echo, with the shell intact.
  - So M3b ships VM panes always attached: simple and correct, at the cost of
    never pausing.
  - Detaching idle, unwatched VM panes so their sprite can pause is a
    follow-up that depends on the replay fix. Not tested yet: whether
    WebSocket pings, rather than the open connection, are what keep it awake.
- **Crash sweep.** Name sprites `illogical-eph-<daemon>-<machine>`. At startup,
  delete any whose machine isn't in the tree.
- **After geek reboots (decided 2026-10-01).** wisp runs on geek, so a reboot
  kills every sprite. A VM pane follows its restart policy on a **fresh**
  machine, with scrollback restored from the host log as for local panes:
  - `shell` gets a new VM with a login shell;
  - `rerun` and `hook` run in a new VM;
  - `none` shows "machine gone".

  The restored rule notes that the machine is new.
- **`rerun` during normal running.**
  - On a pane-owned machine, `rerun` gets a fresh VM, because the old one
    went away with the pane.
  - On a tab-owned machine (M3c), `rerun` reuses the tab's machine.
- **Block methods on VM panes.**
  - `process` runs `ps` inside the guest over a second exec. If that fails it
    returns `unavailable`.
  - `capture`, `keys` and `mouse` work unchanged, because they act on the
    host's VT state and input path.

**CLI and UI.**

- `run --vm [--image]` creates a pane-owned machine.
- `illogical machines` lists machines with their owner and sprite state.
- A "New VM pane" action and a host badge on blocks.
- "Machine gone" shown in the exit event.

**Spike: done 2026-10-01.** See [spikes/m3b-machines](spikes/m3b-machines/README.md).

- **Exec TTY is the transport.** `seq 1 1000000` (7.9MB) took 299ms over
  exec (about 26 MB/s), against 285ms through the proxy to an in-guest
  daemon, and 327ms on a local PTY. All lines arrived in order. S4's 400KB/s
  figure doesn't reproduce.
- **Resizing works after reattach,** whether or not the client is the owner,
  and the last resize wins. No owner handoff is needed.
- **Two execs on one sprite are independent:** separate PTYs, sizes and
  sessions, with separate detach and reattach. They share one process space
  and one user, which is what tab-owned machines want.
- **Still open:**
  - whether pings or the open connection keep a sprite awake;
  - what wisp does with a slow reader;
  - a kill with a chosen signal;
  - a wispd restart;
  - behaviour after the 1h warm period, when wisp reboots the VM;
  - how Fly handles a resize from a non-owner, and Fly's exec throughput.

**Done when:**

- from the phone, open a VM pane, run `claude` in it, and close it;
- the sprite is gone from wisp's list;
- `illogical tail %N` still prints its whole session.

### M3c: tab-owned machines (a throwaway box per tab)

A **VM tab** owns one wisp sprite that all its panes share. You open it, split
it, run a shell in one pane and `claude` in another, all on the same files,
and closing the tab deletes the machine. M3b gave one pane a machine. M3c
makes the tab the unit, which is what working in a sandbox actually looks
like. M6 depends on it, because a browser block has to sit on the same machine
as the dev server it shows, and an agent block beside the terminals it works
with.

**It's mostly a change of owner.** M3b's model already has every block carry a
`host` and every `Machine` an `owner: NodeId`, with the rule "blocks under the
owner may run on it, and closing the owner deletes it". M3c lets the owner be
a tab. The M3b spike showed what this needs from wisp: two execs on one sprite
are independent (their own PTY, size and session), share the filesystem and
processes, and detach and reattach separately.

**Decisions (2026-10-01):**

- **Splits default to the tab's machine, and local panes are allowed.**
  - Splitting a pane in a VM tab starts the new pane on the tab's machine.
  - The split menu also offers "Split (local)" for a shell on geek beside the
    sandbox.
  - Local panes in a VM tab carry a `local` badge, so it's always clear which
    side of the line a shell is on.
- **A pane on the tab's machine can't be dragged out of the tab.**
  - The drop is refused, with a short "runs on this tab's machine" message, so
    nothing dies by accident.
  - Local panes in a VM tab move freely.
  - Moving the *whole tab* (to another session or position) is fine. The
    machine goes with it, because the tab owns it.
- **A pane's machine can be promoted to the tab: "Share machine with tab".**
  - The right-click action moves ownership from the pane to its tab. The VM
    keeps running, and new splits join it.
  - It's only offered when the tab has no machine yet.
  - The reverse ("give the machine back to one pane") isn't offered.

**Lifecycle, compared with pane-owned:**

| | Pane-owned (M3b) | Tab-owned (M3c) |
|---|---|---|
| VMs | one per pane | one per tab |
| Split a pane | the new pane is local | the new pane joins the tab's VM (or "Split (local)") |
| A shell exits | the pane follows its policy; closing it deletes the VM | the pane follows its policy; the VM stays until the tab closes |
| `rerun` | gets a fresh VM | reuses the tab's VM |
| After geek reboots | each pane gets a fresh VM | the tab gets **one** fresh VM, then every pane follows its restart policy on it |
| Closing | closing the pane deletes the VM | closing the tab deletes the VM, after its panes' scrollback is flushed |

- **Creating:** "New VM tab" (in the `+` menu and the tab bar's right-click),
  and `illogical run --vm-tab [--image] -- cmd`. Create returns in about 14ms;
  the first exec boots the VM in about a third of a second (M3b spike).
- **The last pane on the machine closes but the tab stays open** (it still
  holds local panes): the machine stays, and the tab shows "machine idle" with
  a "New pane on machine" action. Closing the tab deletes it.
- **Persistence:** unchanged from M3b. The machine lives in `layout.json` with
  its owner, and each pane keeps its exec id and replay byte count in its
  `meta.json`.
- **Crash sweep:** unchanged. Sprites are named per machine, not per pane.

**Cost:** every attached exec keeps the sprite awake (M3b spike), so a VM tab
never pauses while any of its panes is open. That's fine on geek. Pausing
idle, unwatched VM tabs waits for the same replay fix as M3b's.

**UI:**

- The tab itself carries the machine badge (name, state). Pane badges show only
  where they differ (`local`).
- The tab's right-click menu has "Machine": status, "New pane on machine",
  "Reset machine" (delete and recreate; panes restart per policy on the new
  one, with a `── machine reset ──` rule), and "Close tab and machine".
- `illogical machines` shows each machine's owner as `@tab` or `%pane`.

**Done when:**

- from the phone, open a VM tab, split it, and run `claude` in one pane and a
  shell in the other, both seeing the same files;
- add a "Split (local)" pane and see it run on geek;
- a drag of a VM pane out of the tab is refused, while the local one moves;
- promote a VM pane's machine to its tab, and a new split joins it;
- reboot geek, and the tab comes back with one fresh VM and every pane
  restored per policy;
- close the tab, and the sprite is gone from wisp's list.

### M4: reach (a shell on any machine or sandbox)

Shape copied from Superlogical (see [docs/superlogical.md](docs/superlogical.md)):

- Every daemon is a peer: it owns its terminals and serves the page, the
  protocol and the CLI.
- Clients federate several daemons into one host list, which they get from
  the home daemon (below).
- **How a daemon is reached is a pluggable `Transport`.** It must not shape
  the protocol or the core.
- No provider is required. Sandbox providers are adapters.

**The home daemon (accepted for now, decided 2026-10-01).** One daemon (geek's)
is special. It is a directory and control point, never a relay: terminal bytes
go straight between a client and the daemon that owns the terminal. It holds:

- **the host list.** Clients fetch it from the home daemon and then connect to
  each host directly. That means one bookmark on the phone and no lists
  drifting apart between devices. A client caches the last list, so hosts it
  already knows stay reachable while geek is down.
- **provider tokens and adapters** (M4b), and the machines it creates (M3b).
- **minting per-host tokens** for transports without tailnet identity.
- **the receiving end** of dial-out connections and of log sync (M4c).

What happens when geek is down, and whether the role can move or be shared,
is deferred.

**Layout is per host; a tab doesn't mix hosts (decided 2026-10-01).**

- Each daemon owns its own layout tree, and the client switches between
  hosts.
- M3b's VM panes are unaffected, because geek owns both the machine and the
  layout.
- Keep mixing possible later: nothing in `core` or the protocol may assume a
  pane's terminal lives on the daemon that owns the layout. Every block has a
  `host` (M3b). A later "home layout, remote panes" mode would be a host value
  that names another daemon.
- **Options for later:**
  - (a) geek's tree holds panes whose terminals live elsewhere, so the client
    connects to each host and the tree has to cope with panes it can't reach;
  - (b) Superlogical's model, where daemons own terminals only and clients
    arrange tabs and splits. That gives up M1's "same layout live on every
    device".

**Identity.** A daemon authorizes the connecting tailnet identity (from
`Tailscale-User-Login` behind serve, or by asking tailscaled who is connecting
on direct connections) against an allowlist of owners. For transports without
tailnet identity, it accepts a per-host token minted by the home daemon
instead.

**Transports.**

- **Choosing one (from S4).**
  - Machines you own: use the tailnet.
  - Sandboxes that sleep: **wake through the provider first**, because tailnet
    packets don't wake a paused sprite. Then use the tailnet if it comes up
    within about 5s; otherwise stay on the provider tunnel, which is just as
    fast (about 50ms round trip).
  - Clients drop their connections to hosts that aren't visible, because an
    open connection keeps a sprite awake.

1. **tailnet (default for machines you own).** Also used for long-lived
   sandboxes once they are awake.
   - The daemon listens on the node's tailnet address, or behind
     `tailscale serve`.
   - In sandboxes, `tailscaled --tun=userspace-networking` runs with an
     ephemeral, `tag:sandbox` auth key. Ephemeral nodes are removed when they
     go away; the ACL stops `tag:sandbox` reaching other sandboxes or home
     services unless allowed.
   - You get a direct WireGuard path, no single point of failure, the phone
     opening `https://<host>.ts.net` straight to that daemon, and the rest of
     the network (dev servers, rsync, git) for free.
   - An idle `tailscaled` does **not** keep a sprite awake (S4), so it is free
     to leave running. But it can't wake a paused sprite either.
2. **provider wake.** For sandboxes that sleep and can only be woken from
   outside, an adapter does three things: list hosts, open a byte stream to
   the daemon's port, and optionally open a plain exec TTY when no daemon is
   installed.
   - **First adapter: the Sprites API.** This covers Fly and wisp; the
     endpoints are in docs/superlogical.md and ravix-hq/ravix#236.
   - **Later adapters:** `docker exec`, `kubectl port-forward`/`exec`.
   - Status for sleeping hosts comes from the provider API, never by
     connecting.
   - **The `Provider` trait treats differences as capabilities to query**
     (exec replay size, owner-on-reattach, kill semantics), not assumptions.
     S4 found them differ even between two compatible implementations. On
     providers with a small replay buffer (Fly, about 6.5KB), shells opened
     without a daemon are disposable.
3. **dial-out (fallback).** For sandboxes that only allow outbound HTTPS, the
   daemon dials a configured peer with `--peer wss://… --token …` and serves
   the protocol over that socket. The receiving daemon treats it as one more
   host. It isn't a hub; nothing else routes through it.

**S4 conclusions** ([spikes/s4-reach](spikes/s4-reach/README.md)):

1. **Only the provider can wake a sleeping sandbox.** Tailnet packets to a
   paused sprite go nowhere. Connecting means: provider wake (a proxy
   WebSocket or exec), then the data path.
2. **tailscaled is free to keep.** It doesn't hold a sprite awake, survives
   60-minute sleeps, and has a direct path again within seconds of a wake.
   Order: wake through the provider, use the provider tunnel immediately, and
   upgrade to tailnet when it answers.
3. **The provider tunnel is a good data path, not just a waker:** about 50ms
   round trip, the same as the tailnet from geek.
4. **"Cold" means different things per provider.** On Fly it was a memory
   restore every time we saw it; on wisp it's a reboot. The resident daemon
   must handle both: restore from disk if it rebooted, carry on if not.
5. **Provider exec has no durable scrollback** (Fly replays about 6.5KB).
   No-install shells are disposable; history requires resident `illogicald`.
6. **A detached idle shell lets the sprite sleep; output keeps it billed.**
   Clients drop connections to hidden hosts.

**Milestones:**

- **M4a, federation + tailnet.** (Done 2026-10-01; see README.)
  - The host list on the home daemon, shown in the client and cached there.
  - Per-host attach.
  - The CLI takes a `--host` flag.
  - A static `x86_64-unknown-linux-musl` daemon that runs without systemd
    (M2b's scopes and FD store become optional).
  - An `illogical install --tailnet <authkey>` path for sandboxes.
  - **Done when:** from the phone, open a sandbox's own URL and get a working
    vim, and the same host shows in geek's host list.
- **M4b, provider adapters.**
  - The `Provider` trait plus the Sprites adapter.
  - "Open shell" without a daemon, using the provider's exec TTY.
  - "Promote to resident": copy the binary in and register it with the
    provider's restart mechanism (a sprite service). On sprites, reach it
    through the provider tunnel, never a public URL.
  - **Done when:** start `claude` in a resident sprite, let it go cold, reopen
    from the phone, and the layout and scrollback are back.
- **M4c, dial-out and history.** The dial-out transport (read-only share
  tokens moved to M15), and an optional log-segment sync to the home daemon, so history
  outlives a deleted sandbox.
  - **Decided 2026-10-01: encrypt synced segments at rest, with a key held
    by the home daemon.** The question was whether to encrypt logs at rest. Synced sandbox logs are
    where an agent's secrets end up. The likely answer is to encrypt synced
    segments with a key held by the home daemon, and later fetch that key with
    the secrets-manager identity under Risks.

### M6: non-terminal blocks (after M4b; M5 is independent of it)

This is Superlogical's step 2, "multiplexer for all work" (see
[docs/superlogical.md](docs/superlogical.md)), cut down to what illogical is
for: agents, and the dev servers they start in throwaway machines. A terminal
becomes one block type among several. Tabs, splits, drag, close, `host`, the
event stream and the CLI all work the same for every type.

M6 ships two types: **browser** (M6a) and **agent** (M6b). S8 chooses what comes next;
M10 (job and service) and M11 (file and diff) are the expected result.

**The block contract.** Every block type provides:

- **config**, saved in `layout.json`: whatever it needs to recreate the block
  (a URL, an agent session id). Restarting it follows the restart policies,
  read per type.
- **state**, as JSON: `describe %N` returns it, and changes go out as `event`.
- **attention**, which reuses M3's `idle | working | needs-input | done`, so
  badges, the "needs you" list and Web Push work for every type with no extra
  code.
- **`capture --text`**, a plain-text rendering. `history`, `search`, M5 and
  agents can all read every block through this one method.
- **its own methods**, called as `illogical call %N <method> [json]`. A type
  can also add CLI sugar on top.
- **a log** in `blocks/%N/`, using the M2 segment-and-index store. A terminal
  logs bytes; an agent logs its event stream.

**The rules this sets for earlier milestones:**

- **IDs.** All blocks share one ID space (`%N`). M5's tmux front end needs
  every leaf to look like a `%pane`.
- **Store.** The store directory is `blocks/%N/` from M2 onwards (already in
  the Architecture section), so nothing has to be migrated later.
- **Web client.** The `TerminalView` pool becomes a `BlockView` pool with a
  renderer per type. Re-parenting without remounting works the same way.
- **M5.** A `-CC` client sees a non-terminal block as a read-only pane drawn
  from `capture --text`, with a hint to open it in the web app.

#### M6a: browser blocks

A block that shows a web page, mainly a dev server inside a machine: run
`npm run dev` in a terminal block, then open a browser block on port 5173 next
to it, on the desktop and the phone.

- **Prerequisite: tab-owned machines** (M3c). The terminal block
  and the browser block in a tab have to share one VM.
- **Config:** `{ host, port, path }` for machine ports, or `{ url }` for other
  pages.
- **Routing.**
  - Machine ports go through the daemon, which reaches a sprite's port through
    the Sprites proxy (M4b), opening one proxy WebSocket per TCP connection,
    and a local port directly.
  - It has to carry WebSockets, so hot reload works. In S6, hot reload worked
    through the Sprites proxy for Vite 8.3 (about 16–45ms, no full reload) and
    Next.js 16.3 (React state kept).
  - **The proxy speaks HTTP, not raw bytes:**
    - it rewrites `Host` and `Origin` to `localhost:<port>`, which made both
      dev servers work with no config;
    - it strips the `Tailscale-User-*` headers, because serve adds them on
      every port;
    - it enforces its own owner and `Origin` check, because the rewrite turns
      off the dev servers' own host and origin guards.
  - **One hostname per block (decided 2026-10-01).**
    - Each browser block gets its own origin, such as
      `b-42.illogical.<domain>`, served at `/`.
    - Dev servers need no base-path config, and blocks can't read each other's
      pages or storage.
    - Rejected: one shared serve port with `/b/%N/` paths, because every dev
      server needs its base set and all blocks share one origin. Also
      rejected: one serve port per block, because serve may only allow HTTPS
      on 443, 8443 and 10000.
  - **How it works:**
    - a wildcard DNS record `*.illogical.<domain>` points at geek's tailnet
      IP, so only the tailnet can reach it;
    - a wildcard certificate comes from ACME with a DNS-01 challenge (geek
      already has ACME and a Cloudflare token for wisp);
    - the daemon terminates TLS for these names itself, on its own listener
      rather than through `tailscale serve`;
    - it identifies the caller by asking tailscaled who is connecting
      (`WhoIs`), the same path M4 uses for direct connections.
  - **Checked (2026-10-01):**
    - the domain is `illogical.widgets.wtf`: blocks are
      `b-<id>.illogical.widgets.wtf`;
    - `*.illogical.widgets.wtf` is an A record (DNS only) for
      100.71.195.119, and resolves to nothing else (no AAAA) through 1.1.1.1
      and 8.8.8.8;
    - the listener is `100.71.195.119:7443` (443 is serve's, 8443 wispd's);
    - the daemon gets the wildcard certificate itself (Let's Encrypt,
      DNS-01 through Cloudflare's API with wisp's token: staging, then
      production, about 25s each) and renews it two thirds of the way
      through its life. A client on the tailnet verifies the chain.
  - **Dev scheme, for tests:** with no domain, blocks are
    `http://b-<id>-<key>.localhost:<port>` on loopback. There is no WhoIs
    there, so the name carries a random key from the block's config.
- **Security: proxied pages must never share the app's origin.**
  - `tailscale serve` adds your identity to every request, so any script
    served from the app's origin can drive every terminal you have. A dev
    server in a sandbox is running code an agent wrote.
  - So proxied pages are served from a separate origin: each block's own
    hostname (above), never the app's. S6 proved the model with a second
    serve port (:10000), which had a valid certificate and carried hot reload.
    Port 8443 is taken by wispd on geek.
  - The app's WebSocket keeps refusing any `Origin` other than its own. S6
    confirmed a :10000 page gets a 403. The check must match the **exact**
    origin, including the scheme; today it ignores the scheme.
  - The iframe is sandboxed with `allow-scripts allow-forms allow-same-origin`.
    - `allow-same-origin` here means the frame's *own* origin (its block
      hostname), never the app's.
    - Without it, S6 found storage throws, Vite needs `cors: true`, and Next
      fails completely.
- **Other pages.** Many external sites refuse to be framed
  (`X-Frame-Options`, CSP `frame-ancestors`). Those show a card with "open in
  new tab". This is not a browser engine.
- **Methods:** `navigate{url}`, `reload`, `back`.
- **Events:** `navigated{url, title}`, `load_error`.
- **Attention:** the block is `working` while loading and `needs-input` on a
  load error (for example, the dev server died).
- **CLI:** `illogical open [--host m] [--split right] :5173/path` or
  `illogical open https://…`.
- **Done when:**
  - in a tab-owned VM, `npm run dev` runs in one block and its app runs in a
    browser block beside it;
  - hot reload works on the desktop and the phone;
  - a script in that app can't reach the illogical API;
  - closing the tab deletes the VM and both blocks.
- **Done 2026-10-01** (`web/e2e/vm-dev-server.spec.ts`, against a real
  Vite in a VM tab; `browser-ports.spec.ts` and `crates/daemon/tests/sites.rs`
  in the dev scheme). The real-domain scheme was checked by hand on geek;
  the S6 checklist on real phones is still open.

#### M6b: agent blocks (ACP clients)

A structured view of an agent run in place of its TUI: messages, tool calls
and permission requests as UI, with approve and deny buttons that work well on
a phone. A Claude Code TUI in a terminal block stays fully supported. The
agent block is the better phone and audit view, not a replacement.

**The agent block is an ACP client (decided 2026-10-01).** It speaks the
[Agent Client Protocol](https://agentclientprotocol.com) (JSON-RPC over the
agent process's stdio) to whatever agent server the block names. This
replaces the earlier design, which drove Claude Code's own `stream-json` mode
behind a custom `AgentAdapter` trait, with an MCP server as a workaround for
permissions.

- **What ACP gives us:**
  - one protocol for every agent, so there is no per-agent adapter or event
    mapping;
  - `session/update` streams messages, thoughts and tool calls;
  - `session/cancel` interrupts a turn;
  - `session/request_permission` is the native permission request. It puts
    the block in `needs-input`, which pushes to the phone (M3).
  - `session/load` replays a session into a block that was just opened or
    restored.
- **Two kinds of agent, the same block:**
  - **Local agents.** The daemon spawns an ACP agent server with a `cwd` and
    an optional `host`, so it can run in a VM.
    - **Agent definitions:** each is a command line plus a few defaults.
    - **Tested in S7:** Claude Code through `claude-agent-acp` (Fountain pins
      0.81.2; npm has 0.84.0) and Codex through `codex-acp` 2.1.0, with
      `CODEX_PATH` pointing at the installed codex-cli.
    - **Untested:** Gemini CLI and opencode, which aren't installed.
  - **Fountain agents.** The daemon spawns `fountain acp --agent X
    [--vault v] [--permission ask]` (see
    `~/dev/managoat/fountain/docs/integrations/editors.md`).
    - The agent runs in a Fountain sandbox, and the turn lives on Fountain's
      servers, so our restarts and reboots don't touch it. The adapter
      reconnects, and `session/load` replays the transcript.
    - Secrets come from Fountain vaults, and with the egress broker on they
      never enter the sandbox.
    - Idle machines park and cost nothing.
    - **Limits:** the agent can't see local files. Fountain refuses an
      approval left unanswered for 5 minutes, and the turn continues without
      permission. opencode never asks. A reclaimed sandbox keeps the
      transcript, but the agent loses its memory.
- **Agent commands as terminal blocks: not viable with today's adapters (S7).**
  - With `terminal` and `fs` offered, neither `claude-agent-acp` nor
    `codex-acp` ever called `terminal/*` or `fs/*`. Neither has code that
    would, and no setting turns it on.
  - What they send instead is Zed's `_meta.terminal_output` /
    `terminal_output_delta` extension on the tool call. For Claude it arrives
    as one chunk after the command exits.
  - **So M6b renders a command's output inside its tool-call card,** with a
    read-only terminal renderer for the ANSI.
  - The client side of `terminal/*` and `fs/*` stays planned but unbuilt, for
    when an adapter uses it. If that happens, each command becomes a live
    terminal block. Re-check on adapter upgrades.
  - Fountain offers the agent neither capability.
- **Permissions.**
  - **Shapes (S7):**
    - a request offers options such as `{optionId:"allow-once",
      kind:"allow_once"}`, `allow-with-updates` (`allow_always`) and `reject`
      (`reject_once`). They vary by tool, and `reject_always` never appeared;
    - the answer is `{outcome:{outcome:"selected",optionId}}` or
      `{outcome:{outcome:"cancelled"}}`.
  - **Waits:** `claude-agent-acp` waited 25 minutes with no timeout. Fountain
    refuses after 300s: the tool call goes to `failed`, and the turn carries
    on.
  - **Approve or deny** from the block, the notification or
    `illogical call %N approve`.
  - **"Always allow" lives in the block's config, and the block answers from
    it itself.** It never selects the agent's `allow_always` option, because
    `claude-agent-acp` writes that rule into `.claude/settings.local.json` at
    the git root of the agent's cwd (your repo), even with
    `settingSources: []`.
  - **When the block cancels a turn,** it answers every open request with
    `cancelled`.
  - **A card clears when its tool call goes `completed` or `failed`.**
    Fountain's refusal doesn't cancel the client's request.
  - Read-only commands (`ls`, `echo` without a redirect) never ask; only side
    effects reach the client. Codex ran a side-effecting command in its own
    sandbox without asking.
- **Methods:** `send{text}`, `approve{id, option}`, `deny{id, reason}`,
  `cancel`.
- **State:** turn status, the current tool, the pending permission request,
  cost and tokens where the agent reports them.
  - The block is `working` while its `session/prompt` is outstanding,
    `needs-input` while a permission request is open, and `done` or `idle`
    from the stop reason.
  - Cost comes from `usage_update.cost`, which is cumulative per session, so
    store per-turn deltas. Per-turn tokens come in the prompt response, except
    through Fountain, which reports none.
- **History.**
  - The JSON-RPC stream is the block's log.
  - `capture --text` renders the transcript as Markdown.
  - `history` and `search` cover agent runs as well as shell commands, which
    makes "where did the agent's work go" one query.
- **Daemon restarts (M2b), for local agents: the FD store is mandatory.**
  - Unlike `claude -p` in S6, `claude-agent-acp` exits as soon as its
    connection closes. It records a pending permission as rejected and kills
    a running command.
  - Holding the pipes works (S7):
    - the adapter waited 20s with nothing attached;
    - a second client answered the old permission request by its id, got the
      first client's prompt response, and ran another turn on the same
      process.
  - So it needs:
    - the agent server in its own scope, like a pane's shell;
    - the daemon's ends of its stdio pipes in the FD store, like PTY masters;
    - the daemon to persist the open request ids with their options, and its
      own next JSON-RPC id, so ids don't collide after a restart.
  - No permission relay is needed. That was S6's workaround for the MCP
    route.
- **Restore after a reboot.** The ACP session id is stored in the block's
  config.
  - The transcript comes back from the log.
  - The `rerun` and `hook` policies reopen with `session/load`, which replays
    prompts, messages and tool calls (560ms in S7), or `session/resume`,
    which keeps context without replaying.
  - **Fountain agents need care when reconnecting (S7):**
    - a turn keeps running on Fountain while we're gone, but after
      `session/load` the new client gets the replay up to that moment and **no
      live updates** for the rest of the turn. The block shows "running
      remotely" and loads again when the conversation goes idle;
    - a permission request whose client died is **not re-sent**. The
      conversation is `conversation_busy` until Fountain's 5-minute refusal.
      So a daemon restart during a Fountain approval costs that tool call;
    - Fountain's replay leaves out your own prompts (`user_message_chunk`),
      so the block keeps them in its own log.
  - A local agent in a VM resumes on a fresh machine, like M3b.
- **Local Claude Code specifics (S6, S7):**
  - pass `settingSources: []` in `session/new`, otherwise your Claude Code
    settings and hooks, including M3's attention hooks, fire inside agent
    blocks;
  - the adapter defaults to Opus (`opus[1m]`). A model in `_meta` is silently
    ignored; set it with `session/set_config_option`;
  - the adapter runs its own bundled Claude Code (2.1.280 in 0.81.2) unless
    `CLAUDE_CODE_EXECUTABLE` points at yours;
  - Claude's own transcript (`~/.claude/projects/<cwd>/<id>.jsonl`) is the
    source of truth for anything said while the daemon was down.
- **Credentials in VMs (decided 2026-10-01):** a token from a file only the
  user controls, passed into the agent server's environment in the VM and
  never written to the VM's disk or logged: a Claude Code OAuth token from
  `claude setup-token` (`~/.config/illogical/claude-oauth-token`, as
  `CLAUDE_CODE_OAUTH_TOKEN`), or an API key
  (`~/.config/illogical/anthropic-key`, as `ANTHROPIC_API_KEY`). Local agents use the user's own
  Claude Code login.
- **CLI:**
  - `illogical agent [--acp <cmd> | --fountain <agent>] [--host m|--vm]
    [--cwd d] "prompt"` prints the block id;
  - then `wait %N --idle|--needs-input` and `tail %N`.
- **Done when:**
  - from the phone, start a local Claude Code agent block in a VM;
  - its commands' output shows in its tool-call cards;
  - it asks to run something, and you approve it from the push notification;
  - restart the daemon mid-turn and with an approval pending, and the turn
    carries on and the approval still works;
  - reboot geek, and the transcript is back and the agent resumes;
  - the same block type drives Codex and a Fountain agent, and a Fountain turn
    that ran through a daemon restart ends up complete in the block.

#### S7: ACP spike, done 2026-10-01

See [spikes/s7-acp](spikes/s7-acp/README.md). A hand-rolled ACP client of
about 200 lines drove `claude-agent-acp`, `codex-acp` and `fountain acp`
unchanged. The findings are folded in above.

**Still open:**

- answering after Fountain's 5-minute refusal;
- following a Fountain turn live after reattaching;
- an agent block in a VM;
- cancelling with a permission request open;
- permission waits of hours;
- the adapter against the installed Claude Code (2.1.286).

#### S6: done 2026-10-01

See [spikes/s6-blocks](spikes/s6-blocks/README.md). Both block types are
feasible, and the findings are folded in above.

**Still open:**

- real phones (there's a manual checklist in the README);
- cookies when the real app on :443 frames a block's hostname;
- `MCP_TOOL_TIMEOUT` and permission waits of hours;
- Next.js under a `basePath`;
- an agent block running inside a VM.

**Not a replacement for M3b (noted 2026-10-01):** `fountain runner
--backend firecracker` would turn geek into a Fountain runner, with Fountain
agents in Firecracker VMs. It overlaps with wisp, but Fountain deliberately
never creates a sandbox without a conversation, and a VM pane is exactly that.
M3b stays on wisp. Fountain machines are reached through Fountain agent
blocks.

**More block types** are explored in S8 and built in M10 and M11, below.

### After M6: order and triggers

Everything below is planned, but each item starts when its trigger holds, not
on a date. The suggested order:

1. **M7**, because M11 needs its filesystem method, and the picker and session
   names are cheap.
2. **S8**, to choose block types from real use of M6.
3. **M10 / M11.**
4. **M5** when a tmux client is wanted.
5. **M8** when ghostty-web is ready.
6. **M9** when the scale numbers say so.

### M5: tmux control mode (`-CC`) front end

Lets iTerm2, and anything else that speaks tmux control mode, attach to
illogicald and show its tabs and splits as native windows. It is independent
of M4 and M6. The protocol was shaped for this from the start (the M5 rule
under Protocol).

- **First:**
  - capture iTerm2's attach sequence through a logging proxy against real
    tmux;
  - read HTM's tests;
  - list the `%` notifications and commands iTerm2 actually uses.
- **Entry point:** `illogical tmux -CC [attach -t $s]`, which runs over ssh or
  locally. It pretends to be a tmux client on stdio and talks to the daemon
  over its socket.
- **Mapping:**
  - session to session, tab to window, leaf block to `%pane`;
  - layout changes become `%layout-change`, with cells derived from stored
    ratios;
  - output becomes `%output`, which needs octal escaping and flow control
    (`%pause` / `refresh-client -A`).
- **Non-terminal blocks** show as read-only panes drawn from
  `capture --text`, with a one-line hint to open them in the web app.
- **Done when:**
  - iTerm2 attaches to geek over ssh and shows the session's tabs and splits
    as native tabs and splits;
  - a split or drag in iTerm2 shows up in the web client, and the other way
    round;
  - vim in a pane survives detach and reattach;
  - an agent block appears as a readable read-only pane.

### M7: files and navigation

Superlogical's go-to-directory picker, and the filesystem method that M11's
file and diff blocks also need.

- **`fs` methods on every host:**
  - `fs.list(path)`, `fs.stat(path)`, `fs.read(path, range)`,
    `fs.watch(path)`;
  - read-only, scoped to the host's user, with sizes capped.
  - **Hosts with a daemon** (local, M4a peers, resident sandboxes) answer
    these themselves.
  - **Provider-only hosts** (VM panes and no-install sprite shells) go
    through the provider's filesystem API. S4 found the Sprites API has list,
    read and write. This becomes an optional `Provider` capability.
- **The picker.**
  - It opens from the right-click menu, the tab bar's `+`, and an optional
    shortcut.
  - It shows a fuzzy directory list on the focused block's host, starting at
    the block's cwd (from OSC 7) and with recent cwds from `history` first.
  - Actions: "new pane here", "new tab here", "cd there" (sent as input to an
    idle shell only, using M3's `needs-input`/`idle` state).
  - It works on the phone.
- **Generated session names.**
  - New sessions get an adjective-noun name ("drifting cedar") instead of
    `$1`, unique per daemon. VM tabs name their machine the same way.
  - Rename stays a double-click, and the IDs are unchanged.
- **Done when:**
  - from the phone, open the picker on a VM tab, browse to a directory in the
    VM, choose "new pane here", and get a shell in that directory;
  - the same works on a local host and an M4a host;
  - new sessions show generated names.

### M8: client terminal engine (ghostty-web) and local echo

Swaps xterm.js for ghostty-web behind the `BlockView` terminal renderer, so
client and server run the same engine. This is Superlogical's replica model.

- **Trigger:** ghostty-web passes the S1/S5 fixture corpus in a browser,
  including the phone, and its rendering bugs are fixed upstream. Re-check
  each time libghostty-rs is bumped.
- **Wire.**
  - `snapshot` can carry GHOSTSNP, negotiated per client in `hello`. xterm
    clients keep formatter VT bytes.
  - Attach becomes visible-first: screen, then READY, then history newest
    first (the protocol already has `part: screen|history`).
- **Predictive local echo** (Mosh-style), for the phone on cellular and for
  remote hosts.
  - Typed printable characters are drawn at once and underlined until the
    server's output confirms them; mismatches are rolled back.
  - It is off in alt-screen apps and when a password prompt is detected (echo
    off).
  - It's a per-host setting that is on automatically when the measured round
    trip exceeds about 80ms.
- **Done when:**
  - the web client runs ghostty-web on desktop and phone, and the S1 fixture
    corpus renders identically to the daemon's `plain_text()`;
  - a 64k-row pane is usable within 50ms of attach;
  - typing in a Fly-hosted shell from the phone on cellular feels local,
    with underlined predictions that settle correctly.

### M9: parking (scale)

Superlogical's server numbers are about 400KB per terminal against 5MB for
tmux, and unparking takes about 200µs.

- **Trigger:** any one of these:
  - geek's daemon holds more than about 50 panes;
  - an agent fleet runs;
  - RSS per idle pane is measured above 2MB.

  Measure first. The milestone starts with a benchmark (RSS per pane at
  empty, full screen and 10k scrollback; per attached client) committed as a
  test.
- **Terminal parking.**
  - After 60s with no PTY reads, write the VT state as a GHOSTSNP checkpoint,
    using the M2 path and S5's format, and free the engine.
  - Typing doesn't unpark it.
  - Attaching streams the parked snapshot from disk.
  - Parked state is encrypted with the key decided for M4c.
- **PTY parking.** Idle or unwatched PTYs move off their own read tasks onto
  one shared epoll task.
- **Client buffer parking.** Free an idle client's per-pane buffers.
- **Done when:**
  - 500 idle shells on geek cost under 1MB each in daemon RSS;
  - attaching to a parked pane draws it in under 50ms;
  - the benchmark guards against regressions in CI.

### S8: block exploration (after M6 has been used for about two weeks)

This spike decides which block types come next, from evidence instead of a
list.

- **Read the friction log** (`docs/dogfood.md`) and `history` for things done
  in terminals that wanted structure:
  - polling a build;
  - tailing a service's logs;
  - re-reading a diff an agent made;
  - opening a file only to read it.
- **Prototype each candidate as a throwaway type** behind the M6 block
  contract: config, state, attention, `capture --text`, methods, log. Note
  where the contract doesn't fit.
- **Candidates:**
  - **job:** a non-interactive command, or a hal0 or CI job;
  - **service:** a long-running process with restart, logs and a port, for
    example a sprite service or a dev server. It might subsume part of M6a's
    browser block;
  - **file:** a read-only view;
  - **diff:** from `git diff` on a host, or from an agent's edits;
  - **notes:** a Markdown scratchpad per tab, as a wildcard.
- **Output:** a short README choosing types, and changes to the block contract
  if any. M10 and M11 below are the expected outcome, and S8 can reshape or
  drop them.

### M10: job and service blocks

Structured views of work that has no human typing into it. These are
Superlogical's "automatic work disappears into jobs and logs".

- **Job block.**
  - Config: `{ host, cmd, cwd, env, retries, timeout }`, or an adapter
    reference, `{ hal0: job_id }` or `{ ci: url }`.
  - It runs without a PTY: stdout and stderr go into the block log,
    separately.
  - State: queued, running, succeeded or failed with an exit code, the
    attempt number, and duration.
  - Attention maps to `working` / `done`, or `needs-input` on failure.
  - Methods: `retry`, `cancel`, `logs`.
  - `illogical run --job` creates one.
  - **Adapters:** local and host processes first. hal0 jobs and a CI provider
    are separate, optional adapters with the same state shape.
- **Service block.**
  - Config: `{ host, cmd, port?, restart }`.
  - On sprites it maps onto `sprite-env services`; on other hosts the daemon
    supervises it.
  - State: up or down, restarts, the last exit, and the port.
  - If it has a port, "open" makes an M6a browser block next to it.
- **Done when:**
  - a job block runs a build on a VM tab, fails, and shows as `needs-input`
    on the phone;
  - `retry` from the phone succeeds;
  - a service block keeps a dev server up across a cold wake, and opens a
    browser block on its port.

### M11: file and diff blocks (after M7)

Read-only views for checking an agent's work from anywhere, especially the
phone.

- **File block.**
  - Config: `{ host, path }`, read through M7's `fs` methods.
  - Syntax highlighting and line numbers.
  - It follows `fs.watch` live, which matters while an agent edits.
  - `capture --text` returns the file.
- **Diff block.** It takes any of three sources:
  - `{ host, repo, rev_a, rev_b }`, computed by the host's daemon;
  - a working-tree diff;
  - the edit diffs from an M6b agent block's tool-call cards (ACP tool calls
    carry diff content). "Open diff" on a tool call opens one.
  - Unified on the phone, split on the desktop. It is read-only, with "open
    file" per hunk.
- **Done when:**
  - from the phone, open the diff of what an agent just changed in a VM tab,
    tap a hunk, and land in a live file block showing that line;
  - both keep updating while the agent keeps editing.

### Multiplayer track (M12–M15, added 2026-10-01)

Superlogical builds sharing in "from the start". illogical adds it as its own
track, for a small group: a few people you'd hand a shell to, plus their
agents. Enterprise access control stays a non-goal (BRIEF.md).

**What changes from single-user:**
- `config.owner` becomes a list of principals with roles.
- "Last input wins" becomes per-pane driving.
- Every input byte gets an author.

**Order:**
- S12, then M12 and M13 are the core.
- M14 makes write access safe enough to give out.
- M15 reaches people outside your tailnet.
- The track needs M3c (VM tabs) and M4a (federation). It's independent of M6
  to M11, but agent blocks (M6b) gain per-person approvals when both exist.

**Decisions this track makes:**

| Question | Decision | Why |
|---|---|---|
| Unit of sharing | **The session.** Tabs, blocks and machines inherit. Block-level sharing comes later if ever. | One grant to reason about; layout stays one shared tree. |
| Layout | **One shared tree per session, as today.** Focus, scroll, selection and the active tab stay per client. | M1's "same layout live everywhere" already is multiplayer layout, like a shared document. |
| Who types | **One driver per pane.** Viewers take or request control. Free-for-all only in panes marked "pair". | Interleaved keystrokes from two people corrupt commands. Superlogical serializes input; we also make it visible. |
| Pane size | **Follows the driver.** Everyone else letterboxes, as non-owners already do. | Extends the existing rule; no new mechanism. |
| Guests typing on your machine | **Not by default.** A guest's new panes run on a VM (M3b/M3c). Driving one of your local panes needs a per-pane, time-limited "trust" grant. | Write access to a local shell is code execution as your uid. VMs make sharing safe by default. |
| Identity | **Tailnet identity first** (including users from tailnets you share a node with); M15 adds invites for everyone else. | Zero new auth for the common case, and it's already proven (S2). |

#### S12: spike before M12 (about half a day)

- **Node sharing.** Share geek with a second tailnet (a test account).
  - What do `Tailscale-User-Login`/`-Name`/`-Profile-Pic` and WhoIs report
    for a shared-in user, behind serve and on direct connections?
  - Can the ACL limit them to port 443?
- **Funnel.**
  - Is `tailscale funnel` on a second port usable for M15's invite flow?
  - What headers arrive, given there is no identity?
  - What are the rate limits?
- **Input attribution cost.** Add a per-input index record
  `(offset, principal, len)` to the M2 index. Measure index growth with the
  full typing of a day of use, and with `paste` of 1MB.
- **"From now" sharing.** Can a viewer's first snapshot be taken without
  scrollback (screen only, via GHOSTSNP partial encode or the formatter), so
  history before the share point never leaves the daemon?

#### M12: principals and roles

Every request has an author, and every session has an access list.

- **Principals:**
  - **user:** a tailnet login, or an M15 invitee;
  - **agent:** an M6b block, or a CLI/API token. It acts *for* a user, with
    at most that user's role;
  - **host:** an M4 peer daemon.
- **Roles per session:**
  - **owner:** everything, including sharing;
  - **editor:** create, close and arrange blocks; drive panes; approve
    agents;
  - **viewer:** watch, scroll, select, copy, `capture`, `tail`.

  The daemon's owner is owner of every session.
- **Enforced on every path in one place** (a `core` authorization function
  over intents and API calls): the WebSocket, HTTP API, Unix socket (uid maps
  to the daemon owner), CLI, M5's tmux front end, and federation between
  daemons. M4's per-daemon allowlist becomes this.
- **Grants are data.** `acl.json` per session is written atomically like
  `layout.json`, and an audit log records grant changes (who, what, when).
- **Push and approvals are per user.** Web Push subscriptions belong to a
  principal. `needs-input` goes to editors who opted in. An agent permission
  request records who approved it.
- **Done when:**
  - a second tailnet user with `viewer` on one session sees it live and
    nothing else;
  - typing, method calls, `send` and `approve` from them are refused, with a
    403 on the API and a toast in the UI;
  - granting `editor` takes effect without reconnecting, and revoking
    disconnects them within a second;
  - the audit log shows each grant and revoke.

#### M13: live sharing and presence

What it feels like to be in a session with someone.

- **Share dialog** (right-click on the session or tab bar):
  - pick a person, set the role;
  - choose **with history** or **from now**. "From now" means the viewer's
    first snapshot is the screen only, and logs before the share offset are
    never sent (S12).
  - Shows who has access and lets you revoke.
- **Presence.**
  - Avatars (from `Tailscale-Profile-Pic`) on the session, on each tab, and
    on each pane someone is focused on.
  - Each person's focused pane gets an outline in their colour.
  - The `hello`/`layout` messages gain a `presence` list.
- **Driving.**
  - Each pane shows its driver.
  - "Take control" is instant for owners and editors, and leaves the previous
    driver a notice. "Request control" asks the driver.
  - Only the driver's input reaches the PTY; anyone else's keystrokes are
    held with a "you're not driving" hint.
  - "Pair" mode on a pane lets every editor type at once.
  - The driver owns the size.
- **Follow.** Clicking an avatar follows that person's focus (tab and pane)
  until you act.
- **Attribution.**
  - Every input record in the index carries its principal (S12).
  - `history` and command marks show who ran each command.
  - `illogical log %p --who` lists the drivers over time.
- **Done when:**
  - two people on two machines plus a phone are in one session, and each sees
    the others' avatars and focus;
  - control passes back and forth with no interleaved keystrokes;
  - `history` attributes each command to the right person;
  - a "from now" viewer cannot reach earlier output by `tail`, `capture` or
    scrolling.

#### M14: safe write access

Make `editor` something you can hand out.

- **Guest panes run on machines.** A non-owner's new pane or tab defaults to
  a VM (M3b/M3c) in your wisp, with its own quota. A local pane for a guest
  is an owner-only option.
- **Trust grants for local panes.** Before a guest can drive a pane on a real
  host, the owner grants trust for that pane, for a set time (default 30
  minutes), from a prompt they can answer on the phone. It's revocable, and
  it ends when the pane closes.
- **Quotas per principal:** machines, CPU and memory, and concurrent agent
  blocks. Limits are visible in the share dialog.
- **Secrets.** A pane marked "private" is never shown to non-owners. Shared
  sessions warn before showing a pane whose recent output matches common
  token patterns. It's a heuristic, and it says so.
- **Agents.** An editor's agent blocks act as that editor, run on their VM,
  and their approvals go to them. Owners can approve anything.
- **Done when:**
  - an editor opens a tab, gets a VM, and runs `claude` in it;
  - they can't drive the owner's local shell until the owner approves from a
    phone notification;
  - access ends by itself after the grant expires;
  - quotas stop a fourth VM.

#### M15: beyond the tailnet

Share with someone who isn't on your tailnet and won't install anything.

- **Invites.**
  - An owner creates an invite link (role, session, expiry, single-use)
    served over Tailscale Funnel on its own hostname (S12). It is never the
    app's origin on 443.
  - The invitee signs in with GitHub (OAuth) or a passkey. Their principal is
    that GitHub login.
  - Funnel traffic reaches only the invite and session endpoints, never the
    host list or other sessions.
- **Read-only share links** (this replaces M4c's share tokens).
  - A link that shows one session live, read-only, with no sign-in, until it
    expires.
  - It's "from now" by default.
- **Hardening** (needed once the app faces the internet):
  - rate limits and lockouts per invite;
  - a CSP, and the M6a origin rules for proxied pages;
  - audit entries carry the invitee's IP;
  - a kill switch, `illogical sharing off`, that closes Funnel and revokes
    every outside principal.
- **Sessions shared with you.** Your client lists sessions other people's
  daemons share with you, using M4a federation with your identity, under a
  "shared with me" section of the host list.
- **Done when:**
  - someone with only a browser and a GitHub account opens an invite, signs
    in, watches a session, takes control of a pane in their own VM, and loses
    access when the invite is revoked;
  - a read-only link stops working at expiry;
  - `illogical sharing off` cuts everyone outside the tailnet within a
    second.

**Not planned in this track:**
- organisations, SSO/SCIM and policy engines (Superlogical's step 3);
- text chat and comments (for now, use a notes block from S8 if it exists);
- voice;
- shared undo of layout changes.

## Acceptance tests (automated where possible)

| Brief test | How it's checked |
|---|---|
| Kill browser mid-command; output complete, vim/htop correct | Playwright: run `seq 1e6` and vim, kill the context, reattach. Compare xterm's buffer with the daemon's `plain_text()`; screenshot-diff the vim pane. |
| Two machines + phone, same layout and live output | Playwright with 3 contexts (one mobile viewport): intents from A show up in B and C within 200ms. Then a manual check from a real phone. |
| Reboot restores tabs, splits, cwd, scrollback; `rerun` panes running | Integration test: stop the daemon with SIGKILL, wipe the runtime dir, start it, and assert on the tree, cwd and scrollback text. Then one real reboot per release. |
| A full day without a layout shortcut | Jake dogfoods for a day. Keep a friction log in `docs/dogfood.md`. |

Unit and property tests live in `core`. The `vt` crate is tested with snapshot round-trips over recorded fixtures (`tests/fixtures/*.bin`) from S1.

## Risks

- **libghostty-vt API churn (pre-1.0).** Pin the Ghostty commit (the crate pins `a887df42`, which needs Zig 0.15.2 exactly); keep the trait narrow; run the S1 fixture corpus in CI against both Ghostty and `@xterm/headless` so a bump that breaks snapshots fails loudly.
- **Snapshot fidelity edge cases** (wide characters, graphemes, images). Grow the fixture corpus whenever a bug turns up, and add a "redraw" menu item that does a snapshot plus SIGWINCH.
- **Browser-reserved keys** (Ctrl+W/T/N) in the windowed PWA. The terminal can't receive them; offer a fullscreen + Keyboard Lock toggle, and accept it.
- **Phone input quirks** (IME, autocorrect). Turn off autocorrect and autocapitalize on xterm's textarea; keep the extra-keys bar.
- **The tailnet hostname** appears in Certificate Transparency logs. Acceptable.
- **Sprites billing.** Anything that holds a connection open (an attached
  pane, a status poll over the proxy) keeps a sprite awake. Status comes from
  the Sprites API, and hidden sprites get disconnected.
- **Provider token scope** (for example `SPRITE_TOKEN`). It can control
  every sandbox in the org. It lives only in the home daemon's config (mode
  0600) and never goes to a client or a sandbox.
  - **Later:** fetch it at runtime from a secrets manager such as Infisical,
    using a machine identity per daemon, instead of keeping it in a config
    file. The same mechanism could give sandboxes short-lived, narrowly scoped
    secrets without the home daemon handing them out.
- **Sandboxes run untrusted agents.** A sandbox daemon must never hold
  credentials that reach other hosts: `tag:sandbox` ACLs, and per-host
  dial-out tokens that can only register that host.
- **Clickjacking: fixed 2026-10-01, after M2b.** S6 found the app could be framed by any page, because serve authenticates by source.
  - Every response now carries `Content-Security-Policy: frame-ancestors 'none'` and `X-Frame-Options: DENY`.
  - WebSocket `Origin` must match exactly, scheme and port included: `http://` for loopback and `https://` for the tailnet name.
  - Still to do in M3: the HTTP API must refuse cross-origin requests that change anything (an `Origin` check, JSON-only bodies, no simple-form POSTs).
- **Untrusted pages on the app's origin (M6a).** `tailscale serve` adds your identity to every request, so any script served from the app's origin is you. Proxied dev servers must be on a separate origin, and the WebSocket must keep checking `Origin`.
- **Loopback trust.** Any local process can forge serve headers on 127.0.0.1. That is the same trust as the uid, and acceptable for single-user; require the `Host` header to match anyway.

## One-time setup (done 2026-10-01)

1. rustup (stable 1.98) in `~/.cargo`.
2. Zig 0.15.2 and 0.16.0 in `~/.local/opt`; `~/.local/bin/zig` points at 0.16. Builds of libghostty-vt need 0.15.2 first on PATH.
3. Neovim 0.12 in `~/.local/opt` (for fixtures).
4. `sudo tailscale set --operator=jake`.
