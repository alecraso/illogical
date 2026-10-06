# S15: end-to-end channels, device keys and the relay
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s15-control/<file>`.

Run 2026-10-01 from geek, with a throwaway relay on Fly
(`illogical-s15-relay`, `shared-cpu-1x`, 256 MB). The design that came out
of it is [docs/control-e2e.md](../../docs/control-e2e.md).

## What's here

- `src/main.rs`, the `s15` binary:
  - `keygen`;
  - `bench` (in-process costs);
  - `daemon` (a stand-in daemon: Noise IK responder, echo and burst);
  - `client` (timings; `--streams N` holds N channels).
- `src/relay.rs`: the relay as control would run it. Daemons dial
  `/dial/<name>`, clients connect to `/c/<name>`, and it splices them over
  M4c's mux (`src/mux.rs`, copied from the daemon). It also serves the test
  pages and logs `/report` posts.
- `web/noise.ts`: Noise IK initiator on WebCrypto alone (X25519, AES-GCM,
  SHA-256, HMAC). Build it with `tsc` into `web/dist/noise.js`.
- `web/keys.html`: checks device keys and passkey PRF in a browser.
- `web/rtt.html`: phone to relay, relayed channel and direct channel timings.
- `web/share.html`: whether a link previewer runs scripts and sees the
  fragment.
- `web/client.ts`, `web/browser-check.mjs`, `web/lna-check.mjs`,
  `web/page-check.mjs`: the Node and Playwright drivers.

```sh
cargo build --release
./target/release/s15 bench
./target/release/s15 keygen > d.key
./target/release/s15 relay --listen 127.0.0.1:7801 &
./target/release/s15 daemon --key d.key --relay "ws://127.0.0.1:7801/dial/box?pub=$(awk '/public/{print $2}' d.key)" --listen 127.0.0.1:7802 &
./target/release/s15 client --url ws://127.0.0.1:7801/c/box --peer <public hex>
node --experimental-strip-types web/client.ts ws://127.0.0.1:7801/c/box <public hex>
fly deploy --local-only   # the Fly relay; fly apps destroy illogical-s15-relay when done
```

## Results

**Crypto costs** (`s15 bench`, geek):

- **Handshake (IK):**
  - 96 bytes out and 48 back;
  - about 370 µs of CPU for both ends, including two key generations.
- **1 MB burst in 16 KB messages:**
  - 0.098% wire overhead;
  - 805 MB/s encrypt plus decrypt.
- **Keystroke:** 1 byte becomes 17; 0.4 µs.
- **Fan-out of 10 MB to N viewers:**

  | Viewers | Per-viewer channels | One session key (relay copies) |
  |---|---|---|
  | 2 | 13 ms CPU, 21 MB up | 6.5 ms, 10.5 MB |
  | 5 | 33 ms CPU, 53 MB up | 6.5 ms, 10.5 MB |
  | 20 | 130 ms CPU, 211 MB up | 6.6 ms, 10.5 MB |

  CPU is irrelevant; uplink is what a session key would save.

**WebCrypto interop:**

- `web/noise.ts` against `snow` worked first time, directly and through the
  relay.
- **Node 22:**
  - handshake 4.7–6 ms on first use;
  - keystroke round trip 0.055 ms direct and 0.087 ms relayed (loopback);
  - 1 MB in 2.3–2.7 ms.
- **Chrome 153:**
  - X25519 and Ed25519 keys generate as non-extractable;
  - the key survives a reload from IndexedDB;
  - `extension:prf` is reported;
  - the in-page handshake took 0.7 ms on reload.

**The relay:**

| Path | Keystroke p50 | p99 | 1 MB |
|---|---|---|---|
| Loopback, direct | 15 µs | 38 µs | 1.1 ms |
| Loopback, relayed | 36 µs | 67 µs | 1.1–1.8 ms |
| geek → Fly ord → geek | 51.6 ms | 54.7 ms | 208 ms |
| geek → Fly ewr → geek | 18.7 ms | 22.4 ms | 99 ms |
| geek → ewr echo only | 9.1 ms | 9.8 ms | |
| Phone on cellular → Fly → geek | pending | | |

Capacity: 1,000 relayed channels at once, each typing once a second, gave
0 failures, p50 0.9 ms and p99 7.2 ms. The relay's RSS was 115 MB
(loopback).

**Findings:**

1. **Nagle cost 40 ms per round trip on the dial-out path.**
   - The relayed keystroke took 41 ms whenever the accepting end of the
     daemon's socket had Nagle on. That is `axum::serve`'s default.
   - The real home daemon had the same bug: `/h/<name>/api/host` took
     41.7 ms against 1.4 ms direct.
   - Fixed in the daemon (`TCP_NODELAY` on accepted connections): 2.0 ms.
2. **The relay costs one relay↔daemon round trip.** Run the relay in the
   region nearest the daemon: ewr from geek, not ord. Hosted control goes
   multi-region with `fly-replay`.
3. **Chrome's Local Network Access blocks public pages from tailnet
   addresses.**
   - `wss://geek.<tailnet>.ts.net` from a page on `*.fly.dev` failed with
     `ERR_BLOCKED_BY_LOCAL_NETWORK_ACCESS_CHECKS`.
   - Granting `local-network-access` fixed it.
   - So a web client served by control must ask for that permission before
     using direct paths, and fall back to the relay.
4. **Per-viewer channels win for small teams.** A session key only saves
   uplink, and only with relay fan-out, flow control in the relay and key
   rotation on revoke. That is deferred until a measured need.

**Pending, on Jake's phone** (results go to `fly logs -a
illogical-s15-relay`, lines starting with `REPORT`):

- `rtt.html?direct=wss://geek.tail1234.ts.net:10000/` on cellular and on
  Wi-Fi;
- `keys.html`: PRF create and authenticate, then a reload and authenticate
  again, on iOS Safari and Android Chrome;
- `share.html#k=…` pasted into Slack and iMessage.

**Left running for those tests:**

- the spike daemon on geek (`run-daemon.sh` in the session scratchpad);
- `tailscale serve --https=10000`;
- the Fly app.

Tear down with `tailscale serve --https=10000 off`, `pkill -f
run-daemon.sh`, and `fly apps destroy illogical-s15-relay`.
