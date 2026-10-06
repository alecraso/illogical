# S22: studio apps as panes (framing, hud's questions as illogical asks)
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s22-apps/<file>`.

Run 2026-10-02 on geek, against a real studio box: a clone of `arugula-box-template` on geek's
wispd (`s21-app`, made before this spike was renumbered from S21), with hud, the door, the app and the
steward running as studio makes them. **Result: go. A studio app works as an illogical block, and its
agent's questions work as illogical asks, both ways, once two small changes are made: the door hands
out a partitioned cookie, and the daemon's ask route takes browser blocks.**

- **Framing: works, with a partitioned cookie.** illogical's page (`geek.<tailnet>.ts.net`) and
  every block (`b-N.illogical.widgets.wtf`, or a box's own `app-x.studio.arugula.io`) are different
  sites, so a block's frame is always third party. hud's session cookie is `SameSite=Lax`, so a
  box framed from another site never logs in: the entry link's redirects run, the cookie is refused,
  and the frame lands on "You need a link to get in" (401). The same cookie as `SameSite=None;
  Secure; Partitioned` logs in and hud's API answers 200 in the frame:

  | | Chrome | Firefox |
  |---|---|---|
  | stock (`Lax`), framed from another site | 401 | 401 |
  | stock (`Lax`), framed from the box's own site | 200 | 200 |
  | `None; Partitioned`, framed from another site | **200** | **200** |
  | `None; Partitioned`, framed from the box's own site | 200 | 200 |
  | stock, Chrome blocking third-party cookies, from another site | 401 | |
  | `None; Partitioned`, Chrome blocking third-party cookies, from another site | **200** | |
  | stock, in illogical's own client (`illogical open <entry link>`) | 401 | 401 |
  | `None; Partitioned`, in illogical's own client | **200** | **200** |

  Chrome keys the cookie to the top-level site (`partitionKey: https://parent.example`), so it is
  sent only to frames under illogical's page, never to the box opened on its own or under any other
  site. (`frame.mjs`, `app.mjs`; `results/frame.jsonl`, `results/*-chrome.png`, `results/*-firefox.png`.)
  WebKit wasn't run: Playwright's WebKit won't install on geek (missing host libraries).
  "Blocking third-party cookies" is Chrome's setting (and its Incognito default), set as
  `editors.spec.ts` sets it for #69. The partitioned cookie gets through, as CHIPS intends, but the
  frame's `localStorage` is refused there (#69's finding), so anything in the box's page that
  needs storage needs #69's head script or its own fallback.
- **hud's panel never mounted, framed or not.** hud's proxy injects `/hud-config.js` and
  `/hud-client.js` at the top of `<head>`, and the client throws `Cannot read properties of null
  (reading 'appendChild')` before `<body>` exists, so there's no `<hud-root>`. The template's page
  does the same opened on its own, so it's a box or hud bug, not framing. It does mean the panel
  under refused storage is untested. hud's API and chat, which everything below uses, work.
- **A plain web-page block is enough.** The box is already its own origin, so the per-block
  origin and proxy that M6a gives a port add nothing here. `illogical open <entry link>` framed it
  directly, sandbox `allow-scripts allow-forms allow-same-origin allow-popups`, and the app and hud's
  API worked in it. Nothing in the door or hud refuses framing (hud#642 already strips framing headers).
- **hud's questions → illogical asks: works, 11 ms.** When a turn waits on a question, hud
  sends it in every `hud-chat-queue` frame (`question`: `requestId`, `summary`, options, a
  five-minute `expiresAt`), including the first frame a new subscriber gets, so a follower needs no
  history. `bridge.mjs` follows `/__hud/api/chat/stream` for each of the box's tabs (`/__hud/api/tabs`)
  and puts each question to illogical as AskUserQuestion's card (`POST /api/panes/<id>/ask`). It
  became an M24 `ask` reason (`illogical attention`, the pane's card, the swarm's needs-you rail,
  push) **11 ms** after hud asked, on all four questions measured.
- **Answers go both ways.** An answer in illogical (the act route, or clicking **Blue** on the
  swarm's rail in Chrome) reached hud as `POST /__hud/api/chat/answer` in **48–74 ms**. hud
  answered 200 and the agent's turn went on with the `tool_result`. An answer in hud's own panel
  withdrew illogical's card **33 ms** later (the question leaves the queue frame), and so did an
  interrupt. (`demo.mjs`; `results/bridge.jsonl`, `results/tab-with-question.png`,
  `results/swarm-with-question.png`, `results/swarm-answered.png`.)

## What has to change

- **studio's door: a framed mode.** Hand hud's session (and the door's own `box_next`) out as
  `SameSite=None; Secure; Partitioned` (`door-patch.sh` does it behind a flag file, as a stand-in).
  Either always, which costs nothing when the box isn't framed (a partitioned cookie works top-level
  too, in its own partition), or only for an entry link that asks for it (`&framed=1`). Check before
  shipping: with `None`, hud's write routes rely on refusing cross-origin JSON (no CORS headers for
  an unknown origin, so the preflight fails). That held for the bridge, which had to send the box's
  `Origin`, but it should be checked on purpose.
- **illogicald: asks on browser blocks.** `POST /api/panes/{id}/ask` answers `no terminal %2` for
  a block (`Api::Ask` is terminal-only), so the bridge raises the card on a terminal pane beside the
  app instead. That gets the card's label ("Claude Code asks") and its project wrong: the bundle
  key is `ask:<the bridge's cwd>:claude`, so on the swarm the box's question clusters under
  *illogical*. In the daemon: an app block (a web-page block that knows it's a hud box) runs the
  follower itself, raises asks on itself with source `hud`, and takes its project from the box's
  repo.
- **Who answered.** hud records whichever hud player the follower's cookie belongs to (the owner,
  here) as `answeredBy`, whoever clicked in illogical. With teams (M29, M30) that's wrong. Either
  each person's illogical gets its own hud player link (`hud share --role player`), or hud takes
  "answered in illogical by X" from a trusted follower.
- **Entry links, from studio, for illogical.** The owner's way in is a 10-minute `/__enter` link
  that only a signed-in studio session can mint (`POST /api/apps/:name/open`, passkeys). illogical
  needs a studio token to list a person's apps and mint links, and must not keep the link: today it
  sits in the block's URL bar and config (the `k=` in the screenshots is for an expiry that has
  passed, so it's useless now). After the first load the partitioned cookie lasts a year, so a
  reload doesn't need a new link. A restore after a reboot and a second device weren't tried.

## Setup and how it was run

- geek's wispd (`127.0.0.1:7789`, `--url-domain widgets.wtf,games.arugula.io,studio.arugula.io`)
  already holds studio's boxes and `arugula-box-template`. `POST /v1/sprites {name: "s21-app",
  from: {sprite: "arugula-box-template"}, url_settings: {auth: "public"}}` cloned it in 70 ms. It
  answered on its URL about 40 s later, from cold.
- **The clone landed on `s21-app.widgets.wtf`** (wispd's first domain) while the template's
  `~/box/domain` says `studio.arugula.io`, so hud printed its owner link for the wrong host and the
  door refused to find it. Writing `widgets.wtf` there and restarting hud and the door fixed it. The
  lobby passes no `url_domain` either, so check which domain studio's own boxes get on geek.
- **Claimed by hand:** the claim enrolls the box with studio.arugula.io, which doesn't know this
  sprite, so the spike wrote `~/box/enter-secret` and `~/box/claimed` itself (what a claim leaves
  behind) and mints entry links with the door's own HMAC (`enter.mjs`). Everything from `/__enter`
  on is the door's and hud's own code.
- **No credentials in the box:** `stub-anthropic.mjs` runs in the box on `127.0.0.1:9999`, and the
  box's `run-daemon.sh` points hud's local agent (Claude Code through the Agent SDK, 78 tools) at it.
  It answers the person's message with an `AskUserQuestion` tool_use and a tool_result with text, so
  the turn is real and the inference is fake. Claude Code ends its message list with a
  `system`-role message (its environment), so "the last message" means the last non-system one.
- illogical 0.6.0 (`~/.local/bin/illogicald`) as a throwaway daemon on `127.0.0.1:7850` with its own
  state directory, driven with `--socket`. Playwright 1.63 with Chrome (`channel: chrome`) and its
  Firefox.

```
./box.sh s21-app '<script>'                      # run in the box (wispd's exec)
node enter.mjs <enter-secret> s21-app s21-app.widgets.wtf   # an entry link
node frame.mjs <entry link> <label> chrome firefox          # the framing matrix
illogical --socket S open <entry link>                       # the box as a block
illogical --socket S run --split %2 "ILLOGICAL_SOCK=S node bridge.mjs https://s21-app.widgets.wtf <cookie file>"
node demo.mjs http://127.0.0.1:7850 https://s21-app.widgets.wtf <cookie file> <chatKey> 3
```

The cookie and the enter secret were kept outside the repo (`.gitignore`). `s21-app` is still
on geek's wispd, claimed by hand and pointed at the stub.
