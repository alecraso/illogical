# S30: talk spike (#239)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s30-talk/<file>`.

Run 2026-10-05 on geek (Ubuntu 26.04), jake-air (macOS 15.5) and the Win11 VM. This spike comes before the Talk track: M61 threads, M62 team channels, M63 voice. The tracker is #244.

## Answers

- **Voice: go, with one native piece; on iOS, the mic works only in the foreground.** WebRTC works in Chrome, Firefox, Safari, WKWebView (the macOS app) and Edge/WebView2 (the Windows app), including relay-only calls through TURN.
  - **The Linux desktop app's WebKitGTK has no WebRTC on any distro.** Upstream builds it only with experimental features.
  - The fallback works: a native Rust peer (webrtc-rs, Opus, cpal, AEC3) called Chrome on jake-air through TURN. 15 ms RTT, 0 loss, about 2% of a core.
- **Team channels: go, with openmls.**
  - It runs in wasm in Chrome, Firefox and WebKit: 540 kB gzipped; 8 ms to join and 11 ms to commit at 50 devices.
  - Every roster event maps to one commit. A device refuses any commit the signed roster doesn't allow, so control can't add a reader.
  - Control can still drop, delay or fork a channel. Forks are detectable and recoverable. Details are in [mls/FINDINGS.md](mls/FINDINGS.md).
- **Signed DTLS fingerprints work.** A signaling server that swaps the fingerprint is refused by both sides. Pinning the key to the device certificate is what stops a server that also swaps the key.
- **TURN: Cloudflare for the hosted control** (1 TB a month free, then $0.05/GB); coturn for self-hosting.
- **Team machines aren't channel members by default.** Agents use pane and session threads (M61).

## What M62 and M63 need from this

**M63 (voice):**
- On Linux, the desktop app runs calls in Rust: webrtc-rs 0.21, Opus, cpal, and `webrtc-audio-processing` (AEC3, bundled). The page does signaling and the UI, and hands SDP to Rust through a Tauri command.
- Everywhere else, the page's `RTCPeerConnection` runs the call.
- The mic prompt goes through wry's `with_permission_handler`. macOS needs `NSMicrophoneUsageDescription`.
- The signature covers the `a=fingerprint:` lines and is made with the **device key**. The peer's key must come from its device certificate, never from the signaling message.
- Mesh limit: 5. Bandwidth isn't the limit (about 40 kbit/s per stream).
- CI for the desktop app needs meson, ninja and libclang for the bundled AEC.

**M62 (channels):**
- openmls 0.9 with RustCrypto and ciphersuite 0x0001.
- A **separate MLS key per device**, bound by the device key, because WebCrypto signs asynchronously and openmls needs synchronous signing.
- A `BasicCredential` holding the device certificate plus that binding.
- A `CommitAad` naming the roster version and revocation count. Every device runs the roster check before merging a commit.
- `max_past_epochs` of 3–5.
- Lock removes non-owner leaves; Unlock re-adds them.
- Merge your own commit only after control accepts it.
- Fork detection: devices report epoch-authenticator hashes to team daemons.
- Open designs:
  - the roster state as a GroupContext extension, for Welcome joiners;
  - persisting group state to IndexedDB through a write-behind cache.

## Voice: WebRTC in every client

### Go/no-go per client

| Client (where tested) | `RTCPeerConnection` | Relay-only call through TURN | Microphone | Insertable streams (for SFrame, M64) | Verdict |
|---|---|---|---|---|---|
| **Desktop app, Linux** (WebKitGTK 2.52.6, Ubuntu 26.04, geek) | **no**, even with `enable-webrtc` on | n/a | `getUserMedia` exists | no | **no-go in the webview → native fallback (go)** |
| WebKitGTK on other distros (Fedora 44 2.54.0, Arch 2.52.6, Debian trixie 2.54.0 and sid 2.54.1, Tumbleweed 2.52.6) | **no** (no WebRTC backend in the library) | n/a | n/a | n/a | same |
| **Desktop app, macOS** (WKWebView, macOS 15.5, jake-air) | yes | yes: 94 ms to connect, 6 ms RTT | yes, launched from the GUI with `NSMicrophoneUsageDescription`: real mic, echo cancellation on. Launched over ssh it's refused, because macOS asks on behalf of sshd | `RTCRtpScriptTransform` | **go** |
| **Desktop app, Windows** (Edge 154 headless, Win11 VM; same engine as WebView2) | yes | yes: 137 ms | fake device ok | both | **go** (WebView2 itself not run) |
| Chrome (geek, jake-air) | yes | yes: 103–129 ms | Air's real mic ok | both | **go** |
| Firefox (Playwright, geek) | yes | yes, on the tailnet IP; it refuses TURN on loopback | fake device ok | `RTCRtpScriptTransform` | **go** |
| Safari 18.5 (macOS 15.5, jake-air) | yes | yes: 93 ms, 7 ms RTT | yes: real mic, echo cancellation on | `RTCRtpScriptTransform` | **go** |
| Phone PWA (iOS, from the home screen) | yes | yes: relay to relay with the Air, 8–11 ms RTT, 0 lost | yes, in the foreground. **In the background the mic stops** (see "Live checks") | not checked | **go, foreground only** |

**Why Linux has none.** Upstream `Source/cmake/OptionsGTK.cmake` sets `ENABLE_WEB_RTC` to `${ENABLE_EXPERIMENTAL_FEATURES}`, which release builds leave off, and no distro turns it on. `ENABLE_MEDIA_STREAM` is on, which is why `getUserMedia` exists without `RTCPeerConnection`. Playwright's own WebKitGTK build, with experimental features on, does have it (libwebrtc symbols in the library, relay-only call ok). That build served as the positive control for the library check (`grep -a -c 'webrtc::PeerConnection'` finds 3 hits there and 0 in every distro build). Bundling our own WebKitGTK is out of the question for size and security updates.

### The native fallback (Linux desktop app)

`native/` is a standalone peer, built the way the app's Rust side would run it: webrtc-rs 0.21 (with its TURN client), libopus (static), cpal for audio, and Ed25519 signing.

**Done-when demo:** the native peer on geek called Chrome on jake-air, **relay-only on both ends through TURN**. Both sides set `iceTransportPolicy: relay`, so no direct UDP path was used.
- Connected 827 ms after the offer. Path `relay -> relay`, 15 ms RTT, 0 packets lost out of 748, 4 ms jitter, 38 ms jitter buffer.
- The Air heard geek's 440 Hz tone (its analyser peak was 439 Hz, RMS 0.141). geek received the Air's real microphone; the recording was deleted.
- Local runs against headless Chrome checked both directions: geek heard the browser's 660 Hz at the expected level, and the browser heard 439 Hz.

| Measure | Value |
|---|---|
| Binary, stripped, standalone (tokio, rustls, ring, webrtc-rs, libopus, AEC3) | 13.0 MB (12.1 MB without AEC3). The app already has tokio, so the real increase is smaller |
| CPU, one call (Opus encode and decode, DTLS-SRTP, TURN) | 1.5–2% of one core on geek |
| Connect time after the offer | 519 ms locally through TURN, 827 ms to the Air |
| Echo cancellation (AEC3 via `webrtc-audio-processing` 2.1, bundled; `aec/`) | 22–28 dB of echo removed once converged; 5.7 ms per second of audio (about 0.6% of a core); +1.3 MB |

Gaps and gotchas:
- **Real mic capture on geek is unverified.** cpal opened ALSA's default device (PipeWire) and the speaker, but the default source (the OBSBOT Meet 2) delivered nothing, and `pw-record` got 0 bytes from it too. So this is geek's audio state, not the code. Jake chose to skip it (see "Still open").
- **Double talk** (both people speaking at once) through AEC3 can't be judged from a synthetic tone; it needs a listening test.
- **Building:** `webrtc-audio-processing`'s bundled build needs meson and ninja (installed in a venv here) and libclang. bindgen needed `BINDGEN_EXTRA_CLANG_ARGS=-I/usr/lib/gcc/x86_64-linux-gnu/15/include` on geek because there are no clang headers. CI will need the same.
- **webrtc-rs 0.21** is a rewrite on the sans-IO `rtc` crate, so expect its API to keep moving. It worked first time, TURN included. str0m wasn't tried because it has no TURN client, and the app needs one when both ends are behind NAT.

**Shape for M63:** the page does signaling as it does everywhere. On Linux the desktop app hands the SDP to the Rust side through a Tauri command, and the Rust side runs the call: audio in and out, AEC, Opus, ICE/TURN. The page shows the same UI. Everywhere else the page's own `RTCPeerConnection` runs it. wry 0.57 has `with_permission_handler` (WebView2's `PermissionRequested`, WKWebView's `requestMediaCapturePermission`, WebKitGTK's `permission-request`) for the mic prompt in the app. macOS also needs `NSMicrophoneUsageDescription` in the app's Info.plist.

### Signed DTLS fingerprints

Each side signs the `a=fingerprint:` lines of its SDP with an Ed25519 key (WebCrypto in the browser, ed25519-dalek natively) and checks the other side's signature before applying the SDP.
- **A hostile signaling server** (a `tamper-*` room in `server.mjs` swaps the fingerprint, as a MITM would) is caught at once. The native answerer refuses before answering, and an answering browser refuses too.
- **A server that also swaps the key** is caught only when the peer's key is pinned (`?peer=` / `--peer-key`). An honest server with a wrong pinned key is refused, naming both keys. That's why in M63 the key has to be the device key, checked against the peer's device certificate (control-e2e.md's chain), and not a key that arrives with the SDP.

### TURN

The spike ran coturn in Docker on geek (command in "Reproduce"), on the tailnet IP.

| | Cloudflare Realtime TURN | coturn on Fly |
|---|---|---|
| Cost | first 1,000 GB free, then $0.05/GB (server-to-client) | $2/month for the dedicated IPv4 UDP needs, plus Fly egress |
| Setup | an API key; control asks Cloudflare for short-lived credentials | its own app, binding `fly-global-services`. Fly doesn't rewrite UDP ports, and the docs don't say whether a service can take a port range, which TURN relays need |
| Reach | anycast, TURN over TLS on 443 for strict networks | one region unless we build more |
| Privacy | Cloudflare sees peers' IPs and traffic volume, never content (SRTP) | Fly, likewise |

Usage: Opus voice is about 40 kbit/s with overheads, roughly 18 MB per relayed stream-hour. 1 TB free is about 25,000 relayed stream-hours a month. **Recommendation: Cloudflare for the hosted control**, named in the privacy notice. A self-hosted control documents coturn. Not tested: no Cloudflare TURN key was made, because that's an account change for Jake.

**Mesh size.** At about 40 kbit/s per stream, a 5-person mesh is 160 kbit/s up and down per person, which fits any link. The limit is CPU (one encoder per peer connection) and how the call sounds, not bandwidth. Keep M63's limit at 5 until a live call says otherwise.

## Team channels: MLS

All of it, with numbers, is in [mls/FINDINGS.md](mls/FINDINGS.md). In short:

| | |
|---|---|
| wasm bundle | 1.35 MB raw, 540 kB gzipped (RustCrypto, opt-level z); about 350 kB looks reachable |
| Chrome, 50 devices | Welcome join 7.7 ms, external-commit join 12.4 ms, add/remove commit 10.7–12.6 ms, receiving a commit about 3 ms, a message 0.2 ms |
| Welcome / GroupInfo at 50 devices | about 34 kB each (about 680 bytes per device, mostly the JSON credential) |
| Demo | `cargo run --release --bin demo` in `mls/`, or `bench.html` in a browser. bob, removed mid-conversation, reads 0 of 2 later messages; control's forged device is refused by everyone |

## Live checks with Jake (2026-10-05)

- **Mic prompts on the Mac:** the WKWebView probe app, launched from the GUI, asked for the mic. Once Jake allowed it, it captured the MacBook Air microphone with echo cancellation on. Safari 18.5 did the same.
- **iPhone PWA ↔ Chrome on the Air**, relay to relay through TURN (`/p` on the test server, added to the home screen):
  - **Foreground:** connected with 8–11 ms RTT, 0 packets lost, about 36 ms jitter buffer. Jake heard it clean, with the phone next to the Air (both mics and both speakers side by side).
  - **In the background or locked (about 16 s):** the phone **stopped sending**. The Air got 0 packets a second, apart from one 2 s burst. It **kept receiving and playing** the Air's audio (Jake still heard it), the connection stayed up, and the page's script kept running.
  - **After coming back, there was some feedback for the rest of the call.** This was the hardest setup for echo cancellation: both mics and both speakers right next to each other. The same setup was clean in the foreground before the lock, so something changed on resume, possibly iOS bringing the mic back without its echo cancellation. With devices apart, it might not show at all. Not verified.
- **What M63 does about the phone:**
  - say plainly that the phone's mic only works with the app open, and show the user as "muted (app in background)" to the others;
  - on `visibilitychange` back to visible, get a fresh mic track (`getUserMedia` plus `replaceTrack`) instead of trusting the resumed one. Then check again that the feedback is gone.

## Still open

1. **Native mic capture and AEC3 in a live call** on the Linux app. geek's default source (the OBSBOT Meet 2) gave PipeWire 0 bytes, so Jake chose to skip it. The native peer now runs AEC3 and noise suppression on the mic (`--no-aec` turns them off), but only the synthetic echo test has exercised them.
2. **A Cloudflare TURN key** for the hosted control, if Cloudflare is the choice.

## Reproduce

On geek, from `spikes/s30-talk/` (PATH as in `mls/FINDINGS.md`):

```sh
# test server: probe reports + signaling (stands in for the daemon); tailnet https via tailscale serve
(cd webrtc && pnpm install && node server.mjs &)            # 127.0.0.1:8799, HOST=0.0.0.0 for the VM
tailscale serve --bg --https=8443 http://127.0.0.1:8799     # off: tailscale serve --https=8443 off
docker run -d --name s30-coturn --network host coturn/coturn -n --log-file=stdout \
  --listening-port=3478 --listening-ip=<tailnet ip> --listening-ip=127.0.0.1 --relay-ip=<tailnet ip> \
  --external-ip=<tailnet ip> --min-port=49160 --max-port=49200 --realm=s30 --user=s30:s30secret \
  --lt-cred-mech --fingerprint --allow-loopback-peers --no-tls --no-cli

# client probes
python3 webrtc/gtkprobe.py 'http://127.0.0.1:8799/probe.html?client=gtk' --enable --mock   # WebKitGTK
TURN_IP=<tailnet ip> node webrtc/pwprobe.mjs        # Chrome, Firefox, WebKit (uses web/'s Playwright: pnpm install in web/)
webrtc/mac/build.sh && S30Probe.app/Contents/MacOS/S30Probe '<probe url>'                  # on the Mac
# distro WebKitGTK: grep -a -c 'webrtc::PeerConnection' libwebkit2gtk-4.1.so.0 (0 = no WebRTC)

# native peer (Linux deps without sudo: apt-get download libasound2-dev libopus-dev, dpkg-deb -x into a
# sysroot, PKG_CONFIG_PATH and PKG_CONFIG_SYSROOT_DIR at it)
(cd native && cargo build --release)
native/target/release/s30-native --ws ws://127.0.0.1:8799/ws?room=r1 --turn turn:<ip>:3478 \
  --user s30 --pass s30secret --relay --seconds 20 [--mic] [--play] [--wav out.wav]
# the other end: https://geek.<tailnet>:8443/call.html?room=r1&auto=1&relay=1&turn=turn:<ip>:3478&user=s30&pass=s30secret
# hostile signaling: any room named tamper-*

# echo cancellation (needs meson + ninja, LIBCLANG_PATH, BINDGEN_EXTRA_CLANG_ARGS=-I<gcc include>)
(cd aec && cargo run --release)
```
