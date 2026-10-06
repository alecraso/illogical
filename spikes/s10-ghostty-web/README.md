# S10: ghostty-web in the browser, and attach time
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s10-ghostty-web/<file>`.

Run 2026-10-01 on geek. **Result: M8's trigger is not met.** ghostty-web's
engine reproduces the daemon's screen for 6 of the 7 S1 fixtures in every
browser tried, but it runs a ten-month-old Ghostty, can't read GHOSTSNP, has
real bugs, and lacks APIs the client needs.

**Visible-first attach is not worth building for the xterm.js client.** The
slow part of a deep attach on a phone is the 3.9 MB uncompressed snapshot on
the wire, not drawing it. Two cheaper changes get a 64k-row attach on a
throttled phone from 3.7 s to about 0.35 s:

- compress snapshot frames;
- send only the history the client keeps.

## Setup

- **ghostty-web** (coder/ghostty-web, MIT), both builds on npm:
  - `0.4.0`: `latest`, released 2025-12-09.
  - `0.4.0-next.20.g1858a59`: `next`, 2026-06-28; repo `main` is the same commit.
  - Both embed Ghostty `5714ed07a` (2025-12-01) plus a 65 KB patch (`patches/ghostty-wasm-api.patch`).
- **xterm.js** `@xterm/xterm` 6.0.0, with unicode11 0.9.0 and webgl 0.19.0, configured as in `web/src/terminal-view.ts`. The harness uses WebGL except on touch devices.
- **Upstream libghostty-vt wasm.** Built from S5's pinned Ghostty `22d13172` (the daemon's engine) with Zig 0.16:
  - command: `zig build -Demit-lib-vt -Dtarget=wasm32-freestanding`;
  - ReleaseSmall is 851 KB (262 KB gzip); ReleaseFast is 5.3 MB.
  - It is the VT core only, with the full C API including `ghostty_snapshot_*`, and no renderer.
- **Reference.** `gen/` uses `crates/vt`'s `GhosttyEngine` (libghostty-rs `8953a74`). For each S1 fixture it writes:
  - `plain_text()`;
  - the formatter VT `snapshot()` (the exact bytes the client gets);
  - the raw GHOSTSNP;
  - the visible cell grid.

  It also writes three attach cases at 120x40:

  | case | contents | VT bytes |
  |---|---|---|
  | small | a prompt and 20 lines | 7 KB |
  | 10k | 10k coloured lines | 616 KB |
  | 64k | 200k coloured lines, 64,511 kept (as in S5) | 3.9 MB |

- **Browsers.** Playwright 1.63:
  - Chromium 153 on desktop and as a Pixel 7 (DPR 2.625, touch);
  - WebKit 26.6 on desktop and as an iPhone 15 (DPR 3, touch).
  - WebKit needed six host libraries that weren't installed. They came from `apt-get download` and were unpacked into its `minibrowser-wpe/sys/lib` (no root needed).
- **Throttling.** On Chromium only, via CDP:
  - CPU with `Emulation.setCPUThrottlingRate` 4;
  - network with `Network.emulateNetworkConditions` at 10 Mbps and 50 ms latency.

  WebKit can't be throttled, so its numbers are desktop CPU speed.

```
cd gen && PATH=~/.local/opt/zig-x86_64-linux-0.16.0:$PATH CARGO_TARGET_DIR=../work/target \
  LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast cargo run --release   # -> work/data
(cd work && npm i ghostty-web@0.4.0 ghostty-web-next@npm:ghostty-web@next playwright@1.63.0 \
  @xterm/xterm@6.0.0 @xterm/addon-unicode11@0.9.0 @xterm/addon-webgl@0.19.0 @xterm/addon-fit@0.11.0)
SHOTS=1 ./run.sh fidelity.mjs     # Q2, -> work/fidelity.json, work/shots/
./run.sh attach.mjs 3             # Q3 + Q4, -> work/attach.json
node ghostsnp-fixtures.mjs        # Q3 on the S1 fixtures
./run.sh debug.mjs; ./run.sh dpr.mjs   # probes behind individual findings
```

The page (`page.html`, `page.mjs`, `engines.mjs`, `ghostsnp.mjs`) is served
by `server.mjs` on 127.0.0.1:7791. No illogicald was needed, because `gen/`
produces the daemon's bytes directly.

## Q1: what ghostty-web is today

- **Activity.** One part-time maintainer team at Coder.
  - 0.4.0 is the last release. A 0.5.0-rc PR (#182) has been open since June.
  - The last commit was 2026-06-28.
  - The roadmap issue (#156, April) asks whether the project is still maintained.
  - 29 open issues and about 40 open PRs, many of them external fixes waiting on review. PR #162 is a WIP move to Ghostty 1.3 and its C API.
- **Upstream.** ghostty-org/ghostty has no web package and no renderer.
  - It does build `ghostty-vt.wasm` (`example/wasm-vt`), with the whole libghostty-vt C API: formatter, render state, snapshot, search, Kitty graphics state.
  - ghostty-web's README says it "will eventually consume a native Ghostty WASM distribution once available". Nothing like that has been published yet.
- **Size, gzip -9:**

  | build | size |
  |---|---|
  | ghostty-web `next` (wasm inlined into the JS as a data URI) | 197 KB |
  | xterm.js + webgl + unicode11 + fit | 136 KB |
  | upstream VT-only wasm, ReleaseSmall | 262 KB |

  Superlogical's "218 KB" is a different build.
- **API compared with what `TerminalView` uses:**

  | need | ghostty-web |
  |---|---|
  | `write(Uint8Array, cb)`, `resize`, `onData`, `reset`, `focus`, `getSelection`, `selectLines`, `attachCustomKeyEventHandler` | present. The key handler's return value is inverted (#192). |
  | `parser.registerCsiHandler/OscHandler` (to swallow queries; OSC 133/633 marks) | **missing** |
  | `registerMarker`, `registerDecoration` (command marks) | **missing** |
  | `onBinary`, `term.modes` (appCursor, mouse tracking) | **missing**. `wasmTerm.getMode()` and `hasMouseTracking()` exist. |
  | `onTitleChange` | fires only for **string** writes; the client writes `Uint8Array`, so it never fires |
  | Unicode11 / WebGL addons | none. Canvas 2D only (WebGL requested in #155); only `FitAddon` ships. |
  | `buffer.active.getLine().translateToString()` | collapses never-written cells (`x`, 10 blanks, `y` gives `"xy"`) and returns only a cell's first codepoint (`👍🏽` gives `👍`, `é` gives `e`) |
  | query replies | it answers DA/DSR/etc. itself through `onData`, so the program would get a second reply after the daemon's. No option to turn this off (PR #165 open). |

- **Open bugs that matter:**
  - `scrollback` is bytes, not lines (#140; reproduced: 10000 keeps 854 rows).
  - `write('')` throws (#199).
  - One throw in `render()` kills the render loop for good (#189).
  - It repaints every frame at fractional DPR (#198; not reproduced at Pixel 7 / 80 cols, where the canvas width happens to be an integer).
  - The render loop runs at 60 fps even when idle.
  - IME: composition is drawn in the corner (PR #190), Hangul doesn't work (#119, PR #120).
  - No screen-reader support (#187).
  - It needs a CSP that allows data: URI fetches (#188).
  - Kitty graphics exists only as an unmerged PR (#197).
  - The selection work was merged in February; there is no touch selection.

## Q2: fixture fidelity

Each fixture was fed two ways:

- **raw:** its recorded PTY bytes, with the recorded resizes;
- **snap:** the daemon's formatter VT snapshot, which is what the client receives.

It was compared with the daemon engine on:

- the text including scrollback, as `plain_text()`;
- the cursor;
- the active screen;
- every visible cell: text, width, bold/italic/faint/underline/inverse/strike, fg, bg.

ghostty-web only exposes resolved RGB, so the reference's palette colours
were resolved through ghostty-web's palette, read back from a probe terminal.

**Every fixture gave the same result in all four browser setups** (Chromium,
Pixel 7, WebKit, iPhone 15). That's expected: the engine is wasm, and only
drawing differs between browsers. Each fixture ran in a fresh page (see
"stale cells" below).

| fixture | xterm.js 6, raw | xterm.js 6, snap | ghostty-web (both builds), raw | ghostty-web, snap |
|---|---|---|---|---|
| seq (5k lines) | identical | identical | top 4,248 of 5,001 lines missing with `scrollback: 10000` (bytes). Identical with 64 MiB. | same |
| modes | `link` cells report underline (OSC 8 styling) | identical | **differs:** `👍🏽` is one 2-wide cell (the daemon makes two, 4 columns), so the rest of the line shifts 2 columns left | same |
| nvim | identical | identical | identical engine state; only `translateToString` collapses blank cells | identical |
| nvim_resize | identical | identical | identical | identical |
| less | identical | identical | identical | identical |
| top | 2 cells in the last column get inverse/bold | identical | identical | identical |
| resize (reflow) | identical | identical | identical | identical |

Notes:

- **Emoji width.** ghostty-web turns grapheme clustering (mode 2027) on by default; the daemon's libghostty has it off. xterm.js with unicode11 agrees with the daemon. The formatter snapshot doesn't emit `?2027l` because off is the default. One mode would fix it, but today the client and server disagree on columns.
- **OSC 4 is ignored.** Palette changes are ignored for drawing: `\e]4;1;…` leaves red cells in the theme red. The daemon and native Ghostty recolour them.
- **Stale cells across terminals.**
  - In one page, a new ghostty-web terminal shows rows left over from a disposed one: `xy` from the previous terminal, or garbage CJK when scrollback is 64 MiB. Reproduced in `debug.mjs`, and in `work/fidelity-reused-instance.log` where every fixture after the first picked up junk.
  - The client opens and closes panes all the time, so this alone blocks adoption.
  - It's in the wasm patch's page handling: #141, PR #142 (zero-init) and PR #134 are open.
- **Byte-based scrollback.**
  - With 64 MiB, ghostty-web kept 44,417 rows of the 64k case, where the daemon keeps 64,511.
  - The daemon's own wasm build keeps only 56,384 rows in 64 MiB, because wasm32 pages hold fewer rows per byte. About 76 MiB keeps them all.
  - A byte budget therefore doesn't mean the same history on the client and the server; use a line limit (`SCROLLBACK_MAX_LINES`) on both.
- **Screenshots** are in `work/shots/`, for nvim, modes and top, xterm.js against ghostty-web, desktop and phones.
  - nvim looks the same apart from cell metrics: ghostty-web's cells are 9x15 px against xterm's 8x18.
  - In modes, ghostty-web draws `👍🏽` as one glyph; xterm.js draws 👍 plus a skin-tone swatch, matching the daemon's columns.

## Q3: GHOSTSNP in the browser

- **ghostty-web: no.** Its wasm has no `ghostty_snapshot_*` exports. Its Ghostty predates the snapshot API (August 2026), and GHOSTSNP isn't stable across Ghostty commits anyway (S5).
- **Upstream libghostty-vt wasm at the daemon's commit: yes.**
  - Every S1 fixture's GHOSTSNP decodes to text identical to `plain_text()` (`ghostsnp-fixtures.mjs`).
  - So does the 64k case, once the scrollback budget is raised to 80 MiB.
  - The screen at READY equals the final screen.

Decode in the browser, 64k case (11.07 MB GHOSTSNP), median of 3, ReleaseSmall:

| | READY (copy into wasm + header + screen) | rest of history (166 pages) | longest page |
|---|---|---|---|
| desktop Chromium | 4.8 ms | 76 ms | 2.5 ms |
| Pixel 7, CPU 4x | 20 ms | 303 ms | 9 ms |
| iPhone 15 (WebKit, unthrottled) | 6 ms | 67 ms | 1 ms |

- Most of the READY time is copying 11 MB into wasm memory. A streaming `ghostty_snapshot_decoder_new` with a reader would reach READY after the first chunk. For the 10k case, READY takes 3 ms on desktop and 12 ms on the throttled Pixel.
- ReleaseFast is no faster (5.3 ms / 23 ms at READY) and is 6x bigger.
- There is no renderer for this build. Drawing from it means writing one on top of the render-state API, or ghostty-web moving to it (PR #162).
- **GHOSTSNP isn't small on the wire.** The 64k case is 11 MB raw, 299 KB gzip, 150 KB zstd -3. Uncompressed, it took 9.0 s at 10 Mbps.

## Q4: attach time (the gate)

The page fetches the snapshot (standing in for the WebSocket frame), then
writes it into a fresh terminal like `Client.onFrame` (`reset()` + `write`).
It times three things:

- **net:** time until the bytes are in hand;
- **drawn:** the write callback, then the next render of the final screen;
- **total:** net plus drawn, i.e. usable.

All numbers are ms, medians of 3, with a fresh browser per run.

| config | case | xterm.js, sb 10k (today) | xterm.js, sb 10k, gzip wire | xterm.js, sb 100k | ghostty-web next, 64 MiB |
|---|---|---|---|---|---|
| desktop Chromium | small | 23 | 28 | 24 | 49 |
| | 10k | 57 | 63 | 61 | 49 |
| | 64k | 100 | 110 | 165 | 116 |
| Pixel 7, CPU 4x | small | 164 | 164 | 166 | 144 |
| | 10k | 283 | 276 | 277 | 196 |
| | 64k | 422 | 426 | 659 | 441 |
| **Pixel 7, CPU 4x, 10 Mbps / 50 ms** | small | **203** | 206 | 206 | 182 |
| | 10k | **805** (552 of it net) | **336** | 804 | 691 |
| | 64k | **3,684** (3,244 net) | **651** | 3,930 | 3,623 |
| iPhone 15 (unthrottled) | small | 58 | 63 | 60 | 227 |
| | 10k | 103 | 105 | 103 | 229 |
| | 64k | 160 | 156 | 283 | 343 |

What this shows:

- **On the throttled phone, the network is the problem.** The 64k snapshot is 3.9 MB on the wire, because there's no permessage-deflate and the frame isn't compressed. That's 3.2 s of the 3.7 s.
  - gzip-6 cuts it to 183 KB (zstd -3: 58 KB) and the total to 0.65 s. Decompressing (`DecompressionStream`) costs very little.
  - The synthetic lines compress far better than real output will. Expect 5–20x, not 20–70x.
- **The client discards most of what it gets.** xterm keeps 10k lines, so 54k of the 64k rows are parsed and thrown away.
  - Sending only the newest ~10k rows of history (the client's scrollback) makes the 64k case cost what the 10k case costs: 0.34 s with gzip on the throttled phone, 0.8 s without.
  - The daemon can do this cheaply. The formatter writes history first; formatting the READY terminal of a GHOSTSNP decode gives the screen, and a capped history can be prepended.
- **The floor is about 0.15–0.2 s on the throttled phone.** Even the small screen takes 143 ms to draw: xterm.js setup plus the first DOM-renderer paint at 4x CPU. Visible-first attach could only reach that floor. After the two changes above, it would save roughly 0.15 s per deep attach at 4x CPU and nothing on desktop or the iPhone.
- **ghostty-web is not faster here.**
  - It parses faster: 342 ms against 425 ms for 64k at 4x.
  - Its `write()` is synchronous, so the main thread is blocked for the whole parse (366 ms at 4x CPU for 64k). xterm.js parses in slices and stays responsive.
  - Its first frame is slow on WebKit (~170 ms even for the small case).
- **GHOSTSNP's READY is fast** (20 ms at 4x CPU, bytes in hand). M8's "usable within 50 ms" is plausible for the decode, but not for the network, which adds 50 ms of RTT on its own. Measure the target from bytes-in-hand, and compress GHOSTSNP on the wire (zstd, 150 KB for 64k).

**Recommendation for the gated visible-first attach: don't build it for the
xterm.js client.** Do these instead, both small and server-side:

1. Compress snapshot frames, and ideally all output frames: permessage-deflate on the axum WebSocket, or zstd snapshot frames.
2. Cap the history in a wire snapshot at the client's scrollback (10k lines), or a `hello` field.

With both, a 64k-row attach on a 4x-throttled phone at 10 Mbps is about
0.35 s, against 3.7 s today. M8 (ghostty-web + GHOSTSNP) makes visible-first
native later, through the decoder's READY/HISTORY split, so building the
offscreen-xterm swap now would be throwaway work.

## Verdict on M8's trigger: not met

What's missing:

1. **Engine version.** ghostty-web embeds Ghostty from 2025-12-01. The daemon runs `22d13172` (2026-08-06) and will move again.
   - There's no snapshot API in ghostty-web's wasm.
   - Its byte-budget rows differ from the daemon's.
   - The upgrade (PR #162) is WIP, and the project has stalled since June.
2. **Fidelity.** 6/7 fixtures match. modes doesn't, because the default grapheme-clustering mode differs (2027 on in ghostty-web, off in the daemon). OSC 4 palette changes aren't drawn.
3. **Bugs that block the client:**
   - stale cells leak from disposed terminals into new ones;
   - scrollback is in bytes;
   - `write('')` throws;
   - a dead render loop after any throw;
   - it replies to queries itself (duplicate DA/DSR answers alongside the daemon's);
   - `onTitleChange` doesn't fire for byte writes.
4. **API gaps for `TerminalView`:**
   - no parser hooks, so no query swallowing and no OSC 133/633 marks;
   - no markers or decorations;
   - no `modes` or `onBinary`;
   - the buffer text API drops blank cells and grapheme tails;
   - no WebGL renderer.
5. **Phone input.** IME is broken for CJK and Hangul. There's no screen-reader support and no touch selection. None of this was tested on a real device.

One way to get there that doesn't wait on Coder: drive upstream
libghostty-vt wasm, the daemon's exact commit with GHOSTSNP (Q3), and write
or borrow a renderer for it. Or fork ghostty-web onto it, which is what
PR #162 starts to do. Re-check when ghostty-web ships a release on Ghostty
1.3+ or upstream publishes a web package.

## Still open

- **Real devices (for Jake):**
  - attach a 64k-row pane on the iPhone over `tailscale serve` on cellular, and time it;
  - type into ghostty-web's demo (`npx @ghostty-web/demo@next`, loopback) on the phone: soft keyboard, autocorrect, IME, selection.
- How well real terminal output compresses, so the 5–20x guess can be replaced. Record a day of `claude` and build output and gzip/zstd it.
- Whether CDP throttles WebSocket frames like fetch. The harness used fetch.
- A renderer for upstream libghostty-vt wasm, and what drawing from READY costs. Not measured: nothing draws from it today.
- Kitty graphics, sixel, IME and selection fidelity weren't exercised.

## Cleanup

- No daemon was started. The only server was the harness's static server on 127.0.0.1:7791, which each script closes; nothing is left running.
- Nothing outside `spikes/s10-ghostty-web/` was changed. No commits.
- `work/` (git-ignored) holds about 1.3 GB:
  - the Playwright browsers (930 MB, plus the unpacked WebKit libraries);
  - a copy of the Ghostty source with the wasm builds (311 MB);
  - npm modules;
  - generated data and screenshots.

  The cargo target dir has already been deleted. Remove the rest with `rm -rf spikes/s10-ghostty-web/work`.
- `node_modules` here is a symlink into `work/`, ignored by this directory's `.gitignore`.
