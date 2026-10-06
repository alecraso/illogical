# S4: reach (sprites, wisp, tailnet)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s4-reach/<file>`.

Run 2026-10-01 from geek. The sandboxes tested:

- **Fly:** a throwaway sprite in `managoat-tenants`, Ubuntu 26.04.
- **wisp:** a throwaway sprite on the home host, Ubuntu 24.04.

Both were named `illogical-s4-probe`. Both run x86_64 with 8 vCPUs.

**Tools used:**

- **`s4-probe`** stands in for `illogicald`. It is a 572KB static-pie musl
  binary. Each TCP connection on `127.0.0.1:7681` gets a login shell on its
  own PTY, passed through as raw bytes.
- **`probe.ts`** is a Bun harness for any Sprites-compatible API. Set
  `SPRITES_API_URL`/`SPRITE_TOKEN` and run `bun probe.ts <cmd> <sprite>`.

```sh
cargo build --release --target x86_64-unknown-linux-musl
SPRITE_TOKEN=... bun probe.ts create|run|replay|detach|attach|kill|sessions|watch|install|proxy|hold|status|delete <sprite> [args]
```

## Answers

| Question | Fly | wisp |
|---|---|---|
| CPU arch | x86_64 | x86_64 |
| Static musl daemon opens PTYs and runs as a `sprite-env` service | yes | yes |
| Detached **idle** exec shell keeps the sprite awake | **no** (warm after about 30s) | **no** (about 35s) |
| Detached **busy** exec session keeps it awake | yes (I/O counts) | yes |
| Detached shell survives a warm pause and reattaches | yes | yes |
| Exec replay buffer on reattach | **about 6.5KB** (the last 538 of 200k lines) | **1 MiB** ring |
| Reattacher is owner (can resize) | `is_owner:true` | `is_owner:false` |
| Held-open TCP proxy connection keeps it awake | yes | yes |
| Status polling (`GET /v1/sprites/{name}`) keeps it awake | no | no |
| Time to pause with nothing attached | about 40–50s | about 15–30s |
| Proxy to the in-sprite daemon, sprite **running** | first byte 221ms, round trip 51ms | 21ms / 50ms |
| Proxy wakes a **warm** sprite and the daemon answers | yes, first byte 224ms (no visible wake cost) | yes, 77ms |
| Proxy wakes a **cold** sprite and the service is back | yes, first byte 248ms. After about 5 min idle (`cold`), **processes survived**: same boot time, a marker `sleep` still alive, services not restarted | yes, a real cold boot: first byte 305ms. The service was restarted at wake, and the proxy held the connection until the daemon listened (connected 6ms after `listening`) |
| Time until cold | `warm` at +40s, `cold` at +283s | 1h (`--warm-ttl`) |
| `tailscaled --tun=userspace-networking` as a service | yes (1.102.4 static tarball) | not tested (key was single-use) |
| tailscaled keeps the sprite awake | **no** (warm after 40s) | — |
| Tailnet direct path | yes (DERP for the first 2 pings, then direct UDP), round trip 47ms | — |
| Daemon bound to `127.0.0.1` reachable over the tailnet | yes (netstack forwards to loopback) | — |
| Tailnet traffic **wakes** a paused sprite | **no** (ping and TCP time out; it stays warm) | — |
| Tailnet usable again after a wake through the proxy | within about 5s, direct path restored | — |
| Ephemeral node survives a long pause | yes: still listed after about 45 min offline; pong on the first ping after the wake, and the direct path is restored (tailscaled logs "time jump detected") | — |

**Fly's `cold` was a memory restore, not a reboot, every time we saw it.**
After 5 and 60 minutes `cold`, all processes kept their PIDs (a marker
`sleep`, the daemon, tailscaled). Uptime excluded the frozen time, so the
reported boot time shifted. Wakes took 248ms and 836ms to the daemon's first
byte.

**wisp's `cold` is a real reboot.** Services restart at wake, and the proxy
holds the connection until the port listens.

**Original note:** Fly's docs say a cold wake drops
process state and takes 1–2s. In this run, a sprite reported `cold` 4.7 min
after the last activity and woke in about 250ms with every process intact.
Dropping processes may happen later or under host pressure. Either way,
design for both: a resident daemon must survive a real reboot (the M2
restore path), and must not assume one happened.

## Protocol and compatibility notes

**JSON bodies.**
- Fly requires `Content-Type: application/json` on JSON bodies (`create` returns
  400 "name is required" without it).
- wisp doesn't require it.

**Killing a session.**
- Use `POST /v1/sprites/{name}/exec/{id}/kill`; `DELETE` returns 405 on wisp.
- The response is streamed NDJSON. wisp ends it with
  `{"type":"complete","exit_code":137}`; Fly printed nothing.

**TCP proxy** (`wss://…/v1/sprites/{name}/proxy`).
- Send `{"host":"localhost","port":N}`. You get one text frame
  `{"status":"connected","target":…}`, then raw bytes.
- Fly reports the target as `10.0.0.1:7681`; wisp reports `localhost:7681`.
  Both reach a daemon bound to `127.0.0.1`.

**Exec TTY.** Everything in ravix-hq/ravix#236 held on both providers:
- send stdin raw;
- send a resize after `session_info`;
- always pass `max_run_after_disconnect`.

**Throughput.** About 2.4MB of TTY output took 25s over Fly exec (from geek)
and 6s over local wisp.

## What this means for M4

1. **Sprites sleep when idle, and only the provider can wake them.** A paused
   sprite is frozen, so tailnet packets go nowhere. Any reach design needs a
   provider "wake" call first: a proxy WebSocket, an exec, or a URL hit.
2. **Use both transports.**
   - The tailnet is free to keep: it doesn't hold the sprite awake. Once the
     sprite is awake it gives a direct path and the rest of the network (dev
     ports, rsync, phone-to-sandbox).
   - The provider tunnel is what wakes the sprite, and it's a fine data path in
     its own right: about 50ms round trip, the same as the tailnet from geek.
   - Order: **wake through the provider, then prefer tailnet if it comes up
     within about 5s, else stay on the provider tunnel.**
3. **The resident daemon is the only durable option.**
   - On Fly, provider exec scrollback is about 6.5KB, so "attached" panes
     (M4b's no-install shell) get almost no history on reattach. Treat them as
     disposable.
   - Resident `illogicald` keeps its own log and snapshots on the sprite's
     disk.
4. **Idle shells cost nothing; busy output keeps a sprite billed.** That is
   the right shape for agents: an agent working is billed, an agent waiting
   for input is not. Clients must drop proxy and tailnet connections to hidden
   sprites, because an open connection holds the sprite awake.
5. **Adapter differences are real but small**, even between two "compatible"
   implementations: replay size, owner semantics, content-type strictness,
   kill output. The `Provider` trait should treat exec replay and ownership as
   capabilities to query, not assumptions.

## Cleanup

- Both sprites were deleted (the API returns 404 for each).
- The local token and key copies were removed.
- `tailscale logout` inside the sprite hung and didn't finish. The ephemeral
  node `illogical-s4-probe` was still listed (offline) afterwards. Tailscale
  should remove it automatically; otherwise remove it in the admin console.

## Still open

- tailscaled on wisp (needs another key).
- Whether Fly ever drops processes on cold (none seen up to 60 min).
- How long ephemeral nodes last while their sprite sleeps for days.
