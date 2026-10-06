# M3b follow-up spike: awake, slow readers, kill, cold, replay
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/m3b-followup/<file>`.

Run 2026-10-01 on geek against local wisp. It answers the "Still open" list of
[the M3b spike](../m3b-machines/README.md), apart from Fly and a wispd
restart. **Results in short:**

- the open exec connection is what keeps a sprite awake, not pings;
- a reader that stalls under 30s gets backpressure with no loss, and one that
  stalls longer is dropped;
- `kill?signal=HUP` closes a shell in about 1ms;
- `output_offset` makes replay exact;
- a cold sprite has rebooted: sessions gone, disk kept, 404 on reattach.

## Setup

- wispd as in the M3b spike: `127.0.0.1:7788`, `--idle-timeout 30s`,
  `--warm-ttl 1h`. It runs the build of `~/dev/jhgaylor/mini-sprites` from
  2026-09-25 (HEAD `9e45d58`). The source was read to explain the results;
  every claim below was also measured.
- Guest: Ubuntu 24.04, user `sprite`. The guest agent shows as `exe` in `ps`.
- `m3bf.ts` reuses the helpers of `spikes/m3b-machines/m3b.ts`. The WebSocket
  client is raw, over `node:net`, so that it controls pings, pongs and reading
  (`socket.pause()`) and sees every control frame. It refuses any sprite name
  not starting with `illogical-m3bf-`. Bun 1.4.2.

```sh
SPRITE_TOKEN="$(cat ~/.local/share/wisp/token)" bun m3bf.ts <cmd> [args]
# awake NAME idle|nopong|ping10|paused|detached SECS | silent NAME sleep|cpu
# slow NAME PAUSE_S [N] | kill NAME | replay NAME
# capture NAME LABEL CMD... | align LABEL... | coldsetup|coldwatch|coldwake NAME
```

Logs are in `work/` (git-ignored).

## 1. What keeps a sprite awake

**The open exec connection itself. Pings play no part.** Each mode had a
`bash -l` at its prompt on a fresh sprite, with the status polled every 1s
for 75s (`awake`):

| mode | control frames seen | status |
|---|---|---|
| attached, no pings either way | none at all | `running` throughout |
| attached, client never answers pings (`nopong`) | none, since wisp never pings | `running` throughout |
| attached, client pings every 10s | 7 pings, each ponged within 1ms | `running` throughout |
| attached, client stops reading (`socket.pause()`) | none | `running` throughout |
| detached (control) | — | `warm` at 32s |
| attached, no pings, 300s | none | `running` for 300s |

- wisp sends no WebSocket pings on an exec connection. Neither wispd nor the
  guest agent sets a keepalive. The TCP connection sat byte-for-byte idle for
  300s and the sprite stayed up.
- The source gives two reasons, and either one alone keeps it awake:
  - wispd *pins* the sprite for the life of any `/exec` request, WebSocket
    included (`proxyAgent`, `pin=true`). Only `/ports/watch` and `/control`
    sockets are unpinned.
  - The idle check asks the guest agent for `attached_sessions`. Any live
    session with a client attached counts, and so does an exec op on a
    `/control` channel. The sprite suspends only when that is 0 and nothing
    happened for `--idle-timeout`.
- So no client trick (no pings, paused reads, the control channel) lets a
  sprite pause while a client is attached.
- Correction to the M3b README: in `GET /exec`, `is_active` means "output or
  input in the last 5s", not "a client is attached". Every attached session
  above showed `is_active:false`.

**Silent work in a detached session gets frozen** (`silent`). A detached
shell ran `sleep 60` in one test and a CPU-bound loop (`timeout 60 sh -c
'while :; do :; done'`) in another. Both sprites went `warm` at 33s, in the
middle of the work.

- On reattach (about 100s after the detach) the work resumed. It finished
  about 29s later, so the guest had run about 33s before the pause and the
  remaining 27s after it.
- The shell's `date` arithmetic reported 129–130s, because the guest's wall
  clock is stepped on resume.
- What counts as activity: exec input and output, API calls, and Tasks API
  holds. CPU use doesn't count, and neither do child processes.

## 2. Slow reader

`seq 1 5000000` (43.9MB through the TTY) in an interactive shell. The client
stopped reading 300ms in, held off for 20s or 60s, and then read to the end
(`slow`). Every 4s a second exec checked `seq` in the guest, and the host
checked `ps -o rss` of wispd.

**Paused 20s: backpressure, no loss.**

- The client had 7.2MB when it paused. Within 4s `seq` was blocked writing to
  the PTY:
  - its state was `S+` with `wchan=wait_woken`;
  - `wchar` stayed at 15,375,712 for the rest of the pause.
- So about 8MB was in flight. That is the agent's per-client queue, which
  stops reading the PTY above 4 MiB (`clientHighWater`), plus the PTY, the
  vsock and TCP buffers.
- After resuming, the rest arrived in 0.9s. All 5,000,000 lines arrived in
  order, with none missing or duplicated. (The harness counts one line as
  missing because the `1` follows the echoed command without a newline.)

**Paused 60s: wisp drops the client at 30s, and output is lost.**

- `seq` was blocked as before. Between 28s and 32s into the pause it
  disappeared, having finished.
- The guest agent sets a 30s write deadline on every frame it sends. When one
  write blocks for 30s, the agent gives up on the client and detaches it.
  With no client attached, nothing throttles, and `seq` ran to the end into
  the 1 MiB ring.
- The client saw nothing until it read again. Then it got the buffered 6.3MB
  (14,873,050 bytes in total) and a close with **1006** (TCP closed, no close
  frame).
- Reattaching with `output_offset=14873050` returned 1,048,566 bytes, the
  ring, and the end marker.
- In total, 1,892,494 of the 5,000,000 lines arrived; lines 1,776,001 to
  about 4,869,000 were lost. **Nothing marks the gap.** `session_info` is the
  same, and the replay starts at the start of the ring.

**wispd memory** stayed within 70.7–94.9MB (RSS) across both runs, the same
range as with no test running (81–91MB, with other users on the host). It
doesn't buffer per connection; the reverse proxy streams. The guest agent
went from 19.7MB to 23.2MB while blocked, and back to 18.7MB after.

`yes | head -c 200M` was not run; `seq` had already shown blocking, the
deadline and loss.

## 3. Kill with a chosen signal

**`POST /v1/sprites/{name}/exec/{id}/kill?signal=…&timeout=…` takes the
signal as a query parameter.** The signal goes to the session's whole
process group. Each case was on a fresh interactive `bash -l` (`kill`). The
reply is NDJSON; `time` fields are left out here.

| request | HTTP | NDJSON | exit frame | time to WS close |
|---|---|---|---|---|
| no params | 200 | `sent SIGTERM`, `timeout`, `killed`, `complete 137` | 137 | 10,008ms |
| `?signal=SIGHUP` / `HUP` / `hup` / `1` | 200 | `sent SIGHUP`, `exited`, `complete 129` | **129** | **1–6ms** |
| `?signal=9` / `SIGKILL` | 200 | `sent SIGKILL`, `exited`, `complete 137` | 137 | 1–6ms |
| `?signal=INT` | 200 | ignored by bash, SIGKILL after 10s | 137 | 10,002ms |
| JSON body `{"signal":"SIGHUP"}` | 200 | **body ignored**: SIGTERM, then SIGKILL after 10s | 137 | 10,003ms |
| `?signal=HUP&timeout=0` | 200 | `sent SIGHUP`, `complete` (returns at once, doesn't wait) | 129 | 6ms |
| `?signal=TERM&timeout=1s` | 200 | SIGKILL after 1s | 137 | 1,002ms |
| `?signal=BOGUS` | **400** `unknown signal "SIGBOGUS"` | — | (none; still running) | — |
| HUP with `sleep 1001` in the foreground | 200 | exited | 129 | 2ms; the `sleep` died too |
| HUP with `sleep 1002 &` and `nohup sleep 1003 &` | 200 | exited | 129 | 2ms; the job died, the **nohup one survived** |
| HUP after `trap '' HUP` in the shell | 200 | SIGKILL after `timeout=3s` | 137 | 3,003ms |
| WS text frame `{"type":"signal","signal":"HUP"}` | — | — | 129 | 5ms |
| Ctrl-D (`\x04`) on input at the prompt | — | — | 0 | 41ms |

- Signal names work with or without `SIG`, in any case, or as numbers.
- `timeout` is a Go duration; the default is 10s and `0` means "send and
  return".
- `kill` on a session that already exited returns 410 (from the source; not
  hit here).
- The agent advertises `X-Sprite-Capabilities: signal` on the exec upgrade,
  and a `{"type":"signal"}` text frame on the attached WebSocket does the same
  thing without a second HTTP call.
- SIGHUP to an interactive bash does what closing a terminal does:
  - bash forwards HUP to its jobs and exits 129, taking foreground and
    background jobs with it;
  - `nohup`/`setsid` processes and HUP-ignoring programs survive. Those are
    children of the exec's process group only if they didn't move out of it.
- The daemon's `Wisp::kill` already sends `?signal=HUP&timeout=3s`. That is
  right.

## 4. The cold reboot after the warm period

**A cold sprite has rebooted. The exec sessions and processes are gone, the
disk stays, and the first request boots it in about a third of a second.**
(`coldsetup`, `coldwatch`, `coldwake`)

Steps:

- **13:12:18.** Create `illogical-m3bf-cold`, then exec a `bash -l` with
  `max_run_after_disconnect=6h` (session `1`). It:
  - wrote the time to `/tmp/cold-mark` and `~/cold-mark`;
  - started `(sleep 86400 &)` and a `nohup sleep 86401 &`;
  - printed `boot_id` `ea94ec05…` and `uptime -s` 13:12:18.

  Then detach.
- After that, only `GET /v1/sprites/{name}`, every 3 minutes.

Status and timings:

| time (UTC) | status | |
|---|---|---|
| 13:12:18 | `running` | create and first exec |
| 13:12:50.7 | `warm` | `last_warming_at` (32s after the detach) |
| 14:12:22 | `warm` | 59.5 min after warming |
| 14:15:22 | **`cold`** | first poll seen cold, so the change came 60–62.5 min after warming |
| 14:15:39.0 | `cold` → `running` | `running` within 100ms of the first request |

- The GET body has no field for the cold transition; `last_warming_at`
  stays. Only `status` changes. wispd's janitor drops the memory snapshot
  once `warm-ttl` has passed and logs a `sprite.cold` event (from the
  source).
- **The first request after cold:** reattaching the old session id
  (`/exec/1?output_offset=0`).
  - The WebSocket upgrade was answered **404 `{"error":"not_found",
    "message":"exec session not found"}` after 334ms**, the cold boot
    included (S4/M3b: about 250–330ms to boot).
  - There is no `session_info` and no `exit` frame. That is unlike a session
    killed by `max_run_after_disconnect`, which still answers with
    `session_info` + `exit 137` for 30s.
- The next exec took 52ms (sprite already running), and showed:
  - a new `boot_id` (`2b6ca4ee…`) and `uptime -s` 14:15:39, so this was a
    real reboot;
  - `/tmp/cold-mark` gone (tmpfs), and `~/cold-mark` still there (disk
    kept);
  - no `sleep` processes, even with `nohup`;
  - `GET /exec` returned `{"sessions":[]}`.
- So a detached session's `max_run_after_disconnect` (6h here) doesn't help.
  The VM reboots at about warm + 1h, and everything in memory goes, sessions
  included. Reattach and replay are impossible; the ring was in the agent's
  memory.

What a VM pane sees after a long idle, by how long the sprite has been
detached and idle:

- **Under 30s:** reattach, exact replay.
- **30s to about 1h (`warm`):** reattach in about 33ms, exact replay of
  anything under 1 MiB.
- **Over about 1h (`cold`):** reattach is refused with a 404. The disk
  survives; processes and sessions don't.

While the daemon stays attached none of this happens, because the sprite
never pauses (1).

## 5. Replay alignment, re-check

**The M3b README's finding holds only without `output_offset`. wisp has an
`output_offset` attach parameter that makes replay exact.** The daemon
already uses it (`attach_url` in `crates/daemon/src/machine.rs`). The M3b
spike tried `since` and `offset` but never this name. (`replay`)

- **No offset:** the whole ring is replayed, with no end marker.
  - Small session: 310 bytes live, then 310 identical bytes on reattach.
  - After 3,889,574 bytes: 1,048,188 bytes, exactly the last bytes of the
    stream (its start was at byte 2,841,386 = total − 1,048,188).
  - The ring keeps whole read chunks (up to 32 KiB each) while it holds over
    1 MiB, so it starts at a chunk boundary, mid-line (`\r\n369014…`).
- **`output_offset=N`:** the replay starts exactly at byte N of the session's
  output, cut inside a chunk if needed.
  - `N` = all received: 0 bytes.
  - `N` = all − 10: exactly those 10 bytes.
  - Big session with output while detached (`sleep 2; seq 400001 500000`
    after the detach), `N` = bytes received live: 800,114 bytes, lines
    400,001–500,000 complete, then the end marker. No duplicates and no gap.
- **`output_offset` older than the ring** (`N`=1000 after 3.9MB): the whole
  ring, 1,048,188 bytes, **with nothing to say bytes were skipped**.
  `session_info` carries no total or ring start. Output after the gap can be
  told apart from a clean continuation only by its content.

**Tail matching, the plan's fallback, measured anyway.** It is what a provider
without `output_offset` (Fly?) would need, and it is the way to detect a gap.
For six captured TTY streams (`capture`, then `align`), the test took 200
random cut points D inside the last 1 MiB. For each, it checked whether the L
bytes before D occur exactly once in that 1 MiB:

| stream | bytes | L=8 | 16 | 32 | 64 | 128 | 256 | 512 | 1024 | 4096 |
|---|---|---|---|---|---|---|---|---|---|---|
| `seq 1 400000` | 3.1M | 100% | 100% | 100% | 100% | 100% | 100% | 100% | 100% | 100% |
| test log (`test suite::module_N::case_M ... ok`, 60k lines) | 2.4M | 0% | 13% | 49% | 100% | 100% | 100% | 100% | 100% | 100% |
| progress bars (`\r` redraws, 300 files × 51 steps) | 1.1M | 0% | 7% | 36% | 80% | 100% | 100% | 100% | 100% | 100% |
| `ls -lR /usr` | 0.9M | 12% | 31% | 72% | 94% | 91% | 98% | 97% | 98% | 100% |
| watch-mode rebuild (same 42-line block × 400) | 0.96M | 0% | 0% | 0% | 0% | 0% | 0% | 0% | 0% | 0% |
| `yes` | 4.5M | 0% | 0% | 0% | 0% | 0% | 0% | 0% | 0% | 0% |

- No unique match was ever in the wrong place.
- Content with numbers or paths in it is unique from 64–256 bytes.
- Repetitive output (a rebuild loop, `yes`, a log that repeats an identical
  block) never is, at any length. Its period is longer than the match, and
  any length up to the period finds every repeat.
- So tail matching alone is unreliable, but it fails safe: "not unique" means
  "can't align".

## Consequences for M3b/M3c

1. **VM tabs can't stay attached and still pause.** It isn't pings, so there
   is nothing to turn off. wispd pins the sprite for every open `/exec`
   request, and the guest counts attached sessions. To let a VM tab (or pane)
   pause, the daemon has to **detach** all of the machine's execs, and only
   when it is safe:
   - **Detach only panes whose shell is at its prompt** (no foreground job),
     with no input or output for a while and no viewer.
   - A detached session doing silent work (a build waiting on I/O, `claude`
     thinking, `sleep`) is frozen 30s after its last output. wisp doesn't
     count CPU or child processes as activity. A pane whose program can go
     quiet for 30s must stay attached, or the program must hold a Task
     (wisp's keep-awake API, served in the guest).
   - Check the foreground job with the existing second exec (`ps -o
     tpgid,pgid`), or keep it simple: detach only when the pane's own process
     is the shell and it's the PTY's foreground group.
   - Reattach on input, on a viewer arriving, or on a user action: about 33ms
     from warm (M3b spike). See 4 for cold.
   - Output written while detached and before the pause sits in the ring, and
     `output_offset` recovers it exactly, if it is under 1 MiB (5).
2. **Never stop reading an exec WebSocket for 30s or more.**
   - Up to 30s, wisp applies proper backpressure: the guest program blocks on
     the PTY with no loss, and memory is bounded (about 4 MiB queued in the
     agent; wispd doesn't grow).
   - Past 30s the agent drops the client (1006 when read). The program then
     runs unthrottled into the 1 MiB ring, and anything past that is lost
     with no marker.
   - `machine.rs` already reads the socket in a loop and hands each frame to
     a `sink`. The rule is that the sink (VtEngine, log, fan-out to clients)
     must never block that loop for long. If a downstream client is slow,
     buffer or drop **on the daemon side**, per client. Don't stop reading
     wisp.
   - If the daemon ever wants flow control, a short pause (seconds) is safe.
     It blocks the guest program, which is the right behaviour.
3. **Close a pane fast: `kill?signal=HUP&timeout=3s`** (what `Wisp::kill`
   sends). Or send the `{"type":"signal","signal":"HUP"}` frame on the pane's
   own WebSocket and skip the HTTP call.
   - A shell ends in 1–6ms with exit 129, and its jobs go with it.
   - Only a program that ignores HUP waits for the timeout and SIGKILL.
   - Never rely on a JSON body; it is ignored, and the default is SIGTERM
     with 10s.
   - On a tab-owned machine, `nohup`/`setsid` children of a closed pane keep
     running until the tab's machine is deleted. That is expected (a `tmux`
     in a sandbox should outlive a pane), but `machine idle` should mean "no
     panes", not "no processes".
   - A pane-owned machine still just DELETEs the sprite (about 35ms).
4. **After a cold reboot,** the sprite exists but its sessions don't. Treat a
   404 `exec session not found` on attach, with the sprite still there
   (`GET` 200), as **"machine restarted"**, not "machine gone" and not a
   transient drop:
   - The daemon can't hit this while it stays attached. It hits it when it
     reattaches after more than about an hour detached: a long daemon outage,
     or the detach-idle follow-up.
   - Restart the pane per its restart policy **on the same sprite**, with
     the scrollback kept and a `── machine restarted; processes were lost ──`
     rule. The disk (home) is intact, so `shell` and `rerun` make sense
     there.
   - For a VM tab every pane sees this at once; restart them all on the one
     sprite. Don't create a new one.
   - The first exec pays the cold boot (about 0.33s).
   - Today `machine.rs` retries a refused attach 3 times, 0.5s apart, and then
     reports `Lost { machine_gone: false }`. It should read the 404 body
     (`not_found` / `exec session not found`) and give up at once with a
     distinct "restarted" outcome.
   - If the detach-idle follow-up is built, a detached machine should be
     reattached (or deliberately left to reboot) before warm + 1h. `GET
     /{name}` doesn't wake it and shows `warm` vs `cold`; there is no
     cold-at timestamp.
5. **Replay alignment rule:**
   - Always attach with `output_offset = bytes received`. Don't attach
     without it; that replays the whole ring.
   - **Detect a gap by overlap.** Attach with `output_offset = received − K`,
     with K = 256 bytes (or all of `received` if smaller). Compare the first
     K replayed bytes with the last K the daemon logged:
     - equal: contiguous. Drop those K bytes and carry on.
     - different, or fewer than K bytes came back before live output: the
       ring moved past `received − K`. Write the `── reattached; output while
       detached may be missing ──` rule, and log the replay after it.
   - That works because wisp cuts the replay byte-exactly at the offset. A
     false "contiguous" needs the ring to start with exactly those 256 bytes,
     which only happens with periodic output like `yes`, where the gap hardly
     matters.
   - Today `machine.rs` attaches at `received` exactly and would log a gap
     silently.
   - Without `output_offset` (another provider): match the last 256 bytes of
     the log in the replay, and accept only a single match. Otherwise drop the
     replay and write the rule. Expect that to fail on repetitive output
     (0% for rebuild loops and `yes` at any length).
   - Ask wisp upstream for the ring's start offset (or the session's total)
     in `session_info`. That turns gap detection into one comparison. This is
     smaller than the `since=` ask in PLAN.md, which `output_offset` already
     covers.
6. Small corrections to the M3b README:
   - `is_active` in `GET /exec` means "I/O in the last 5s", not "a client is
     attached".
   - The replay "from the start of the session" finding is only true without
     `output_offset`.

## Still open

- A wispd restart while attached (not run, to avoid disturbing other users).
  Presumably the connection drops with 1006 and `GET` fails or returns 200
  once wispd is back; whether running VMs and their sessions survive a
  wispd restart is untested.
- Fly: whether upstream Sprites supports `output_offset`, `kill?signal=` and
  the signal frame, and whether it pins awake the same way.
- Whether a Task hold from inside the guest (wisp's keep-awake API) is a
  workable way for a detached pane running silent work to keep its sprite
  up. Read in the source, not tested.
- The exact write-deadline behaviour with a reader that is slow rather than
  stopped (a trickle that keeps each write under 30s should never be
  dropped). Only full stops of 20s and 60s were tested.
- `yes | head -c 200M` (not run; `seq` showed the mechanism).

## Cleanup

- Sprites created, all named `illogical-m3bf-*`: `cold`, `aw-idle`,
  `aw-nopong`, `aw-ping10`, `aw-paused`, `aw-detached`, `aw-long`, `slow`,
  `kill`, `replay`, `silent-sleep` and `silent-cpu`.
  - Each was deleted (204), and each returns 404 from `GET
    /v1/sprites/{name}` (checked again at the end for all twelve).
  - No other sprite was listed or touched.
  - `illogicald` instances and wispd were not touched. wispd's RSS was only
    read with `ps`.
- The API token was passed inline at runtime only (`SPRITE_TOKEN="$(cat …)"`),
  and the raw client sends it only in the upgrade request. A `grep -rF` of
  this directory, `work/` included, for the token found nothing.
