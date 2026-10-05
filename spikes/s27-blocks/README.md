# S27: blocks through control, end to end

Spike for #148 (PLAN.md, "No special machines track", S27). Run on
2026-10-05 on an Apple M5 Pro (18 cores, macOS 26.6) under a shared,
heavily loaded machine; the load average is next to every number. Each
measurement was taken once.

**Verdict: go for M50**, with the changes listed under "What M50 should
take from this". Chromium and both WebKit builds tested (macOS and Linux)
pass every functional check: Vite, Next and code-server (webviews
included) work inside control's page through the block's worker, with
control relaying only ciphertext. Real Safari (macOS and iOS) wasn't run
here; the test for it is in `safari/` and needs Track E's VM.

## What was built

One binary, `s27`, with two halves, and control's static files for block
origins (`web/`, bundled into `dist/` by `build.mjs`):

- **`s27 control`** stands in for control. On `https://control.test` it
  serves a stand-in for control's page (`web/parent.ts`). On
  `https://b-<key>.blocks.test` (a registrable domain of its own, so every
  block is cross-site to control's page) it serves only its own files: a
  bootstrap page for any navigation the worker didn't answer, the worker
  (`/.s27/sw.js`) and the WebSocket shim (`/.s27/shim.js`).
  `/.s27/relay?d=<daemon>` on the block's own origin splices the
  browser's socket to a stream on that daemon's dial-out socket
  (`illogical_e2e::mux`, `len ‖ Noise message` frames), as `/api/relay/c`
  does. It counts what it relays and looks in every byte for a marker the
  test pages are full of.
- **`s27 daemon`** is the daemon's end: it dials control's relay and is
  the Noise responder (`illogical_e2e::channel`, the product's
  `Noise_IK_25519_AESGCM_SHA256`) for each block's channel, then proxies to
  the block's port the way `sites.rs` does (Host, Origin and Referer
  rewritten to `localhost:<port>`, hop-by-hop and `X-Frame-Options`
  dropped, redirects kept on the block). It also serves `wss://daemon.test/e2e`
  (the direct path) and `https://b-<block>.direct.test`, today's block
  site, as the baseline.
- **The parent page** holds an Ed25519 device key (not extractable). For
  each block it makes a one-off X25519 key and signs a grant: daemon id,
  block id, the key, an expiry (`src/wire.rs`). It hands key and grant to
  the frame by `postMessage`, to the frame's origin only, and answers only
  the frame it made, only for that frame's block.
- **The bootstrap page** registers the worker, asks the parent for
  credentials, gives them to the worker and loads the page again, now
  through the worker.
- **The worker** answers every request on the block's origin over the
  block's channel, opening it on demand. The grant is the first transport
  message; the daemon checks it (a device it trusts, this daemon, a block
  it has, the handshake's key, not expired by its clock) before anything
  else, and answers `ok` or `refused`. An expired grant makes the worker
  ask its pages, which ask the parent, for a new one.
- **The shim**, first in every HTML page (the worker puts it there), sends
  WebSockets to the block's own host over a channel the page opens itself.

Locally, `*.test` names reach 127.0.0.1 through a CONNECT proxy the
harness runs on 7753 (both browsers take a proxy; no `/etc/hosts`, no
system DNS). The certificate is self-signed for every name; Chromium trusts
it by key hash, WebKit by `ignoreHTTPSErrors`. Each frame's first
navigation reached control as `Sec-Fetch-Site: cross-site`, so the
separate site is real.

How to run it: [docs/testing.md](../../docs/testing.md#spike-blocks-through-control-s27).

## Answers

### Does it work, per browser?

| Check | Chromium (Playwright, macOS) | WebKit (Playwright, macOS) | WebKit (Playwright, Linux) | Real Safari / iOS |
|---|---|---|---|---|
| Worker registers in a cross-site frame | yes | yes | yes | not run (`safari/`) |
| Page, POST, redirect, 3 MB download through it | yes | yes | yes | not run |
| WebSocket through the shim | yes | yes | yes | not run |
| Control saw no plaintext (marker hits) | 0 | 0 | 0 | |
| Vite: CSS hot update, JS full reload | yes | yes | yes | not run |
| Next 16 (Turbopack): Fast Refresh, no reload | yes | yes | yes | |
| code-server 4.140.0: workbench, file, Markdown preview webview | yes | yes | skipped (macOS binary) | |
| Worker straight to the daemon (no relay) | yes | **no** (see below) | not run | |

Linux WebKit is the engine family the desktop app's WebKitGTK webview
uses, but Playwright's build is newer than the WebKitGTK in Ubuntu 22.04
that the desktop app links; see "What still needs a real machine".

### Vite, Next and code-server

All three work with no change to the dev server and no flags.

- **Vite 8**: its client opens `wss://<block>/?token=…`, which the shim
  carries. A CSS save was a hot update (no reload: a value set on `window`
  survived); a save to `main.js` (no `import.meta.hot`) reloaded the page.
  Through the block 80–180 ms from save to visible, the same as today's
  path (load 10–17).
- **Next 16 (Turbopack)**: `/_next/webpack-hmr` through the shim; an edit
  to `app/page.jsx` showed with Fast Refresh, no reload, 30–80 ms (load
  10–18). Next's dev-origin check passes because the daemon rewrites
  `Origin`, as `sites.rs` does.
- **code-server** (the pinned release editor blocks run): the workbench,
  its remote connection (a WebSocket, through the shim), a file opened from
  the explorer, and a Markdown preview, which is a webview. The webview
  needed one addition, below.

**Apps that register a service worker of their own.** VS Code registers
two (its PWA's and the webview's, `…/webview/browser/pre/service-worker.js`).
A worker script's fetch never reaches a worker: it went to control, which
doesn't have it, and the webview stayed blank. Control now answers any
request carrying `Service-Worker: script` with
`self.S27_APP_SW="<path>"; importScripts("/.s27/sw.js")`. That worker
fetches the app's script through the channel and runs it inside itself
(`new Function`), with an `addEventListener` that collects the app's
handlers; it offers each fetch to them first and carries the rest over the
channel. VS Code's webview worker ran unchanged this way in both
browsers. It depends on control's worker being allowed to `eval` (no CSP
on it), and an app's worker gets the block's worker's scope, not its own
global; a worker that does more than VS Code's might need more of the
global passed through.

### Latency added per request

Loopback, so this is the cost of the path, not of a network. 300
sequential `GET`s of 2 bytes from inside the block's page, then 200 at
once, a 10 MB download, and first load (frame created to the page's
DOMContentLoaded, through the worker after the bootstrap's reload).

| | p50 | p90 | p99 | 200 at once | 10 MB | first load |
|---|---|---|---|---|---|---|
| Chromium, relayed (load 16.8) | 0.7 ms | 0.9 | 1.1 | 37 ms | 140 MB/s | 27 ms |
| Chromium, worker straight to daemon | 0.5 | 0.7 | 0.8 | 32 | 145 | 20 |
| Chromium, today's block site | 0.5 | 0.6 | 0.7 | 33 | 909 | 8 |
| WebKit macOS, relayed (load 17.6) | 1 | 2 | 2 | 66 | 147 | 42 |
| WebKit macOS, today's block site | 1 | 2 | 2 | 83 | 833 | 10 |
| WebKit Linux, relayed (container) | 2 | 3 | 4 | 106 | 92 | 47 |
| WebKit Linux, today's block site | 1 | 2 | 2 | 98 | 333 | 37 |

(WebKit's `performance.now()` is coarse, 1 ms. The Linux numbers are from
Docker's VM, 12 cores, whose own load was 1.4; the Mac's load at the time
isn't comparable.)

- Per request it adds about 0.2 ms in Chromium and about 1 ms in WebKit
  on loopback. On a real network the relay costs one more hop: the
  browser to control to the daemon instead of the browser to the daemon.
  A dev server's page is many requests in parallel, which share one
  channel, so it pays that round trip per dependency level, as the direct
  path does.
- Bulk throughput tops out at about 140 MB/s in both browsers (Noise in
  WebCrypto, one AES-GCM call per 16 KB chunk, in order), against
  300–900 MB/s for today's path. A dev server's bundles are a few MB, so
  that's tens of ms; a large download in a block would notice.
- First load costs one extra round trip for the bootstrap page and the
  worker's install: 20–40 ms here.
- Through Linux WebKit the first run showed 43 ms per relayed request:
  the spike's daemon dialed the relay without `TCP_NODELAY`, so small
  frames waited on delayed ACKs. Fixed; the product's relay dial
  (`dial.rs`) and control's listener already set it.

### Hard reload, a crashed worker, idle, a block left open

| Case | Chromium | WebKit macOS | WebKit Linux |
|---|---|---|---|
| Control's page reloaded, block on the same origin | through the worker, no bootstrap | same | same |
| Hard reload of control's page (CDP `ignoreCache`) | through the worker, no bootstrap | no API | no API |
| Worker stopped (CDP `stopAllWorkers`) | next request restarts it from IndexedDB, 7 ms, reconnect 3 ms; the shim's socket unaffected | no API | no API |
| Worker unregistered, then also its storage deleted | the block keeps working: through the bootstrap page and the parent, or the old worker answers the reload (it goes once no page uses it) | same | same |
| 45 s idle | no restart (DevTools keeps it alive), 4 ms | not restarted, **first request 1 s**, then 1 ms; today's path 5 ms | 5 ms |
| Grants of 3 s, block open 16 s, channels dropped 4 times | 4 renewals through the parent; first request after each drop 4–8 ms; open sockets outlive their grant | same, 9–54 ms | same, 11–29 ms |
| Control restarted under an open block | daemon back in 0.4 s; sockets close (as for a dev server restart), new ones work | same | same |
| Parent page's clock 25 h behind | grants arrive expired, block doesn't load | same | same |

- **A block left open for a day** comes down to the rows above: the
  browser stops the worker when it's idle, channels drop when control
  deploys or the daemon restarts, and grants expire. Open channels outlive
  their grant (expiry gates new channels only), and a new channel asks the
  parent for a new grant. Simulated with 3 s grants and forced drops,
  then the clock skew row; not run for a day.
- **WebKit's idle wake-up**: after about 45 s with no events, the first
  request through the worker took about 1 s (no restart, no reconnect),
  then 1 ms. At 5 s and 15 s idle there was no penalty. Today's path has
  none. Worth checking in real Safari.
- **Playwright's Chromium never stops an idle worker** (it attaches
  DevTools to workers), so the idle row doesn't show Chrome's 30 s stop;
  the crashed-worker row is the same path.
- **Clock skew**: the parent mints the expiry from its own clock and the
  daemon checks it against its own. A device a day off can't open blocks.
  M50 should take the daemon's time from the device's channel to it (the
  page already has one) rather than trust the page's clock; letting the
  block's frame report the daemon's time would let block code extend its
  own grant.

### Cookies

A response a worker builds can't set cookies, and requests made from a
worker don't carry them. The worker keeps the block's cookie jar itself
(IndexedDB), sends it with every request and stores what responses set, so
a server's session cookie works (`basics`). The page's `document.cookie`
doesn't see those cookies (as if all were HttpOnly), and cookies the page
sets with `document.cookie` don't reach the server. In a cross-site frame
Safari blocks third-party cookies anyway, so the jar is the better
behaviour there. A dev server whose client JS reads its own cookies would
break; none of the three tried does.

## Trust: what a malicious control could do

Today, control serves its own page, which holds the device key in the
browser (non-extractable). A malicious control can already serve that page
code that uses the key: open terminals, read panes, approve devices, for as
long as the page is open. Blocks are reached directly (tailnet) and
control isn't in their path.

With S27, control also serves the bootstrap page, the worker and the shim
on every block origin, and relays the block's traffic. What changes:

- **It can read and change what blocks show**, by serving a worker that
  sends the decrypted traffic somewhere, or by keeping a block's key. That
  is no new power: its page code could already mint grants with the device
  key, or simply drive a terminal to read the dev server's files. The
  party is the same; the surface is larger, and worker code persists in
  the browser between visits (until its next update check, which goes to
  control).
- **What it learns without being malicious**: the block hostnames
  (`b-<key>`, random per block), when and how much each block sends, and
  which daemon. Not paths, bodies or the block's id (that's in the
  encrypted grant). Today, for tailnet blocks, it learns nothing.
- **What it can't do**: read the traffic it relays (the check above), or
  open a channel itself: a grant needs a device key the daemon trusts, and
  control holds none.

What block code (the dev server's, written by an agent) could do, against
today:

- It runs on its own origin as today. It can read the block's key and
  grant (they're in the origin's IndexedDB) and open its own channel, but
  that reaches only its own port, which it can already reach.
- It can ask the parent for a new grant, and gets one for its own block
  only; nothing in its message is used. It can't reach other blocks,
  daemons or the device key.
- It can unregister the worker or register its own: its script goes
  through control's wrapper and runs inside control's worker, in the
  block's own origin, as it would anyway.
- A grant copied out keeps working until it expires (an hour in the
  spike), from anywhere that can reach control's relay. Grants should be
  short and renewed, and the daemon should forget a block's grants when
  the block closes.
- Opening a block's URL top-level gets another storage partition (the
  frame's is keyed to control's site), so no worker, no key, and a
  bootstrap page with no parent to ask.

## What M50 should take from this

- **The grant in Noise message 1's payload.** The spike sends it as the
  first transport message because `illogical_e2e::channel::Responder::read`
  drops message 1's payload; returning it is a small change and saves the
  round trip for the `ok`. A replayed message 1 can't finish a handshake
  without the block's private key.
- **WebSockets on the page's own channel**, not through the worker: a
  worker is stopped when idle, and a hot-reload socket is idle for minutes.
- **Hosting apps' own workers** in control's worker, as above.
- **`frame-ancestors 'self' <control>`** on block pages: VS Code frames its
  own origin (the webview), as `sites.rs` already allows.
- **A stable block key**, kept with the block, so a reload finds the
  worker and storage it had; the spike's reload rows rely on it.
- **Expiry against the daemon's clock**, from the device channel.
- **Read a refusal that arrives with the close**: the spike first reported
  an expired grant as "closed during the handshake" when the close beat it
  through the receive queue, and never renewed (seen in WebKit).
- **The daemon's head script still works**: HTML comes through the
  daemon's proxy, so `set_head_script` (editor storage, #69) applies as
  today; the shim is the worker's to add.
- **Request bodies are buffered** in the worker (a fetch event's body is
  read whole); responses stream. Fine for dev servers.
- **The direct path from the worker**: in Playwright's WebKit a worker's
  WebSocket to another host than its own failed (the page's own to the
  same host worked). That may be the test certificate rather than WebKit;
  real Safari settles it. Until then, M50 can use the relay in WebKit.

## What still needs a real machine

These are not person steps; they need hardware or a VM that this run
didn't have. The PLAN's demo on jake-mini, geek's Chrome and the phone is
replaced by the automated runs above plus these:

- **Safari on macOS and iOS**: `node safari/safari.ts` (and `--ios`, the
  Simulator) on a Mac with `safaridriver --enable`, the test certificate
  trusted and three `/etc/hosts` names (`--print-setup` lists them; all
  need sudo, so Track E's tart VM). It prints a JSON verdict. Checked here
  with `--driver playwright-webkit`.
- **The desktop app's WebKitGTK**: Linux Playwright WebKit passes, but the
  app links Ubuntu 22.04's WebKitGTK, which is older. The Xvfb desktop
  image (#203) could load control's stand-in page in the real webview.
- **A real network**: every number here is loopback. The relay's cost is
  one more round trip to control per request level; measuring it needs
  control somewhere else (or `tc netem` in the testnet).
