# S26: how native can the desktop app get (macOS and Linux)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s26-native/<file>`.

Run 2026-10-04 on geek (Ubuntu 26.04, GNOME/Wayland, 240 Hz, Radeon 8060S, GTK 4.22, libadwaita
1.9) and on Jake's MacBook Air (M4, macOS 15.5, 60 Hz Retina). Built on jake-mini too. Issue #141.
S25 showed a Tauri webview shell is good enough. S26 asks how far past it to go, per platform, with
each platform's own toolkit: Swift and AppKit on macOS, Rust and gtk4-rs/libadwaita on Linux.

**Result, ranked:**

1. **The hybrid: native window, native terminals (B2), native chrome, web blocks.** One shared
   Rust core (the daemon connection and a libghostty-vt terminal) under a GTK4 front end and an
   AppKit front end. It's faster than any webview, matches the daemon cell for cell by
   construction, and worked on both platforms with real input and IME. Blocks that are web pages
   anyway (diff, editor, browser, apps, agents) stay web, in webviews.
2. **Level A alone: native chrome around web content.** It works with almost no code, but the
   gain over Tauri is only the chrome: tabs, menus, a native rail.
3. **B1: Ghostty's own surface (macOS only).** It works (`illogical attach` in a GhosttyKit
   surface), but it's macOS-only, double-emulates, and leans on Ghostty's internal API.
4. **Level C: everything native.** Not worth it. A list-shaped block like the rail is cheap
   natively (80 lines), but several blocks are web pages by nature, and the rest is about 13.5k
   lines of UI to write twice.

## What's here

- `core/`: the shared core.
  - `Conn` speaks the daemon's WebSocket over its Unix socket on its own thread. Input wakes it
    at once through a socket pair, with no polling interval.
  - `Pane` is a client-side libghostty-vt terminal (the daemon's pin and patches). It's fed the
    snapshot and output, and **never answers queries**: the daemon's terminal already does.
  - Keys are encoded by libghostty from physical key codes (evdev and macOS tables).
  - 303 lines.
- `gtk/`: Linux. `TermView` draws libghostty's render state through GSK, with **a cached render
  node per row, rebuilt only when the row is dirty**.
  - `s26-gtk` shows one pane or runs the bench.
  - `adw` is the hybrid: libadwaita tabs, the native view for terminal tabs, WebKitGTK 6 for
    block tabs, and a native attention rail.
  - 970 lines.
- `ffi/`: the core as a C library for Swift (`mac/s26.h`). Rust walks the cells and hands Swift
  each dirty row as runs. 321 lines.
- `mac/`: macOS (`build.sh`).
  - `term.swift`: the B2 view, a CALayer per row drawn with CoreText, NSTextInputClient for IME,
    and the bench on CADisplayLink.
  - `s26-mac`: one pane, or the bench.
  - `s26-tabs`: native window tabs, B2 or WKWebView.
  - `s26-b1`: a GhosttyKit surface.
  - 700 lines.
- `scripts/drive_gtk.py`: real keys through S25's uinput keyboard into the GTK view, checked
  against the dev daemon's pane. `scripts/bench_server.py`: S25's bench in a browser on a machine
  without Node.
- `results/`: the numbers, and screenshots of nvim on geek and of B2 on the Air.

Every live run used a throwaway dev daemon (`illogicald --listen 127.0.0.1:0 --state-dir …`),
never the daily one.

## The numbers

S25's bench, on the same terminal size and content, with latency measured the same way in every
engine: write a marker, render, then the next vsync-aligned frame (rAF in browsers, a tick callback
in GTK, CADisplayLink on macOS).

**geek, 240 Hz:**

| | Chrome | WebKitGTK (S25) | **GTK4 native (B2)** |
|---|---|---|---|
| `yes`: latency p50, MB/s | 21 ms, 5.3 | 2–15 ms, 5.9 | **10 ms, 54–68** |
| log flood: fps, latency p50, MB/s | 238, 10.6 ms, 28 | 60, 31 ms, 29 | **239–248, 4.1 ms, 121–137** |
| htop redraw: fps, latency p50 | 240, 7.7 ms | 70, 33 ms | **224–248, 4.3–5.3 ms** |
| vim scroll: fps, latency p50 | 240, 7.8 ms | 70, 33 ms | **240–279, 7.3 ms** |
| frames over 50 ms | 0 | ≤1 | 0 |

**MacBook Air, 60 Hz:**

| | Chrome | Safari (WKWebView, what Tauri gets) | **AppKit native (B2)** |
|---|---|---|---|
| idle latency p50 | 17 ms | 18 ms | **13 ms** |
| log flood: fps, latency p50, MB/s | 60, 29 ms, 28 | 60, 30 ms, 30 | **60, 15 ms, 12.7** |
| htop redraw: fps, latency p50 | 60, 30 ms | 60, 30 ms | **60, 16 ms** |
| vim scroll: fps, latency p50 | 60, 30 ms | 60, 30 ms | **60, 17 ms** |

- **Native shows output a frame sooner** on both platforms: one frame after the write, where
  browsers take two. On geek, GTK also keeps up with the 240 Hz display where WebKitGTK stops at 60.
- **GTK moves 4–5x Chrome's data.** On macOS, B2 draws rows on the CPU with CoreText and moves
  about half of Chrome's data in a log flood. A glyph atlas on Metal (what Ghostty does) is the
  known next step if that matters. A 13 MB/s flood is still far more than a person reads.
- GTK's idle number from this method (about 20 ms) is GTK restarting its frame clock from idle,
  not lag. The write is painted 0.5 ms later, and the one idle frame with a compositor presentation
  time reached the screen at 5.7 ms.

## B2: our own renderer over libghostty-vt (both platforms)

- **Fidelity is the daemon's own.** The same engine and the same pin parse the same bytes, and the
  renderer reads libghostty's render state.
  - nvim with splits, syntax and a status bar, and `top` with its inverse header, drew correctly
    on geek.
  - Bold, italic, 256 and true color, inverse, wide CJK on the grid and emoji drew correctly on
    both platforms.
- **Input:**
  - geek: typing, Ctrl-C through libghostty's encoder, and Mozc (日本語) and Hangul (한국)
    through GtkIMContext all reached the pane (`drive_gtk.py`, real keys).
  - Air: typing, `top` and Japanese IME through NSTextInputClient worked (checked by Jake).
- **Start:** the first snapshot is drawn 84 ms after launch on geek.
- **Lessons:**
  - **GLib priorities.** A loop that waits by re-arming a short timer, or a receive loop that is
    always ready, runs above `GDK_PRIORITY_REDRAW` and starves every frame. The live receive loop
    runs at `DEFAULT_IDLE`, and the bench waits on frame-clock signals.
  - **Backgrounds as merged, pixel-aligned runs** under the text. Per-cell rectangles at
    fractional widths leave seams.
  - **Wide characters are laid out one by one at their column**, so a font's CJK advance can't
    drift a run off the grid.
- **What xterm.js gives the web client for free that B2 doesn't have yet:**
  - selection and copy, and the mouse;
  - scrollback scrolling, links, search, kitty images and ligatures;
  - accessibility (VoiceOver and Orca);
  - OSC 133 marks.

  libghostty-vt has selection, mouse encoding, kitty graphics and search. The work is the UI on
  each platform.

## B1: Ghostty's surface with `illogical attach` (macOS only)

- GhosttyKit built from the daemon's pinned Ghostty in 2.5 min on jake-mini (Zig 0.16, Xcode
  16.2): `libghostty-internal.a`, 135 MB. A bare AppKit host takes 130 lines.
- On the Air the surface came up in 168 ms. Ghostty started `login … illogical --socket DEV attach
  %1`, and typing and rendering worked (Jake).
- **Against it:**
  - It's macOS and iOS only: the embedding API has no Linux platform, at the pin or on main.
  - The terminal I/O backend has one kind (`exec`), so it can only be fed through a PTY.
    Everything is emulated twice. Scrollback starts at attach. Attaching zooms the pane to the
    window (one size per pane). Client-side features (marks, our own selection model) can't reach
    into Ghostty's terminal.
  - The C API is Ghostty's internal one, made for its own app.
  - It's the best-looking terminal for no work, and it's useful as a reference for B2 on macOS.
- Screenshots weren't possible over ssh: `screencapture` needs Screen Recording permission, and
  the surface draws into an IOSurface the app can't read back.

## Level A: native chrome, web content (both)

- **Linux:** libadwaita's tab bar in the header bar, with native window controls (`adw`).
  **macOS:** NSWindow tabbing, the system tab bar (`s26-tabs`). A block tab is a webview of the
  client at `#pane=N`, with the client's tab bar hidden by an injected stylesheet. A file block
  rendered correctly on both.
- **Every embedded webview is a whole client**, with its own WebSocket, its own first-run dialog
  (it showed in the dev daemon's tab) and its own menus and picker. A real hybrid needs an embed
  mode in the web client that shows one block, with no chrome or onboarding, and passes intents up
  to the native shell.
- Keeping native tabs in step with the daemon's tab model needs `State` events and intents (new,
  close, rename, move). The spike only reads the layout once.

## Level C: everything native (priced)

- **The attention rail** as a libadwaita list: every pane, sorted by what wants you, with its
  reason as the subtitle and a click that opens its tab. About 80 lines, refreshed from the daemon
  every 2 s. The web swarm is about 4.7k lines, most of it the visual themes (city, hive,
  timeline).
- **The 13 block types:**
  - browser, editor (code-server) and app (studio) are web pages by nature;
  - diff, file, forge (PRs and issues), fountain, workspace, agent and ask are lists, cards,
    timelines and text: possible natively, at roughly the client's 13.5k lines of UI, twice.
  - Not worth it when webviews draw them well. A native rail and native notifications are the
    pieces worth having.

## Size and cost

- Spike code: core 303, GTK 970, C library 321, Swift 700 lines. That's one terminal view per
  platform plus the hybrid shells, against S25's Tauri shell (about 200 lines and an injected
  script).
- A real hybrid is two UI codebases (Swift and GTK) over one Rust core, plus the web client's embed
  mode, where Tauri is one. The core (connection, terminal, keys) is shared, and so are the blocks.

## What this means for M46

**Decided (Jake, 2026-10-04): Tauri, as planned.** Native terminals come later, when a trigger in PLAN.md fires, starting on macOS. The options were:

- **Hybrid (recommended):** `crates/client` (the core here, also usable by the TUI), a GTK4 app and
  an AppKit app. Each has native terminals, native tabs and rail, native notifications, and
  webviews for blocks through a new embed mode. The daemon supervision and packaging in M46 stay as
  planned.
- **Tauri as planned, native later:** ship S25's shell, and bring B2 in later as a native terminal
  surface. Tauri can't easily put a native view over its webview, so that is a later rewrite of the
  window anyway.
- **Mixed:** the hybrid on macOS, where a native terminal matters most and AppKit is a single
  target, and Tauri on Linux.
