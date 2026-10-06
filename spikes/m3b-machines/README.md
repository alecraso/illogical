# M3b spike: ephemeral wisp machines
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/m3b-machines/<file>`.

Run 2026-10-01 on geek against local wisp. **Result: pass. Build M3b on exec
TTY sessions; the proxy + in-guest daemon path is not needed for speed.** Two
things need design work: an attached exec holds the sprite awake, and replay
on reattach resends bytes the daemon already has.

## Setup

- wispd listens on `127.0.0.1:7788` (`--listen`, API and dashboard) and on
  `127.0.0.1:7789` (`--api-listen`, bearer API only, any Host). Both serve
  `/v1/sprites` without a Host header. The harness uses 7788. The
  `wisp-api-*` units are `systemd-socket-proxyd` forwarders from the tailnet
  address to these two ports.
- wispd defaults: `--idle-timeout 30s` (running to warm), `--warm-ttl 1h`
  (warm to cold, a real reboot per S4). Not waited for here.
- Guest: Ubuntu 24.04, x86_64, user `sprite`, as in S4.
- `m3b.ts` extends `spikes/s4-reach/probe.ts`. It refuses any sprite name not
  starting with `illogical-m3b-`. Bun 1.4.2 (`.mise.toml`).

```sh
SPRITE_TOKEN="$(cat ~/.local/share/wisp/token)" bun m3b.ts <cmd> [args]
# firstprompt N | createwatch | initsize NAME | resize NAME | winch NAME
# seqexec NAME N | install NAME PROBE | seqproxy NAME N | oneshot NAME N
# pause NAME attached|detached N | pollexec NAME | replaycheck NAME
# two NAME | maxrun NAME LIMIT WAIT_MS | gone NAME idle|busy [PROBE]
```

The client runs on the same host, so these numbers have no network in them.
Through the tailnet or the public listener, add about 50ms per round trip
(S4).

## 1. Create to first prompt

Five fresh sprites (`firstprompt 5`). Each run did a POST create, then an
exec `bash -l` TTY with `max_run_after_disconnect`. Times are in ms from the
start of the POST.

| step | median | range |
|---|---|---|
| POST create returns (201) | 13.6 | 4.7–40.6 |
| exec WebSocket open | 266.6 | 233.5–418.9 |
| `session_info` | 266.7 | 233.6–419.2 |
| first output byte | 324.2 | 283.9–601.3 |
| prompt visible (`$ `) | 327.8 | 284.1–602.2 |
| `echo` round trip done | 333.0 | 289.3–607.8 |

- The first run was the slowest (419ms open, 602ms prompt). The other four
  were 234–334ms open and 284–390ms prompt.
- Status transitions (`createwatch`, polled every 10ms):
  - create returns `status: "cold"`;
  - the sprite stays `cold` with no exec (checked for 3s), so create does not
    boot it;
  - it shows `running` 7ms after the exec request, with no other state in
    between;
  - the WebSocket upgrade blocks until the guest agent is up (about 250ms),
    then `session_info` arrives at the same moment.
- So the cost is the boot, paid by the first exec. Create itself is nearly
  free.
- `session_info` reports 80x24 unless the exec URL carries `cols`/`rows`.
  With `&cols=133&rows=37`, `session_info` and `stty size` both said 37x133
  (`initsize`). That is what the wisp dashboard does too. Passing the size in
  the URL removes the window where the shell starts at 80x24.

## 2. Resize after reattach

**Resize works after reattach on wisp. `is_owner:false` changes nothing we
could find.** (`resize`, `winch`)

- The session started at 100x30 (`stty size` = `30 100`), then the
  WebSocket closed and a new one attached to the same session id.
- `session_info` on reattach has `is_owner:false` and the current size, for
  example `cols:100, rows:30`.
- A `{"type":"resize"}` frame from the reattached client was applied every
  time, and `stty size` inside showed the new size. Variants tried, all with
  the same result (`is_owner:false`, resize applied, no error or other frames):
  - plain attach;
  - resize sent before `session_info`;
  - `?tty=true&cols=…&rows=…`;
  - `?tty=true&stdin=true&cols=…&rows=…`;
  - `?owner=true`, `?takeover=true`, `?is_owner=true`.
- The `cols`/`rows` query on **attach** is ignored. The size changes only
  when a resize frame is sent.
- A foreground program gets SIGWINCH. A `bash -c 'trap … WINCH'` loop printed
  `GOT-WINCH 45 140` when the reattached client sent 140x45.
- Every attached client can resize, and the last frame wins:
  - with the owner still attached, a second non-owner client sent 111x33 and
    it applied;
  - the owner's 122x44 applied after it;
  - after the owner left, a reattacher's 140x45 applied, then a third
    client's 90x20.
- The workaround we planned to try (a second exec runs `stty -F /dev/pts/N`
  and `pkill -WINCH`) doesn't work: exec PTYs are `root:tty 0620` and execs
  run as `sprite`, so it fails with "Permission denied". It isn't needed.

So `is_owner` on wisp is informational. On Fly a reattacher is `is_owner:true`
(S4), so resize after a restart works on both. Whether Fly lets a non-owner
resize while the owner is still attached is not tested.

## 3. Throughput

`seq 1 1000000` in an interactive shell: 6,888,896 bytes from `seq`, which
the TTY turns into 7,888,896 (CRLF). Timed from sending the line to the end
marker, five runs each, on one sprite.

| path | median | range | MB/s median (range) | lines correct |
|---|---|---|---|---|
| exec TTY | 299ms | 250–378 | 26.4 (20.9–31.6) | 1,000,000 in order, every run |
| proxy to `s4-probe` | 285ms | 274–291 | 27.7 (27.1–28.8) | 1,000,000 in order, every run |

- The two paths are the same speed. The proxy is a little steadier.
- References:
  - `seq 1 1000000 >/dev/null` in the guest takes 4ms;
  - the same through a host PTY on geek (`script | wc -c`) takes 327ms.

  So both paths run at about host-PTY speed.
- A one-shot exec (`bash -c "seq -f 'L%09g' 1 200000"`, 2.4MB) delivered
  every byte **before** its `exit` frame, then a close with code 1000, in
  204ms (`oneshot`).
- **S4's figure (about 400KB/s, 2.4MB in 6s on wisp) doesn't reproduce.** The
  same command now takes 0.24–0.27s end to end. S4's number probably included
  harness sleeps.
- Not measured: backpressure when the client reads slowly (does wisp buffer
  without limit, block the PTY, or drop?).

## 4. Resume from pause

**An attached idle exec WebSocket keeps the sprite awake.** In two runs, a
shell sat at its prompt with the WebSocket open and no I/O, and the status
stayed `running` for the full 120s both times. With the WebSocket closed and
nothing else changed, it went `warm` after 31–32s.

Detached, five runs (`pause detached 5`):

| | median | range |
|---|---|---|
| detach to `warm` (status polled every 1s) | 32.0s | 31.0–32.0 |
| reattach WebSocket open on a warm sprite | 25.3ms | 24.0–27.4 |
| `session_info` | 30.4ms | 29.1–32.5 |
| keystroke to echo after that | 2.2ms | 2.1–2.3 |
| **total, reattach to echo** | **32.6ms** | 31.3–34.8 |

- Resume from `warm` costs about 25ms, and the shell, its PID and its
  environment were all intact.
- Cold (after 1h warm) is a real reboot on wisp (S4). The exec sessions
  would be gone then. Not waited for.
- Things that do and don't keep the sprite awake:
  - `GET /v1/sprites/{name}` polling does **not** (S4, and confirmed here at
    1s intervals);
  - `GET /v1/sprites/{name}/exec` polling **does**. Polled every 2s with a
    detached idle shell, the sprite stayed `running` for 70s (`pollexec`);
  - that listing does show output made while detached: `last_activity` moved
    from the detach time to +8s when a backgrounded `sleep 8; echo` printed,
    and the reattach replay contained the line.

## 5. Two execs on one sprite

(`two`) Two `bash -l` TTY sessions, A at 100x30 and B at 140x45.

- **Independent PTYs and sizes.**
  - A was `/dev/pts/6` (30x100) and B was `/dev/pts/7` (45x140).
  - Each shell leads its own session (`sid` = its own pid).
  - Both are children of the guest agent (`exe`, pid 372).
- **Not isolated.** It is one VM: one PID namespace, one user, one
  filesystem, and each shell can see the other's processes in `ps`. That is
  what a tab-owned machine wants.
- **Detach and reattach independently.**
  - Both detached, then each reattached by its own id with its own size and
    environment (`WHO=alpha` / `WHO=bravo`).
  - The sizes stayed separate across the reattach.
- **Pause and resume.**
  - With both detached, the sprite went `warm` after 32s.
  - Reattaching both brought back both shells with the same PIDs, PTYs,
    sizes and environments.
- **Kill.** `POST …/exec/{A}/kill` returns 200 with NDJSON:
  - `sent SIGTERM`, then `process did not exit; sending SIGKILL`;
  - an interactive bash ignores SIGTERM, so the kill takes **10s**;
  - A's WebSocket then got `{"exit_code":137}` and close 1000;
  - B was untouched and still answered, and the listing showed only B.
- **`max_run_after_disconnect` is enforced** (`maxrun`). A shell detached
  with `5s` was gone after 12s:
  - its pid was gone and it had dropped out of `GET /exec`;
  - attaching to its id still returned `session_info` and then
    `{"exit_code":137}` and close 1000.
- `GET /exec` lists live sessions with `is_active` (a client is attached),
  `last_activity` and `bytes_per_second`.

## 6. Machine gone

(`gone`) DELETE on a sprite with an attached exec shell and an attached proxy
connection to `s4-probe`. One run was idle and one was busy (printing every
50ms).

| | idle | busy |
|---|---|---|
| DELETE | 204 in 33ms | 204 in 39ms |
| exec WebSocket | closed **1006** at +32ms, no text frames, **no `exit` frame** | 1006 at +37ms, same |
| proxy WebSocket | 1006 at +9ms (before DELETE returned) | 1006 at +9ms |
| reattach the old exec id | upgrade refused (404) | same |
| `GET /exec/{id}`, `GET /{name}` | 404 `sprite not found` | same |

Bun reports the 1006 reason as "Connection ended", so the TCP connection
simply drops. A dropped connection looks the same as a network break or a
wispd restart. The only way to tell "machine gone" apart is a follow-up
`GET /v1/sprites/{name}`: 404 means gone.

## Replay on reattach (found while testing 2 and 4)

- On reattach, wisp replays its ring from the start of the session, up to
  1 MiB (S4), **including bytes this client already received**. Example
  (`replaycheck`): 298 bytes live, then 298 bytes again on reattach.
- No frame marks the end of the replay. These query params made no
  difference: `since`, `offset`, `replay=false`, `scrollback=0`,
  `history=false`.

## Consequences for M3b

1. **Use exec TTY, not proxy + in-guest daemon.**
   - Exec is as fast as the proxy (about 26 vs 28 MB/s, both near host-PTY
     speed), and it needs nothing installed in the image.
   - The first prompt arrives about 330ms after create.
   - Keep `s4-probe`/proxy for M4's resident daemon only.
2. **Put the size in the exec URL** (`&cols=…&rows=…`), and still send a
   resize after `session_info` in case it changed. Then the shell never sees
   80x24.
3. **Resize after a daemon restart needs no special handling on wisp.**
   - Reattach, then send a resize frame; it applies and the program gets
     SIGWINCH.
   - We don't need an owner handoff from wisp. Drop that from the plan.
   - Treat `is_owner` as a capability hint in the `Provider` trait, not a
     gate.
4. **An attached exec keeps the machine running.**
   - A VM pane the daemon keeps attached never pauses. For throwaway agent
     machines that may be fine, since an agent usually exits and the pane
     closes.
   - If idle VM panes must pause, the daemon has to detach after some idle
     time and reattach on input. That costs about 33ms on the first key from
     a warm sprite.
   - While detached the daemon can't see output without waking the sprite:
     `GET /exec` keeps it awake too, and only `GET /{name}` doesn't.
   - Proposal: ship M3b **always attached** (simple and correct), and record
     "detach idle VM panes" as a follow-up. That follow-up depends on
     replay dedup (item 5).
5. **Reattach must skip replayed bytes.**
   - The daemon's VtEngine and log already hold everything up to the
     disconnect, and wisp resends it.
   - Under 1 MiB of total output: count bytes per exec (persist the count in
     `meta.json`) and drop that many from the start of the replay.
   - Past 1 MiB: the replay is the last 1 MiB of the ring and its start
     offset is unknown. We'd need an end-of-replay marker or a `since` offset
     from wisp. Until then, M2b restarts of VM panes can duplicate up to 1 MiB
     of output into the log, or the daemon has to diff by content.
   - Ask wisp for an offset in `session_info` or a `since=` param. That is a
     small wisp change and the cleanest fix.
6. **Close: DELETE the sprite and skip the kill.**
   - For a pane-owned machine, DELETE alone takes about 35ms and closes
     every exec.
   - `kill` on an interactive shell takes 10s (SIGTERM, then SIGKILL).
   - For tab-owned machines, closing one pane should kill that exec in the
     background. Or send `exit`/EOF first, or `kill` with a signal if wisp
     supports one (not tested).
7. **"Machine gone"** = the exec WebSocket closes abnormally (1006, no `exit`
   frame) **and** `GET /v1/sprites/{name}` returns 404.
   - 1006 with a 200 means a transient drop: reattach.
   - A normal process exit always comes as an `exit` frame followed by close
     1000.
   - Attaching to a session that died (for example after
     `max_run_after_disconnect`) also returns `session_info` + `exit` 137.
     Treat that as the process ending, not the machine going away.
8. **`max_run_after_disconnect` is enforced.** Pick a value longer than any
   daemon restart or upgrade (hours), not seconds. Otherwise a slow M2b
   restart finds the shells killed.
9. **Two execs per machine work independently** (separate PTYs, sizes,
   sessions, detach/reattach, kill), so tab-owned machines are safe to build
   on later. They share one process space and user by design.

## Still open

- Whether an exec WebSocket keeps the sprite awake because of the connection
  itself or because of WebSocket pings. wisp might pause with pings
  disabled. Not tested.
- Backpressure with a slow reader on exec (buffer, block or drop).
- Fly's behaviour for a non-owner resize while the owner is attached, and
  Fly's exec throughput measured the same way.
- `kill` with a chosen signal (for example SIGHUP) to avoid the 10s wait.
- What an exec WebSocket sees during a wispd restart (not tested, to avoid
  disturbing other users of this host). It is presumably also 1006, and the
  GET check above separates the two cases.
- Behaviour after warm-ttl (cold, a real reboot): the exec sessions should be
  gone. Not waited for.

## Cleanup

- Sprites created: `illogical-m3b-a`, `illogical-m3b-fp-{653382,655537,657374,659200,661056}`,
  `illogical-m3b-cw-{672476,676184}`, `illogical-m3b-gone-{1,2}`,
  `illogical-m3b-init`. All were deleted, and each returns 404 from
  `GET /v1/sprites/{name}`. No other sprite was listed or touched.
- The API token was passed inline at runtime only. It is not stored in any
  file here; a grep of this directory and the scratch logs for it found
  nothing.
