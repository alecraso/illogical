# S9: memory per pane (does M9 need to happen?)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s9-memory/<file>`.

Run 2026-10-01 on geek (Ryzen AI MAX+ 395, 32 threads, 121 GB). **Result: the
M9 trigger holds today (3.1 to 3.7 MB of daemon RSS per idle pane), but
parking isn't what fixes it.** Two build-level fixes take an idle pane from
3.3 MB to 0.48 MB, under M9's own "done" bar of 1 MB. What really costs
memory is scrollback (about 33 MB per pane with 10k lines) and memory the
allocator keeps after it's freed. Parking would help with those, but a
smaller cap and a fixed malloc threshold help first.

## Setup

- **Binary.** `target/release/illogicald`, copied to `work/illogicald`
  (mtime 08:43, sha256 `cc8dcb43…`). It was built from the tree that became
  `77ad4ee` (M6 blocks, 08:45). HEAD `a09543a` changes only PLAN.md since
  then. libghostty-vt `8953a74` (Ghostty `22d13172`), built `ReleaseSafe`
  as `.cargo/config.toml` sets it.
- **Variants.** Built from `git archive HEAD` into my own
  `CARGO_TARGET_DIR`s under `work/`:
  - `illogicald-tls`: Ghostty with `ghostty-no-signal-stack.patch`, still
    ReleaseSafe.
  - `illogicald-tls-fast`: the same patch, plus
    `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast`.
- **Flags.** `--listen 127.0.0.1:774x --shell "bash --norc --noprofile"
  --no-manager-env --state-dir work/state-<scenario>
  --wisp-token-file work/no-wisp-token` (so no VM panes). `NOTIFY_SOCKET` is
  unset, so there are no systemd scopes and no FD store. Every pane is
  `shim → bash --posix` (with shell integration on). `PS1='$ '`.
- **How it's driven.** Panes are opened with `POST /api/run` (one tab each,
  80x24). Content scenarios also keep one WebSocket that claims every tab at
  200x50 with `View` but attaches to nothing. Clients connect to `/ws` and
  `Attach` to every pane with `offset: null`, so each one gets a fresh
  snapshot.
- **What's measured.** `/proc/PID/smaps_rollup` (Rss, Pss,
  USS = Private_Clean + Private_Dirty) and `status` (Threads) for the daemon.
  Its descendants are summed by comm. Each reading is taken after RSS has
  stopped moving (<0.5% over 2s), and content scenarios also wait 8s more
  for the idle checkpoint (5s). USS and RSS of the daemon differ by about
  5 MB (shared libc and binary text), so "per pane" figures use RSS deltas.
- **No heaptrack or valgrind on geek.** The breakdown comes from an
  engine-only microbench (`engine/`, see below), from differential runs and
  from `readelf`.

## Results

### 1. Idle empty panes (80x24, prompt only)

Daemon only. "Per pane" is (RSS − baseline) / (N − 1). The baseline has the
one pane every daemon starts with.

| panes | daemon RSS | per pane | threads | bash PSS (all) | shim PSS (all) |
|---|---|---|---|---|---|
| 1 (baseline) | 23.8 MB | | 38 | 0.8 MB | 2.2 MB |
| 10 | 52.0 MB | 3.2 MB | 84 | 7.6 MB | 12.8 MB |
| 50 | 172.5 MB | 3.1 MB | 284 | 36.7 MB | 53.3 MB |
| 200 | 738.7 MB | 3.7 MB | 1,034 | 143 MB | 203 MB |
| 500 | 1,614 MB | 3.3 MB | 2,534 | 356 MB | 503 MB |
| close 499 of them | 190.9 MB | | 39 | | |
| reopen to 500 | 1,622 MB | 3.3 MB | 2,534 | | |

- **500 idle shells cost about 2.47 GB in all today**, or 4.9 MB per pane:
  - the daemon, 1.61 GB;
  - shims, 0.50 GB (1.02 MB USS each);
  - bash, 0.36 GB (0.72 MB USS each).

  That is tmux territory (about 5 MB), not Superlogical's (about 400 KB).
- **Five threads per pane:** `pane-vt`, `-read`, `-write`, `-wait`, and an
  unnamed reaper for the shim (`child.wait()`) that inherits the name
  `paneN-vt`. 32 tokio workers on top.
- **Closing panes doesn't return memory** (191 MB is held for 1 pane). It
  isn't a leak: reopening 500 panes ends at the same 1.62 GB. glibc keeps
  the freed memory. See §6.
- `MALLOC_ARENA_MAX=1` changes nothing here (3.07 MB per pane at 50).

The same idle runs with the cheap fixes:

| binary | 500 idle: daemon | per pane | shim USS each | all in (daemon + shims + bash) |
|---|---|---|---|---|
| today | 1,614 MB | 3.26 MB | 1,021 KB | 2.47 GB (4.9 MB per pane) |
| + no Zig signal stack | 994 MB | 2.01 MB | 765 KB | 1.73 GB (3.5 MB per pane) |
| + ReleaseFast libghostty | **246 MB** | **0.48 MB** | 765 KB | 0.99 GB (2.0 MB per pane) |

### 2. Where an idle pane's 3.3 MB goes

| what | per pane | how we know |
|---|---|---|
| Static TLS: 5 threads × 256 KiB | ~1.3 MB | see below |
| libghostty page memory filled in ReleaseSafe | ~1.5 MB | engine bench: an empty engine is 1,602 KB in ReleaseSafe and 67 KB in ReleaseFast |
| Thread stacks, sigaltstacks, arenas, the read thread's 64 KiB buffer, the ring holding the prompt, channels and the rest | ~0.3 to 0.4 MB | the remainder (the tls-fast build's 0.48 MB, minus the engine's 67 KB) |
| bash (outside the daemon) | 0.72 MB USS | smaps |
| shim (outside the daemon) | 1.02 MB USS (0.77 with the TLS fix) | smaps |

**The TLS.** `readelf` shows a `.tbss` of 0x402c0 bytes (256 KiB) in
illogicald. It is Zig std's
`Thread.maybeAttachSignalStack.global.signal_stack`, a
`threadlocal [std.options.signal_stack_size]u8`. The default size is
`1 << 18`, and libghostty's `lib_vt.zig` doesn't override it. glibc gives
every thread in the process (Rust's too) its own static TLS block and zeroes
it, so each thread costs about 260 KB:

- 5 bare threads cost 1,337 KB in the engine bench, and 122 KB with the
  patch;
- the patch is `options.signal_stack_size = null`, which takes `.tbss` down
  to 0x1b0 bytes.

It also costs 8 MB across the 32 tokio workers, and 256 KB in every shim.

**The page fill.** `PageList.initPages` says it plainly: *"In runtime safety
modes we have to memset because the Zig allocator interface will always
memset to 0xAA for undefined."* Every page from the pool is written in
ReleaseSafe, including the preheated ones, so each screen costs about
1.6 MB before it holds any text. Entering the alternate screen adds a second
screen (3,184 KB in ReleaseSafe against 183 KB in ReleaseFast).

### 3. Engine-only microbench (one libghostty terminal as `GhosttyEngine` configures it)

KiB of RSS per engine, averaged over K engines (`engine/`):

| state | ReleaseSafe (today) | ReleaseFast |
|---|---|---|
| empty, 80x24 or 200x50 | 1,602 | 67 |
| alt screen entered, empty | 3,184 | 183 |
| full screen, 200x50, 16 colours (`screen16.ans`) | 3,586 | 200 |
| full screen, 200x50, unique truecolor per cell (`screen.ans`) | 4,087 | 696 |
| 10k lines (~100 cols, 256-colour), 80x24 | 15,816 | 13,678 |
| 10k lines, 200x50 | 19,052 | 16,948 |
| 200k lines, 200x50 (64 MiB cap reached) | 76,948 | 73,123 |

- **Scrollback costs about 1.7 KB per row at 200 cols.** That is 8 bytes a
  cell whatever the content, plus row and style metadata.
- **The 64 MiB byte cap (`SCROLLBACK_BYTES`) holds about 38k rows at 200
  cols.** S5's 64,511 rows were at 80 cols.
- **Raising or lowering the cap**, measured with a raw Terminal in
  ReleaseSafe at 200x50:

| `set_scrollback_max_bytes` | 10k lines | 200k lines |
|---|---|---|
| 4 MiB | 5.8 MB | 15.5 MB |
| 16 MiB | 17.9 MB | 27.6 MB |
| 64 MiB (today) | 19.1 MB | 76.9 MB |

### 4. Panes with content (daemon, N panes at 200x50)

"Per pane" is the daemon's RSS growth from the same panes empty. Child
processes are listed separately.

| scenario | N | today | + TLS fix | + TLS + ReleaseFast | + malloc tunables¹ |
|---|---|---|---|---|---|
| full screen, 16 colours (alt screen), then `exec sleep` | 50 | 3.1 MB | 3.1 MB | 1.2 MB | |
| full screen, truecolor per cell | 50 | 4.9 MB | | | |
| `nvim --clean bench.py` (syntax on) | 20 | 2.6 MB | 2.6 MB | 1.2 MB | |
| 10k lines of scrollback | 50 (20 with tunables) | **32.8 MB** | 33.6 MB | 34.4 MB | **19.8 MB** |
| 200k lines (64 MiB cap) | 10 (5 with tunables) | **162.6 MB** | 164.3 MB | 161.5 MB | **96.6 MB** |

¹ `GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072:glibc.malloc.trim_threshold=131072`
on today's binary.

- **With scrollback, the daemon costs about twice what the engine holds**
  (32.8 MB against 19 MB at 10k lines; 162 against 77 at the cap). The
  extra is:
  - **The ring.** 1.3 MB at 10k lines, and 4 MiB once a pane has written
    more than about 2 MiB. `Ring::push` extends before it drains, so the
    `VecDeque` doubles to 4 MiB of capacity, and as a ring it touches all of
    it (bench: 1 MiB pushed costs 1,032 KB, 2.5 MiB costs 2,568 KB, 4 MiB
    costs 4,105 KB).
  - **Memory glibc keeps.** Snapshots and checkpoints (`encode_snapshot_alloc`
    plus zstd, every 2 MiB of output and after 5s idle) allocate and free
    large buffers on each pane's thread. glibc's dynamic mmap threshold then
    puts later large buffers on that thread's arena heap, and it doesn't
    give that memory back. Fixing the threshold cuts the excess to about
    0.8 MB at 10k lines and about 20 MB at the cap (ring plus remaining
    fragmentation).
- **What the programs cost.** nvim is 4.9 MB PSS per pane (two processes).
  `sleep` is 1.3 MB PSS. bash with 10k lines printed is unchanged at
  0.75 MB.

### 5. Attached clients

Each client attaches to every pane, with a fresh snapshot each. Daemon RSS
growth:

| panes | first client | each further client | after all clients leave |
|---|---|---|---|
| 50 × full16 | +5.7 MB | +0.19 MB (2→20 clients) | not given back |
| 50 × full truecolor | +28.8 MB | | not given back |
| 20 × nvim | +2.1 MB | | not given back |
| 50 × 10k lines | +30.7 MB | +2.9 MB (2→5) | not given back |
| 20 × 10k lines, malloc tunables | +2.1 MB | +0.9 MB (2→5) | given back |
| 10 × 200k lines | +5.0 MB | | not given back |

- **A connected client is cheap at steady state.** Its queue is empty once
  it's drained, about 0.2 MB.
- **The cost is transient.** A snapshot's VT bytes for each pane are built
  and queued while it attaches, and glibc keeps the high-water mark after.
  The tunables make most of that come back.
- **A stalled client** (4 panes, `seq 1 3000000` in each; the client never
  reads after attaching) costs **+28 MB over the same burst with no client
  attached**. That is the 1024-frame `CLIENT_QUEUE` plus axum's write
  buffer, until the resync. The memory stayed after the client left.
  - The bound is CLIENT_QUEUE × frame size per client, and a frame is up to
    one 64 KiB PTY read, so the worst case is about 64 MB per stalled client.
  - The first version of this scenario reported 560 MB with no control run.
    That was the panes' scrollback, not the client.

### 6. After panes close

| scenario | before closing | after closing all but 1 | with malloc tunables |
|---|---|---|---|
| 500 idle | 1,614 MB | 191 MB | 37 MB (from 50 panes) |
| 50 × full16 | 331 MB | 101 MB | |
| 50 × 10k lines | 1,819 MB | **794 MB** | 61 MB (from 20 panes) |
| 10 × 200k lines | 1,644 MB | **1,022 MB** | 153 MB (from 5 panes) |

- **libghostty's own pages are mmapped and go back**; glibc keeps the
  rest. Without a fixed mmap threshold, a daemon that once had busy panes
  keeps about half of its peak forever.
- That memory is reused (reopening reaches the same peak), so it is a
  ceiling, not a leak.

## Verdict against M9's trigger

- **Is RSS per idle pane above 2 MB?** Yes: 3.1 to 3.7 MB in the daemon,
  plus 1.7 MB of shim and bash outside it. The trigger holds.
- **What would 500 idle shells cost today?** About 1.6 GB of daemon, plus
  0.5 GB of shims and 0.36 GB of bash, so 2.5 GB. That's while they're
  empty.
  - If each has 10k lines of history: about 16 GB of daemon.
  - If each has reached the 64 MiB cap: about 80 GB.
- **But most of the idle cost isn't state that parking frees.** It's TLS
  per thread and a debug-style memset. With both fixed, 500 idle panes are
  246 MB of daemon (0.48 MB each), which already meets M9's "done when"
  (under 1 MB each) with no parking at all.
- **Parking earns its keep on panes with history**, and those cost 20 to
  160 MB each. But a smaller cap plus fixed malloc thresholds get most of
  the way there:
  - 10k lines at 200 cols becomes about 20 MB;
  - a 16 MiB cap bounds the worst case near 30 MB.
- **Recommendation:**
  1. Do the cheap wins below first.
  2. Rerun this benchmark.
  3. Start M9 only if a fleet of panes with real history still lands above
     the budget.

  Terminal parking (free the engine after 60s idle) would then take a
  scrolled pane from about 20 MB to about 0. Nothing here argues for "PTY
  parking" (the threads are cheap once TLS is fixed) or for "client buffer
  parking" (idle clients hold about 0.2 MB).
- **Parking can never save the processes:** 0.72 MB for each bash and
  1.02 MB for each shim (0.77 MB with the TLS fix). At 500 panes that's
  0.75 to 0.87 GB, roughly as much as everything the daemon holds after the
  fixes.

## Cheapest wins (no parking)

In order of payoff for idle panes, then busy ones:

1. **Drop Zig's 256 KiB threadlocal signal stack.** This saves 1.2 to
   1.3 MB per pane, 256 KB per shim, and 8 MB per daemon.
   - It's a one-line change in libghostty's `std_options`
     (`ghostty-no-signal-stack.patch`): upstream it to Ghostty or
     libghostty-rs, or carry it as a `GHOSTTY_SOURCE_DIR` patch.
   - Nothing in our process uses that stack: Rust installs its own
     sigaltstack.
2. **Stop paying for ReleaseSafe's page fill.** This saves about 1.5 MB per
   screen (3 MB with the alt screen).
   - Building libghostty ReleaseFast does it, but loses Zig's safety checks
     on untrusted program output. `.cargo/config.toml` chose ReleaseSafe on
     purpose, so this is a decision for Jake, not a free win.
   - Alternatives:
     - an upstream change so pool pages aren't filled with `undefined`
       (0xAA) and then memset in safe builds;
     - preheat fewer pages;
     - ReleaseSafe in tests and fuzzing, ReleaseFast in the shipped daemon.
3. **Fix the malloc mmap threshold.** This saves about 13 MB per pane at
   10k lines and about 65 MB at the cap, and makes closed panes and departed
   clients give memory back.
   - Call `mallopt(M_MMAP_THRESHOLD, 128 KiB)` and `M_TRIM_THRESHOLD` at
     startup (the tunables measured here do the same), or switch to
     jemalloc or mimalloc. Either way, call `malloc_trim(0)` after a pane
     closes. That trim wasn't measured: `ptrace_scope=1` stops gdb from
     calling into the daemon.
4. **Shrink the ring to what replay can use.** This saves up to 3 MB per
   busy pane.
   - Replay never sends more than `MAX_REPLAY_BYTES` (1 MiB), but
     `RING_BYTES` keeps 2 MiB, and extend-then-drain grows the `VecDeque`
     to 4 MiB.
   - Set `RING_BYTES = MAX_REPLAY_BYTES`, and drain before extending (or
     preallocate exactly).
5. **Lower `SCROLLBACK_BYTES` from 64 MiB to 16 MiB.** This caps a pane
   near 28 MB instead of 77+ MB (about 9.6k rows at 200 cols, about 24k at
   80). The full history is on disk anyway (`history`, `tail`, `search`).
6. **Fewer threads per pane.** Merge the reaper into the wait thread (it
   waits on the shim, which exits with the program), and later move PTY
   read and write onto one epoll task (M9's "PTY parking"). With the TLS
   fix a thread costs about 24 KB, so this is tidiness more than memory.
7. **A smaller shim.** It is the whole 21 MB illogicald re-executed: 1 MB
   USS, and 0.77 MB after the TLS fix. A tiny static `illogical-shim`
   binary, or `exec` straight after recording the pid, would bring it to
   about 100 to 200 KB, saving about 0.3 GB at 500 panes.

## The benchmark

- **`bench.py`** is the end-to-end benchmark. It needs `uv` for the
  `websockets` package. It starts its own daemon for each scenario, measures
  and kills it.

  ```
  uv run --with websockets python3 spikes/s9-memory/bench.py \
      --bin spikes/s9-memory/work/illogicald --port 7741 \
      [--scenarios idle,full16,full,nvim,sb10k,sb200k,stalled] \
      [--counts 10,50,200,500] [--env GLIBC_TUNABLES=...] [--tag name]
  ```

  - Scenarios: `idle` (N empty panes, close, reopen); `full16`, `full` and
    `nvim` (full screens); `sb10k` and `sb200k` (scrollback); `stalled` (a
    client that stops reading, against a control).
  - The content scenarios also attach 1, 5 and 20 clients and then close
    the panes.
  - Fixtures (`screen.ans`, `screen16.ans`, `lines10000.ans`,
    `lines200000.ans`) are generated in `work/` with a fixed seed.
  - Output is a line per step on stdout, and
    `work/results-<scenario>[-tag].json` for each scenario.
  - The defaults take about 25 minutes and peak near 2 GB.
- **`engine/`** is the libghostty-only microbench (§3), with its own
  workspace. Build instructions are at the top of `src/main.rs`.
- **`smaps_breakdown.py PID`** groups a process's mappings by kind and size.
  **`hold.py N [cols rows]`** leaves a daemon with N panes running for
  inspection.
- **`cleanup.py`** kills any daemon, shim or shell this spike left behind.
- **For M9's CI test**, the `idle` scenario at N=50 is the one to keep: one
  number (daemon RSS per idle pane), 3.07 to 3.11 MB across four runs
  here (one ad hoc `hold.py` daemon read 3.9 MB, so leave headroom), and
  done in under a minute. Assert it stays under 1 MB once wins 1 and 2 land. A
  `sb10k`-style test with N=10 would guard the retention fixes.

## Still open

- **ReleaseSafe or ReleaseFast** for the shipped libghostty (win 2) needs a
  decision. An upstream fix to the safe-mode page fill would avoid having to
  choose.
- **`malloc_trim(0)` after a close** wasn't measured (no ptrace into the
  daemon). jemalloc and mimalloc weren't tried.
- **Unparking time** (M9's "draws in under 50 ms") wasn't measured. S5's
  incremental decode (0.31 ms to READY for an 11 MB snapshot) suggests it's
  easy.
- **Real workloads.** Claude Code, long `cargo build` logs and wide panes
  were not covered: only the synthetic coloured log lines, a full screen and
  nvim. Scrollback memory scales with columns, not with text.
- **The stalled-client worst case** (a 64 MiB queue per client) was bounded
  by reasoning. Only one 4-pane burst was measured.
- **The variant binaries were built from `git archive HEAD`, not from the
  exact tree of `work/illogicald`.** The only diff is PLAN.md, and the
  `tls` build's idle shim and bash numbers match.

## Cleanup

- **Every daemon this spike started is stopped**, including all the shims,
  shells, `sleep` and `nvim` children. They ran on 127.0.0.1:7741 to 7745
  and all were killed. Afterwards no process had an `ILLOGICAL_PANE`
  environment whose parent was ours, and none of the ports was listening.
- **The other daemons were never touched:** 7681, 7695, 7696, 7706, 7716
  and the tailnet one.
- **No changes outside `spikes/s9-memory/`:** no cargo runs in the shared
  `target/`, and no commits.
- **Scratch output is in `work/` (git-ignored), about 2.9 GB.** It holds:
  - the copied and variant binaries;
  - `target*/` (own cargo target dirs);
  - `ghostty-patched/`;
  - `src/` (the archive);
  - fixtures, `results-*.json`, `run*.log` and `daemon-*.log`.

  The `state-*` dirs were deleted. `rm -rf spikes/s9-memory/work` removes
  the rest.
