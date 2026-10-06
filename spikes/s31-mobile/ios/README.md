# S31, iOS: what an iPhone app can run
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s31-mobile/ios/<file>`.

Everything here ran on a real iPhone 15 Pro (iOS 26.6.1), built on jake-air (Xcode 16.4) and installed with `devicectl`. The app was signed with the team's development profile, so it carries `get-task-allow`. The test app is `app/`: an Objective-C shell around `probe.c` (processes, ptys, dlopen, code generation), `wasmhost.c` (wasm3) and `rt/` (a Rust staticlib shaped like the daemon). The raw device output is in `results/`.

## Answers

**iOS can't be a machine in the sense Linux and macOS are. There are no processes and no ptys, and the app stops about a second after it leaves the screen.** Daemon-shaped Rust runs fine inside the app, but only while the app is in front. A shell would have to be in-process commands (ios_system) or wasm, with the terminal semantics emulated.

| Question | Answer |
|---|---|
| `fork` | `EPERM`. |
| `posix_spawn` of a binary signed into the bundle | `EPERM`. The same goes for a copy in Documents, and `/bin/sh` doesn't exist. |
| `openpty` / `posix_openpt` | Both fail with `EPERM`. **No ptys at all**, so there's no line discipline, `termios`, job control or `SIGWINCH`. |
| `dlopen` of a framework signed into the bundle, then a command's `main` on a thread | Works (ios_system's model). |
| `dlopen` of code added at run time | Refused ("code signature invalid"). New native commands can only arrive in an app update. |
| JIT | `MAP_JIT` gives `EPERM`. An RWX mapping is allowed, but jumping into it gets the process `SIGKILL`ed. |
| A wasm interpreter (wasm3, no JIT) | Works once its 8 GiB guard reservation is turned off. **About 4.4x native** on a CPU bench. An interactive WASI program answers a line in 0.03–0.3 ms. |
| tokio + axum on loopback, reqwest HTTPS, wss through rustls + the platform verifier | All work. HTTPS to control takes 100 ms, and wss to a public echo 276 ms. |
| Ghostty's lib-vt for `aarch64-apple-ios` | Builds with Zig 0.16 through the vendored sys crate's existing iOS path (55 s) and links into the app. 2,000 styled lines plus a snapshot take 13–15 ms on the phone. |
| Backgrounded | Suspended within about 1 s. `beginBackgroundTask` buys 30 s. Brought back to the front, it resumes where it froze, with its loopback server and outbound HTTPS working again. |
| App Store | 2.5.2 forbids downloading code that changes the app's features. a-Shell, iSH and Blink ship anyway, as self-contained terminals. "A machine other people drive remotely" is a much harder story to tell review. |

## 1. Processes and ptys

`probe.c` tries each call and reports `errno`:

```
FAIL fork: 1 Operation not permitted
FAIL posix_spawn …/S31Probe.app/hello: 1 Operation not permitted
FAIL posix_spawn /bin/sh: 1 Operation not permitted
FAIL posix_spawn …/Documents/hello-copy: 1 Operation not permitted
info /bin/sh: No such file or directory
FAIL openpty: 1 Operation not permitted
FAIL posix_openpt: 1 Operation not permitted
```

`hello` is a separate executable that the build signs with the app's identity and puts in the bundle. The sandbox still refuses to spawn it. That rules out the daemon's whole pane model: there's no child process to hold and no pty to read.

**Where the tty goes, iOS terminals fake it.** ios_system (used by Blink and a-Shell) runs each command as a function on a thread, with thread-local `stdin`/`stdout`/`stderr` streams. The terminal's own code reimplements what a pty would do: line editing, `^C`, window size. Programs that expect a real tty (`vim`, `less`, anything using `termios` or `ioctl(TIOCGWINSZ)`) have to be patched to ask the host instead. Blink and a-Shell carry those patches.

## 2. In-process commands (the ios_system shape)

`helpers/cmd.c` builds as `S31Cmd.framework`, signed into `Frameworks/`. The app `dlopen`s it and runs `cmd_main(FILE *out, argc, argv)` on a new thread with an `open_memstream` as its output:

```
ok   dlopen bundled framework + cmd_main on a thread: rc 42, "cmd_main on thread 0x16fd1b000, argc 3: ls -l /tmp"
FAIL dlopen a copy in Documents (runtime-installed code): … code signature invalid …
```

**So every native command has to be compiled into the app ahead of time,** and it ships only in an App Store update. A user can't install a tool, and neither can an agent.

## 3. Code generation and wasm

- `mmap(PROT_READ|PROT_WRITE|PROT_EXEC)` succeeds, and so does `mprotect` from RW to RX. The app is debuggable (`get-task-allow`), which is probably why.
- Writing `mov w0,#42; ret` into the page and calling it ends the process with signal 9.
- `MAP_JIT` gives `EPERM`.

No JIT, then, and an App Store build would be stricter still.

**wasm3, an interpreter, works.** `wasm/bench.c` is a 2M-entry sieve plus a hash loop, built natively and for `wasm32-wasi` with `zig cc`:

| | bench(5), three runs |
|---|---|
| native (arm64, -O2) | 28.0, 26.9, 27.0 ms |
| wasm3 | 119.2, 123.1, 119.0 ms |

That's about 4.4x slower than native. For comparison, the same build on the M4 Air's macOS took 116 ms, and on geek 177 ms.

`wasm/repl.c` is a WASI program that reads lines from stdin and answers each one. The app points fds 0 and 1 at pipes, runs `_start` on a thread and types into it:

```
ok   wasi repl started: "wasi repl ready"
     -> hello      answered in 0.27 ms
     -> ls -la     answered in 0.03 ms
     -> bench      answered in 24.47 ms
     -> exit       answered in 0.07 ms: "bye after 5 lines"
```

**Gotcha: wasm3's guarded memory fails on iOS.** By default it reserves 8 GiB of address space per memory (128 slots) so that bounds checks go away. On iOS that reservation fails, and every load reports `memory allocation failed`. Building with `-Dd_m3GuardedMemory=0` fixes it. Any runtime that relies on large virtual reservations (wasmtime's default memory config, for instance) needs bounds checks on iOS, or the `com.apple.developer.kernel.extended-virtual-addressing` entitlement.

a-Shell takes a different route. It runs wasm commands inside a `WKWebView`, where WebKit's own process may JIT. That's faster, but it costs a web content process and adds a JS bridge for I/O, and WASI there has no sockets and no fork.

## 4. Daemon-shaped Rust in the app

`rt/` is a staticlib (`cargo +1.98 --target aarch64-apple-ios`). It links `illogical-vt` from this repo and uses the daemon's own versions of tokio, axum, reqwest, tokio-tungstenite, rustls (aws-lc-rs) and rustls-platform-verifier:

```
ok   axum bind 127.0.0.1 (0.8 ms): port 63649
ok   GET loopback (3.0 ms): illogical-ish
ok   ws loopback (0.7 ms): echo:hi
ok   GET https://control.illogical.widgets.wtf/ (100.8 ms): HTTP 200 OK
ok   GET https://github.com/ (64.8 ms): HTTP 200 OK
ok   wss echo.websocket.org (platform verifier) (276.4 ms): echoed s31
ok   lib-vt feed 2000 lines + snapshot (13.7 ms): snapshot 110675 bytes, last line "line 1999 hello from the phone"
```

**Nothing in the networking or lib-vt layers stands in the way.** The vendored `libghostty-vt-sys` already handles iOS: it builds Ghostty's xcframework slice and links statically. The only build noise was object files built for iOS 18.5 being linked at 17.0, which `IPHONEOS_DEPLOYMENT_TARGET=17.0` fixes. The daemon's pane, holder, procinfo and port code is all `fork`/pty/`/proc`, and none of it has an iOS form.

## 5. Backgrounding

With the `bg` argument the app logs a tick every second from a thread, and a beat every 5 s from the Rust runtime (a loopback request plus HTTPS to control). Launching Settings over it with `devicectl` backgrounds it.

| | after leaving the screen |
|---|---|
| plain | The last tick came 0.6 s before the "entered background" log line was even written. Nothing more ran for 3 minutes. |
| `beginBackgroundTask` | `backgroundTimeRemaining` was 29.3 s. Ticks and beats continued, then the task expired at +26 s and the process froze at +31 s. |
| brought back to the front (after 3 min) | It resumed at the next tick (`tick 44` right after `tick 43`), and loopback and HTTPS beats worked at once. The process was frozen, not killed. |

The screen wasn't locked for these runs (that needs someone at the phone). Locking only makes things stricter.

What else could keep it running, none of which fits:

- **Background modes** (`audio`, `location`, `voip`, `bluetooth`) are each reviewed for their stated purpose. Playing silent audio to stay alive gets an app rejected.
- **`BGAppRefreshTask` and `BGProcessingTask`** run when the system chooses, for seconds to minutes, usually while charging.
- **`BGContinuedProcessingTask` (iOS 26)** is the closest. A task a person starts keeps running after they leave the app, with a system progress UI they can cancel. It's meant for finite jobs with progress (exports, uploads), it has to come from a person's action, and Apple DTS confirms it pauses when the device locks (FB19916760, still open on iOS 26.2). It might keep a single long command alive while the screen is on. It can't keep a machine up. Testing it needs someone to tap.

**A terminal on an iPhone exists only while the app is open.** Another client could attach to it during that time. Once the phone locks or another app comes forward, the "machine" is gone, its panes are frozen, and its connection to control drops.

## 6. Distribution and review

- **2.5.2** (current text): apps "may not download, install, or execute code which introduces or changes features or functionality of the app." Educational apps get a narrow exception for user-visible, editable code. ([guidelines](https://developer.apple.com/app-store/review/guidelines/))
- **iSH** (x86 emulation, Alpine userland) was threatened with removal under 2.5.2 four days after launch in October 2020. It won on appeal, and Apple apologized. a-Shell got the same notice at the same time. Both are still listed. ([iSH blog](https://ish.app/blog/app-store-removal), [AppleInsider](https://appleinsider.com/articles/20/11/09/apple-backtracks-on-app-store-removal-threat-for-unix-shell-ios-apps), [Michael Tsai](https://mjtsai.com/blog/2020/11/09/ish-and-a-shell-vs-the-app-store/))
- **a-Shell** ships commands through ios_system plus wasm commands, which a user installs with `pkg`. ([a-shell](https://github.com/holzschu/a-shell), [a-Shell-commands](https://github.com/holzschu/a-Shell-commands))
- **Blink** builds all of its local commands into ios_system frameworks. ([blink](https://github.com/blinksh/blink))
- **UTM SE** was rejected, then approved in July 2024 as a JIT-less QEMU (TCTI). Apple then updated 4.7 to allow PC emulators to download software. ([9to5Mac](https://9to5mac.com/2024/07/14/pc-emulator-app-for-iphone-and-ipa/), [OMG Ubuntu](https://www.omgubuntu.co.uk/2024/07/utm-se-qemu-pc-emulator-apple-app-store))
- **What that means for us:** a self-contained local terminal has precedent, and so do user-installed wasm commands (a-Shell). An app whose point is to let *remote* people and agents run commands on the phone has none. Under 4.7.2, an app also may not "extend or expose native platform APIs" to software it offers.
- **EU and Japan alternative marketplaces** (EU since iOS 17.4, Japan since 26.2) still require notarization, and no JIT exemption for ordinary apps turned up. They change who distributes the app, not what the sandbox allows. ([Apple](https://www.apple.com/newsroom/2024/01/apple-announces-changes-to-ios-safari-and-the-app-store-in-the-european-union/), [TechCrunch](https://techcrunch.com/2026/02/22/move-over-apple-meet-the-alternative-app-stores-available-in-the-eu-and-elsewhere/))
- **Testing:** the team is a paid one (its profiles last a year), so development builds and ad-hoc builds (100 devices) need no review. Free provisioning lasts 7 days. External TestFlight builds go through beta review against the same guidelines.

## How to run it

On a Mac with Xcode, rustup's `aarch64-apple-ios` target for 1.98, and Zig 0.16 in `~/.local/opt`:

```
./gui.sh env DEVELOPMENT_TEAM=<team> ./build.sh <device-id>      # build, sign, install
xcrun devicectl device process launch --device <id> --terminate-existing --console com.jhgaylor.s31probe
xcrun devicectl device process launch --device <id> --terminate-existing com.jhgaylor.s31probe bg [bgtask]
xcrun devicectl device copy from --device <id> --domain-type appDataContainer \
  --domain-identifier com.jhgaylor.s31probe --source Documents/bg.txt --destination bg.txt
```

### Gotchas

- **Signing from ssh fails with `errSecInternalComponent`** because the login keychain is locked in an ssh session. `gui.sh` runs the build as a one-shot LaunchAgent in `gui/501`, where the keychain is unlocked while the user is logged in. `xcodebuild -allowProvisioningUpdates` then created the app's profile without anyone at the Mac.
- `xcodebuild -derivedDataPath` needs `-scheme`. A hand-written project with no shared scheme builds with `-target` and `SYMROOT` instead.
- Helper executables and frameworks that a run-script phase copies into the bundle must be `codesign`ed with `$EXPANDED_CODE_SIGN_IDENTITY` before Xcode signs the app.
- `devicectl … launch --console` is the easy way to read results. The app also writes `Documents/*.txt`, which `devicectl device copy from` can fetch even while the app is suspended.
