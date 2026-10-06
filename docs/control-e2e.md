# illogical control: keys, channels and the relay (S15 spec)

Written 2026-10-01, from S15 ([spikes/s15-control](../spikes/s15-control/README.md)).
This is the design M17–M21 build. It turns the control track's promise
(PLAN.md, "Control track") into mechanisms: **control can refuse service,
but it can't read.** That promise has limits, set out in
[What holds against control](#what-holds-against-control): the client code
control serves, and the first account a machine joins, are trusted.

## Summary

- **One channel type:** every client–daemon stream is a Noise channel,
  `Noise_IK_25519_AESGCM_SHA256`, on the relay *and* on direct paths.
  Browsers run it on WebCrypto alone; daemons and the CLI use `snow`.
- **Every viewer gets its own channel, including on shared sessions.**
  Sessions have no shared key. Revoking someone closes their channel; there
  is no key to rotate. Fanning out one ciphertext at the relay waits until
  a measured need (see [Fan-out](#fan-out-later-if-ever)).
- **Device keys:**
  - a browser keeps a non-extractable X25519 key (for Noise) and an Ed25519
    key (for signing approvals) in IndexedDB;
  - the CLI and daemons keep key files;
  - a passkey's PRF output can re-derive a browser's keys after storage is
    lost (go/no-go per platform: see [PRF](#prf-re-deriving-a-browsers-keys)).
- **Trust:**
  - control distributes keys, but each device certificate is signed by a
    device the account already trusts;
  - daemons verify the chain themselves, so control can't add a reader;
  - a joining machine pins its account's root only once the person has
    compared the account's fingerprint on the machine with the one on the
    device that approved it ([Joining](#joining-a-machine-the-accounts-fingerprint)).
- **Relay:** control keeps each enrolled daemon's dial-out socket (M4c's
  mux) and splices clients onto it. It sees connection metadata and byte
  counts only.
- **Read-only links:** a link carries a one-off device key in its fragment
  (`#k=…`). The daemon lists that key as a viewer of one session until the
  link expires.
- **Push:**
  - the daemon encrypts the notification (RFC 8291) to a subscription
    the device signed;
  - control only adds the VAPID signature and posts it;
  - control's own notices (something waits for approval) it encrypts
    itself, and they say only that.

## Keys

| Holder | Keys | Where | Lost when |
|---|---|---|---|
| Browser / PWA | X25519 `noise`, Ed25519 `sign` | IndexedDB, non-extractable `CryptoKey`s (wrapped where those don't survive: below) | site data is cleared; Safari's 7-day eviction for sites not added to the home screen |
| CLI | X25519 + Ed25519 | `~/.config/illogical/device.key`, 0600 | the file is deleted |
| Daemon | X25519 `noise`, Ed25519 `sign` | `<state>/daemon.key`, 0600 | the state directory is deleted (re-enroll) |
| Recovery code | Ed25519 seed | printed once at first sign-in, never stored | the user loses the paper |

S15 checked in Chrome 153 (desktop, headless) that:

- both key types generate as non-extractable (an export attempt throws);
- they survive a reload from IndexedDB;
- they work with `deriveBits` and `sign`.

Safari and Firefox support both curves (Safari 17+, Firefox 130+). The
phone runs below confirm Safari on iOS.

**WebKit loses X25519 keys in IndexedDB.** Playwright's WebKit
(Safari 26.6's engine, on Linux) stores a record holding an X25519
`CryptoKey` and reads it back as `null`, in the same page load or the next;
Ed25519 and AES-GCM keys come back fine. A Safari that enrolled therefore
made new keys on every later page load, and its approvals were signed by a
device nobody trusted. Now:

- **Keys are checked after they're stored:** read back from IndexedDB, they
  must sign what their public key verifies and agree on an X25519 secret,
  before the browser enrolls. The same check runs on every load.
- **Where the check fails, the keys are kept wrapped:** generated
  extractable, then stored as PKCS#8 encrypted with a non-extractable
  AES-GCM key (`wrapKey`), and unwrapped inside WebCrypto as
  non-extractable on each load. If that fails too, the page says this
  browser can't keep a device key.
- **What wrapping gives up:** script running in the page's origin (an XSS,
  a malicious build) can unwrap the keys as extractable and copy them out,
  to use anywhere until the device is removed. With plain non-extractable
  keys it can only use them while it runs in the page. On disk both forms
  are equally readable to someone with the browser profile.
- **A browser enrolled with keys it lost** says so at boot and offers to
  forget itself and enroll again, approved by another device or a recovery
  code.
- `/key-probe.html` (on control and on every daemon) runs the round trip
  in whatever browser opens it and says plainly whether it works, with the
  Safari version.

**A device certificate** is what control stores and hands out:

```
{ v: 1, account, device: <id>, name, kind: browser|cli|daemon|recovery,
  noise: <x25519 pub>, sign: <ed25519 pub>, created, team? }
  + approver: <device id>, sig: Ed25519(approver.sign, canonical JSON)
```

- The **first device** of an account is self-signed. It is trusted on
  first use, like an SSH host key.
- Every later device, daemon or recovery code is signed by an existing
  device. That is the "approve this device?" prompt, showing a fingerprint
  to compare (the first 8 bytes of SHA-256 of `noise‖sign`, as words).
- A daemon's own certificate is signed by the device that approved its
  `join` code. When the approver puts it in a team, that device also
  signs the choice (`illogical team join v1`: the daemon, the team, its
  founder). The daemon pins only a team signed that way.
- **A team's member list** (its roster) is versioned, and each version is
  signed by a device of an owner in the version before. The one exception
  is a **presigned invite**:
  - The owner's device signs `illogical team invite v1` (team, role,
    expiry, a one-time Ed25519 public key) when it makes the link. The
    private half of that key goes only in the link's `#fragment`
    (`#pinvite=<team>.<seed>`). Signing in with GitHub from the link keeps
    the seed in the tab's `sessionStorage` rather than in the `next` URL
    control sees, and pages from before presigned invites don't recognize
    the fragment, so they never send the seed as an invite code.
  - The invitee's device writes the next version (`v: 2`). It is the
    previous version plus the invitee at the end, with the invite listed
    as `spent`. The one-time key signs `illogical team redeem v1` over
    that exact member and version, and signs the whole roster too (`by` is
    the one-time key). No device of the invitee's signs it: devices are
    judged as of now, so revoking that device later would stop every later
    version from checking.
  - Daemons, control and browsers accept it only when all of these hold:
    the invite is signed by a device of an owner in the previous version;
    it isn't for an owner; it's unexpired at the version's `at`, and that
    `at` isn't before the previous version's; it isn't already spent; and
    nothing else changed.
  - Control sees the proof only when the invitee submits it. Because the
    proof names that account and root, control can't move it to an
    account of its own.
  - **Accepted risks:**
    - The link is a bearer token. Whoever holds it joins, which is why it
      lasts a day (control refuses longer), works once and never grants
      owner.
    - Expiry is checked against `at`, which the invitee writes. Control
      also refuses an expired invite by its own clock, but a daemon can't
      tell.
  - **Older daemons** refuse `v: 2`, and would then stop taking any later
    version of that team's roster, removals included. So every daemon
    tells control what it understands on each team call
    (`features=presigned-invites`), and control makes a presigned invite
    only when every daemon that checks the team's rosters has said so. That
    means the team's own machines, plus any machine that asked about the
    team in the last week because a session was shared with it. Otherwise
    the owner gets an *Ask me first* link and a note naming the machines to
    update. Control checks again when the invite is redeemed, since a
    machine may have started checking the team in between.
  - Once a team's history has a `v: 2` version, an older daemon can't
    follow it. Control won't put one into such a team (a join or *Move
    to…*; daemons also report their features when they ask to join and on
    every trust refresh). A daemon that was only shared a session with the
    team gets nothing for the team until it's updated: its members lose
    that session there, rather than members who were removed keeping it.

**Verification on the daemon:**

- The daemon keeps its account's chain (the certificates back to the
  first device) and caches it on disk.
- A client's Noise static key must belong to a certificate that chains to
  that root, isn't revoked, and has a role on the session it touches
  (M12's principals; role grants are signed the same way).
- Control serves the chain, but it can't extend it: it holds no `sign`
  private key.

**Revocation:**

- A revocation is a signed `{revoke: <device id>, at}` from any device of
  the account, or from a team owner's device for team grants.
- Control distributes revocations to daemons as it does certificates.
  Approving devices also push them straight to every daemon they can reach.
- **Accepted risk:** a malicious control can *withhold* a revocation from a
  daemon that no trusted device can reach directly. Control can't add
  readers, but it can delay removing one. As with Tailnet Lock, this is
  written down rather than solved.

**Recovery:**

- At first sign-in, the user gets two recovery codes. Each is an Ed25519
  seed, shown as words, whose certificate (`kind: recovery`) the first
  device signs.
- Any of the account's devices can make new ones. It signs their
  certificates and revokes every code still good, in one request; control
  refuses new codes that don't retire the old.
- A recovery code can sign exactly one certificate (a new first device),
  and daemons then treat it as spent.
- Control never sees the seed.

### PRF: re-deriving a browser's keys

A passkey created with the `prf` extension returns 32 bytes per salt, the
same every time for that credential. With a fixed salt
(`"illogical device key v1"`), those bytes are imported as an X25519 PKCS#8
seed, non-extractable (the spike does this). A second salt gives the
Ed25519 seed.

- If storage is cleared, signing in with the passkey restores the same
  device: no approval needed, and no new certificate.
- **A synced passkey is one device.** iCloud Keychain and Google Password
  Manager sync the credential, so every device sharing it derives the same
  keys. The certificate's `name` says so ("iCloud Keychain passkey"), and
  revoking it removes all of them. That is the same trust the passkey
  already carries as a login.
- Where PRF isn't available, losing storage means a new device key and an
  approval from another device. That is safe, just less convenient. **So
  PRF is an improvement, not a requirement:** M17 ships either way.

**Go/no-go per platform:**

| Platform | `extension:prf` | Same seed after reload | Verdict |
|---|---|---|---|
| Chrome 153, Linux (headless) | reported `true` | needs a real authenticator | pending a real device |
| Safari, iOS | pending phone run | | |
| Chrome, Android | pending phone run | | |

### Joining a machine: the account's fingerprint

The chain a daemon checks is only as good as the root it pins, and at
`join` that root comes from control. A control that wanted to read a
machine could approve it itself, into an account of its own (its own
"first device", signing the daemon's certificate), and every check above
would pass. So the daemon doesn't take the root on control's word:

- **The account's fingerprint** is its root device's id (the first 8 bytes
  of SHA-256 over its keys) in four groups, `1a2b-3c4d-5e6f-7a8b`: the same
  form as every device fingerprint.
- **The approving device shows it:** in the *Add a machine?* dialog, next to
  the code, and under *Devices and machines…*. A browser shows the root it
  pinned when it enrolled (from IndexedDB), not what control says now (when
  those differ it says so).
- **The machine shows it** once the approval arrives and checks out, and
  pins nothing until the person says they match:
  - `illogicald join` prints it and asks (`y`, or type the fingerprint);
    `--account FINGERPRINT` checks it without asking, for scripts;
  - Getting started (the daemon's own page) shows it with *They match* /
    *They don't*.
- **If they don't match,** nothing is saved: the daemon asks control to drop
  it (best effort) and makes a new key, so it can join again.
- **What this rests on:** the machine's side is the daemon's own output
  (terminal, or its local page), which control doesn't serve. The other
  side is the approving device, and it's only as trustworthy as that
  device's own pin (below) and the client code it's running
  ([What holds against control](#what-holds-against-control)).
- **Trust on first use, still:**
  - an account's first device is self-signed, and a browser enrolling as a
    later device takes the account's root from control. A browser approved
    by your other device checks its own certificate chains to that root, but
    a control that lied at that moment would have to keep lying to it;
    compare *Devices and machines…* across two devices to check;
  - a machine joined before 0.17.0 pinned whatever root control sent then.
    Its *Devices and machines…* fingerprint and `control.json`'s
    `trust.root` should be the same; if not, `illogicald leave` and join
    again.
- **Hosted sandboxes** skip the check: control creates the VM through
  its provider and writes the sandbox's `control.json` itself
  (`crates/control/src/sandboxes.rs`). The operator runs that machine, so it
  can read it whatever the keys say. A sandbox is end to end encrypted
  against the network, not against control.
- **Older daemons** (0.16.0 and before) join as they did, pinning without a
  check; control's API is unchanged, so they keep working with a newer
  control, and a new daemon works with an older control.

## What holds against control

Who can read your terminals, depending on what control (the hosted service,
or whoever runs yours) does:

| Control… | Your terminals |
|---|---|
| relays and stores honestly | only your devices and the people you share with read them |
| has its database or relay copied, or logs everything it sees | still unreadable: it holds certificates and ciphertext, never a private key |
| turns malicious **after** your machines and devices are set up, without changing the page it serves | can't add a reader: daemons check every device against the root they pinned. It can withhold revocations and refuse service |
| lies at **join** about which account a machine joins | caught by the account fingerprint, if you compare it |
| serves a **modified web client** | can read what that client shows and use its keys while it runs |

The last row is the important limit. The browser client at control's
address is JavaScript that control serves, and a page can't check its own
code. A modified page could send keys or plaintext anywhere. That applies
to:

- any browser or phone that opens control's page;
- **the desktop app once it's joined**: its window then loads
  control's page, signed in through your browser;
- not to a daemon's own page (`http://127.0.0.1:7681`, or its tailnet
  name) or the desktop app before it's joined: the daemon serves those from
  its own binary;
- not to `illogicald` or the CLI.

So the promise is end to end encryption against the relay and against
anyone who gets control's data, with the client code and the join trusted.
To avoid trusting control's page, reach your machines from their own pages
(over the tailnet, or `--direct-url`): a daemon's page talks to the daemon
it came from.

## Channels

**Pattern.** Noise `IK` with `25519`, `AESGCM` and `SHA256`.

- The client always knows the daemon's static key: it comes from the
  directory as part of the daemon's signed certificate. So IK applies, and
  attach takes one round trip:
  ```
  -> e, es, s, ss   + payload   (client static encrypted to the daemon)
  <- e, ee, se      + payload
  ```
- AES-GCM rather than ChaCha20-Poly1305, because WebCrypto has AES-GCM and
  not ChaCha. The browser then ships no crypto code: the whole initiator is
  [about 190 lines](archive/spikes:spikes/s15-control/web/noise.ts) over `crypto.subtle`.
- **Prologue:** `"illogical/1" ‖ daemon id ‖ session id or empty`. If the
  relay splices a client onto the wrong daemon, the handshake fails.
- **Message 1's payload** is encrypted to the daemon's static key only, so
  it is replayable. It may carry only idempotent requests (`hello`, and
  `attach` with offsets). It never carries input, method calls or intents.
  Message 2's payload carries the `hello` tree.

**Framing.**

- On WebSocket, one binary message is one Noise message.
- On a mux stream (the relay's splice), each Noise message is
  `len (u32 BE) ‖ bytes`.
- Plaintext starts with one byte: `0` means a whole protocol message
  follows (today's JSON or binary frame), and `1` means more follows. That
  covers snapshots over the Noise limit of 65,535 bytes. Senders chunk at
  16 KB.

**Costs, measured in S15:**

| | Result |
|---|---|
| Handshake bytes | 96 out and 48 back, plus payloads |
| Handshake CPU, both ends, Rust | about 370 µs |
| Handshake in Chrome, WebCrypto | 0.7 ms (key already loaded) to 6 ms (first use) |
| 1 MB burst | 0.098% wire overhead; 805 MB/s encrypt plus decrypt in Rust; 390–460 MB/s through Node's WebCrypto |
| Keystroke | 1 byte becomes 17 (plus framing); 0.4 µs |

Encrypting the tailnet path too costs nothing anyone will notice, and it
keeps one code path.

**Rekeying.** Noise's 64-bit counter never wraps on a terminal stream.
Channels are re-handshaken on every reconnect anyway.

## Shared sessions: per-viewer channels

The plan suggested a session key wrapped for each member device. S15
compared that with plain per-viewer channels, for 10 MB of output in 4 KB
writes:

| Viewers | Per-viewer: daemon CPU | Per-viewer: daemon uplink | Session key: CPU | Session key: uplink |
|---|---|---|---|---|
| 2 | 13 ms | 21 MB | 6.5 ms | 10.5 MB |
| 5 | 33 ms | 53 MB | 6.5 ms | 10.5 MB |
| 20 | 130 ms | 211 MB | 6.6 ms | 10.5 MB |

- **CPU doesn't matter** either way.
- **Uplink does, at scale.** With per-viewer channels the daemon sends one
  copy per viewer. A session key saves that only if the *relay* fans the
  one copy out, and that means:
  - the relay does per-viewer flow control (today it's per stream on the
    daemon);
  - snapshots and "from now" history still go per viewer;
  - revoking someone needs a key rotation that every remaining device
    acknowledges.
- **Decision: per-viewer channels.**
  - M19's target is small teams (2–5 people). Five viewers of a busy build
    log cost five times its output on the daemon's uplink, which a home
    connection carries.
  - Per-viewer channels mean one mechanism for direct, relayed and shared
    access, and revoking someone is instant: close the channel, refuse the
    key.

### Fan-out later, if ever

The trigger: relayed sessions regularly have more than 5 viewers, or a
daemon's uplink saturates on shared output. Then:

- add a group frame (output only, sealed with a per-session key and
  distributed through each viewer's channel);
- the relay fans group frames out;
- everything else stays per channel.

Nothing in this spec blocks it.

## The relay

Control's relay is M4c's dial-out transport with control at the home end:

- An enrolled daemon keeps one WebSocket to `wss://control/dial`,
  authenticated by a Noise handshake with its daemon key. (The spike
  trusts `?pub=`; the real one checks the key against the account's chain.)
- A client connects to `wss://control/c/<daemon id>`. Control checks that
  the account may reach that daemon (metadata it has anyway), opens a mux
  stream and splices the two. It forwards opaque Noise messages and counts
  bytes per account (M18's fair use, M22's meter).
- The mux is unchanged from M4c: per-stream credit, 256 KB windows, 16 KB
  frames.

**Measured** (spike relay on Fly, `shared-cpu-1x`, 256 MB):

| Path | Keystroke round trip p50 | 1 MB burst |
|---|---|---|
| Loopback, relay on the same machine | 36 µs | 1.1–1.8 ms |
| geek → Fly ord → geek | 51.6 ms | 208 ms |
| geek → Fly ewr → geek | 18.7 ms | 99 ms |
| geek → Fly ewr (echo only) | 9.1 ms | — |
| Phone on cellular → Fly → geek | pending phone run | |

- A relayed round trip is about the client↔relay round trip plus the
  relay↔daemon one. Compared with the direct path, the relay adds about
  one relay↔daemon round trip (minus whatever the triangle saves).
- **So the relay must run in the region nearest the daemon.** From geek,
  ewr adds about 9–19 ms; ord added 52 ms.
- **Hosted control is multi-region on Fly.** A daemon dials the anycast
  name and lands on its nearest machine, which holds its mux. A client may
  land elsewhere; that machine answers with `fly-replay:
  instance=<machine>` so Fly routes the WebSocket to the machine holding
  the mux. Self-hosted control is one process and needs none of this.
- **Capacity:** one relay process held 1,000 concurrent channels to one
  daemon. Each typed once a second, with p50 0.9 ms and p99 7 ms, 0
  failures, and 115 MB RSS on loopback. Streams are cheap; uplink
  bandwidth is the limit.
- **Cost:**
  - a `shared-cpu-1x` relay is about $2 a month and has headroom for
    thousands of channels;
  - egress is $0.02/GB in North America and Europe;
  - an active terminal user-hour is roughly 2–50 MB (typing, an agent's
    output, a few attaches), so **$0.0001–0.001 per active user-hour**.

  Relay traffic can be included in every plan; fair-use caps exist for
  abuse, not margin.

**Found and fixed on the way: Nagle.** `axum::serve` leaves Nagle on, and
the dial-out mux writes frames back to back (OPEN then DATA, DATA then
GRANT), so the second frame waited for the peer's delayed ACK. That was
40 ms on every request and keystroke through `/h/<name>/…` on the home
daemon today: 41.7 ms against 1.4 ms direct. The daemon now sets
`TCP_NODELAY` on every accepted connection, and relayed requests take
2.0 ms. Control's relay must do the same.

### Direct paths, and Chrome's Local Network Access

The client tries the tailnet or LAN URLs from the directory first, then the
relay. S15 found a wrinkle:

- **Chrome (153) blocks a public origin from connecting to a private
  address**, and that includes the tailnet's 100.64.0.0/10:
  `net::ERR_BLOCKED_BY_LOCAL_NETWORK_ACCESS_CHECKS`.
- A page served by control (`https://control.illogical.widgets.wtf`)
  therefore can't open `wss://geek.<tailnet>.ts.net` until the user grants
  the *local network access* permission. Granting it worked in the spike.
- **So M17's web client:**
  - asks for local network access the first time the directory lists a
    direct URL, with a line saying why ("connect straight to your machines
    when you're on the same network");
  - falls back to the relay when it's denied, or until it's granted;
  - doesn't need the permission when served by a daemon itself, as today:
    that page is on the tailnet already.

The host chip says "direct" or "relayed" either way.

## Read-only links

- A link is `https://control/s/<link id>#k=<base64url X25519 private key>`.
- Making one: the sharing device generates the key pair. Its public key
  goes to the daemon as a **link principal** (viewer of one session,
  expiry, "from now" by default), signed by the sharer's device.
- Opening one: the page reads the fragment, uses it as its Noise static key
  and connects through the relay. The daemon finds the key among its link
  principals and serves that session read-only.
- Revoking or expiring the link removes the principal and closes its
  channels.
- **Control sees** that the link id was opened, never the key or the
  content. Fragments are never sent in requests.
- **Link previews:** an unfurler that ran the page's script would see the
  fragment. S15's share page reports what a script-running fetcher saw:

  | Previewer | Fetched the page | Ran its script (saw the fragment) |
  |---|---|---|
  | Slack | pending | pending |
  | iMessage | pending | pending |

  Whatever the results, the share page also refuses to connect until it has
  focus and a user gesture ("Open the live view"). A headless previewer
  never clicks.

## Push

RFC 8291 encryption needs only the subscription's public parts
(`p256dh`, `auth`). Decrypting needs the browser's private key, which never
leaves it. So:

- **Subscribing:**
  - devices subscribe once, with control's VAPID public key as the
    `applicationServerKey`;
  - the device signs `{endpoint, p256dh, auth}` with its `sign` key, and
    control stores and distributes that.
  - The signature matters: otherwise control could swap in a `p256dh` of
    its own and read notifications.
- **Sending:**
  - the daemon checks the signature and encrypts the payload (today's
    `push::encrypt`);
  - it hands control `{endpoint, ciphertext, ttl, urgency}` through the
    dial-out socket;
  - control signs the VAPID JWT and posts it.
- **What control and the push service see:** endpoint, size and timing.
  Control logs "daemon X notified device Y".
- **Control's own notices:** a new device waiting for the account, or
  someone asking to join a team (to its owners). Control encrypts these
  itself. They say only that something waits ("A new browser wants into
  your account", "Ada asks to join Acme"); approving still happens on the
  page, with the fingerprint.

This is confirmed by construction: the daemon's existing RFC 8291 encrypt
plus a separate VAPID signer is exactly what the RFC allows. M21 adds the
handoff.

## Identity

Signing in proves who the account is. It doesn't make a device trusted:
only an approval does.

- **M17:** GitHub OAuth, and passkeys as a first-class login (the same
  passkey can supply PRF).
- **Later:** Google OAuth, and email magic links for invitees.
- Control's session cookie is for control's API only (directory, approvals,
  invites). Daemons never accept it.

## What control stores

| Stored | Never stored |
|---|---|
| accounts and their display names, OAuth ids, passkey public keys | Noise or signing private keys of anything |
| device and daemon certificates, revocations, signed grants | terminal bytes, snapshots, logs, history |
| directory: daemon ids, names, URLs, last seen, presence | link keys (the fragment) |
| connection metadata: who, which daemon, when, byte counts | push payloads in the clear |
| signed push subscriptions, VAPID key pair | provider tokens for your own machines |

Logs follow the same rule: nothing a daemon sends inside a stream is
logged.

## Open items

These are pending the phone runs (see the spike README):

- PRF on Safari iOS and Chrome Android (the go/no-go above);
- the cellular round trip through the relay, against M18's 30 ms budget;
- link-preview behaviour in Slack and iMessage.
