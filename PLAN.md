# illogical: plan

Written 2026-10-01 from the original brief and [docs/research.md](docs/research.md).
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
Superlogical's model. Inside a
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
  - `attach{panes: [{pane, offset, history?}], zstd?}`: `history` caps the scrollback in a snapshot at what the client keeps; with `zstd`, snapshots may come compressed (`snapshot_zstd`, frame kind 4). (#49)
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
  - Otherwise it sends a `snapshot` with at most `history` rows of scrollback, compressed for a client that asked, then live output.
  - **After a `resync`** (the client's queue filled), the client attaches again from its own offset with `history: 0`. A gap of up to 1MB replays; otherwise it gets the screen alone, keeps its own scrollback, and marks the gap with a dim `── output skipped here ──` rule. Asking for the whole history again is what kept a client behind a flood resyncing forever (S19). (#49, done 2026-10-02.)
  - After attach, the pane gets one SIGWINCH nudge.
  - **Visible-first attach is gated on measurement (decided 2026-10-01). Measured in S10: don't build it for xterm.js.**
    - **Where the time goes:** on an emulated Pixel 7 at 4x CPU throttling and 10 Mbps / 50 ms, attaching to a 64k-row pane took 3.7 s, of which 3.2 s was download. The snapshot goes out uncompressed (3.9 MB; 183 KB gzipped). The client keeps only 10k lines, so 54k of the 64k rows are downloaded and thrown away. Even a small screen takes about 150 ms to draw at 4x, which is the most visible-first could save. See [spikes/s10-ghostty-web](spikes/s10-ghostty-web/README.md).
    - **Do instead (small, server-side, fix now; done 2026-10-02 in #49):**
      - **compress snapshot frames:** zstd frames, because the tungstenite under axum 0.8 has no permessage-deflate. The web client decodes them with `fzstd`.
      - **cap the history in a snapshot at the client's scrollback** (sent in `attach`): through a formatter selection, so `screen_snapshot` no longer replays the whole snapshot into a scratch terminal.

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
  - **Built in #52 (2026-10-02).** A client that attaches with `acks` sends `ack{pane, offset}` from xterm's write callback, about every 64KB, and keeps no more than a 512KB window unacked.
  - **Holding back:** a client past its window, or whose queue is full, is held back rather than sent more. When it acks to within 256KB, it gets what it missed from the log, with nothing lost. If the log no longer has that (more than 1MB), it gets `resync`, and since #49 that brings the screen alone. A resize while it's held back also resyncs it, since the missed output was printed for the old size.
  - **Clients that don't ack** (`illogical attach`, the tmux front end, the share viewer) keep the old rule: a full queue means `resync`.
  - **The program is paused when the daemon falls behind.** It is never paused for a slow client; the log absorbs that output. But a pane's own program now writes into a bounded queue (64 chunks), so a flood runs only as fast as the pane takes it in, as in any terminal. What clients ask (attach, ack, keys) is served before that queue, so Ctrl-C stops a flood at once. Before #52, output piled up without limit in front of everything else: a debug daemon was still working through a flood 17 s after it ended, with every key and ack waiting behind it.
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
  - `https://geek.tail1234.ts.net` serves a valid cert.
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
  - **Fix now, in `crates/vt` (not tied to a milestone): done 2026-10-02
    (#1),** in `crates/vt/src/ghostty/wire.rs`: all 18 fixtures exact,
    0.8–1.8 ms a snapshot (0.03–0.7 ms unchanged). All seven fixes
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
  - **Alt-screen snapshots mid-sequence: fixed 2026-10-02 (#53).** Under
    an alt screen the wire path flips the live terminal to the primary
    with `CSI ?47l` and back. If the PTY stream had stopped inside an
    escape sequence or a UTF-8 character, the flip landed inside it, and
    the rest of the sequence printed as text for every client.
    - Decision: flip the live terminal only when its continuation is
      empty (the parser is at ground). Otherwise decode a GHOSTSNP copy
      with its scrollback, end its sequence with CAN, flip the copy and
      format the primary from it. If no copy can be made (the sequence is
      longer than the 1 MiB continuation limit), the snapshot has only the
      alt screen; the next one after the sequence ends has it all.
    - Why not always copy: a full GHOSTSNP round trip of up to 64 MiB of
      scrollback on every attach and resync under a full-screen app. At
      ground, a complete `?47l`/`?47h` pair can't disturb the parser, so
      the copy is only paid for when it is needed.
    - The same fix found a second half: a client fed a snapshot taken
      mid-sequence had its parser at ground, so the rest printed there
      too. The wire snapshot now ends with the continuation (the
      sequence's start), as GHOSTSNP checkpoints already did.
    - Tests: `an_alt_screen_snapshot_mid_sequence_leaves_the_stream_alone`
      (split CSI, ESC, OSC, UTF-8, and an OSC too long to keep, on either
      screen): the live terminal, a new snapshot, and a terminal fed the
      snapshot and then the rest all match the stream fed without the
      snapshot. `reattach.spec.ts` opens a page while a full-screen app's
      SGR is half written.
    - Also found: under systemd 254 and later, `systemd-run --scope`
      expanded `$VAR` and `$$` in pane and agent commands (#56). The
      launcher passes `--expand-environment=no` where systemd-run takes it.
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

#### #35: a pane closed as it starts (done 2026-10-02)

- **What landed.** The shim records the program's pid only after the exec (a close-on-exec pipe), so the program already has its session, group and controlling terminal: a hangup sent to the group can't be lost. The record now names the shim too (`shim <pid> <start>`). A close signals the shim (`SIGUSR1`); the shim hangs the group up and kills it 3s later if anything is left, without the daemon. A shim whose terminal hung up before the program started, or that can't write its record, closes the program itself. The daemon keeps its own 3s timer for older shims, and only signals shims that wrote the `shim` line (an old shim would die of `SIGUSR1`).
- **Tests.** api.rs closes a pane right after `run` and kills the daemon at once, then a program that ignores SIGHUP the same way; both must be gone within seconds. The test daemons' `Drop` kills the group of every program recorded under `blocks/*/process` and `closed/*/process` before deleting the state dir (`tests/strays/mod.rs`).
- **Why it was seen.** Not only the record race: killing the daemon while `run` was still starting the program hung the terminal up before the program had made it its own, and the test then deleted the dir the shim records into.
- **Not covered.** Shims started by an older daemon still depend on that daemon for the SIGKILL.

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
a terminal as one block *type* among many. A VM is not a block type. A
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

Shape copied from Superlogical:

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
- **(a) is built (#17, 2026-10-02):** the home daemon's tabs and splits can
  hold panes from other hosts; each host still owns its own layout too. See
  *#17* below.

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
     endpoints are in the Sprites API docs.
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

#### #17: mixed-host tabs (M4 option (a))

**Done 2026-10-02, apart from separate machines and networks.**

- **What landed:**
  - **A remote block** (`BlockType::Remote`, `crates/daemon/src/remote.rs`): a leaf of the home daemon's tree whose config is `{host, pane}`, a host in its list and the pane's id there. It's a block, so `layout.json`, restore, split, move, dock, break out and close all work as for any block; the daemon never talks to the other host about it. `/api/blocks` takes `type: remote` only for a host in the list, never the daemon itself.
  - **The web client** (`web/src/blocks/remote.ts`) draws it: one connection per host serves every remote pane on it (a `Client` with `only`, which attaches just those panes), and the pane's xterm sits in the block's slot, so moving it never redraws it. The slot's badge names the host; the tab gets a host tag. While the host can't be reached the terminal greys out under "box is unreachable · reconnecting…", and the connection's usual retries bring it back by itself.
  - **Making one:** *New tab on box* (the `+` button's right-click menu) and *Split right on box* (a pane's menu); the page asks the host for a shell (`/api/run`, in a session named after the home daemon), then records its place (`/api/blocks`). If recording fails it closes the pane there. `illogical --host box run --home [--split %N] [cmd]` does the same from the CLI.
  - **Sizes:** the window that sizes the home tab sends the host the size of the pane's place (`view`, zoomed if the host's tab has other panes); typing in it claims the home tab first, so "last input wins" holds across both.
  - **Closing:** closing it here (the pane, its tab or its session) closes it on the host too, from the web and from `illogical close`; a pane its host closed leaves the layout here, removed by the first client that sees it gone.
  - `illogical run --session NAME` with a session that doesn't exist yet now starts that session with the pane itself, not a shell beside it.
  - The fleet and the swarm skip remote blocks (the host lists the pane itself); `illogical tui` names the host and pane in their place.
- **Decisions (2026-10-02, the issue's "To decide"):**
  - **What the tree stores:** a block of type `remote` holding `{host, pane}` and nothing else. It isn't `PaneInfo.host`, which names a machine of this daemon's (M3b): another daemon is named by its place in the host list, and its pane by that daemon's id. A block, because the block machinery already persists, restores, moves and closes leaves that aren't PTYs; nothing in core changed. While the host is unreachable the slot keeps the last screen, greyed, with a note; a host no longer in the list says so; a pane the host won't show you (M12 roles) says it isn't shared with you.
  - **Creating one:** the client asks the host, then records it on the home daemon, rather than the home daemon asking the host. The home daemon has no credential for another daemon (tailnet identity is the person's, M4's per-host tokens go the other way), the client already reaches each host directly, and it keeps the home daemon a directory that never talks to hosts on a client's behalf. A client that dies in between leaves a pane in the host's own layout, where it can be seen and closed.
  - **Who owns size, restart policy and history:** the host, as for any of its panes. The remote block has no policy, log or PTY. Its pane sits in a session named after the home daemon on the host, so the host's own page shows where it came from.
  - **Closing:** the daemon closes only the reference; clients close the pane on its host too (the web for the pane, its tab or its session; `illogical close`). If the host can't be reached the pane stays open there, and the client says so. When the host closes it first (it exited, or was closed on the host's page), the first client to see it gone closes the reference: at once if it was seen there before, after 5 s if never (the host's layout can arrive after the home daemon's), and never when the client isn't the host's owner (a guest can't see everything).
  - **CLI:** `--home` rather than the issue's `--tab`: what changes is whose layout the pane lands in, and `--split %N` (a pane here) works with it as well as a tab.
- **Tests:**
  - `crates/daemon/tests/hosts.rs`: `run --home` makes a tab here and a pane there in a session named `home` (and nothing else in it); `--split` too; unknown hosts and the daemon itself are refused; the daemon closes only the reference, `illogical close` both. A unit test for the config.
  - `web/e2e/remote.spec.ts` (two throwaway daemons on 7850–7851): one tab bar holds a home tab and a tab on the other host; a split of the home tab holds a shell there; the pane's size there is its place here; a second window shows the same layout; dragging the remote pane by its grip moves it in both windows without redrawing it, and breaks it out to a tab and back; stopping the other daemon shows both windows "unreachable" while home's pane works, and restarting it brings the pane back by itself with its scrollback; closing it here closes it there; `exit` in it removes it here.
- **Not covered:**
  - separate machines and networks (loopback daemons, as for M4a); a Mac's daemon in particular;
  - control mode (M17): remote panes need the home daemon's page and its host list;
  - the phone's key bar and the terminal-only menu items (restart policy, share link, search) for remote panes: they're on the host's own page;
  - a host whose state was wiped reuses pane ids, so an old reference could show a new pane;
  - a remote pane on a sandbox host keeps that sandbox awake while a page shows the home layout.

### M6: non-terminal blocks (after M4b; M5 is independent of it)

This is Superlogical's step 2, "multiplexer for all work", cut down to what illogical is
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
      100.64.0.10, and resolves to nothing else (no AAAA) through 1.1.1.1
      and 8.8.8.8;
    - the listener is `100.64.0.10:7443` (443 is serve's, 8443 wispd's);
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
  - **Standing rules (#166, decided 2026-10-04 for Jake: the daemon
    store).** "Always" can also be kept by the daemon, for the block's
    directory (and below) or for every agent block, in `rules.json` in the
    daemon's state dir. They're this machine's: not synced between machines,
    and not written into Claude Code's settings (`--user-settings` still
    reads those; illogical never writes them).
    - *As built:* `approve {option: "always", scope: "cwd"|"everywhere",
      prefix?}`. Without a prefix the rule allows the whole tool; with one,
      titles that start with it word for word, and never one with a shell
      separator, substitution or redirect in it. A rule for a VM agent's
      directory names the VM.
    - Every agent block checks the daemon's rules as they are now, after its
      own, so a new block inherits them and forgetting one takes effect at
      once. The transcript says which rule answered.
    - Only the owner makes them (a guest or an MCP caller can't: the MCP
      `agent_respond` has no scope). `GET /api/rules`, `DELETE
      /api/rules/{index}` and `DELETE /api/rules`, owner only;
      `illogical rules [--forget N | --forget-all]`; the session menu's
      *Permission rules…* lists them with Forget. The card's *From now on…*
      offers the prefix (Bash's first word by default) and the scope.
    - Limit: an agent with a shell on this host can edit `rules.json` (as it
      can `layout.json`); the daemon reads it at start only.
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
- **Implementation:** the official Rust SDK (`rmcp`, pinned; 3.5.0 in S14)
  in the daemon. One `StreamableHttpService` at `/mcp` serves both the
  stateless 2026-07-28 protocol (Claude Code) and legacy sessions at
  2025-06-18 (Codex). `illogical mcp` is rmcp's stdio server in front of
  its Unix-socket HTTP client (`from_unix_socket`). Three defaults change:
  - `allowed_hosts` (loopback-only by default) gets the tailnet name and IP;
  - `allowed_origins` (empty, so unchecked, by default) gets the app's
    origins;
  - every resource list or read result sets `ttlMs` and `cacheScope`,
    which Claude Code rejects results without.

**Tools.** About a dozen, shaped for agents rather than mirroring every
endpoint. Output is capped (about 16KB a page by default, 40,000 chars at
most, because Claude Code swaps anything over ~50,000 for a 2KB preview and a
file path) and pageable by offset, so a chatty pane can't flood the agent's
context. Each result's `structuredContent` carries a `summary` sentence, and
the text block is the same JSON. Claude Code shows the model only the
structured part; Codex shows both.

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

- **Long calls:** `run --wait` and `wait` send a progress notification
  with a message every 15s. This is required: over HTTP, Claude Code kills
  a call that is silent for 60s. They return a resumable "still running"
  with the offset after 100s by default (`timeout` asks for longer),
  before interactive Claude Code moves the call into a background task at
  120s. So a long build never fails a tool call or ends the agent's turn.
  Claude Code's hard limit (`MCP_TOOL_TIMEOUT`, default ~27.8h) is not
  extended by progress.
- **Errors** are tool results (`isError`) with a sentence an agent can act
  on, for example "pane %7 is gone; it exited 2 at 14:03", not protocol
  errors.

**Resources:**

- `illogical://pane/%N/output`, `illogical://pane/%N/screen`,
  `illogical://block/%N` (state), and `illogical://history`, read-only.
- Resource templates, so clients can list them.
- Not subscribable in v1. S14 found that no client subscribes: Claude Code
  only listens for resource-list changes, and Codex never lists resources.
  Agents follow a pane with `wait` and `read_output`.

**Agent blocks get it automatically, scoped to their tab:**

- `session/new` passes `mcpServers` with an `illogical` server. S13 showed
  `claude-agent-acp` uses MCP servers passed that way.
- The scope is a token minted per block. The agent can create panes and
  blocks in its own tab (on the tab's machine, in a VM tab), read and drive
  what it created, and read the rest of its tab. It can't touch other tabs
  or hosts.
- **Local agents** get an `http` server: the daemon's loopback `/mcp` with
  `Authorization: Bearer <block token>` in `headers`, so no bridge process
  is needed. claude-agent-acp 0.85.1 advertises `mcpCapabilities.http`, and
  S14 saw the header arrive on every request.
- **VM agents** get a host-side bridge. A wisp guest can't reach any host
  address (bridge, LAN or tailnet: wisp's nftables drop them by design),
  so the daemon opens a non-TTY exec in the VM running a small relay on a
  guest Unix socket and pipes it into its MCP server under the block's
  token. The agent's `mcpServers` stdio command connects to that socket
  (`nc -U …`, or `illogical mcp --socket` if the binary is in the image).
  The relay accepts again whenever the agent reconnects. In S14 this gave
  a 1ms tool-call round trip, with progress flowing through.
- The adapter also hands the agent the user's claude.ai connectors
  (`mcp__claude_ai_*`) even with `settingSources: []`, so the block's
  tools aren't only illogical's.
- **Fountain agents** can't reach the tailnet, so they don't get it.

**Safety.** External clients get full scope, so:

- every tool carries honest annotations (`readOnlyHint`, `destructiveHint`,
  `idempotentHint`), which clients use to decide what to ask about;
- the README recommends a Claude Code permission set: allow the read-only
  tools, ask for the rest;
- every MCP call is logged with the client's name and token. The pane shows
  "started by mcp:<client>", and `history` records it.

**S14: done 2026-10-02.** See [spikes/s14-mcp](spikes/s14-mcp/README.md).
The findings are folded in above. It ran against rmcp 3.5.0, Claude Code
2.1.287, Codex 0.155.1, claude-agent-acp 0.85.1 and a throwaway wisp
sprite:

- **rmcp:** go. Streamable HTTP (both protocols), stdio, a Unix-socket
  client, progress, `structuredContent`/`outputSchema`, annotations and
  `isError` all worked with real clients. Both kinds of resource
  subscription worked with rmcp's client and curl.
- **Claude Code:**
  - over HTTP it kills a tool call after 60s of silence, and progress
    resets that; the hard limit isn't extended by progress;
  - interactive Claude Code backgrounds a call still running at 120s;
  - results over ~50,000 chars become a 2KB preview plus a file, and over
    `MAX_MCP_OUTPUT_TOKENS` a "saved to file" notice; nothing is cut
    silently;
  - with `structuredContent` present, only that reaches the model;
  - it never subscribes to resources.
- **Codex:** stdio and HTTP both work. The model sees text and structured
  content, and its default tool timeout is 300s.
- **VM:** the guest reaches nothing on the host. The host-opened relay exec
  works.
- **claude-agent-acp:** passes `http` servers with headers, and `stdio`
  with or without `type`.

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

#### M16: as built

**Done 2026-10-02, apart from a real phone.** Agent blocks in a VM came after, in #59 (below).

- **What landed:**
  - **The server** (`crates/daemon/src/mcp/`): rmcp 3.5.0 (pinned) in the daemon, one `StreamableHttpService` at `/mcp` on every router the API is on (the socket, TCP, the dial-out tunnel, end-to-end channels), so both the stateless 2026-07-28 protocol and 2025-06-18 sessions work. It's a layer over the mux's own calls (`Api::Run`, `Api::Open`, the API's waits and `act`), not a client of the HTTP API.
  - **Thirteen tools** (`mcp/tools.rs`), the table's twelve with `history` and `search` apart: `run`, `send_input`, `read_output`, `capture_screen`, `wait`, `list`, `close`, `history`, `search`, `open_port`, `start_agent`, `agent_respond`, `read_file`.
    - Each answers with `structuredContent` that has a `summary` sentence, and the same JSON as text. Failures are `isError` with a sentence ("pane %7 is gone; its last command `make` exited 2 3m ago").
    - Annotations: the seven readers are `readOnlyHint`; `run`, `send_input`, `close` and `agent_respond` are `destructiveHint`; `run` and `start_agent` are open-world.
    - Output is paged at 16,000 characters (40,000 at most) by stream offset, cut at line ends, with `next_offset`; an agent's transcript pages by character.
    - `wait` and `run` with `wait` send progress every 15s and answer "still running" (with the offset and the last lines) after 100s, or `timeout`.
  - **Resources:** `illogical://history` and the templates `illogical://pane/{id}/output`, `…/screen` and `illogical://block/{id}`, read-only, with cache hints.
  - **`illogical mcp`** (`crates/cli/src/mcp.rs`): a stdio server relaying to `/mcp` over the socket, or another daemon with `--host`. It adds the session id, protocol version and `Mcp-Method`/`Mcp-Name` headers Streamable HTTP wants, keeps the client's order (each message goes once the one before has its headers, and everything waits for `initialize`), and reopens the session with the client's own `initialize` when a restarted daemon answers 404.
  - **Tokens** (`mcp/tokens.rs`): `illogical mcp token --name N [--scope full|read]`, `--list`, `--revoke N`, over `/api/mcp/tokens` (the owner's). Only hashes are kept, in `mcp/tokens.json`. A request to `/mcp` with a bearer token skips the identity check (`Class::McpToken`, from this machine or the tailnet only) and is checked against the tokens on every request, so revoking cuts a client off at its next call.
  - **Agent blocks** get an `illogical` server in `session/new`, `load` and `resume`: `http` on the daemon's loopback `/mcp` when the agent advertises `mcpCapabilities.http` (claude-agent-acp does), else `illogical mcp --socket` with the token in its `env`. The token is an HMAC of the block's id under `mcp/key`, so it's the same after a restart, ends with the block, and is `<redacted>` in the block's log.
  - **Scope of a block's token:** `run`, `open_port` and `start_agent` split beside the agent (or a pane in its tab), on the tab's machine in a VM tab, with no `vm`, `vm_tab`, `machine` or `session`. `send_input`, `close` and `agent_respond` reach only what it started; the readers reach its tab; `history` and `search` are filtered to its tab's panes.
  - **Who did it:** every call is logged (`mcp call`, with tool, client, token and scope). A pane or block an MCP client started carries `started_by` (`{by: "mcp:<client>", block}`, in `layout.json`), shown on the pane ("started by mcp:claude-code"). `run` types its command into a new shell (`Api::InputBy`), so history has it with `by: mcp:<client>` and the shell stays for you. The client's name is its own (`clientInfo`, per request at 2026-07-28), else the token's name.
  - **Web:** a small "started by …" badge on the pane, bottom left.
- **Decisions (2026-10-02):**
  - **The bridge is a plain relay, not rmcp in the CLI.** The CLI stays without tokio, and the bridge doesn't need to understand the tools. rmcp's HTTP client is used in tests instead.
  - **`run` types into a shell** instead of `$SHELL -c`, so the command is in history with its exit code and who ran it, `wait` can wait for the command's end, and the shell is left for you to take over. It waits for the shell's prompt first (20s here, 5 minutes for a VM).
  - **Any `Authorization` on `/mcp` must be one of our bearer tokens.** It's what skips the identity check, so a request with some other credential (or a malformed one) is refused, never treated as the owner's.
  - **rmcp's Host check is off.** The daemon's own (`Access::check_host`, with the tailnet names) runs on every TCP request; the socket is private. The exact-Origin rule is the API's (`api_origin`).
  - **`tools/list` needs `ttlMs` and `cacheScope` too.** Claude Code 2.1.287 refused our `tools/list` without them (it retried four times and loaded no tools), which S14 hadn't seen. Every list and read result now carries `ttlMs: 0`, `cacheScope: private`.
  - **`list` and `history` for a block's token are filtered, not refused**; a closed pane is readable with a full token only (its tab is gone).
  - **`agent_respond` maps `skip` to deny** (a question's decline), as `/api/attention/act` does.
- **Tests:**
  - `crates/daemon/tests/mcp.rs`, with rmcp's client:
    - through `illogical mcp` (a 2025-06-18 session): the tool list and annotations; `run` with `wait` (exit code, last lines); "started by" and history's `by`; 4,000 lines read back a page at a time; a wait answering "still running" with progress, then `C-c` and exit 130; typing and a match; `capture_screen`, `list`, `search`, resources, `read_file`; a closed pane's error and its output still read;
    - stateless 2026-07-28 on the socket: cache hints on `tools/list` and templates, and the client's name from `_meta`;
    - the bridge across a daemon restart;
    - HTTP with a client token: used, `used_ms`; a read token sees seven tools and can't `run`; revoked mid-session and refused after; an unknown token, `Basic` credentials and an empty bearer refused; a foreign Origin refused;
    - an agent block (`fake_acp.py`, which now advertises http MCP and calls tools on `mcp TOOL JSON`): it got loopback `/mcp` with its token, kept out of its log; it starts `python3 -m http.server` beside itself, waits for it, and opens it in a browser block beside itself; it lists only its tab; six ways of touching another tab are refused; history has nothing from the other tab; it can't close what it didn't start; its token is refused once it closes;
    - one agent starts another (`start_agent`), waits until it asks, answers it (`agent_respond`), waits for the end of its turn and reads the answer in its transcript, and the answer is recorded as `mcp:fake-agent`'s;
    - "what failed in this repo yesterday": history moved back 30 hours, asked with `failed`, `cwd`, `since 2d`, `before 1d`.
  - `web/e2e/mcp.spec.ts`: a command run over `/mcp` as `claude-code`, its pane showing "started by mcp:claude-code", and history having it as theirs.
  - `agents_real.rs` `mcp-cc` (opt-in, `ILLOGICAL_REAL_AGENTS=mcp-cc`, with `mcp-vm` for a VM pane): the real Claude Code (`claude -p`, haiku) with `illogical mcp` runs a build that fails after 20s, waits through it, reads why, fixes it and reruns, all in history as `mcp:claude-code`. Passed on geek, on the host and in a wisp VM pane.
  - Unit tests: tokens (hash only, block tokens stable and per block, revoke), paging, durations, the annotations, the bridge's headers.
- **Not covered:**
  - Fountain agents don't get a server (by design);
  - watching the build on a real phone (Needs Jake);
  - Codex as a client wasn't run against it (S14 ran it against rmcp; the bridge's session path is what it uses, and is tested);
  - resource subscriptions (dropped in S14: no client subscribes);
  - `run` with `host` (another daemon): use `illogical mcp --host` instead.

#### M16 follow-up: agent blocks in a VM (#59)

**Done 2026-10-02.**

- **What landed:**
  - **A host-opened relay** (`crates/daemon/src/mcp/relay.rs`), as S14 found works: for each VM agent block the daemon opens a non-TTY exec in its VM running `guest_relay.py` (python3), which listens on `/tmp/illogical-mcp-<block>.sock` and carries every connection over the exec's stdin and stdout as `N+`, `N:LINE` and `N-` lines. Each connection is its own rmcp session on stdio framing (`mcp::pipe_server`, the server's fallback caller), scoped as the block's token is over HTTP (`Scope::Block`). The relay lasts as long as the block, across agent restarts; it's killed in the guest when the block closes.
  - **The agent's server** is `guest_client.py` on stdio (`python3 -c …`), in `session/new`, `load` and `resume`. It waits for the socket (up to 5 minutes), and when the connection drops (a restarted daemon starts a new relay, which replaces the old one by its pid file) it connects again and replays the client's `initialize`; requests in flight get an error saying to call again.
  - **What a VM agent runs lands on its machine.** The first `run` or `start_agent` from an agent whose machine is its own makes the machine its tab's (`Api::ShareMachine`, as *Share machine with tab*), so what it starts joins the machine and keeps it while they run. Before, such a `run` was refused ("%N's machine is its own").
  - **Fixed on the way:** a new VM agent could die at once (exit 1, `export: … not a valid identifier`): the daemon sent `initialize` before the blank line that ends the guest boot script's environment preamble. Lines now wait for the preamble.
- **Decisions (2026-10-02):**
  - **One MCP session per connection, multiplexed over one exec,** rather than an exec per connection: the guest can't ask the host for a new exec, and one exec per block is what M3b's "an attached exec keeps the sprite awake" cost already pays for the agent.
  - **python3 on both ends in the guest,** not `nc -U` (not in every image) or `illogical mcp` (not in the image). S14's relay already relied on python3.
  - **No token in the VM.** The scope comes from which relay a connection arrives on, so there is no secret to keep out of the guest. Whatever runs in the VM as the agent's user can reach the socket (it's private to that user), the same reach a token in the agent's environment would give.
  - **The relay waits for its machine.** It starts beside the agent, whose own start creates the machine; at first it took wisp's 404 as "the machine is gone" and gave up, which CI caught about one run in three. It now waits (up to 10 minutes) for the machine to exist.
  - **The relay doesn't survive a daemon restart.** Its sessions live in the daemon, so they'd be gone anyway; the client reconnects and replays `initialize` instead.
- **Tests:**
  - `mcp::relay` unit test, both Python scripts run on this host: two clients get sessions of their own; replacing the relay errors the call in flight, and the client reconnects and replays `initialize` (its answer kept from the agent) onto a new session.
  - `crates/daemon/tests/mcp.rs` `an_agent_block_in_a_vm_gets_mcp_through_the_relay` (needs wisp's token; skips without, like `machines.rs`): `fake_acp.py` (now also an MCP client over stdio) runs in a wisp VM; `list` shows its tab only; `run` lands in its VM (the relay's socket is there), in its tab, and the machine becomes the tab's; another tab is refused; after a daemon restart the agent gets through the new relay; closing the tab deletes the machine. Passed on geek.
- **Not covered:** real Claude Code in a VM block calling the tools (`claude-agent-acp` takes stdio servers, S14; not run, it costs money); `open_port` from a VM agent is the existing path (a browser block on its machine's port) and wasn't exercised here.

### After M6: order and triggers

Everything below is planned, but each item starts when its trigger holds, not
on a date. The suggested order:

1. **M6c** (S13 is done), because agent questions are a daily papercut now that
   agent blocks exist.
2. **S14 then M16 (MCP server)**, because it turns everything built so far
   into tools any agent can use, and it is mostly a layer over M3's API.
3. **M7**, because M11 needs its filesystem method, and the picker and session
   names are cheap.
4. **S8**, to choose block types from real use of M6. Done 2026-10-02 (below).
5. **M11, cut** (S8's choice), with M24's Rerun on terminals in place of M10.
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

#### M9 step 1: memory cheap wins (#3)

**Done 2026-10-02.** An idle pane went from 3.3 MB to 1.8 MB of daemon RSS,
a pane with history from 32 to 18 MB (10k lines) and from 149 to 18 MB (at
the cap), and closed panes give their memory back.

- **What landed:**
  - **No Zig signal stack.** libghostty-rs's sys crate is vendored in
    `vendor/libghostty-vt-sys` (the root `Cargo.toml` patches it in). Its
    `build.rs` applies `patches/*.patch` after checking Ghostty out, and
    fetches again when they change. `0001-no-signal-stack.patch` sets
    `signal_stack_size = null`, so `.tbss` is 0x1f8 bytes, not 256 KiB per
    thread. libghostty stays ReleaseSafe (Jake, 2026-10-02).
  - **malloc** (`crates/daemon/src/heap.rs`, glibc only): `mallopt` fixes
    the mmap and trim thresholds at 128 KiB at startup, and each pane's
    thread calls `malloc_trim(0)` once its pane is gone. musl's malloc (the
    static release builds) and macOS's don't need it.
  - **The output ring** holds 1 MiB (`MAX_REPLAY_BYTES`), allocated once;
    `push` drains before it extends, so it never grows.
  - **Scrollback in memory** is capped at 16 MiB, not 64 (about 9.6k rows at
    200 columns). The pane log on disk keeps everything.
  - **Four threads per pane, not five:** the wait thread reaps the shim
    after its program has gone.
  - **The client queue is capped in bytes:** 8 MiB of live output per client
    (it was 1,024 frames of up to 64 KiB, so up to 64 MiB). Snapshots and
    replays count toward it but are never refused for it, since an attach
    queues one per pane at once. Clients that ack (web, TUI) were already
    held to `ACK_WINDOW` per pane.
- **Not done, on purpose:**
  - **A tiny shim binary.** The shim is still `illogicald _shim`: 1.0 MB USS
    each with the signal stack gone (1.26 MB before), so a separate binary
    would save about 0.4 GB at 500 panes. But it's one more binary in every
    tarball, `install.sh`, `install --tailnet` and the macOS build. Shims
    outlive daemon upgrades, so its record format would need versioning.
    Worth it when 500-pane fleets are real.
  - **ReleaseFast** (decided against) and **the page fill:** most of an
    idle pane's 1.8 MB is libghostty's ReleaseSafe page fill. An upstream
    fix would take it to about 0.5 MB. Drafts for Jake to file are in #63.
- **The S9 rerun** (`bench.py`, same parameters, geek, 2026-10-02; daemon
  RSS; `main` at 2440cc3 against this branch, both release builds):

  | scenario | before | after |
  |---|---|---|
  | idle, baseline (1 pane) | 30.3 MB | 21.0 MB |
  | idle, per pane at 10 / 50 / 200 / 500 | 3.48 / 3.33 / 3.30 / 3.30 MB | 2.15 / 1.88 / 1.81 / 1.78 MB |
  | idle, 500 panes | 1,638 MB, 2,534 threads | 887 MB, 2,035 threads |
  | idle, 500 closed down to 1 | 212 MB | 35 MB |
  | idle, 500 reopened | 1,652 MB | 896 MB |
  | shim USS each / bash USS each | 1.26 / 0.72 MB | 1.01 / 0.73 MB |
  | 500 idle shells all in (daemon + shim + bash PSS) | 2.61 GB (5.2 MB each) | 1.74 GB (3.5 MB each) |
  | 10k lines at 200x50, per pane (50 panes) | 31.7 MB | 17.8 MB |
  | 10k lines, 50 panes closed down to 1 | 718 MB | 51 MB |
  | 200k lines (the cap), per pane (10 panes) | 149.3 MB | 18.2 MB |
  | 200k lines, 10 panes closed down to 1 | 776 MB | 44 MB |
  | stalled client: 4 panes after the burst, no client | 315 MB | 94 MB |
  | stalled client: over that, a client not reading | +1.8 MB | +0.5 MB |

  The stalled-client run didn't fill the queue in either build, so the
  byte cap is covered by a unit test, not by this number.
- **Does the trigger still hold?**
  - **RSS per idle pane above 2 MB:** no longer, just. It's 1.8 MB at 50
    to 500 panes (2.1 MB at 10).
  - **More than about 50 panes on geek, or an agent fleet:** these are
    about use, not measured here. If either holds, the trigger holds.
  - **The done bar isn't met:** 500 idle shells cost 1.78 MB each in daemon
    RSS, not under 1 MB. Parking wouldn't fix that cheaply; the page fill
    upstream (#63) would.
  - **Panes with history** are about 18 MB each now, whatever their length:
    a 500-pane fleet with full scrollback would be about 9 GB. That's what
    step 2's terminal parking would save, if real fleets get there.
- **Step 2 (parking, #10): not now. Decided 2026-10-02, #10 closed.**
  - **Use, measured:** geek's daemon held 7 panes in 25 MB of RSS
    (53 threads) after the 0.4.0 upgrade, far under "about 50 panes".
    Agent fleets run in a handful of panes, not hundreds.
  - **Idle cost** is under the 2 MB trigger (1.78 MB at 500 panes).
  - **What's left** of the 1 MB done bar is libghostty's ReleaseSafe page
    fill, which parking doesn't remove cheaply; the upstream fix (#63) does.
  - **Reopen #10** when geek holds more than about 50 panes with real
    history, or when a fleet's daemon passes about 2 GB. `memory.rs` guards
    the idle cost in CI meanwhile.
- **Tests:**
  - `crates/daemon/tests/memory.rs` (in `just check` on Linux, about 5 s):
    S9's idle scenario at 50 panes against the debug binary. An idle pane
    must cost at most 2.6 MB (it's 1.85 MB; `main` measured 3.1 MB and
    fails), and the daemon may keep at most 8 MB after 49 panes close (it
    keeps about 5 MB).
  - Unit tests: the ring never grows past 1 MiB, even for a chunk bigger
    than itself; the client queue refuses live output past 8 MiB but takes
    snapshots, and counts only what it holds; libghostty keeps about 9.6k
    rows of 40k written at 200 columns.
- **Not covered:**
  - `malloc_trim` and the thresholds on musl and macOS (they don't apply).
  - Real workloads (Claude Code, long build logs, wide panes), as in S9.
  - A stalled client that really fills its queue end to end.

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

#### S8: done 2026-10-02

**Result: a cut of M11 next; no M10 block types; no notes block** (see
[spikes/s8-blocks](spikes/s8-blocks/README.md)).

- **The trigger hadn't held.** M6 landed 2026-10-01, and the daily daemon's
  whole history is 131 commands over about 23 hours. #55 asked for the
  decision anyway, so it was made on that day of use, Forgejo's issues, git
  history, shell history on geek, and Claude Code's transcripts. There is no
  `docs/dogfood.md`.
- **People don't build, tail logs, read diffs or read files in illogical yet;
  agents do all four in bulk** (this repo's transcripts: ~1,260 build or test
  commands, ~1,000 polls of a background job, 625 git history or diff reads,
  ~1,900 file prints, out of 7,595). Jake's own messages ask whether things
  are built, merged and green, not to see diffs or logs.
- **Job:** terminals already are job blocks for a person (M23's `build`/`test`
  kinds, M24's `failed` reason with exit code and duration and a push,
  `illogical wait`), and agents have their own background jobs and M16. What's
  missing is acting on a failure from the phone: M24's unbuilt **Rerun**.
- **Service:** no evidence of a dev server or service that needed keeping up.
- **Diff and file:** nothing reviews an agent's changes on the phone, for any
  agent, after the fact (M28's diff card is one pending Claude Code edit;
  M27's code-server is the desktop). Agents read changes as a list first
  (`--stat`/`--oneline` 3:1 over full diffs). A typical change here is 8
  files and ~500 lines, p90 28 files and ~3,400. Most of the parts exist: M7's
  `fs` on every host, M28's CodeMirror follow view, `Provider::run`, agent
  tool calls' `locations`.
- **Contract changes** (from fitting each candidate to `block.rs` on paper):
  - viewers can't call block methods (`/api/blocks/N/call/*` is editor,
    `/api/fs/*` owner), so a file or diff block puts what's drawn in its
    pushed state;
  - a block's log may be just an event log (a file block's truth is the file);
  - a block must know whether any client draws it: `fs.watch` keeps a VM
    awake, so file and diff blocks watch only while drawn;
  - (not needed yet) a block can raise only plain `input`/`done` reasons; a
    job or service type would need `BlockCtx::attention_with(Reason)`.
- **Revisit** when someone keeps a terminal open only to watch a dev server or
  a log, a VM tab's dev server dies across a wake, a CI or hal0 job is watched
  from illogical, or the daily daemon has two weeks of history.

### M10: job and service blocks

**Not as block types (S8, 2026-10-02).** #7 closes with S8's numbers. Its
*Done when* about a failed build moves onto terminals: M24's `failed` reason
gets a **Rerun** action, built with M11's cut below. The service block waits
for S8's *revisit* triggers. The original plan is kept below for then.


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

**Cut by S8 (2026-10-02). Done 2026-10-02 (below), apart from a real phone.** The full brief is in
[spikes/s8-blocks](spikes/s8-blocks/README.md#what-m11-cut-must-deliver).
Changes from the plan below:

- **Diff block:** sources are a host's working tree against `HEAD` (staged,
  unstaged, untracked), one rev against the working tree, or a range,
  computed by `git` on the host (`Provider::run` on a VM). A file list with
  +/− first, each file expanding to unified hunks, every line with **Open
  file**. Unified on the desktop too (M27 has split diffs). No ACP tool-call
  source: an agent block's tool call with a location gets **Open file**
  instead, and M28's card already shows Claude Code's pending edit.
- **File block:** `{host, path, line?}` through `fs.read`, drawn in M28's
  CodeMirror follow view, scrolled to and marking `line`.
- **Both** watch only while some client draws them (so a VM can sleep), put
  what's drawn in their pushed state so a shared session's viewers see it
  (methods stay editor-only), and log only what they were pointed at.
- **Ways in:** `illogical diff [--host H | %N] [REV_A [REV_B]]`,
  `illogical view %N:PATH[:LINE]`, the same as MCP tools, and **Changes** on
  a tab's and a pane's menu and on swarm tiles with a project.
- **Plus M10's remainder:** M24's `failed` reason on a terminal gets
  **Rerun**, which types the command again into the pane's idle shell, from
  the badge, *Needs you* and the push.
- **Done when (replaces the one below):**
  - from the phone, on a VM tab where an agent has changed files, **Changes**
    opens a diff block listing them; tapping a hunk's line opens a live file
    block at that line;
  - both keep updating while the agent edits, and stop watching once no
    client draws them;
  - a shared session's viewer sees both and can't change what they show;
  - `capture --text`, `describe`, `illogical diff` and `illogical view` work
    on a local host and a VM;
  - a build that fails in a VM tab's terminal shows as *Failed* on the phone,
    and **Rerun** from the phone runs it again in that pane.

#### M11 (cut): diff and file blocks, and Rerun

**Done 2026-10-02, apart from a real phone.**

- **What landed:**
  - **Diff blocks** (`type: diff`, `crates/daemon/src/review/diff.rs`): `{repo, rev_a?, rev_b?}` on the block's host. One `sh -c` there (one exec on a VM, through `Provider::run`) finds the repository's top, checks the revisions, runs `git diff -M` and diffs each untracked file against `/dev/null`, cut at 4 MB, with `GIT_OPTIONAL_LOCKS=0`. The state is the file list (status, +/−, binary, too big over 256 KB) and, for files someone opened (`file {path}`, at most 12), their hunks with both sides' line numbers. `capture --text` is the unified diff. A repository with no commits is compared with the empty tree; an option-like revision is refused.
  - **File blocks** (`type: file`, `review/file.rs`): `{path, line?}` read through M7's `fs` (`fs::Target`, now shared), so the same places are refused and a VM's file goes through the provider with no symlinks followed. At most 1 MiB, cut at a line; binary says so. Following an edit keeps the mark on its text (`follow`: common head and tail, else the same line nearest its place). `goto {line}`; `open {path, line?}` is the owner's only (someone with editor could otherwise read any file of the owner's).
  - **Drawn** (S8's gap 4): `Block::drawn(bool)`. The mux works out, after every message, which blocks some full client draws: one whose `View` is their tab, and on a phone (`zoom`) the zoomed pane only; summaries-only clients don't count. The two views poll only then (every second here, 3s on a VM; the diff every 2s here) and say `watching` in their state. A block that nobody draws holds what it last read; brought back after a restart it reads nothing until drawn.
  - **Viewers** (gap 2) get what's drawn in the pushed state; methods stay editor (gap 3: the log has only what each was pointed at).
  - **Ways in:** *Changes* on a pane's and a tab's menu, the phone's sheet and a swarm tile with a project (a diff block beside the pane, on its machine, its directory's repository: `view_defaults` in the mux, like M27's editor); `illogical diff [%N] [--repo D] [REV_A [REV_B]]` (prints the block, then the files) and `illogical view [%N:|mN:]PATH[:LINE]`; MCP's `show_changes` and `show_file`; *Open file* on an agent block's tool call location. Tapping a hunk's line opens a file block beside the diff, or points the one it opened last there.
  - **Web:** `blocks/diff.tsx` (the list, then hunks highlighted by `highlightLines`, the follow view's Lezer parsers and colours as `hl-*` classes, in the same lazy chunk) and `blocks/file.tsx` (M28's `CodeView`, read-only, with a marked line; edits replace only what changed, and it scrolls to the line only when someone moves the mark).
  - **Rerun** (M10's remainder): M24's `failed` reason carries a `rerun` action when the command line is known. `/api/attention/act` with `rerun` types it again (`fs::type_line`, `cd`'s check that the shell is idle at its prompt) or says why not. From the phone's *Needs you*, a swarm card, the push (`sw.ts`: Rerun and Dismiss), the tab's ✗ badge (a menu), the TUI and `illogical rerun %N`.
- **Tests:**
  - `crates/daemon/tests/review.rs`: a repository with every kind of change (unstaged, staged, deleted, renamed, untracked, binary, a 400 KB diff), one revision, a range, a bad revision, not a repository, `capture`, `describe`, `illogical diff` and `illogical view`; a file block and a diff block that don't see edits while nothing draws them, follow them (the mark moving with its line) while a WebSocket client views their tab, stop when it goes, and ignore a summaries-only client; a viewer gets both states (hunks, text) and is refused every call, an editor may `goto` but not `open`; a failed build's `rerun` refused while busy, then run again (twice in its history).
  - Unit tests: parsing git's output (every status, quoted and odd names, caps), hunk numbering, the mark following edits, the rerun line.
  - `e2e/changes.spec.ts` on a Pixel 7 profile: on this host, *Changes* from the sheet lists the stand-in agent's files with +/−, a hunk's line opens a live file block marked there, both follow later edits, both stop watching when the phone shows the terminal and catch up when shown again; a viewer's phone sees both and can't change them; a failing build is *Failed* and *Rerun* in *Needs you* runs it again. On a VM tab (wisp): the same Changes → hunk → file flow with the agent writing through the Sprites API, both blocks on the tab's machine, `illogical diff`, `view`, `capture` and `describe` there, and a build failing in the VM tab's terminal rerun from the phone. `attention.rs` and `attention.spec.ts` expect Rerun on a failure and its notification.
- **Decisions (2026-10-02):**
  - "Drawn" comes from what clients already send (`View`'s tab and zoom), not a new message: no protocol change, and a phone showing another pane, or a phone page hidden long enough to drop its socket, stops the polling.
  - "Open file" is an ordinary file block opened from the diff block (`from_pane`), not a diff method: the host and repository follow from the block, and the client reuses the file block it opened last.
  - Expanded files are shared state, like the layout: someone opening a file's hunks opens them for everyone looking, which is what lets viewers see them.
  - Polling (stat every 1–3s, `git diff` every 2–3s) rather than inotify: it works the same on a VM through the provider, and only runs while someone looks.
  - Rerun types the command without dismissing first: its start sets the pane working, which replaces the reason; refused, the reason stays.
  - The spec's ports are 7830 and 7831 (7826–7828 were already `editor-swarm.spec.ts`'s).
  - *Changes* made the terminal's menu taller than a 640px window, so menus now scroll when they don't fit (`layout.spec.ts` and `tui.spec.ts` found it).
- **Not covered:**
  - a real phone (Playwright's Pixel 7 profile only), and tapping Rerun on a real notification (the test checks the notification's actions);
  - an M4a peer's or a resident sandbox's repository is reached by opening the block on that host (its own daemon), not tested here;
  - a working tree with more than 200 untracked files lists the first 200; a diff over 4 MB is cut (both say so);
  - a file block on a VM refuses any path with a symlink in it, as `fs` does there.

The original plan:


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
agents. Enterprise access control stays a non-goal.

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
- **Presigned invites and expiry (#136, from #126).** The roster rule checks a redeem's `at` against the invite's `expires`, but the invitee writes `at`, so only control checks expiry with a clock of its own. The gap: an invitee holding the link redeems after expiry with a backdated `at`, no owner has written a roster version since the invite expired, and control lets it through. Decision (2026-10-04, from Jake via the user): accept the gap. A daemon doesn't check invite expiry against its own clock; control's check stands.
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

**The MVP (#44) is done (2026-10-02), apart from real phones and different networks.** `e2e/swarm-mvp.spec.ts` walks its done-when in one flow: two people on a team, a laptop and a phone each, two machines each and a team box, all through a local control's relay; both swarms grouped by person; a Claude Code approval on one person's machine on all four rails, allowed by the other from the phone's strip and attributed to them; the follow-up through the agent's inbox (after a trust grant on a personal machine, straight through on the team's box); `log --who` and history naming who did both.

- **Still pending:**
  - real phones (iOS notification actions, a service worker's WebSocket there, the canvas on real hardware), and people on genuinely different networks; loopback and Playwright's phone contexts stand in;
  - what each ticket left for after the MVP: previews and live preview text (#37, #40), prompt detection and the rerun, restart and send actions (#38), pulse clustering, the correlation toast and keyboard shortcuts (#40);
  - two owners of one team box are one principal as drivers (#47).

**Order:**

1. **S16** (#36) and **S18** (#45): done, below.
2. **M23** (#37: pane summaries) and **M24** (#38: attention reasons and actions), side by side with **M29** (#46: team answers): done.
3. **M25** (#39: every host in one page), then **M30** (#47: the team's swarm): done.
4. **M26** (#40: the swarm view): done.
5. **After the MVP:** editors, with **S17** (#41: done, below), **M27** (#42: VS Code blocks, done) and **M28** (#43: your editor in the swarm, done).
6. **M41** (#116: themes, blocks and city): below.
7. **M42** (#117: the hive and the timeline themes): below.

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

#### S17: editors spike (Claude Code's IDE protocol, remote extensions, editor events, servers)

**Done 2026-10-02, apart from the phone and the editors that weren't installed** (see [spikes/s17-editors](spikes/s17-editors/README.md)). Claude Code 2.1.287.

- **illogicald as a Claude Code IDE: go, as a complement to M29's hook.**
  - **How it works:** a lockfile in `~/.claude/ide/<port>.lock` (pid, folders, `ideName`, `transport: "ws"`, `authToken`) and MCP over a loopback WebSocket (subprotocol `mcp`, header `X-Claude-Code-Ide-Authorization`, no `Origin`).
  - **What Claude Code sends:** every Edit and Write in default mode becomes `openDiff`, which waits for the answer: `FILE_SAVED` (with contents, which may be changed before accepting), `DIFF_REJECTED`, or `TAB_CLOSED`.
  - **When the terminal answers first,** Claude Code calls `close_tab`, so the IDE learns it lost. The hook never does (S18).
  - **Only Edit and Write.** Bash, MCP tools and AskUserQuestion stay with M29's hook and M6c, and acceptEdits sends nothing.
  - **No reconnect:** after the IDE goes away, Claude Code stays disconnected until someone types `/ide`.
- **Beside a real IDE:** an extension registers with its folders. If illogicald does too, `--ide` finds two valid IDEs and connects to neither.
  - So illogicald registers with **no folders**, and puts `CLAUDE_CODE_SSE_PORT` in every pane's environment.
  - Then Claude Code in a pane always picks illogicald, and anywhere else never sees it. Tested with a real code-server running Anthropic's extension.
- **Remote extensions:** a `workspace` extension runs in the server's extension host and reached a unix socket on the server's machine.
  - **Where it worked:** Microsoft's VS Code Server 1.140 (the build Remote-SSH installs, through `code serve-web`), code-server 1.140, and openvscode-server 1.109.
  - **Containers:** in Docker it runs in the container, and reaches the host's socket only when its directory is mounted.
  - **nvim** reaches the socket with core `vim.uv`.
  - **Zed is out:** its extensions are WASM with no editor events (from its docs).
  - **Not run:** the SSH leg itself, Cursor and the Dev Containers extension.
- **Event rates while typing at 7.4 characters a second:** 16 events/s in nvim, 23 in VS Code.
  - **Summary fields** (file, diagnostic counts, unsaved buffers, debugger) change about 0.1 times a second: about 2 B/s per editor at M23's 1 s tick.
  - **A follower's stream** needs a 100 ms throttle: 7 messages and 0.4–0.6 KB/s, at most 100 ms behind. A 250 ms trailing debounce starved for up to 52 s while someone typed.
  - Debugger events weren't measured.
- **M27's server: code-server.**
  - **For it:** VS Code 1.140 and about weekly (openvscode-server's latest is 1.109.5, from February); MIT; Open VSX; brotli (6.1 MB a page against 17 MB uncompressed); `--auth none`, `--disable-workspace-trust`, `--socket` and `--idle-timeout-seconds`.
  - **Memory** on geek: 135 MB idle, 417 MB with a client once `chat.disableAIFeatures` is on (openvscode-server: 84 and 259).
  - **In a wisp sprite:** 5.4 s to first start, 130 MB / 429 MB.
  - **In an M6a browser block** with no auth of its own, both worked unchanged: a file showed 1.7–2.0 s after the block opened.
  - **But the port answers any local process,** so M27 serves it on a 0600 unix socket.
- **Follow mode: CodeMirror 6.** Read-only, with three languages, lint underlines and the terminal's colours: 182 KB gzipped, drawn in 66 ms. Monaco cut down to the same is 780 KB (4x the whole app).

#### M23: pane summaries

**Done 2026-10-02, cut to the MVP (#44).**

- **What landed:**
  - every pane says what it's busy with (`PaneInfo.kind`: shell, build, test, agent, server, logs or editor), from the foreground process's argv first, then the typed command, then the pane's own process (what `illogical run` started); agent blocks are `agent` (`classify.rs`, S16's heuristic, which sees `c` as the `claude` it runs);
  - `project`: the git root of the cwd and its name, found by walking up for `.git` (no git process) and cached per directory;
  - `activity`: `{bps, last_ms}`, from the pane's running byte count and last output time, kept under the lock `State::output` already takes; the mux works out the rate once a second, so idle panes need no timer and parked panes aren't woken;
  - `title`: the OSC 0/2 title, read only when a chunk carries one;
  - **deltas for every client:** the hello is a whole `State`; layout changes (anything that moves the layout's `rev`) and grant changes still go as a whole `State` at once; everything else is a `delta` (`ServerMsg::Delta`): each changed pane as `{id, field: value…}` (`null` for a field gone back to absent), panes that left the client's view, and `machines` or `presence` when they change. Attention, questions and drivers go within 40 ms (`touch`); directories, commands and activity at the next once-a-second tick. Only the panes that changed are rebuilt, and what the OS says a pane runs is read at most once a second. A `ping` answers after whatever came before it was sent;
  - `subscribe {summary: true}`: summaries only, for the swarm and the fleet: answered with a fresh `State` whose panes leave out `epoch`, `policy` and `integration`, and it attaches to nothing (`new Client(base, e2e, true)` in the web client makes no terminals);
  - role filtering: a person sees summaries only for sessions they have a role on, and someone else's private pane is only `{id, private: true, …}`, with no directory, command, kind, project, activity, title, question or reason;
  - `State::apply` (proto) for Rust clients: the tmux front end and the tests follow deltas; `illogical ls --json` shows kind, project and activity.
- **Measured** (`spikes/s16-swarm/m23.py`, S16's load: 500 panes, 50 busy, a build-like command every ~5 s each, release build on geek):

  | | daemon CPU | to each client | deflate | messages |
  |---|---|---|---|---|
  | S16 (whole `State`s), 1 client | 14.7% | 6.6 MB/s | 390 KB/s | 34 `State`/s |
  | M23, 1 client | **2.5%** | **5.3 KB/s** | 0.48 KB/s | 9.3 `delta`/s |
  | M23, 5 clients | 3.1% | 5.3 KB/s each | 0.48 KB/s | 9.5 `delta`/s |

  Idle (500 panes, no load) is 1.1–1.4%, as before. The hello is 243 KB at 500 panes (S16: 209 KB; the new fields).
- **Tests:** `crates/daemon/src/classify.rs` (S16's labelled fixture plus this repo's commands, and projects from git roots and worktrees); `crates/daemon/tests/summaries.rs` (`illogical ls --json` shows kind, project and activity for real processes, `cargo test`, `npm run dev`, `claude` behind an alias, `nvim`, `tail -f`, `journalctl -f`, and an `illogical run` pane; 40 panes with 4 busy send only deltas, under 20 KB/s; a summaries-only `State`); `e2e/summaries.spec.ts` (a summaries-only client gets kind, project and activity as deltas and makes no terminals while the tab view beside it draws the pane; a viewer gets only the shared session, a private pane blanked, and nothing after revoking).
- **Not covered (after the MVP):** on-demand previews (the client naming panes it draws big enough to read); hover uses `/api/panes/{id}/capture`. A layout change still sends a whole `State` (243 KB at 500 panes): fine while layouts change at human speed, but a script opening hundreds of panes sends one each. A title set by an OSC split across two reads is picked up at the next one. M9's PTY parking (not built) must keep calling the byte count update.

#### M24: attention reasons and actions

**Done 2026-10-02, cut to the MVP (#44).**

- **What landed:**
  - every `needs_input` and `done` pane has a `reason` (`PaneInfo.reason`, `illogical_proto::Reason`): its kind, `since_ms`, a one-line headline, the command, exit code and duration where there is one, a bundle key, and the actions it takes;
  - kinds: `ask` (an open question or permission request: Claude Code's AskUserQuestion through its hook, or an agent block's), `failed` (a command that ran at least 3 s ended non-zero, not Ctrl-C), `done` (a command that ran at least 5 s finished unwatched), `exited` (the pane's program died non-zero, or its machine went) and `input` (a bell, a notification, an agent gone quiet; the Notification hook's message is the headline);
  - bundle keys: `failed:<machine>`, `exited:<machine>`, `ask:<project>:<agent>` (the project is M23's: the cwd's git root, else the cwd, in `mux::project_key`); `done` and `input` never bundle;
  - an ask's reason is worked out live from the open question, so it changes as soon as the question does;
  - `POST /api/attention/act` with one pane or a list: `allow` and `deny` (an agent block's approval), `answer` and `deny` (a question), `dismiss`. Each pane needs editor on its session, checked in the handler (all or nothing), and each is answered on its own (`{results: [{pane, ok, error?}]}`, 409 when none took);
  - `GET /api/attention` and `illogical attention [--json]` list them, `illogical events` carries the reason on `attention` events, and push notifications are titled by kind ("Failed", "Done", "Needs you") with the headline as the body and the reason's actions in the payload (a failure's offers Dismiss);
  - the web client: the tab and pane badges say failed or done with the headline as their title, the phone's "Needs you" list shows headlines, and Dismiss goes through the act route, so it clears on every client.
- **Tests:** `crates/daemon/tests/attention.rs` (a failing `cargo test`, a long `make build`, a quick failure that isn't one, Claude Code's question through the hook answered by `act`, three agent approvals allowed and denied as a list, the push's actions, the event stream); `e2e/attention.spec.ts` (a failure's badge and headline, dismissed on one client and gone on the other; a pushed failure offers Dismiss).
- **Not covered (after the MVP):** prompt detection for `input` (`[sudo] password`, `[y/N]`), and the `rerun`, `restart` and `send` actions. A 10-minute build is tested as a 5-second one. Approvals of Claude Code's tool permission prompts in terminals come with M29.

#### M25: the fleet in one page

**Done 2026-10-02, apart from real phones and a real provider sandbox.**

- **What landed:**
  - **Every host at once** (`web/src/fleet.ts`): the page holds a summaries-only connection (M23) to every host in the directory, control's list or the home daemon's, each over its usual transport and its own end-to-end channel. The tab view still connects for real to the one host it shows, so a pane's output is attached only when it's opened (`fleet.open(host, pane)`, which also wakes a sleeping sandbox).
  - **One model:** a pane is `host:pane` (`FleetPane`: the host, its `PaneInfo`, its session, `stale`, and the host's owner and team from control's directory). Each host is `connected`, `stale` (dropped, with when it was last heard), `offline` (never reached, or gone a minute), `asleep` or `capped`. A host that's away keeps its panes in view from the last summary, greyed (`stale: true`), and the last summaries are kept in `localStorage` for the next load.
  - **One socket to control for every relayed daemon** (`/api/relay/m`, `web/src/e2e/relaymux.ts`): numbered channels (`OPEN`, `OPENED`, `DATA`, `CLOSE`) inside one WebSocket, each carrying one daemon's Noise channel through the daemon's existing relay stream, so the daemon side is unchanged. Control routes, checks `may_reach` per channel, counts bytes per account and slows free accounts over their allowance, as for `/api/relay/c/<id>`. The tab view's own channel goes the same way. Hosted sandboxes keep a socket each (their provider's proxy). A control without the route falls back to a socket per daemon.
  - **Behaving well:**
    - connects go through a limiter (4 at a time, each holding its slot until it connects or fails, at most 3 s), with jitter;
    - after a wake (a gap in the page's timers, the page becoming visible, or `online`) every host that's down reconnects, spread over 1.5 s;
    - a heartbeat (a `ping` to a host quiet for 3 s) notices a link that died without closing within 6–7 s;
    - a slow or dead host only ever holds its own slot;
    - at most 24 connections, the most recently used first; the rest show from what was last known, with a notice in the host menu;
    - a provider sandbox that isn't `running` is never connected just to be counted.
  - The host menu (and the phone's sheet) says what each host is doing: "2 panes · live", "stale, seen 4s ago", "asleep".
  - A summaries-only connection isn't a person: it's left out of presence, and doesn't keep its person driving a pane (M13).
- **Measured** (`e2e/fleet.spec.ts`, `e2e/fleet-control.spec.ts`, headless Chrome on geek, loopback):

  | | hosts | first connect | after a wake (3 runs) | failed tries | relay sockets |
  |---|---|---|---|---|---|
  | S16 (one socket each, all at once) | 20 | 4.2 s | 2.2–4.6 s | 0 | 20 |
  | M25, direct | 23 | 0.38 s | 0.47–0.48 s | 0 | 0 |
  | M25, through control | 20 (19 relayed) | | 0.50–0.52 s | 0 | **1** |

- **Tests:** `e2e/fleet.spec.ts` (three machines' panes in one page on the laptop and a Pixel-sized phone; opening one attaches it in its tab; a killed machine greys at once and a stopped one, its socket still open, within 10 s, and both come back; 20 more machines back after three simulated wakes with no failed tries; a cold sandbox not connected; the cap's notice); `e2e/fleet-control.spec.ts` (a direct and two relay-only machines on a local control, on the laptop and an approved phone; the relayed ones over one `/api/relay/m` socket and no `/api/relay/c/`; a relayed machine killed greys within 10 s and comes back; 20 machines with 19 relayed come back after three wakes over one new socket with no failures).
- **Not covered:**
  - real phones and different networks (loopback stands in, as for S15, S16 and S18), and daemons on different addresses (all here are 127.0.0.1);
  - a real resident sandbox: a tailnet stand-in plays it, and "asleep" is tested from a provider status, not a real Sprites or wisp sandbox;
  - an "unplugged" machine is a stopped process (its socket stays open), not a pulled cable.
- **For M30 (#47):** `fleet.list` and `fleet.panes` are the merged model; each host carries `owner` and `team` from control's directory (a session's owner is its daemon's: absent means yours, `team` a team box). `fleet.touch(host)` keeps a host among the 24 live ones.

#### M26: the swarm view

**Done 2026-10-02, cut to the MVP (#44), apart from real phones.**

- **What landed:**
  - **Where:** `/#swarm`, from a *Swarm* button beside the tabs, the host menu and the phone's sheet. It draws every pane in the fleet (M25, M30), and works with one daemon too.
  - **The field** (`web/src/swarm/field.ts`, ported from the prototype):
    - every pane is a tile on one Canvas 2D, coloured by kind and lit by activity;
    - tiles are pulled toward their cluster's centre by how busy they are, and pushed apart through a grid;
    - it shows stubs when you zoom in, and a header with the command and machine at reading zoom (no live text: that's after the MVP);
    - stale hosts' panes are greyed;
    - physics sleeps once everything settles (S16: it's half the frame) and wakes on a regroup, a new pane, attention or a touch.
  - **Cluster by** project, machine, kind, session or person (M30's `person`: "you", a teammate, "team …"):
    - panes outside any git project group by their working directory's top directory under a home (`~/scratch`), else its first path component (`/tmp`), never one "none" pile;
    - switching animates the panes to their new clusters, then fits them;
    - the choice is remembered per device;
    - clusters spread wide on a laptop and tall on a phone.
  - **On the field:**
    - hover peeks at a pane's last lines (`/api/panes/N/capture` through its host);
    - clicking a pane opens it in its tab, connected for real (`fleet.open`);
    - clicking a cluster's name zooms to it;
    - Fit brings everything back;
    - the view keeps fitting until you move it yourself.
  - **The "needs you" rail:**
    - **What lands there:** M24's reasons, one card per bundle key. Failures and exits bundle by machine (with "here" named), so "3 failed on build-02" bundles across the fleet. Asks bundle by project and agent ("2 agents ask"). The rest are one card each.
    - **On the field:** a pane with a card flares in place, then flies to it, with a thread back to its cluster.
    - **Actions:** each card has its reason's actions for all its panes (Allow all, Deny all, Dismiss all), one request per host. Then Open and Show.
    - **Permission cards:** a single Claude Code permission prompt shows its command and Claude's suggestions (M29's card, now shared in `ui/answer-card.tsx` with the terminal's).
    - **Questions:** AskUserQuestion is answered on the card itself.
    - **Viewers** get the card without buttons.
    - **After an answer:** acting sends the panes back to the swarm. An answered ask leaves a card saying who answered it ("Allowed by sam, 14:02"), with the follow-up box, for a minute.
    - **When the rail is full,** the rest pulse in place and the rail says how many.
    - **Done cards** clear themselves after 15 s.
  - **Phone:** the rail is a strip of cards along the bottom, the field pinches and pans, and a tap opens a pane.
  - **Notifications:** a notification with a reason deep-links to its card (`/#swarm=[daemon.]N`), and the service worker tells an open page to show it.
  - **A fake fleet:**
    - `e2e/fake-fleet.ts`: daemons with scripted panes. Stand-in `cargo`, `npm`, `journalctl` and `nvim`; projects in git repos and plain directories; a stand-in `claude` that asks through the real hooks and waits on its inbox. `trouble(machine)` fails a batch on one machine.
    - `just fake-fleet` runs it by hand.
    - `src/swarm/fake.ts` adds a few hundred synthetic panes for the frame-rate check and screenshots.
    - `just screenshots` now makes `site/img/swarm.png` and `swarm-phone.png`.
  - **The classifier** now looks through a shell running a script (`bash ./bin/cargo test` is a test), which is how /proc shows `#!/bin/bash` programs.
- **Frame rate at 500 panes** (`e2e/swarm-fps.spec.ts`, physics kept awake, headless Chrome on geek, the S16 setup):

  | profile | fps | work per frame (p50) |
  |---|---|---|
  | laptop (1400×860) | 60 | 1.1 ms |
  | phone, Pixel 7 viewport, CPU 4x slower | 60 | 4.3 ms |

  This is the same as S16 measured for the prototype (1.1 ms and 4.4 ms). Settled, the field draws nothing.
- **Tests:**
  - `e2e/swarm.spec.ts`, against the fake fleet:
    - each grouping, with the fallback groups and person from M30, remembered across a reload;
    - a failure bundle dismissed together;
    - an approval allowed, and its follow-up reaching the agent's inbox;
    - two agents denied as one card;
    - a question answered on its card;
    - a done card clearing itself;
    - a full rail;
    - hover peek, cluster zoom, opening a pane, a deep link;
    - on the phone: allow and dismiss in the strip, pinch, tap to open.
  - `e2e/swarm-real.spec.ts` (opt-in, `ILLOGICAL_REAL_AGENTS=swarm`): the real Claude Code's Bash permission allowed from the rail. The file appears only after Allow, and the agent carries on to its reply.
- **Not covered (after the MVP):**
  - live preview text;
  - pulse clustering;
  - the correlation toast;
  - keyboard shortcuts;
  - rerun on failure cards (M24's later actions);
  - real phones. The phone numbers are S16's 4x-throttle stand-in.

#### M30: the team's swarm

**Done 2026-10-02, apart from real phones and different networks.**

- **What landed:**
  - **What the fleet holds:** your machines, the team's machines (M19), and machines of teammates that shared a session with you or with the team. Control's directory lists each with how to reach it (and now says which of your own machines are a team's); never what's on them. Each daemon still filters the summaries it sends by role (M23).
  - **Sharing a session with the whole team** on a personal machine (the Share dialog's "Share with everyone in Acme"): a `team:<id>` grant pinned to the team's founder as the owner's browser pinned it. The machine fetches the team's rosters (`/api/daemon/teams`, only for teams its owner is in), checks them back to that founder, and lets members in by them: the grant's role, at most their role in the team. Members who come and go come and go with the roster, a locked team lets only its owners in, and control nudges members' machines when a roster changes or a team locks.
  - **People:** each pane of the merged model (`fleet.panes`) carries `person` (you, a teammate by account, or a team for a team's machine: a session's owner is its machine's), `driver` (M13) and `watchers` (who has it open, from presence, which summary clients still receive). `fleet.byPerson()` groups them for "cluster by person"; the host menu groups machines the same way ("Yours", "bob's", "Team Acme").
  - **Names:** someone who is an owner on a machine through control is called by their login, not "owner": the machine learns its account's login from control, and a team box's owners are named by the roster (`Subscriber.name`). Drivers and presence use it.
  - **Leaving:** revoking a share, removing a member or locking the team takes those panes, and with them their cards, out of the other person's fleet: the machine says "your access was removed" before it hangs up (now on end-to-end channels too, when a device stops being trusted), and the fleet drops what that machine showed instead of keeping it greyed. Control's shared relay socket now sends a channel's close in order behind its last message, so those words aren't lost.
  - Private panes (M14) aren't in another person's fleet at all, not even as tiles.
  - **Scale:** 5 people with 4 machines each is 20 summary connections, under M25's cap of 24; past it, the least recently used drop to "capped" (shown from the last summary) and come back when opened (`fleet.touch`).
- **Tests:** `e2e/team-swarm.spec.ts` (a local control; Alice and Bob on a team, two machines each and a team box, all relayed): Alice shares a session with Bob and one with the team, Bob one with Alice, and both pages hold the same team swarm; each sees their own private pane and not the other's; by person it's three groups for each; a pane's driver and watcher reach the other's fleet by name; revoking Bob's share takes its panes and its card out of his fleet within a second; locking the team takes the team box's and the team share's panes out in about 0.23 s (8 runs).
- **Not covered:**
  - real phones and different networks (loopback, as for S15, S16 and S18);
  - two owners of one team box both act as its owner, so as drivers they are one principal (`owner`) with two names;
  - a team share's members are found through control's rosters, so a member added while control is down waits for it (as a team box's do).

#### M29: team answers

**Done 2026-10-02, apart from real phones.**

- **What landed:**
  - **Every answer has an author.** Approving, denying, answering and skipping, from a card, `call`, the act route or a notification, record the person who made the request. Their name goes into an agent block's transcript ("Allowed git push by sam") and its history entries. Any pane's card closes on every client saying who and when (`PaneInfo.answered`: "Allowed by sam, 14:02"). It also goes into the pane's history (`allowed: Bash: cargo test`, by them), into `illogical log --who` (a turn of theirs) and into the audit log (`action: answer`). The first answer wins; a second gets "it was answered".
  - **Claude Code's permission prompts in terminals** become approval cards through `illogical hook` on its `PermissionRequest` hook. The card is matched to the `PreToolUse` just before it (same session and subagent, tool and input) for its `tool_use_id`, whichever arrives first. It shows the tool, its input (the command, the file and its diff) and Claude's own suggestions: Allow, Always (one suggestion as `updatedPermissions`), Deny, and Deny with a message. AskUserQuestion stays with `illogical ask`.
  - **Cards close when the terminal answers first:**
    - `PostToolUse` or `PostToolUseFailure` for its tool call ("allowed in the terminal");
    - the session's next `PreToolUse`, `Stop`, `UserPromptSubmit` or `SessionStart` ("closed");
    - the hook's SIGTERM after "No" or Esc ("denied in the terminal").
    `illogical hook` passes these events to the daemon.
  - **Follow-ups.** Once a card is answered, it has a "Send a follow-up" box (`POST /api/panes/N/followup`).
    - Agent blocks: the block's next prompt, attributed.
    - Claude Code in a terminal: `illogical inbox`, a background (`asyncRewake`) hook on `Stop` and `SessionStart`, waits for it. There is one waiter per pane (a newer one replaces the older), and follow-ups queue (up to 8) until one is waiting. It exits 2 with the text, which wakes Claude Code. Nothing is typed into the prompt.
    - The follow-up is recorded as input from its sender.
    - Who may send one is the drive-rights rule (`MayDrive`, as for `send`). On someone's own machine, a 403 makes the box offer "Ask <owner> for 30 minutes" (M14's trust request).
  - **Who's looking:** the card shows avatars of teammates who have the pane open (M13 presence).
  - **Push to the team.** `needs_input` goes to the owner and to every editor of the session who opted in, per session or for everything they may edit ("this team's agents"). Opting in is in the session menu (`/api/notify`, kept in `notify.json`). This holds for the daemon's own push, whose subscriptions now belong to a principal and may come from anyone with access, and through control, which before notified every editor.
  - **Answering from a notification.**
    - Pushes carry what to approve, for terminals too, and through control as well (encrypted per device).
    - `sw.js` is now built from `src/sw.ts` (`vite.sw.config.ts`).
    - The control page copies its checked directory (daemon id, Noise key, URLs, relay) into IndexedDB. The service worker answers Allow, Deny, a one-tap answer or Dismiss through `/api/attention/act`: on the daemon's own page with a fetch, and through control over an end-to-end channel it opens with the device key.
    - A tap opens the pane, with its card.
  - **Viewers** see the card and who answered, without buttons or a follow-up box. The API refuses them (403).
- **Tests:**
  - `crates/daemon/tests/team_answers.rs`, on S18's fixtures:
    - a card matched to its tool call;
    - allow, allow always and deny-with-a-message as Claude Code takes them;
    - history and audit;
    - each way the terminal closes a card;
    - AskUserQuestion left alone;
    - the inbox waking, queueing and being replaced.
  - `e2e/team-answers.spec.ts` (control, a team box and Jake's machine on loopback, all reached through the relay; Sam on a Pixel-sized touch context):
    - Sam allows Jake's cargo test from the phone, and Jake's page says "Allowed by sam";
    - Sam's follow-up needs Jake's trust, then reaches the inbox;
    - `illogical log --who` and `history` attribute both to sam;
    - on the team box the follow-up goes straight through, and the viewer sees the card but gets a 403;
    - the service worker allows a card over its own channel.
  - `agents_real.rs` `team` (opt-in, haiku): the real Claude Code 2.1.287 TUI. A `touch` allowed from the card ran ("Allowed by PermissionRequest hook"), and a follow-up through the inbox woke the idle agent.
- **Not covered:**
  - Real phones: iOS Web Push actions, and a service worker's WebSocket there. The tests stand in with CDP push delivery and a dispatched `notificationclick`.
  - The two people really on different networks.
  - Notification opt-in through control's own UI: the daemon decides, and the session menu sets it.

#### M27: VS Code blocks

**Done 2026-10-02, apart from a real phone and a real reboot.**

- **What landed:**
  - **An `editor` block type** (`crates/daemon/src/editor/`): VS Code as code-server on a folder, on the block's machine, drawn like a browser block on a port (its own site, `b-<id>-<key>.localhost` or `b-<id>.<domain>`). A folder opens as itself; a file opens in its project (its git root, else its directory) at its line.
  - **Starting one:** *Open in editor* on a pane's menu, a tile's right-click menu in the swarm and *Edit* on a one-pane card there (on the pane's machine, in its directory), and `illogical edit [PATH[:LINE]] [--line N] [--machine mN|local] [--split right|%N]`. The owner's only, like ports (block sites admit only the owner): guests are refused, viewers and editors alike, and the menus don't offer it.
  - **One server per machine** (`editor/server.rs`), shared by its blocks. On this host: a 0600 Unix socket beside the CLI's (`<sock>-code`), no TCP port, no auth of its own, `--config`/`--user-data-dir`/`--extensions-dir` under `<state>/editor/`, in a systemd scope of its own (else its own process group) so a daemon restart leaves it running. It starts when a block is made, or when a block's site is dialed and nothing answers (`ports::Target::Service`), and stops itself after `--editor-idle` (900 s, code-server's `--idle-timeout-seconds`); `--reconnection-grace-time 300`.
  - **The release:** code-server 4.140.0 (VS Code 1.140, MIT), downloaded into `~/.cache/illogical/code-server/` the first time and checked against the release's SHA-256 (all four Linux/macOS builds pinned); the block shows the download's progress. `--code-server PATH` runs another. Nothing is committed or bundled.
  - **Settings, extensions, theme:** a settings folder per user in `<state>/editor/`; new settings get the illogical theme, `chat.disableAIFeatures`, no startup editor and no secondary sidebar, and are the user's after that. Extensions come from Open VSX (code-server's default). illogical's extension (`editor/ext/`: the theme, from the terminal's colours, and a reporter) is installed by writing it into the extensions folder and its `extensions.json`.
  - **Reports:** each block has a workspace file (`<state>/editor/w/<id>/<name>.code-workspace`) whose settings name the block (`illogical.block`). The extension reads it and `$ILLOGICAL_SOCK` and calls the block's `report` method (active file, cursor, the 7 lines around it, unsaved count), throttled to 250 ms and only on change.
  - **In summaries (M23):** `kind: editor`, the project, and a new `PaneInfo.file` (relative to the folder); the title is "file — folder". `capture` (the swarm's hover preview) is `file:line` and those lines. Blocks now give their own summary fields (`Block::summary`), and a block's change goes out at the next tick.
  - **Restoring:** the block's config keeps the folder, file, line and key (so the same origin). After a daemon restart the window's sockets reconnect to the same code-server session, file and all. After a reboot the page asks the new server for the file: opening a file is in the page's address (VS Code's `payload=[["openFile", "vscode-remote://<block's host>/path:line"]]`), so it needs no channel into the window.
  - **In a VM:** the server is a sprite service on loopback port 13340 in the VM, which downloads and checks the same release there, reached through the Sprites proxy; the folder and project are found on the VM. The extension can't reach the daemon from there, so a VM's block shows the file it was opened on and doesn't follow the cursor.
  - The TUI shows an editor block as "VS Code file:line".
- **Measured** (geek, warm server, debug daemon, headless Chrome through the dev scheme): `illogical edit crates/control/src/auth.rs:20` to line 20 drawn in the block: 1.4 s on a desktop page, 1.9 s on a Pixel 7-sized page. code-server starts in about 0.3 s once unpacked; the download and unpack took 6 s here.
- **Tests:** `crates/daemon/tests/editors.rs` with a stand-in code-server (`tests/fake_code_server.py`): its flags, socket mode and environment (no `ILLOGICAL_PANE`, no `VSCODE_*`); the workspace, theme and extension; the page's address; requests reach it only through the block's site, as `Host: localhost`; reports in summaries and captures; two blocks share one server; a daemon restart keeps the file and the server; a dead or idle server starts again on the next request; closing removes the site and workspace; viewers and editors can't open one. Unit tests: the download (bad checksum refused, nothing left over), the settings and extension install, payloads, paths. `web/e2e/editors.spec.ts` with the real code-server: *Open in editor* on the pane's directory, its origin and theme, the 0600 socket; `illogical edit FILE:LINE` under 3 s on desktop and phone-sized pages, reports following the cursor; the swarm's editor tile, its preview, and *Open in editor* from a tile; a daemon restart and a "reboot" (code-server killed too) with the file still open; a viewer has no menu item and is refused. `web/e2e/editors-vm.spec.ts` (needs wispd): VS Code in a VM tab from its terminal's menu, the theme there, and `illogical edit --machine mN` on a file in the VM.
- **Decisions (2026-10-02):**
  - Owner only, not "anyone with write access": block sites admit only the owner, so a guest's block couldn't be shown to them.
  - The block's identity reaches the extension through its workspace file, and files open through the page's address; no daemon-to-extension channel. The cost: the title says "(Workspace)".
  - The pinned release, not a `code-server` on `PATH`: illogical relies on recent flags.
  - code-server's own logs stay in `~/.local/share/code-server`: moving them means a different `XDG_DATA_HOME`, which its terminals and language servers would inherit.
  - `illogical edit --machine`, not `--host`: the global `--host` (another daemon) swallows a subcommand's `--host` (#61 for `open` and `agent`).
- **Blank in a frame from another site (#69, fixed 2026-10-02):**
  - **What happened:** with the app and the blocks on different sites, an editor block was white or an empty workbench, while the same address on its own worked.
  - **Why:** a browser that blocks third-party cookies (Chrome's setting, its Incognito default) refuses a cross-site frame its storage too. VS Code falls back to memory when IndexedDB is refused, but reading `localStorage` threw (the profiles and the secrets provider), and the workbench stopped. The specs ran in Playwright's Chrome, which allows third-party cookies; their app (127.0.0.1) and blocks (`*.localhost`) were already different sites (`Sec-Fetch-Site: cross-site`), and they passed. `sites.rs`'s checks refused nothing here.
  - **The fix:** a site can carry a head script (`Site::set_head_script`), served at `/.illogical/head.js` on the block's own origin and put first in its HTML navigations (asked for uncompressed, so the proxy can edit them). An editor block's is `editor/storage.js`: in-memory `localStorage` and `sessionStorage` when the real ones are refused. VS Code keeps its settings on the server, so nothing that matters is lost with the page.
  - **Also:** `frame-ancestors` now lists `'self'` (VS Code frames its own web worker extension host; every ancestor must still match, so a block is only ever inside the app) and leaves out IPv6 literals (CSP can't say them, and Chrome logged an error per page). The site logs each refusal at debug (`illogicald::sites`), with the host, path, `Origin` and `Sec-Fetch-*`.
  - **Decisions:** a script in the page, not the Storage Access API (it needs a click and a prompt per block, and VS Code would still read `window.localStorage`), and not a patched code-server (VM blocks unpack their own copy, and `--code-server PATH` runs another). The security model is unchanged: the script is illogical's, on the block's own origin, and `check` is as it was.
  - **Tests:** `editors.spec.ts` opens a file at its line in a Chrome profile that blocks third-party cookies, from the app on another site (`sec-fetch-site: cross-site`, `sec-fetch-storage-access: none`); it fails without the script. `tests/editors.rs`: the page through the site has the script first, uncompressed, and the script is served from the site but not to another site. Unit tests for the script's place, `frame-ancestors`, and `check`'s refusals.
  - **Not covered:** the window in the report had both sockets connected, which the reproduction never gets to; whether that browser blocks third-party cookies is still to confirm (Needs Jake). Safari and Firefox weren't run.
- **Not covered:**
  - the 3 s target from a real phone on geek over the tailnet scheme (Needs Jake);
  - a real reboot of geek, and a `systemctl --user restart` of the installed service (the tests restart a daemon run by hand);
  - macOS (the download and process group paths compile; not run on a Mac);
  - the cursor in VM blocks, and blocks on another host's daemon from this page beyond what that daemon does itself;
  - installing an extension from Open VSX in the tests.

S17's notes for M27:

- **The server is code-server.** It runs with `--auth none --disable-workspace-trust --disable-telemetry --disable-update-check`, with `--config`, `--user-data-dir` and `--extensions-dir` under illogical's state. Without those, it writes `~/.config/code-server` even for `--help`.
- **Its defaults:**
  - `chat.disableAIFeatures: true` (VS Code 1.140's agent host and Copilot runtime are 117 MB);
  - the illogical theme;
  - the illogical extension (M28) pre-installed, so every block is also an editor presence. Its preview tile is the lines around the cursor from that stream; a cross-origin block can't be screenshotted from the page.
- **The daemon is its only auth.** It listens on a 0600 unix socket (`--socket`, `--socket-mode`), not a TCP port, which any local process can reach. M6a's port proxy gains a unix-socket target.
- **Stopping it:** `--idle-timeout-seconds` for the idle stop, and a short `--reconnection-grace-time`. Otherwise its extension host stays up after the last block closes.
- **The 3 s target:** on geek a block showed a file in 1.7–2.0 s; the phone run is still to do.

#### M28: your editor in the swarm

**Done 2026-10-02, apart from a real VS Code on the laptop over Remote-SSH, a real phone, real Claude Code, and the marketplaces.**

- **What landed:**
  - **Editors join** (`crates/daemon/src/editor/link.rs`). An editor connects to `/api/editors/connect` on the daemon's socket: an HTTP upgrade to lines of JSON both ways, which Node's `http` and nvim's `vim.uv` both speak with nothing added. It says `hello` (editor, remote, authority, workspace, the block it is), then S17's schema: `summary` (file, diagnostic counts, unsaved files, debugger, conflict, at most once a second and at once for attention), `peek` (the lines around the cursor, kept in the daemon for previews and `capture`), and while someone follows, `follow`, `open`, `edit` and `diagnostics`. The daemon says `welcome`, `followers`, `resend` and `continue`. Lines from 1, columns from 0 (UTF-16), as VS Code and CodeMirror count.
  - **A presence** (`editor/presence.rs`) is a block outside the layout: an id from the panes' space (`Mux::reserve_pane`), `type: editor`, `kind: editor`, its project, its file, and `PaneInfo.editor` (app, remote, authority, hostname, diag, dirty, debug, conflict, followers). No tab, no PTY, nothing saved; it goes the moment the connection closes (a whole `State` to everyone). An editor block's window (M27) says its block in `hello` and becomes that block's link instead, so blocks follow and pause the same way.
  - **Follow** (`ClientMsg::Follow`, `ServerMsg::Follow`): the daemon keeps the last `open`, the edits since, the diagnostics and the cursor, so a new follower draws at once, and tells the editor how many follow. Only the clients following get the stream, on their own connection (end to end through control). It needs read access to the pane.
  - **Reasons** (M24): `paused` (Continue, Dismiss), `errors` (a save took the error count from 0), `conflict` (an open file with markers), and `diff` (Accept, Reject, Dismiss). A reason of the same kind saying more (the debugger's line arriving after the stop) replaces the last.
  - **The VS Code extension** (`editor/ext/`, now `illogical.illogical-editor` 0.2.0; M27's `illogical.illogical` is removed where found): workspace kind; joins only on *illogical: Show this workspace in the swarm*, remembered per folder in a file under its global storage on the files' machine (workspace state was flushed too late to survive a reload); a status bar item that says when someone follows; the debugger from a debug adapter tracker (`stopped`, `continued`, the first `stackTrace` frame for the line); Continue runs `workbench.action.debug.continue`. `illogical editors vsix` writes its VSIX (a stored zip made by the daemon, `editor/vsix.rs`), `illogical editors install` runs `code`/`cursor --install-extension`, and `just vsix` makes one for the marketplaces.
  - **illogical.nvim** (`editors/nvim`): core `vim.uv`, `:IllogicalJoin`/`:IllogicalLeave`/`:IllogicalStatus`, remembered per folder in `stdpath('data')`; edits from `nvim_buf_attach`'s `on_lines` as line ranges (the end of the file, which has no newline, handled); nvim-dap's stops when it's installed.
  - **Dev containers:** the daemon also listens on `<state>/editors/sock` (a 0700 directory with only that socket, serving only `/api/editors/connect`), and `editors/devcontainer` is a dev container feature that mounts that directory and sets `ILLOGICAL_SOCK`.
  - **illogicald as Claude Code's IDE** (`crates/daemon/src/ide/`). A relay process (`illogicald _ide_relay`, in a scope or process group of its own) holds the loopback listener, the lockfile (`~/.claude/ide/<port>.lock`, 0600, `workspaceFolders: []`) and every Claude Code connection; it checks the token, refuses any upgrade with an `Origin`, answers MCP itself, closes diffs on `close_tab`/`closeAllDiffTabs`, and passes `openDiff` and `getDiagnostics` to the daemon over `<state>/ide/relay.sock`. A daemon that connects (again) is told every connection and open call. The port and token are kept in `<state>/ide`, so a new relay takes the same port the panes already have. It stops 5 s after its daemon goes with no Claude Code connected, 60 s with one.
  - **Diff cards:** the daemon finds the pane by walking up from Claude Code's pid (`ide_connected`) to a pane's shell, retrying each tick (after a restart the relay speaks before the panes are adopted). `PaneInfo.diff` (file, +/−, a unified diff of at most 16 KB, from a small line diff in `ide/diff.rs`) and the `diff` reason; `GET /api/panes/N/diff` has before and after. Accept answers `FILE_SAVED` with the contents (changed ones if *Change…* was used), Reject `DIFF_REJECTED`; it's recorded as M29's answers are (history, audit, "Accepted by …"). The terminal answering first closes the card as "answered" by the terminal. `CLAUDE_CODE_SSE_PORT` is in every terminal's environment, not in blocks' (code-server's own terminals keep their own IDE).
  - **Which IDE gets diffs:** `illogical ide --diffs NAME` (`PUT /api/ide`, the owner's), or *Diffs here ▾* on a card: the daemon reads that IDE's lockfile and passes each `openDiff` (and its `close_tab`) on with its token; if that fails, the card shows here. `POST /api/ide/mention` sends `at_mentioned` (*Ask Claude* in a follow view).
  - **The web:** editor tiles are labelled with their file and app; clicking an editor that joined follows it (`swarm/follow.tsx`), and *Follow* is on cards and on blocks' right-click. The follow view is CodeMirror 6 (`swarm/code.ts`, its own chunk: 508 KB, 180 KB gzipped, loaded on first follow) in the terminal's colours, with the selection, diagnostic underlines and the debugger's line; *Continue*, *Open here* (`vscode://`/`cursor://`, on this computer or `vscode-remote/ssh-remote+HOST`, or an editor block there) and *Ask Claude*. The rail draws the new reasons; the diff card (`ui/diff-card.tsx`) is on the rail and over the terminal (`TermDiff`, before any permission card for the same edit).
  - **CLI:** `illogical editors`, `editors vsix`, `editors install`, `illogical ide [--diffs NAME]`; `--no-claude-ide` on the daemon.
- **Measured** (geek, debug daemon, headless Chrome, `web/e2e/editor-swarm.spec.ts`): the phone-sized follow view moved to the editor's new cursor line 30–32 ms after Go to Line in VS Code; *Take this workspace out of the swarm* to the tile gone from the phone's swarm, palette typing included: 335 ms.
- **Tests:**
  - `crates/daemon/tests/editor_swarm.rs`: an editor joins, reports (file, counts, title, `capture`, `GET /api/editors`) and leaves at once; following streams the file, cursor, edits and diagnostics to the follower alone, a second follower gets the current file at once, and the editor hears the count; a paused debugger is a card Continue answers; errors only after a save, and a conflict; guests of a session don't see editors; the editors' socket joins editors and serves nothing else; illogical.nvim in a real headless nvim (joins, follows, its edits including deleting the last line, a save, leaves and forgets the folder).
  - `crates/daemon/tests/ide.rs`, with `tests/fake_claude.py` (S17's recorded behaviour, standard library only) in a pane: the lockfile and the port in panes; accept, change then accept, reject, a stale id refused, the terminal answering first; a daemon restart keeps the connection and the card; browsers and wrong tokens refused; diffs passed to another IDE; viewers see a diff and can't accept it, editors can; `at_mentioned`.
  - `crates/daemon/tests/editors.rs`: a block's window links as that block.
  - Unit tests: attention from summaries, the follow snapshot, the line diff, the VSIX (its CRCs, through Python's `zipfile`), the authz policies.
  - `web/e2e/editor-swarm.spec.ts`, with the real code-server and the extension installed from its VSIX into a code-server of its own (the stand-in for a Remote-SSH window): joins only when asked; the phone's swarm shows it; the phone follows the cursor, typing, another file and a selection, and the status bar says so; a real breakpoint (js-debug, a `debugger` statement) is a card on the phone's rail and Continue runs it to the end; a diff from the stand-in Claude Code in a terminal shows beside the terminal and is accepted from the phone's rail, and lands in the file; turning the workspace off removes the tile at once and stays off after a reload.
- **Decisions (2026-10-02):**
  - An editor that joined is in no session, so it's the owner's, and a team daemon's members' by their team role; a session's guests don't see it. Following needs read access, Continue and Accept need editor. An editor block stays in its session's roles.
  - The relay is a process, not the FD store: it works without systemd (macOS, tests), and the daemon needs no WebSocket state to come back.
  - The editor protocol is lines of JSON over an HTTP upgrade, not a WebSocket: neither the extension host nor nvim has a WebSocket client for a Unix socket.
  - The extension's id is `illogical.illogical-editor` (the issue's name); the publisher `illogical` is a placeholder until one is registered.
  - The opt-in is remembered in a file on the files' machine, as nvim's is, not in VS Code's workspace state.
  - When diffs go to another IDE there's no card here; if passing one on fails, the card shows here instead.
  - A dev container gets an editors-only socket, never the daemon's own (which can drive every pane).
- **Not covered:**
  - VS Code and Cursor on the laptop over Remote-SSH (code-server stood in, as in S17), the Dev Containers extension (the feature isn't published or run), a real phone, and real Claude Code (the stand-in does what S17 recorded 2.1.287 doing);
  - publishing the extension to Open VSX and the Marketplace, and the dev container feature to a registry (Needs Jake);
  - nvim-dap (no debugger in the nvim test), and Cursor's own Remote-SSH;
  - "Open here" links were built, not clicked through to a desktop editor;
  - `getDiagnostics` answers with nothing (editors' per-file diagnostics reach only followers).

S17's notes for M28:

- **Editors:** VS Code and Cursor (one extension, `extensionKind: ["workspace"]`, on Open VSX and the Marketplace), and nvim (core `vim.uv`, no dependencies). Zed is dropped: its extensions can't see the cursor or open a socket.
- **Finding the daemon:** the extension connects to `$ILLOGICAL_SOCK`, else the default socket path. Under Remote-SSH that path is on the remote machine.
  - Dev containers need the socket's directory mounted. A dev container Feature adds the mount and `ILLOGICAL_SOCK`.
- **The schema** is in [spikes/s17-editors](spikes/s17-editors/README.md#the-editor-event-schema-for-m28), in two parts:
  - **A presence** in M23's summary (`kind: "editor"`, host, remote, project, file, diagnostic counts, unsaved buffers, debugger, followers) at the 1 s tick. Attention changes (paused, errors) go at once.
  - **A follow stream** only while someone follows: cursor, selection, visible lines, then `open` and `edit` messages and the file's diagnostics. A 100 ms throttle, never a trailing debounce; it's content, so end-to-end channels only.
- **Follow mode** draws with read-only CodeMirror 6, loaded only when someone follows.
- **illogicald as a Claude Code IDE: go.**
  - **Registering:** one loopback listener per daemon, and a lockfile with `workspaceFolders: []`, mode 0600. `CLAUDE_CODE_SSE_PORT` goes in every pane's environment.
  - **Checks:** the token, and refuse any upgrade with an `Origin`.
  - **The diff card:** `openDiff` becomes an accept/reject card, which can also edit the proposal before accepting. It closes on `close_tab` or `closeAllDiffTabs`.
  - **"Which IDE gets diffs"** is a daemon setting. To send diffs to the user's real IDE, the daemon forwards `openDiff` to it, using that IDE's lockfile and token, instead of re-pointing the environment variable.
  - **Extras:** `selection_changed` and `at_mentioned` let the web hand Claude Code lines from a follow view.
  - **Restarts:** Claude Code doesn't reconnect by itself, so a daemon restart must keep these WebSockets (S3's fd store, or a small process that outlives the daemon).
  - **Only Edit and Write** come this way; M29's hook still handles everything else.

#### M41: swarm themes, blocks and city (#116)

**Built 2026-10-03.** The swarm's bar has *Theme*: **blocks** (M26's field, the default) or **city**, the same panes in 3D, remembered per browser (`illogical.swarm.theme`). The mockup it came from: <https://claude.ai/artifact/J9usdMhboLiQPUjG4Pvkt1>.

- **One scene interface.** `SwarmScene` (`set`, `regroup`, `fitAll`, `diveTo`, `start`/`stop`/`resize`, `clusters`, `screenOf`, `measure`) in `field.ts`; `Field` implements it, and so does `City` (`web/src/swarm/city.ts`). The view makes one or the other on a canvas of its own (a canvas with a 2D context can't take WebGL) and feeds both the same `FieldPane`s, which grew what the city needs: `id`, `sub` (the row: machine, or project when clustering by machine), `bps`, `started` (the running command's `started_ms`), `lastDur`, `lastExit`, `people` (watchers other than you, `driving` for the driver), `att.since`.
- **What the city means (every channel one thing a pane reports).**
  - A block is a cluster; its rows are machines; lots go in pane order, so a pane keeps its address and buildings move only on a regroup (or when panes come and go in its block).
  - Height: how long its command has run, `0.45 + 1.4·log2(1 + s/8)` (13 at most), kept from `last` when it's done; while it waits on you (a reason newer than its start) it stops at `since`. Servers, logs, studio apps and editors (they run until stopped), and PRs and issues (not processes), stand low and fixed.
  - A lit roof: a command still running (only kinds that finish). A red roof: the last one exited non-zero.
  - Windows: scrolling while it prints (speed from `bps`), lit and still for a minute or two after `last_ms`, dark when quiet. The scroll is accumulated, so text stops where it was.
  - Colour is kind (the field's `KINDS`); shape is lifecycle: box finishes, drum runs until stopped, hexagon agent, pentagon editor, slab PR or issue.
  - A beam in the reason's colour on whatever has one, `3 + 9·log2(1 + s/20)` tall after `s` seconds waiting. A red wall around a block with a failure in it. Greyed, dark: its host isn't live. A marker over a building: a teammate with it open, a cone while they drive it.
  - *How to read the city* (a `<details>` under the legend) says all of this on the page.
- **Same behaviour as blocks.** Hover peeks (the view's peek), a click opens the pane (an editor that joined: follow), right-click is its menu, a block's name or plate flies to the block, *Show* and a notification's `#swarm=N` fly to the pane, *Fit* frames the whole city in what the bar and rail leave free (corners projected, distance searched) and keeps doing so until you orbit. Orbit, pan, zoom and pinch are three.js's `OrbitControls`; the camera's view offset centres it beside the rail (or above the phone's strip). Reduced motion: no scrolling windows or flights.
- **Loading.** `three` (0.180, MIT; THIRD_PARTY.md) is imported only by `city.ts`, which the view imports when the city is picked: its own chunk (≈136 kB gzipped); blocks doesn't fetch it. If it fails to load, the view goes back to blocks.
- **The synthetic fleet** (`swarmFake`) now has running and finished commands that start and stop, so the frame-rate check and screenshots show heights; its first pane is always on sam's machine (a random 40 sometimes had none, which made *clusters by project … remembered* fail about one run in twenty).
- **Tests:** `web/e2e/swarm-city.spec.ts` (3): blocks is the default and three.js isn't fetched; the city draws the same clusters and pane count for the same grouping, regroups, is remembered across a reload, and goes back; three failed tests on one machine are one card and three beams, *Show* flies to the first, dismissing drops the beams; a building has height, peeks its last lines on hover and opens its tab on a click; a notification's link opens the city at its card and flies to the pane; on a phone the rail is a strip and a tap opens a building. `swarm-fps.spec.ts` adds the city at 500 panes (≥ 20 fps in headless Chromium's software WebGL; about 52–55 measured). The swarm, editor-swarm, team-swarm and fps specs pass.
- **Left:** the city on real phones and GPUs. Agents: an agent block reports no `current`, so it stands low; Claude Code in a terminal is one long command, so its height is the session's length, not its turn's (no turn start is reported yet).

#### M42: the hive and the timeline themes (#117)

**Built 2026-10-03.** Two more themes under *Theme*, taken from a mockup of nine views: <https://claude.ai/artifact/NZoeJbYneDHuyZcxKYC36D>. Both are Canvas 2D `SwarmScene`s on a shared base, `Flat` (`web/src/swarm/flat.ts`). The base handles the camera, pan, zoom, pinch, fitting to what the bar and rail leave free, hover, click, right-click and the frame-rate check. Each theme is its own lazy chunk, like the city: the hive is about 2.5 kB gzipped and the timeline about 3.5 kB.

- **Hive** (`hive.ts`): one hex cell per pane, in a comb per cluster.
  - Cells spiral out from the comb's middle by row (machine), then pane number, so a pane keeps its cell until a regroup.
  - Combs are packed in rows. The packer tries row widths and keeps the one that draws biggest in the free box, so the hive is wide on a laptop and tall on a phone.
  - Fill: how long the command has run, on the city's log scale, full at an hour (`fillFor`). It stops where it was while the pane waits on you. Bright while running, faded once done.
  - What runs until stopped (`UNTIL_STOPPED`, the city's `LONG` kinds) is full and hatched.
  - Edge: pulsing while it prints (faster for more `bps`), faint for a while after `lastOut`.
  - A red rim: the last command exited non-zero.
  - Needs you: the cell fills with the reason's colour and shows how long it has waited. An additive glow spills onto its neighbours, `1.3 + 1.15·log2(1 + s/20)` cell radii wide (7 at most, `spillFor`).
  - A ring is a teammate with the pane open; it's dashed and turns while they drive it.
- **Timeline** (`timeline.ts`): one lane per pane under its cluster's name, with now at the right edge.
  - Fit shows the last 40 minutes across, with lanes at their own height, so 150 lanes scroll rather than shrink.
  - Bars: finished commands, as long as they ran, coloured by kind. Stripe density is bytes a second (`(end − start)` stream offsets over the duration). A red cap means a non-zero exit.
  - The bars come from each connected daemon's `/api/history?since=2700`, fetched through a new optional `FieldHooks.history` every 30 s. Commands the view sees end while it's open are added too, as is each pane's `last` (`FieldPane.lastEnded` is new).
  - The running command's bar has its last four minutes shaded by the `bps` it sampled, and a lit leading edge while it grows. What runs until stopped is a thin line.
  - Needs you: a band in the reason's colour from `att.since` to now, with a "waits 4m06s" tag past the now edge.
  - A dot at the now edge is a teammate with the pane open; it's ringed while they drive.
  - The label column (`%id` and command), the cluster names and the time axis stay in place while you pan. Axis ticks pick their own step as you zoom.
- **Same behaviour as blocks and city:** hover peeks, a click opens the pane (or follows an editor that joined), right-click is its menu, *Show* and a notification's `#swarm=N` go to the pane, and *Fit* frames everything again. *How to read the hive* and *How to read the timeline* sit under the legend, as the city's key does (`ThemeKey`).
- **Tests:** `web/e2e/swarm-hive-timeline.spec.ts` (7):
  - Each theme draws the same clusters and pane count as blocks, regroups, is remembered across a reload, and goes back to blocks.
  - Three failed tests are one card, and the cluster's three panes need you. *Show* centres the pane, and clearing the rail clears them.
  - A pane peeks on hover and opens its tab on a click.
  - The timeline has the failed runs (exit 101), and a finished build is a bar as long as it ran, taken from history.
  - On a phone, the rail is a strip and a tap opens a pane.
  - `swarm-fps.spec.ts` adds both themes at 500 panes: 60 fps, with median frame work of 2.6 ms for the hive and 0.6 ms for the timeline. The swarm, city, MVP, editor-swarm and fps specs pass.
- **Left:** panes closed in the last 45 minutes have no lane (their history is kept, but the timeline draws today's panes), and synced history from hosts that are away isn't drawn. Agent blocks report no commands, so their lanes are empty apart from reasons; Claude Code in a terminal is one long bar.

### TUI track (S19, M31–M32, added 2026-10-02)

illogical in any terminal, as [herdr](https://herdr.dev) does. `illogical tui` draws tabs, splits and a "needs you" sidebar over the same socket and protocol as the web client, locally, over ssh, or against another host. It is one more client, so the daemon doesn't change.

**Order:**

1. **S19** (#48): done, below.
2. **The resync fix** (#49): done 2026-10-02. Snapshots are capped at the client's scrollback and zstd-compressed, and a resync brings back the screen alone. With S19's four-pane flood, snapshots went from 1.44 GB of 1.53 GB received to 9.8 KB of 612 MB, and the TUI drew 7x as much real output. A quiet pane beside a flood in a browser throttled 6x echoes in under 100 ms (`web/e2e/flood.spec.ts`).
3. **Flow control** (#52): done 2026-10-02. Clients ack what they have drawn, the daemon holds them to a 512 KB window, and a pane's program waits when the pane can't keep up. In a browser throttled 6x, a flooded pane catches up 0.2–0.4 s after the flood ends, against 15–25 s before. With four floods, the TUI never resyncs and uses about 36% of a core.
4. **M31** (#50: `illogical tui`): done 2026-10-02. Then **M32** (#51: copy mode): done 2026-10-02.

#### S19: TUI spike

**Done 2026-10-02: go** (see [spikes/s19-tui](spikes/s19-tui/README.md)).

- **What was built:** a standalone crate of about 950 lines.
  - It attaches every pane of a tab, keeps a local libghostty terminal per pane fed with the web client's snapshot and output frames, and copies their cells into a ratatui buffer.
  - It draws the daemon's own `TabView.layout`. Its one-cell gaps are the dividers, so there is no layout code in the client.
  - A sidebar shows tabs with attention and *needs you* from `reason`. The mouse focuses, drags dividers and scrolls; Ctrl-] then a key splits, opens tabs and closes panes.
- **What worked first time:** typing, focus, splits, divider drags, tab switching, a bell under *needs you* (live, through deltas), `top`, `less` and colors.
- **Drawing is cheap.** At 200x50 with four panes and a full redraw (no dirty rows yet):
  - frame build p99 0.6–0.7 ms;
  - with ratatui's diff and the write, p99 1.0–1.2 ms;
  - one pane flooding at 16 MB/s takes a fifth of a core;
  - 30–36 MB RSS with every pane's scrollback held locally.
- **Four flooding panes fall into a resync loop,** and the web client takes the same path. A client whose queue fills is sent `resync`. It re-attaches past the 1 MB replay window, so it gets a full snapshot of up to 64k rows (about 5 MB), which puts it further behind. 84–90% of the bytes received were snapshots. Resuming from the client's offset didn't help. The fix is #49: the capped and compressed snapshots *Attach and resume* already asks for, and the screen only after a resync.
- **What M31 still needs:**
  - keys read as events and encoded per pane by libghostty's `key::Encoder` (raw bytes break kitty keys and modifyOtherKeys);
  - right-click menus;
  - agent blocks as a transcript with approve and deny;
  - dismiss;
  - synchronized output;
  - bracketed paste.

  Copy mode is the largest piece and is M32.

#### M31: `illogical tui`

**Done 2026-10-02.** `crates/cli/src/tui/` (about 2,500 lines with tests); docs/features.md has the keys and the mouse.

- **Engine:** `crates/vt` gained a client side (`ghostty/view.rs`): `cells()` over the render state (palette colors kept as indexes), the cursor's shape and color, scrollback, and key, mouse, paste and focus encoding through libghostty's encoders for each pane's own modes.
- **Kitty keys:** the daemon used to drop the kitty keyboard reply for everyone (xterm.js can't send those keys). Now `attach` takes `kitty_keys`, and a pane answers while a client that speaks them is attached. With that, Claude Code's Shift+Enter (CSI 13;2u) works in a TUI pane.
- **Protocol:** it attaches as #49 and #52 left things (history 10k, zstd, acks), and holds a pane's drawing while its program is mid-frame (mode 2026, at most 250 ms).
- **Moving a pane** is Alt-drag (or *Move pane…*, then a click): the panes have no title bars to drag by.
- **Measured** (`spikes/s19-tui/bench.sh` with `TUI_BIN`, release, four flooding panes at 200x50): a frame builds in p99 0.86 ms and builds and writes in p99 1.5 ms, using 42% of a core.
- **Tests:**
  - `web/e2e/tui.spec.ts` runs the TUI in tmux beside the browser: splits, typing, renames and closes go both ways;
  - unit tests for key encoding (kitty and legacy), the engine's view, and the agent transcript;
  - a daemon test for the kitty keyboard answer.
- **Not checked as written:** `ssh geek` itself (geek runs no ssh server). The TUI ran in a PTY at 80x24 and 300x80, and against a daemon by URL (`--host`).

What #50 asked for:

- a `cells()` walk in `crates/vt`, shared with the daemon;
- the sidebar with M24's actions;
- keys and mouse through libghostty's encoders;
- the web client's menus;
- agent blocks as transcripts;
- `--host`.

**Done when:**

- Claude Code, Neovim and htop behave as they do in Ghostty;
- the TUI and the web client edit one layout at once;
- an approval is answered from the sidebar;
- `ssh geek illogical tui` works at 80x24 and at 300x80.

#### M32: copy mode in the TUI

**Done 2026-10-02, apart from OSC 52 on real terminals over ssh.** `crates/vt/src/ghostty/copy.rs` and `crates/cli/src/tui/copy.rs`; docs/features.md has the keys.

- **What landed:**
  - **The engine** (`crates/vt`) selects and finds with libghostty's own selection. A selection starts at a tracked point, so it stays on its text as output scrolls it, and grows by cell, word or line (`select_word`, `select_line`). `select_output` takes a command's output between its OSC 133 marks, as Ghostty does. `prompt_rows` lists prompts. `find` searches up or down from a point and wraps; a lower-case needle ignores case, and columns count wide characters as two. `selection_text` formats the selection as plain text, unwrapped and trimmed, as `illogical capture` does. `cells()` marks selected cells, drawn reversed.
  - **The mouse:** drag selects, a double-click selects a word and a triple-click a line; letting go copies. A drag past the pane's edge scrolls it. When the program takes the mouse the drag goes to it, and Shift-drag selects instead.
  - **Copying** writes OSC 52 to the outer terminal after the next frame, and says "Copied N lines".
  - **Copy mode** (`Ctrl-] [`, or the pane's menu): a cursor through the pane's history, starting where the program's is. It has hjkl and arrows, half and whole pages, `0 $ g G`, `v`/`V`, `y`/Enter, `/` `?` `n` `N`, `[` `]` between prompts and `o` for a command's output. The status line turns yellow and lists them.
  - **Scrollback:** the wheel and Shift+PgUp/PgDn (M31) now show a dim `↑N` marker. Typing goes back to the bottom and clears a selection.
  - **Deep search.** A search that misses in the 10k rows the TUI holds reads the pane's output log (`GET /api/panes/N/tail?from=…&until=OFFSET`, up to 32 MiB before what the TUI has). It replays the log into an *archive* terminal (`GhosttyEngine::archive`, 64 MiB of scrollback) with the output that arrived meanwhile, and searches again there. The pane shows the archive, still fed live output, until copy mode ends; then it's dropped. A snapshot (a resync) drops it too, since its offsets no longer follow.
  - `tail` takes `until=OFFSET`.
- **Decisions (2026-10-02):**
  - **The log, not a bigger snapshot,** for deep search. The daemon's own terminal keeps 16 MiB since M9 step 1, which is about 20k rows at 92 columns. A line 30k rows up isn't in any snapshot it can send, but it is in the 256 MiB log. The first draft re-attached with 200k rows of history and couldn't pass the done-when.
  - **An archive beside the live engine, not in place of it.** A log replayed from the middle of a stream, at today's width, can differ from the real screen (modes set before it starts, earlier widths). So the live engine is never replaced. The archive is only read, and only while copy mode is on.
  - **Shift-drag selects when the program takes the mouse.** This is what xterm, Ghostty and iTerm2 do, and #51's wording was ambiguous.
  - **Search runs over plain text, one row at a time.** A match doesn't cross a soft wrap. libghostty's C API has no text search at the pinned commit, and this is fast enough.
- **Measured** (release, 92 columns): 32 MiB of log replays into an archive in 53 ms and keeps 84k rows; a search through all of them that finds nothing takes 38 ms.
- **Tests:**
  - `crates/vt` unit tests: drag, word and line selections (backwards too, soft wraps joined); a selection staying on its text as output scrolls; command output by its marks; find both ways, wrapping, case and wide characters; an archive holding a line 40k rows up that the daemon's engine has lost.
  - `crates/cli` unit tests: OSC 52 and its base64; a pane's archive replaying the log and then what came meanwhile, keeping up, and dropped by a snapshot.
  - `crates/daemon/tests/api.rs`: `tail` with `until`.
  - `web/e2e/tui-copy.spec.ts`: the TUI in tmux with `set-clipboard on`, so OSC 52 lands in tmux's paste buffer. It drives the mouse with SGR reports and checks:
    - a drag across a line break, a double-click and a triple-click copy what they should;
    - a program with mouse reporting gets the click, and Shift-drag still selects;
    - `V` `y` and `v` `l` `y` from the keyboard; the wheel's ↑ marker, gone when you type;
    - `?needle-42` finds a line 40k rows up through the log, the view lands on it, and `y` copies it;
    - `[` `o` `y` on the last command copies exactly what `illogical capture --last-command` prints (spaces inside a line and a blank line kept).
- **Not covered:**
  - OSC 52 into a real clipboard: iTerm2, a phone's terminal, and `ssh geek illogical tui` on the laptop (tmux's buffer stands in). Some terminals cap OSC 52's size or ask first.
  - Output that `capture --last-command` and the screen don't agree on: tabs (the log has a tab, the screen has spaces), trailing spaces a program printed, and output redrawn in place (progress bars). There, `o` copies what the screen shows.
  - The archive replays at the pane's current width, so output from when it was another width is wrapped as it would be now.

### Conversations track (S20, M33, added 2026-10-02)

Every Claude Code conversation on a machine shows up in illogical, whether it ran in a terminal or in the desktop app's Code tab. Any of them can be opened as a block and continued. Both write `~/.claude/projects/<cwd-slug>/<sessionId>.jsonl`; geek has 386 of them. Claude Desktop chats are out of scope (decided 2026-10-02). They live on claude.ai's servers, with no local store or supported API to continue them in.

**Done 2026-10-02:** S20, then M33 (#72). See *M33: as built* below.

**Most of the work is already done.** An agent block whose config holds a `session_id` opens that session when it starts. It uses `session/resume` when it already has a transcript and `session/load` when it doesn't (`crates/daemon/src/agent/mod.rs`, around line 1346). `Status::Stopped` is a block that isn't running until you press *Resume*. So a past conversation is a stopped Claude agent block with that session id and a transcript read from the jsonl. What's missing: an index of the sessions, a jsonl-to-transcript converter, liveness, and the pickers.

**What's on disk (checked 2026-10-02):**

- `~/.claude/projects/*/<id>.jsonl`: one JSON object per line. `user` and `assistant` lines carry `message.content`, plus `cwd`, `gitBranch`, `entrypoint`, `isSidechain` and `parentUuid`. There are also bookkeeping lines (`attachment`, `queue-operation`, `ai-title`, `last-prompt`, `cost-state`, …). In the files touched in the last 30 days, `entrypoint` was `cli` 206 times, `sdk-cli` 103 times and `sdk-ts` 68 times. `sdk-ts` is probably our own agent blocks (`claude-agent-acp` is TypeScript). Which one the desktop app writes isn't known yet.
- `~/.claude/sessions/<pid>.json`: one file per running Claude Code process, with `pid`, `sessionId`, `cwd`, `entrypoint`, `kind`, `status` (idle, …) and `updatedAt`. This tells us which sessions are live and which process owns each one.
- `claude-agent-acp` 0.85.0 implements `session/list`, `session/resume`, `session/load`, `session/close` and `session/fork` (`unstable_forkSession`).

#### S20: conversations spike (about half a day)

Answer these before M33. Each answer goes in as a fixture or a measured number:

1. **The transcript's shape.**
   - Which line types and content blocks occur across the 386 files, and the Claude Code versions that wrote them.
   - How a rewind, an edited prompt or a compaction shows up. If the file holds branches, the conversation is the `parentUuid` chain back from the last leaf, not the lines in file order.
   - Where subagent (Task) runs live: sidechain lines or separate files.
   - Capture redacted fixtures covering Bash, Edit, thinking, a compaction, a subagent, AskUserQuestion and an image.
2. **Continuing a CLI session through the adapter.** Does `session/resume` work on a session the CLI created? The adapter's bundled Claude Code (2.1.280) may be a different version from the CLI that wrote the session. Does the next turn remember the earlier context, and does `claude --resume <id>` in a terminal see the new turns afterwards?
3. **`settingSources`.** Agent blocks pass `[]` so your hooks don't fire inside them (M6b). Find out whether that also drops `CLAUDE.md` and project settings. Someone continuing a terminal session expects those. Find the smallest set that keeps `CLAUDE.md` and leaves out the hooks.
4. **Fork.** Does `session/fork` leave the original jsonl untouched and return a new id that `session/resume` then works on?
5. **Two writers.** What actually goes wrong when a block resumes a session that a terminal still has open, and both write turns. This decides whether fork is the default (the expectation) or only advice.
6. **The desktop app.** Start one session in the desktop app's Code tab on geek. Note its `entrypoint` and `kind`, where its jsonl goes, and whether `~/.claude/sessions` lists it.

**Done 2026-10-02: go** (see [spikes/s20-conversations](spikes/s20-conversations/README.md); fixtures in its `fixtures/`).

- **Shape (1):** 389 sessions and 219 subagent runs (`<id>/subagents/agent-<x>.jsonl` plus `.meta.json`), 931 MiB, all 2.1.x. The index's head-and-tail reads take 9 ms for all of them. The `parentUuid` chain isn't a clean tree: compactions re-link their preserved tail, `away_summary` lines parent the next prompt, and parallel tool calls branch. Walking it drops real exchanges in 18 files. **File order is right:** no result before its call anywhere, and only 5 rewinds in 389 sessions.
- **Resume (2):** a CLI (2.1.288) session resumed through the daemon's adapter (0.85.0, Claude Code 2.1.286) in 0.6–1.0 s with its whole context, and `claude --resume` showed the new turns. A resume resets the model to the adapter's default and writes `/model` command lines into the transcript.
- **Settings (3):** `[]` drops `CLAUDE.md`; `project` loads it and the project's hooks with it. `user,project,local` plus `settings: {disableAllHooks: true}` loads `CLAUDE.md`, skills and permissions, and no hook fires.
- **Fork (4):** 22 ms, leaves the original byte for byte, marks every line `forkedFrom`. It doesn't open the fork: resume the new id before prompting it.
- **Two writers (5):** each sees only its own turns, and a later resume follows the newest `last-prompt` leaf, so the other writer's turns silently drop out. Forking a live session is required.
- **Desktop (6):** the Code tab runs its bundled Claude Code (2.1.275) and writes the same jsonl with `entrypoint: claude-desktop`; `~/.claude/sessions` lists it while open, and it forks like any other. The app also keeps `claude-code-sessions/<account>/<org>/local_<uuid>.json` (`cliSessionId`, title, model, effort, `isArchived`). A session with no folder runs in a scratch workspace the app deletes when the session goes.

#### M33: Claude Code conversations as blocks (#72)

1. **The index (daemon).**
   - Watch `~/.claude/projects` (or `$CLAUDE_CONFIG_DIR/projects`) with inotify. Index each session's id, cwd (from its lines; the directory slug is lossy), git branch, entrypoint, title, first prompt, last activity, message count and size.
   - The title comes from `custom-title` or `agent-name`, else the latest `ai-title`, else the first prompt. `relocated` moves the cwd; `continued-in` and `forkedFrom` link sessions (the picker shows a fork under its original).
   - Read only the start and end of each file, as the SDK's `listSessions` does. Keep the index in the state directory, keyed by path with mtime and size, so a restart only rereads files that changed. The index builds in the background and never delays startup.
   - **Sources:** terminal (`cli`), desktop (`claude-desktop`; title, model and `isArchived` from the app's `local_*.json` by `cliSessionId`, archived ones hidden like missing folders) and other.
   - **Left out:**
     - sessions any agent block of this daemon has ever had (the daemon keeps a set of them, ended blocks included);
     - sidechain and subagent files;
     - sessions with no prompt;
     - sessions whose cwd no longer exists, which is how test daemons' `/tmp/ilg-*` sessions disappear, except desktop ones (their scratch workspace is deleted with the session; *Continue* creates the folder again, empty). *Show all* brings these back.
   - **Liveness:** a session is live while a process in `~/.claude/sessions` holds it and that pid is still alive (`procStart` must equal field 22 of `/proc/<pid>/stat`, so a reused pid doesn't count). `/proc/<pid>/cgroup` then names its scope; `illogical-pane-<N>-….scope` is pane N. If it is, the pane's `PaneInfo` gets the session id, and the conversation says *Live in pane %N*.
   - Each daemon indexes its own machine. M25's fleet view gathers them from every host, the Mac included.
2. **The converter (jsonl to `transcript::Entry`).**
   - **File order**, not the `parentUuid` chain (S20). A prompt whose parent already had a later child is a rewind and gets a `Note` before it.
   - `user` text becomes `User`, with a leading `<system-reminder>` (the desktop app puts one on the first prompt) removed. `assistant` content becomes `Agent` for text, `Thought` for thinking, and `Tool` for a tool_use (name, title, and for Bash the command). A `tool_result` fills in that tool's output and its status (completed, or failed when `is_error` is set). A compaction becomes a `Note` ("Conversation compacted"); its summary line (`isCompactSummary`) is skipped. Slash-command lines (`<command-name>`, `<local-command-stdout>`, `<local-command-caveat>`) become one `Note` (`/model haiku`), and the adapter's own `/model` lines are dropped. Lines of one API response share `message.id`; a subagent call is a `Tool` whose output is the agent's answer (its own transcript stays in `subagents/`).
   - Meta lines, attachments, bookkeeping and unknown types are skipped, so a new Claude Code version shows less rather than breaking.
   - It is pure and synchronous, and is unit tested against S20's fixtures.
3. **Conversations as stopped blocks.**
   - Opening a conversation creates a Claude agent block that is `Stopped`. Its config holds `cwd`, `session_id` and `imported: true`, and its transcript comes from the converter. No process starts, so opening costs only the reading.
   - While the conversation is live elsewhere, the block reads its jsonl again on every change, so you can follow a terminal session from the phone, read-only.
   - The header names the source and where it is live (*Live in pane %4*, *Live in Claude desktop*, *Live in a terminal, pid 1234*). `capture --text`, `history` and `search` work on it like any agent block.
4. **Continue, or fork when it's live.**
   - **Not live:** *Continue* starts `claude-agent-acp` through the existing start path. The block already has a transcript, so it sends `session/resume` with no replay. The block's log begins with the imported entries as one `imported` record, so a restart or reboot restores the block as M6b does.
   - **Live in one of our panes:** *Go to pane* is the main action.
   - **Live anywhere else:** *Fork* calls `session/fork`, then `session/resume` on the new id (fork doesn't open it), and the block's `session_id` becomes the fork's. The original is left alone. Continuing is offered again once that process exits. There's no *Continue anyway*: S20 lost a writer's turns that way.
   - Imported blocks pass `settingSources: ["user","project","local"]` and `settings: {disableAllHooks: true}` (S20 item 3), and set the session's last model (the last assistant line's `message.model`) after resuming, since a resume resets it.
5. **Pickers.**
   - **Web:** a *Conversations* picker grouped by project (cwd). It searches titles and first prompts, filters by live, source and machine, and opens into a new tab or the focused pane.
   - **TUI:** the same picker on a key.
   - **CLI:**
     - `illogical claude ls [--cwd D] [--live] [--json]`;
     - `illogical claude open <id|prefix>` prints the block id;
     - `illogical agent --resume <id>` and `--fork <id>` continue a conversation directly.
   - **MCP:** `list_conversations` and `open_conversation`.

**Tests:**

- unit tests for the converter (S20's fixtures, branches, unknown line types) and the index (incremental rescans, what's left out, liveness with a reused pid);
- a daemon test with a fake `~/.claude` and the fake ACP agent: opening starts no process, *Continue* sends `session/resume` with the id, *Fork* sends `session/fork`, and the block restores after a restart;
- `web/e2e/conversations.spec.ts`: a seeded `~/.claude` shows up in the picker, opens and continues. Dev and test daemons always pass `--socket`.

**Done when:**

- a Claude Code session run in a plain terminal outside illogical and then exited shows up in the picker within 2 s, with its title and folder, and opens with its whole transcript (tool calls and their output) without starting anything;
- *Continue* gets a reply that uses the earlier context, and `claude --resume <id>` in a terminal then shows the new turns;
- a session still open in a terminal shows as live, its block follows new turns, and *Fork* continues it without changing the terminal's jsonl;
- a session in an illogical pane says which pane and jumps there;
- a desktop Code tab session shows up as *desktop* and continues;
- our own agent blocks and test daemons' sessions don't show up;
- geek's 386-plus transcripts index cold in the background, with the time measured, and a restart only rereads files that changed;
- a continued block survives a daemon restart and a reboot like any agent block.

**Not in M33:**

- Claude Desktop chats;
- cloud sessions (claude.ai/code);
- full-text search across transcripts that aren't open;
- Codex and other agents' histories (the same shape would fit, behind the index's source).

#### M33: as built

**Done 2026-10-02.** `crates/daemon/src/conversations/` (the index, liveness and the converter), the agent block's `import` and `fork` (`crates/daemon/src/agent/mod.rs`), `/api/conversations` and `/api/conversations/open`, `illogical claude ls|open` and `agent --resume|--fork`, MCP `list_conversations` and `open_conversation`, `web/src/ui/conversations.tsx` and the block's Continue/Fork, and Ctrl-] `C` in the TUI. docs/features.md has how it's used.

- **The index** is read when asked (the picker, `ls`, MCP), not watched: it stats every transcript and reads the two ends of the ones whose size or mtime changed, so a new or finished session is there on the next listing. A cold listing of geek's 387 took 0.6 s; later ones only reread what changed.
- **Decisions (2026-10-02):**
  - **In memory, not in the state directory.** A cold scan is under a second, and a cached index could go stale against files Claude Code rewrites. A restart rereads every transcript's ends once.
  - **No message count.** It would mean reading whole transcripts (931 MiB on geek); the list shows size, last activity and the prompts instead.
  - **Liveness ignores pane and block ids that aren't this daemon's.** A dev or test daemon's processes run in scopes named like ours; without the check, the e2e run saw a conversation "open in pane %76" of the daily daemon.
  - **`kill(pid, 0)` failing with EPERM counts as alive.** Inside a sandbox it can't signal the user's own processes; only ESRCH means gone.
  - **The imported entries go in `imported.json`** in the block's directory, named by a note in its log, not into the log itself: a long transcript would be one huge log line, and the jsonl can be deleted later (Claude Code's cleanup).
  - **Continuing checks for a holder again first.** It reads `~/.claude/sessions` at that moment, not the 1-second follow, so a session opened elsewhere a moment ago is refused.
- **Tests:**
  - unit tests for the converter (S20's fixtures: Bash, Edit, AskUserQuestion, parallel calls, a subagent, a compaction, an image, an API error, a rewind, slash commands, the desktop app's reminder, unknown lines) and the index (titles by precedence, forks, sources, folders gone, archived, both ends of a long file, a reused pid, model names);
  - `crates/daemon/tests/conversations.rs`, with a seeded `$CLAUDE_CONFIG_DIR` and the fake agent as Claude Code's adapter (`$ILLOGICAL_AGENTS_DIR`): listing and filters, opening by a prefix with no process, the same block twice, following a growing transcript, continuing (the fake reads the transcript as Claude Code would), the `_meta` and model it resumed with, a restart; and a held session refused, then forked, the original byte for byte;
  - under systemd (#82, added 2026-10-03): a fake Claude Code detached from its pane's processes (as under tmux) is placed in the pane by its scope alone, *open in pane %N*, and the opened block's `held` names that pane; once it exits, the block continues it and the list credits the block's own agent (in its `illogical-agent-<id>-…` scope);
  - `web/e2e/conversations.spec.ts`: the pane menu's picker, search, a block beside the pane, Continue by sending, picking it again goes to the block; a held one with Continue disabled, forked and continued; the phone's sheet. The e2e run now has a Claude directory of its own and the fake agent as Claude Code's adapter for every spec;
  - the TUI's drawing of an opened conversation.
- **Against the real thing** (a dev daemon on geek, `claude-agent-acp` 0.85.0): S20's terminal session opened with its tool calls and output, continued with its context (it named the courier), and the new turn is in the session's jsonl for `claude --resume`; the desktop app's live "Hello" session was refused with "it's open in the Claude desktop app" and forked.
- **Not covered:**
  - jumping to a pane that runs the conversation in the web client: the daemon tests (#81, #82) cover what the list and block say, not the picker's or block's *Go to pane* click;
  - the Mac (`~/Library/Application Support/Claude` for the desktop app's records). #81 (2026-10-03): liveness there reads `procStart` as Claude Code writes it on macOS (`LC_ALL=C TZ=UTC ps -o lstart=`, to the second) against the process's start time from libproc, and a holder is placed in a pane or agent block by its parent processes (the daemon's panes' and agents' pids, before the systemd scope, on both OSes). jake-mini's desktop app (1.7196.0) had never run a Code tab session, so its record folder and the scratch-workspace deletion are still unchecked there;
  - inotify: a listing finds new sessions, but an open picker doesn't update by itself.
- **The remembered branch (#79, 2026-10-03):** `conversations/branch.rs` walks back from the leaf Claude Code resumes from, as Claude Code 2.1.288 does (read from its bundle): the newest `last-prompt.leafUuid` unless the last line goes on from it, a missing parent bridged to the nearest line up to 5 s older, a preserved compaction segment re-linked under its summary. Compactions are bridged through `logicalParentUuid` (the summary holds what came before). Entries off it get `forgotten`, folded under a note in the web client and dimmed in the TUI.
  - **Checked against real resumes** (`claude -p --resume` on copies in a throwaway `CLAUDE_CONFIG_DIR`): S20's scratch session answered "UNKNOWN, teal" (the vault code is marked); cut before the CLI's last `last-prompt`, "2468, UNKNOWN" (the lighthouse is marked); a real rewind (`--resume-session-at`) "APPLE, blue" (red is marked).
  - **Side lines aren't bridged:** an `away_summary` written under an older line cuts off the exchange after it, and a resumed copy of such a session didn't know that prompt. On geek, 17 of 436 sessions have something marked (134 of 80,826 lines): rewinds and edited prompts, prompts sent while a tool ran, the scratch session's two writers, one away summary, and one session restarted from a parentless line. Marking all 436 takes 0.4 s in a debug build.

**Every host's (#78, 2026-10-03).** The web picker asks every host in the fleet (M25) at once, through `fleet.request`, and shows each under its name as it answers, then by folder. A host gets 5 s; an asleep sandbox isn't asked (`asleep`), a capped one says so, and one that doesn't answer shows `not answering` while the rest stay usable. Picking another host's conversation sends `POST /api/conversations/open` to that host (its first session, a new tab), then `fleet.open` shows the block there. `illogical claude ls --host all` does the same per host, printing each as it answers (`hosts::each`, a thread per host, 5 s). MCP's `list_conversations` stays this machine's: the daemon never talks to other hosts for a client (clients do, M4a), so there is no fleet to ask from inside it.

- **Tests:** `web/e2e/fleet-conversations.spec.ts`: three daemons, each with its own Claude directory; the picker grouped by host, search across them, one of jake-mini's opened and continued on jake-mini (its adapter read jake-mini's transcript), picked again from geek goes to that block; a stopped host (SIGSTOP) holds nothing up (the others in 124–164 ms, it gives up at 5.4 s); the phone's sheet.
- **Against the real fleet:** `illogical claude ls --host all` from geek listed geek's and jake-mini's in 0.15 s.
- **For Jake:** the phone with jake-mini and geek: a conversation from jake-mini's terminal and one from geek's desktop app in one list, each opened and continued on its own host.

#### #77 and #80: as built

**Done 2026-10-03.**

- **#77: the block's holder is only ever this daemon's pane.** The block's 1-second follow kept the pane id from the holder's systemd scope as it was, so a scope from another daemon on the machine showed as "open in pane %76". The multiplexer now shares its pane and block ids with blocks, and the list and the block both filter through `Live::ours`; anything else is "open in a terminal (pid N)".
- **#80: following converts only what was appended.** `convert::Follow` keeps the converter's state (the entries, open tool calls, the rewind set) and the offset of the last whole line, and reads from there. It converts again from the start when the file is shorter than what was read, is another file (device and inode), or its last 4 KiB before that offset differ. A last line without its newline is converted on a copy until the newline comes, so the entries always match converting the whole file.
- **Measured on geek** (release build, `follow_cost` in `convert.rs`; the transcript's last 100 lines appended to a copy one at a time):

  | transcript | entries | whole file, each change (before) | an appended line (after) |
  |---|---|---|---|
  | 30.6 MiB (hud demo session) | 744 | 39 ms, 31 MiB peak | 0.21 ms median, 1.1 ms max (a 762 KiB line); under 0.5 MiB peak |
  | 23.6 MiB (ravix session) | 1,897 | 55 ms, 25 MiB peak | 0.43 ms median, 0.9 ms max; no measurable peak |

  The first read when a block opens costs what a whole conversion did. What's left per change is mostly copying the entries into the block's transcript, which grows with the entries, not the bytes (744 entries from 30 MiB).
  With #79 merged, each read also walks the whole chain again to mark what a resume wouldn't follow: on the 30.6 MiB transcript an appended line went to 1.3 ms median (2.1 ms max), against 31–47 ms for the whole file. Still no measurable memory.
- **Tests:** following gives exactly what a whole conversion gives, for S20's fixtures appended a line at a time and in pieces that split lines (finished and not); a rewind whose earlier prompt was read before its new one; a file truncated, rewritten in place at the same length, replaced by a rename, and emptied.
- **Not covered:** a rewrite in place that leaves the file at least as long and the 4 KiB before the offset as they were, but changes something earlier. Claude Code only appends.

### Workspaces track (S21, M34, added 2026-10-02)

A [chant](https://intentius.io/chant) workspace (a repo with a `chant.workspace.json`, such as `~/dev/intentius/chant`) as a block you work in. Its members are cards you open shells, agents and diffs on. Its records show with their state. A gate waiting in any member is illogical attention you can approve. illogical reads the workspace only through chant's read contract (chant `ws-017`), as one more reader beside hud and behold. Review actions on records stay hud's (`ws-052`).

**Order:** S21 (#70), done below; then M34 (#73), with #74 (the shell environment) first or alongside, and #75 (approve as owner or editor) inside it. #76 holds drafts for chant that Jake files himself.

#### S21: chant workspace as blocks spike

**Done 2026-10-02: go** (see [spikes/s21-chant-workspace](spikes/s21-chant-workspace/README.md); the throwaway block is on branch `s21-workspace-block`).

- **Read contract alone is enough.**
  - `workspace ls`, `check --format json`, `records --current` and `status <env>` (all `--json`) give members, findings per member, records with `blockedBy` and drift, releases, and **each member's pending gates with chant's approve command**.
  - No chant change is needed. `graph` runs only kind-`chant` members (24 of chant's 25 are `skipped`), and `lineage` needs a lock file, so neither is used.
- **It works end to end on a dev daemon.** A gated op shows as `needs_input` ("delivery: ship waits at gate approve-ship") about 1 s after `chant run` exits. *Approve* runs `chant approve` in the member, and the attention clears. *Shell* opens a pane in the member, and a nested workspace opens as a second block.
- **Cost:**
  - A full read takes 1.2–1.5 s wall and **about 7.5 CPU-s** (four chant processes, each loading TypeScript through tsx; 285 MB peak).
  - So, while drawn, the block checks a git fingerprint every 3 s (HEAD, `chant/lifecycle`, `status --porcelain`, `diff HEAD`; about 0.1 CPU-s), and reads in full only when it changes. That's 0.4% of a core when idle.
- **The daemon has no node.** mise sets it up in `.bashrc`, which the daemon's `sh -c` never reads, so the spike takes PATH from `$SHELL -ic`. #74 makes that a cached per-host shell environment.
- **Cards, not member blocks.** chant's 25 members as blocks would be 25 tiles of mostly lexicons. Cards launch real blocks in a member when you work on it.
- **Approve:** chant records `resolvedBy` as the host's user. Decided 2026-10-02: the owner and editors may approve, each as themselves (`--approver`), and view-only guests may not (#75).
- **For chant (#76):**
  - a nested workspace's runs write gates under the outer prefix, so its own `status` never shows them;
  - chant's declaration names no record kinds;
  - gates need an env;
  - one process for a workspace read would cut the cost about 4×;
  - `--json` is inconsistent.

#### M34: chant workspace blocks (#73)

`BlockType::Workspace` as S21 built it, finished:

- the reads through the workspace's own chant;
- fingerprint freshness;
- gates as attention, with *Approve* for the owner and editors (#75);
- *Run op* in a pane;
- *Shell*, *Agent* and *Changes* on a member;
- nested workspaces as blocks;
- gate attention written against a gate type with a `source`, not against chant, so M35's gates from hud in a studio box use the same reason, card and sheet (added from S22);
- `illogical workspace [DIR]`;
- *Open as workspace* in a pane's menu and the picker when a directory holds a `chant.workspace.json`;
- an MCP `open_workspace`;
- web cards, the phone sheet (gates first) and a TUI line.

**Tests:** the composer's fixtures, daemon tests for gate attention and approve, and an e2e spec with a toy gated op (CI needs node and a pinned chant).

**Done when:** on geek, `illogical workspace ~/dev/intentius/chant` (after `npm install`) shows its members, and a gated op shows as attention within about 5 s. The phone can approve it, and the next `chant run` walks through.

#### #74 and M34: as built

**Done 2026-10-02 (#74 merged first, then M34; tracker #86).**

- **#74:**
  - `crates/daemon/src/shellenv.rs` runs the login shell once per host (`$SHELL -l -i -c`, `env -0` between sentinels, 10 s timeout). It starts in the background when the daemon starts, and the first block that needs it waits.
  - Blocks opt in with `Runner::user(ctx)`. A VM resolves once per machine through its provider.
  - `illogical shell-env [--refresh]` and `/api/hosts/self/shell-env[/refresh]` (owner only).
  - On geek, a cold resolve took **236 ms** and a refresh 182–190 ms. A dev daemon started with `PATH=/usr/bin:/bin` found mise's node.
- **M34:**
  - `Gate` and `GateSource` live in `crates/proto`; approval is dispatched on the source in `crates/daemon/src/gate.rs`. The reader and composer are in `crates/daemon/src/workspace/`.
  - A gate is its own reason kind (`gate`, bundled as `gate:<root>`). The web card, the swarm rail, the phone sheet (gates first) and the TUI line are all built from `Gate`.
  - *Approve* runs `chant approve <op> <gate> [--env] --approver <name>` in the member. Guests get 403 (#75). The name is the caller's: the owner's illogical login, not the OS user. Approvals are logged in the block's log, `illogical history` and the audit log.
- **Decisions (2026-10-02):**
  - **A fingerprint every 5 s while nothing draws the block** (this host only), as well as every 3 s while drawn. Otherwise a gate reached with no tab open would never reach push or the swarm. It costs about 0.24 CPU-s a minute.
  - **Only gates raise attention.** Errors and drift show in the block's headline.
- **Against the real thing** (a dev daemon on geek, chant 0.87.0, the daemon's own PATH without node):
  - `illogical workspace ~/dev/intentius/chant` showed 25 members in 1.6 s, through the cached shell environment with no per-call fallback.
  - A toy gated op in a top-level member of a scratch clone of chant (Pixel 7 emulation drawing the session; `chant run` exits 3):
    - the gate reached the phone's sheet in **0.8–1.8 s** over three runs (5.3 s once);
    - *Approve* on the phone cleared it in the daemon in **2.6–2.8 s**;
    - the next `chant run` walked through every time;
    - chant's ledger says `resolvedBy: jhgaylor@gmail.com`.
  - A full read of chant costs 11.3 CPU-s and 2.4 s wall (S21 measured about 7.5). Idle while drawn, 0.8 CPU-s a minute.
- **Found on the way:**
  - **chant pushes `chant/lifecycle` to `origin`** after a run or approve. A scratch clone of a local checkout therefore writes gate records into that checkout. Remove the clone's remote first.
  - **A run inside a nested workspace** writes its gates under the outer prefix (`_members/reference-workspace/_members/delivery/_gates`), and neither the outer nor the inner `status` shows them (#76).
  - **A gate is per op and gate name, and an approval lasts.** Re-running an approved op walks through, so a test needs a fresh op name each time.
  - **The phone hides a gate's card when it's tapped,** and the daemon clears it once chant has written the approval, about 2.7 s later.
- **Not covered:**
  - a real phone (only Pixel 7 emulation; editor and viewer on the phone are in `workspace.spec.ts`);
  - a workspace on a VM tab (the path exists, untried);
  - jake-mini's resolve time.

### Studio apps track (S22, added 2026-10-02)

arugula-salad's studio makes a box per app on wisp: the app, hud's proxy and panel in front of it, a door, and a steward that runs releases. The idea is that the box shows up in illogical as a block beside its terminals and agents, and whatever it waits on reaches the swarm's needs-you rail. That covers hud's agent asking a question now. Release gates are chant's (`chant approve release ship`), so they come with M34's workspace attention.

#### S22: studio apps spike

**Done 2026-10-02: go** (see [spikes/s22-apps](spikes/s22-apps/README.md)), against a real studio box cloned from `arugula-box-template` on geek's wispd.

- **Framing:** a block's frame is always third party (illogical's page is on `*.ts.net`, the box on its own site), and hud's `SameSite=Lax` session cookie is refused there: 401 in Chrome and Firefox, in illogical's own client too. The same cookie as `SameSite=None; Secure; Partitioned` works in both (200), including Chrome with third-party cookies blocked, and Chrome keys it to illogical's site. A plain web-page block (`illogical open <entry link>`) is enough: the box is already its own origin. With third-party cookies blocked the frame's `localStorage` is still refused (#69). WebKit wasn't run.
- **Questions:** hud puts a waiting question in every `hud-chat-queue` frame, and a follower of `/__hud/api/chat/stream` turned each one into an M24 `ask` 11 ms after hud asked. That puts it on the pane's card, the swarm's rail, push and `illogical attention`. Answered on the swarm's rail, it reached hud (`/__hud/api/chat/answer`) in 48–74 ms and the agent's turn went on. Answered in hud's own panel, the card was withdrawn 33 ms later.
- **What it needs:** studio's door hands out the partitioned cookie; `Api::Ask` takes browser blocks (it's terminal-only, so the spike's follower raised the card on a terminal beside the app, and the swarm filed it under the wrong project); the follower moves into the daemon as an app block; illogical gets a studio token to list apps and mint entry links (passkey-only today); and hud learns who answered in illogical (it records the follower's own player).
- **Found on the way (arugula-salad):** hud's panel never mounts on the template's page, framed or not (the injected client looks for `<body>` from `<head>`). A clone of the box template on geek gets `widgets.wtf` while the template's `~/box/domain` says `studio.arugula.io`.

**Order:** S22, done; then M35 (#85), after M34 (#73) and #75.

#### M35: studio app blocks (#85)

A studio box as a block: the app in the frame (its own origin, so no block site), its agent's questions as asks on the block, and its chant gates as M34's gate attention. A box's repo is a chant workspace, but illogical reads it through hud in the box, never by running chant there. illogical can't run commands in a hosted box, and hud already pays for the reads.

- entry links minted from a studio token each time, never kept; studio's door hands out partitioned cookies (both studio's to build);
- S22's follower in the daemon: hud's chat queue → asks on the block (`Api::Ask` on browser blocks), answers back, withdraws;
- gates from hud's work board, refreshed by its live feed. Approving a gate hud didn't start has no hud route yet (open, #85);
- who answered: the person who clicked, in hud and in chant's ledger (#75's rule);
- records linked into hud's pages, never reviewed here (`ws-052`);
- kind `app` on the swarm, `illogical app [NAME]`, *Open a studio app…*, MCP `open_app`.

**Done when:** a real studio box opens from the picker on geek; its agent's question shows on the rail and the phone and is answered from illogical with hud naming who; a release waiting at `ship` is attention you can approve; and the block comes back after a daemon restart without a stored link.

#### M35: as built

**Done 2026-10-02 (tracker #86).**

- **The block and the follower:**
  - The block is `crates/daemon/src/apps/` (`mod.rs` the block, `studio.rs` studio's client, `hud.rs` S22's follower) and `web/src/blocks/app.tsx`.
  - Config `{box_url, app, studio, title?, follower?}`. The owner-only `enter {to?}` method mints a fresh `/__enter` link that the frame navigates to after its first load. The link is never in the frame's `src`, the config, the log or storage.
  - `Api::Ask` takes browser and app blocks. An ask carries `source` and `agent` (the card says "hud asks"), and the bundle and project come from the block.
  - The follower holds its own cookie jar from a minted link, kept in memory only and re-minted on a 401. It follows each of hud's tabs with backoff and posts answers with the box's own `Origin`.
  - Gates come from hud's work board, re-read when hud's live feed moves, as M34's `Gate{source: GateSource::Hud}`. *Approve* goes to hud's new route.
- **Studio:**
  - `illogical studio login|logout|follower APP` keeps a personal token (studio#292) and any follower links in `studio.json`, mode 0600, never sent to a client.
  - `illogical app [NAME]`, *Open a studio app…* (pane menu, + menu, phone sheet), MCP `open_app`, swarm kind `app`.
- **Decisions (2026-10-02):**
  - **Who answered is hud's trusted follower (hud#736), not a player link per person.**
    - The box's owner runs `hud share --role follower` and keeps the link with `illogical studio follower APP`. Every block of that app then uses it.
    - Answers and approvals then carry `onBehalfOf: {name, via: "illogical"}`, with illogical's name made to fit hud's display-name rules (an email becomes its local part, at most 32 characters, never a role label).
    - Without a follower link, hud records the box's owner.
  - **The frame enters as the owner**, never with the follower's credential. Guests can't enter the frame.
- **arugula-salad PRs it needs** (open, for Jake): hud#734 (refuse cross-site writes, needed before cookies are `None`), hud#735 (approve any pending gate), hud#736 (trusted follower), studio#291 (partitioned cookies at the door), studio#292 (personal tokens).
- **Against the real thing** (a dev daemon on geek; box `ilg-m35-a` cloned from `arugula-box-template` with hud and the door built from those PRs; a throwaway studio from #292 on loopback; stub Anthropic in the box, as S22):
  - **From the picker:** the block opened in 0.3 s, and the box was framed (cross-site, 200) and followed within 0.4–1.7 s.
  - **A question:** the agent's question reached the daemon, the swarm's rail and the phone's sheet 2.2–3.8 s after the prompt. That includes the stub's turn; the follower's own lag was 13–16 ms.
  - **An answer:** answered on the block, the card cleared in 34–39 ms and hud had it in 10–23 ms. The turn went on, and hud's tool result says "jhgaylor answered through illogical".
  - **A release at `ship`:** `npm run release` in the box stopped there, the gate showed as attention, and *Approve* on the phone (Pixel 7 emulation) cleared it in 7–8 s. chant's ledger says `resolvedBy: "jhgaylor"`.
    - The time from the release exiting to attention was 32 s, because the 2 GB box stopped answering HTTP for minutes while the release built (load 16, with the box's own steward running chant too). The gate showed 2.9 s after the follower got back in.
  - **A daemon restart:** the block came back and was following in 139 ms, framed with a fresh link. The state directory holds no entry link.
- **Found on the way:**
  - **The follower link wasn't used from the picker.** A block opened there passed no `follower`, so hud recorded the owner. Fixed: a kept link is the default.
  - **The IDE tests read the fake's file before it was written.** This showed up on macOS CI. They now wait for the file.
  - **A studio box under a release build can't serve HTTP for minutes.** The follower retries with backoff and catches up when the box answers.
  - **Studio's routes only take names like `app-xxxxxxxx`,** so the throwaway studio ran with that check relaxed.
  - **The box can't reach a studio on the host** (wisp's firewall), so the box was enrolled from the host side.
- **Not covered:**
  - Jake's real studio (it needs studio#291/#292 and hud#734–#736 deployed, and a token from it);
  - a real phone;
  - a second device;
  - WebKit;
  - planter's boxes on Fountain.

#### M35 follow-up: prompting a box's agent (added 2026-10-03)

Jake asked whether illogical could prompt a studio app's hud agent. It couldn't: the follower read tabs and streams, answered questions and approved gates, but never started a turn. Now an app block's `send` (`{text, tab?}`) goes to hud's `POST /__hud/api/chat/prompt {chatKey, text}` through the follower's session, in the box's first tab or the one named (title or chat key). MCP's `send_input` passes `tab`; the CLI is `illogical call %N send '{…}'`, as for agent blocks.
- **Who:** owner and editors, the same as answering (any block call). hud takes no `onBehalfOf` on prompts, so its chat shows the session's player (the owner, or the follower). The block's log and `illogical history` say who sent what to which tab. If hud should name the person, that's a hud change: `onBehalfOf` on `/api/chat/prompt` for a follower, as hud#733 did for answers.
- **Refusals** come back as hud gives them: unknown tab (404), turn budget or full queue (429), chat unavailable (503), a spectator (403).
- **Against a real hud (2026-10-03):**
  - a throwaway `ilg-prompt-a` cloned from `arugula-box-template` (hud 0a4cf0e), with S22's stub Anthropic as its model;
  - a dev daemon from this branch, entering with a hud follower link and a fake studio that only lists the app.
  - MCP `send_input` → hud took it in `main` (position 0); `wait` (needs_input) showed the agent's question; `agent_respond` answered it.
  - A second prompt (`tab: "MAIN"`) queued 1 behind the running turn. `tab: "nope"` was refused, listing `main`.
  - `illogical history --pane` shows both prompts `by mcp:live-check`.
  - Box, daemon and fake studio removed afterwards.
- **Not covered:** Jake's real studio (its own entry links rather than a follower link).

### Forge track (S23, M36–M40, added 2026-10-02)

Pull requests and issues from a git forge (Forgejo, then GitHub, then GitLab) as blocks. A PR is a block beside the terminals, agents and diffs that work on it, and whatever it waits on reaches the needs-you rail: a review asked of you, CI red on your PR, changes requested. An issue is where an agent's work starts. The point isn't drawing a forge's pages again; it's attention, `capture --text` for agents, and opening the blocks we already have (diff, terminal, agent) on the PR's code.

**Decisions (2026-10-02):**

- **One block type, `forge`,** with the provider in its config, not a type per forge. Under it, a normalized model (item, review, check, timeline event) and one adapter per provider.
- **Forgejo first.** Our tracks run there, it's on the tailnet, and its API is close to GitHub's. GitHub follows behind the same adapter; GitLab (merge requests by `iid`, pipelines, discussions) is last.
- **Auth through the user's own CLIs:** `tea` (or the Forgejo API with `tea`'s login), `gh api`, `glab api`, run with #74's per-host shell environment. illogical stores no forge tokens. A host with no logged-in CLI says so on the block. Hosted boxes (no CLI login) wait for the control plane's GitHub App (M40).
- **Freshness: poll now, webhooks later.** Conditional requests (ETag / `If-None-Match`; a GitHub 304 doesn't count against the rate limit), every few seconds while drawn or while the item has attention, else every few minutes, like M11's "drawn" rule. Webhooks are M40.
- **Agents draft, people send.** Read methods are open to agents (MCP, `illogical call`). Write methods (`comment`, `approve`, `request_changes`, `merge`, `close`) called by an agent become an ask on the block holding the draft; the owner or an editor sends, edits or drops it, and it goes out as that person through their CLI. A person in the UI or CLI sends directly. This is the general form of "never file on ghostty-org as an agent": nothing reaches a forge in an agent's name.

**Order:** S23 (#87); then M36 (#88, Forgejo PR blocks), after M34's gate `source` and M35's `Api::Ask` on non-terminal blocks; then M37 (issues, issue → agent); M38 (GitHub); M39 (GitLab); M40 (webhooks and hosted). M38 can start once M36's model is settled; it doesn't wait for M37.

#### S23: forge blocks spike (#87, about half a day)

**Done 2026-10-02: go** (see [spikes/s23-forge](spikes/s23-forge/README.md)). Tried against this repo's Forgejo (#84), Codeberg's `forgejo/forgejo` (forks, red checks, team requests), GitHub's `cli/cli` and Jake's own open PRs, and gitlab.com's `gitlab-org/cli`. Read-only throughout.

- **Read path:** `tea api`, `gh api` and `glab api` all reach the PR, reviews, checks, timeline, files and diff refs. Both CLIs that were run pass `If-None-Match` through (`gh` exits 1 on a 304).
  - The daemon should take the token from the CLI and make the requests itself with its existing `reqwest`. Tokens: `tea login helper get` (16 ms, refreshes OAuth) and `gh auth token` (39 ms). Keep them in memory only.
  - A CLI per request costs ~25 ms (`tea`) to ~150 ms (`gh`) more, and each prints headers its own way.
- **Logins:** `tea` matches an `ssh://` remote to a login by `ssh_host`. This repo's `git.tail1234.ts.net` matches no login: the login's `ssh_host` is `git.inevitable.fyi`, and the tailnet name serves only SSH.
  - The daemon matches the remote's host against each login's URL host and `ssh_host`. If none matches, it asks each Forgejo login for `repos/{path}` and compares `ssh_url` (one request; it finds `forgejo` here).
  - The block offers a choice when no login matches, or several do. The pick is kept in config.
- **Cost:** a full read is 7 requests on GitHub (1.4–1.6 s, 7 points) and 6 on Forgejo (0.3–0.8 s, 29 KB).
  - An unchanged GitHub poll is all 304s for **0 points**. GraphQL is one request (1.3–1.8 s) but costs a point on every poll, with no ETag, so M38 stays on REST.
  - Forgejo sends no ETags and no rate-limit headers, so its poll is the item plus the combined status (2 requests, ~100–200 ms). It re-reads the rest when `updated_at`, the head sha or the status changes.
  - gitlab.com's 304s still count against its rate limit.
- **Model:** `Item` / `Review` / `Check` / `Event` (Rust fields in the README) fit 9 PRs across the three forges (`model.mjs`). What doesn't fit:
  - Forgejo's `requested_reviewers` keeps reviewers who already reviewed. A request is pending only while that reviewer's latest review entry is `REQUEST_REVIEW`.
  - GitHub's combined status says `pending` when it has no statuses. Check runs and statuses both feed checks.
  - Forgejo has no rerun API.
  - GitLab's head pipeline runs on a merge commit. Its discussions need auth even on public projects.
  - Only GitHub says whether branch protection blocks a merge.
- **Attention:** each rule fired on real data. `ask`: a request direct and through a team. `failed`: **Jake's studio#291**, 3 checks red. `input`: changes requested, and a mention (GitHub's `mentioned` event, or `@login` in a body, newer than `seen_at`). `done`: merged, or green and not blocked (**studio#292, hud#736**). "You" is `GET /user`; teams come from `GET /user/teams`.
- **Code:** a treeless fetch of `refs/pull/N/head` or `refs/merge-requests/N/head` worked on all three forges, from forks too (0.2–4.3 s). `git diff $(git merge-base target head) head` matched the forge's file list every time.
  - GitLab's `diff_refs.start_sha` is the target's tip, not the merge base: diffing from it listed 130 files instead of 4.
  - M11's diff block takes `rev_a = merge_base`, `rev_b = refs/illogical/pr/N` as it is.
- **Drafts as asks:** M35's `Mux::ask` already takes any block that isn't an agent or remote block. Ask → posted wasn't measured, since nothing was posted. What M36 changes:
  - The block queues its drafts, because the mux holds one ask per pane and a second withdraws the first. Each draft is a `Form` ask (`body`, markdown) with `source: forge`.
  - The PR writes are named `review {event}` and `comment`, because `approve` on a block holding an ask is routed to the card.
  - It resolves `by` for its writes: MCP is `mcp:<client>`, so a draft. The CLI sends `agent: true` under `CLAUDECODE` or `AI_AGENT`, a courtesy, not a boundary.
  - New MCP tools `open_pr`, `read_pr` and `pr_*` return a draft id at once.
  - The web gets a textarea for a markdown field.
  - **Decided 2026-10-02 (Jake): the owner and editors may send.** Sending posts with the owner's CLI login (the spike leaned owner only, as M35's `enter`), so the block's log and the card record which person sent each draft; viewers can't.

**Order:** S23, done; then M36 (#88).

Answer these before M36. Each answer goes in as a fixture or a measured number:

1. **The CLIs as the read path.**
   - Do `tea`, `gh` and `glab` cover what the block reads (PR, reviews, checks or statuses, timeline, diff refs) through their `api` passthroughs, with their own logins?
   - How does each pick a login for a repo? `tea` here says "no login matched this repository" for `ssh://git@git.tail1234.ts.net/...` and falls back to login `forgejo`. Find the mapping rule (host name, SSH vs HTTPS remote) and what the block shows when it's ambiguous.
   - Do conditional requests (ETag, 304) go through each CLI's passthrough, or does the daemon need the token (`gh auth token`, `tea`'s config) and its own HTTP client?
2. **Cost.** One full read of a PR (item, reviews, checks, timeline) on Forgejo and GitHub: wall time, requests, and rate-limit use. The same for an unchanged poll. Use real PRs (this repo on Forgejo; a public GitHub repo with Actions).
3. **The normalized model.** Map one PR on each of Forgejo, GitHub and GitLab (gitlab.com, a public project) onto item / review / check / timeline event. List what doesn't fit: GitHub's check runs vs commit statuses, Forgejo Actions' statuses, GitLab pipelines and approvals, draft vs `WIP:`, review threads vs discussions.
4. **Attention rules.** From real data, which states make `needs_input` for *you*: a review requested from you (direct or through a team), your PR's checks failing, changes requested on your PR, a mention, CI going green on a PR you're waiting to merge (`done`). Which login is "you" on each forge.
5. **The PR's code.** `git fetch` of a PR head on each forge (`refs/pull/N/head` on GitHub and Forgejo, `refs/merge-requests/N/head` on GitLab), into a worktree, from a fork. Does M11's diff block on `merge-base..head` match the forge's own diff?
6. **Drafts as asks.** A throwaway agent calls `comment` through MCP. The draft shows as an ask on a terminal beside it, as in S22, and *Send* posts it as the person through `tea`. Measure ask to posted.

#### M36: Forgejo pull request blocks (#88)

`BlockType::Forge` with `{provider: forgejo, host, repo, kind: pr, number}`:

- **Reads** through the provider adapter (S23's read path), polled per the freshness rule. State: title, body, author, labels, draft, base and head, mergeable, reviews, checks, and the newest timeline events. `describe %N` returns it and changes go out as events.
- **The log:** the timeline as an event stream in `blocks/%N/`, so `history` and `search` find a PR's comments and check runs beside the terminal output of the work.
- **Attention** through M34's gate `source` (`forge`) and M24's reasons: `ask` for a review requested from you; `failed` for red checks on your PR, with *Rerun checks*; `input` for changes requested or a mention; `done` when your PR is merged or its checks go green. Bundled by repo.
- **Methods:**
  - `comment {body}`, `review {event: approve|request_changes|comment, body?}`, `merge {style?}`, `rerun_checks` (GitHub; Forgejo has no rerun API, so it links the run), `refresh`. Not `approve`: on a block holding an ask that name answers the card (S23). Writes follow "agents draft, people send": an agent's call becomes an ask (M35's `Api::Ask` on non-terminal blocks), and a person's goes out as them. Owner and editors only (M12); viewers read.
  - `diff`: an M11 diff block beside it on `merge-base..head`, on a worktree of the PR head.
  - `checkout`: a terminal block in that worktree (`.illogical/worktrees/pr-N`, or the repo's own convention if it has one), on the block's host.
- **`capture --text`:** the PR as text (header, body, reviews, checks, the timeline), for agents.
- **Ways in:** `illogical pr [URL | REPO#N | N]` (N in the current repo), *Open pull request…* in the picker, a PR link in a terminal (OSC 8 or a plain URL on a known forge host), MCP `open_pr`, and the swarm (kind `pr`, filed under the repo's project).
- **Web:** `web/src/blocks/forge.tsx`: header, checks, reviews, the timeline, and the draft asks. The phone's sheet puts what waits on you first. A TUI line.

**Tests:** adapter unit tests on S23's fixtures (every review and check state); daemon tests for attention, drafts as asks and the viewer's refusal, against a fake Forgejo (recorded responses, ETags included); an e2e spec on a Forgejo in CI if one is cheap to run, else the fake.

**Done when:** on geek, a real PR on this repo's Forgejo opens as a block; a review requested from Jake is on the rail and the phone within about a minute of being asked (polling), and approved from the phone shows on Forgejo as Jake; red checks show as *Failed* with *Rerun checks*; and an agent's `comment` waits as a draft until Jake sends it.

#### M36: as built

**Done 2026-10-02 (merged as `e01f369`; the real-write "done when" is Jake's, below).** `crates/daemon/src/forge/` (`mod.rs` the block, `model.rs` S23's normalized model and the attention rules, `forgejo.rs` the adapter, `login.rs` tea logins and tokens), `illogical pr` (and `pr comment|review|merge`), MCP `open_pr`, `read_pr`, `pr_comment`, `pr_review`, `pr_merge`, `web/src/blocks/forge.tsx`, a TUI line, swarm kind `pr`. docs/features.md has how it's used.

- **The block.** `BlockType::Forge`, config `{provider, api?, login?, repo, kind: pr, number, host?, dir?}` plus what it has seen (`seen_ms`, `done_ack`, `log_mark`) and the drafts still waiting. Opened from a link, `OWNER/REPO#N` or N (`forge::open_config`: the clone's `upstream`, else `origin`).
  - **The adapter trait** (`me`, `poll`, `rest`, `write`, `repo_urls`) returns the normalized model, so M38/M39 add a file each. `Write` is `comment | review {event} | merge {style}`.
  - **Login matching** is S23's rule in `login::resolve`: URL host or `ssh_host`, else `GET repos/O/R` per login against `ssh_url`/`clone_url` (kept per host while the daemon runs), else the block lists the logins and `login {name}` picks one. The pick is in the config, so a restart doesn't resolve again. No tea: "no tea here".
  - **Tokens** from `tea login helper get`, through `Runner::user` (#74's environment), held in a process-wide map by login URL, asked for again after a 401. Never in config, state, logs or a client's view (the tests grep for it).
  - **Polling:** item + combined status every poll; reviews + timeline only when the item's fingerprint (`updated_at`, head sha, comment counts, state, requested reviewers) moves. Checks are always fresh because they come with the poll. 5 s while drawn or wanting you, 180 s otherwise (`ILLOGICAL_FORGE_POLL_MS=fast,slow`).
- **Attention, one reason at a time** (most pressing first), bundled `forge:<host>/<repo>`:
  - **A review asked of you → M34's `gate` reason with `GateSource::Forge {api, url, number}`** (member = repo, op = `#N`, gate = `review`). That reuses the rail's and the phone sheet's *Approve*, push and the bundle; `act` sees the forge source and calls the block's `review {event: approve, key}` instead of `approve`, and the card closes saying who. Chosen over M24's `ask` reason, whose `allow` routes to a held ask's `approve`, which is the collision S23 found.
  - **Red checks → `failed`** (attention `done`, like a failed command), `dismiss` only: Forgejo has no rerun API, so there's no `rerun` action; the state's `rerun` says so and links the first red check's run (*Open the run* on the block).
  - **Changes requested, a mention → `input`.** A mention counts after `seen_ms`, which moves whenever a client draws the block.
  - **Merged / green → `done`, once per key** (`merged`, `green:<sha>`), acknowledged when drawn. A PR opened already done isn't news (found against the real #84, which raised `done` on open).
  - A different kind replaces the reason by clearing the old one first: the multiplexer keeps a needs-input reason over a newer `input` otherwise. A `done`/`failed` waits while a draft is on the card (it would hide the card's `ask` reason), and every settled draft re-raises the block's own reason (`after_ask` leaves a block idle).
- **Drafts:** an agent's write (by `mcp:<client>`, or `agent: true`: the CLI sends `X-Illogical-Agent` under `CLAUDECODE`/`AI_AGENT`, and the `call` route adds it for forge blocks) is queued in the block and raised one at a time as a `Form` ask (`source: forge`, `agent` the client), body `format: markdown` (the web draws a textarea; *Send*/*Drop*). The answer path is the existing one: authz already makes `call/answer` editor+, so viewers get 403. The send uses the owner's login; the draft (`settled_by`), the log and `history` name who sent it and that an agent drafted it. A failed send puts the draft back with the error on its card. Waiting drafts are in the config, so they survive a restart.
- **The code:** `diff` and `checkout` fetch `+refs/pull/N/head:refs/illogical/pr/N` and the base into the person's clone (`dir`, from where it was opened, or `{dir}`), from the remote whose URL names the repo (else `origin`), make a detached worktree in `.illogical/worktrees/pr-N` (added to `info/exclude`) or `.claude/worktrees/pr-N` where that directory exists, and leave an existing worktree alone if it has changes or a branch. Then an M11 diff block `{repo: worktree, rev_a: merge_base, rev_b: refs/illogical/pr/N}` or a terminal there. Owner only (with `login`), via `authz`.
- **The log:** each new timeline event is a JSON line (`{e: event, event, line}`) in `blocks/%N/`, and each write a `Command`/`End` pair with `by`, so `search` finds a PR's comments and `history` says who sent what.
- **Tests:**
  - unit: the adapter on S23's Forgejo fixtures (copied to `crates/daemon/tests/fixtures/forgejo/`: #84, Codeberg #14606/#14657/#14665/#14667: merged, a draft with three reviewers and red checks, a team request after an approval, a reviewer still listed after reviewing, a mention), every review and status state in Forgejo's shapes, times with offsets, fingerprints, the rollup, mentions as words, login parsing/matching/resolving, PR links, drafts as cards and their edits;
  - `crates/daemon/tests/forge.rs` (10) against a fake Forgejo (S23's #84, changed per test) and a stand-in `tea`: the review gate approved from the rail with the token and named; failed/changes/mention/merged in turn, and search; a PR opened already done; polls that re-read only on change; four drafts queued, edited and sent by the owner, refused to a viewer, sent by an editor and recorded as theirs, dropped twice; a person's and an editor's direct writes; no `approve` method; login by repo lookup, none matching and picked, owner-only; no tea; a restart keeping the login and the draft; the PR's code as a worktree, a diff block with the right files, and a terminal;
  - `web/e2e/forge.spec.ts` (2): *Open pull request…* from the pane menu, the review approved from the block (the fake got `APPROVED` with the token); an agent's comment as a card with a textarea, edited and sent (the edited text posted), a merge draft dropped. The e2e config puts a stand-in `tea` on the daemon's PATH, so no spec can reach a real forge.
- **Against the real thing (read-only):** a dev daemon on geek (127.0.0.1:7855, its own state) opened `illogical pr 84` from this repo's clone: the tailnet SSH remote matched login `forgejo` by the repo lookup, `GET /user` said jhgaylor, the PR, its two green checks and three events came back in 0.86 s end to end, and the daemon's log and state directory held no token. Nothing was written.
- **Not covered:**
  - **The "done when" on geek:** a review requested from Jake reaching the phone, approved there and showing on Forgejo as him, and an agent's real `comment` sent. Those post to the real forge, which this run doesn't do: Jake's to try (the fake-forge tests cover the same paths);
  - *Rerun checks* on Forgejo (no API; it links the run), and GitHub's (M38);
  - `illogical pr` with no argument (the current branch's PR);
  - review comments on lines (`reviews/ID/comments`), shown flat in the timeline only as `code` entries;
  - a terminal's plain-text PR URL: links xterm already recognises (its web-links addon) open as a PR block when they're Forgejo-shaped (`…/pulls/N`; Shift-click opens the browser); OSC 8 hyperlinks aren't hooked;
  - blocks on a VM (the token comes from tea there, which is untested), and a real phone.

#### M37: issue blocks, and issue → agent (#89)

- `kind: issue` on the same block: state, labels, assignees, linked PRs, the timeline. Attention: assigned to you, or a mention.
- **Start work:** *Agent on this* makes a worktree and branch named after the issue (`iNN-<slug>`, as we name them now), an agent block there with the issue's text as its prompt and a link back, and a tab holding both. When that branch's PR appears, its PR block joins the tab.
- `illogical issue [URL | REPO#N | N]`, MCP `open_issue`, `illogical issue new` (a person's; an agent's is a draft ask, as in M36).
- **Done when:** on geek, *Agent on this* on a real issue in this repo gives a tab with the agent working in its own worktree, and the PR it opens shows up in that tab.

#### M37: as built

**Done 2026-10-03 on branch `m37-forge-issues` (not merged; the real "done when" is a write, Jake's, below).** `crates/daemon/src/forge/issue.rs` (the issue's read, *Agent on this*, new issues), additions to `model.rs` and `forgejo.rs`, `illogical issue` (and `issue new|comment|agent`), MCP `open_issue`, `read_issue`, `issue_comment`, `issue_new`, the issue view in `web/src/blocks/forge.tsx`, swarm kind `issue`. docs/features.md and docs/cli.md say how it's used.

- **The block.** `kind: issue` on M36's forge block, same config, login, polling and log. The model gains `ItemKind::Issue`, `Issue {item, events, linked}` and `Linked` (a PR that refers to it: number, title, state, url). A poll is `GET issues/N` (it refuses a PR's number: "open it as one"); the timeline is read again when the fingerprint (`updated_at`, comments, state, assignees) moves. Linked PRs come from the timeline's refs whose `ref_issue.pull_request` is set (no extra request). Timeline events now carry an assignment's target (`assigned X` / `unassigned X`) and a ref's `#N`.
  - **The adapter** gains `issue`, `issue_events`, `pr_by_head`, `new_issue` and `default_branch`, each with a default that says "not on this forge yet", so M38's and M39's adapters compile and work for PRs without them (merged with both cleanly on that basis).
  - **GitHub too** (after M38 merged): `github.rs` implements the five (`GET issues/N`, refusing a PR; the timeline's `cross-referenced` events from PRs as linked; an `assigned` event's target; `pulls?state=all&head=OWNER:BRANCH`; `POST issues`; `default_branch`), `github.com/O/R/issues/N` links open a GitHub issue, and a new issue in a GitHub clone goes to GitHub. GitLab issues aren't read (the defaults say so).
- **Attention** (`model::issue_attention`, M36's marks): an open issue given to you after `seen_ms` (the newest assignment to you, by someone else; or, never looked at, any assignment) is `input` "assigned to you by sam"; a mention after `seen_ms` (or `@you` in the issue's own text, never looked at) is `input`. **Chosen: closed → `done` once if it's assigned to you** ("closed", key `closed`; opened already closed isn't news, as with M36's PRs), nothing otherwise, and a closed issue raises nothing else. Bundled `forge:<host>/<repo>` with the repo's PRs.
- **Agent on this** (`agent {agent?, prompt_extra?, dir?, base?, again?}`, owner only via `authz`):
  - the branch `iNN-<slug>` (`model::branch_for`: up to five ASCII words of the title, about 28 characters, apostrophes dropped: `i89-m37-issue-blocks-and-issue`), from the forge's `default_branch` (else the remote's HEAD), fetched into `refs/remotes/<remote>/<base>` from the remote whose URL names the repo (else `origin`), in a worktree at `.claude/worktrees/<branch>` where the repo has that directory, else `.illogical/worktrees/<branch>` (excluded, as M36's). `--no-track`, so a plain `git push` can't go to main. An existing worktree is reused as it is; an existing branch is checked out;
  - the clone is M36's `dir` (from where it was opened, or `{dir}`);
  - the issue block takes a tab of its own, next to the one it was in, named `#N` if unnamed (`Api::OwnTab`: M2's `BreakPane`, then `RenameTab`), and an agent block splits beside it in the worktree (`claude` by default; `codex`, `fountain`, `acp` with `command` pass through);
  - **the prompt** (`issue::prompt`): "Work on issue #N in O/R: TITLE", the URL, the body, then that it's on a new branch made from the base, and to commit, push and open a PR from it into the base that says "Closes #N" (the block opens it beside the agent), then `prompt_extra`;
  - the link `{branch, worktree, base, block, agent, at_ms, pr?, pr_url?, pr_block?}` is in the config (survives a restart) and the state; a second `agent` while that agent block is open is refused unless `again`;
  - **its PR:** while linked and not found, every read also lists `pulls?state=all&sort=recentupdate&limit=50` and takes the newest whose `head.ref` is the branch in the repo itself (a fork's same-named branch doesn't count; merged ones do, so a PR that landed between polls is still found). Reads are every 30 s while linked (`LINKED`), 5 s while drawn. Found: marked first (so once), then a PR block opens split beside the agent (beside the issue if the agent is gone), with the issue's login and clone, and the lookups stop.
- **New issues** (`{issue: "new", title, body?, repo? | dir?}`): number 0 until it's on the forge.
  - **Decided: the draft lives on a new issue block in draft state** (not on the caller's block): the block that will become the issue holds it, so it's beside whoever asked, and `read_issue` follows it. An agent's (MCP's `by: mcp:<client>`, or the CLI's `X-Illogical-Agent`, which `open_block` turns into `agent: true`) is a form card (`id: new`, title and markdown text); *Send* posts it edited with the owner's login and the block becomes that issue; *Drop* leaves it saying who dropped it; a failed send waits again with the error. A person's goes out when the block opens (`by` from `who_is`), and the block reads it back. Both are `Command`/`End` in the block's history with who sent it and who drafted it.
  - Comments on an issue are M36's `comment` (Forgejo's `issues/N/comments` serves both), drafts from agents as before. `review`, `merge` and `rerun_checks` are refused on an issue.
- **Ways in:** `illogical issue [URL | O/R#N | N]` (prints the issue as text), `issue new -t -b [--repo]` (waits for the number, or says it's a draft), `issue comment %N`, `issue agent %N [--agent --prompt --dir]`; MCP `open_issue`, `read_issue` (read-only), `issue_comment`, `issue_new` (28 tools, 10 read-only); `open_config` takes `{issue: …}`, and any link opens what it links to (`parse_item_url`: `…/issues/N` is an issue even given as `pr`); in a terminal, Forgejo `…/issues/N` links open an issue block (Shift: the browser); *Open issue…* in the pane and `+` menus and *Issue* in the phone's sheet; swarm kind `issue`; the TUI line says the agent instead of checks.
- **Web:** the issue view in `forge.tsx`: header, state, labels and assignees, what waits on you, the agent (branch, worktree, its PR, or *Agent on this* and *With instructions…*), the PRs that refer to it (each *Open* as a block), drafts (shared with the PR view), description, timeline, *Comment…*; a new issue's draft shows as such until its card is answered.
- **Tests:**
  - fixtures (read-only, public): Codeberg `forgejo/forgejo#14663` (a mention, assigned, fixed by #14667, closed), `#14556` (open, labels, self-assigned, a linked open PR, a mention) and this repo's `#73` (a label, refs from issues, comments, closed), in `crates/daemon/tests/fixtures/forgejo/`;
  - unit: issues from those fixtures (items, linked PRs, events, attention for assignee, mention, closed, seen, given by someone else, taken away), a PR by head branch (not the fork's), branch names, the prompt, a new issue's card, issue configs and links (35 forge unit tests in all with M39's);
  - `crates/daemon/tests/forge_issues.rs` (3) against a fake Forgejo and the stand-in agent (`fake_acp.py` as `claude-agent-acp` via `ILLOGICAL_AGENTS_DIR`): assigned → input, a mention, search, a person's comment and an agent's draft sent, merge refused, closed → done, capture; *Agent on this* from a clone whose remote main moved on (branch from the remote's main, no upstream, clone clean), the agent's cwd and prompt, one tab named `#14556` apart from the terminal's, the link in `layout.json`, a second start refused, the agent's MCP calls (`read_issue`, `issue_comment` as a draft, `issue_new` as a draft block beside it), then the PR (and a fork's same-named one) listed: its PR block in the tab, once, and no more lookups; a person's new issue out at once and read back, an agent's (through the header) edited and sent, another dropped;
  - `tests/forge_github.rs` gains `an_issue_on_github_and_a_new_one` against M38's fake GitHub: an issue link read through `gh`'s token, assigned → input, a comment, a person's new issue posted; a unit test on GitHub's issue, timeline and linked-PR shapes;
  - `web/e2e/forge-issue.spec.ts` (2): *Open issue…*, assigned while looking, the linked PR, an agent's new issue edited on its card and sent; *Agent on this* from the button, the worktree, the agent's prompt, the tab, the PR joining it. `forge.spec.ts` and `swarm.spec.ts` (legend of 10) pass;
  - M36's `a_poll_rereads_the_rest_only_when_the_item_moves` waited for the status request instead of racing it (it failed once under load).
- **Against the real thing (read-only):** a dev daemon (127.0.0.1:7856, its own state) opened `illogical issue 89` and `illogical issue 87` from this repo's clone with tea login `forgejo` (the only one): each came back as text in 0.2–0.33 s end to end (#87's refs and close in its timeline), and the state directory and log held no token. `GET pulls?state=all&sort=recentupdate` (80 ms) gives `head.ref` and `head.repo.full_name` as the fake does (#84: `ci-cap-target`), and `default_branch` is `main`. Nothing was written, and no agent was started on a real issue.
- **Not covered:**
  - **The "done when" on geek:** *Agent on this* on a real issue in this repo, the agent pushing its branch and opening its PR, and that PR appearing in the tab. The push and the PR are writes to the real forge: Jake's to run (the fake-forge tests cover the same path end to end);
  - the real agent (Claude Code) given the prompt; only the stand-in ran;
  - issues on GitLab (M39): the adapter's defaults say "not on this forge yet"; GitHub's are covered by the fake GitHub only (no real GitHub issue was read), and GitHub Enterprise issue links aren't told from Forgejo's (they open as Forgejo's; `OWNER/REPO#N` in a GHE clone works);
  - a clone found from a link: like M36's `diff`, `dir` is only known when the issue is opened from a clone whose remote names the repo (or given); otherwise *Agent on this* says to give `{dir}`;
  - closing an issue, assigning, labelling (no writes but comments and new issues), and an *Agent on this* on a VM tab's machine (the block and the worktree are this host's).

#### M38: GitHub (#90)

- The GitHub adapter through `gh api`: check runs and commit statuses both feed checks; teams count for "a review requested from you".
- GraphQL only if S23's numbers say one query beats REST's several for a full read.
- **Done when:** M36's and M37's "done when", run against a GitHub repo, including a PR from a fork.

#### M38: as built

**Done 2026-10-03 on branch `m38-forge-github` (the write part of "done when" is Jake's, below).** `crates/daemon/src/forge/github.rs` (the adapter, gh and its token, ETags, the rate limit), small changes to `forge/mod.rs` (GitHub's connect, `rate` in the state, GitHub links and clones in `open_config`, the rerun note), `model.rs` (`Provider::Github`, `CheckSource::CheckRun`, `EventKind::Mentioned` and its attention rule), `illogical pr rerun %N`, `web/src/blocks/forge.tsx` (*Rerun*, the rate note) and `client.ts` (`/pull/N` links in terminals). It reuses M39's `rerun_checks` write and the rail's `rerun` routing. REST, not GraphQL, as S23 said. docs/features.md has how it's used.

- **The block.** The same `forge` block with `provider: github`, `api: https://api.github.com` (GitHub Enterprise: `https://HOST/api/v3`, URL building only). Opened from `https://github.com/O/R/pull/N` (`illogical pr URL`, *Open pull request…*, MCP `open_pr`, a link in a terminal; a `/pull/N` link on another host is taken as Enterprise), or `OWNER/REPO#N` / `N` in a clone whose remote is on github.com (`git@github.com:`, `ssh.github.com`, https).
  - **Login:** `gh auth token --hostname H` through `Runner::user` (#74), held in memory by host and asked again after a 401 (a test hands out a stale token first). The block's `login` is the host; `login {name}` says gh's is used. A Forgejo-provider block whose host no tea login has been picked for, and that gh has a token for, becomes GitHub Enterprise. **Changed from the spec:** that check is `gh auth token --hostname H` (keyring, no request), not `gh auth status`, which validates each token against its host; the answer is cached per host.
- **Polling:** every GET is conditional: the ETag, body and `Link` per URL are kept in a process-wide map (so a second block, or a `refresh`, gets `user` and `user/teams` as 304s too), and a 304 hands back the kept body. A poll is the item, `commits/SHA/check-runs` and `commits/SHA/status` (three 304s when nothing moved); reviews and the timeline are read (conditionally) only when the item's fingerprint (`updated_at`, head sha, comment counts, state, merged, `mergeable`, `mergeable_state`, requested people and teams) moves. A whole read is 7 requests the first time (`user`, `user/teams`, item, check runs, status, reviews, timeline), 5 for another PR on the same host. The timeline and reviews take the newest pages through `Link: rel="last"` (the newest 50 events).
- **Rate limits:** `X-RateLimit-*` (core) kept as heard. Under 100 left: a poll reads GitHub at most once a minute (the rest reuse the last answer). A 403/429 with `Retry-After` or 0 left stops reads until then (or the reset). Both show in the state's `rate.backoff` and on the block; writes still go.
- **The mapping** (`github::item/checks/reviews/events/me`): merged by `merged`/`merged_at`; `head.repo` names a fork; `merge_base` none (`diff` works it out); `mergeable`, and `blocked` from `mergeable_state: blocked`. Requests are `requested_reviewers` and `requested_teams` as `org/slug` (the org from the team's page, else the repo's owner), matched against `GET /user/teams` (`org/slug`). Check runs (status, then conclusion: `timed_out`/`startup_failure` fail, `stale` is neutral; an Actions run's `details_url` gives the workflow run to rerun) and commit statuses both are checks; the combined status's own `state` is never read, so its `pending` with no statuses means nothing. Reviews `APPROVED`/`CHANGES_REQUESTED`/`COMMENTED`/`DISMISSED`/`PENDING`, stale when on another commit. Timeline: comments, reviews, line comments (first of the thread), requests (person or team), pushes (`committed`, forced), labels, references, merged/closed/reopened, branch deleted, ready/draft in words; `subscribed` and Copilot's markers dropped. **`mentioned`** keeps who was mentioned as its target with no actor, and counts as a mention of you.
- **Attention:** M36's rules. A review asked of you or your team → the `gate` reason, approved as `review {event: approve}` → `POST pulls/N/reviews {event: APPROVE}`. Your checks red → `failed` with `rerun, dismiss` (state `rerun: {api: true, runs, url}`); *Rerun* (rail, block, `illogical rerun %N`, `illogical pr rerun %N`) calls `rerun_checks`, which reads the check runs again and posts `actions/runs/ID/rerun-failed-jobs` once per red workflow run. Changes requested, a mention → `input`; merged, or green and not blocked → `done`.
- **Writes** (drafts and people rules unchanged): `comment` → `POST issues/N/comments`; `review` → `POST pulls/N/reviews {event, body?}`; `merge {style: merge|squash|rebase}` → `PUT pulls/N/merge {merge_method}` (other styles refused before GitHub); `rerun_checks` as above (an agent's is a draft card like the rest).
- **The code:** M36's `diff`/`checkout` unchanged: `refs/pull/N/head`, which GitHub keeps in the base repository for forks too; the merge base is git's.
- **Tests:**
  - unit (6 in `github.rs`, 1 in `mod.rs`): S23's `cli/cli` #14519 (merged, from a fork, 18 check runs, combined status `pending` with none, 15 reviews, `mentioned`), #13788 (a request, 3 builds red with their run, `blocked`, green-but-blocked isn't done, unblocked is) and #13899 (changes requested and asked again, a team request in the timeline, GitHub's mentions), copied to `crates/daemon/tests/fixtures/github/`; synthetic: a team request yours or not, every review state and staleness, every check-run status and conclusion, statuses, another app's run (no rerun), links and hosts, `Link` pages, merge styles, GitHub configs;
  - `crates/daemon/tests/forge_github.rs` (9) against a fake GitHub (#13788, ETags and 304s, rate headers) and a stand-in `gh` (and a `tea` with no logins): a second poll is If-None-Match and 304s with no reviews or timeline asked, and an unchanged poll costs no points; a comment re-reads the rest (reviews 304, timeline 200) and search finds it; a 401 asks gh again; the token nowhere a client sees; a team request approved from the rail as `APPROVE` with the token; red checks rerun from the rail (one `rerun-failed-jobs` for the run), then running, named in history; an agent's comment and rerun as drafts, sent and dropped; a person's `REQUEST_CHANGES` and squash merge, `fast-forward-only` refused; a low limit polls nothing for the minute and says so; a spent limit waits for `Retry-After`; the timeline's last page; `OWNER/REPO#N` and N in a github.com clone, and a fork's PR from `refs/pull/N/head` with the merge base while trunk moved on; GitHub Enterprise by gh's login, and a host gh doesn't know staying Forgejo's;
  - `web/e2e/forge-github.spec.ts` (1): the block on #13788, 11 check runs, *Failed* with *Rerun* on the rail and the block, polls answered 304, *Rerun* posts `rerun-failed-jobs` with gh's token and the jobs queue. The e2e config adds a stand-in `gh` that knows only the hosts a spec names.
- **Against the real thing (read-only; gh as jhgaylor):** a dev daemon (127.0.0.1:7857, its own state, polls every 5 s for the measurement) opened `illogical --socket … pr https://github.com/cli/cli/pull/14583` (open, a review asked of tidy-dev, 14 checks green) in **2.2 s** end to end through the CLI, 7 requests and **7 points**; then the fork PR `cli/cli#14474` (`jarrensj/gh-cli`, 20 checks green, `blocked` by branch protection) in 3.3 s, 5 requests and **5 points** (`user` and `user/teams` were 304s). Then **60 s of polling both blocks (~22 polls, all 304s) cost 0 points** (`rate_limit.core.used` 80 → 80; one earlier 30 s window moved by 1 point while every poll was a 304, so something else of Jake's used it). No token in the daemon's log or state directory. Nothing was written.
- **Not covered:**
  - **The "done when"'s writes (Jake's):** a review requested from Jake on a GitHub repo reaching the rail and the phone, approved there and shown on GitHub as him; his PR's red checks rerun from the rail; an agent's `comment` waiting as a draft until he sends it; and M37's issue part once M37 lands;
  - GitHub Enterprise beyond URL building and the gh check (no GHE to try); `gh auth switch` between two github.com accounts (the active one's token is used);
  - rerunning another app's check runs (`check-suites/ID/rerequest`: they link their page), more than 100 check runs, review threads' resolution (GraphQL only), line comments as positions;

#### M39: GitLab (#91)

- The GitLab adapter through `glab api`: merge requests by `iid`, pipelines as checks, approvals as reviews, discussions as the timeline.
- `checkout` from `refs/merge-requests/N/head`.
- **Done when:** M36's "done when" against a gitlab.com project.

#### M39: as built

**Done 2026-10-03 on branch `m39-forge-gitlab` (the authenticated and write part of "done when" is Jake's, below).** `crates/daemon/src/forge/gitlab.rs` (the adapter, glab and its token), small changes to `forge/mod.rs` (the provider's connect, `read_only`, `rerun_checks`, GitLab links in `open_config`), `model.rs` (`Provider::Gitlab`, `CheckSource::PipelineJob`, `CheckState::Manual`), `api.rs` (*Rerun* on a forge block), `web/src/blocks/forge.tsx` and `client.ts` (MR links in terminals). docs/features.md has how it's used.

- **The block.** The same `forge` block with `provider: gitlab`, `api: https://HOST/api/v4`, `repo` the project path (subgroups too: `group/sub/proj`, URL-encoded to `group%2Fsub%2Fproj`) and `number` the MR's `iid`. Opened from `…/G/[SUB/]P/-/merge_requests/N` (`illogical pr URL`, *Open pull request…*, MCP `open_pr`, a link in a terminal), `GROUP/PROJECT!N`, or `N`/`REPO#N` in a clone whose remote is gitlab.com or a `gitlab.` host (others say `provider`).
  - **Login:** `glab config get token --host H` through `Runner::user` (#74), when glab knows the host: gitlab.com, `glab config get host`, or a host in its `config.yml`. The token is held in memory by host and asked again after a 401, never in config, state or logs (the tests grep for it). The block's `login` is `glab:HOST`; `login {name}` is tea's and says so.
  - **No login (no glab, or none for the host):** the adapter reads anonymously and the state's `read_only` (and `capture --text`) says `read-only: no glab login for HOST (why) …`: no discussions (401 even on public projects), no "you" (`GET /user` isn't asked), so no attention, and every write, a person's or an agent's draft, is refused before the forge. `refresh` runs glab again, so a `glab auth login` takes effect.
- **The mapping** (`gitlab::item/reviews/checks/events`):
  - state `opened`/`closed`/`merged`/`locked` (→ closed, as S23); `draft` or `work_in_progress` or a `Draft:`/`WIP:` title; labels, assignees; head = the MR's `sha` and source branch (a fork's head says `project ID`); `head_ref` `refs/merge-requests/N/head`; **`merge_base` = `diff_refs.base_sha`**, never `start_sha`.
  - `detailed_merge_status` → `mergeable` (`conflict`, `need_rebase`, `broken_status`, `has_conflicts` → false; `checking`/`unchecked`/`preparing` → unknown) and `blocked` (`not_approved`, `discussions_not_resolved`, `blocked_status`, `requested_changes` and the rest; the CI and draft states aren't blocks, the checks and draft flag say those). So green but not approved isn't `done`. It's there anonymously too (S23 thought it needed auth).
  - **Reviews:** `GET …/reviewers` states (`unreviewed`/`review_started` → pending requests; `reviewed` → commented; `approved`; `requested_changes` → changes requested) plus `GET …/approvals`' `approved_by` (an approver who isn't a reviewer is an approved review; a reviewer who approved without submitting isn't still asked). No team requests on GitLab.
  - **Checks:** the head pipeline's jobs (`GET projects/PID/pipelines/ID/jobs`, up to 5 pages of 100), `PipelineJob`, `allow_failure` kept, `manual` → `Manual`, not counted in the rollup. The head pipeline's project is its own (a fork's, or merged results on the target), so jobs and retries go there.
  - **Timeline:** discussions' notes, newest 50; system notes are events in GitLab's words (`what`), with pushes (`added N commits`), review requests (with the target), merged/closed/reopened and "mentioned in" mapped, and never a mention; diff notes are code comments.
- **Polling:** a poll is the MR, conditional (`If-None-Match`; weak ETags kept per path in the adapter), plus the jobs only when the head pipeline's status or times moved. Reviewers, approvals and discussions are re-read when the MR's fingerprint (`updated_at`, `sha`, `user_notes_count`, state, `detailed_merge_status`, `has_conflicts`, reviewers) moves. 5 requests for a whole read, 1 (a 304) for an unchanged poll; gitlab.com counts a 304 against the rate limit, so that's the floor.
- **Attention:** M36's rules unchanged. A review asked of you → the `gate` reason, approved as `review {event: approve}` → `POST …/approve`. Your pipeline red → `failed` with actions `rerun, dismiss` (state `rerun: {api: true, pipeline, url}`): *Rerun* on the rail, the block or `illogical rerun %N` calls the block's `rerun_checks` (`api.rs` routes a forge block's `rerun` there), which retries the head pipeline (`POST projects/PID/pipelines/ID/retry`); running again clears it. Changes requested, a mention → `input`; merged, green and not blocked → `done`.
- **Writes** (M36's drafts and people rules unchanged; `rerun_checks` is a write, so an agent's waits as a card): `comment` → `POST …/notes` (the link is `…#note_ID`); `review approve` → `POST …/approve` (and a note for its text); `review request_changes` → a note with the text, saying so: **GitLab's REST API has no call that sets a reviewer's `requested_changes`** (the web UI's review submit does, through GraphQL), so the reviewer state isn't set; `review comment` → a note; `merge {style: merge|squash}` → `PUT …/merge {squash}`, other styles refused; `rerun_checks` → the retry. Forgejo's `rerun_checks` says it has no API.
- **The code:** `diff`/`checkout` as M36's, fetching `refs/merge-requests/N/head` and the target branch, the diff on `base_sha..refs/illogical/pr/N`.
- **Tests:**
  - unit (9): `gitlab-org/cli!3941` (copied to `crates/daemon/tests/fixtures/gitlab/`: merged, from a fork, approved, 23 jobs with 3 manual and 6 allowed to fail, base_sha not start_sha, the approval's time, done for the author only, the text) and synthetic JSON for what it lacks: a review requested then approved without submitting, a failed job (and an allowed one), running, changes requested, a mention in a thread, system notes as events, every job state, every merge status, `locked`, drafts by flag or title, approvers who aren't reviewers, fingerprints, links and project paths, what glab says, GitLab configs from links, `!N` and hosts;
  - `crates/daemon/tests/forge_gitlab.rs` (7) against a fake GitLab (the recording in `group/sub/proj`, ETags and 304s, discussions and `/user` for logins only) and a stand-in `glab`: the review gate approved from the rail with glab's token and named; a failed pipeline with *Rerun* retried from the rail with the token, then green, changes requested, a mention and merged, and search; an unchanged poll is one conditional request and jobs are re-read only when the pipeline moves; an agent's comment and squash merge as drafts sent by the owner (`#note_` link, read back from discussions, `{squash: true}`), `rebase-merge` refused; glab knowing no login: anonymous, `read_only`, no events, no attention, no rerun, four writes refused and no draft; no glab at all; the MR's code from `refs/merge-requests/N/head` on `base_sha` while the target moved on;
  - `web/e2e/forge-gitlab.spec.ts` (2): the MR opened from the menu by its link, 23 checks (3 manual), *Rerun* retrying the pipeline with the token; with no glab login, read-only and no write buttons. The e2e config puts a stand-in `glab` beside the stand-in `tea`.
- **Against the real thing (read-only, anonymous; glab isn't installed here):** a dev daemon (127.0.0.1:7858, its own state) opened `illogical --socket … pr https://gitlab.com/gitlab-org/cli/-/merge_requests/3992` in **1.03 s** end to end (5 requests: MR, the fork project's pipeline jobs, reviewers, approvals, and discussions 401), and `gitlab-org/gitlab-runner!7510` in about **2.4 s** (87 jobs in one page; the jobs request alone ~1.8 s). Then each poll was **one 304** (every 5 s for the measurement). !3992 showed `requested_changes` by a reviewer and a pending request; !7510 two pending requests, `not_approved` (blocked), its pipeline green with one allowed failure among 87 jobs. Discussions were refused anonymously: the block's timeline is empty and its state, the web block and `capture --text` say `read-only: no glab login for gitlab.com (glab isn't installed here)`, no discussions, no "you", no writes. Nothing was written.
- **Not covered (Jake's, needs glab installed and `glab auth login`):**
  - **the authenticated "done when":** a real MR with a review requested from Jake reaching the rail and the phone, approved there and shown on gitlab.com as him; his pipeline red showing *Failed* with *Rerun* and the retry running; an agent's `comment` waiting as a draft until he sends it; `GET /user`, discussions and mentions with a real token; the token format (`glab config get token` with an OAuth login made by `glab auth login --web`, and a keyring-stored token, which `config get` may not print);
  - `request_changes` as a reviewer state (no REST call; a note instead); merge trains, `merge_when_pipeline_succeeds`, rebase merges; line comments as positions (shown flat); resolving threads;
  - `GITLAB_TOKEN`/`GITLAB_HOST` in the environment (glab honours them; this asks glab's config only); a self-managed GitLab under a path prefix; blocks on a VM.


#### M40: webhooks and hosted boxes (#92)

- **Forgejo:** a webhook straight to the daemon over the tailnet, set up from the block (*Live updates*) with the person's own CLI; the daemon checks its signature, and the block drops to slow polling while the hook is healthy.
- **GitHub:** the control plane's GitHub App (control track) takes webhooks and relays them to the daemons that have that repo's blocks open, through M18's relay. The same App gives hosted boxes (M20) read access with no CLI login. Writes still go out as the person.
- **GitLab:** webhooks to the daemon, as for Forgejo, when GitLab is reachable from it; else the relay.
- **Done when:** a review requested on Forgejo shows on the phone within about 5 s, and a hosted box's PR block reads through the App.

#### M40: as built

**Done 2026-10-03, released in v0.8.0.** Control is deployed with the App's six `GITHUB_APP_*` secrets (imported from `~/.config/illogical/github-app-forge.env`). An unsigned POST to `/github/webhook` gets 401, and GitHub's redelivered ping got 200 (the first ping, sent before the route existed, got 404). The real-forge checks are Jake's, in #93. `crates/control/src/forge.rs` (the App, webhooks, subscriptions, tokens) and small changes to `relay.rs` (text messages both ways on a daemon's socket), `db.rs`, `main.rs`; `crates/daemon/src/forge/live.rs` (pokes, health, hooks to the daemon, App tokens) and small additive changes to `forge/mod.rs` (`live_key`, `poked`, the wait, `live` as a write, the state's `live*`), `forgejo.rs`/`gitlab.rs` (`hook`), `github.rs` (the App fallback), `control.rs`, `dial.rs`, `server.rs`, `authz.rs`; *Live updates* in `web/src/blocks/forge.tsx`. docs/control.md has the App's setup; docs/features.md the live updates.

- **Control's GitHub App.** Config is exactly the manifest flow's names: `GITHUB_APP_ID`, `GITHUB_APP_SLUG`, `GITHUB_APP_CLIENT_ID`, `GITHUB_APP_CLIENT_SECRET`, `GITHUB_APP_WEBHOOK_SECRET`, and `GITHUB_APP_PRIVATE_KEY_FILE` (a `.pem`) or `GITHUB_APP_PRIVATE_KEY` (the PEM itself, `\n`s allowed: the Fly form). PKCS#1 or PKCS#8, signed with `aws-lc-rs` (already in the tree through rustls; no OpenSSL). Off without an id and key. With no `GITHUB_CLIENT_ID/SECRET`, the App's client id and secret sign people in (the App needs the callback URL for that). `control.json` says `github_app: SLUG`.
  - **JWT:** RS256, `iat` now−60, `exp` now+540, `iss` the App id.
  - **`POST /github/webhook`:** `X-Hub-Signature-256` checked with `hmac`'s constant-time `verify_slice`; bad, wrong or missing → 401. Deduped by `X-GitHub-Delivery` (the last 10,000, in memory). `ping` → `{pong}`. `installation*` events drop the caches. Each event becomes pokes `{provider: github, host: github.com, repo, number?, event, delivery}`: PR/review/review comment/thread by `pull_request.number`, issues and issue comments by `issue.number`, check runs/suites and workflow runs by their `pull_requests` (one poke each), and a status, a fork's check (empty `pull_requests`) or a job: repo-wide. Logged: event, delivery, repo, number, how many daemons. Never the payload.
  - **Subscriptions (decided: over the relay socket, not HTTP).** A daemon sends `{"t": "forge.watch", "repos": [...]}` as a text message on its existing relay socket (on connect and whenever its github.com blocks change; at most 200, `owner/name` only). Control answers `{"t": "forge.watching", "repos": [{repo, live, why?}]}` and sends it again every minute: that's the heartbeat. Pokes go as `{"t": "forge.poke", "poke": …}` on the same socket. A subscription lives as long as the socket (a reconnect re-sends it; a newer socket's isn't dropped by an older one's end).
  - **Who hears what (implemented rule):** the daemon's account must have a GitHub identity on control (signed in with GitHub); `GET repos/O/R/installation` (as the App) must find an installation; and either its account is that GitHub login, or `GET repos/O/R/collaborators/LOGIN/permission` with an installation token scoped to that repository says anything but `none`. Answers are kept ten minutes. **Limits:** organization membership alone isn't checked (the App has no members permission), so an org member who isn't a collaborator (or a team-only member, if GitHub doesn't count them) hears nothing; on a public repository GitHub may report `read` for anyone, so anyone signed in with GitHub could get pokes for it (a poke says only that something changed on a public repo); passkey-only accounts and team daemons go by the daemon's enrolling account; hosted sandboxes have no relay socket (M20), so they poll, reading through the App.
  - **`POST /api/daemon/github/token {repo}`** (a daemon's signature): the same rule, then `POST /app/installations/ID/access_tokens {repositories: [name], permissions: {metadata, contents, pull_requests, issues, checks, statuses, actions: read}}`. Returns `{token, expires_at, login, app}`. Control keeps a token until five minutes before GitHub's `expires_at`; logs only daemon and repo.
- **The daemon (`forge/live.rs`).**
  - **A poke** (from control, or a hook here) records the repository as heard and wakes the matching blocks: same provider, host and repo, and the same number, or any number for a repo-wide poke. PR and issue blocks alike (M37's issue blocks share the key). The state counts `pokes`.
  - **Health and the wait:** the path is healthy while something (a poke, a delivery, control's `live: true` heartbeat, or making the hook) was heard within `ILLOGICAL_FORGE_LIVE_MS` (default 10 minutes). Then the block waits the slow interval (180 s) even while drawn or wanting you, and wakes when health would lapse; after that it's back to M36's rule. Control's `live: false` drops it at once, with control's reason.
  - **State:** `live: webhook|polling`, `live_via` (`github-app`, `hook`), `live_why` (not joined to control; control hasn't said; control's reason, like the App not installed; no webhook; nothing heard in N s), `live_heard_ms`, `hook`; `capture --text` says `live: webhook` or `live: polling (why)`. The block's footer says *live (GitHub App)*, *live (webhook)* or *polling*, with the reason as its title.
  - **The App fallback (hosted boxes):** a github.com block's token source tries `gh auth token`; if that fails and the daemon is joined, it asks control for the repository's installation token (memory only, until a minute before it expires; a 401 asks again). Then `me` is the login control sends (no teams), so a review asked of you still reaches the rail; `read_only` says it reads through the App and that writes need `gh auth login`; every write, a person's or an agent's draft, is refused before the forge (M39's read-only path). `refresh` tries gh again.
- **Forgejo and GitLab hooks to the daemon.**
  - **Routes** `POST /api/forge/hooks/forgejo?k=…` and `/api/forge/hooks/gitlab?k=…`, on the TCP listener (the tailnet's), classed like `/api/dial` and `/e2e`: no tailnet identity is asked; the handler authenticates by the hook's secret alone. `k` finds the record; Forgejo's `X-Forgejo-Signature` (or `X-Gitea-Signature`) must be the hex HMAC-SHA256 of the body (`verify_slice`), GitLab's `X-Gitlab-Token` must equal the secret (constant-time); an unknown `k`, or a key of the other forge's, is 401 like a bad signature; a body for another repository is 400. Forgejo: number from `pull_request` or `issue`; GitLab: `merge_request` → `object_attributes.iid`, `note`/`pipeline` → `merge_request.iid`; GitLab issues (numbered apart from MRs) count as heard without a poll.
  - **`live {on}`** (a write like `comment`, so an agent's call is a draft card; the route is owner-only in `authz`): on Forgejo `POST repos/O/R/hooks {type: forgejo, active, branch_filter: *, config: {url, content_type: json, secret}, events: pull_request, pull_request_assign/label/comment/sync, pull_request_review_approved/rejected/comment/request, issues, issue_comment, status}`; on GitLab `POST projects/ID/hooks {url, token, merge_requests_events, note_events, pipeline_events}` (needs a glab login). `{on: false}` deletes it (a 404 counts as gone). The URL is the daemon's tailnet `https://` direct URL (else the first `--direct-url`; `ILLOGICAL_FORGE_HOOK_BASE` overrides), the secret 32 random bytes; both, with the forge's hook id, are kept in `<state>/secrets/forge-hooks.json` (dir 0700, file 0600), never in the block's config, state or log. A GitHub block's `live` says its updates come through control's App. **Limit:** an agent's `live` draft can be sent by an editor (drafts are answered by owner or editor); the direct call is the owner's.
- **Tests:**
  - control unit (6 in `forge.rs`): the JWT's header and claims verified with the public key of a throwaway RSA key made in the test (and a tampered one refused), PEM forms (`\n`, EC refused); signatures good, wrong secret, wrong body, missing, `sha1=`, not hex, empty secret; what a poke keeps (no title or body), issue comments, check runs' PRs deduped, a fork's suite and a status repo-wide, workflow runs; deliveries once (and the oldest forgotten); routing only to live subscriptions and an old socket's end leaving a new one's; repo names and times.
  - control end to end (`forge_wire.rs`, 1): the real router on a port, a fake GitHub that checks the App's JWT (signature, `iss`, `exp`), three daemons on real relay sockets signed as daemons sign: Jake's (owner of `jhgaylor/hud`, collaborator on `acme/tool`), a stranger's, a passkey account's. Each subscribes and hears where it stands (not installed; not a collaborator; sign in with GitHub); the access check's token was scoped to one repository, all read. A signed `pull_request` reaches only Jake's daemon as exactly the poke; a redelivery is a duplicate; bad, missing and tampered signatures 401; a ping pongs; a check run on `acme/tool` pokes both its PRs; the others hear only heartbeats; the heartbeat comes again; the token endpoint mints `hud`-only for Jake and refuses the stranger, the passkey account, an uninstalled repo and an unsigned call; hanging up drops the subscription. Plus `real_app_reads` (ignored; by hand).
  - daemon (`tests/forge_live.rs`, 4) against a fake control (its relay socket and token endpoint), a fake GitHub with a stand-in `gh`, and a fake Forgejo with a stand-in `tea`: a GitHub block polls fast while it wants you, subscribes once joined (`forge.watch ["cli/cli"]`), goes `webhook` on control's heartbeat and polls slowly, polls within ~1 s of a poke for its PR (not another PR's; a repo-wide one too), goes back to `polling` and fast after the quiet time, shows control's reason, and unsubscribes when closed; a box whose gh has no login reads through the App token (`me` from control, a review asked of you on the rail, `read_only`), refuses a person's and an agent's comment saying `gh auth login`, and the token is in no state or file; on Forgejo an agent's `live` is a draft with no hook made, the owner's makes it with tea's token (the events, the URL on the daemon's port, a 64-hex secret in a 0600 file and not in the state), unsigned/wrong/unknown-key deliveries 401, another repo's 400, a signed one pokes the PR, an issue block on the same repo hears #73 but not #84's, Gitea's header works, `live off` deletes hook 77 and forgets the secret; GitLab's route takes only its token, from a tailnet stranger too, and that stranger reaches nothing else (`/api/run` 403).
  - `web/e2e/forge-live.spec.ts` (1): *Live updates* on #84 makes the hook (the URL on the test daemon), the footer says *live (webhook)*, unsigned and wrongly signed deliveries 401, a signed one reads the PR at once (`pokes: 1`), *Stop live updates* deletes it.
  - Whole run: `cargo test --workspace` (all pass once `web/dist` is built; `share` needs it), clippy `-D warnings`, fmt, web typecheck, `forge`, `forge-github`, `forge-gitlab`, `forge-issue`, `forge-live` and `control` e2e specs (13 passed).
- **Against the real thing (read-only):** `real_app_reads` with `~/.config/illogical/github-app-forge.env`: a JWT from the App's key was accepted (`GET /app/installations` 200: installation 167408409 on jhgaylor, all repositories); `GET repos/jhgaylor/home-cloud/installation` found it; the access rule said jhgaylor may; an installation token scoped to `home-cloud`, read-only, was minted (expires in an hour); `GET repos/jhgaylor/home-cloud/pulls/269` read with it (200). Nothing was written, no webhook was made, and no secret was printed.
- **To go live (the orchestrator, after review):** deploy control (`just control-deploy`), then `fly secrets set GITHUB_APP_ID=… GITHUB_APP_SLUG=illogical-jhgaylor GITHUB_APP_CLIENT_ID=… GITHUB_APP_CLIENT_SECRET=… GITHUB_APP_WEBHOOK_SECRET=… GITHUB_APP_PRIVATE_KEY="$(cat ~/.config/illogical/github-app-forge.pem)"` (the values from `github-app-forge.env`; `GITHUB_APP_PRIVATE_KEY_FILE` is for a local file, not Fly). If GitHub sign-in should use the App, add `https://control.illogical.widgets.wtf/auth/github/callback` as the App's callback URL (or set `GITHUB_CLIENT_ID/SECRET` separately).
- **Done when, still to run (Jake's):**
  - after the deploy: the App's settings show the ping delivered (200 `pong`), and a real event on a jhgaylor repo reaches geek (sign in to control with GitHub, open one of your PRs on a joined daemon: the block says *live (GitHub App)*; a comment on it shows within seconds);
  - a hosted box (M20) opening a PR of yours reads through the App (`read_only` says so) and refuses writes;
  - Forgejo: *Live updates* on a real PR on this repo's Forgejo (needs tea's token to be allowed to make repository hooks, Forgejo's webhook `ALLOWED_HOST_LIST` to allow the tailnet name, and the Forgejo node to reach geek's `https://` over the tailnet ACLs); then a review requested on Forgejo reaches the phone within about 5 s;
  - GitLab: the same on a gitlab.com MR only works where gitlab.com can reach the daemon, which a tailnet address isn't; that case polls (the relay for GitLab, as the plan said, isn't built).
- **Not covered:** GitHub Enterprise webhooks; org-membership-only access; pokes to hosted sandboxes (no relay socket: they poll through the App); a Forgejo hook that already exists from another daemon (each daemon makes its own); refreshing a GitHub App token's 401 beyond asking control again.

### Fountain track (S24, M43–M45, added 2026-10-03)

Today illogical knows Fountain only as an ACP command: an agent block runs `fountain acp --agent X` (S7). This track goes further in three ways. It shows the account's agents as a catalog. It runs one of them locally, as a Claude Code wearing its prompt, skills and MCP servers. And it makes geek the account's Fountain runner, so a Fountain conversation runs on a machine illogical can open a shell on, diff and take over.

**Decisions (2026-10-03, Jake):**

- **geek is the only runner.** Fountain puts a runner conversation on the most recently connected online runner, because per-agent pinning (`agents.runner_id`, ADR 0022) isn't built. Jake fixes that in Fountain going forward. Until then there is one runner: jake-air's runner is stopped and its registration deleted, along with `jake-mbair` and `fireball`.
- **Process backend, as a dedicated `fountain` user.** A sandbox is a directory on geek, so illogical can open it with code it already has. Its agent can't read Jake's keys, `~/.fountain/credentials` or illogical's socket. firecracker stays out (it overlaps wisp, and M3b stays on wisp).
- **The catalog shows every agent, with a filter**, not only the curated ones.
- **Local `${VAR}`s come from Infisical first** (agent-specs keeps its secrets there), then the host's shell environment (#74), then known helpers (`GITHUB_TOKEN` from `gh auth token`). Fountain never returns a secret's value, so they can't come from Fountain.
- **The three agents on `sandbox_provider: runner` today move to geek:** `hud-playground`, `fireball-smoke` and `home-cloud-steward` (the last is `managed-by: chant`, so it changes in agent-specs).
- **Also decided (2026-10-03):** geek's runner is a systemd unit, not a pane, because M2's real reboot check is still pending. *Run here* covers `claude` agents only. M44 adds `metadata.illogical.local: false` to the orchestrators in agent-specs itself. A sandbox shell goes through a sudoers rule (bash as `fountain`). M45's setup creates the runner's key (`fountain keys create geek-runner`), written straight to the `fountain` user's credentials, never printed.
- **The catalog is read-only.** agent-specs (chant's fountain lexicon) stays the one place a curated agent is edited, so illogical never fights chant's converge. *Spec* opens the file.

**Order:** S24 (#120), done. Then M43 (#121), alongside M45's setup half (geek's runner up as `fountain`, `illogical fountain runner install`) and #124. Then M44 (#122) and the rest of M45 (#123), in parallel: both need M43's API client and block. The run is described in `BRIEF-fountain.md` (tracker #125).

#### S24: Fountain spike (#120)

**Done 2026-10-03: go** (see [spikes/s24-fountain](spikes/s24-fountain/README.md)). Run against hosted Fountain (`managoat.com`, CLI v0.21.0). Read-only, apart from building local bundles.

- **108 agents, mostly made by apps:** 13 Switchyard, 8 Salon, 5 Mend, 4 Rounds, planter, Paddock and probes. About 23 are `managed-by: chant` (from `~/dev/jhgaylor/agent-specs`), and a handful are hand-made. Metadata tells them apart: `managed-by`, `switchyard`, `salon`, `paddock`, `attemptId`, `part-of`.
- **An agent is a full recipe:** `system` (105 of 108), `skills` (25: inline `{name, content}` or GitHub `{source, name?}`), `mcp_servers` (43, with `${VAR}` in any string), model, runtime, environment, `sandbox_provider`. `fountain agent list --json` returns all of it, about 550 KB.
- **Secrets are write-only** (`docs/concepts/vault.md`).
- **Runners:** `GET /api/runners` returns name, os, arch, version, online and last seen. Python's default User-Agent gets a 403 from `managoat.com` and curl doesn't, so the daemon sends its own User-Agent. A runner sandbox is named `runner-<runner_id>-<short>`.
- **Wearing an agent locally** (`wear.py`): inline skills are written out, and GitHub skills are shallow-cloned and each directory with a `SKILL.md` is copied. `pr-reviewer` got 19 skills and three MCP servers once `GITHUB_TOKEN` was set. `claude-agent-acp` takes the system prompt as `_meta.systemPrompt.append` and SDK options (plugins, model) as `_meta.claudeCode.options`. illogical already sends `mcpServers`.
- **Seen working through `claude-agent-acp`** (`q1-acp.mjs`, 11 s): the session calls itself pr-reviewer running locally, lists the plugin's skills as `fountain-pr-reviewer:*`, and has tools from `github` (through `gh auth token`) and `context7`.
  - `mem0` (HTTP with OAuth) can't connect headless, so M44 lists a server that needs OAuth as left out.
  - The account's claude.ai connectors come along too.
  - The client must keep `settingSources: []` (as illogical's Claude blocks do). Without it, the user's SessionStart hook `illogical inbox` (24 h timeout) holds the session. That is also why headless `claude -p` never answered on geek (#124).
- **The prompts are written for the sandbox.** Orchestrators such as `captain-picard`, `team-lead` and `tech-lead` clone into `/workspace` and spawn with `vault_id`, so they're for Fountain, not for wearing.

#### M43: Fountain agent catalog (#121)

`BlockType::Fountain` with `{profile, view: catalog}`:

- **Reads** go through Fountain's HTTP API with the key from the user's own CLI login (`FOUNTAIN_API_KEY`, or `~/.fountain/credentials` and its `[profile]`), the way M36 uses `tea`'s login. The key is kept in memory only. A host with no login says so on the block.
  - The list loads when the block opens, then refreshes every few minutes while it's drawn, and on *Refresh*.
  - Environment names come from `GET /api/environments`.
- **Every agent is a card:** name, description, runtime and model, skills, MCP servers, environment, sandbox provider and mode, conversation count, last updated, and where it comes from.
- **Where it comes from:**
  - **agent-specs:** `managed-by: chant`;
  - **made by an app:** `switchyard`, `salon`, `paddock`, `part-of` or `attemptId` in its metadata, or a name ending in a UUID;
  - **hand-made:** everything else.
- **Filters:** a search box over name, description, skills and MCP servers; chips for source, runtime and sandbox provider. The filters are kept in the block's config. Every agent is listed, and app-made ones are only filtered, never hidden by default.
- **Actions:**
  - *Run on Fountain*: today's `fountain` agent block, beside the catalog.
  - *Run here*: M44. Shown only for `claude` agents, greyed out with the reason when `metadata.illogical.local` is `false`.
  - *Spec*: for `managed-by: chant`, the agent-specs file that declares it (found by matching `name:` under `src/agents/`), opened as a file block. Otherwise Fountain's dashboard page for the agent.
- **MCP:** `list_agents {query?, source?}` returns compact rows, and `read_agent {name}` returns the full recipe with secret-shaped values left as their `${VAR}`. Any local agent can see the team and hand off a task through `start_agent`, which already takes a Fountain agent.
- **`capture --text`:** the filtered list as text.
- **Ways in:** `illogical fountain [agents]`, *Fountain agents…* in the picker, and MCP.
- **Web:** `web/src/blocks/fountain.tsx`, with cards in a grid (a list on the phone) and the filter bar on top. One line in the TUI.

**Tests:** filters and source rules as unit tests on S24's `agents.json`, scrubbed. Daemon tests against a fake Fountain made of recorded responses.

**Done when:** on geek, the catalog shows all of Jake's agents. Filtering to agent-specs leaves the ~23 curated ones. *Run on Fountain* on `games` opens a working block. *Spec* on `pr-reviewer` opens its agent-specs file. And a Claude block's `list_agents` finds `designer` by the skill `frontend-design`.

#### M43: as built

**Done 2026-10-03 (f77ece0).**

- **Module:** `crates/daemon/src/fountain/`: `api.rs`, `login.rs`, `catalog.rs`, `mod.rs`. `BlockType::Fountain {profile, view}`, where `view` is an enum ready for M45b's `runner`. A `WorkKind::Fountain` swarm kind.
- **Fountain's API:**
  - `GET /api/agents`, `/api/environments`, `/api/runners` and `/api/sandboxes` all answer `{data: …}`. Sandboxes carry `runner {id, name, path}` and their conversations.
  - An agent's `model` can be null.
  - Lists are parsed row by row, so a bad row is counted as unreadable and never empties the list.
- **Sources:** app-made also counts `drydock` metadata and names like `Mend: …`, `Rounds: …` and `Cantor audit: …`; without them about 50 app agents read as hand-made. agent-specs is 23 either way.
- **The review found** (and the branch fixed before merging):
  - an editor could `run` (a local `fountain acp` on the owner's login) or `spec`. `run`, `run_fountain` and `spec` are now owner-only, with `profile` and `specs`;
  - `read_agent` returned literal header and env values. Anything without a `${…}` is now `<redacted>`;
  - null fields broke the whole list;
  - the cache wasn't keyed on host and key;
  - an explicitly picked profile now uses its own key.
- **Fixtures:**
  - The first push carried private MCP hostnames and prompt excerpts. The remote branch was deleted and its history rewritten.
  - `scrub.py` now maps every non-public host to `mcp-N.example.com`, cuts prompts to 80 characters, and refuses to write if anything private is left.
- **R2 on geek** (a dev daemon, the real account):
  - 110 agents, matching `fountain agent list`; agent-specs gives 23; `frontend-design` finds `designer`.
  - *Spec* on `pr-reviewer` opens `agent-specs/src/agents/specialists/engineering/pr-reviewer.ts` at line 5.
  - *Run on Fountain* on `hud-playground` answered in 30 s from geek's runner.
  - A real Claude block found "designer, from agent-specs" through `list_agents`.
  - `games` couldn't be used: its Fountain environment's apt package `love` fails in postinst, so every `games` sandbox fails to provision. That's for Fountain, not illogical.
- **Merging with M45a:** both added `illogical fountain`. There's now one command, with `runner` and `agents` under it.

#### M44: wear a Fountain agent locally (#122)

`illogical agent --as <fountain agent>`, and *Run here* in M43: a Claude agent block on this host, in a worktree (or the current directory), configured as that agent. A terminal `claude` launched from the picker as *Claude Code as…* gets the same bundle through flags.

- **S24's q1 passed** through `claude-agent-acp` (`spikes/s24-fountain/q1-acp.mjs`). Keep `settingSources: []` when merging the options into Claude's `meta`.
- **The bundle** is built by the daemon from the agent's recipe (M43's read), and cached under `~/.cache/illogical/fountain/<agent id>/<updated_at>/`:
  - a plugin whose `skills/` holds the inline skills, plus the GitHub ones. GitHub repos are shallow-cloned into a shared cache and refreshed once a day;
  - the system prompt, after a short preamble: you're local, and `/home/sprite`, `/workspace`, vaults and spawning describe the sandbox;
  - MCP servers with their `${VAR}`s resolved. Fountain's rules: `$$` is a literal `$`, references are resolved once, recursively, and every unset name is reported.
- **`session/new`:** `_meta.systemPrompt.append`, plus `_meta.claudeCode.options` with `plugins: [{type: "local", path}]` and `model` (`anthropic/` stripped). `mcpServers` is the agent's servers alongside illogical's own.
- **Secrets, in order:**
  - Infisical: agent-specs' `.infisical.json` project, mapping each `${VAR}` through the agent's environment and vault in `dist/fountain.yaml` to its `infisical://` URI, read with the `infisical` CLI and Jake's login;
  - then #74's shell environment;
  - then helpers (`GITHUB_TOKEN` ← `gh auth token`).

  Values are held in memory and passed in `session/new`. A terminal `claude` gets a 0600 `--mcp-config` file in the daemon's runtime directory, deleted when the pane closes. A server with an unresolved `${VAR}` is left out, and the block's header says which server and which variable.
- **Not wearable:** an agent with `metadata.illogical.local: false` is refused with the reason. M44 adds it to the orchestrators (`captain-picard`, `team-lead`, `tech-lead`) in agent-specs and applies it. `codex`, `gemini`, `opencode` and `acp` agents say "Run on Fountain" for now.
- **The header** says "as <agent>" with the bundle's skills and servers, and what didn't carry over.

**Tests:**
- unit tests: substitution, matching the cases in Fountain's `managoat_substitution`; and building a bundle from fixtures (inline skills, a GitHub skill with a `name`, all of a repo's skills, an unset variable);
- daemon tests: the `_meta` on `session/new`, against the fake adapter;
- a real test (`agents_real.rs`, gated): `games` worn locally names its three skills.

**Done when:** on geek, *Run here* on `pr-reviewer` opens a Claude block in a worktree of this repo. It lists `code-review` and `iterate-pr` among its skills and has `github`, `context7` and `mem0`, with GitHub working through `gh auth token`. Running `captain-picard` says it's for Fountain.

#### M44: as built

**Done 2026-10-03 (5110bb6). It also fixed #127 and #128.**

- **The bundle:** `fountain/wear.rs` builds it under `~/.cache/illogical/fountain/<id>/<updated_at>/`, in versioned directories behind a `current` pointer, kept a week. GitHub skills share a clone cache that's fetched daily.
- **Secrets:**
  - In order: Infisical through agent-specs (the vault's mapping, then the environment's; an unmapped variable is tried as `dev/NAME` and labelled so), then #74's shell environment, then helpers (`gh auth token`).
  - **No value goes on a command line.** The SDK under claude-agent-acp writes `--mcp-config` onto `claude`'s argv. So worn servers carry `${ILLOGICAL_FTN_…}` references, and the values sit in the adapter's environment. Claude Code expands them; checked for real for headers, URLs and stdio env.
  - **Left out, with the reason:** stdio args with variables (they'd land on the server's own argv), values containing `${` (Claude Code expands twice), unresolved variables and OAuth servers.
  - **#128:** every local Claude block's own illogical MCP token travels the same way (`${ILLOGICAL_MCP_BLOCK_TOKEN}`).
  - **Scrubbing:** values are scrubbed from logs, the transcript and reasons.
- **Restarts:** after a restart, a worn block is worn again before it takes over its adapter. An adapter from before #128 is restarted.
- **#127:** ordinary blocks keep `settingSources: []` on resume, load and fork.
- **Authz:**
  - *Run here*, `--as` and `start_agent {as_fountain}` are owner-only.
  - An agent on a machine (a guest's) can create things only on machines, through every MCP tool that creates one (`run`, `start_agent`, `open_conversation`, the `open_*` tools). This closed an older way onto the owner's host.
- **Refusals:** `illogical.local: false` (on captain-picard, tech-lead and team-lead; agent-specs#20) and non-claude runtimes.
- **Not built:** *Claude Code as…* for a plain terminal `claude`.
- **R3 on geek:**
  - `pr-reviewer` worn in a worktree of this repo had 19 skills, `context7` and `github`, and said `mem0` needs OAuth.
  - It found the planted bug in `driven()`, named the test it breaks, and read agent-specs#20 through GitHub MCP.
  - `captain-picard` is refused as "for Fountain only".
  - `fountain-workbench` wears, and says why its `workbench` server didn't carry over.
  - No token was in any process's argv.

#### M45: geek as the Fountain runner (#123)

- **Set up (once, with Jake):**
  - Jake stops the runner on jake-air. Then `DELETE /api/runners/:id` for `jake-air`, `jake-mbair` and `fireball`.
  - Create a `fountain` user on geek, with Jake in its group. Sandboxes go under `/home/fountain/sandboxes` (group-readable).
  - Create its key from Jake's login (`fountain keys create`, named `geek-runner`, full scope), written straight to `/home/fountain/.fountain/credentials` (0600) and never printed.
  - Run `fountain runner --name geek --root /home/fountain/sandboxes` as a systemd unit (`User=fountain`, `Restart=always`). Then move `hud-playground` and `fireball-smoke` to the runner provider through the API, and `home-cloud-steward` in agent-specs.
  - `illogical fountain runner install` does the steps that don't need Jake, so it's written down and can be done again. It refuses on a host without `sudo`.
- **Runner status:** the Fountain block (M43) gets `view: runner`, since only a block can raise attention today. That view, plus a line on geek in the machine panel (a `fountain_runner` field on `HostInfo`) and the swarm's machine, shows *Fountain runner*: online or offline, version against the installed `fountain` CLI, last seen, and how many sandboxes it holds. `/api/runners` is polled every minute while drawn. Attention (`failed`) when geek's runner has been offline for 5 minutes while the unit says it's running, and for any other runner on the account (one would win placement).
- **Runner conversations as blocks:**
  - `fountain sandbox list` gives the conversations whose sandboxes are on geek. Each sandbox is a directory under `--root`, named `runner-<runner_id>-<short>`, and its git checkouts are inside it.
  - From a conversation's card in the catalog (M43), or a *Fountain on geek* group in the swarm:
    - *Follow*: today's `fountain` agent block attached to that conversation (`session/load`);
    - *Changes*: M11's diff block on each git checkout in the sandbox, read through group permissions;
    - *Shell*: a terminal as `fountain` in that directory with `HOME` set to it, through one sudoers rule (`jake ALL=(fountain) NOPASSWD: /bin/bash`).
  - A parked sandbox (processes stopped) still opens. A shell started from illogical isn't Fountain's, so Fountain's park doesn't stop it, and the block says so.
- **Not in M45:** a runner picker. It waits for Fountain's pinning; when that lands, M43's card shows the agent's runner and *Move to geek*.

**Tests:**
- unit tests: runner status and the stale-runner attention, from recorded `/api/runners`;
- daemon tests: sandbox → directory mapping, and the sudo argv for *Shell*;
- a manual checklist for the real setup, in the README.

**Done when:** `/api/runners` lists only geek, online. A conversation with `hud-playground` lands on geek; from its card, *Changes* shows its edits and *Shell* opens in its sandbox as `fountain`. That user can't read `~jake/.ssh` or connect to illogical's socket. Stopping the unit puts *Fountain runner offline* on the rail within about 5 minutes.

#### M45: as built

**Done 2026-10-03: M45a (4aa386f), M45b (308afb8).**

- **Setup:**
  - `scripts/fountain-runner-setup.sh` (root) creates the `fountain` user, `/home/fountain/sandboxes` (2750), a root-owned node and npm in `/opt/fountain-node`, `/usr/local/bin/fountain`, and a sudoers file allowing bash as fountain plus start, stop, restart and status of `fountain-runner`. It also writes the systemd unit: `User=fountain`, `UMask=0027`, `NoNewPrivileges`, `PrivateTmp`, `InaccessiblePaths` over the user's home and `/run/user/<uid>`, and `ProtectProc=invisible`.
  - `illogical fountain runner install | status | adopt` does the rest without root. The key from `fountain keys create` is never printed.
  - jake-air's launchd runner was removed, and the `jake-air`, `jake-mbair` and `fireball` registrations were deleted.
  - hud-playground, fireball-smoke and home-cloud-steward run on geek (the last by a direct PATCH; agent-specs doesn't declare it).
- **The runner view** (`view: runner` on the Fountain block, `illogical fountain --view runner`):
  - the runner's status, version and sandboxes;
  - `HostInfo.fountain_runner`;
  - attention `failed:fountain-runner` when the unit is active but Fountain says it's offline for 5 minutes, or when another runner is online. A deliberately stopped unit raises nothing.
- **Runner conversations:**
  - *Follow* `session/load`s the conversation and never starts a new one.
  - *Changes* is a `run_as: fountain` diff.
  - *Shell* opens a terminal as fountain.
  - Every sudo read is hardened against what a sandbox controls: global and system config are off; hooks, fsmonitor, pagers, external diffs, textconv and every filter driver are disabled; there's no lazy fetch (`protocol.allow=never`); and paths are confined to the unit's root, canonicalized (macOS's `/private/var`).
  - *Shell* runs `bash --noprofile --norc` with `HOME=/home/fountain`, and says it's outside the unit's protections.
  - *Open file* is off for `run_as` diffs.
- **Security found along the way:**
  - claude-agent-sdk puts MCP config on `claude`'s argv, and `/proc` was readable by `fountain`. It was fixed at the source by M44 (#128). `ProtectProc=invisible` on the unit is the second layer: inside a sandbox, `ls /proc` shows only its own 9 PIDs, and Jake's home, `/run/user/1000` and illogical's state are unreachable.
  - The two M45b reviews also found code execution through repo config and lazy fetch in the sudo paths. Both are closed, with a hostile-repo test.
- **R1 and R4 on geek:**
  - A hud-playground conversation ran as fountain in `/home/fountain/sandboxes/runner-<id>-<short>`.
  - The runner view lists geek online and its 4 sandboxes.
  - *Changes* showed `r1-check/hello.txt`.
  - *Shell* opened as fountain in the sandbox, with `HOME=/home/fountain`, and couldn't open `/home/jake`.
  - With the unit stopped and a stand-in `systemctl` reporting "active", *Fountain runner offline* fired after the grace period. It cleared about 80 s after the runner reconnected.
  - The phone wasn't checked: that needs the daily daemon on this release.
- **Still Fountain's (Jake):**
  - runner pinning (`agents.runner_id`);
  - a scoped runner key (the key is readable by the runner's own agents);
  - the `games` environment's broken `love` package.

### No special machines track (S27, M49–M50, added 2026-10-04)

geek has been a hub: its page listed jake-mini (added by hand to its `hosts.json`, with `--allow-origin https://geek.<tailnet>` on jake-mini), and only geek has block sites (`--block-listen`, a wildcard DNS record, a Cloudflare token). Everything that works on geek should work on any machine joined to control, with nothing set up by hand, and control's page is the way in to all of them.

**Decisions (2026-10-04, Jake):**

- **Control's page is the only front door.** It already lists every joined machine, direct or relayed. A daemon's own page shows that daemon (and its tailnet `hosts.json`, kept for setups without control). The plan from the onboarding work, where a joined daemon lists the account's other machines and relays to them with its own key, is dropped: it would make whichever page you open a hub.
- **Blocks go through control, end to end encrypted.** A port or editor block on any joined machine works from control's page with no DNS, certificate or token on the machine. Control still never sees what the block says. A spike decides whether that holds up (S27).
- **The daemon only.** geek stays the Fountain runner and one of the two CI runners; those are placement choices, not illogical's.
- **Done by hand on 2026-10-04:** jake-mini removed from geek's host list, jake-mini reinstalled without `--allow-origin`, both on 0.13.0. geek keeps its block flags until M50 replaces them.

**Order:** S27 (#148) and M49 (#149) in parallel; M50 (#150) after S27 says go. Tracker #151.

#### S27: blocks through control, end to end (#148)

Today a block site is `https://b-<id>.<domain>` on the daemon's own listener (`sites.rs`): the browser must reach the machine over the tailnet, and the machine needs a wildcard name and certificate. Control's page may already frame blocks while the daemon is enrolled (`set_control_origin`), but only where the browser reaches the block origin directly.

The shape to try:

- **Control serves the block origins:** `https://b-<key>.<block domain>`, one wildcard certificate on control. `<key>` is random per block, so origins stay unguessable and separate, as today. The block domain should be a registrable domain of its own (not under `widgets.wtf`), so a block's code is cross-site to control's page and to everything else on `widgets.wtf`; the spike checks what that costs (a service worker in a third-party frame, storage partitioning).
- **A first load serves only a bootstrap page and a service worker,** both control's own static files. The service worker carries every request of the block's page over a Noise channel to the daemon, through the relay (or directly, when the parent page has a direct path), and the daemon answers it from `sites.rs`'s proxy as it does today.
- **The block's key:** the block origin can't use the device key (it lives in control's origin). The parent page makes a one-off X25519 key for the block, signs a grant for it with its device key (scoped to one block, short-lived), and hands both to the frame by `postMessage`. That's read-only links' shape (a one-off key the daemon lists for one session), applied to one block.
- **WebSockets** (hot reload, code-server): a service worker can't intercept them, so `set_head_script` puts a `WebSocket` shim first in the block's pages, carrying them over the same channel.
- **Trust:** control serves the bootstrap code, as it serves its own page's code today; no new party. Write down what a malicious control could do here and how it compares.

**Questions it answers:** does Vite's hot reload, a Next dev server and code-server work through it; the latency added per request against the tailnet path; whether a service worker in a third-party frame registers at all on Safari (iOS and macOS) and in the desktop app's WebKitGTK; and what happens on a hard reload, a crashed worker, a block left open for a day. Go/no-go per browser, as S15 did for PRF.

**Done when:** `spikes/s27-blocks/README.md` has the answers, with a demo of Vite on jake-mini (no block flags) reloading on save inside control's page on geek's Chrome and on the phone.

**Done-when, automated (2026-10-05):** the demo on jake-mini, geek's Chrome and the phone is replaced by tests that need no person: the spike's Playwright suite (Chromium and WebKit on macOS, WebKit on Linux in a container) runs a real Vite, Next and code-server through control's block origins, and `safari/safari.ts` runs the same checks in real Safari and the iOS Simulator through safaridriver on Track E's macOS VM. What's still open is listed in the README under "What still needs a real machine".

#### S27: findings (2026-10-05)

Answers in `spikes/s27-blocks/README.md`. Go for M50, per browser:

| Browser | Verdict | On what |
|---|---|---|
| Chrome / Chromium | **go** | every check, including code-server's webviews; about 0.2 ms added per request on loopback |
| Safari, macOS | **go, provisional** | Playwright's WebKit on macOS passes every check; real Safari waits on `safari/safari.ts` in Track E's VM |
| Safari, iOS | **unknown** | not run; `safari/safari.ts --ios` in the same VM's Simulator |
| Desktop app (WebKitGTK) | **go, provisional** | Playwright's Linux WebKit passes; the app's older Ubuntu 22.04 WebKitGTK not checked yet |

What M50 takes from it: the grant in Noise message 1 (`Responder::read` returning the payload), WebSockets on the page's own channel rather than the worker's, control's worker hosting apps' own service workers (VS Code's webviews need it), `frame-ancestors 'self'` plus control, a stable origin key per block, grant expiry against the daemon's clock from the device channel (a page a day off can't open blocks), and the worker keeping each block's cookie jar. Throughput through the worker tops out near 140 MB/s (WebCrypto), against 300–900 MB/s today.

#### M49: the CLI and the daemon page without a hub (#149)

- **`illogical --host <machine>` through control:** names come from control's directory (the account's and the team's machines), not only `hosts.json`. The CLI connects over a Noise channel, direct when it can, else through the relay, with a `cli` device key of its own (the kind already exists in device certificates). The first use enrolls it the way a browser enrolls (a code to approve on another device). `illogical hosts` lists both sources, marked.
- **A joined daemon's page says where the others are:** the host menu shows *All your machines…*, opening control's page. It doesn't list them itself.
- **Docs:** "A Mac as another host" leads with joining control, and the hand-added tailnet host becomes the setup for people without control.

**Done when:** from a pane on jake-mini, `illogical --host geek run …`, `list` and `capture` work with nothing in jake-mini's `hosts.json`, relayed when Tailscale is down on jake-mini; and the same from geek to jake-air.

#### M49: as built (2026-10-04)

- **Enrollment is a join.** `illogical login [URL]` asks `/api/join` with a `cli` certificate and a proof signed with its new key (as `illogicald join` does), prints `URL/#join=CODE`, polls, and pins the account root from the approval after checking the fingerprint (`--account FP`, or asking). The approve page says *Add a terminal?* and offers no team. Approving adds a device, not a machine, and nudges the account's daemons so they take its key. The URL defaults to the local daemon's control, else the hosted one. Key and pin: `~/.config/illogical/cli-key` and `cli-control.json`.
- **No session cookie.** The CLI signs every request to control with its key, in the daemons' `x-illogical-auth` v2 format (`illogical_e2e::cert::request_auth`, now shared). Control treats a signature from an approved, unrevoked `cli` device as a session for its account; `DaemonAuth` takes daemon keys only. Nothing expires after 30 days.
- **`--host NAME`:** the local daemon's list first, then control's directory (the account's own machines; team and shared machines need another account's root, which the CLI doesn't pin, so they're left out for now). The machine's certificate is checked against the pinned root; the CLI tries each URL the machine lists (`/e2e`, 3 s), then control's relay. One Noise channel per command carries each HTTP request as a `Q`/`R` message, so `run`, `ls`, `capture` and the rest work unchanged; `attach`, `tui` and streamed answers (`events --follow`) don't go through control yet. `ILLOGICAL_VERBOSE=1` prints which way it went. No local daemon is needed.
- **`illogical hosts`** prints control's machines (online, direct URLs or relayed, and which control) above the local list; `--json` has both.
- **Host menu:** a joined daemon's page shows the host button even alone, with *All your machines…* opening control's page.
- **Tests, in place of jake-mini/geek/jake-air:** control-smoke (CI) logs the CLI in and drives a direct and a relayed machine with no daemon of its own; `web/e2e/host-menu-control.spec.ts`; and the testnet `control` profile's `m49` claim: box-systemd (no URL, relayed) and box-bare (listing `http://box-bare:7681`, direct) joined, the CLI on the bastion logged in by the headless device, `run`/`ls`/`capture` on both, `BREAK=1` without the login. The "relayed when Tailscale is down" case is the relayed box: a machine whose URLs don't answer is the same path.

#### M50: blocks through control (#150, after S27)

Built from S27's findings: block sites on control for every enrolled daemon, with the daemon-served schemes (tailnet, dev) kept for people without control. A port or editor block opened on any joined machine shows in control's page wherever that page is open. The host menu says "direct" or "relayed" for blocks too.

**Done when:** on jake-mini and jake-air, with no block flags, *Open a port…* on a Vite server and *Open in editor* both work from control's page on geek, on the phone and in the desktop app; then geek's own block flags are removed and the same holds there.

### SSH track (S28, M51–M53, added 2026-10-04)

A machine you can ssh into should be reachable with nothing set up there first, and ssh should be enough to make it a joined machine. Today every way in (tailnet, dial-out, provider tunnel, control's relay) needs something on the far machine before it works. The herdr comparison (2026-10-04) showed that herdr's whole remote story is plain OpenSSH: its client runs `ssh box herdr remote-client-bridge` and copies itself over when the box has none.

Most of the machinery exists already. `illogical_e2e::mux` (M4c) runs many streams over one link and doesn't care what carries its frames. Dial-out carries them over a WebSocket; this track carries them over ssh's stdin and stdout, to an `illogical bridge` on the box that connects each stream to the box daemon's Unix socket. Reaching that socket already means full control, so an ssh login as the user owns the daemon, the same trust as locally.

**Decisions (2026-10-04):**

- **Clients run ssh, daemons don't.** The CLI, the TUI and the desktop app each run the system `ssh` for themselves, where the user's agent and any password or 2FA prompt are available.
- **No ssh relaying.** A home daemon running ssh and answering for the box at `/h/<name>` (dial-out's shape) was considered and rejected. It would make that daemon a hub, against the "no special machines" decisions, and it would need the user's ssh credentials inside a background service.
- **ssh to set up, control to reach.** For the web, the phone and sharing, ssh is used to install illogical and join the box to control (M52), and control reaches it afterwards like any other machine.
- **Nothing stored but the target.** OpenSSH does all authentication. We keep `user@box` and nothing else.
- **Only the owner's agent reaches panes.** The fixed `SSH_AUTH_SOCK` path links only to an agent forwarded by the box's owner. A guest's forwarded agent is never used in the owner's panes, so on a box several people attach to, `git push` doesn't depend on who attached last.
- **M52 doesn't wait for M49.** Until M49 (#149) ships, M52 shows the box's join code in the terminal and you approve it from the phone or the web. Approving with the CLI's `cli` device key becomes the shortcut once M49 lands.
- **Version skew.** S28 records what a mismatched client and box do today. After that, a client refuses a box whose major version differs from its own and offers to upgrade the box over the same ssh session.

**Order:** S28 (#153) first; M51 (#154) and M52 (#155) after it, in parallel. M53 (#156) is gated (see below). Tracker #157.

#### S28: reach a machine over ssh (#153)

The bridge over stdio, measured against the tailnet path. Installing when the box has no illogical. Whether the daemon outlives the ssh login: systemd linger without sudo, and the launchd domain from an ssh session with no GUI login on jake-mini. Which ssh options to use (ControlMaster, keepalives, `BatchMode` for checks, ProxyJump, Tailscale SSH's check prompt). What version skew does. Whether `illogicald join` works from an ssh session.

Daemon lifetime is answered here, not left to M52, because M52's design depends on it and jake-mini is needed in person either way. The throwaway boxes come from the `ssh` profile of the test stack (#200): a bastion and a bare box with no illogical, built small and first, so the checks can be rerun and later run in CI. The rest of #200 stays out of this track.

**Done when:** `spikes/s28-ssh/README.md` has the answers. From geek, `illogical tui --ssh` installs illogical on a fresh throwaway box and its pane survives disconnects and a logout. The same into jake-mini.

#### M51: the CLI and the TUI over ssh (#154)

`--ssh user@box` on any command, and saved hosts with transport `ssh` for `--host`. Clients already reach hosts directly, so the host list only holds the target. Install when missing, after asking once. While a client is attached over ssh, the box's panes get a fixed `SSH_AUTH_SOCK` path pointing at the owner's forwarded agent (never a guest's), so `git push` works from them. A box on a different major version is refused, with an offer to upgrade it. Web and phone show ssh hosts as reachable from a terminal only, with the M52 step offered.

**Done when:** from jake-air with Tailscale off, `illogical tui --ssh geek` and `--ssh geek run` work. A fresh box gets installed by the first command. `git push` from a pane there uses jake-air's agent. An e2e test drives it against a local sshd.

#### M52: add a machine over ssh and join it to control (#155)

One command: install over ssh, set up the service to outlive the login, run `illogicald join` on the box and show its join code in your terminal, to approve from the phone or the web. Once M49 ships, approving with its `cli` device key (after asking) is the shortcut; M52 doesn't wait for it. Afterwards the box is an ordinary machine on control's page, and ssh is out of the picture. A box that can't reach control says so and stays reachable over `--ssh`. The getting-started steps gain "Add a machine you can ssh into".

**Done when:** from geek, a fresh throwaway box becomes a machine on control's page in one step plus the approval, and its pane opens from the phone. The same for jake-mini with no GUI session. The box survives a reboot.

**Testing it without a person (2026-10-04):**

- The e2e is a claim of the test stack's new `control` profile, `just testnet test control m52`, not a Rust test beside M51's in `ssh.rs`. It drives Docker, ssh, the CLI and an approving device the way a person would, and a shell claim does that with less code; like the `ssh` claims it has a `BREAK=1` form (polkit masked, so no lingering, so the box doesn't come back after `docker restart`).
- The approval comes from a headless device, `web/fixtures/device.ts`: the web client's own e2e code without a page, lifted out of `control-smoke.ts`, which now uses it too. It's TypeScript, not Rust in `crates/e2e`, so it stays the browser's behaviour rather than a second implementation; `device-cli.ts` makes it usable from shell and Rust tests. "Its pane opens from the phone" is checked as the phone's browser does it: a device on the account reaches the pane end to end through control's relay. A real phone's browser is Track B's (phone device contexts).
- In the stack, control has a fixed address on the inner network and that address is its public URL. A daemon accepts plain http only to loopback or a private IP (`private_http` in the daemon's control.rs), so a hostname like `http://control:8080` would be refused; allowing single-label hostnames was considered and not done, since it's a change to what the daemon trusts made only for a test.
- `illogical join` passes `--account` through to `illogicald join`, so the join runs with no terminal to confirm the fingerprint in.
- A box that can't reach control: the CLI recognises `illogicald join`'s "can't reach control at" and says so, naming the box and control, with `illogical --ssh box tui` as the way that still works. The `unreachable` claim checks it on box-bare, which has no route out, joining the hosted control.
- Not covered by the stack: jake-mini with no GUI session (launchd), which needs Track E's macOS harness.

**A Mac with no GUI login (decided 2026-10-04):** Track E's tart VM showed that `illogicald install` over ssh fails for a user who hasn't logged in to the desktop: there's no `gui/UID` domain (`Bootstrap failed: 125`). A Background agent in `user/UID` installs without sudo and survives logging out but not a reboot; a LaunchDaemon with `UserName` survives both and needs sudo once.

- `illogicald install` keeps the GUI-domain LaunchAgent when a GUI session exists. With no `gui/UID` domain it installs the same plist with `LimitLoadToSessionType` Background into `user/UID`, and prints plainly that the daemon won't start again after a reboot until the user logs in or runs `illogicald install --system`.
- `illogicald install --system` installs `/Library/LaunchDaemons/illogicald.USER.plist` with `UserName`. It's run as the user, not root, says it needs sudo and prints each `sudo` command before running it; it never sudoes silently. It removes the user's LaunchAgent, so a later login doesn't start a second daemon. A later plain `install` keeps the LaunchDaemon (an upgrade shouldn't drop boot start); `illogicald uninstall` first goes back to an agent.
- `illogicald uninstall` (new) removes whichever is installed: either agent, the LaunchDaemon (with sudo), or the systemd user service on Linux. Binaries and state stay.
- `illogical --ssh box …` (M51/M52's `prepare`) starts a Mac box's daemon with `illogicald install` and passes its `note:` line through, so `illogical --ssh box join` shows the reboot warning. After a reboot the next `illogical --ssh` to the box starts it again. Upgrading a box over ssh restarts a LaunchAgent; a LaunchDaemon keeps the old binary until it restarts, since that needs sudo.
- Tested by `just macos launchd` (docs/testing.md): default install over ssh with no GUI, the warning, logout, uninstall, the `--ssh` path, `--system` through a VM restart with its pane, and uninstall of the LaunchDaemon, with a `BREAK=1` form.

#### SSH track: the person-free checks (2026-10-04)

Engineering calls made while turning the SSH track's in-person checks into tests (#214):

- **`git push` (M51).** The test stack's `ssh` profile has a `git` server (bare repositories over ssh, `git-shell`, the stack's key). `crates/daemon/tests/ssh.rs` pushes from a pane on box-bare while a client is attached over `--ssh`, and the same push with `ILLOGICAL_SSH_AGENT=no` must be refused, so the push can only have used the forwarded agent. This stands in for "from jake-air".
- **#26 in a container.** `crates/daemon/tests/reboot.rs` uses `docker restart` of box-systemd as the reboot: systemd stops the user service (the daemon logs "saved for shutdown") and boots again with lingering. The container keeps the host's boot id, so the journal is checked by counting lines, not with `-b`. The "phone reconnects" check is the web app in headless Chromium (`web/reconnect-watch.ts`) through an ssh forward the test re-opens after the boot; the daemon's Host check wants `localhost:7681`, so Chromium maps that name to the forward instead of the test changing the daemon. VM tabs across a real kernel reboot stay a separate check (wisp, #214 section 4).
- **A rerun pane forgets its command after one restart.** Found by the reboot test: a restored rerun ran `bash -c 'CMD; exec $SHELL'`, which nothing recorded, so the second restart gave a shell. Fixed with `Start::Rerun` (recorded as `run`'s command is); the clean-stop test restarts twice now.
- **Stacks per worktree.** `COMPOSE_PROJECT_NAME` names a stack's containers, images, networks and state directory, so parallel worktrees don't share boxes or rebuild each other's images.
- **The tailnet comparison (S28)** is done in containers (`tailnet` profile: headscale, a userspace box, a client with `tailscale0`) rather than on geek; numbers in `spikes/s28-ssh/README.md`. Over a real network both paths add the same round trips, so the container numbers are the overhead difference. Tailscale SSH's check mode needs an identity provider's login and stays untested (a limit in #214).

#### S28, M51, M52: as built (2026-10-05)

Merged to main in 9ebcc45 (#209, #210, #211 and #214); #153, #154, #155 and #157 are closed. S28's answers are in `spikes/s28-ssh/README.md`. M51 and M52 work as planned, and every done-when check that named geek, jake-mini or a phone runs as a test instead: the testnet `ssh` and `control` claims (with `BREAK=1`), `ssh.rs` (including the real `git push`), `reboot.rs`, `measure-tailnet.sh` and `just macos launchd`. docs/testing.md's "The SSH track's tests" maps each to the promise it guards. Left: Tailscale SSH's check mode (needs an identity-provider login) and M53 (gated).

#### M53: the desktop app over ssh (#156, gated, after M51)

Gated (2026-10-04). M48 (#159) made the desktop app control's client, so this only covers boxes that never join control. M46 shipped in 0.14.0 (#144), so only M51 is left as a dependency.

- **Trigger:** someone asks for the desktop app on boxes that will never join control (and the CLI or TUI over `--ssh` isn't enough for them).

The host menu lists ssh hosts and has *Connect over ssh…*. The app runs the system `ssh` with M51's options and the user's agent and `~/.ssh/config`, and shows any password or 2FA prompt. It serves only itself, so it isn't a hub. This is the GUI for boxes that will never join control.

**Done when:** on jake-air and geek, the desktop app opens a pane on a box reached only over ssh (Tailscale off, not joined to control), including through a ProxyJump bastion.

#### M65: a pane for a guest who has only OpenSSH (#198, decided 2026-10-04)

Someone with nothing but `ssh` joins one of your panes from a pasted command. This is separate from M51–M53, which are you reaching your own boxes: here the guest has no client, no account and no tailnet. Jake said build it (2026-10-04).

**Decisions (2026-10-04):**

- **The credential is a token in the username**, not a throwaway key. The invite is one command to paste and nothing to save. The username travels inside the encrypted transport, after key exchange, and the host key is pinned and checked before user auth starts, so someone in the middle never gets as far as seeing it. A throwaway key would mean writing a private key to a file with mode 0600 first (ssh refuses a readable one), so the invite becomes two steps and leaves a key on the guest's disk. The token's cost is that it sits in the guest's shell history, and in `ps` on their machine while they're connected. That's acceptable because tokens are short-lived (an hour by default), single use by default, revocable, and the daemon stores only their hash.
- **Auth is ssh's `none` method.** The server accepts `none` for a live token, so the guest's ssh never prompts and never offers keys. A wrong, expired or used token is refused, and `none` is the only method offered, so there's no password prompt to wait at.
- **The host key is pinned in the command**, and nothing is written to the guest's `~/.ssh/known_hosts`:
  `ssh -p 7684 -o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=yes -o 'KnownHostsCommand=/bin/echo [box]:7684 ssh-ed25519 AAAA…' <token>@box`.
  `KnownHostsCommand` needs OpenSSH 8.5 or later (2021). For older clients the invite also gives the known-hosts line on its own, to save into a file named with `-o UserKnownHostsFile=`. The key is ed25519, made once per daemon and kept in the state directory, so pins survive restarts.
- **The daemon runs its own ssh server (russh)**, not the box's `sshd`, on its own port: `--guest-ssh` (default `0.0.0.0:7684`, next to the app's 7681; 7683 is the e2e daemon's; `off` turns the feature off). It listens only while at least one invite exists and closes the port when the last one ends. It has no shell and no accounts. A session can do one thing: a pty and a shell request, attached to the invite's pane. `exec`, subsystems (sftp), port forwarding, agent forwarding and X11 are all refused. That's the attack-surface answer: off by default, on only while you have something shared, and the only pre-auth input it acts on is a hash lookup.
- **Read-only is the default.** `--rw` lets the guest type. A read-write invite is M14's trust grant made up front by the owner, so its life is capped at two hours like a trust grant (read-only invites at a day). Typing still follows the one-driver rule: the guest drives only if nobody else is driving, or in a pair-mode pane; otherwise their keys go nowhere and the terminal title says who is driving.
- **Input is labeled** with the name given at invite time (`--name`, default `guest`), as principal `guest-ssh:<invite id>:<connection>`, so `illogical log --who` and command history show it. A token holder has no other identity.
- **Size and `TERM`.** A read-write guest who is driving sizes the pane from their window (claiming the tab, zoomed to the pane, as `illogical attach` does) and keeps doing so on window changes. A read-only guest never resizes; they see the stream at the driver's size, so a smaller terminal wraps. A plain terminal can't letterbox, and we don't redraw for it. The guest's `TERM` is recorded and not applied: the program in the pane is already running against the pane's terminal, and colors pass through as it writes them.
- **Revocation is immediate.** `illogical guests revoke ID` (and the pane menu) drops live sessions at once. Expiry drops them at the deadline, and closing the pane ends them. A single-use invite is spent at its first successful login: the session lasts, but reconnecting needs a reusable invite (`--reusable`), which also lets several guests watch at once and survives detach and reconnect.
- **Where invites are made:** `illogical share --guest [%N]` prints the command (`--rw`, `--reusable`, `--ttl`, `--name`, and `--addr` for the address the guest should use; it defaults to `--guest-ssh-host` or the machine's hostname). The issue said `share --ssh`, but `--ssh` is M51's global flag for reaching a box, so the invite flag is `--guest`; `illogical --ssh box share --guest %3` makes an invite on that box. `illogical guests` lists invites. The pane menu on the web and phone gets *Invite over ssh…*.
- **Boxes behind NAT go through control's relay with ProxyJump**, as a later step (below). The direct path comes first: a box with an address the guest can reach.

**Status (2026-10-04):** the direct path is built (`crates/daemon/src/guest_ssh.rs`, `illogical share --guest`, `illogical guests`, *Invite over ssh…* in the pane menu). Tests: `crates/daemon/tests/guest_ssh.rs` runs the system `ssh` on a pty against a dev daemon (read-only can't type and a spent single-use token is refused; read-write types as its label, drives, sizes the pane and holds off a second guest; revoke, expiry and pane close end sessions; a wrong token and a different host key are refused, the latter before the token is sent; `exec` is refused; the port closes with the last invite; the CLI's printed command works). `web/e2e/guest-ssh.spec.ts` makes the invite from the pane menu, on desktop and phone viewports, and runs ssh with it. The relay has a skipped test stub, `a_guest_reaches_a_daemon_behind_nat_through_controls_jump_host`.

**The relay (next step, not built):** `ssh -J <route>@<control-host> <token>@<daemon id>`. Control runs an ssh jump host. The route is a second, separate token the daemon registers with control over its relay socket while an invite is live, and withdraws when it ends. At the jump hop control accepts `none` auth for a registered route and nothing else, allows only a `direct-tcpip` channel whose target is that route's daemon, and splices the channel onto a new raw stream over the daemon's dial-out mux. The daemon hands that stream to the same russh server (`run_stream` takes any byte stream), so the inner session is end to end and control sees ssh ciphertext only. Control never learns the secret token: the route opens the hop to one daemon, and the daemon still wants the secret. Host-key pinning names the daemon id with `HostKeyAlias`. What's needed: an ssh listener in control (russh), a route table fed by a new relay-socket message, a raw stream kind on the dial-out mux next to the Noise streams, and its test against the `control` testnet profile (Track A).

**Done when:**
- the direct path: a stock OpenSSH client against a dev daemon, unattended. A read-only guest sees the pane and can't type; a read-write guest types and their input is labeled; a resize from a driving guest reaches the pane; revoke disconnects a live guest at once; an expired invite is refused and a live session ends at expiry; closing the pane ends the session; a wrong token is refused; a different host key is refused by the pinned command; a used single-use token is refused;
- the relay path, when built: the same over `-J` through control, with control unable to read the pane;
- the pane menu entry, with a Playwright test.

### Desktop track (S25, M46–M48, added 2026-10-04)

A desktop app for macOS and Linux: the web UI in a native window, with the app installing, supervising and upgrading `illogicald`. Control stays the SaaS layer, and the browser and phone clients stay as they are. The full plan is #130.

**Decisions (2026-10-03, Jake):**

- **The daemon stays a separate service.** The app bundles `illogicald` and `illogical`, registers the service and upgrades it in place. Sessions outlive the window; if the app owned the PTYs, quitting it would end every pane.
- **Tauri 2, decided by S25.** Electron is the fallback if WebKitGTK isn't usable on geek.
- **macOS and Linux together.** No Windows: it has no daemon.
- **Unsigned macOS builds for now** (ad-hoc signed). Notarization waits for a Developer ID.

**Order:** S25 (done, go), then S26 (#141, done: Tauri stays, native on a trigger), then M46 (window, installer, supervisor, packaging), then M47 (Finder and Nautilus) and M48 (connections in Rust, the device key in the Keychain or Secret Service).

#### S25: desktop shell spike

**Done 2026-10-04: go** (see [spikes/s25-desktop](spikes/s25-desktop/README.md)), on geek's run. Jake called it before the macOS half; those checks are in M46's done-when.

- **WebGL xterm works in WebKitGTK 2.52.** Idle write-to-paint matches Chrome (7 ms). Under full-screen redraws WebKitGTK paints at about 60 fps where Chrome follows geek's 240 Hz display (33 ms against 8 ms); Jake didn't notice it in use.
- **48 of 49 chords reach the page**, Ctrl-W/T/N/Q/Tab included. F10 is GTK's menu-bar key.
- **IME:** Mozc and Hangul commit through xterm.
- **Clipboard:** Ctrl-Shift-C/V work as in Chrome. The page can't write without a gesture, so OSC 52 writes through Rust.
- **No `PushManager`** in the webview: notifications come from Rust.
- Start to the daemon's page: about 280 ms on geek, 215–310 ms on jake-mini. `.deb` 5.8 MB before the daemon.

#### S26: how native can it get (#141)

**Done 2026-10-04** (see [spikes/s26-native](spikes/s26-native/README.md)). The spike recommended the hybrid on performance: a native window, native terminals, native chrome and rail, and web blocks in webviews.

**Decision (Jake, 2026-10-04): M46 stays on Tauri.** The gap is about one frame at 60 Hz, Jake didn't feel WebKitGTK's in use, and every feature in the web client lands once instead of three times. Native terminals wait for a trigger:
- someone feels terminal lag in the Tauri app;
- the macOS app has to compete with Ghostty head to head;
- Tauri hits a wall native wouldn't (keys, WKWebView).

When one fires, start with native terminals on macOS (B2 in AppKit). S26's core is the seed for `crates/client`, which the TUI and M8 can use too.

- **B2 (own renderer over libghostty-vt's render state, both platforms):** one shared Rust core (the daemon connection, a client-side libghostty-vt terminal that never answers queries, keys) under GTK4 (GSK, a render node per row) and AppKit (CoreText, a CALayer per row).
  - It shows output a frame sooner than any browser: on geek, a 4.1 ms log flood against Chrome's 10.6 and WebKitGTK's 31, at 135 MB/s; on the Air, 15–17 ms against Chrome's and Safari's 29–30.
  - IME and real typing worked on both.
  - Missing so far: selection, the mouse, scrollback UI, links, accessibility.
- **B1 (GhosttyKit surface running `illogical attach`):** works, but macOS only, emulates twice, and uses Ghostty's internal API.
- **Level A:** libadwaita tabs and NSWindow tabs around webviews work. Each webview is a whole client, so a hybrid needs an **embed mode** in the web client.
- **Level C:** a native attention rail is 80 lines. Rebuilding every block natively isn't worth it: browser, editor and app blocks are web pages anyway.

#### M46: the app as window, installer and supervisor

See #130. From S25:

- The window loads the UI from the local daemon (`http://127.0.0.1:7681`, already an accepted origin).
- Notifications from Rust, off the attention events the window already gets; the click opens the pane.
- An OSC 52 handler in the client, writing through Rust in the app.
- Clear GTK's F10 binding. On Linux, window buttons in the client's bar when the titlebar is the client's (`data-tauri-drag-region`), in place of the PWA's `env(titlebar-area-*)`.
- macOS: an Edit-only menu (copy, paste, select all), so Cmd-W, T, N and Q reach the page.

**Also done when (S25's macOS half):** on jake-mini, the bench against Chrome; Cmd-W, T, N, Q, H and M reach the page; Japanese and Korean IME; a notification click opens the pane; the dock badge shows the needs-you count. And on geek, a notification click opens the pane.

#### M46: the gaps, as built (2026-10-05)

The audit's M46 gaps, each with a test that needs no person (docs/testing.md, "The desktop app's tests"):

- **SMAppService.** The bundle carries a launch agent (`Contents/Library/LaunchAgents/wtf.widgets.illogical.daemon.plist`) that runs the bundled `illogicald`; a Mac with no daemon registers it on first start, so it shows under Login Items, and an update restarts it on the new binary. Decision: the plist uses `Program` with the fixed path `/Applications/illogical.app/Contents/MacOS/illogicald`, because under an ad-hoc signature launchd can't resolve `BundleProgram` ("The specified path is not a bundle"). An app run from anywhere else, or a Mac where `illogicald install` already wrote a plist, uses `illogicald install` as before. Re-check `BundleProgram` once there's a Developer ID (#177). The daemon logs to `ILLOGICAL_LOG_FILE` (`~/Library/Logs/illogicald.log`), since launchd can't expand `~`.
- **Tabs in the titlebar.** The client's bar is the titlebar: an overlay titlebar on macOS, no decorations on Linux with the bar's own minimize, maximize and close. On macOS new windows (Cmd-N, the tray's *New window*) join as native tabs, and *Move Tab to New Window* is AppKit's. A tab's menu has *Open in new window*. Cmd-W closes the pane, Cmd-T opens a tab.
- **Global hotkey**, off by default: the tray's *Global hotkey* item, or `hotkey_on` and `hotkey` in `desktop.json` (default `Ctrl+Alt+Space`). It hides a focused window and brings it back. On Linux the app stays in the tray when its last window closes while the hotkey is on.
- **`illogical://`**: `illogical://pane/%N` and `illogical://open?cwd=DIR`. macOS through `CFBundleURLTypes`, Linux through the packages' `x-scheme-handler/illogical` and single-instance (an AppImage registers itself).
- **Packaging.** `.deb`, `.rpm` and AppImage for x86_64 and aarch64 (`just desktop-linux ARCH`; the release builds arm64 under qemu-user), and a `.dmg` made with `hdiutil` beside the zip. The `.rpm` declares no glibc version: Tauri writes the whole string as a package name.
- **Updates.** The Tauri updater against `latest.json` on the latest release, registered only in builds with a public key in `tauri.conf.json`. Decision: it updates the macOS app and the AppImage only; a .deb or .rpm belongs to the package manager. A downloaded update is in place at once and runs from the next start (the tray offers *Restart to update*).
- **Notarization** waits on the Developer ID: `scripts/macos-sign` signs, notarizes and staples when the `APPLE_*` secrets are set, and otherwise says it skipped and why.

#### M47: OS integration

See #130.

#### M48: native transport

See #130.

### Command palette (#139, built 2026-10-04)

- **Chord:** Ctrl+Shift+P, and Cmd+Shift+P on a Mac (the desktop app's Edit-only menu leaves Cmd chords to the page). Caught on `window` in the capture phase like the picker's Ctrl+Shift+G, so a focused terminal never sees it. Firefox keeps Ctrl+Shift+P for a private window; the session menu's *Command palette…* opens it there. Blocks in a cross-origin iframe (web pages, editors, studio apps) keep their keys until focus is back on the page.
- **One registry:** `web/src/ui/commands.tsx` builds the pane, tab, `+` and session menus, and the palette reads the same lists, so a menu item is a palette command without more work. A `MenuItem` can carry a `shortcut`, shown in both.
- **Also in it:** jumps to sessions, tabs, panes (in split tabs) and workspace blocks by name; the swarm, and the next pane that needs you (in the tabs or in the swarm).
- **Recent picks** come first, kept in `localStorage` (per browser; nothing synced).
- **Phone:** a full-height sheet like the picker's, from the sheet's *Commands* button; the keyboard stays down until the filter is tapped.
- **Not in it:** pane contents and history (`search`). Names and actions only.

### Agents in terminal panes (#145, #146, #147): as built (2026-10-04)

- **Replay agent.** `crates/vt/fixtures/agents/`: `record.py` records the real Claude Code (2.1.289, haiku) with its timing and state markers; Codex is drawn from the 0.155 screens because it isn't installed where this was made. `replay.py` plays a recording as a program named `claude` or `codex`, waits where someone typed, logs markers and every start's argv, and keeps Claude Code's session file and transcript under a test's own config dir. docs/testing.md has the details.
- **#145 leftovers.** The detection unit tests play both recordings and check every marker, including the transcript view (Ctrl-O) idle and working, which only the title tells apart. `agent_screens.rs` does the same live, with no hooks. Found and fixed on the way: a dialog drawn during the 1 s startup grace and never redrawn (the trust dialog) was never read. `classify::agent` resolves the agent through runners, interpreters, `python -m`, Nix wrappers and Homebrew's `Python`, and `python -c codex` isn't Codex. `GET /api/panes/N/detection` and `illogical describe %N --detection` show each rule's region text and which fired. Still open: the chant agent inventory, and the live check on geek.
- **#147.** `POST /api/panes/N/prompt`, the `prompt_agent` MCP tool and `illogical send %N --wait`: `done`, `needs_input` with the question, `blocked` (already waiting on someone, nothing typed) or `stalled` with the screen's last lines (5 s). Decision: no `until` parameter; a turn that stops at a question returns there, since nothing more happens without someone. Typing alone marks a terminal "working", so where the agent's screen is read, the screen says when it started. Enter goes 150 ms after the text so it isn't taken for a paste. A pane with no agent is never typed at.
- **#146.** `Policy::Resume`; `PaneMeta.session` (agent, id, transcript, cwd, running) from any Claude Code hook's `session_id`, else from `sessions/<pid>.json` under the pane's process. A pane left at the default policy that gets a session switches to `resume`; a policy someone picked (`policy_set`) is kept. On restore: `claude --resume <id>` (or `codex resume <id>`) in the agent's own directory, through `sh -c '"$@"; exec SHELL' illogical claude --resume ID`, so the id is an argument and never shell text; ids are also checked (`[A-Za-z0-9._-]`, 128 at most). A missing transcript or directory, or a bad id, gives a shell with a one-line note. `resume.rs` covers two panes in one repo, a deleted transcript, and a metacharacter id (refused from a hook and from a hand-edited layout). The reboot of a real box (`docker restart`) is Track A's; the helpers in `tests/replay/` work there too. The restart menus (web, TUI) show "Resume Claude Code conversation <id>"; the title isn't looked up yet.
- **Real agents.** `ILLOGICAL_REAL_AGENTS=screen` runs all three against the real Claude Code, on `ANTHROPIC_API_KEY` in its own config dir when that's set.

### Windows track (S29, M54–M60, added 2026-10-05)

illogical on Windows: the desktop app, the CLI and illogicald, as on Linux and macOS. Jake has no Windows machine, so the work is built on GitHub's `windows-latest` runners and tested in a Windows 11 VM on geek.

A survey on 2026-10-04 found:

- **The desktop app** is close to portable: about 15 sites, mostly paths, sidecar names and notifications.
- **The CLI** has about 45 sites: termios raw mode, `nix::poll` loops and the Unix socket.
- **The daemon** has about 150 real sites across about 45 files. Neither the daemon nor the CLI compiles for `x86_64-pc-windows-msvc` today: `nix` is unconditional, and `procinfo` has no fallback.
- **`core`, `proto` and `vt`** have none. libghostty-vt-sys's `build.rs` already maps windows-msvc, but the pinned Ghostty hasn't been built for it.

**Decisions (Jake, 2026-10-04):**

- **Full port, including the daemon.** Panes can run on a Windows machine, not only be driven from one.
- **Built on GitHub-hosted `windows-latest`** in check.yml and release.yml. These runners are free for the public repo and aren't our host runners, so PR triggers are fine.
- **Tested in a Windows 11 VM on geek.** It runs as the `dockurr/windows` container `illogical-win`, reached over ssh. Jake can watch it at localhost:8006.
- **Unsigned for now.** Users click through SmartScreen, as with macOS's ad-hoc signing. Azure Trusted Signing is the upgrade when it matters.

**Shape (from the survey):**

- **Panes run under a ConPTY host.** Windows has no fork, no `SCM_RIGHTS`, no FD store and no process groups, and a pseudoconsole can only be resized or closed by the process that made it. So the Windows shim becomes a per-pane **pty host**.
  - It calls `CreatePseudoConsole`, starts the program in a Job Object, and serves a per-pane named pipe.
  - The pipe carries bytes, resize, close and exit. This is holder's protocol, extended.
  - It is started detached, so it outlives the daemon, and holder's lease and grace rules carry over unchanged.
  - Close is `ClosePseudoConsole`, then `TerminateJobObject` after the kill delay.
- **The local socket is a named pipe** (`\\.\pipe\illogical-<user>`). Its DACL admits only the user's SID, and the peer is checked by SID, as `SO_PEERCRED` checks it today.
- **The service is a Task Scheduler logon task, not a Windows service.** A service runs in session 0 without the user's profile, environment or credentials.
  - The app installs into `%LOCALAPPDATA%\Programs\illogical`, and state lives in `%LOCALAPPDATA%\illogical`.
  - An upgrade renames the running exe aside, because Windows can't overwrite it.
- **The default shell** is `pwsh`, else Windows PowerShell, else `%COMSPEC%`. Shell integration (OSC 133 and OSC 7) comes from a PowerShell profile snippet.
- **Left out on Windows:** the sandbox and Tailscale supervisor, the Fountain runner, code-server, the systemd paths and the tmux `-CC` front. Each is cfg-gated, not stubbed with errors.

**Order:**

1. S29 first.
2. M54, the app as a cloud client, ships alongside S29. It needs no daemon, so Windows users get something early.
3. M55, then M56, then M57 and M58 in parallel, then M59, then M60.

The tracker is #224.

#### S29: Windows feasibility (risks first) (#216)

Answer the questions that could change the shape before any milestone starts:

1. **Ghostty.** Does the pinned Ghostty build lib-vt for `x86_64-windows-msvc` with Zig 0.16, and does the `vt` crate's test suite pass there?
2. **ConPTY host.** A tiny detached pty host keeps `pwsh` running while its parent exits and a new parent reconnects over a named pipe. Measure:
   - the extra hop's latency for echo and bulk output;
   - resize from the new parent;
   - close through the Job Object.
3. **ConPTY's output through `vt`.** ConPTY rewrites the VT stream. Check the reflow after a resize, cursor queries (ConPTY answers DSR itself), and whether full-screen apps (vim, htop-likes) render the same as on Linux.
4. **axum over a named pipe.** A custom `axum::serve::Listener` with a DACL and a SID check on the peer. The CLI's http client reaches it.
5. **Logon task lifetime.** A task registered without admin starts at logon and survives logoff and logon. What happens to detached pty hosts at logoff?
6. **PowerShell integration.** OSC 133 prompt marks and OSC 7 cwd from `$PROFILE`, without breaking user profiles. Also what `cmd.exe` gives us.

**Done when:** `spikes/s29-windows/README.md` has the answers with numbers. In the VM, a demo pty host keeps a `pwsh` pane alive across its parent's restart, and the go/no-go for each later milestone's approach is written down.

**Done (2026-10-05, PR #235): go.** What changed in the milestones below:

- **Ship Microsoft's current ConPTY** (`conpty.dll` and `OpenConsole.exe`, MIT). Windows' own adds a frame, about 16 ms, to every echo; 1.25 echoes in 0.07 ms.
- **The pane host works.** The pipe hop costs about 0.05 ms per keystroke, and a pane survives its parent's ssh session.
- **lib-vt builds under MSVC** once our patch applies to CRLF checkouts.
- **A logon task is the service.** S4U survives logoff but has no DPAPI, so it's an opt-in.
- **Shell integration goes inline** with `-EncodedCommand`, past the `Restricted` policy.

#### M54: the desktop app on Windows as a cloud client (#217)

M48 made the app control's client, so on Windows it starts with sign-in and every machine on control's page, with no local daemon. Until M59 the window shows that this machine can't run panes yet.

- An NSIS installer, per user, with no admin rights.
- Toast notifications whose click opens the pane, through the AUMID the installer registers.
- The tray and single instance, as on the other systems.
- URLs open through the opener rather than `xdg-open`.
- A `windows-x86_64` job in release.yml builds the installer with `cargo tauri build --bundles nsis` on `windows-latest`. A desktop check job runs on PRs.
- The site offers the Windows download.

**Done when:**

- In the VM, the installer from a CI release installs with no admin rights.
- Sign-in through the system browser works, and a device approved from the phone shows geek's panes.
- Typing in a pane on geek works.

Native notifications come from a local daemon, so they arrive with it in M59.

#### M55: the workspace compiles on Windows (#218)

- `nix` and `std::os::unix` are gated, and the Linux-only modules are cfg'd out.
- `procinfo` and `sys` get Windows modules: stubs at first, made real in M60.
- `e2e`'s key-file modes are gated.
- S29's CRLF fix (`build.rs` clones Ghostty with `core.autocrlf=false`, and `.gitattributes` marks our patches `-text`) is what lets the job build lib-vt.
- check.yml gains a `windows-x86_64` job: clippy and the unit tests for every crate except `control`, plus the `vt` tests on Windows from S29. It needs Zig and pnpm through mise.

**Done when:** the Windows check job is green on main and required. No Linux or macOS behaviour changes.

#### M56: illogicald runs panes on Windows (#219)

- The named-pipe transport for the CLI and editors, with the SID check.
- `%LOCALAPPDATA%` paths.
- ConPTY panes inside the daemon, not yet surviving a restart, with resize and close.
  - Through the shipped `conpty.dll`, falling back to the inbox ConPTY when it's missing.
  - Spawned with `STARTF_USESTDHANDLES` and null handles; otherwise a daemon with redirected std handles gives them to the pane (S29).
  - State in `%LOCALAPPDATA%\illogical\state`. The app's installer owns `%LOCALAPPDATA%\illogical` (M54).
- The local socket is a named pipe, served by S29's `PipeListener`: a fresh instance waits while the last one serves. The DACL names the user's SID rather than `OW`, and the CLI retries on error 231.
- The shell defaults from the shape above.
- localauth maps loopback peers through `GetExtendedTcpTable`.
- The integration test harness gets a transport abstraction, so its tests run on Windows CI.

**Done when:** in the VM, `illogicald` runs and the web client on it opens `pwsh` panes, splits and tabs. vim and a long `Get-ChildItem -Recurse` render correctly. The daemon's integration tests pass on the Windows CI job.

#### M57: the CLI and the TUI on Windows (#220)

- Raw mode, size and input through crossterm, with a reader thread in place of `nix::poll`.
- Duplex paths (`attach`, the TUI's WebSocket) use overlapped I/O, such as tokio's named-pipe client. In S29 a blocking pipe handle held a write behind a read for more than 3 s.
- `attach`, `tui`, `run`, `ls` and the MCP server work.
- The tmux `-CC` front stays out.

**Done when:** in the VM, `illogical tui` drives local panes and a remote host. `illogical attach` works in Windows Terminal and in conhost.

#### M58: panes survive daemon restarts on Windows (#221)

The S29 pty host becomes `illogicald _shim` on Windows:

- its exec record;
- the lease and grace rules;
- `holder::collect` over named pipes;
- the watchdog through the Job Object;
- keeping a chunk read but not yet sent when a client leaves (S29's host drops it);
- running hosts from a versioned path, because they outlive upgrades and a running exe can't be replaced.

**Done when:** in the VM, restarting the daemon, killing it, and upgrading it to a new build each keep running panes with their scrollback. The e2e restart test runs on Windows CI.

#### M59: install, upgrade and the app carrying the daemon on Windows (#222)

- `illogicald install` registers the logon task (interactive, no admin). Panes end at logoff, as with launchd and systemd without linger.
- `--survive-logoff` registers an S4U task at startup instead. It says that panes there have no DPAPI, so Git Credential Manager and Credential Manager don't work in them (S29).
- Upgrades work by renaming the running exe aside.
- An `install.ps1` counterpart to `install.sh` (`irm … | iex`), and Windows text in the update notice.
- The desktop app carries `illogicald.exe` and `illogical.exe` (a PowerShell `sidecars.ps1`), installs the daemon when none answers, and upgrades an older one, as on macOS and Linux.
- Windows zips in release.yml, and a Windows machine can join control.

**Done when:** in a fresh VM, the installer alone gives a joined machine whose panes open from the phone, and they survive a reboot. A notification's click opens its pane (moved here from M54, which has no local daemon). Upgrading from the previous release keeps panes running.

#### M60: Windows parity (#223)

- procinfo for real: cwd, argv and the deepest process in the pane's job as its foreground. These feed titles, classification and conversation matching.
- PowerShell shell integration from S29: started with `-NoExit -EncodedCommand`, which wraps the profile's prompt and emits OSC 133 and OSC 7. Windows' default `Restricted` policy blocks any script file, a profile included.
- Conversations' process matching on Windows.
- The README and the site describe Windows like the other systems.

**Done when:**

- In the VM, pane titles follow the running program and cwd as on Linux.
- Prompt marks and cwd come through from `pwsh`.
- A Claude Code session started in a pane shows up as a conversation.

### No person in the loop (#214): as built (2026-10-05)

Merged to main in 9ebcc45. Every manual and real-device check became a test that runs with no person: the testnet profiles (`ssh`, `control`, `tailnet`) and the Docker stacks for forges, two hosts and VS Code; a headless approving device (`web/fixtures/device.ts`); the tart macOS VM harness (`just macos ...`); phone device contexts with a fake push service; a replay agent for terminal agents; `crates/testkit`; Playwright and the testnet in CI. docs/testing.md is the reference. Rules that came with it: missing Docker or tart fails a test (only `ILLOGICAL_SKIP_DOCKER=1` / `ILLOGICAL_SKIP_MACOS_VM=1` skip, loudly); secret-gated tests skip naming the secret; tests wait on what the daemon reports, not on time. Left: #256 (wisp reboot), #257 (iOS Simulator), and #93's github.com half (#264); follow-ups #252-#262.

### Talk track (S30, M61–M64, added 2026-10-05)

A team should be able to talk about a pane, about a session and about everything else, by text and by voice, inside illogical. Control's promise (control.md, "What it can and can't see") still holds: it can't read what it relays and never holds a private key. So control keeps no plaintext messages, and no hosted voice server decrypts audio.

**Decisions (2026-10-05, Jake):**

- **Pane and session threads live on the daemon** that owns the pane and travel over the Noise channels clients already have. The pane's rules apply to its thread: watchers read and drivers post, private panes stay private, and a shared session's guests see its threads.
- **Team channels are ciphertext in control.** MLS (openmls), with each device a member. A new version of the team's signed roster moves the group to its next epoch, so control can't add a reader. Control orders, stores and fans out messages, and reads none of them.
- **Voice is WebRTC on a session,** signaled through the session's daemon. Audio goes peer to peer, encrypted end to end by DTLS-SRTP, and each device signs its DTLS fingerprint with its device key. Control only hands out short-lived TURN credentials, and TURN sees only ciphertext.
- **Agents are in the conversation.** MCP `read_thread` / `post_thread`, and an @mention in a pane's thread goes to that pane's agent.
- **TURN: Cloudflare Realtime TURN for the hosted control** (Jake, 2026-10-05, after S30). Control holds the API key as a Fly secret and hands devices short-lived credentials. The privacy notice names Cloudflare. Self-hosted controls document coturn.
- **Not Slack.** No reactions, file uploads, video or screen share: everyone in a session already sees its panes.

**Order:** S30 (#239) first. M61 (#240) after it. M62 (#241) after S30 and M61. M63 (#242) after S30, in parallel with M62. M64 (#243) is gated. Tracker #244.

#### S30: talk spike (#239)

WebRTC and the mic in each client:
- the desktop app on WebKitGTK (distro build and the AppImage's bundled WebKit), WKWebView on jake-air, and WebView2 on the Win11 VM;
- Chrome, Firefox and Safari;
- the phone PWA in the background.

Where WebKitGTK has none, measure str0m or webrtc-rs with cpal on the Tauri side. For openmls in wasm: size, join and commit times at 2, 10 and 50 devices, and how invites, *Ask me first*, removal, locking and removed devices map to commits and Welcomes. Also: offline devices several epochs behind; what a hostile control gets from dropping or reordering messages; whether team daemons should be MLS members so agents can post; coturn on Fly against Cloudflare's TURN.

**Result (2026-10-05, PR #270): go for both.**
- **Voice:**
  - WebRTC, including relay-only calls through TURN, works in Chrome, Firefox, Safari, WKWebView and Edge/WebView2.
  - **No distro's WebKitGTK has WebRTC** (upstream builds it only with experimental features), so the Linux desktop app runs calls in Rust: webrtc-rs + Opus + cpal + AEC3. That path called Chrome on jake-air through TURN: 15 ms RTT, 0 loss, about 2% of a core.
  - Signed fingerprints caught a hostile signaling server.
  - TURN: Cloudflare for the hosted control.
- **Channels:**
  - openmls runs in wasm everywhere: 540 kB gzipped; 8 ms to join and 11 ms to commit at 50 devices.
  - M62 needs a separate MLS key per device, bound by the device key (WebCrypto signs asynchronously), `max_past_epochs` of 3–5, and merging its own commits only after control accepts them.
  - Team machines aren't channel members.
- **Live checks (Jake):**
  - Mac mic prompts work.
  - On the iPhone, the PWA call is clean in the foreground, but **iOS stops the mic in the background** (playback continues), and there was feedback after coming back. M63 shows "muted (app in background)" and gets a fresh mic track when the app is visible again.
  - Still open: native mic plus AEC3 in a live call on Linux (deferred to M63's done-when), and Jake creating the Cloudflare TURN key (needed by M63).

**Done when:** `spikes/s30-talk/README.md` has a go/no-go per client and these two demos:
- a call between geek's desktop app and jake-air's Chrome through TURN, with direct UDP blocked;
- two accounts exchanging MLS messages through a stub delivery service, with a member removed mid-conversation who can't read anything sent after.

#### M61: threads on panes and sessions (#240)

- Each pane and session has a thread, held by the daemon with the pane's state and kept in history after the pane closes.
- Quote a scrollback range as a block that jumps to the output.
- @mentions notify through push.
- MCP `read_thread` / `post_thread`. An @mention of the agent from someone who drives goes to the pane's agent as a follow-up.
- An unread thread is a channel in the swarm themes.

**Done when:**
- Two accounts on a team talk on a team pane from geek's desktop app and the phone.
- A watcher can't post, and a private pane's thread doesn't reach a member.
- A shared session's guest sees its threads, with and without history.
- An agent answers an @mention.
- Threads survive daemon restarts and in-place upgrades (Playwright).

#### M62: team channels (#241, after S30 and M61)

- Each team has channels, starting with `#general`.
- MLS with each device a member, driven by roster versions; *Lock* leaves only owners.
- Control keeps ciphertext, ids, sizes, times and sender device ids.
- New devices read from the epoch they joined.
- Pane and session links open subject to the reader's own access.
- control.md, control-e2e.md, the terms and the privacy notice are updated before the hosted control gets it.

**Done when:**
- Three accounts chat from the desktop app, Chrome and the phone.
- A member removed mid-conversation reads nothing after, and nothing in a copy of control's database decrypts.
- *Lock* cuts members off within a second.
- A device offline across three roster changes catches up.

#### M63: voice on a session (#242, after S30)

- *Join call* in the session menu, with who's speaking on presence and in the swarm.
- The daemon signals and admits people by the same rules as viewing.
- Peer to peer up to S30's limit (about 5), with signed DTLS fingerprints checked against device certificates.
- TURN from control.
- On Linux the desktop app runs the call in Rust (S30's native peer: webrtc-rs, Opus, cpal, bundled AEC3). The page keeps signaling and the UI and hands the SDP over through a Tauri command. The mic prompt goes through wry's `with_permission_handler`.

**Done when:**
- A 30-minute call on a shared session with geek's desktop app, jake-air and the phone, one of them forced through TURN.
- A revoked guest drops out within a second.
- After a daemon restart, *Join call* brings everyone back.

#### M64: bigger calls and calls in channels (#243, gated, after M63)

- **Triggers:** a team regularly wants more people on a call than peer to peer handles, or people want a call that isn't on a session.

An SFU with SFrame, keyed from the MLS group or the session's participants, never by the SFU. Channel calls signaled through control with device-signed messages.

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
