# Research notes

Gathered 2026-10-01 to back the decisions in [PLAN.md](../PLAN.md). Items
marked *(unverified)* were not checked against a primary source.

## Server-side VT engine

| Option | Snapshot story | Verdict |
|---|---|---|
| **libghostty-vt** (Rust: `libghostty-vt` 0.2.2 by Uzaaft/pluiedev) | C API formatter emits VT, HTML or plain text. The "all" extras restore the palette, non-default modes, scroll region, tab stops, OSC 7 cwd, the Kitty keyboard stack, charsets, cursor, style and OSC 8 hyperlinks. Starts from the top of scrollback. | **Chosen.** Pre-1.0 API, needs Zig 0.16, pinned Ghostty commit, links statically. |
| `alacritty_terminal` 0.26 | No grid-to-VT serializer exists. Zed's persistence RFC ships bincode state instead, which only works when the client is also alacritty. | Avoid: we would be writing the serializer, which is the part most likely to have bugs. |
| `@xterm/headless` + `addon-serialize` (Node) | Primary screen + scrollback + `?1049h` + alt screen, plus most modes. Drops SGR mouse encoding (1006), OSC 4 palette, hyperlinks, cursor shape and the Kitty keyboard state. Still marked "experimental". | Proven: it is what VS Code's pty host ships. Fallback engine, or a test oracle. CPU-heavy (vscode#338148). |
| `vt100` / `shpool_vt100` | `contents_formatted()`; no alt screen or scrollback. | Too thin. |
| Go: go-libghostty, charmbracelet/x/vt | go-libghostty has the same formatter, and a prebuilt-static wrapper exists. | Viable if the daemon were Go; it is not. |

**Known libghostty-vt gap.** The formatter writes the *active* screen only, and
its modes extra switches screens before writing content. While vim or htop is
running, the attach would show the alt screen correctly but an empty primary
screen and no scrollback. The Zig side can format the screens separately; the
C API does not expose that *(unverified)*. Fix options are in PLAN.md, spike S1.

Links: [ghostty](https://github.com/ghostty-org/ghostty),
[formatter.h](https://github.com/ghostty-org/ghostty/blob/main/include/ghostty/vt/formatter.h),
[libghostty docs](https://libghostty.tip.ghostty.org/),
[libghostty-rs](https://github.com/Uzaaft/libghostty-rs),
[hauntty](https://github.com/seruman/hauntty) (a Go session daemon on libghostty-vt wasm),
[VS Code ptyService](https://github.com/microsoft/vscode/blob/main/src/vs/platform/terminal/node/ptyService.ts),
[SerializeAddon](https://github.com/xtermjs/xterm.js/blob/master/addons/addon-serialize/src/SerializeAddon.ts),
[Zed persistence RFC](https://github.com/zed-industries/zed/discussions/50584).

## Prior art, lesson -> action

- **tmux control mode.** Line protocol: `%begin/%end/%error` replies;
  `%output`, `%layout-change`, `%window-add`, etc. as notifications; per-pane
  `pause-after` / `%pause` / `continue` flow control. Layout string format is
  `csum,WxH,X,Y{..}` / `[..]`.
  -> Copy the ID model (`$session @window %pane`, never reused). Keep sizes
  derivable in cells. Map every event 1:1 to a `%` notification. Add request
  correlation numbers. Before M5, capture iTerm2's attach sequence with a
  logging proxy. [wiki](https://github.com/tmux/tmux/wiki/Control-Mode),
  [layout-custom.c](https://github.com/tmux/tmux/blob/master/layout-custom.c)
- **Size reconciliation.** tmux offers `window-size largest|smallest|manual|latest`
  (`latest` is the default). Zellij 0.45 sizes each tab by its current viewers.
  -> Per pane, the latest-input client wins; other viewers letterbox.
- **Zellij resurrection.** Serializes KDL every 1s, including commands, and
  restores them behind a "Press ENTER to run" banner.
  -> Restart policy `rerun` defaults to confirm-first.
  [docs](https://zellij.dev/documentation/session-resurrection.html)
- **Zellij web client** (0.43+). Separate terminal and control WebSockets,
  token -> HttpOnly cookie, read-only tokens, mobile UI.
  [docs](https://zellij.dev/documentation/web-client.html),
  [design](https://poor.dev/blog/building-zellij-web-terminal/)
- **shpool / dtach / abduco.** One holder per session. shpool keeps
  `shpool_vt100` state and restores via `session_restore_mode`.
  -> Snapshot, then the live stream, then a SIGWINCH nudge.
- **Keeping PTYs across daemon restarts.** The systemd FD store
  (`FDSTORE=1`, `FDNAME=`, `LISTEN_FDNAMES`) works.
  [sail PR #261](https://github.com/standardapplied/sail/pull/261) does it for
  PTYs: the child opens the slave and calls setsid, a shim writes the exit
  status to a file (a restarted daemon is not the parent), and pid-reuse
  guards use start time. On their own, fds do *not* keep shells alive: the
  default `KillMode=control-group` kills them. Run panes in their own transient
  scopes, as tmux does.
- **WezTerm mux** sends rendered line diffs (`GetPaneRenderChanges`)
  *(unverified)*. **mosh SSP** syncs state, not bytes.
  -> Bytes for xterm.js and `-CC`. If a client falls too far behind, drop its
  queue and send a fresh snapshot (the cheap 80% of SSP).
- **HTM** (Eternal Terminal's headless multiplexer) speaks tmux control mode
  from a non-tmux daemon. It is the closest prior art for M5. Windows Terminal
  integration PR is open:
  [microsoft/terminal#20639](https://github.com/microsoft/terminal/pull/20639).
- **sshx.** Each chunk carries an absolute byte offset; clients request what
  they are missing. **ttyd.** Client-side high/low watermarks.
  -> Per-pane absolute offsets with resume-from-offset, and per-client ACK
  windows. Never pause the PTY.
- **Log format.** asciicast v3 is NDJSON with relative times: JSON-inflated
  and no byte offsets, so seeking is a linear scan.
  -> Raw byte segments plus a sidecar index; export `.cast` on demand.
  [spec](https://docs.asciinema.org/manual/asciicast/v3/)
- **Shell integration.** OSC 133 A/B/C/D(exit), OSC 7 cwd, OSC 633 (VS Code).
  Ghostty auto-injects: bash via POSIX mode + `ENV`, zsh via a temporary
  `ZDOTDIR`, fish via `XDG_DATA_DIRS`.
  [ghostty shell-integration](https://github.com/ghostty-org/ghostty/blob/main/src/shell-integration/README.md)

## Web client

- **@xterm/xterm 6.0.0.** The canvas renderer is gone; the choice is WebGL or
  DOM. Chrome allows about 16 WebGL contexts per page, and
  `WebglAddon.dispose()` leaks its context (xterm#6068).
  -> WebGL only on visible panes; call `WEBGL_lose_context` on dispose.
  Mobile touch is basic (xterm#5377).
- **ghostty-web 0.4.0** (Coder). 2D canvas only, with open bugs: grapheme
  memory corruption (#141), render loop dies after an error (#189), inverted
  key handler (#192), Korean IME (#119).
  -> Later swap-in behind a `TerminalView` interface.
- **Layout.** react-mosaic 7.2 (tabs are in the tree since v7) is fully
  controlled: `value`, `onChange(tree, meta)`, `onRelease`. dockview 8.4 is
  stronger at floating/popout windows but not controlled. golden-layout is
  stale.
- **Phone.** Termux-style extra-keys bar with sticky Ctrl/Alt;
  `visualViewport` + `interactive-widget=resizes-content`; one pane at a time.
- **PWA.** Needs HTTPS (a secure context) for service worker, install and
  clipboard. Keyboard Lock only works in JS-initiated fullscreen, so Ctrl+W/T/N
  cannot be captured in a windowed app *(test it)*.
  `display_override: ["window-controls-overlay"]` works on Linux Chrome.
- **Flow control.** xterm discards data past 50MB. Use `write(data, cb)` with
  ACKs about every 100KB and a high watermark of 500KB or less.
  [guide](https://xtermjs.org/docs/guides/flowcontrol/)

## Ops on geek (checked read-only)

- Tailnet name: `geek.tailb2e8f2.ts.net`. MagicDNS and HTTPS certs are enabled.
  No serve config yet, and no operator set.
- `tailscaled.sock` is world-rw, so WhoIs works unprivileged.
- systemd 259, linger on. The user manager env currently has `PATH`,
  `SSH_AUTH_SOCK`, `DISPLAY` and `WAYLAND_DISPLAY` because GNOME imported them.
  They are absent at boot until login.
- `tailscale serve` adds `Tailscale-User-Login/-Name/-Profile-Pic` headers and
  strips spoofed copies from incoming requests. WebSockets proxy through
  *(untested here)*.
- Toolchains: Go 1.27, Node 22, pnpm. **No Rust, no Zig** (Ghostty 1.3.0-dev
  is installed as a binary).
