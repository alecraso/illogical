# S32: images into a pane, measured (#248)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s32-images/<file>`.

Can a screenshot get into a Claude Code session in any pane, from any client, by uploading it to the pane's host and pasting the path (PLAN.md, "Images track")? This spike answers the questions that shape M70 and M71. Everything here ran on alecraso's Mac mini (macOS, arm64) on 2026-10-05:

- Claude Code 2.1.289;
- Chrome 154, plus Playwright 1.63's WebKit 26.6 and Firefox 155;
- this branch's `illogicald`, as a throwaway daemon on 127.0.0.1:7692 with its state in `/tmp/il32`.

There was no `geek`, phone, Codex or sprite to hand, so those checks are listed under *Not measured*.

The prototype was about 150 lines: an upload route, `PaneHandle::bracketed`, the client's capture-phase paste and drop listener with its chunked upload, and raw request bodies on the e2e channel. It was enough to measure with and to demo, and M70 replaces it, so it isn't merged here. It's commit `d4b4e4e` on `alecraso/illogical`'s `s32-images` branch. It wrote on the daemon's own filesystem only, had no quota or sweep, and skipped the driver rule (see 6).

## Answers

**Go, with the plan's shape. Drop the "answer Claude's clipboard read" idea, because Claude never asks.**

| Question | Answer |
|---|---|
| Which pasted strings become `[Image #N]` | Any **bracketed** absolute path to a PNG, JPEG or GIF. Spaces, quotes and backslash escapes all work, and so do several paths at once. A typed (unbracketed) path doesn't, nor does a path without an extension, `~`, `file://`, HEIC, TIFF, or a missing file. |
| When Claude reads the file | **At paste time.** A file deleted after the chip appears is still seen on submit, so cleanup can never break a prompt. |
| Claude's Ctrl+V | It sends **no clipboard query at all**: no OSC 52, 5522 or 1337, even when told it's in kitty or Ghostty. Over "ssh" it prints *No image found in clipboard. You're SSH'd; try scp?*. Nothing to answer, so nothing to build. |
| Browsers: an image paste | Chrome and WebKit give the paste event a `File` (`image.png`, no text). xterm.js 6 reads only `text/plain` and stops the event, so today **an image paste is silently dropped**. A capture-phase listener on the terminal's host takes it first. Firefox: not measured (Playwright's Firefox can't put an image on its clipboard). |
| Browsers: a drop | The file arrives in all three. |
| By hand, in real Safari and Chrome | All six checks passed: a paste, a drag from Finder, and a drag of the screenshot thumbnail, in each browser. Claude read all six (section 3). |
| HEIC | WebKit decodes it and re-encodes it as JPEG in the browser. Chrome and Firefox can't decode it, but HEIC comes from iPhones, which run WebKit. |
| The route | It works. A `0600` file appears in a `0700` folder, and the path is pasted bracketed because `claude` asked for 2004. Refusals: the same id gets 409, a wrong offset 409, a body over 4 MB 413, and past 20 MB 413. A pre-existing `0755` folder is refused. |
| Body size | Axum's 2 MB default applies to every route today. Chunks of 1 MB under a 4 MB route limit move 20 MB locally in 0.24 s. |
| `/h/NAME`, dial-out, relay | `/h/NAME` streams bodies. Dial-out's 1 MB WebSocket cap is per mux frame (16 KB), so it doesn't bite. The relay holds a request as one message (≤ 64 MB) and needed a small change for raw bodies. |
| Machines (VM panes) | Code only: the sprite's agent writes **as root**, so a `0600` file there may be unreadable to a non-root `claude`. |
| Conversation blocks | *Continue* runs as an agent block (`illogical agent --resume`), so M71 covers it, not M70. |
| Agent blocks | `claude-agent-acp` 0.85.0 advertises `promptCapabilities.image: true` and turns base64 image blocks into Claude image content. |
| Demo | Chrome on the throwaway daemon's page: a real Cmd+V of a screenshot into a `claude` pane shows `[Image #1]` **124 ms** later, and Claude reads it ("S32 GREEN"). |

## 1. Claude Code's forms

`forms.sh` pastes each string through `illogical send %N -` (raw bytes, so the bracketed paste is exact) into `claude` 2.1.289 waiting at its prompt, then reads the prompt line. `images.py` makes the files. Results (`forms.txt`):

| Pasted | Prompt shows |
|---|---|
| `ESC[200~/tmp/…/img-red.png ESC[201~` | `[Image #1]` |
| the same, typed (no brackets) | the path, as text |
| under `$TMPDIR/illogical-uploads/70/` | `[Image #N]` |
| `/private/tmp/…`, single-quoted, `with\ space`, `with space` raw | `[Image #N]` each |
| two paths, space- or newline-separated | `[Image #N] [Image #N+1]` |
| `noext` (a PNG with no extension) | the path |
| `~/…`, `file:///…` | the path |
| `look at /tmp/…png` in one paste | `[Image #N]look at` (the chip moves to the front) |
| `describe ` typed, then the path pasted | `describe [Image #N]` |
| a missing file | the path |
| `.jpg`, `.gif`, `.PNG` | `[Image #N]` |
| `.heic`, `.tiff` | the path (Claude can't view it) |
| `.pdf`, `.txt` | the path (Claude reads it with its Read tool on submit) |

**What M70 takes from this:**
- The daemon pastes alone: a bracketed absolute path with an image extension, nothing else in the same paste.
- Several files are space-separated paths in one paste.
- Our names never need quoting.
- HEIC and TIFF are converted before upload, or they arrive as an unreadable path.

**Read time:** pasting `gone.png`, deleting it, then submitting "what colour?" got "Blue". Claude reads and holds the image when the chip is made. The 24h sweep, or even deleting straight after the paste, can't break a prompt.

## 2. Claude's Ctrl+V

`ctrlv.py` runs `claude` in a pty, waits for its prompt, sends `^V`, and records every byte it writes. It answers `CSI c`, and for some runs `CSI > 0 q` (XTVERSION) as kitty 0.39.1 or Ghostty 1.2.0.

| Run | Sent at startup | On Ctrl+V |
|---|---|---|
| local | `CSI > 0 q`, `CSI ? u`, `CSI c`, `CSI > 4 m` | reads the Mac's clipboard itself (osascript) and pastes it |
| `SSH_CONNECTION` set | the same | **no OSC 52/5522/1337**; prints *No image found in clipboard. You're SSH'd; try scp?* |
| ssh, XTVERSION says kitty | adds `CSI > 5 u`, `CSI > 4;2 m` | the same: no query |
| ssh, XTVERSION says Ghostty | the same | the same: no query |

The OSC 52 read path reported upstream (anthropics/claude-code#69330) doesn't exist in 2.1.289, at least on macOS. **Decision: don't build an answer to Claude's clipboard read.** If a later version starts asking, `ctrlv.py` will show it. (On Linux Claude shells out to `xclip` or `wl-paste`, which read a display server's clipboard, not the terminal's, so there's nothing for us to answer there either.)

One case already works without us: a local pane on the same Mac as the browser. Ctrl+V reads that Mac's clipboard directly.

## 3. Browsers

`paste.html` is xterm.js 6.0.0 with a capture-phase `paste`/`drop` listener on its container, and `paste.mjs` drives it. Each browser:

1. writes a PNG to its own clipboard with `navigator.clipboard.write` (Playwright's browsers keep their own clipboards, so the desktop's is untouched);
2. presses the real paste key;
3. then pastes text, to check that still reaches xterm;
4. then dispatches a drop carrying a `File`.

| | Image paste | Text paste | Drop |
|---|---|---|---|
| Chrome 154 | `File image.png, image/png`, text `""`; xterm sends nothing | reaches xterm | `File shot.png` |
| WebKit 26.6 | the same | reaches xterm | `File shot.png` |
| Firefox 155 | not measured: the event had no items, because Playwright's Firefox can't hold an image on its clipboard | reaches xterm | `File shot.png` |

xterm.js 6's `handlePasteEvent` calls `stopPropagation()` and reads only `getData("text/plain")`, so **today an image paste does nothing at all**: an empty string reaches the program. The capture listener on the host runs first, takes events that carry files, and leaves text pastes alone. The drop is synthetic (Playwright can't drag from the OS), so it checks our listener rather than the browser's drag source.

**By hand, on the Mac mini:** Aaron ran this branch's daemon on 127.0.0.1:7692 with `claude` in its pane, and tried three ways in each browser:

| Browser | How | File | Prompt |
|---|---|---|---|
| Safari | Cmd+V of a screenshot | PNG 974x736, 316 KB | `[Image #1]` |
| Safari | a drag from Finder | PNG 1898x1278, 1.10 MB (two chunks) | `[Image #2]` |
| Safari | a drag of the screenshot thumbnail (Cmd+Shift+Ctrl+4) | PNG 826x472, 70 KB | `[Image #3]` |
| Chrome | a drag of the screenshot thumbnail | PNG 1164x920, 162 KB | `[Image #4]` |
| Chrome | Cmd+V of a screenshot | PNG 2048x920, 494 KB | `[Image #5]` |
| Chrome | a drag from Finder | PNG 1706x1322, 1.29 MB (two chunks) | `[Image #6]` |

Claude described all six correctly. Firefox isn't installed on that Mac, so its real paste is still open.

**By hand, after M70 (2026-10-05):**

- **Android phone, Chrome:** M70's *Attach file* (📎) sent photos through `tailscale serve` into a real `claude`, which saw and described them. The key bar overflowed with nothing to say so, which hid 📎; it's pinned at the end now.
- **Android, a long-press Paste of a copied image** (Gboard), on a test page with a `<textarea>` and a contenteditable: both get a `paste` event carrying the file (`image.png image/png`, 1.19 MB and 367 KB), then a `beforeinput insertFromPaste`. So a textarea, like xterm's, receives pasted images as files. A long-press in a terminal pane offers Paste too, and a copied image pasted that way reached `claude` as an image. Gboard's own image and GIF panels and its "paste copied image" chip were offered in neither. Native apps turn those on by declaring the content types they accept, which a web page can't do.
- **The desktop app (WKWebView):** a paste worked but a drop didn't, because Tauri's drag-and-drop handler takes dropped files before the page sees them. It's turned off now; that still needs checking in a rebuilt app.

**HEIC** (`heic.mjs`): `createImageBitmap` on a HEIC blob works in WebKit (64x48, re-encoded to an 843-byte JPEG through `OffscreenCanvas`), and throws `InvalidStateError` in Chrome and Firefox. So the client converts HEIC when it can decode it, which is WebKit, the case that matters. Otherwise it uploads the file as it is.

## 4. The route and transport

`POST /api/panes/<id>/upload?id=<hex>&ext=<ext>&offset=N[&last=true]` takes a chunk. Offset 0 creates the file with `O_EXCL`, and each later chunk must start where the file ends. `last=true` pastes the path. Through the daemon's socket with `curl`:

| | Result |
|---|---|
| a 156-byte PNG, `last=true` | 200, `{"bracketed":true,"path":"…/illogical-uploads/2/img-aa01.png"}`; `claude` shows `[Image #1]`; the bytes match |
| the same id again | 409 `File exists` |
| `id=../../x` | 400 |
| 5 MB in one request | 413 (the route's 4 MB limit; without it, axum's 2 MB default) |
| 21 MB in 1 MB chunks | 20 chunks in 0.24 s; the 21st gets 413 *files are capped at 20 MB* |
| a chunk at the wrong offset | 409 |
| an `illogical-uploads` folder left at `0755` (from step 1's setup) | 500 *isn't a private directory of ours*: the owner and mode check works |
| into a zsh pane | the bracketed path lands on the command line, unexecuted |

**Over the other paths (by code):**

- **`/h/NAME`** (`dial.rs` `via_api`) hands the request body to hyper as a stream, so nothing buffers or caps it on the way.
- **Dial-out** caps a WebSocket message at 1 MB (`dial.rs:137,416`), but each message is one mux frame of at most 16 KB (`e2e/mux.rs` `CHUNK`). The cap doesn't limit uploads.
- **Control's relay** carries a request as one e2e message (`MAX_MSG` 64 MB), whole in memory at both ends and on the one channel a phone has. With 1 MB chunks, no message is large and other traffic interleaves. `channel.ts` sent only JSON bodies, so it now sends a `Uint8Array` as `application/octet-stream`; the frame already carried raw bytes.
- **Not measured:** an upload actually going through control's relay, `/tunnel/NAME`, and dial-out (*Not measured* below).

## 5. Machines

By code only, since there was no provider account here. A VM pane's file would go through `Provider::write_file`. On sprites, that's `PUT /{name}/fs/write`, served by the guest agent **as root** (`provider/sprites.rs`). A `0600` file it writes belongs to root, and a `claude` running as the sprite's user can't read it. M70 should either write `0644` inside a `0700` folder owned by the pane's user, or `chown` through `run`. Cleanup also goes through `run`, since providers have no delete. What an upload to a sleeping machine does is still open.

## 6. What M70 should change from the plan

- **Paste through the driver rule.** The prototype pastes with `Cmd::Input { client: None }`, as `/send` does. That skips the one-driver-per-pane rule that typing from a client goes through (`mux.rs`, `Cmd::Input`). M70 should pass the uploader's client, so an upload by someone who isn't driving is refused and explained, like typing.
- **`last=true`, not `last=1`.** Serde's bool rejects `1`; the client sends `true`.
- **Chunks of 1 MB, a route limit of 4 MB.** That's measured, and it suits dial-out and the relay.
- **A machine's files are root's** (5).
- **HEIC and TIFF** are converted in the client where it can decode them.
- **Conversation blocks belong to M71**, since *continue* is an agent block.

## Not measured

These are for a person, or for #214's suite:

- **Phones:** iOS Safari, the camera, and whether iOS hands the picker HEIC or JPEG. Android Chrome's picker and paste are measured above.
- **The desktop app:** a drop in a rebuilt app on macOS, and WebKitGTK on Linux.
- **Firefox's paste** of a real screenshot (Safari and Chrome are done by hand).
- **The other routes in practice:** an upload through control's relay, `/tunnel/NAME` and dial-out.
- **On Linux:** `claude` on geek, including its Ctrl+V.
- **Codex** with the same forms.
- **A real sprite:** file ownership, and an upload to a sleeping machine.

## Files

- `forms.sh`, `images.py`, `forms.txt`: the paste forms and their results.
- `ctrlv.py`: what `claude` sends on Ctrl+V.
- `paste.html`, `paste.mjs`: paste and drop in Chrome, WebKit and Firefox.
- `heic.mjs`: HEIC decoding per browser.
- `demo.mjs`: Chrome on the throwaway daemon, a real paste into `claude`.
