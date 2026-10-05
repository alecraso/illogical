# S31: a phone as a machine (#246)

Can a phone be an illogical machine, with illogicald running on it and panes that every other client can see and drive? A phone that's only a client already works. This spike asks about running terminals *on* the phone.

- **iOS** ran on a real iPhone 15 Pro (iOS 26.6.1) through jake-air. Details are in [`ios/README.md`](ios/README.md).
- **Android** ran on an Android 16 emulator (API 36, x86_64, `google_apis`) on geek. The real-phone run is still open (see [Not yet measured](#not-yet-measured)).

## Answers

**Android: go.** illogicald builds for Android with three small changes and runs inside an ordinary app that targets API 36. That app runs it as a foreground service, its panes run in real ptys, and a full Debian runs there under proot. Two limits shape the track:

- Android kills an app's child processes beyond 32 unless the person turns that off once.
- proot makes stat-heavy work about 15× slower.

**iOS: no-go as a machine.** The sandbox refuses `fork`, `posix_spawn` and ptys, and the app freezes about a second after it leaves the screen. The iPhone stays a client, and S33 (#268) asks what it can offer as a "hand" instead.

| Question | Android (emulator, API 36) | iOS (iPhone 15 Pro) |
|---|---|---|
| Daemon and lib-vt build | Yes, for `aarch64-` and `x86_64-linux-android` with NDK r29 and Zig 0.16 (`ANDROID_NDK_HOME` set). The daemon needed 14 `cfg` gates widened to `android` and one hard-link fallback. | lib-vt builds and links. The daemon's pane model has no iOS form. |
| Run it from our own app | Yes. It ships as `lib/<abi>/libillogicald.so`, and a `specialUse` foreground service runs it in `untrusted_app`. | No processes at all (`EPERM`). |
| ptys and a shell | `/dev/pts/N`, `/system/bin/sh` (mksh) and toybox, as the app's uid. | `openpty` gives `EPERM`. |
| Exec what the app downloads | Direct exec is refused (exit 126, SELinux `execute_no_trans`). **`/system/bin/linker64 FILE` works.** | Refused, and `dlopen` of new code fails its signature check. |
| A real userland | **Debian 13 under proot works**: `apt-get install git python3` took 22 s. Termux works too, as is. | ios_system or wasm only (wasm3 is about 4.4× slower than native). |
| proot's cost | Spawning and plain CPU work are fine. **A stat-heavy `find` is about 15× slower** (386 ms against 23–31 ms). | n/a |
| Many panes | **Android's phantom process killer kills children beyond 32**, the daemon included. With the restriction off: 119 processes, no kills. | n/a |
| Doze, screen off | With the foreground service: a 10 s tick over TCP to geek arrived on time for 5 min of forced deep idle. (The emulator never suspends its kernel; see below.) | Frozen in about 1 s; `beginBackgroundTask` buys 30 s. |
| Outbound TLS for control | Plain Linux networking (the S31 app has `INTERNET`). | tokio, axum, reqwest and wss work in the app while it's open. |

## Android

Everything ran from `android/`:

- `env.sh` sets up the NDK.
- `build-apk.sh` builds the test app without Gradle.
- `app/` holds the app: an activity that starts a foreground service, which runs and restarts the daemon.
- `il.sh` runs the CLI as the app through `run-as`.

### 1. Building for Android

`cargo build --release -p illogicald -p illogical --target aarch64-linux-android` works with the NDK's clang as linker and `ANDROID_NDK_HOME` set. Ghostty's build already knows Android: `pkg/android-ndk` finds the NDK's sysroot from that variable, and fails with `AndroidNDKNotFound` without it. The binaries are 47 MB (daemon) and 12 MB (CLI) unstripped.

Three source changes, all on this branch:

- **`target_os = "android"` isn't `"linux"`.** The 14 Linux gates in the daemon (procinfo's `/proc`, `SO_PEERCRED`, the heap and `sys.rs`) now read `any(target_os = "linux", target_os = "android")`. Without that, procinfo has no `imp` module and `holder.rs` calls `getpeereid`, which bionic lacks.
- **SELinux refuses hard links in app data** (and in `/data/local/tmp`). The local token is written aside and hard-linked in, which failed with `EACCES`. On Android it now renames instead. A milestone should use `renameat2(RENAME_NOREPLACE)` (bionic has it from API 30).
- (No change needed:) the web bundle has to be built first, as for any release build.

### 2. Where it can run

| Where | Result |
|---|---|
| adb's shell user in `/data/local/tmp` | The daemon starts, then can't bind its Unix sockets (`EACCES`, SELinux). Not a real target anyway. |
| **Our own app** (targetSdk 36, `extractNativeLibs="true"`) | Works. The daemon, its IDE relay and each pane's `_shim` all run as `untrusted_app` children of the app's process. The socket and state live in `files/.local/state/illogical`. |
| **Termux** (GitHub build 0.118.3, targetSdk 28, `untrusted_app_27`) | Works with no changes. Copy both binaries into `$PREFIX/bin` and run `illogicald`, and the panes get Termux's native packages (git 2.56). |

Two apps each running a daemon collide on `127.0.0.1:7681`, because loopback ports are shared across apps. `--listen 127.0.0.1:0` avoids that, and an Android daemon should always pick its port.

### 3. Userland: what a pane can run

Out of the box, a pane in our own app gets mksh and toybox. The app can't list `/system/bin`, but it can run what's there. Since API 29, a file the app writes can't be `exec`ed (exit 126; the audit log shows `execute_no_trans` denied). Only the APK's extracted native libs can be. **The loader is the way around it:** `/system/bin/linker64 ~/bin/tool` runs a file the app wrote (the audit log shows `execute` *granted* but audited). Termux's newer builds rely on the same opening, and Android may close it one day.

Three ways to give panes a real userland:

1. **proot and a distro rootfs.** Ship Termux's `proot`, its `loader` and `libtalloc`/`libandroid-shmem` as native libs (renamed `lib*.so` and patched with patchelf, see `build-apk.sh`). Set `PROOT_LOADER` to the loader lib, unpack a rootfs into `files/`, and run `proot -0 --link2symlink -r ~/debian ...`. Debian 13 then works, with apt, git and python3. Measured:

   | | Android sh + toybox | Debian under proot |
   |---|---|---|
   | 500 spawns of `true` | 1,650 ms | 655 ms |
   | 200k-iteration shell loop | 296 ms | 111 ms |
   | 50 MB through a pipe | 11 ms | 16 ms |
   | `find` over 5,784 files | 23–31 ms | **386 ms** |

   The shells differ (mksh against dash), so the first two rows compare whole stacks. The last row is the real cost: every path-taking syscall goes through ptrace, which makes `git status` on a big repo, builds and `npm install` noticeably slow.

   **Gotchas:**
   - Alpine's busybox can't fork on x86_64. musl calls the old `fork` syscall, and the app seccomp filter answers `ENOSYS`. arm64 has no such syscall, and glibc uses `clone` everywhere, so it's an emulator-only problem.
   - Unpacking a rootfs with toybox `tar` fails on hard links (`perl5.40.1`). Unpack inside proot, with `--link2symlink`.
2. **Termux.** It has native packages and no ptrace, but it's a separate app the person installs from F-Droid or GitHub. Its Play Store build is a different, restricted one. illogical would be a Termux package, or `install.sh` would learn about Termux.
3. **Our own native package set**, built for our prefix and run through `linker64` (Termux's approach). That's fast, but it means maintaining a distribution. Not worth it.

**Android 16's Linux Terminal** (an AVF Debian VM) is a fourth host, at native speed. It's on Pixels and a few other phones, and not in the emulator image, so it waits for the real phone. illogicald would run there as on any arm64 Linux, though not under our app's lifetime control.

### 4. Lifetime

**The phantom process killer is the limit that matters.** Android 12 and later track an app's child processes as "phantom" and allow 32 in total (`max_phantom_processes=32` in `dumpsys activity settings`).

- 40 panes made 87 processes. Within a few minutes, and at once when the app went to the background, the system logged `Killing PhantomProcessRecord …: Trimming phantom processes` 54 times. The daemon was among them, and the service's supervisor restarted it.
- **Each pane costs two processes**, its `_shim` and its shell, plus whatever the person runs in it. The daemon and its IDE relay take two more, so at most about 15 idle panes fit.
- With `settings put global settings_enable_monitor_phantom_procs false` (Developer options → "Disable child process restrictions" on Android 14+), 119 processes ran in the background for a minute with no kills.

So the app has to:

- **cut processes:** no `_shim` on Android (exec the shell directly, or have one helper hold every pane), and no IDE relay process;
- **ask the person to turn the restriction off** during setup. It's one toggle in Developer options, and Termux's docs ask the same;
- **put the daemon in charge of keeping panes:** a kill should look like any daemon restart (M2's restore), not lost work.

**Doze:** after `dumpsys deviceidle force-idle` with the screen off and the battery "unplugged", a pane sent a line to geek every 10 s over TCP for 5 minutes. Every line arrived on time. The foreground service keeps the app's network out of Doze's restrictions. An emulator never suspends its CPU, though, and a real phone does with the screen off unless a wakelock is held. The test app holds a partial wakelock when started with `--ez wakelock true`, ready for that check.

### 5. What M-numbers would take

1. **illogicald on Android, in Termux.** Add the targets to CI, merge the `cfg` changes and the token fix, have the daemon pick its port, and teach `install.sh` about Termux (`termux-wake-lock`, a `termux-services` runit service). This one is cheap, and it already makes a phone a machine for people who use Termux.
2. **The illogical Android app as a machine.**
   - The daemon runs as a native lib under a `specialUse` foreground service with a wakelock setting.
   - The web client runs in a WebView, and the app is the cloud client of M48 as well.
   - Debian under proot is the default userland, unpacked on first run, with plain Android `sh` as the fallback.
   - Panes run on a process budget, and setup checks the phantom-process toggle.
   - Joining control works as `illogicald join` does.
   - Distribution is GitHub and F-Droid first. Google Play's policy against downloading executable code is the open risk (Termux left Play over target-SDK rules). Check it before promising Play.
3. **Later, on a trigger:** the Linux Terminal VM as a fast userland where it exists, and native packages if proot's file-system cost turns out to hurt in daily use.

## iOS

See [`ios/README.md`](ios/README.md). In short:

- no processes and no ptys;
- in-process commands must be compiled into the app;
- a wasm interpreter runs at about 4.4× native;
- daemon-shaped Rust (axum, reqwest, wss, lib-vt) works while the app is open;
- the app is suspended about a second after leaving the screen, and a background task buys 30 s.

A terminal on an iPhone lives only while the app is in front, and App Review has no precedent for a phone that remote people and agents drive. **The iPhone stays a client.** S33 (#268) asks whether it can be a hand: device tools (camera, location, Shortcuts, the on-device model) that agents call through control.

## Not yet measured

These need the real Android phone (Jake chose to skip it for now, 2026-10-05), with the arm64 APK from `build-apk.sh arm64-v8a aarch64-linux-android`:

- the phantom process killer on a vendor ROM, and its developer toggle there;
- screen off on a real CPU: does a pane's timer stall without the wakelock, and does it keep time with it;
- battery over an hour, idle, with the wakelock;
- Debian under proot on arm64 (fork through `clone`), and the `find`/`git status` cost on a real repo;
- joining the hosted control from the phone, and driving its panes from another client;
- whether the phone has Android 16's Linux Terminal, and illogicald inside it.

The iOS runs didn't lock the screen. Locking only makes things stricter.

## How to run it

On geek: the Android SDK is in `~/Android/Sdk` (cmdline-tools, NDK 29.0.14206865, emulator, `android-36` platform and Google APIs x86_64 image, build-tools 36.0.0), and the AVD is `s31`.

```
. android/env.sh
cargo build --release -p illogicald -p illogical --target x86_64-linux-android   # needs web/dist
~/Android/Sdk/emulator/emulator -avd s31 -no-window -no-audio -gpu swiftshader_indirect &
android/build-apk.sh x86_64 x86_64-linux-android      # proot libs from ~/.cache/s31-dl/extra/<abi> if present
adb install -r -g ~/.cache/s31-target/s31-apk/x86_64/s31.apk
adb shell am start -n wtf.widgets.illogical.s31/.Main [--ez wakelock true]
adb push android/il.sh /data/local/tmp/
adb shell run-as wtf.widgets.illogical.s31 sh /data/local/tmp/il.sh ls      # the CLI, as the app
printf 'uname -a\n' | adb shell run-as wtf.widgets.illogical.s31 sh /data/local/tmp/il.sh send %1 -
```

The proot extras are Termux's `proot`, `libtalloc` and `libandroid-shmem` packages for the ABI, renamed to `lib*.so`. Patch them with `patchelf --replace-needed libtalloc.so.2 libtalloc.so --remove-rpath` (proot) and `--set-soname libtalloc.so` (talloc). The Debian rootfs is `docker export` of `debian:stable-slim` for the platform.
