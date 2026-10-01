# illogical: plan

Written 2026-10-01 from [BRIEF.md](BRIEF.md) and [docs/research.md](docs/research.md).
Scope: v1 is M0 to M2 (with M2b) plus enough of M3 to `run`/`tail`/`wait`.
Beyond v1, M3b (ephemeral machines) and M4 (reach) are planned with their
shape decisions made, and M5 is kept cheap. Decisions made after the first
draft are dated inline.

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
           panes/%N/log/000001.seg ...     raw output bytes
           panes/%N/index                  (offset, ts, resize | osc133 | osc7 | exit)
           panes/%N/meta.json              cmd, cwd, policy, exit status
           panes/%N/checkpoint             periodic VT snapshot + log offset
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
  - Exec replay on reattach: about 6.5KB on Fly, 1 MiB on wisp. Ownership on
    reattach differs: `is_owner:true` on Fly, `false` on wisp.
  - Proxy round trip is about 50ms on both, the same as the tailnet from geek.
  - Still pending: a cold wake on wisp; tailscaled on wisp; an ephemeral node
    surviving 60 min cold.

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

- **Log store:** segments of 4MB, plus the index and checkpoints (on idle 5s or every 2MB). Checkpoints are in whichever format S5 picks. Retention defaults to 256MB per pane and is configurable.
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

### M3b: ephemeral machines (a fresh VM owned by a pane or tab)

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
- **M3b ships pane-owned machines only.** A tab-owned machine ("this tab is a
  throwaway box", where splits inherit the host) is the cheap follow-up the
  model is shaped for: a shell and `claude` side by side on one machine. Later
  that tab could add a browser block on the VM's dev port (through the Sprites
  proxy, which S4 showed working) or an agent block. Both are out of scope here.

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

- **Create.** Create the sprite for the machine. For each terminal on it, exec
  a login shell with `max_run_after_disconnect`, then send a resize after
  `session_info`.
- **Close.** When the owner node closes, kill its execs, then `DELETE` the
  sprite. If a terminal's process exits but its owner node stays open, the
  machine stays too.
- **Persistence.**
  - Machines are tree state, so they live in `layout.json`.
  - Exec ids live in each pane's `meta.json`.
  - On a restart (M2b), the daemon reattaches the execs and doesn't create
    new sprites.
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
  - Once tab ownership exists, `rerun` reuses the tab's machine.
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

**Spike first (about half a day, against local wisp):**

- How long from create to the first prompt?
- wisp reattaches with `is_owner:false`. Can a reattached exec still resize?
  If not, reattach after a restart leaves the size fixed, and we have to ask
  wisp for an owner handoff.
- Is exec throughput (about 400KB/s in S4) OK for `seq 1e6`? If not, run the
  shell through `s4-probe` over the proxy instead of exec.
- How long does a paused sprite take to resume when you type into an idle VM
  pane?
- Do two execs on one sprite behave independently? The tab-owned follow-up
  depends on it.

**Done when:**

- from the phone, open a VM pane, run `claude` in it, and close it;
- the sprite is gone from wisp's list;
- `illogical tail %N` still prints its whole session.

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

**Milestones:**

- **M4a, federation + tailnet.**
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
- **M4c, dial-out and history.** The dial-out transport, read-only share
  tokens, and an optional log-segment sync to the home daemon, so history
  outlives a deleted sandbox.
  - **Decide first:** whether to encrypt logs at rest. Synced sandbox logs are
    where an agent's secrets end up. The likely answer is to encrypt synced
    segments with a key held by the home daemon, and later fetch that key with
    the secrets-manager identity under Risks.

### Later

- **M5:** a `-CC` front end on the daemon. First capture iTerm2's attach sequence through a logging proxy against real tmux, and read HTM's tests.
- **ghostty-web:** swap it in behind `TerminalView` once it is past its current bugs. Then attach can send GHOSTSNP directly, and history really does arrive newest first.
- **Parking** (Superlogical's numbers are about 400KB per terminal against 5MB for tmux; unparking takes about 200µs):
  - **Terminal parking:** after 60s with no PTY reads, write the VT state to disk (encrypted, because scrollback holds secrets) and free the engine. Typing doesn't unpark it; attaching streams the parked snapshot from disk. This reuses the checkpoint path, so S5's format decision covers it.
  - **PTY parking:** idle or unwatched PTYs move off their own read tasks onto one shared epoll task.
  - **Client buffer parking:** free an idle client's per-pane buffers.
  - Do this once there are tens of panes or agent fleets, not before.
- **Small UX items (from Superlogical, decided 2026-10-01):**
  - **A go-to-directory picker** that works on remote hosts. It needs a small
    filesystem-listing method on each daemon. "New pane here" and "cd there"
    are mouse-first.
  - **Mosh-style local echo** for the phone on cellular and for remote hosts:
    predict typed characters and underline them until the server confirms.
  - **Automatically generated session names** ("drifting cedar") in place of
    `$1`, which you can rename.

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
- **Loopback trust.** Any local process can forge serve headers on 127.0.0.1. That is the same trust as the uid, and acceptable for single-user; require the `Host` header to match anyway.

## One-time setup (done 2026-10-01)

1. rustup (stable 1.98) in `~/.cargo`.
2. Zig 0.15.2 and 0.16.0 in `~/.local/opt`; `~/.local/bin/zig` points at 0.16. Builds of libghostty-vt need 0.15.2 first on PATH.
3. Neovim 0.12 in `~/.local/opt` (for fixtures).
4. `sudo tailscale set --operator=jake`.
