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
- `illogical workspace [DIR]`;
- *Open as workspace* in a pane's menu and the picker when a directory holds a `chant.workspace.json`;
- an MCP `open_workspace`;
- web cards, the phone sheet (gates first) and a TUI line.

**Tests:** the composer's fixtures, daemon tests for gate attention and approve, and an e2e spec with a toy gated op (CI needs node and a pinned chant).

**Done when:** on geek, `illogical workspace ~/dev/intentius/chant` (after `npm install`) shows its members, and a gated op shows as attention within about 5 s. The phone can approve it, and the next `chant run` walks through.

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
