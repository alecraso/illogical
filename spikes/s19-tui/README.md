# S19: illogical as a TUI (herdr's shape)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s19-tui/<file>`.

Run 2026-10-02 on geek, for #48. **Result: go. The protocol and libghostty already do
the hard parts; drawing is under a millisecond a frame. What's left is
client work (keys, copy mode, menus), plus one daemon change that the web
client needs too.**

[herdr](https://herdr.dev) is a Rust/Ratatui multiplexer: a server owns the
PTYs, a thin TUI client draws panes, tabs and a sidebar of agent states, over
a Unix socket or SSH. That is illogical with a TUI in place of the browser.
The question was what that client costs and how it feels.

## What was built

`src/main.rs`, about 950 lines, one file, its own workspace (not part of the
product build):

- Attaches every pane of one tab over the daemon's socket, exactly as the web
  client does (`hello`, `view` with `claim`, `attach`, binary frames).
- **A local libghostty terminal per pane** (the same pin as `crates/vt`), fed
  with the same snapshot and output frames as the web client; `size` resizes
  it. Each frame walks ghostty's `RenderState` rows and cells into a ratatui
  buffer. Palette colors stay palette indexes, so the outer terminal's theme
  applies.
- `hello` and `state`, plus `delta` through `State::apply` (M23), so the
  sidebar follows attention changes live.
- **The daemon's own layout:** `TabView.layout` already has every pane's cell
  rectangle for the size we `view`, with one cell between neighbours. Those
  gaps are the dividers. No layout code in the client.
- **A sidebar:** sessions and their tabs (named after the first pane's
  command or directory), each with the worst attention state of its panes
  (● needs you, ✓ done, ◌ working), then a **needs you** list from
  `PaneInfo.reason`. Clicking either goes there.
- **Mouse:** click to focus; drag a divider (it becomes `resize_split` with
  the new extents as weights); the wheel scrolls the local engine's
  scrollback, or sends arrow keys on the alternate screen, or goes to the
  program when it has mouse reporting on (SGR, coordinates made relative to
  the pane).
- **Ctrl-] then a key:** `v`/`s` split right/down, `c` new tab, `x` close,
  `o` next pane, `n`/`p` next/previous tab, `q` quit. Ctrl-] twice types it.
- Keys go to the focused pane as raw bytes, except for one fix-up: cursor keys
  become SS3 when the pane's program has turned on application cursor mode
  (DECCKM). The outer terminal encodes keys for its own modes, not the
  pane's.

## How it went

Against a dev daemon (`--state-dir` in a scratch directory, port 7791), with
the TUI in a 200x50 tmux pane driven by `send-keys`:

- Typing, clicking into panes, `^] s` and `^] v` splits (the new pane gets
  focus), dragging a divider (43 → 28 columns, stored by the daemon),
  switching tabs from the sidebar, the bell'd pane listed under *needs you*:
  all worked first time.
- `top` stayed live in its pane. `less -R PLAN.md` worked: alternate screen,
  Down keys (DECCKM), wheel scrolling.
- `git log --graph --color` and `ls --color` kept their colors.

## Numbers

Frame build is the time to copy every pane's cells into the buffer. Draw adds
ratatui's diff and the write to the tty. 200x50, four panes.

| Load | Frames | Build p50 / p99 | Draw p50 / p99 | TUI CPU | Resyncs |
|---|---|---|---|---|---|
| Normal use (`top` in one pane) | 56 in 44 s | 0.40 / 0.60 ms | 0.63 / 0.95 ms | ~0 | 0 |
| One pane flooding (`ls --color` in a loop, 16 MB/s) | 463 in 11 s | 0.38 / 0.66 ms | 0.56 / 0.99 ms | 19% of a core | 0 |
| Four panes flooding | 327 in 11 s | 0.45 / 0.73 ms | 0.76 / 1.21 ms | 75% of a core | 328 |

RSS was 30–36 MB with every pane's scrollback held locally.

**After #49** (snapshots capped at the client's 10k rows, zstd, the screen
alone after a resync), with the same four floods:

| | Received | Of it snapshots | Resyncs | Frames | TUI CPU |
|---|---|---|---|---|---|
| Before (`S19_LEGACY=1`) | 1.53 GB | 1.44 GB | 214 | 317 | 64% |
| After | 612 MB | 9.8 KB (45 KB unpacked) | 155 | 569 | 56% |

The TUI still can't keep up with four uncapped floods, so it still gets
resynced. But each resync now costs a screen, not 5 MB, and the time goes
to drawing seven times as much real output.

**After #52** (the TUI acks what its engines take in; a pane's program
waits when the daemon can't keep up), the floods are paced by the daemon.
In 10 s the TUI received 402 MB with no resyncs, using 36% of a core
(`S19_NO_ACKS=1`: 359 MB, 38%; `S19_LEGACY=1`: 413 MB, 39%, also no
resyncs, because the pacing is in the daemon).

**Drawing is not the problem.** A full redraw of every cell, with no use of
ghostty's dirty rows yet, stays under 1 ms even when floods keep frames
coming at 30–40 per second. The engines keep up with one 16 MB/s pane at a
fifth of a core.

**Four uncapped floods fall into a resync loop.** The daemon drops a client
whose queue is full and sends `resync`. The client attaches again, and
because the gap is past the 1 MB replay window, gets a **full snapshot with
the whole 64k-row scrollback** (about 5 MB per pane). That costs more to
parse, so it falls behind again. Of 1.73 GB received in 10 s, 1.57 GB was
snapshots and 167 MB was output. Resuming from the client's offset instead
of `null` made no difference, because by then the gap is always over 1 MB.
The web client does the same (`client.ts` re-attaches with `offset: null`)
and would hit the same loop. The screen stayed current throughout, so it
looks fine; it just burns CPU. The fix is the one PLAN already names under
*Attach and resume* and that isn't built yet: **cap the history in a snapshot
at what the client keeps (sent in `attach`)**, and for a resync send the
screen only (the `floor` path M13 already has). That fix is for the daemon
and helps both clients.

## What a real `illogical tui` still needs

In rough order of cost:

1. **Key encoding.** Raw bytes plus the DECCKM fix-up is enough for shells,
   `less` and `top`, but not for a program that turns on the kitty keyboard
   protocol or modifyOtherKeys (Claude Code, Neovim, Helix). libghostty has
   the answer: `key::Encoder::set_options_from_terminal` encodes a key for a
   pane's own modes. So the TUI would read keys as events (crossterm, with
   kitty flags asked of the outer terminal), not bytes. `mouse::Encoder` does
   the same for mouse.
2. **Copy mode.** The outer terminal can't select inside a pane (it sees the
   composite), and the wheel scrolls but nothing selects. That means
   selection on drag, OSC 52 to the outer clipboard, and search. libghostty
   has selection (`selection.rs`, `RowIteration::selection`). This is the
   largest piece of work.
3. **Menus.** A right-click popup per pane and tab (policy, rename, close,
   move), as the web client has. Ratatui makes this straightforward.
4. **Blocks.** Agent and browser blocks are drawn as a placeholder line
   today. An agent block could be a plain transcript with approve and deny
   on keys, using `attention/act` from the *needs you* list. Browser blocks
   stay in the browser.
5. **Small things found along the way:**
   - Dismiss, as the web client's menu has (`attention/act`). Focusing a
     pane already clears *done* (the daemon does it on `focus`); *needs you*
     stays until it's answered or dismissed, by design.
   - Honor synchronized output (mode 2026): skip drawing a pane while its
     program is in the middle of a frame.
   - Bracketed paste: turn on 2004 on the outer terminal and pass pastes
     through when the pane wants them.
   - Cursor shape, and the pane title in the status line.
   - `--host`, through the CLI's existing `Target`, so it works the same way
     over the tailnet as on the socket.

Where it belongs: `illogical tui` in `crates/cli/src/tui/`, since the CLI
already links `illogical-vt`. The engine code should move into `crates/vt`
as a `cells()` walk next to the formatter, so that client and daemon share
one libghostty wrapper. New dependencies: ratatui and crossterm.

Filed as #50 (M31: `illogical tui`), #51 (M32: copy mode) and #49 (the
resync fix, for the daemon).

## Running it

```
cd spikes/s19-tui
CARGO_TARGET_DIR=../../target/s19 mise exec -- cargo build --release
../../target/s19/release/s19-tui [SOCKET]    # default: the daily daemon's
```

It is another client, so it claims the tab's size the way opening a browser
window does. `S19_STATS=file` writes the frame timings on exit;
`S19_LEGACY=1` attaches as clients did before #49 (all history, no zstd, a
fresh snapshot on every resync). `S19_NO_ACKS=1` doesn't ack (#52).

`bench.sh` repeats the flood measurements on its own dev daemon (port 7791,
state in a temporary directory) with the TUI in a 200x50 tmux session:
`./bench.sh` floods four panes, `PANES=1 ./bench.sh` one; `S19_LEGACY=1
./bench.sh` compares with before #49.
