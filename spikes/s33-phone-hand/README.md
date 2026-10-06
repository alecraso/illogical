# S33: the phone as a hand
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s33-phone-hand/<file>`.

Can an agent in a pane use the phone: its location, its camera, its mic, the person holding it? #268 asked this for native apps first, with a push to wake the phone. Two things changed the plan on the day (2026-10-05):

- **There's no Apple Developer Program membership yet**, so no APNs key: native pushes, silent or not, can't be sent. Jake chose Web Push "for now".
- Then Jake narrowed it further: **first find out how useful the phone is while the page is open**, and leave waking it for another time.

So this round tests the **web page as the hand**, on the iPhone 15 Pro (iOS 26.6.1, Safari, in a tab, not installed). Android, the installed PWA, waking by Web Push, and a native shell are still to do (see the end).

## What was built

All on branch `s33-phone-hand`; it's a prototype, but written to keep.

- **Protocol** (`crates/proto`): `ClientMsg::Hand { tools, name }` (a client lends tools; none: it stops), `ServerMsg::HandCall { id, tool, args, from }`, and `ClientMsg::HandReply { id, result | error }`. The first request the daemon sends to one particular client and waits for.
- **Daemon** (`crates/daemon/src/hand.rs`): which connected clients lend what, the calls waiting on each (a client that leaves fails its calls at once), files for results (`{"image" | "audio": {data, mime}}` is written to `<state>/hand/` and replaced by its path), and a wake path: a call to a device that isn't connected pushes a notification to that device through control (`Control::push_device`) and waits up to 2 minutes for it to come back. Only owners' clients may lend. Devices over control's channel are remembered in `hands.json` by device id.
- **MCP** (`mcp/tools.rs`): `list_devices` and `device_call { tool, args, device?, timeout? }`, in illogical's own server rather than one server per phone: the agent already has illogical's tools, and one call can reach any of the user's devices. The wait sends progress every 15 s like `wait` does.
- **Web** (`web/src/hand.ts`): with lending on, a summary connection to each of the account's machines (or, without control, the daemon that served the page) offers seven tools. Each call shows a card: **Deny / Allow**, plus **Allow for 15 min** for the tools that don't need the person. The camera, the file picker and the mic start inside the Allow tap, because Safari opens them only from a gesture. The switch is "Lend this device to agents" in Account, or `#lend=on` in the address.
- **Test**: `crates/daemon/tests/hand.rs`, a fake phone on `/ws` against the real daemon through `illogical mcp`: list, a location, a photo that lands as a file, a tool it lacks, a denial, and the phone leaving while a call waits.
- **Probe**: `call.py SOCKET TOOL ARGS` calls a tool as an agent would and logs it to `results/calls.jsonl`.

## How it was measured

A dev daemon on geek (`--state-dir` in the scratchpad, `tailscale serve --https=10443`), the page on the iPhone over the tailnet (direct path, 18 ms ping), and `call.py` on geek as the agent. Times are from the agent's call to its answer, so they include the person reading the card and tapping.

## Answers

### What's callable from the page, on iOS Safari

| Tool | Worked | Time (call to answer) | Notes |
|---|---|---|---|
| `location` | yes | 7.8 s (first call: card + iOS permission) | 8.7 m accuracy, altitude; no heading/speed while still |
| `take_photo` | yes | 10.7 s | iOS's own camera opens from the Allow tap (`<input capture>`). 12 MP original (3024x4032, 1.8 MB) scaled on the phone to 1200x1600, 483 KB JPEG, saved on geek |
| `ask` (with choices) | yes | 14.9 s | the person as a tool; free-text answers work the same way |
| `record_audio` (5 s) | yes | 8.4 s | `getUserMedia` + `MediaRecorder` after the mic permission: AAC in MP4, 48 kHz mono, 114 KB |
| `device_info` | yes | 11.0 s with the card, **0.73–2.1 s** with a standing grant | iOS gives no battery and no network type |
| `pick_photo` | not run | | same path as `take_photo` without `capture` |
| `read_clipboard` | not run | | Safari shows its own "Paste" bubble too |

Not reachable from a web page on iOS at all: contacts, calendar, reminders, HealthKit, NFC, Shortcuts / App Intents, the on-device model, background location. Those need the native app (below).

**Unexplained:** with no card at all, `device_info` takes ~730 ms on the phone's side, while ping is 18 ms. Likely Wi-Fi power saving on an idle phone, or Safari's event loop; it needs a no-op tool timed in a loop to tell.

### Locked and unlocked

- **Locking ends the hand within seconds.** 20 s after Jake locked the phone, its socket was already closed and the daemon had dropped it: a call then fails at once ("no device has offered tools here"), it doesn't hang until its timeout. A call waiting when the phone goes away fails as soon as the socket closes (the test covers this).
- **Unlocking brings it back by itself**: the page reconnected and lent its tools again within seconds, as a new client.
- **The 15-minute grant didn't survive the lock.** The next call showed a card again (8 s). Grants live in the page's memory, and iOS reloaded or discarded the page. They should be kept per device (and shown, with a way to take them back).
- Over the tailnet the device has no stable id (`client-N`), so a locked phone vanishes from `list_devices`. Over control's channel it has its device id, is remembered, and is the one a wake push goes to.

### Consent

What worked in the hand: **every call is a card on the device**, saying who asks (`<MCP client> on <machine>`) and what for (the agent's own words: "take a photo: “anything on your desk”"). Tools that need the person (camera, mic, a question, the clipboard) are asked every time, since the person acts anyway; tools that don't (location, device info) can be allowed for 15 minutes, per tool and per caller. Only the owner's clients lend, and only the owner's agents call (the MCP server is the owner's).

Still to decide: grants that persist (per device, scoped to machine + agent + tool, with an end), a list of them to revoke, and whether teammates' agents may ever call (default no).

### Transport

The client's existing connection carries it: no new protocol, no new port. A hand is a client that says what it lends. A 483 KB photo went as base64 in one message (control's channel allows 64 MiB); that's fine for photos and short recordings, and the Images track (#267) is where large files should go instead.

### MCP and A2A

Two tools in illogical's own server work well for an agent: `list_devices` tells it what exists (with each tool's schema), and `device_call` reads naturally ("use a tool on the user's device"). A server per phone would mean configuring each agent per device. **A2A wasn't tried**, and nothing here needs it: waiting for a person is a long MCP call with progress, and that held up to the 15 s it took a person to answer.

### Waking a phone that isn't open

**Not measured.** The daemon side exists (a push to that device through control, then a wait for it to come back), but it needs the page from control (the tailnet page has no device id), and on iOS only an installed PWA receives Web Push, always as a visible notification. Silent pushes, Live Activities and background location need the native app and the developer membership.

### App Store

Not reached: the page needs no review. It matters once there's a native shell (2.5.2: features driven by remote agents; the privacy labels would list location, photos, audio and user content).

## What a native iOS app could add

Not built or measured in this round; this is what the platform allows, written down so the next round starts from it. Whether each capability works under free signing is from memory and has to be checked in Xcode.

### When native code may run

S31 measured that an app is suspended about a second after it leaves the screen. Native code runs only:

| When | For how long | Needs the Developer Program? |
|---|---|---|
| The app is in front | as long as it's open | no |
| Background location updates ("Always" + the location background mode) | kept running while it tracks, network included | no for a build sideloaded to your own phone; App Store review would question it |
| The audio background mode (playing or recording) | kept running | as above |
| A silent push | about 30 s, throttled, not guaranteed | yes (APNs) |
| A notification's action button (Allow / Deny on the lock screen) | a few seconds in the background | the notification has to arrive: local, or a push |
| A Notification Service Extension | about 30 s per visible push; it can decrypt the payload, so alerts stay end to end | yes |
| A VoIP push (PushKit) | wakes it reliably, but must show a CallKit call | yes |
| HealthKit background delivery, region monitoring, significant location changes | relaunched by the system on the event | HealthKit's capability |
| `BGTaskScheduler` | when the system decides (minutes to hours) | no |

**Background location is the one that matters without a membership.** It needs no push. A sideloaded app could keep its connection to control open while the phone is locked, at a cost in battery, which would make the phone callable while locked without APNs.

### Tools

**Without the person, once permission is granted** (whenever the app runs, including kept alive in the background):

- location, including regions ("tell the agent when I'm home") and visits;
- motion: steps, activity (walking, driving, still), altimeter, and 7 days of motion history;
- Contacts, Calendar and Reminders, read and write;
- HealthKit: steps, heart rate, sleep, workouts, and what the Watch records;
- Focus status, battery, network and thermal state;
- on-device intelligence on the iPhone 15 Pro (iOS 26): Foundation Models (Apple's on-device model), speech to text, Vision (text, barcodes, image classification) and Translation, so an agent can have the phone transcribe or summarize without the data leaving it;
- the photo library with full access: search by date and place, metadata, Vision over photos the person didn't pick;
- HomeKit (read and control accessories), Bluetooth devices, UWB (Nearby Interaction).

**With the person, in front:**

- the camera, all of it: which lens, video, LiDAR depth, ARKit room scans and measurements, the document scanner;
- NFC tags (with the system sheet);
- compose sheets for messages, mail and calls, which the person sends;
- the clipboard (iOS asks unless there's a paste button), files through the document picker;
- **Shortcuts**: `shortcuts://x-callback-url/run-shortcut?name=…` runs any Shortcut the person built and returns its output, so every Shortcut is a tool, including actions iOS never gives apps (it opens Shortcuts in front).

**The other way, the phone starting things:**

- App Intents: Siri, Shortcuts and personal automations ("when I arrive home", "when I tap this tag") call the app in the background, which can start or message an agent;
- Live Activities: an agent's progress on the lock screen and in the Dynamic Island (updated remotely only through APNs; locally only while the app runs);
- the Apple Watch through WatchConnectivity: a haptic on the wrist, heart rate.

**Never, on iOS:** reading messages, the call log, mail, Safari history or other apps' notifications or data; taking a photo or recording in the background (the indicator shows, and background capture is refused); running arbitrary code when it likes; downloading new native code (S31).

### Without a membership

A free Apple ID installs to your own phone through Xcode, but the app stops working after 7 days until it's signed again (a script on jake-air could do it), and there's no push at all. Location, camera, Contacts, Calendar, motion, speech, Vision and Foundation Models should all work that way; HealthKit, HomeKit and NFC may need the paid program.

## Recommendation

**The open page is already a useful hand, and nothing in it needs an app.** Location, a photo, a recording and a question all reached an agent on geek in 8–15 s with the person in the loop, and the photo arrived as a file the agent could read. That's worth shipping as a milestone on its own:

1. **M-hand: lend this device** (web, both phones): the code on this branch, plus grants that persist and can be revoked, the switch in a place people find, and a tool list that matches what the browser can do. Done when: an agent on one machine gets a photo and a location from the phone, through control's page (not the tailnet one), on iPhone and Android.
2. **M-wake: call a phone that isn't open**: Web Push to the installed PWA (the daemon side is written), then measure latency and how often it's dropped, locked and unlocked, on both platforms.
3. **A native spike, within free signing**: a thin Swift app around the same page (WKWebView and a JS bridge, `window.webkit.messageHandlers`, the way M48 wraps control on the desktop), so consent and the protocol stay in the page, adding Contacts, Calendar and Reminders, motion, Vision, speech to text and Foundation Models. Its real question: **does background location keep the hand reachable with the phone locked, and what does it cost in battery?** If it does, the phone is callable while locked without APNs.
4. **Native for everyone, on a trigger**: the membership, push, and the App Store story, once a native-only tool is wanted by people other than Jake.

## Still open in this spike

- Android (Chrome): the same run, plus the installed PWA and Web Push there.
- `pick_photo` and `read_clipboard` on the iPhone.
- The ~730 ms floor on a granted call.
- The page from control (e2e, with a device id) rather than the tailnet one: needs control deployed with this web build, or a local control.
- Everything under "What a native iOS app could add": written from the platform's documentation and S31, not measured.
