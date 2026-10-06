# S1: libghostty-vt snapshot fidelity
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s1-ghostty/<file>`.

Run 2026-10-01 on geek. **Result: pass.** With a ~150-line fix-up layer
on top of libghostty-vt's formatter, a snapshot reproduces the source
terminal exactly, in Ghostty and in xterm.js, for every fixture, including
nvim with scrollback underneath.

## How it works

- `record.py` drives programs in a real PTY at 100x30 (keystrokes, resizes) and
  saves the raw output (`fixtures/<name>.bin`) plus resize offsets (`.json`).
- `src/main.rs` feeds a fixture into terminal A, snapshots A, feeds the
  snapshot into a fresh terminal B, and compares:
  - every visible cell (graphemes + style + background);
  - cursor position, visibility, shape and blink;
  - 20 DEC/ANSI modes;
  - title, cwd, palette, Kitty keyboard flags, scrollback row count;
  - full plain text and VT text including scrollback.
  For alt-screen fixtures it then sends `?1049l` to both and repeats every
  check on the primary screen underneath. It also checks that taking the
  snapshot does not change A.
- `xterm-check/check.mjs` replays each snapshot into `@xterm/headless` 6.0
  and compares text (including scrollback), cursor, active buffer and input
  modes.

```
PATH=$HOME/.local/opt/zig-x86_64-linux-0.15.2:$PATH cargo run --release
(cd xterm-check && npm i && node check.mjs)
```

## Results

| fixture | what | ghostty -> ghostty | xterm.js |
|---|---|---|---|
| seq | 5000 lines, deep scrollback | OK | OK |
| modes | 256/true color, OSC 8, OSC 4, bar cursor, mouse 1006, bracketed paste, wide/emoji/combining, title, OSC 7 | OK | OK |
| nvim | 300 lines then nvim with vsplit, numbers, cursorline | OK, primary + scrollback intact after exit | OK |
| nvim_resize | nvim resized 100x30 -> 70x20 | OK | OK |
| less | alt screen pager | OK | OK |
| top | full-screen redraw | OK | OK |
| resize | long lines narrowed 100 -> 60 (reflow) | OK | OK |

A snapshot takes 1–3 ms and is 6–35 KB for these fixtures. The cost is
mostly the trailing-row scan, which can be made cheaper.

## What the formatter gets wrong, and the fix-ups

These are libghostty-vt at the commit pinned by `libghostty-vt-sys` 0.2.2
(`a887df42`). Items 1–4 and 6 are worth upstream issues or PRs.

1. **Cursor clobbered by tab stops.** It emits CUP, then the tab stops extra
   moves the cursor with CHA and never puts it back. Fix: emit CUP again at the end.
2. **Title not emitted.** Fix: OSC 2 from `Terminal::title()`.
3. **Cursor shape and blink not emitted.** Fix: DECSCUSR from `RenderState`.
4. **Textless rows at the bottom are dropped**, including rows that only have
   erased backgrounds. The screen shifts up a row on replay and the bottom
   row loses its background. Fix: re-add them with CRLFs, then repaint their
   background cells inside DECSC/DECRC.
5. **Only the active screen is formatted.** This is the expected alt-screen
   gap. Fix:
   - flip A to the primary screen with `?47l` (no clear), format it, and
     flip back with `?47h`;
   - reset the `?47` flag with `set_mode` (no screen switch);
   - emit `?1049h` and `CSI H` ourselves;
   - format the alt screen with the modes extra off;
   - emit modes ourselves.
   Don't emit CUP before `?1049h`: mode 47 carries the alt cursor across, and
   the saved cursor that `1049l` restores is not exposed. Leaving the cursor
   after the primary content matches the normal case. The proper fix upstream
   is a C API option to format a chosen screen, plus a getter for the saved
   cursor.
6. **NUL byte inside the OSC 7 it emits** (`file://geek/tmp\0`). Harmless for
   xterm.js, but it should be reported.

## Gaps not covered yet

- No `claude` fixture yet: record one interactively before M1.
- No Kitty graphics, sixel or images.
- No DECSLRM, origin mode or protected cells under a scroll region.
- Saved cursor (DECSC) state is not carried over.
- The mouse *encoding* (1006 vs default) can't be read back from xterm.js,
  only that tracking is on. Ghostty->Ghostty confirms 1006 survives.

## Build notes

- The crate's pinned Ghostty commit needs **Zig 0.15.2 exactly**; 0.16 fails.
  Both are under `~/.local/opt`, and `~/.local/bin/zig` points at 0.16. Put
  0.15.2 first on PATH for builds (the workspace `justfile` will do this).
- First build takes about 45 s (Zig compiles Ghostty) and links statically.
- `max_scrollback` behaves like bytes, not lines: 10,000 kept only 796 rows
  of `seq` output. Use a byte budget (e.g. 50 MB) and verify against Ghostty's
  docs before relying on it.
- libghostty-vt types are `!Send`, so each pane's terminal lives on one
  thread. Plan for a VT thread (or a few) that owns the engines, fed by channels.
