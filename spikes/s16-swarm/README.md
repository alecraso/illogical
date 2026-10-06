# S16: before the swarm (summaries, activity, fleet connections, canvas)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s16-swarm/<file>`.

Run 2026-10-02 on geek (Ryzen AI MAX+ 395, 32 threads, 121 GB), for the
swarm track (#36, cut for the MVP in #44). **Result: go on all three.**

- **Delta summaries: go, and needed.** Today every change sends the whole
  `State`. At 500 panes with 50 busy, that is 34 messages a second of
  195 KB each: **6.6 MB/s to every client**. Field-level deltas, coalesced
  once a second, carry the same changes plus an activity figure per busy
  pane in **4.6 KB/s** (0.44 KB/s with permessage-deflate).
- **Server-side previews: go.** Capturing 50 panes' screens once a second
  costs the daemon 0.8% of a core, 1.1 ms per capture (p50), and the last 6
  lines are about 43 bytes a pane.
- **Canvas 2D: go up to about 2,000 panes.** The prototype draws 2,000 panes
  at 60 fps on the laptop. With 4x CPU throttling at a phone viewport it
  manages 500 at 60 fps and 2,000 at 36 fps. Half of each frame is the
  physics, which can sleep once clusters settle. WebGL isn't needed for the
  MVP.
- **Fleet connections: go, with one change for control mode.** Holding
  plain `/ws` connections to 20 daemons (600 panes) costs a page about
  14 MB. But Chrome
  spaces out WebSocket connections to one address once more than about 8 are
  opening. That added 2.2–4.6 s to a wake with 20 daemons on 127.0.0.1.
  Through control's relay every daemon is the same host, so the page should
  carry all its daemon channels over **one** relay socket.

## Setup

- **Binary.** `target/release/illogicald` built in this worktree from
  `216b864` (main after M21), with `web/dist` built first, as `just build`
  does (`LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseSafe` from `.cargo/config.toml`).
- **Daemons** are started by S9's `bench.Daemon`: `--shell "bash --norc
  --noprofile" --no-manager-env`, a state dir under `work/`, no wisp token,
  no systemd. Panes are opened with `POST /api/run`, in tabs of four (a tab
  and three splits) over five sessions.
- **Ports.** Other sessions' e2e tests on geek use ports in 7750–7789 (the
  control tests take 7750–7753, the guests test 7773). `summary.py` runs
  used 7752–7754 and 7795. An earlier run on 7751 found another session's
  test server there and only read `GET /api/panes` from it. `fleet.py` picks
  free ports at start and moves on if one is taken meanwhile.
- **Busy panes** run a build-like command (20 lines over about 2 s) every
  2.5 s plus an exponential wait averaging 5 s, about 7 commands a second
  across 50 panes. Every command start and end is a `State` broadcast today,
  and so is every attention change.
- **Browser:** the system Chrome, headless, through web/'s Playwright, as
  the e2e tests run it.

## Results

### 1. Summary cost (`summary.py`)

One observer on `/ws`, recording every `State` for 60 s. The delta columns
come from the same recording: each `State` is diffed against the one before
it, field by field per pane (tabs whole when they change), coalesced over the
window. Each pane that printed in the window also gets a modelled
`activity: {bps, last_ms}`, as M23 would send. "Deflate" uses one compression
context per connection, as permessage-deflate does.

| panes | busy | observers | `State`/s | `State` size | today | today, deflate | deltas 0.5 s | deltas 1 s | 1 s, deflate | daemon CPU |
|---|---|---|---|---|---|---|---|---|---|---|
| 100 | 10 | 1 | 7.5 | 40.6 KB | 304 KB/s | 20 KB/s | 1.2 KB/s | 1.0 KB/s | 0.11 KB/s | 1.1% |
| 500 | 0 | 1 | 0 | 209 KB (hello) | 0 | 0 | 0 | 0 | 0 | 1.2% |
| 500 | 50 | 1 | 34.0 | 195 KB | **6.6 MB/s** | 390 KB/s | 5.6 KB/s | **4.6 KB/s** | 0.44 KB/s | **14.7%** |
| 500 | 50 | 5 | 33.3 | 195 KB | 6.5 MB/s each | 383 KB/s | 5.5 KB/s | 4.6 KB/s | 0.43 KB/s | 20.4% |

Idle CPU (500 panes, no clients, no load) is 1.1–1.4% of a core. Daemon RSS
at 500 panes is 1.7–1.8 GB, as S9 found.

**Where a 500-pane `State` goes** (`breakdown.py`, 505 panes, 131 tabs):

- panes 107 KB (about 210 B each at a prompt, about 400 B with `current`
  and `last` filled in), tabs 77 KB (585 B each: the tree and its cell
  rectangles), sessions 0.6 KB;
- the biggest pane fields are `epoch` (28 B), `policy` (26), `attention`
  (19), `cwd` (19) and `integration` (19). A swarm tile needs few of them.

**What this means:**

- **Today's protocol doesn't scale past a few hundred panes, even for the
  existing UI.** Every client of a 500-pane daemon with 10% busy gets
  6.6 MB/s, and the daemon spends 15–20% of a core building and serializing
  it. Over control's relay that is a phone's whole connection. This isn't
  only the swarm's problem: M23's delta subscription should become the
  normal path for the tab view too.
- **Deltas at 1 Hz cost about 1,400 times less** (4.6 KB/s against
  6.6 MB/s) and stay under a phone's budget even without deflate. 0.5 s
  batching costs about 20% more than 1 s; either is fine.
- **#37's target holds with room:** "under 20 KB/s while 10% of panes are
  busy" is met at 4.6 KB/s. "Under 5% of one core" needs the full-`State`
  broadcast gone: today it's 14.7%.

### 2. Activity signal (code reading, no build)

- **Where:** `pane.rs` `State::output`, which already runs for every PTY
  chunk and already locks the pane's shared `Status` to set `end`, `modes`
  and `busy`.
- **What to add:** two fields in `Status`, a cumulative `out_bytes: u64` and
  `last_output_ms: u64`, set in that same critical section. That's one add
  and one clock read per chunk. Keep the EMA out of the hot path: the mux
  reads `out_bytes` when it builds a summary (1–2 Hz) and works out bytes per
  second from the difference. Then an idle pane needs no timer to decay, and
  reading it costs one mutex lock per pane per tick (500 a second, which is
  nothing).
- **Parking (M9, not built yet):**
  - **Terminal parking** frees the VT engine, not `Status`, so counters
    survive it. Reading them never touches the pane's thread, so a summary
    doesn't unpark anything.
  - **PTY parking** (reads moved to one shared epoll task) must keep calling
    the same counter update. That's a one-line requirement for M9, worth
    writing into its ticket.
- **One more existing cost:** the 5 s `REFRESH` reads `/proc` for every
  running pane's cwd and foreground command. At 500 panes that's inside the
  1.1–1.4% idle figure above.

### 3. Fleet connections (`fleet.py`, `fleet.mjs`, `fleet.html`)

One headless Chrome page holds a plain `/ws` to each of K daemons (25 panes
each, plus the starting pane), with the real client's reconnect policy (250 ms
doubling to 5 s). Its origin is allowed with `--allow-origin`. A "sleep"
closes every socket. A "wake" reconnects them all at once, as the client's
`wake()` does after a phone or laptop resumes. Each figure is from one run;
wake times are three cycles.

| daemons | panes | renderer RSS before → after | JS heap after | first connect | wake: all hellos back | hello bytes per wake |
|---|---|---|---|---|---|---|
| 5 | 150 | 318 → 328 MB | 1.2 MB | 15 ms | 5 ms | 56 KB |
| 8 | 240 | 316 → 327 MB | 1.2 MB | 37 ms | 10–21 ms | 89 KB |
| 12 | 360 | 317 → 328 MB | 1.2 MB | 217 ms | 93–133 ms | 134 KB |
| 20 | 600 | 316 → **330 MB** | **1.3 MB** | 4.2 s | **2.2–4.6 s** | 223 KB |

The JS heap is 0.6 MB before connecting, in every run.

- **Memory is not the issue:** 20 connections and 600 panes' states add about
  14 MB to the renderer (10 MB of it is there at 5) and 0.7 MB of JS heap.
- **Connection spacing is.** At 12 daemons, the first six or seven hellos
  arrive in 2–4 ms, then later ones at 16, 24, 40, 77 and 109 ms. At 20
  daemons the wake takes seconds. No connection failed. The daemons are idle
  throughout (0.61 CPU-seconds across all 20 over the 29 s run). The waiting
  is in Chrome, which throttles WebSocket handshakes to one address once
  several are open or opening.
  - **Untested:** daemons on different addresses, as on a tailnet. Every
    daemon here is 127.0.0.1.
- **Control mode** (Noise over the relay, S15's numbers):
  - each channel adds a 96+48-byte handshake and about 0.1% to bulk bytes;
  - every relayed hello adds one relay↔daemon round trip (18.7 ms through
    ewr from geek);
  - one control process handled 1,000 channels.

  The catch is the spacing above: through the relay, every daemon is the
  same host. **#39 should carry all of a page's relayed daemon channels over
  one WebSocket to control** (M4c's mux already multiplexes dial-out),
  rather than one socket per daemon.

### 4. Canvas 2D rendering (`canvas.html`, `canvas.mjs`)

`canvas.html` is the prototype ("Pane Swarm"), instrumented to time each
frame's physics (`step`) and drawing (`draw`). It's measured at the fitted
view, the most panes on screen, 6 s after load for 10 s. "Phone" is a
390×844 viewport at DPR 3 with `Emulation.setCPUThrottlingRate` 4, a proxy
until a real phone run. The times are main-thread JS. Headless Chrome may
rasterize on the CPU, which this doesn't capture.

| profile | panes | fps | physics p50 | draw p50 | frame work p50 | p95 |
|---|---|---|---|---|---|---|
| laptop | 500 | 60 | 0.4 ms | 0.7 ms | 1.1 ms | 1.5 ms |
| laptop | 2,000 | 60 | 2.2 ms | 2.1 ms | 4.3 ms | 5.3 ms |
| laptop | 5,000 | 51 | 7.7 ms | 5.0 ms | 12.9 ms | 15.2 ms |
| phone (4x) | 500 | 60 | 1.5 ms | 2.9 ms | 4.4 ms | 6.0 ms |
| phone (4x) | 2,000 | 36 | 8.6 ms | 9.1 ms | 18.0 ms | 22.0 ms |
| phone (4x) | 5,000 | 19 | 27.8 ms | 16.7 ms | 44.8 ms | 52.3 ms |

- #40's bar (500 panes at 60 fps on the laptop, 30 on the phone) is met at
  500 with plenty of room, and at 2,000 on both.
- **Physics is half the frame.** Letting settled panes sleep (skip the
  spring and repulsion while a pane's velocity is near zero and its cluster
  hasn't moved) would take the phone case at 2,000 to about 60 fps.
- WebGL only matters past about 5,000 panes, which isn't the MVP.

### 5. After the MVP: previews and classification

**Previews (`previews.py`).** A 500-pane daemon with 50 busy (as above),
plus `GET /api/panes/{id}/capture?format=text` for 50 visible panes (25 busy,
25 idle) once a second for 30 s:

| | daemon CPU |
|---|---|
| busy, no captures | 1.9% |
| busy, 50 captures a second | 2.7% |

- **Capture latency:** 1.11 ms p50, 2.16 ms p95, from 1,500 captures.
- **Size:** a whole screen is 191 B on average (mostly blank 80×24 here),
  and its last 6 lines are 43 B. That's 2.2 KB/s for 50 visible panes.
- **Go for server-side previews.** They are cheap to render and tiny to
  send, much less than streaming busy panes' output to a zoomed-out view.
  Rendering on the client would mean attaching and receiving every byte.
- No WebSocket client was connected here, so no `State` was built or sent.
  That's why the busy figure (1.9%) is so far below section 1's 14.7%: most
  of that is building and serializing `State` for a client.

**Classification (`classify.py`).** A first cut of M23's `kind` from a
command line (look through wrappers like `uv run`, `npx` and `sudo`; agents,
editors, logs, servers, tests, builds, else shell) and `project` (the nearest
`.git` up from the cwd):

- **On a labelled fixture of 54 commands: 52 right.** The two misses show
  the limits:
  - `c`, the user's alias for `claude`: the typed text hides it. **Classify
    from the foreground process's argv (`procinfo::argv`) first, and the
    shell integration's text second.**
  - `./scripts/release.sh`: a script's name says little.
- **On real history** (116 commands from geek and jake-mini, read-only;
  `work/`, not committed): nearly everything is `shell` (ad-hoc `ls`,
  `hostname` and test commands), with one `vim`. The live daemons hold 7 and
  a few panes. That's too little to measure accuracy on real work, so this
  stays pending.
- **Project coverage:** only 8 of 105 geek commands ran inside a git repo.
  The rest ran in `/tmp`, `~` or a VM's home. **#40 needs a fallback group
  for panes with no project**, by machine or directory, or "by project"
  becomes one giant "none" cluster.

## What changes in the tickets

- **#37 (M23):**
  - make the delta subscription the normal path, not only the swarm's, since
    today's whole-`State` broadcasts are the CPU and bandwidth problem at 500
    panes;
  - send activity as a cumulative byte counter that the mux turns into
    bytes per second at summary time;
  - classify `kind` from `/proc` argv before typed text;
  - keep summary pane objects small (no `epoch`, `policy` or `integration`
    per tick).
- **#39 (M25):**
  - carry relayed daemon channels over one WebSocket to control;
  - stagger direct (tailnet) reconnects anyway;
  - a 20-connection cap is fine for memory (about 14 MB a page).
- **#40 (M26):**
  - Canvas 2D, with sleeping physics for settled panes;
  - a fallback group for panes with no git project;
  - server-side previews (from #37) are cheap enough to run at 1 Hz for
    everything readable on screen.
- **M9 (#10):** PTY parking must keep updating the pane's output counters.

## Pending

- A real phone run of `canvas.html` (iOS Safari and Android Chrome), in
  place of 4x throttling.
- Fleet connections to daemons on different addresses (a real tailnet) and
  through a local control's relay (`just control-smoke`'s setup).
- Classification accuracy on a week of real panes (builds, tests, servers
  and agents), once there are enough of them.

## Files

- `summary.py`: the `State` recording and the delta model (section 1).
- `breakdown.py`: where a 500-pane `State`'s bytes go.
- `fleet.py`, `fleet.mjs`, `fleet.html`: one page, many daemons (section 3).
- `canvas.html`, `canvas.mjs`: the prototype renderer and its frame timing
  (section 4).
- `previews.py`: capture cost (section 5).
- `classify.py`: the `kind`/`project` heuristic and its fixture (section 5).
- `work/` (not committed): JSON results, daemon logs and state dirs, and the
  history samples.
