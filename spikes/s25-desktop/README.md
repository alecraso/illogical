# S25: a desktop shell (Tauri 2 on WebKitGTK and WKWebView)

Run 2026-10-04 on geek (Ubuntu 26.04, GNOME on Wayland, Radeon 8060S, a 240 Hz display, WebKitGTK
2.52.6) and jake-mini (macOS 15.5, arm64). The question (#130): is a Tauri 2 window around the
existing web client good enough to build the desktop app on, or does it have to be Electron?
**Result: go (Jake, 2026-10-04), on geek's run.** The macOS checks moved into M46's done-when. Jake ran the real UI in the shell (with the HUD and the overlay titlebar)
and it "went fine": no lag he noticed next to Chrome. Every check passed except two that are fixable in
the app (F10, and notifications need a native path, which M46 planned anyway).

## What's here

- `shell/`: the Tauri 2 app. `s25-desktop` opens the daemon's page (`http://127.0.0.1:7681`, an
  origin the daemon already accepts, so no daemon change); `s25-desktop probe` and
  `s25-desktop bench` open the two pages in `probe/`. It has a tray icon ("New window"), a dock
  badge and tray title for the needs-you count, and native notifications that report a click.
  - `S25_TITLEBAR=overlay`: no system titlebar. The client's `.bar` becomes the drag region and
    gets minimize, maximize and close buttons on Linux; macOS keeps its traffic lights.
  - `S25_HUD=1`: frame rate, worst frame and the last chord drawn over the page.
  - `S25_MENU=default` (macOS): Tauri's stock menu, which takes Cmd-W/Q/H/M for itself.
- `probe/keys.html`: chords, IME into xterm, clipboard, notifications, windows and badge. With
  `S25_AUTO=1 S25_LOG=…` it runs what needs no hands and logs every key.
- `drive.py`: presses real keys through a uinput keyboard (logind gives the seat's user
  `/dev/uinput`) and checks the probe logged each one before the next. A missed chord is followed
  by a harmless `z`; if that's missed too the run stops, so keys can't land in another window.
- `probe/bench.html`: the client's xterm 6 + WebGL at a fixed 160x48, through idle, `yes`, a
  colored log, an htop-style full redraw and a vim-style scroll. It measures frame rate, frame
  times and write-to-paint (write a character, wait for xterm's render, then the next frame),
  also while the flood is queued ahead of it. `bench-browsers.cjs` runs it in Chrome.
- `vendor.sh` copies the client's own xterm build into `probe/vendor`.
- `results/`: the raw numbers and the probe's logs.

```
./vendor.sh && (cd shell && cargo build --release)
S25_AUTO=1 S25_EXIT=1 S25_OUT=results/x.json shell/target/release/s25-desktop bench
S25_BIN=shell/target/release/s25-desktop ./drive.py results/probe-geek.jsonl   # hands off the keyboard
```

On geek, building needs `pkg-config libwebkit2gtk-4.1-dev libayatana-appindicator3-dev
librsvg2-dev libssl-dev libxdo-dev` (now installed).

## geek: the renderer

| | Chrome 153 | Tauri (WebKitGTK 2.52) |
|---|---|---|
| write-to-paint, idle (p50 / p95) | 7.3 / 7.4 ms | 7 / 8 ms |
| `yes` flood: fps, latency p95 | 122, 24 ms | 110–130, 28–39 ms |
| colored log flood: fps, latency p50 | 238, 11 ms | 60, 31 ms |
| htop-style redraw: fps, latency p50 | 240, 8 ms | 70, 33 ms |
| vim-style scroll: fps, latency p50 | 240, 8 ms | 70, 33 ms |
| frames over 50 ms (all scenarios) | 0 | 1 (in `yes`) |
| throughput, log flood | 27.7 MB/s | 28.8 MB/s |

- **WebGL works** (xterm's WebGL addon loads and keeps its context). WebKit reports the GPU as
  "Apple GPU" whatever it is; Chrome sees the Radeon through ANGLE.
- **Under load WebKitGTK paints at about 60 fps**, where Chrome follows the 240 Hz display. In
  frames the latency is the same (about 2 frames from write to screen); on a 60 Hz display the two
  should match. Here it's 33 ms against 8 ms while something redraws the whole screen every frame,
  and identical when idle, which is when you type. Jake didn't notice it.
- Parsing isn't the cost: throughput is the same, and parsing a screen takes about 1 ms.
- Two runs agreed (`results/webkitgtk-1.json`, `-2.json`). Playwright's WebKit crashed on geek and
  isn't the system WebKitGTK anyway, so it isn't in the table.

## geek: keys, IME, clipboard, notifications, windows

Driven by `drive.py` (`results/probe-geek.summary.json`).

- **48 of 49 chords reach the page**, Ctrl-W, T, N, Q, H, M, Tab, Shift-Tab, PageUp/Down,
  Ctrl-Shift-T/W/N/C/V/P/I, Alt-as-Meta (b, f, x, ., Backspace, Left), Ctrl-C/D/Z/\\/[/Space,
  Escape and F1–F12, except **F10**: GTK keeps it for the menu bar (`gtk-menu-bar-accel`). M46
  clears that setting for the window. Keyboard Lock isn't needed: nothing else takes the keys.
- **IME works through xterm**: Mozc committed 日本語 and Hangul committed 한, 국, with normal
  composition events, as typed into xterm's textarea.
- **Clipboard:**
  - writing from the page works after a key or click, and is refused without one
    (`NotAllowedError`). The client copies on Ctrl-Shift-C, which is a key, so that's fine.
  - reading from the page is refused even after a key. It doesn't matter for paste:
    **Ctrl-Shift-V gives a paste event** with the clipboard's text, which is how xterm pastes, and
    plain Ctrl-V stays a key (^V for the program), as in Chrome.
  - **OSC 52 needs Rust**: a program's escape sequence isn't a gesture, so the page can't write it,
    but `tauri-plugin-clipboard-manager` writes and reads from Rust. The client has no OSC 52 handler
    today in any engine, so this is new work either way.
  - the Rust clipboard needs the session's `DISPLAY`/`XAUTHORITY` (it uses Xwayland's clipboard).
    Started from a shell without them it timed out. An app started from the desktop has them.
- **Notifications:** native ones show (notify-rust over D-Bus, with a default action). The
  webview has `Notification` (permission granted without asking) and a service worker, but **no
  `PushManager`**, so the client's Web Push path can't work in the app. M46 notifies from Rust
  instead, from the daemon's own attention events, which the window already receives. A click
  wasn't checked automatically: GNOME sends `ActionInvoked` only for its own clicks, and a signal
  from another sender doesn't count. Still to click by hand.
- **Multiple windows** on one app: each loads in about 0.4 s (tray "New window", or from the page).
- **Badge:** the tray tooltip and title update; GNOME has no dock badge, so Linux shows the count
  in the tray only.
- **Titlebar tabs:** with `S25_TITLEBAR=overlay` the client's own top bar is the titlebar: drag on
  empty space, tabs and buttons still click, and the injected window buttons work. M46 does this in
  the client (`data-tauri-drag-region`, window buttons on Linux) instead of from an injected script,
  and replaces `env(titlebar-area-*)`, which only the PWA's window controls overlay sets.

## geek: size and start

- `.deb` 5.8 MB, AppImage 91 MB (it carries WebKitGTK), binary 20 MB unstripped. With `illogicald`
  (48 MB) and `illogical` (13 MB) bundled, M46's `.deb` will be about 25 MB compressed.
- From launch to the daemon's page loaded: **270–295 ms** (three runs); setup is at 61 ms. The
  local probe page loads at 350–540 ms.
- The tray uses libayatana-appindicator, which warns it's deprecated in favour of
  libayatana-appindicator-glib. It works.

## jake-mini

Built over ssh (Rust 1.98 as a second toolchain, `cargo +1.98`: the Mac's default 1.86 is too old
for current Tauri, and its CI runner uses the default).

- From launch to the daemon's page loaded: **215–310 ms** (three runs); setup at 75–100 ms.
  Binary 13.6 MB.
- **Not run; moved to M46's done-when** (Jake called S25 on geek's result). They need the screen unlocked (a locked Mac throttles the window's frames, so the
  bench never finished):
  - the bench, against Chrome on the same Mac;
  - chords, which on macOS is the real question: with the Edit-only menu, does Cmd-W, T, N, Q, H
    and M reach the page (`S25_MENU=default` shows what Tauri's stock menu takes). Driving keys
    there needs a process with Accessibility, so it's by hand;
  - IME (Japanese and Korean ship with macOS), notifications and the dock badge.

## What M46 takes from this

- Tauri 2 stays the choice; Electron isn't needed.
- Load the UI from the local daemon, as here: no daemon change, versions always match.
- Notifications from Rust, from the attention stream, not Web Push.
- An OSC 52 handler in the client that writes through Rust in the app (and the async clipboard API
  in a browser, where it may be refused).
- Clear GTK's F10 binding; Linux needs window buttons when the titlebar is the client's.
