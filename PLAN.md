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
  - **Visible-first attach is gated on measurement (decided 2026-10-01). Measured in S10: don't build it for xterm.js.**
    - **Where the time goes:** on an emulated Pixel 7 at 4x CPU throttling and 10 Mbps / 50 ms, attaching to a 64k-row pane took 3.7 s, of which 3.2 s was download. The snapshot goes out uncompressed (3.9 MB; 183 KB gzipped). The client keeps only 10k lines, so 54k of the 64k rows are downloaded and thrown away. Even a small screen takes about 150 ms to draw at 4x, which is the most visible-first could save. See [spikes/s10-ghostty-web](spikes/s10-ghostty-web/README.md).
    - **Do instead (small, server-side, fix now):**
      - **compress snapshot frames** (permessage-deflate, or zstd frames);
      - **cap the history in a snapshot at the client's scrollback** (sent in `attach`).

      Together they took the 64k case on the throttled phone from about 3.7 s to about 0.35 s. Real terminal output compresses worse than S10's synthetic lines, so re-measure.
    - M8 gives visible-first natively later, through GHOSTSNP's screen-then-history split. The design below is kept only for reference.
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

- **S1/S5 follow-ups: done 2026-10-01.** See [spikes/s1s5-followup](spikes/s1s5-followup/README.md).
  - **New fixtures:** Claude Code (it draws in the alt screen), origin mode,
    DECSLRM, the saved cursor in primary and alt screens (1049 and 47),
    Kitty graphics and sixel.
  - **Checkpoints (GHOSTSNP)** round-trip all of them exactly, except Kitty
    images, which GHOSTSNP v1 leaves out by design. libghostty doesn't parse
    sixel, so there's nothing to carry.
  - **The wire path is wrong today on 13 of 18 fixtures,** in Ghostty and in
    xterm.js. This is the formatter snapshot plus fix-ups that the browser
    gets on attach. The visible failures:
    - the screen shifts up a row whenever the cursor sits below the last
      text (after a program exits, after `clear`);
    - the saved cursor is lost;
    - origin-mode cursors are off;
    - blank cells take the previous text's colours (black boxes beside
      Claude Code's logo);
    - hyperlinks and protected cells are lost;
    - the primary screen's Kitty keyboard flags are lost while an alt screen
      shows.

    S1's fixtures all left the cursor on their last line of text, so they
    missed this.
  - **Fix now, in `crates/vt` (not tied to a milestone).** All seven fixes
    are prototyped in the spike's `src/patched.rs`, and with them all 17
    non-image fixtures are exact in both engines:
    1. pad dropped rows straight after the content, not after the cursor
       move;
    2. place the cursor relative to the scroll region under DECOM;
    3. carry each screen's saved cursor, by cloning through GHOSTSNP and
       restoring on the clone to read it;
    4. replay into a scratch terminal and repaint cells that differ
       (colours, hyperlinks, protection);
    5. emit the primary screen's Kitty keyboard flags;
    6. turn off Kitty image storage for xterm.js clients
       (`set_kitty_image_storage_limit(0)`): today the engine tells programs
       images work, and each pane holds up to 10 MB of images nobody sees;
    7. add the new fixtures, plus probes of cursor, saved cursor and cells,
       to the crate's tests.

    Snapshots then take 1–3.3 ms instead of 0.03–0.5 ms, mostly from the
    prototype's slow cell compare.
  - **Cross-build:** the pinned Ghostty (`22d13172`) and `main` (`0081d453`)
    can't read each other's GHOSTSNP. Both directions fail cleanly with
    `INVALID_VALUE` on every fixture (the 64-byte BLAKE3 removal). So M2's
    "discard and replay" is safe.
    - But the tag doesn't tell builds apart: `build_info` says `0.1.0-dev`
      in both, and `engine_tag()` is identical.
    - Derive a real tag at build time, or hash a fixed canary terminal's
      GHOSTSNP.
  - **Upstream issues to file:**
    - GHOSTSNP: bump the version on wire changes, and carry Kitty images;
    - `build_info`: include the git hash;
    - the formatter: blank-cell colours, hyperlinks and protection not
      emitted, cursor move before the scroll region, only the active
      screen's Kitty keyboard flags.
  - **Still open:** a pending wrap on the live cursor (no fixture ends with
    one); a longer Claude Code session with tool output.

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
- **Follow-ups: done 2026-10-01.** See [spikes/m3b-followup](spikes/m3b-followup/README.md). It also read wisp's source (`~/dev/jhgaylor/mini-sprites`).
  - **An open exec connection keeps a sprite awake, pings or not.**
    - wisp sends no pings. wispd keeps the sprite awake while any `/exec`
      request is open, and the idle check also counts attached sessions.
    - So **pausing means detaching**, and VM tabs can't stay attached and
      still pause.
    - wisp counts only exec I/O and API calls as activity. A *detached*
      session doing silent work (a `sleep`, a CPU-bound loop) was paused 33s
      into it.
    - `is_active` in `GET /exec` means "I/O in the last 5s", not "a client is
      attached".
  - **Slow reader:** backpressure reaches the guest, so `seq` blocked with
    about 8 MB in flight and lost nothing over a 20s stall.
    - At about 30s the guest agent drops the client (close 1006). Output
      after that goes only to the 1 MiB ring, so about 3.1M lines were lost
      with no marker.
    - wispd's memory didn't grow.
  - **Kill with a signal:** `POST …/kill?signal=HUP&timeout=3s` (also `9`,
    `SIGKILL`; a JSON body is ignored), or a `{"type":"signal","signal":"HUP"}`
    frame on the open connection.
    - HUP exits an interactive bash in 1–6ms (129). `nohup` processes
      survive.
    - `machine.rs` already uses `?signal=HUP&timeout=3s`.
  - **The cold reboot:** `warm` at 32s after detach, `cold` 60–62.5 min later.
    - The next request boots it in about 100ms, but reattaching the old
      session gets a plain **404 "exec session not found"** after 334ms.
    - Everything in the old session is gone: the shell, every process
      including `nohup` ones, `/tmp` and the session list. Only `~` is
      kept. A 6h `max_run_after_disconnect` doesn't help.
  - **Replay:** attaching with `output_offset=N` (as the daemon does) replays
    exactly what's after N, with no duplicates.
    - If N is older than the ring, wisp silently sends the whole ring.
    - Matching the tail of what the client has against the replay is unique
      for varied output at 64–256 bytes, and never wrong. It never matches
      for repetitive output (`yes`, watch loops).
  - **Fix now, in `machine.rs`:**
    - **Recognise "machine restarted."** A 404 "exec session not found"
      while the sprite still exists means wisp rebooted the VM. Stop
      retrying (today: three retries, then `Lost { machine_gone: false }`).
      Restart the panes per their restart policy **on the same sprite**,
      because the disk is kept.
    - **Detect replay gaps.** Attach at `received − 256` and compare the
      first 256 replayed bytes with the end of the log. If they match, the
      stream is contiguous; otherwise write the "output may be missing"
      rule. Today it attaches at exactly `received` and would miss a gap.
    - **Never stop reading an exec WebSocket for 30s or more.** Slow
      viewers are absorbed on the daemon's side (the log, the per-client
      queue), never by pausing the read from wisp.
  - **For detaching idle panes later:** only detach a pane whose shell is at
    its prompt with no foreground job (from M3's command marks). Otherwise
    silent work gets frozen 30s after its last output. Reattach on input or
    when a viewer arrives. wisp's in-guest keep-awake ("task") API might
    cover silent work, but that was only read in the source, not tested.
  - **Ask upstream:** `session_info` should say where the ring starts, so a
    gap is explicit.
- **Still open:**
  - a wispd restart;
  - a slow-but-steady reader;
  - wisp's keep-awake task API;
  - Fly (resize from a non-owner, exec throughput).

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
  - **Done 2026-10-01** (see README; `web/e2e/resident.spec.ts` with
    `ILLOGICAL_E2E_CLAUDE=1`, cold forced with wispd's suspend + cool).
    The tunnel is `/tunnel/<host>` on the home daemon with a per-host
    token. Not yet exercised against Fly.
- **M4c, dial-out and history.** (Done 2026-10-01; see README.) The dial-out transport (read-only share
  tokens moved to M15), and an optional log-segment sync to the home daemon, so history
  outlives a deleted sandbox.
  - Read-only share links landed here anyway, for tailnet users only (any
    user, never tagged nodes or Funnel); M15 still owns reaching people
    outside the tailnet, and S12's "from now" snapshot (a link shows the
    pane's scrollback too).
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

### M6c: questions and forms from agents (after M6b)

When an agent asks you something (Claude Code's AskUserQuestion: a few
questions, each with options and an "Other" box), you answer it from a card,
on the desktop or the phone. It works the same in an agent block and for
Claude Code running in a terminal block.

**What happens today.** `claude-agent-acp` turns AskUserQuestion into an ACP
**form elicitation** (`elicitation/create`, an unstable part of ACP), but only
if the client declares it can show forms. illogical doesn't
(`crates/daemon/src/agent/mod.rs`: `clientCapabilities`). S13 found that
without the capability, both adapter versions **disable the AskUserQuestion
tool entirely**, so the model just asks in plain text and nothing is pending.
In the Claude TUI, the question is a keyboard picker that's awkward on a
phone.

**Decisions (2026-10-01):**

- It covers **agent blocks and Claude Code in terminal blocks**.
- **An unanswered question waits indefinitely,** like a pending approval: the
  block stays `needs-input`, with a push notification, until you answer.
- **Generic MCP forms and sign-in links are included,** because they use the
  same mechanism.

#### Agent blocks

- **Declare `clientCapabilities.elicitation: { form: {}, url: {} }`** in
  `initialize`, and handle `elicitation/create` from the agent. Booleans
  (`form: true`) don't work: the ACP SDK's parser silently drops them, and
  the adapter treats that as "not supported".
- **AskUserQuestion forms are recognised and drawn as a question card:**
  - **Recognise one by its tool call, not by `_meta`.** The request's
    `toolCallId` matches a tool call named AskUserQuestion. The update just
    before it carries `rawInput.questions`, with headers, options and
    previews. 0.85.0 sends the `_meta._askUserQuestionCustomAnswer` marker
    only to JetBrains clients.
  - **Field layout:**
    - fields are named `question_<n>`, each with a companion
      `question_<n>_custom` field titled "Other";
    - single-select questions are a `oneOf` enum, multi-select ones an
      `anyOf` array;
    - with one question, `message` is the question; with several, each
      field's `description` holds its question.
  - **Answers are option labels.** "Other" text on its own becomes the answer.
    Next to a single-select pick, it becomes a note; in a multi-select, it's
    added as one more item.
  - Each question shows its options as buttons (single) or checkboxes (multi),
    with descriptions, and an "Other" text box.
  - An option's preview (mockups, code, under
    `_meta["_claude/askUserQuestionOption"].preview`) shows in monospace when
    the option is focused or tapped.
  - **Submit** answers with `{action:"accept", content}`.
  - **Skip** answers `decline`. The tool records "The user did not answer the
    questions.", and the turn continues.
  - **Stop is `session/cancel` alone.** The adapter withdraws its own open
    request with a `$/cancel_request {requestId}` notification, and the turn
    ends `cancelled` in about 10ms. No answer is needed, and a late one is
    ignored. Answering `{action:"cancel"}` instead stops nothing: the tool
    fails, and the model carries on confused.
    - The block treats `$/cancel_request` as "withdraw this card".
- **Any other form** is drawn generically from its JSON Schema: strings,
  numbers, booleans, enums and multi-select arrays, with titles and
  descriptions.
  - **MCP server forms** come without a `toolCallId`, and their schema passes
    through as written, including the old-style `enum` + `enumNames`.
  - **Codex** asks through `elicitation/create` only in its plan mode, with a
    different layout:
    - fields are named by its question ids;
    - "Other" is a "None of the above" option plus a `<id>_note` field;
    - `required` is set.

    The generic renderer covers it.
- **URL elicitations** (an MCP server's sign-in, for example) show a card with
  the message and an "Open link" button. The card closes when the agent sends
  `elicitation/complete` with its `elicitationId`. Dismissing it answers
  `decline`.
- **It behaves like a pending approval, and reuses that machinery** (S13
  confirmed a pending question survives a held-pipes restart and is answered
  by its old id):
  - the block is `needs-input`, with a push notification whose text is the
    first question;
  - a single single-select question with up to two options can be answered
    from the notification itself; anything else opens the block;
  - the open request is in the block's log, so it survives a daemon restart
    (M6b's held pipes) and a reload, and any client can answer it. The first
    answer wins, and the other clients' cards close.
- **Methods and CLI:**
  - `answer{id, content}` and `decline{id}`;
  - `illogical call %N answer '{"question_0":"…"}'`;
  - `wait %N --needs-input` prints the pending question as JSON, so a script,
    or another agent, can answer it.
- **History:** the question and the answer appear in the transcript,
  `capture --text`, `history` and `search`.
- **Fountain doesn't forward questions (S13).** Inside its sandbox the
  adapter never gets the capability, so the tool is disabled and the agent
  asks in plain text. The block needs nothing special. Ask Fountain to
  forward the elicitation capability.
- **Codex in its default mode hangs (S13).** It uses an async variant of its
  question tool that `codex-acp` doesn't pass on, and the model loops on
  `sleep`. Report it upstream to `codex-acp`. Meanwhile, the block's existing
  attention heuristics should surface a Codex turn that's busy for minutes
  with no output.

#### Claude Code in a terminal block

- **The hook.** illogical's Claude Code hooks (the same set M3's attention
  hooks come from) gain a `PreToolUse` hook matching `AskUserQuestion`. It
  runs `illogical ask`, which only acts when `ILLOGICAL_PANE` is set:
  1. it reads the hook input on stdin (`tool_input.questions`, `tool_use_id`,
     `session_id`);
  2. it posts the questions to the daemon, which puts the pane in
     `needs-input` and shows the same question card next to the terminal, on
     every client, with a push notification;
  3. it blocks until you answer, then prints
     `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{…questions, "answers":{…}, "annotations":{…}}}}`.
     `answers` is keyed by question text and holds option labels;
     `annotations` carries "Other" notes. S13 showed Claude Code then runs
     the tool with those answers and never shows its picker.
- **Answering in the terminal instead.** The card has an "Answer in terminal"
  button. It makes the hook exit with no output, so Claude Code shows its
  normal picker.
- **Waiting.** Hooks have no maximum timeout: the settings schema only
  requires a positive number, and the default is 10 minutes. S13 waited 660s
  with `timeout: 900`. So the hook is installed with a very large timeout,
  for example 7 days, which honours "wait indefinitely".
  - When a timeout expires, Claude Code sends the hook SIGTERM and shows its
    picker.
  - The TUI stays responsive while the hook waits. Esc or Ctrl-C interrupts
    the turn and sends the hook SIGTERM.
  - **`illogical ask` must catch SIGTERM and withdraw the card,** so it
    never outlives the question.
- **Outside illogical,** the hook exits silently and changes nothing.

#### S13: done 2026-10-01

See [spikes/s13-questions](spikes/s13-questions/README.md). The findings are
folded in above.

**Still open:**

- hook waits of hours;
- a real OAuth sign-in;
- Codex's own answer deadline;
- two clients answering the same question;
- questions from a subagent;
- the "retry with another model" form.

#### Done when

- From the phone, an agent block's AskUserQuestion with two questions (one
  multi-select, one answered with "Other") is answered from its card. The
  agent continues with those answers, and the transcript shows them.
- A single question with two options is answered straight from the push
  notification.
- A pending question survives a daemon restart and is answered afterwards.
- Claude Code in a terminal block asks a question, it's answered from the
  phone, and the TUI never shows its picker. "Answer in terminal" brings the
  picker back, and pressing Esc in the TUI withdraws the card.
- Stop on an agent block with a question open ends the turn at once.
- An MCP server's form is filled in, and its sign-in link opens and completes.

### M16: an MCP server (illogical as tools for any agent)

Any MCP client (Claude Code, Codex, Claude Desktop, an agent block) gets
illogical as typed tools: run commands in durable panes you can watch and
take over, spin up throwaway VMs, open a dev server next to its terminal,
start and supervise other agents, and search what happened yesterday. M3
already built the CLI and HTTP API, so M16 is a curated layer over them, not
new machinery.

**Why it beats an agent's own Bash tool:**

- **Work outlives the agent's turn.** A build started through MCP runs in a
  real pane. You see it on the phone, scroll it, take it over. It survives
  the agent's session and daemon restarts, and the agent picks it up again
  with `wait` or `read_output`.
- **Sandboxes on demand:** `run` with `vm: true` is an isolated machine that
  disappears afterwards.
- **Agents supervising agents:** `start_agent`, `wait` until it's idle or
  asking, read its transcript, and answer its approvals and questions (M6c),
  or leave them for you.
- **Showing you things:** `open_port` puts its dev server in a browser block
  beside its terminal.
- **Memory across sessions:** `history` and `search`.
- **Permissions per tool** in the MCP client, for example always allowing
  `read_output` and `wait` while asking before `run`.

**Decisions (2026-10-01):**

- **Both transports now:** stdio and HTTP.
- **Scope: everything, like the CLI.** An external MCP client can touch any
  pane on any host. The MCP client's own tool permissions are the guard,
  which makes tool annotations matter (below).
- **Injected into agent blocks automatically, scoped to the block's tab.**

**Transports (one implementation):**

- **The daemon serves MCP over Streamable HTTP at `/mcp`,** with the same
  auth as the API:
  - the owner over the tailnet (serve headers, or `WhoIs` on direct
    listeners);
  - per-client bearer tokens from `illogical mcp token [--name n]
    [--scope …]`, revocable, for clients without tailnet identity;
  - the exact-`Origin` rule for browsers (the MCP spec requires this
    check). Non-browser clients send no `Origin`.
- **`illogical mcp` is a stdio bridge** to that endpoint over the daemon's
  Unix socket, or to another daemon with `--host`. Claude Code and Codex
  configure it as a plain command:
  ```
  claude mcp add illogical -- illogical mcp
  ```
- **Implementation:** the official Rust SDK (`rmcp`) in the daemon. S14
  checks it covers Streamable HTTP, resource subscriptions and progress
  notifications.

**Tools.** About a dozen, shaped for agents rather than mirroring every
endpoint. Each returns a short text summary plus `structuredContent`, with
output capped and pageable by offset, so a chatty pane can't flood the
agent's context.

| Tool | What it does | Annotations |
|---|---|---|
| `run` | Run a command in a new tab or split; `cwd`, `vm`/`vm_tab`/`machine`, `host`, `policy`, `wait` (with a timeout). Returns the pane, and with `wait`, its exit code and the last lines. | not read-only, not idempotent |
| `send_input` | Text (with optional Enter) or named keys (`C-c`, `Up`) to a pane | not read-only |
| `read_output` | A pane's output from an offset, or its last command (escape sequences stripped). Returns text and the next offset. | read-only |
| `capture_screen` | The visible screen as text | read-only |
| `wait` | Until command end, exit, a regex match, idle or needs-input, with a timeout. On timeout it returns "still running" and the offset, so the agent calls again. | read-only |
| `list` | Panes and blocks: type, host, cwd, command, attention state | read-only |
| `close` | Close a pane or block (and a pane-owned VM) | destructive |
| `history` / `search` | Commands across panes (failed, since, cwd), and full-text search of logs | read-only |
| `open_port` | A browser block on a port of the pane's machine, beside it | not read-only |
| `start_agent` | An agent block (Claude Code, Codex, a Fountain agent) with a prompt; returns the block | not read-only |
| `agent_respond` | Approve or deny a pending permission, or answer a pending question (M6c) | not read-only |
| `read_file` | A file on a pane's host or VM (M7's `fs`), capped | read-only |

- **Long calls:** `run --wait` and `wait` send progress notifications. They
  also return before the client's MCP tool timeout (S14 measures Claude
  Code's) with a resumable "still running", so a long build never fails a
  tool call.
- **Errors** are tool results (`isError`) with a sentence an agent can act
  on, for example "pane %7 is gone; it exited 2 at 14:03", not protocol
  errors.

**Resources:**

- `illogical://pane/%N/output` (subscribable, so a client can follow a pane
  live), `illogical://pane/%N/screen`, `illogical://block/%N` (state), and
  `illogical://history`.
- Resource templates, so clients can list them.

**Agent blocks get it automatically, scoped to their tab:**

- `session/new` passes `mcpServers` with an `illogical` server. S13 showed
  `claude-agent-acp` uses MCP servers passed that way.
- The scope is a token minted per block. The agent can create panes and
  blocks in its own tab (on the tab's machine, in a VM tab), read and drive
  what it created, and read the rest of its tab. It can't touch other tabs
  or hosts.
- **Local agents** get the stdio bridge with that token.
- **VM agents** need to reach the daemon from inside the VM. That means an
  HTTP endpoint on wisp's bridge address, or the bridge running host-side.
  S14 checks what the guest network allows.
- **Fountain agents** can't reach the tailnet, so they don't get it.

**Safety.** External clients get full scope, so:

- every tool carries honest annotations (`readOnlyHint`, `destructiveHint`,
  `idempotentHint`), which clients use to decide what to ask about;
- the README recommends a Claude Code permission set: allow the read-only
  tools, ask for the rest;
- every MCP call is logged with the client's name and token. The pane shows
  "started by mcp:<client>", and `history` records it.

**S14: spike before M16 (about half a day):**

- `rmcp` maturity: Streamable HTTP server, resource subscriptions, progress
  notifications, structured content, and tool annotations.
- Claude Code as a client:
  - its MCP tool-call timeout, and whether progress notifications extend it;
  - its output-size limit (`MAX_MCP_OUTPUT_TOKENS`) and what truncation
    looks like;
  - whether it uses resource subscriptions at all.
- Codex as a client: stdio and HTTP.
- From inside a wisp VM: can a process reach an HTTP endpoint on the host
  (the bridge address, given `wisp-netd`'s restricted set), or does the
  bridge need to run host-side?
- Does `claude-agent-acp` pass `mcpServers` of type `http` as well as
  `stdio`?

**Done when:**

- Claude Code outside illogical, with `illogical mcp`, runs a long build in a
  VM pane. You watch it on the phone, Claude waits through it, reads the
  failure, fixes it and reruns, and the pane shows "started by
  mcp:claude-code".
- An agent block starts its project's dev server in a pane in its own tab
  and opens it in a browser block beside itself. A try to touch another tab
  is refused.
- One agent starts a second in an agent block, waits until it asks a
  question, and answers it.
- "What failed in this repo yesterday?" is answered through `history`.
- A client on another tailnet machine uses `/mcp` over HTTP with a token,
  and revoking the token cuts it off.

### After M6: order and triggers

Everything below is planned, but each item starts when its trigger holds, not
on a date. The suggested order:

1. **M6c** (S13 is done), because agent questions are a daily papercut now that
   agent blocks exist.
2. **S14 then M16 (MCP server)**, because it turns everything built so far
   into tools any agent can use, and it is mostly a layer over M3's API.
3. **M7**, because M11 needs its filesystem method, and the picker and session
   names are cheap.
4. **S8**, to choose block types from real use of M6.
5. **M10 / M11.**
6. **M5** when a tmux client is wanted.
7. **M8** when ghostty-web is ready.
8. **M9** when the scale numbers say so.

### M5: tmux control mode (`-CC`) front end

Lets iTerm2, and anything else that speaks tmux control mode, attach to
illogicald and show its tabs and splits as native windows. It is independent
of M4 and M6. The protocol was shaped for this from the start (the M5 rule
under Protocol).

- **Done 2026-10-01** (spike S11 first), except the check on a real iTerm2:
  `illogical tmux -CC` in `crates/cli/src/tmux/`; the daemon changes S11
  asked for (a minimum tab size, the option store, vt accessors, `cwd` on
  split and new tab; plus Ping/Pong). `crates/daemon/tests/tmux.rs` replays
  iTerm2's sequence and matches tmux 3.6's replies, and covers WezTerm's and
  Ghostty's sequences, formats against real tmux, `%pause` and blocks;
  `web/e2e/tmux.spec.ts` edits one layout from both sides. The manual iTerm2
  script is in README (*Use it*).
- **S11: done 2026-10-01.** See [spikes/s11-tmux-cc](spikes/s11-tmux-cc/README.md).
  - **Method:** iTerm2's command sequence taken from its source, replayed
    verbatim against tmux 3.6. HTM's code and tests read.
  - **Layouts:** `tmux_layout.py` converts an illogical split tree to and
    from a tmux layout string with its checksum. It round-trips the
    transcript's layouts byte for byte, and 3,000 random trees exactly.
  - **No redesign needed.** M5 fits the current protocol with the daemon
    changes below; everything else lives in the front end.
- **Entry point:** `illogical tmux -CC [attach -t $s]`, which runs over ssh or
  locally. It pretends to be tmux on stdio (`\033P1000p`, then an empty
  `%begin`/`%end`, then `%session-changed`) and talks to the daemon over its
  socket.
  - It reports tmux version `3.5a`, as HTM does and as the Ghostty and WezTerm
    branches were tested against.
  - It accepts `\r` line endings and a leading `^C`.
- **Mapping:**
  - session to session, tab to window, leaf block to `%pane`;
  - layout changes become `%layout-change`, with cells derived from stored
    ratios through the S11 converter. A drag in iTerm2 becomes weights that
    reproduce tmux's cells exactly.
  - output becomes `%output` (octal-escaped) or `%extended-output`.
- **The minimum command set** (S11 has the exact formats):
  - a real `-F` format expander (variables, `#{?c,a,b}`, `#{@opt}`);
  - `list-sessions`, `list-windows`, `list-panes` (including iTerm2's
    21-field state format), `display -p`;
  - `capture-pane -peqJN`, `-a` and `-P -C`;
  - `refresh-client -C W,H`, `-C @W:WxH`, `-f` and `-A`;
  - `split-window`, `new-window -PF`, `kill-pane`, `kill-window`;
  - `resize-pane -L/-R/-U/-D n` and `-x/-y`;
  - `send` in its `-lt`, `0xNN` and `-H` forms;
  - `select-pane`, `select-window`, `rename-window`, `detach`;
  - `show`/`set` for `@` options;
  - canned success for built-in options (`aggressive-resize off`,
    `status off`, …), `list-keys` and `copy-mode -q`.
  - **Never `%error` a command iTerm2 doesn't expect to fail** (`list-keys`,
    `show @iterm2_id`, `resize-pane`, `select-layout`). iTerm2 disconnects
    with an alert. A `select-layout` that can't be expressed replies success
    and re-sends the unchanged layout.
- **Notifications:** `%output`, `%extended-output`, `%layout-change`,
  `%window-add`, `%window-close`, `%window-renamed`, `%window-pane-changed`,
  `%session-window-changed`, `%sessions-changed`, `%pause`, `%continue` and
  `%exit`. They are held until after the current `%end`.
- **Daemon changes M5 needs (from S11):**
  1. **A minimum tab size** (one cell per pane plus dividers), clamped in
     `core` as tmux does. tmux refuses a layout whose tab is smaller than its
     tree, and Ghostty checks sizes.
  2. **An option store:** an opaque string map per session, per pane and
     global, saved with the layout. iTerm2 keeps tab grouping, hidden tabs
     and its duplicate-attach guard in `@` options, and Ghostty and WezTerm
     use `@affinities`.
  3. **libghostty-vt accessors** for the cursor, the alt screen's saved
     cursor, the scroll region and tab stops. The front end keeps a mirror
     terminal per pane, fed from an attach snapshot, and answers
     `capture-pane` and pane state from it. Output then streams from the
     snapshot's offset, so it lines up with what tmux would have sent.
  4. **A `cwd` option on split and new-tab** intents, for iTerm2's
     custom-directory profiles.
- **Front-end-only rules:**
  - **Size claims.** `refresh-client -C @W` and typing in a pane count as a
    size claim, which fits the existing per-tab owner.
  - **The active pane and tab** are tracked per front-end connection. WezTerm
    hangs after a split without `%window-pane-changed`.
  - **Flow control.** A daemon `Resync` becomes `%pause`. iTerm2 then
    re-captures and sends `continue`, which re-attaches. Never turn on
    `pause-after` unless the client asks, because WezTerm can't parse
    `%extended-output`.
  - **`send -H`** means bytes to tmux and WezTerm, but Unicode code points
    to Ghostty's branch.
  - Don't copy HTM's argument parser: it drops values starting with `-`, so
    `capture-pane -S -1000` returns only the visible screen.
  - **Client quirks, from reading WezTerm's and Ghostty's source (S11):**
    - **WezTerm:**
      - `list-commands` must list `resize-window`, or WezTerm never
        resizes;
      - it needs exactly 8 fields from `list-windows` and 11 from
        `list-panes`, and a window name with a space breaks it, so names go
        out without spaces;
      - it hangs after a split until `%window-pane-changed @W %new`
        arrives;
      - any unknown or blank `%` line ends control mode.
    - **Ghostty:**
      - it needs 5 or 6 tab-separated fields from `list-windows` and 25 or
        26 `;`-separated fields from `list-panes`;
      - `#{version}` must be a single token;
      - an `%error` on `list-windows`, `list-panes` or the version ends the
        session.
    - **`%layout-change`** uses the four-field form, keeping the trailing
      space when flags are empty, with a lowercase checksum that changes
      whenever the layout does.
    - **Window close:** send `%window-close` for windows in the attached
      session, as real tmux does, not HTM's `%unlinked-window-close`.
    - **An empty input line means detach.** No line or block may exceed
      1 MiB.
    - **Real traffic:** HTM has no recorded transcripts. The quickest source
      is htmd's `control command:` log while a GUI client is attached.
- **Still needs a real iTerm2:**
  - a logging-proxy capture, to check pipelining and what it sends after
    windows open;
  - what it does when another client owns the size (resize, letterbox or
    loop);
  - whether `3.5a` is the best version to report;
  - silent resync for clients without `pause-after`;
  - whether tab grouping comes back across reattach once the option store
    exists.

  Ghostty's and WezTerm's upstream `main` aren't usable yet. MisterTea's
  unmerged branches work, and Ghostty can be built here (see
  `~/ghostty-tmux-control-mode-brief.md`).
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

**Done 2026-10-01.** See the README's *Files and navigation*; the fs
methods and their scope are documented in `crates/daemon/src/fs.rs`.

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

- **S10 (2026-10-01): the trigger is not met.** See [spikes/s10-ghostty-web](spikes/s10-ghostty-web/README.md).
  - **ghostty-web (coder/ghostty-web 0.4.0) looks stalled.** It embeds a
    Ghostty from December 2025, has no snapshot API, and its upgrade PR is a
    work in progress.
  - **Fidelity:** it matched the daemon on 6 of 7 fixtures in Chromium and
    WebKit, including emulated phones. On the seventh, it turns grapheme
    clustering (mode 2027) on by default and the daemon has it off, so emoji
    with modifiers take a different width. It also ignores OSC 4 palette
    changes. Its `scrollback` option is in bytes.
  - **Blockers:**
    - a new terminal shows stale cells from a disposed one;
    - `write('')` throws, and any render error stops rendering for good;
    - it answers terminal queries itself, which would double the daemon's
      replies;
    - no parser hooks (needed for OSC 133/633 marks and to swallow queries),
      no markers or decorations, no `modes`/`onBinary`, no WebGL, and no
      buffer API that keeps blanks and graphemes;
    - IME is broken for CJK and Korean, and there's no screen-reader support.
  - **Upstream's own wasm** (the VT core only, no renderer) builds at the
    daemon's Ghostty commit (262 KB gzipped). It decodes every fixture's
    GHOSTSNP to text identical to the daemon's. For 64k rows it's ready in
    5–20 ms, with the history in a further 70–300 ms. **That is the more
    promising route:** our own renderer, or a maintained ghostty-web, over
    upstream's wasm.
  - Re-check when ghostty-web ships a current Ghostty, or when a renderer
    over upstream wasm exists.
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
- **S9: measured 2026-10-01; the trigger already holds, but parking isn't the
  first fix.** See [spikes/s9-memory](spikes/s9-memory/README.md).
  - **Idle cost:** an idle, empty pane costs 3.1–3.3 MB of daemon RSS (50
    panes: 172 MB; 500: 1.6 GB, with 5 threads per pane). Shims and bash add
    about 1.7 MB more, so 500 idle shells cost about 2.5 GB in all.
  - **Scrollback** costs about 1.7 KB per row at 200 columns: 10k lines is
    33 MB per pane, and the 64 MiB cap is about 160 MB.
  - **Memory isn't given back after panes close.** glibc keeps it: 500 panes
    closed down to 1 still hold 191 MB.
- **Step 1: cheap wins, before any parking.** Then rerun the S9 benchmark.
  - **Drop Zig's 256 KiB per-thread signal stack in libghostty**, which glibc
    gives every thread: 1.3 MB per pane. It's a one-line patch
    (`ghostty-no-signal-stack.patch`) to carry and send upstream.
  - **Avoid libghostty's ReleaseSafe page fill:** 1.5 MB per screen.
    ReleaseFast plus the patch measured 0.48 MB per idle pane, which already
    meets M9's done bar. But ReleaseFast drops safety checks on untrusted
    program output, so it's a decision (open); an upstream fix that avoids
    the fill would remove the trade.
  - **Fix the malloc mmap threshold** (`mallopt` at startup, or another
    allocator) and call `malloc_trim` after a pane closes. That saves about
    13 MB per pane at 10k lines, and memory comes back.
  - **The output ring holds 4 MiB, not 2 MiB,** because `Ring::push` extends
    before draining. Set it to 1 MiB (replay never uses more than
    `MAX_REPLAY_BYTES`) and drain first.
  - **Lower the in-memory scrollback cap** from 64 to 16 MiB (about 28 MB per
    pane). Full history is on disk.
  - **Fold the reaper thread into the wait thread,** and use a tiny shim
    binary instead of re-running the 21 MB daemon (about 0.3 GB at 500
    panes).
- **Step 2: parking, only if panes with real history still blow the budget.**
  S9 found nothing that argues for PTY or client-buffer parking. Parking
  can't save the shells and shims either (0.75–0.87 GB at 500 panes).
- **Stalled clients:** a client that stops reading cost about 28 MB in one
  burst, and up to about 64 MB per client from the 1,024-frame queue. Cap the
  queue in bytes, not frames.
- **CI benchmark:** S9's `bench.py` at 50 idle panes (3.07–3.11 MB per pane
  over four runs) is the regression test M9 wants. Leave headroom.
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
- M15 reaches people outside your tailnet. **Superseded (2026-10-01) by the
  control track's M19**, which does it through illogical control.
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
    (Moot since M15 was superseded; skip the Funnel part.)
  - What headers arrive, given there is no identity?
  - What are the rate limits?
- **Input attribution cost.** Add a per-input index record
  `(offset, principal, len)` to the M2 index. Measure index growth with the
  full typing of a day of use, and with `paste` of 1MB.
- **"From now" sharing.** Can a viewer's first snapshot be taken without
  scrollback (screen only, via GHOSTSNP partial encode or the formatter), so
  history before the share point never leaves the daemon?

#### M12: principals and roles

**Done 2026-10-02.**

- **What landed:**
  - `illogical_core::access::need` is the one decision function;
  - the daemon's `acl.rs` stores grants and the audit log, and `authz.rs` is the API half;
  - the mux checks every message and filters each client's state;
  - `illogical access`;
  - `e2e/access.spec.ts` covers the done-when.
- **Deferred to M13:** per-user push, and recording who approved an agent. Non-owners can't subscribe to push yet.

Every request has an author, and every session has an access list.

- **Principals:**
  - **user:** a tailnet login, or an illogical control account (M17);
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

**Done 2026-10-02.**

- **What landed:**
  - presence (avatars in the bar, dots on tabs, focus outlines and names on panes) and following someone;
  - driving: the first to type drives; others are held back with the reason; take control, ask for it, hand over, or pair mode;
  - attribution in the pane index, `illogical log %N [--who]`;
  - the Share dialog;
  - "from now" shares.
  - `e2e/presence.spec.ts` covers the done-when.
- **S12, answered:**
  - **Input attribution** is one index record per handoff (`Driver { who }`) plus `by` on each command, not one record per input, so the index grows with handoffs rather than keystrokes.
  - **"From now"** is a screen-only snapshot: the full snapshot is replayed into a scratch terminal, `ED 3` drops its scrollback, and that is snapshotted again (`VtEngine::screen_snapshot`). Its API (tail, capture and export) is refused below the share point.
  - **Node sharing (a second real tailnet)** wasn't tried. The tests stand in with `Tailscale-User-Login` on loopback, as `tailscale serve` adds it.
- **Known gaps:**
  - A pane moved into a "from now" session after the share has no recorded floor, so its earlier history shows.
  - Per-user push still waits.

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

**Done 2026-10-02.**

- **What landed:**
  - A guest's new tab is a VM tab. Their split gets its own VM, or joins the tab's.
  - VMs record who they're `by`, and a quota (`--guest-machines`, default 3) counts them.
  - Trust grants for panes on the owner's machine: a guest asks, and the owner gets a prompt and a Web Push, then allows 10 minutes to 2 hours. A grant ends by itself and is checked on the WebSocket and the API.
  - Private panes.
  - A token-shape scan (`/api/sessions/{id}/secrets`) behind a warning in the Share dialog.
  - An editor's agents always run on a VM of theirs.
  - `e2e/guests.spec.ts` covers the done-when with real wisp VMs.
- **Not covered:**
  - Running `claude` in the guest's VM; the tests run a shell command, and Claude's credentials in VMs are M3b's.
  - Answering from the phone notification itself. The push is sent; the tests answer in the page.
  - CPU and memory quotas: VM size is wisp's.

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

**Superseded (2026-10-01) by M19 in the control track:** invites, sign-in
and read-only links move to illogical control. Kept for the record.

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

### Control track (S15, M17–M22, added 2026-10-01)

The daemon is the WireGuard: a useful piece of technology for one person
on their own network. This track is the Tailscale: a central service that
makes it work for people who have never heard of a tailnet, and for teams.

The code already draws the line. M4's **home daemon** is "a directory and
control point, never a relay": it holds the host list and provider tokens,
mints per-host tokens, and receives dial-out connections and log sync.
That is a coordination server running on geek. This track moves that role
into its own program, **illogical control** (`illogical-control`), which
anyone can run and which we also host. Then it adds what a single home box
can't do: accounts, reaching machines behind NAT, teams, push and hosted
compute.

**Decisions (2026-10-01):**

| Question | Decision | Why |
|---|---|---|
| Who it's for first | **Small teams sharing sessions.** A few people pairing and watching each other's agents. | Sharing is where a central service adds the most. It needs accounts and a relay anyway, so solo use comes along for free. |
| Can terminal content pass through the service in the clear? | **Never.** Output, input, scrollback, snapshots, history and push payloads are end-to-end encrypted. The service sees metadata only (who, which host, when, sizes). | It's shell access. A blanket promise is the trust story, and E2E can't be retrofitted. |
| Self-hosting | **From day one.** `illogical-control` is open source, in this repo, and the hosted one runs the same code. | Keeps the promise checkable. The business is hosting, compute and teams, not lock-in. |
| Pricing | **Free for one person; per seat for teams; sandboxes by usage.** Relay traffic is included, with fair-use caps. | Tailscale's shape: strangers try it free, teams pay for what teams need, compute costs what it costs. Self-hosted control has no billing. |
| Whose machines a team's sessions run on | **Members' own daemons, team-owned daemons, and hosted VMs.** A team can enroll shared machines (a build box, a staging server) that belong to the team, not a person. | Teams have shared machines. M14's rule still holds: guests type in VMs unless trusted. |
| M15 (Funnel invites, per-daemon GitHub sign-in, read-only links) | **Superseded by M19.** Invites, sign-in and read-only links move to control. | M15 solves per daemon what control solves once. M12–M14 carry over: they're the per-daemon enforcement control relies on. |
| The tailnet | **Still a first-class path.** Tailnet users can skip control entirely, or enroll and keep direct tailnet connections. | Control adds; it doesn't replace. |

**What control knows and doesn't:**

- **Knows (metadata):**
  - accounts, teams, members and roles;
  - devices and daemons, and their public keys;
  - the directory: hosts, sessions, tab and pane ids, names, presence;
  - who connected to what and when, and byte counts;
  - audit entries.
- **Never has:**
  - terminal bytes, snapshots or logs in the clear;
  - keys that decrypt them;
  - provider tokens for your own machines (those stay on your daemons).
- **Names are metadata.** Session and tab names, and the pane titles shown in the directory, are visible to control. The README says so, and a per-team switch keeps names on daemons only (then the directory shows ids).

**Trust.** The service distributes public keys, so a malicious control server could add a device of its own and read what it's sent. As with Tailscale's Tailnet Lock, **a new device must be approved by one of the user's existing devices** (the first is trusted on enrollment). Daemons only encrypt to devices carrying that approval, and team membership changes are signed by a team owner's device. Control can refuse service, but it can't read.

**Order:**

1. **S15**, then M17 and M18: accounts, directory, relay and E2E.
2. **M19, teams.** It builds on M12 and M13; strangers can use illogical together from here.
3. **M20, sandboxes**, and **M21, push**, in either order.
4. **M22, billing**, when there's something to charge for.

The launch issues (#19–#27: licence, releases, install, quickstart) come first: control is worth little if strangers can't install the daemon.

**Later, not planned yet:**

- encrypted history in control, with retention and cross-machine search run on clients;
- the hosted MCP endpoint (M16 over the relay, with scoped tokens);
- SSO/SCIM and policy;
- a native mobile app.

#### S15: spike before M17 (about two days)

- **E2E design**, written up as a short spec:
  - **Pairwise channels:** Noise (IK or XX) between a client device and a daemon, inside the relay's WebSocket. Measure overhead on attach and on a 1 MB burst.
  - **Shared sessions:** the daemon encrypts a session's stream once, to a session key wrapped for each member device. On a revoke, rotate the key. Compare with plain per-viewer channels at 2, 5 and 20 viewers.
  - **Device keys:**
    - **Browser:** WebCrypto non-extractable keys in IndexedDB, plus passkeys (the WebAuthn PRF extension) to re-derive them. Does PRF work in Safari on iOS and in Chrome on Android?
    - **CLI:** a key file.
    - **Phone PWA:** what happens when the browser clears its storage.
  - **Device approval:** the flow from an existing device; recovery codes for losing all of them.
  - **Push:** Web Push payloads are already encrypted to the subscription (RFC 8291). Confirm control can forward them without seeing contents.
- **Relay:**
  - prototype on the M4c dial-out transport with control in the middle;
  - round trip from a phone on cellular, via control, to a home machine;
  - how many concurrent streams per daemon;
  - what a hosted relay costs per active user-hour.
- **Identity:**
  - GitHub and Google OAuth, and passkeys as a first-class login;
  - email magic links for invitees without either.
- **Read-only links with no account:** the key travels in the URL fragment (`#k=…`), which browsers never send to the server. Check that this works through the service worker and with link previews (Slack's unfurler must not fetch the fragment).

**Output:** `docs/control-e2e.md` (the spec) and a go/no-go on PRF for browser keys.

**Done 2026-10-01, apart from the phone runs** (see [spikes/s15-control](spikes/s15-control/README.md) and [docs/control-e2e.md](docs/control-e2e.md)):

- **Channels:** `Noise_IK_25519_AESGCM_SHA256` everywhere.
  - The browser side runs on WebCrypto alone and interoperates with `snow`.
  - Costs: 96+48 handshake bytes, 0.1% overhead on bulk output.
  - Message 1's payload is replayable, so it carries only `hello` and `attach`.
- **Shared sessions:** per-viewer channels, not a session key.
  - A session key only saves the daemon's uplink, and only with relay fan-out; that's deferred until a measured need.
  - Revoking someone means closing their channel.
- **Device keys:** non-extractable X25519 (Noise) and Ed25519 (approvals) in IndexedDB; certificates are signed by an approving device, and daemons verify the chain.
  - **PRF is an improvement, not a requirement:** without it, a browser that lost its storage is approved again as a new device.
- **Relay:** M4c's mux with control at the home end.
  - It adds about one relay↔daemon round trip (18.7 ms geek→ewr→geek, 51.6 ms via ord), so the relay runs in the daemon's nearest region (multi-region on Fly with `fly-replay`).
  - 1,000 channels in one process, at about $0.0001–0.001 per active user-hour.
- **Found:**
  - Nagle added 40 ms to every dial-out round trip; fixed in the daemon (41.7 ms → 2.0 ms).
  - Chrome's Local Network Access blocks a public page (control's) from tailnet addresses until the user grants permission.
- **Read-only links** carry a one-off device key in the fragment, and the daemon holds it as a link principal.
- **Push:** the daemon encrypts (RFC 8291) to a subscription the device signed, and control only adds VAPID.
- **Pending on a phone:** PRF on iOS Safari and Android Chrome, the cellular round trip, and Slack/iMessage previews.

#### M17: illogical control (accounts, devices, enrollment, directory)

**Done 2026-10-01 (see [docs/control.md](docs/control.md)), with M18's relay.** Hosted at <https://control.illogical.widgets.wtf> (Fly, `packaging/control/`).

- **What landed:**
  - `crates/control`, with GitHub sign-in and passkeys (verified in-house: no OpenSSL in static builds);
  - devices and approvals with fingerprints, recovery codes, and `illogicald join` / `leave`;
  - the directory, rate limits on everything that needs no sign-in, and the web client's control mode.
- **How it was tested:**
  - `e2e/control.spec.ts`: a stranger signs up, two machines join, one direct and one relayed, and a phone needs approval;
  - `e2e/passkey.spec.ts`;
  - `just control-smoke`;
  - by hand against the hosted control: a passkey sign-up, a daemon on geek joined, and a shell through Fly's relay.
- **Changed from the plan:**
  - Per-host tokens aren't minted by control. An enrolled daemon trusts device certificates instead, which is stronger and needs nothing minted.
  - The CLI has no device key yet: it still reaches the local daemon, and others over the tailnet.
- **Still to check by hand:**
  - real GitHub sign-in (needs the GitHub App's credentials as Fly secrets);
  - a Mac (jake-mini) joining.

- **`crates/control`, the `illogical-control` binary:** axum, SQLite (Postgres optional for the hosted one), and the same release builds as the daemon. `illogical-control --domain control.example.com` serves the API and the web client.
- **Accounts:**
  - sign in with GitHub, Google or a passkey;
  - a personal space by default; teams come in M19.
- **Devices:**
  - each browser, phone and CLI gets a device key at sign-in;
  - the first device is trusted on enrollment;
  - every later one shows "approve this device?" on an existing device, with a fingerprint to compare (the trust rule above);
  - recovery codes.
- **Daemons:**
  - `illogicald join https://control.example.com` prints a code; approving it on a device enrolls the daemon to your account;
  - a daemon has its own key;
  - the M4 per-host tokens are minted by control from now on;
  - `illogicald leave` removes it.
- **Directory:**
  - control keeps the host list: your daemons, last seen, and how to reach each one (direct URL or relay);
  - it replaces the home daemon's `hosts.rs` list for enrolled daemons;
  - the page fetches the list from control and caches it, so known hosts stay reachable while control is down (the same rule as today);
  - geek stops being special.
- **Daemon auth:**
  - the daemon accepts a client that presents an approved device key for an account with access (personal: only you);
  - tailnet identity still works for tailnet users;
  - enforcement stays on the daemon, using M12's principals once they exist.
- **The web client:**
  - served by control (and still by each daemon);
  - signs in, shows the directory, and connects straight to daemons over the tailnet or the relay (M18).
- **Self-hosting:** documented in the README with a single binary and Caddy or `tailscale serve` in front. No feature is hosted-only except billing (M22).
- **Done when:**
  - a stranger with a Mac and a Linux box, and no Tailscale, signs up with GitHub and enrolls both daemons;
  - the page lists both;
  - adding a phone needs approval from the laptop;
  - a self-hosted control on a VPS does the same.

#### M18: relay and end-to-end encryption

**Done 2026-10-01, apart from the phone run.**

- **What landed:**
  - the relay (M4c's mux in control);
  - Noise channels on both paths in control mode;
  - "direct" or "relayed" in the host chip;
  - per-account relay bytes per day;
  - trust changes pushed to daemons over the relay socket, so approvals and removals take effect within seconds.
- **Done-when results:**
  - `just control-smoke` shows control's wire traffic, database and logs never contain what was typed.
  - Through the hosted relay (ewr) from geek, the keystroke echo was 30 ms p50, measured in the browser.
  - **Still to run:** a phone on cellular against a Mac behind NAT.
- **Not changed:** a page served by a daemon itself still uses plain `/ws`, inside the tailnet's WireGuard. Only control mode uses Noise.

- **Relay:**
  - an enrolled daemon keeps the M4c dial-out connection open to control;
  - clients that can't reach a daemon directly (no tailnet, NAT both sides) connect to control, which splices the client's stream onto that daemon's connection;
  - control forwards opaque frames, multiplexed as dial-out already is.
- **Direct when possible:**
  - the client tries in order: tailnet or LAN URLs from the directory, then the relay;
  - a page served by control asks for Chrome's local network access permission before trying a tailnet URL (S15), and uses the relay until it's granted;
  - the relay runs in the region nearest each daemon (S15), multi-region on Fly with `fly-replay` to the machine holding the daemon's socket;
  - the host chip shows which path is in use ("direct" or "relayed").
  - Hole punching (WebRTC data channels, for example) is a later optimisation, not this milestone.
- **E2E per S15:**
  - every client–daemon stream is encrypted end to end, on the relay *and* on direct paths, so there's one code path;
  - snapshots, output, input, method calls and events all go inside it;
  - control sees connection metadata and byte counts.
- **Fair use:** per-account relay byte counters, exposed to M22. Limits are configurable; self-hosted has none by default.
- **Done when:**
  - from a phone on cellular, attaching to a Mac behind home NAT through control draws vim correctly, and typing feels the same as on the tailnet (within 30 ms of the direct path);
  - a packet capture on control shows no terminal content;
  - control's database and logs contain no terminal content;
  - switching the phone to the tailnet moves it to the direct path on reconnect.

#### M19: teams (sharing, roles, team daemons, invites)

**Done 2026-10-02.**

- **What landed:**
  - signed team rosters (`illogical_e2e::team`): each version is signed by an owner's device of the version before, and a team daemon pins the founder at `illogicald join --team`;
  - teams in control: invites, join requests that an owner admits by signing the next roster, roles, removal, and the lock;
  - team daemons, whose members drive them by team role: the box is the team's, so no personal trust grant is needed;
  - sharing a session with a person on control, with their root pinned in the grant;
  - read-only links: a one-off X25519 key in the fragment, held by the daemon as a "from now" viewer until it expires, with an anonymous relay route only while links are live.
  - `e2e/teams.spec.ts` covers the done-when.
- **Trust on first use:** each browser pins other accounts' roots, and team founders, the first time it sees them, and a fingerprint is shown to compare. Control could lie at that first sight, the same limit as Tailnet Lock's first sign-in.
- **Not covered by tests:** sharing a single session with a person outside a team (it's built, but not exercised end to end), and `illogical team lock` from the CLI (the lock is in the Teams panel).


Builds on M12 (principals and roles on each daemon) and M13 (presence, driving, attribution). Control becomes where principals come from; daemons still enforce.

- **Teams:**
  - create a team, invite by email or link, and set roles (owner, editor, viewer);
  - membership changes are signed by an owner's device, and daemons verify them (the trust rule);
  - an owner can transfer ownership.
- **Team daemons:**
  - `illogicald join --team acme` enrolls a machine that belongs to the team;
  - who may drive it is a team policy, with M14's trust grants for anything but VMs.
- **Sharing a session (M13's dialog):**
  - pick a person or the whole team, set a role, choose "with history" or "from now";
  - each member device gets its own channel (S15: per-viewer channels, no session key).
  - **Revoking** removes the grant and closes that person's channels, cutting them off within a second.
- **Presence** (avatars, focus outlines, follow) flows through control as metadata, so people on different networks see each other.
- **Read-only links** (replacing M4c's share tokens and M15's links):
  - a link with the key in its fragment shows one session live, read-only, with no account, until it expires;
  - "from now" by default;
  - control sees that the link was opened, never what it showed.
- **Guests** (people outside the team) work as M14 says: their panes default to a VM (M20 when the team has hosted compute; otherwise a member's wisp).
- **Kill switch:** an owner's `illogical team lock` revokes all links and invites and disconnects non-owners.
- **Done when:**
  - two people at different companies, neither on a tailnet, join a team by invite;
  - each sees the other's session live, with avatars and focus;
  - control passes back and forth between them;
  - one runs a build on a team-owned box;
  - a read-only link works in a logged-out browser and dies at expiry;
  - removing a member cuts them off within a second.

#### M20: hosted sandboxes

**Done 2026-10-02, against wisp. Hosted on real Sprites once control has a token.**

- **How a sandbox comes up:**
  - Control makes a sprite (`crates/control/src/sprites.rs`), puts the static daemon in it, and runs a service.
  - The service writes a join request with a key made in the sandbox; control fetches it through the provider.
  - The browser that asked approves it by itself (the code is recomputed from the key), and control writes `control.json` back.
- **How it's reached:** through the provider's proxy to `/e2e`. The sandbox never dials in, so it sleeps when idle.
- **How it ends:** closing its last tab deletes it (the daemon tells control, and the page does too).
- **Who may make one:** only allowlisted accounts (`--sandbox-accounts`), each up to a quota (`--sandbox-quota`, default 2).
- **Metering:** sandbox minutes, from creation to deletion.
- `e2e/sandboxes.spec.ts` covers the done-when on wisp, apart from running `claude`, which needs credentials in the VM.
- **To go live:**
  - a Sprites token as `SPRITES_TOKEN` on the Fly app;
  - the static daemon in the control image (`/illogicald`);
  - your account id in `ILLOGICAL_SANDBOX_ACCOUNTS`.


"New VM tab" with no wisp on your own machine: the VM runs on hosted compute, billed by the minute.

- **Providers:**
  - control holds provider credentials for hosted compute and creates machines through the M4b `Provider` trait (Sprites, Fly, or our own Firecracker hosts running wisp);
  - your own provider tokens stay on your daemons, as today.
- **Each sandbox runs a daemon** enrolled to the team (M17's join, done automatically) and reached over the relay or direct.
  - Its terminals are E2E like any daemon's; control creates the machine but holds no keys to its terminals.
  - The sandbox's host key is approved automatically by the requesting device's approval, so no extra click is needed.
- **Lifecycle:**
  - a VM tab's machine lives as long as the tab (M3c's rule);
  - idle machines sleep (M4b's wake rules);
  - quotas per team and per member (M14's quotas, enforced by control).
- **Metering:** sandbox minutes and storage per team, exposed to M22.
- **Done when:**
  - a team member with only a browser opens a VM tab, runs `claude` in it, splits a second shell into the same VM, closes the tab, and the machine is deleted;
  - the minutes show up in the team's usage;
  - a fifth VM over quota is refused with a clear message.

#### M21: push relay

**Done 2026-10-02.**

- **What landed:**
  - Control holds one VAPID key pair.
  - Devices subscribe once, with a subscription signed by their device key (`illogical_e2e::push`), so control can't substitute keys.
  - Daemons verify each subscription against devices they trust and encrypt per subscription (RFC 8291), sending to the owner and to editors of the pane's session.
  - Control adds VAPID and posts, only to the browsers' push services.
- **Tested in `just control-smoke`:** a subscription with swapped keys is refused; a "needs you" reaches a fake push service, and only the phone's key decrypts it; control's logs show "push relayed", never the text.
- **Not covered:** approve and answer actions on notifications through control, which need the daemon's own page. Those notifications open the pane instead.


- **Today:** each home daemon holds VAPID keys, and each phone subscribes to each daemon.
- **With control:**
  - control holds one VAPID key pair;
  - devices subscribe once;
  - daemons send notifications through control, addressed to the user's devices.
- **Payloads are encrypted** for each device's push subscription (RFC 8291), so control and the browser's push service see neither the title nor the body. Control sees only "daemon X notified user Y".
- **Per-user rules** come from M12: `needs-input` goes to editors who opted in, and approvals go to whoever is asked.
- **Done when:**
  - a phone that never connected to a Mac gets "needs input" from an agent there, through control;
  - tapping it opens that pane over the relay;
  - control's logs show no notification text.

#### M22: billing and metering (hosted control only)

**Done 2026-10-02, against a fake Stripe. Real test-mode keys come later (decided 2026-10-02).**

- **What landed:**
  - Stripe Checkout for a personal plan (hosted VM minutes) or a team (per seat, plus minutes);
  - signed webhooks (checked against `STRIPE_WEBHOOK_SECRET`, within five minutes);
  - seat counts that follow the signed roster;
  - hourly meter events for sandbox minutes;
  - free accounts get `--relay-free-mb` a month: a warning over it, and relayed traffic slowed down past twice it;
  - with billing on, hosted VMs need a paid plan, and running ones are never stopped;
  - the Plan and usage panel.
- **Tested in `just control-smoke`:** the relay warning; Checkout; an unsigned webhook refused; the upgrade; a seat added when a member joins; minutes reported; the invoice arithmetic.
- **Self-hosted control has none of it** unless `STRIPE_SECRET_KEY` is set.
- **To go live:**
  - Stripe test-mode keys, prices and a meter as Fly secrets: `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET`, `ILLOGICAL_STRIPE_SEAT_PRICE`, `ILLOGICAL_STRIPE_MINUTES_PRICE`;
  - the webhook endpoint `https://control.illogical.widgets.wtf/api/stripe/webhook` registered at Stripe.


- **Plans:**
  - **Personal:** free; one person, any number of daemons, relay with fair-use caps.
  - **Team:** per seat.
  - **Sandboxes:** by usage (minutes and storage) on any plan with a payment method.
- **Counters:** M18's relay bytes, M19's seats and M20's sandbox minutes, in control's database. A usage page per team.
- **Billing:** Stripe, with webhooks into control. Over-limit behaviour:
  - relay: a warning, then a slowdown;
  - sandboxes: refuse new machines; never kill running ones.
- **Off by default:** a self-hosted control has no billing unless configured.
- **Done when:** a team upgrades, adds a seat, uses sandbox minutes and gets a correct invoice, and a free account over its relay cap sees the warning.

### Swarm track (S16–S18, M23–M30, added 2026-10-02)

One live view of every pane on every machine you (or your team) can see. Panes form clusters on their own, by project, machine, kind or person, and anything that needs you lifts out to a "needs you" rail. There, anyone on the team who may answer can answer, and send the agent its next instruction. The issues hold the detail: the MVP is #44.

**Order:**

1. **S16** (#36) and **S18** (#45): done, below.
2. **M23** (#37: pane summaries) and **M24** (#38: attention reasons and actions), side by side with **M29** (#46: team answers).
3. **M25** (#39: every host in one page), then **M30** (#47: the team's swarm).
4. **M26** (#40: the swarm view).
5. **After the MVP:** editors, with **S17** (#41), **M27** (#42: VS Code blocks) and **M28** (#43: your editor in the swarm).

#### S16: swarm spike (summary cost, fleet connections, canvas)

**Done 2026-10-02, apart from the phone runs** (see [spikes/s16-swarm](spikes/s16-swarm/README.md)).

- **Delta summaries: go.** At 500 panes with 50 busy, today's whole-`State` broadcasts are 34 a second at 195 KB each: 6.6 MB/s to every client, and 15–20% of a core. Field-level deltas once a second carry the same changes plus activity in 4.6 KB/s (0.44 KB/s compressed).
  - So M23's deltas become the normal path for every client, not only the swarm.
- **Activity:** a cumulative byte counter and `last_output_ms` kept under the lock `State::output` already takes. The mux turns them into a rate, and nothing wakes parked panes. M9's PTY parking must keep the counter.
- **Previews: go.** Capturing 50 panes a second costs 0.8% of a core and 2.2 KB/s.
- **Canvas 2D: go up to about 2,000 panes.**
  - Laptop: 2,000 panes at 60 fps, 5,000 at 51 fps.
  - Phone proxy (4x CPU throttling): 500 at 60 fps, 2,000 at 36 fps.
  - Physics is half of each frame, so it sleeps once clusters settle.
- **Fleet:** 20 daemons (600 panes) add about 14 MB to a page. But Chrome spaces out WebSocket connections to one address past about 8, so 20 took 2–5 s to reconnect.
  - Relayed daemons share one socket to control, and direct reconnects are staggered.
- **Classification:** take `kind` from `/proc` argv before the typed text (52 of 54 right). Only 8 of 105 real commands ran inside a git repo, so "by project" needs a fallback group.
- **Pending:** a real phone, daemons on different addresses, a local control relay, and classification on real work.

#### S18: team answers spike (permission hooks, follow-ups, notification answers)

**Done 2026-10-02, apart from the phone runs** (see [spikes/s18-team-answers](spikes/s18-team-answers/README.md)). Claude Code 2.1.287.

- **Permission prompts: go, with the `PermissionRequest` hook.**
  - **When it fires:** only when a dialog is about to show, including in `acceptEdits`, after `--continue` and from subagents. Never in `bypassPermissions`.
  - **What it carries:** the tool, its input and Claude's own "always allow" suggestions (often the exact command).
  - **Answers that work:** allow, allow always and deny with a message.
  - **No `tool_use_id`:** a card is matched to the `PreToolUse` just before it. `AskUserQuestion` stays with M6c's hook.
- **A "Yes" in the terminal never reaches the hook,** so its later answer is dropped silently; only "No" and Esc send it SIGTERM. Cards close on their own, on any of these:
  - `PostToolUse` or `PostToolUseFailure`;
  - the session's next `PreToolUse`, `Stop` or `UserPromptSubmit`;
  - SIGTERM.
- **Follow-ups: go, through a hook, never by typing.**
  - **Typing fails:** typed text merged with the driver's half-typed draft and sent both.
  - **What works:** a `Stop` hook (plus one on `SessionStart`) with `asyncRewake` that exits 2 with the text. It wakes an idle Claude Code, leaves the draft alone, and delivers mid-turn at the next step.
  - **Rights:** who may send one is still the drive-rights rule (M13/M14).
- **Answering from a notification: go on desktop.**
  - **What worked:** a service worker loaded the non-extractable device key from IndexedDB, checked the chain, ran Noise and sent one approve in about 20 ms, against a local stand-in for the daemon.
  - **What it needs:** the directory and pins in IndexedDB (a worker can't read `localStorage`), approve and ask data in pushes sent through control, and `sw.js` as a bundled entry.
  - **Phones:** Android is likely fine, and iOS probably opens the card instead. Both are pending a real phone.

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
- **Hosted control is a target** (control track). It holds every user's directory, device keys (public) and who-connected-when. E2E keeps terminal content out of reach, and device approval keeps control from adding a reader. But metadata leaks (names, hosts, timing) and outages are real: keep the directory cached on clients, keep the tailnet path working without control, and keep control's own logs free of anything a daemon sends inside a stream.
- **Loopback trust.** Any local process can forge serve headers on 127.0.0.1. That is the same trust as the uid, and acceptable for single-user; require the `Host` header to match anyway.

## One-time setup (done 2026-10-01)

1. rustup (stable 1.98) in `~/.cargo`.
2. Zig 0.15.2 and 0.16.0 in `~/.local/opt`; `~/.local/bin/zig` points at 0.16. Builds of libghostty-vt need 0.15.2 first on PATH.
3. Neovim 0.12 in `~/.local/opt` (for fixtures).
4. `sudo tailscale set --operator=jake`.
