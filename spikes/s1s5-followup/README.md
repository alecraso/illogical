# S1/S5 follow-up: new fixtures, both snapshot paths, Ghostty main
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s1s5-followup/<file>`.

Run 2026-10-01 on geek. **Result:**

- **GHOSTSNP (checkpoints): pass.** Every new fixture round-trips exactly,
  including the saved cursor, margins, origin mode and protected cells.
  Kitty images are the one exception: they are dropped by design.
- **Wire (`crates/vt` `snapshot()` today): fail on 13 of 18 fixtures.**
  - A padding bug shifts the screen whenever the cursor sits below the last
    line of text, or inside a scroll region.
  - The cursor is off under origin mode.
  - The saved cursor, per-cell hyperlinks and protection are lost.
  - Blank cells pick up the colours of the text before them. Claude Code's
    banner shows this.

  A prototype with the fixes (`src/patched.rs`) makes all 17 non-image
  fixtures exact in Ghostty and in xterm.js.
- **Cross-build: confirmed, and the failure is clean.**
  - A GHOSTSNP written by the pinned build fails to decode in Ghostty main
    with `INVALID_VALUE`, and the reverse fails the same way. No terminal is
    returned.
  - `build_info` doesn't identify the commit in either build, so M2's tag
    is only a hand-written string.

## Setup

- `crates/vt` as it is in the repo: libghostty-rs `8953a74`, which pins
  Ghostty `22d13172` (2026-08-06), with Zig 0.16.0 from the repo
  `.mise.toml`.
  - The harness depends on `crates/vt` by path, so the wire path runs the
    daemon's own `snapshot()`.
  - It is built in `work/target`, never the repo's `target/`.
- Ghostty `main` `0081d453` (2026-10-01 08:21 -0400), 1,028 commits after the
  pin.
  - `minimum_zig_version` is 0.16.0 in both, so the same Zig builds it
    (`mise exec zig@0.16.0`).
  - libghostty-rs `master` is still `8953a74`; no newer pin exists. So main
    was built straight from source (`zig build -Demit-lib-vt=true`).
  - It was tested through a C harness compiled against each build's own
    headers. `snapshot.h` is identical between the two, but `terminal.h`
    gained ~1,150 lines, so the Rust bindings at `8953a74` can't safely link
    against main.
- @xterm/headless 6.0.0 with the unicode11 addon, Node 22.23.2, Rust 1.98.1.
- Claude Code 2.1.286 with `--model haiku` (Haiku 4.5).

```
python3 record_claude.py   # nested claude; unsets every CLAUDE* variable
python3 gen.py             # synthetic fixtures
CARGO_TARGET_DIR=$PWD/work/target LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast \
  mise exec -- cargo run --release            # writes work/out/ for xterm
(cd xterm-check && npm ci && node check.mjs && node check.mjs .patched)
# cross-build: build work/bin/snapcheck-{pinned,main} from xbuild/snapcheck.c
# (zig cc -I<install>/include snapcheck.c <install>/lib/libghostty-vt.a -lc),
# then xbuild/cross.sh
```

## How it works

- `src/main.rs` feeds each fixture into terminal A, then restores it into B
  three ways:
  - **GHOSTSNP:** encode A, decode B (as in S5).
  - **wire:** the same bytes go into `illogical_vt::GhosttyEngine`. Its
    `snapshot()` is written into a fresh libghostty B.
  - **wire+fix:** the same through `src/patched.rs`.
- A and B are compared on:
  - every cell of the screen *and the scrollback*: grapheme, style,
    background from erases, protection, hyperlink;
  - cursor, pending wrap, cursor pen;
  - 22 DEC and 2 ANSI modes, Kitty keyboard flags;
  - title, pwd, palette, scrollback rows;
  - Kitty images and placements, and plain text.
- State that can't be read directly is compared through **probes**. Each
  probe sends the same bytes to a fresh A/B pair and compares everything
  again:
  - `ESC 8 q@` (DECRC, then print);
  - home, a 160-char line, LFs at the bottom, IL (wrap, margins, region);
  - `CSI ? 2 J` (DECSED: protection);
  - `?1049l ESC 8` and `?47l ESC 8` (each screen's saved cursor).
- `xterm-check/check.mjs` replays the wire snapshots (crate and fixed) into
  @xterm/headless and compares under the same probes:
  - against **xterm fed the raw fixture**: text with scrollback, cursor,
    buffer, modes, every visible cell's attributes. This is the fair test.
  - against **Ghostty A**, as S1 did.
- `xbuild/snapcheck.c` encodes and decodes with one build. It compares a
  decoded terminal with the fixture fed into that same build:
  - formatter VT output with every extra;
  - plain text with scrollback;
  - cursor and pending wrap, as restored and after the DECRC probes.

## 1. New fixtures

| fixture | what | how |
|---|---|---|
| claude | Claude Code TUI: start, one prompt, Haiku answers "hello from haiku", resize 100x30 -> 80x24, still running | `record_claude.py`, real PTY |
| claude_exit | the same, then `/exit` | same recording, to the end |
| decom | DECSTBM 5;20 + DECOM on, text scrolled inside the region, protected cells (DECSCA), left with DECOM and DECSCA on | `gen.py` |
| slrm | DECLRMM + DECSLRM 20;70 + DECSTBM 4;18, wrapped text inside the margins, IL, DL, SU, ICH, DCH | `gen.py` |
| slrm_decom | DECLRMM + DECSLRM + DECSTBM + DECOM, text wrapped and scrolled in the box | `gen.py` |
| decsc_primary | DECSC with bold/underline/256-color fg/RGB bg + DEC graphics charset, then moved on | `gen.py` |
| decsc_wrap | DECSC with a pending wrap (cursor past the last column) | `gen.py` |
| decsc_1049 | primary DECSC, then `?1049h` (which saves again), then DECSC inside the alt screen | `gen.py` |
| decsc_47 | primary DECSC, `?47h` (no save), DECSC inside the alt screen | `gen.py` |
| kitty | Kitty graphics: a=T direct placement (16x16 RGBA, 4x2 cells) and a virtual placement with U+10EEEE placeholders | `gen.py` |
| sixel | a 12x12 sixel between two lines of text | `gen.py` |
| s1_* | the seven S1 fixtures, as a regression set | copied from `crates/vt/fixtures` |

Notes:

- Claude Code 2.1.286 draws in the **alternate screen**. It sets Kitty
  keyboard flags (disambiguate + report alternates) on the primary screen
  first. It redraws fully on resize.
- **Images in libghostty-vt:**
  - **Kitty graphics** are parsed and kept in terminal state (per screen).
    The feature is on by default, and the storage limit defaults to 10 MB
    per screen.
  - **Sixel is not parsed at all.** DCS q is swallowed and leaves no state.
    The DA1 we send doesn't advertise it, and XTSMGRAPHICS gets no reply.
  - The daemon's engine **answers a Kitty graphics query with `OK`**
    (`ESC _G i=31;OK ESC \`). `compat.rs` only rewrites CSI replies.

## 2. Round trips

Ghostty -> Ghostty, everything compared under all 6 probes:

| fixture | GHOSTSNP | wire (crates/vt) | what the wire path gets wrong | wire+fix |
|---|---|---|---|---|
| claude | exact | DIFF | 10 blank cells take the previous run's colours (a black bg beside the logo). After `?1049l`, the primary's Kitty keyboard flags are 0 | exact |
| claude_exit | exact | DIFF | cursor on an empty row below the text: screen shifted up a row, one extra scrollback row | exact |
| decom | exact | DIFF | cursor 4 rows low (39,11 -> 39,15). Protected cells unprotected, so DECSED erases them | exact |
| slrm | exact | DIFF | whole screen one row off (scrollback 6 -> 5) | exact |
| slrm_decom | exact | DIFF | both of the above | exact |
| decsc_primary | exact | DIFF | DECRC goes to 1;1 with the default pen and charset | exact |
| decsc_wrap | exact | DIFF | same | exact |
| decsc_1049 | exact | DIFF | alt DECRC wrong. `?1049l` lands on the row after the primary content, not the saved 14;9 bold green | exact |
| decsc_47 | exact | DIFF | same, and the alt screen comes back as 1049 (`?47`/`?1049` flags swapped) | exact |
| kitty | **images lost** | DIFF | images lost; screen shifted up a row (cursor on an empty row) | **images lost** |
| sixel | exact | DIFF | screen shifted up a row (cursor on an empty row) | exact |
| s1_modes | exact | DIFF | **OSC 8 hyperlinks on cells are lost** (S1 didn't compare links) | exact |
| s1_nvim_resize | exact | DIFF | DECRC (saved cursor) | exact |
| s1_less, s1_nvim, s1_resize, s1_seq, s1_top | exact | exact | | exact |

Kitty images:

- **GHOSTSNP drops them by design.** In `grid.zig`: "Kitty image and
  placement state is not part of this snapshot version". Only the
  U+10EEEE placeholder cells survive.
- The formatter doesn't emit them either.
- No path carries images.

Sizes in bytes. "Repainted" counts the cells the verify step had to fix.

| fixture | GHOSTSNP | wire | wire+fix | repainted |
|---|---|---|---|---|
| claude | 3,472 | 7,115 | 7,405 | 10 |
| decom | 2,758 | 6,956 | 7,588 | 21 (protected) |
| s1_modes | 1,767 | 5,904 | 6,235 | 4 (links) |
| s1_nvim | 15,182 | 16,620 | 16,828 | 0 |
| s1_seq | 35,395 | 34,531 | 34,665 | 0 |

`snapshot()` time, median of 20 (`results/bench.txt`):

| | crate | patched |
|---|---|---|
| s1_seq | 0.47 ms | 3.3 ms |
| s1_nvim | 0.07 ms | 2.5 ms |
| s1_top | 0.03 ms | 0.9 ms |
| claude | 0.09 ms | 1.3 ms |
| decsc_1049 | 0.11 ms | 1.0 ms |

Most of the patched cost is the prototype's string-based cell compare.

**xterm.js** (`results/xterm-wire*.txt`). The wire snapshot replayed into
@xterm/headless is compared with xterm.js fed the raw bytes, under each probe:

| | matches xterm(raw) |
|---|---|
| crate wire | s1_less, s1_nvim, s1_resize, s1_seq only. Every failure above shows up in xterm.js too (the claude black cells, shifted screens, lost links, DECRC). |
| wire+fix | everything except kitty, s1_top, slrm, slrm_decom |

The four that still differ are xterm.js disagreeing with Ghostty on the raw
stream, not snapshot losses:

- xterm.js ignores Kitty graphics, so it doesn't move the cursor past an
  image.
- In `top`, it marks erased cells inverse.
- It has no DECSLRM.

After restore, the slrm screen in xterm.js is exactly Ghostty's. In
slrm_decom the cursor column is off by the left margin, because xterm.js
ignores the margin in the origin-relative CUP. `compat.rs` already tells
programs that DECLRMM is unsupported.

## 3. Ghostty main compatibility

18 fixtures. Each was encoded by both builds and decoded by both
(`results/cross-build.txt`):

| written by | read by | result |
|---|---|---|
| pinned `22d13172` | pinned | SUCCESS, identical to the fixture fed into the build |
| main `0081d453` | main | SUCCESS, identical |
| pinned | main | **`INVALID_VALUE`** on all 18 |
| main | pinned | **`INVALID_VALUE`** on all 18. The Rust decoder fails in `ready()`, after `Decoder::new_buf` succeeds |

**S5's inference is confirmed, and the failure is a clean error, not wrong
state:**

- The envelope (`GHOSTSNP`, version 1) is byte-identical.
- So are the terminal and screen records: the files match byte for byte up
  to the READY record.
- Main's snapshots are exactly **64 bytes smaller**: the two BLAKE3-256
  digests in READY and FINISH (`219173ab3`).
- No terminal is returned, so nothing half-decoded can reach the daemon.
  For these fixtures the vectorized-codec commits changed nothing on the
  wire.

**`build_info` from both builds:** `version_string = "0.1.0-dev"`,
`pre = "dev"`, `build = ""`.

- libghostty-vt's version is the constant `lib_version = "0.1.0-dev"` in
  `build.zig`. Only the app version gets the git hash.
- So `engine_tag()` returns `libghostty-rs@8953a74 ghostty@0.1.0-dev` from
  either build. The only part that tells builds apart is the hand-written
  `libghostty-rs@8953a74`.
- A Ghostty change through `GHOSTTY_SOURCE_DIR`, or a libghostty-rs bump
  that doesn't edit that string, produces the same tag.

## Consequences

**Fix-ups `crates/vt` needs for the wire path.** All are prototyped and
verified in `src/patched.rs`, in priority order:

1. **Pad dropped trailing rows right after the content.** Today the CRLFs
   come after the formatter's own CUP and DECSTBM/DECSLRM:
   - they start from the cursor, not the content end;
   - inside a scroll region they don't scroll.

   It shows whenever the cursor is below the last text: after a program
   exits, after `clear`, while a command waits on an empty line. The screen
   shifts up by the gap.

   Fix: format twice. palette + modes + content is a byte prefix of the
   full output, so insert the padding (and the trailing-row paint) at that
   point.
2. **Final CUP relative to the region under DECOM.** It also needs the
   left margin under DECLRMM. Parse both from the DECSTBM/DECSLRM in the
   tail.
3. **Saved cursor.**
   - Clone through GHOSTSNP (exact, ~0.3 ms to READY), restore on the
     clone, read position, pen, charset, protection and origin.
   - Emit that state, then `ESC 7`, then the real state.
   - Under an alt screen, do this for the primary (probe `?1049l` on the
     clone) before switching.
   - Enter the alt screen with the mode the source used (47, 1047, 1049),
     and reset SGR after the switch.
   - Carry the saved pending wrap by rewriting the cell under the cursor
     before `ESC 7`.
4. **Verify and patch the visible cells.**
   - Replay the snapshot into a scratch terminal and repaint any cell that
     differs.
   - This fixes three formatter bugs (blank-cell colours, missing per-cell
     hyperlinks, missing per-cell protection).
   - It's cheap insurance against new ones.
5. **Primary Kitty keyboard flags** under an alt screen.
6. **Kitty graphics:** set `set_kitty_image_storage_limit(0)` when the
   client can't draw images (xterm.js today), or drop `ESC _G` replies in
   `compat.rs`. As it is, programs are told images work, and each pane can
   hold 10 MB of images nobody will see.
7. **Tests.** Add these fixtures and the probes to `crates/vt`. The current
   tests compare only the state as restored, and every S1 fixture happens
   to leave the cursor on its last line of text. That's why none of this
   showed up.

**M2 checkpoint tagging.**

- In practice it is safe: a cross-build decode fails cleanly, and
  `from_checkpoint` maps it to `Corrupt`, then the log is replayed.
- But the tag doesn't identify the Ghostty build, so it adds little over
  "any decode error -> discard". Two ways to make it real:
  - derive the commit at build time, in a `crates/vt` `build.rs`, from the
    sys crate's `ghostty-src/.ghostty-commit` via `DEP_GHOSTTY_VT_INCLUDE`,
    or `git rev-parse` in `GHOSTTY_SOURCE_DIR`;
  - or, simpler and stronger, a **format fingerprint**: at startup, encode
    a fixed canary terminal and put a hash of the GHOSTSNP bytes in the
    tag. The BLAKE3 change alters those bytes, so it would have changed
    the tag.
- Neither catches a decoder-only change. Discard-on-error stays the safety
  net, and checkpoints stay a cache.

**Upstream issues worth filing.**

Ghostty:

- GHOSTSNP: bump the envelope version, or add a revision field, for wire
  changes. `219173ab3` changed READY and FINISH and kept version 1.
- libghostty-vt `build_info`: `VERSION_BUILD` is empty and the version is a
  constant. Put the git short hash in, as the app version does.
- GHOSTSNP: carry Kitty images and placements (a feature request; the
  source calls them out of scope for version 1).
- Formatter. These are new; S1's list still stands (tab stops clobber the
  cursor, no title or cursor shape, dropped trailing rows, NUL in OSC 7, no
  API to format a chosen screen or read the saved cursor).
  - Blank cells inside a styled row are written as spaces under the
    previous run's SGR, so they gain its colours.
  - Per-cell OSC 8 hyperlinks are not emitted.
  - Per-cell protection (DECSCA) is not emitted.
  - The cursor CUP is emitted before DECSTBM/DECSLRM, and per the VT spec
    those home the cursor. The CUP is also not origin-relative. The crate's
    trailer CUP masks this today.
  - Only the active screen's Kitty keyboard flags are emitted.

## Still open

- Verify-and-patch covers only the active screen's visible cells. Under an
  alt screen, the primary and the scrollback still carry the formatter's
  blank-cell and hyperlink losses.
- The live cursor's pending wrap (fix 3's trailer path) is untested. No
  fixture ends with a pending wrap.
- Only 18 fixtures. A decoder change that decodes old bytes silently wrong
  isn't ruled out in general; none was seen here.
- One short Claude Code session. Tool output, diffs and long scrollback in
  Claude Code are not recorded.
- Sixel: if the web client ever loads `@xterm/addon-image`, sixel images
  will show live but never in a snapshot, because libghostty keeps no
  state for them.
- The ghostty-web path (S10) isn't covered here.

## Cleanup

- **Spend:** one Haiku 4.5 turn, through the existing Claude Code login (no
  API key). The nested claude was killed after `/exit`. It left its session
  file under `~/.claude/projects/` (resume id `82559e0a…`).
- **Processes:** none left running. No illogicald instance was touched.
  Nothing ran in the repo's `target/`.
- **Disk:** deleted all of `work/`:
  - Rust target, 519 MB;
  - Ghostty main clone and pinned worktree, 339 MB;
  - Zig caches, 460 MB;
  - main's install, 28 MB.

  Also deleted `xterm-check/node_modules`. The directory is now 448 KB:
  code, fixtures, and `results/` with the raw outputs.
