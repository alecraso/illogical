# S29: Windows feasibility (#216)

Can illogical run panes on Windows the way it does on Linux and macOS? This spike answers the questions that could change the track's shape (PLAN.md, "Windows track") before any milestone starts. Everything here was run in the Windows 11 VM on geek: Windows 11 Pro 25H2 (build 26200), 8 vCPUs, under KVM. The VM runs in docker as `illogical-win` and is reached with `ssh win`.

The test program is `conpty-host/`, a pane host of about 1,000 lines. It owns a pseudoconsole and serves it on a named pipe, and it includes the benches. The `*.ps1` files drive the multi-session tests.

## Answers

**Go, with the shape the plan proposed, plus one change: ship Microsoft's current ConPTY.**

| Question | Answer |
|---|---|
| Ghostty's lib-vt on `x86_64-windows-msvc` with Zig 0.16 | Builds, and the `vt` crate's 63 tests pass. It needed one fix: our patch didn't apply to Git for Windows' CRLF checkout. |
| A detached pty host keeps a pane across its parent's exit | Yes, including the parent's ssh session ending. Output from the gap arrives, state survives, and reconnecting takes 0.1 ms. |
| Cost of the host's extra hop | About 0.05 ms per keystroke and nothing measurable in bulk. |
| Resize, close, the job, grace | All work. Close takes about 60 ms, and it takes the process tree with it. |
| ConPTY's latency | Windows' own ConPTY adds about 16 ms (one frame) to every echo. **`conpty.dll` 1.25 echoes in 0.07 ms** and moves 190 MB/s against 30. |
| Output fidelity | Both deliver every line (190,650 of 190,650). |
| axum over a named pipe | Works: 0.12 ms per request, against 0.29 ms over loopback TCP. Chunked streaming works. Another user is refused by the DACL. |
| The CLI's client | A synchronous pipe handle can't read and write at once (a write waited behind a read for more than 3 s). Duplex paths need overlapped I/O. |
| Start at logon, and logoff | A logon task (interactive, no admin) starts at logon, and logoff ends it. An S4U task starts at boot and survives logoff, but DPAPI fails there ("Access is denied"), so no Credential Manager and no Git Credential Manager. |
| PowerShell integration | OSC 133 and OSC 7 pass through. Script files are blocked by the default `Restricted` policy, so inject inline with `-EncodedCommand`. |

## 1. Ghostty's lib-vt under MSVC

`cargo test -p illogical-vt` in the VM, with Visual Studio Build Tools 2022 (VC tools), Rust stable, Git for Windows 2.51 and Zig 0.16.0 on `PATH`: **63 passed**. The build script's existing `x86_64-windows-msvc` mapping and its static `ghostty-vt-static.lib` link needed no changes.

**One failure on the way, and the fix:** `git apply 0001-no-signal-stack.patch` failed (`patch failed: src/lib_vt.zig:376`). Git for Windows installs with `core.autocrlf=true`, GitHub's Windows runners use the same setting, and that converts both our patch and the Ghostty clone to CRLF. This branch fixes it two ways:

- `build.rs` clones Ghostty with `-c core.autocrlf=false`;
- `.gitattributes` marks our patches `-text`.

With a fresh CRLF checkout of the repo and the new attributes, the patch applies and the tests pass. Both fixes are on this branch so M55's CI job inherits them.

## 2. The pane host: a detached ConPTY owner on a named pipe

The shape from the plan works. `conpty-host spawn` starts `conpty-host host` detached (`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB`) and exits. The host:

- creates the pseudoconsole;
- starts the program suspended, puts it in a Job Object with `KILL_ON_JOB_CLOSE`, then resumes it;
- serves `\\.\pipe\NAME` with a DACL that admits only the owner and SYSTEM;
- checks each client's user SID against its own (`GetNamedPipeClientProcessId`, then the token's user, then `EqualSid`).

Frames are kind (u8), length (u32 LE) and payload: data, resize, close, exit.

**The pane outlives its "daemon" and the ssh session that started it** (`life1.ps1`, `life2.ps1`):

1. An ssh session starts a host running `pwsh`, types `$x = 42; 1..8 | % { 'tick' + $_; Start-Sleep 1 }; 'TICKS' + 'DONE'` into it, disconnects and ends.
2. Six seconds later a second ssh session connects in 0.1 ms.
3. It receives the ticks printed while nobody was connected, then `TICKSDONE`, and `'v' + $x` answers `v42`.

Breaking away from sshd's job is allowed (`broke away from the job: true`), so a daemon started from ssh can leave panes behind.

**While nobody is connected, output waits instead of being dropped.** The host forwards ConPTY's output through a channel of four chunks. With no client the channel fills, the host's reader stops reading, and the program blocks on its writes, as it does on a pty master that nobody reads. A real host must also keep any chunk it read but couldn't send when the client left; the spike's host drops it.

**Resize** from the client reaches the program: after a resize to 101x37, `[Console]::WindowWidth`/`WindowHeight` print `101x37`, directly and through the host.

**Close** (`ClosePseudoConsole`, then `TerminateJobObject` after 3 s) ends `pwsh` with `0xC000013A`, which is `STATUS_CONTROL_C_EXIT` from its `CTRL_CLOSE_EVENT`, about 60 ms after the close. A `ping -t` it had started is gone too, because the job takes the whole tree.

**Lease and grace** work as holder's do. When no client connects within `--grace`, the host closes the pane and exits.

### Gotchas found

- **Null std handles with `STARTF_USESTDHANDLES`.** When the creating process's own std handles are redirected (an ssh session, a logon task, a service), a ConPTY child inherits them and never reads the pseudoconsole. The direct bench's `echo` child exited at once until the host set `STARTF_USESTDHANDLES` with null handles. A detached host has no std handles, which is why the problem first showed up only in the direct mode.
- **A running exe can't be replaced.** `cargo build` failed with "Access is denied" while old hosts ran from `target\release`. The daemon's upgrade path (M59) has to rename the running exe aside, and the pane hosts need to run from a versioned path, because they outlive upgrades.
- **Windows PowerShell 5.1 splits native arguments on embedded double quotes.** Anything that passes text to an exe through it (scripts, `install.ps1`) needs care, or should use `-EncodedCommand`.

## 3. ConPTY's latency and throughput, and which ConPTY

Keystroke to echo goes through a raw echo program (`conpty-host echo`, 300 keys). Bulk is 20 MB of 110-byte lines: `cmd /k type` writes them a line at a time, and our `cat` writes 64 KB at a time. "Direct" means the bench owns the pseudoconsole; "host+pipe" goes through a detached host and its named pipe.

| ConPTY | echo direct p50 / p99 | echo host+pipe p50 / p99 | bulk `cat` | bulk `type` | lines received |
|---|---|---|---|---|---|
| Windows' own (conhost, build 26200) | 15.6 / 16.2 ms | 15.5 / 16.1 ms | 30 MB/s | 1.2 MB/s | 100% |
| Windows' own, passthrough flag (8) | 15.4 / 16.2 ms | | | | |
| `conpty.dll` 1.25 + `OpenConsole.exe` | **0.07 / 0.17 ms** | **0.12 / 0.16 ms** | **189 MB/s** | 1.3 MB/s | 100% |
| the same, passthrough flag (8) | 0.07 / 0.11 ms | | 193 MB/s | 1.3 MB/s | 100% |

- **The host's pipe hop costs about 0.05 ms per keystroke and nothing measurable in bulk** (15.1 s through the host against 15.2 s direct for `type`).
- **Windows' own ConPTY adds a frame (about 16 ms) to every echo.** It renders on a 60 Hz timer. Microsoft's current ConPTY, the `Microsoft.Windows.Console.ConPTY` NuGet package (MIT; `conpty.dll` plus `OpenConsole.exe`, about 1.5 MB), echoes in 0.07 ms and moves output six times faster. It exports the same three functions, so loading it is `LoadLibrary` plus `GetProcAddress`. WezTerm and VS Code ship it for the same reason. **Recommendation: ship it** (M56) and fall back to the inbox ConPTY if it's missing.
- Neither ConPTY dropped lines: every one of 190,650 lines arrived, so scrollback is complete.
- `type` is slow on both ConPTYs (`cmd` writes a line at a time), so that cost is `cmd`'s and not ours.
- Flag 1 (`INHERIT_CURSOR`) makes ConPTY ask the terminal for the cursor position (`ESC[6n`) and wait for the reply. The daemon's VT would have to answer it, so don't use the flag.

## 4. Shell integration in PowerShell

OSC 133 (prompt marks) and OSC 7 (cwd) come through both ConPTYs unchanged (`shellint.ps1`).

- **Windows' default execution policy is `Restricted`.** No script file runs, including a user's `$PROFILE` and any `illogical.ps1` we would install. Changing the policy for them is out of the question.
- **`pwsh -NoExit -EncodedCommand <base64>` is not covered by the policy.** The spike wraps whatever `prompt` the profile set up (oh-my-posh and Starship included), so integration works with no file and no policy change. The wrapper emits `OSC 133;D;<code>`, `OSC 7;file://HOST/C:/path`, `OSC 133;A`, the user's prompt, then `OSC 133;B`. Recommendation for M60: start pwsh and Windows PowerShell that way.

## 5. axum and the CLI over a named pipe

`conpty-host http-serve` runs the same axum router on a named pipe and on loopback TCP. The pipe side uses `PipeListener`, an `axum::serve::Listener`, which:

- keeps a fresh pipe instance waiting while the last one serves, as a socket backlog would;
- refuses any client whose token's user isn't ours.

`http-bench` makes a request on a fresh connection each time, as the CLI does, with a blocking `std::fs::File` on `\\.\pipe\NAME`:

| | p50 | p99 |
|---|---|---|
| `/ping` over the named pipe | 0.117 ms | 0.188 ms |
| `/ping` over loopback TCP | 0.286 ms | 0.417 ms |

A chunked `/stream` (five chunks, 100 ms apart) arrives in 556 ms.

**Access.** The pipes' DACL is `D:P(A;;GA;;;OW)(A;;GA;;;SY)`. A second local user, logged in over ssh, gets "Access to the path is denied" on both the HTTP pipe and a pane host's pipe; the owner connects. Use the user's SID in the DACL rather than `OW`: when an elevated admin creates the pipe, the owner can be the Administrators group. The SID check behind the DACL catches that case anyway.

**Duplex needs overlapped I/O.** One thread blocked reading a synchronous pipe handle held up another thread's write on the same handle for more than 3 s. On Unix, a `UnixStream` read and write from two threads don't wait for each other. Request and response are fine with a blocking handle. Anything that reads and writes at once (`attach`, the TUI, WebSockets over the local socket) needs overlapped I/O: tokio's `NamedPipeClient`, or a handle opened with `FILE_FLAG_OVERLAPPED`. This decides how M57 restructures the CLI's loops.

**Every instance must be created before the previous one is handed to a client**, or a client arriving in between gets `ERROR_PIPE_BUSY`/not found. The listener does that, and the CLI's open retries on error 231 (all instances busy).

## 6. Starting at logon, and logging off

`logon-register.ps1` registers two tasks that each start a pane host running `pwsh`. `logon-check.ps1` finds them and types into their panes.

| Task | After a reboot | Session | After `logoff` | DPAPI in the pane |
|---|---|---|---|---|
| `s29-logon`: at logon, as the user, `Interactive`, `RunLevel Limited` | running, pane answers | 1 (the user's) | gone | works |
| `s29-s4u`: at startup, as the user, `S4U` (no password stored) | running before anyone logs in | 0 | still running, pane answers | **fails: "Access is denied"** |

- **A logon task is the default (M59).** It behaves like the macOS launchd agent and the Linux user unit without linger: up while you're logged in, gone when you log off. Panes keep the user's full credentials: DPAPI, Credential Manager and Git Credential Manager, and network identity.
- **S4U is the "keep panes across logoff" option**, with its cost stated. With no password behind the logon there's no DPAPI, so stored git credentials and similar don't work in those panes, and there are no network credentials either. Offer it as `illogicald install --survive-logoff`, not by default.
- Registering an `AtStartup` S4U task was done from an admin session. Whether a standard user can register one for themselves (it needs "Log on as a batch job") is left to M59.
- Logoff ends everything in the user's session, detached or not; breakaway only gets a process out of a job, not out of its session.

## What this changes in the plan

- **M55** takes this branch's CRLF fix (`build.rs` and `.gitattributes`). The CI job needs Zig 0.16 and the VC tools, which `windows-latest` has.
- **M56** ships `conpty.dll` and `OpenConsole.exe` from `Microsoft.Windows.Console.ConPTY` (MIT) beside `illogicald.exe`, loaded with `LoadLibrary` and falling back to the inbox ConPTY. Spawning sets `STARTF_USESTDHANDLES` with null handles. The local socket is a named pipe with a per-user SID DACL and the `PipeListener` shape from this spike, and the CLI's client retries on error 231. The daemon's state directory can't be `%LOCALAPPDATA%\illogical`, because the desktop app's NSIS installer puts the app there (M54); use `%LOCALAPPDATA%\illogical\state`.
- **M57**: duplex paths use overlapped I/O (tokio's named pipe client), not blocking handles.
- **M58** is this spike's host, made real:
  - keep the chunk in flight when a client leaves;
  - a record file for exit codes;
  - run hosts from a versioned path, because a running exe can't be replaced;
  - `holder::collect` over the pipes.
- **M59**: a logon task by default, and S4U as an opt-in with its DPAPI limit spelled out. Upgrades rename the running `illogicald.exe` aside.
- **M60**: shell integration through `-EncodedCommand`, never a profile file.

## Running it again

In the VM (`ssh win`), with the Rust and VC tools installed:

```
cd C:\s29\conpty-host; cargo build --release
$env:S29_CONPTY_DLL = "C:\s29\conpty\conpty.dll"   # or unset for the inbox ConPTY
.\target\release\conpty-host.exe bench echo --direct
powershell -ExecutionPolicy Bypass -File ..\life1.ps1 p1   # then, from a new session:
powershell -ExecutionPolicy Bypass -File ..\life2.ps1 p1
```

`conpty.dll` and `OpenConsole.exe` come from the `Microsoft.Windows.Console.ConPTY` package on NuGet (1.25.260930003 here): `runtimes/win-x64/native/conpty.dll` and `build/native/runtimes/x64/OpenConsole.exe`, in one directory.
