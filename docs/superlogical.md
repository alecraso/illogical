# Superlogical: what they're building, and a best guess at the whole plan

Written 2026-10-01 from public sources only (see [Sources](#sources)). There is
no Superlogical code, beta, pricing or spec yet. Everything below is labelled:

- **[said]**: stated by Superlogical or a founder (site, posts, videos).
- **[code]**: visible in public code (Ghostty, go-libghostty) or their infra.
- **[3p]**: third-party claim, not confirmed by the company.
- **[guess]**: our inference, with a confidence of high / medium / low.

## TL;DR

Superlogical is Mitchell Hashimoto's new company (announced 2026-07-29), and it
is running the HashiCorp playbook on a new primitive: the **durable session**.

1. **Now (step 1): "Rex", a terminal multiplexer that is really a
   distributed system.** It is a self-hosted Go server (also embedded in the
   macOS app) that owns the PTYs. Smart clients on libghostty run their own
   replica of each terminal's state machine; the server sends a binary
   snapshot and then tees raw PTY bytes. The macOS app is done first, web works,
   iOS is coming. It has Tailscale built in and does a real system login, which
   makes it a **full SSH replacement**. A CLI and event stream make every
   session scriptable. No hosted service at launch. **[said]**
2. **Next (step 2): "multiplexer for all work".** A terminal becomes one
   *block type* among many. Blocks already have typed methods and events.
   Expect non-terminal blocks (agents, jobs, files, browser-like views,
   services) and an API that software and agents drive directly. **[said]
   that phase 2 is this transition; [guess] which blocks, medium**
3. **Later (step 3): "safe and operable in production".** A Rex server on
   *every machine, server and Kubernetes pod*, used as the access, audit and
   control layer for humans and agents touching production. That means
   identity mapping, policy, approvals, recorded and encrypted history. In
   effect it competes with SSH, bastions, Teleport/Boundary and agent
   sandboxes, plus the "where did the agent's work go" problem. **[said]**
   for "every machine / every pod"; **[guess]** for the production feature
   set, medium-high.
4. **Business model [guess, medium]:** open protocol (in libghostty) and a free
   self-hosted server and clients for adoption. Revenue comes from
   team/enterprise features (sharing at org scale, SSO/RBAC, audit, policy,
   approvals, retention) and later probably an optional hosted control plane.
   That is HashiCorp's open-core then cloud path, with Ghostty and libghostty
   as the non-profit commons underneath.

## Who and how much

| Fact | Source |
|---|---|
| Founders: Mitchell Hashimoto (Ghostty; HashiCorp co-founder, CEO/CTO), Jack Pearkes (HashiCorp's first employee, VP Eng/R&D), Alasdair Monk (Poolside Head of Experience, ex-VP Design Vercel), Hector Simpson (Poolside, Heroku, HashiCorp, Vercel designer) | [said] homepage |
| Monk and Simpson run Replay Software, makers of **Echo**, a Ghostty-backed iOS SSH and Mosh client pitched for "agents anywhere". The launch tweet says "from the creators of Ghostty and Echo". | [said] |
| Seed **led by Notable Capital** (Glenn Solomon), with Amplify Partners and angels including Collison, Lütke, Levie, Rauch, Dadgar, Dax Raad, Steve Ruiz, Mario Zechner | [said] homepage + investor posts |
| Round size **$10M**; HQ Manhattan Beach, CA | [3p] aventure.vc, Caplight; not confirmed |
| Only open role is *Software Engineer, Infrastructure*: "systems software and infrastructure for our first product and its release infrastructure… reliability, performance, reproducibility… networking, processes, packaging, deployment, observability. **Nix, NixOS, Go, Linux** strongly preferred." Hiring in LA, London and NYC. | [said] `ssh superlogical.jobs` (the server banner is `SSH-2.0-Go`) [code] |
| Ghostty stays a non-profit (Hack Club fiscal sponsorship). Superlogical consumes MIT libghostty and upstreams "shared terminal work". | [said] |

## The stated plan (verbatim)

From the homepage:

> We believe the missing layer is a durable session around the work itself:
> one that can span applications and environments, provide relevant context by
> default, expose structured data and actions, preserve history, and be driven
> by software while remaining visible and controllable by people.
>
> 1. Build an incredible multiplexer.
> 2. Make everything in it composable.
> 3. Make it safe and operable in production.
>
> A multiplexer brings multiple independent streams together through a common
> interface. For us, that means interactive work, automatic work, and
> production work would share one well-crafted underlying system instead of
> living in separate tools.

The rotating headline lists the scope: *local development, remote access,
coding agents, background jobs, production applications, live debugging,
sandboxes, shared terminals, incident response, humans and machines,
operational history, multiplayer work.*

Mitchell, on X, about step 2: "phase 2 of our plan (the step that transitions
from 'multiplexer for terminals' to 'multiplexer for all work')". Also: "Big
assumption that we see any of those as TUIs. Multiplexer for all work, one of
those workloads is a terminal. But the rest…"

## Step 1 in detail: what Rex is (mostly confirmed)

**Name.** The multiplexer, server and CLI are called **Rex** (`rex block close`,
"the Rex multiplexer"). Rex shows up as the login service in `who`. [said]

### Architecture: N replicated terminal state machines [said, code]

1. **Attach.** The server pauses PTY processing, sends a binary snapshot of the
   visible screen, then a READY frame. The client can draw and type
   immediately. Scrollback streams afterwards, **newest first**.
2. **Steady state.** The server **tees raw PTY bytes** to every client "like
   SSH". Each client runs its own libghostty terminal and gets the same state
   because VT parsing is deterministic. The server keeps the authoritative
   replica. A client that diverges throws away its state and re-snapshots.
3. **Input.** Input is serialized through the server: one writer, many
   readers.
4. **Views.** Every client owns its own viewport, scroll position and
   selection. Splits and tabs are client-side arrangement, with one connection
   per terminal. This is why scrollback and selection are "native" and why
   Kitty graphics just work: the server and client run the same engine.
5. **Dumb clients.** There are two fallbacks: attach one terminal by ID into
   any terminal, or a tmux-like mode where the server re-renders.
6. **Protocol.** It is a custom binary protocol, "predominantly part of
   libghostty, so an open protocol". Mosh-like latency tricks are used for
   remote hosts.

The public code backs this up. Ghostty merged a **binary snapshot format**
(`GHOSTSNP`; TERMINAL/SCREEN/PAGE/READY/HISTORY/FINISH records; CRC32C; Kaitai
spec; zstd) two days after launch. Mitchell said it was the format "used [as]
a proof-of-concept in my own projects". Follow-up PRs added:

- a PTY *continuation* record, so a client can join a live stream without
  losing its place;
- an incremental decoder that reaches READY in about 40µs;
- a C API;
- "write until ground state", so custom sequences can be spliced into the
  stream;
- callbacks for unknown APC/OSC sequences, "to implement their own custom
  protocols";
- render-state diffing APIs;
- heavy wasm work: faster than xterm.js; 218KB compressed, or 132KB for a
  read-only viewer build; 75% less memory.

On a Ghostty discussion about reconnectable terminals, Mitchell wrote: "We're
doing this ourselves using the binary snapshot protocol."

The server is almost certainly **Go via `go-libghostty`** (cgo, statically
linked, cross-compiled with Zig, `flake.nix`, Buildkite). Mitchell created it
in 2026-04, Pearkes was using it in 2026-07, and the job post asks for Go and
Nix. **[guess, high]**

### Server built for agent scale [said]

> Our vision of where this is going is that you will run a Superlogical server
> on every machine you own, on every server component that you or your company
> has, on every Kubernetes pod potentially… it's not a multiplexer for one
> person… it's dozens of people, hundreds of people with… hundreds of
> thousands of agents or more.

How the server keeps memory down:

- **About 400KB per full terminal**, against about 5MB for tmux. Zellij's
  per-client cost is called "unscalable".
- **Terminal parking.** After 60s with no PTY reads, the full emulator state is
  snapshotted to disk, encrypted because scrollback holds secrets. Unparking
  takes about 200µs. Typing alone doesn't unpark, and attaching to a parked
  terminal streams the snapshot straight from disk.
- **PTY parking.** Each PTY normally gets a dedicated blocking thread for
  throughput. When idle or unwatched, its fd moves to a shared epoll/kqueue
  thread.
- **Client buffer parking.** An idle client's buffers are freed.

### Clients and access [said]

- The **macOS app is a server too**: it starts a local server invisibly.
- The web client is "very functional". iOS is announced; Echo's team builds it
  **[guess, high]**. Ghostty removed its own iOS target in August **[code]**.
  The platform list for the first release is "still being figured out".
- **Linux CLI and server, plus a NixOS module.** "Nothing you see here requires
  any services and we're not launching any hosted services."
- **Networking.** The server "has built-in support for Tailscale (and
  Headscale) and acts like a node", and will use "something like tailcat for
  direct connects". tailcat is Tailscale's Go data-plane library that needs no
  accounts.
- **SSH replacement.** It does a full SSH-style system login, so it
  appears in `who`, loads the login shell and honours user limits. Identity
  comes from Tailscale (`mitchellh@github`), with a server-side mapping of
  "who can act as who", and existing SSH keys are respected. "I don't run SSH
  on my servers anymore."
- **UX.** It deliberately "feels like a normal terminal" with no modes to
  learn: "The world's best multiplexer is the one you don't have to learn."
  It loads in under half a dock bounce. Sessions get auto-generated names
  ("drifting cedar"). Vertical tabs are optional. Cmd-Shift-G is a go-to
  directory picker that **works on remote hosts**, so the server exposes a
  filesystem API. Per-layout "Deck icons" are "favicons for your terminal
  apps", and the design borrows from the web browser.
- **Sharing.** Live multiplayer sharing is "built in from the start" but has
  not been demoed yet.

### Already composable: the CLI and API [said]

The model is **session → window (tab) → block**. A terminal is one *block
type*. Each block has an ID, a label and a type, plus **methods** (for
example `process`, which returns the child and foreground process as JSON) and
**events** (child exited, size changed, block closed, layout changed, client
connected). From the CLI you can:

- create sessions and run commands into blocks;
- split, move and rename blocks;
- `wait` for a block to finish;
- inject key and mouse events;
- capture the screen as text or HTML;
- stream a session's events.

Everything the GUI does goes through this API, and the CLI can do more than
the GUI. The CLI is injected into every terminal session.

## Steps 2 and 3: best guess

### Step 2: "make everything in it composable"

What's already in place (block types, per-block methods and events, an event
stream, an API-driven UI) is the scaffolding for step 2. Our reading:

- **Non-terminal block types [guess, medium-high].** Most likely: an agent
  block (structured agent runs with "waiting for approval / thinking / done"
  state instead of scraping a TUI); a job or task block (the CI and background
  work that "disappears into jobs and logs"); file, diff and editor views
  (go-to-directory already crosses hosts); and probably web or browser panes
  (the "browser" design language and the cmux comparison).
- **Software as a first-class client [guess, high].** Agents attach like any
  client, subscribe to events, call methods and get "relevant context by
  default". The session becomes the agent's workspace *and* its record.
- **A typed artifact and context model [guess, medium].** "Expose structured
  data and actions" and "preserve history" suggest the session history becomes
  queryable: commands, exit codes, cwd, outputs and artifacts, rather than a
  scrollback blob.
- **An ecosystem surface [guess, medium].** An SDK and a published protocol in
  libghostty so third-party terminals, editors and agent tools can be Rex
  clients. This is the "primitives others standardize around" thesis Amplify
  calls out.

### Step 3: "make it safe and operable in production"

"Server on every machine and pod", "SSH replacement", the identity mapping and
the encrypted parked scrollback all point here. Our reading **[guess,
medium-high]**:

- **Access.** Rex replaces sshd and bastions: tailnet or SSO identity, mapped
  to OS users with policy. This is HashiCorp Boundary and Vault territory,
  rebuilt around sessions, and directly competes with Teleport.
- **Audit and history.** Every human or agent session is durable, recorded and
  attributable: who did what, with handoff between human and agent. This is
  the CIO pitch analysts already picked up on.
- **Approvals and guardrails for agents.** Agents act in production through
  sessions where a human can watch, intervene or approve live, with sharing as
  the incident-response primitive.
- **Fleet operation.** A NixOS module, Kubernetes sidecar or daemonset,
  reproducible releases (the job post), and scale to hundreds of thousands of
  sessions.
- **Incident response and live debugging.** Multiplayer sessions attached to
  production pods.

### Business model and go-to-market [guess]

- **Launch:** free (or cheap) native apps plus a self-hosted server; no hosted
  service, Tailscale for networking. The beta is aimed at developers who live
  in terminals with agents, which drives bottom-up adoption the way Vagrant and
  Terraform did. *(high)*
- **Open-source drops:** the protocol and snapshot work in libghostty (already
  public), probably client libraries, possibly the server. Mitchell promised
  "devlogs and open-source drops"; the server's license is unknown. *(medium
  that some of the server is open)*
- **Monetisation:** team and enterprise editions with SSO/RBAC, org-wide
  sharing, audit and retention, policy and approvals, and fleet management.
  Later, an optional hosted control plane (discovery, relay, sharing across
  orgs), but explicitly *not* at launch. *(medium)*
- **Endgame:** the session layer that sits between humans, agents and
  infrastructure. Gergely Orosz's "agentic operating system a level above
  OSes" is a fair outside summary. *(low-medium as stated; the direction is
  clear)*

### Why we believe this reading

- It is the HashiCorp pattern: a developer-loved open tool, then a standard
  primitive, then enterprise control plane features (security, identity,
  audit), then a hosted platform. Mitchell and Pearkes built exactly that
  once.
- In Dec 2025, before the company, Mitchell described the goal as "a tmux
  replacement based on libghostty, embed that in Ghostty GUI so all GUIs can be
  servers too optionally". Rex is that, productized.
- Pearkes's 2025 "Bigwig" project was a self-hosted (BYOC) worker running
  long-lived CLI agents with web and iOS clients, structured UI tools and
  sandboxing. He wrote it "demands better solutions for secrets management,
  authentication workflows, and sandboxing". That is step 2 and step 3 in
  miniature.
- Monk and Simpson already shipped a Ghostty-based mobile terminal for "agents
  anywhere".

## Timeline

| Date | Event |
|---|---|
| 2025-09 | "libghostty is coming" post |
| 2025-12-17 | Mitchell: goal is "a tmux replacement based on libghostty" |
| 2026-04-10 | go-libghostty created |
| 2026-07-04 | Pearkes upstreams OSC 52 fix found "using go-libghostty" |
| 2026-07-29 | Superlogical announced; Notable leads seed |
| 2026-07-30 | Architecture video on X (snapshot, tee, replicas) |
| 2026-08-01 to 03 | Binary snapshot format and follow-ups merged into Ghostty |
| 2026-08-04 | Monk: Deck icons, "borrowing from the web browser" |
| 2026-08-14 to 17 | libghostty wasm: speed, size and memory work |
| 2026-08-28 | First pre-alpha demo (macOS); "N replica distributed terminal state machines" |
| 2026-08-30 | "phase 2… from multiplexer for terminals to multiplexer for all work" |
| 2026-09-02 | Server memory benchmarks and parking video |
| 2026-09-08 | Remote sessions demo; SSH replacement |
| 2026-09-14 | CLI demo; blocks, methods, events |
| 2026-10-01 | Still pre-beta; sign-up list only; no public repos in `github.com/superlogical` |

## What would change our mind / what to watch

- **A hosted service at or soon after launch** would point to a SaaS-first
  business and away from open core.
- **The server's license** (open source or proprietary) decides how much of
  the moat is code and how much is product.
- **The first non-terminal block** shows what step 2 really looks like.
- **SSO, audit or policy features in a beta** would mean step 3 is starting
  early.
- **Ghostty gaining an "attach to a Rex server" mode** would make Superlogical
  the default backend for millions of Ghostty users.
- **A published protocol spec** would test whether "open protocol" is real.

## What this means for illogical

The design converges with [PLAN.md](../PLAN.md) on the core idea: the server
owns the session and the client owns the view. Superlogical goes further in
four places worth noting:

- **Replica clients instead of server-rendered output.** They tee raw bytes to
  libghostty clients, so scroll and selection are native. Our xterm.js client
  plus `snapshot` then `output` frames is the same shape. Their attach order
  (visible screen, READY, then history newest-first) is worth copying.
- **They use the upstream snapshot format.** `ghostty_snapshot_*` with
  continuation records is now public. It may replace our formatter-based
  `snapshot()` and our checkpoint files. As of 2026-10-01 the
  `libghostty-vt` 0.2.2 crate the S1 spike uses does **not** bind
  `ghostty_snapshot_*`. We would need a newer crate, our own `-sys`
  additions, or an upstream PR to libghostty-rs.
- **Parking.** Snapshotting idle terminals to disk and moving idle PTYs to a
  shared poller is cheap to adopt later and fits our checkpoint design.
- **Blocks with methods and events.** Their `wait`, `process` (foreground
  process as JSON), screen capture and event stream map onto our M3
  `run`/`tail`/`wait` and the `event` frame. They suggest adding a typed
  per-pane `process` query.

## Sources

Primary (company and founders):

- [superlogical.com](https://www.superlogical.com/) (homepage, plan, team, funders); `ssh superlogical.jobs`
- [Mitchell, "Superlogical"](https://mitchellh.com/writing/superlogical), 2026-07-29
- [@superlogical launch post](https://x.com/superlogical/status/2082489910942994665) · [@mitchellh launch post](https://x.com/mitchellh/status/2082489600715661389)
- Mitchell on Mastodon: [launch](https://hachyderm.io/@mitchellh/117003996375060120), [demo + "N replica" + platforms](https://hachyderm.io/@mitchellh/117175283681274864), [tmux "learn it"](https://hachyderm.io/@mitchellh/117175343438307850), [Tailscale/Headscale/tailcat](https://hachyderm.io/@mitchellh/117175411327821846), [NixOS module, no hosted services](https://hachyderm.io/@mitchellh/117175457057731197), [snapshot upstream](https://hachyderm.io/@mitchellh/117186436169699826), [memory benchmarks](https://hachyderm.io/@mitchellh/117202903279918618), [SSH replacement](https://hachyderm.io/@mitchellh/117237397684548220), [CLI / Rex](https://hachyderm.io/@mitchellh/117271724131710790), [wasm vs xterm.js](https://hachyderm.io/@mitchellh/117096017608717427)
- Mitchell on X: [phase 2](https://x.com/mitchellh/status/2093928444631621889), [not TUIs](https://x.com/mitchellh/status/2089858093735973282), [Dec 2025 tmux replacement goal](https://x.com/mitchellh/status/2001396290337583268), [architecture video](https://x.com/mitchellh/status/2082936029426892960)
- [Alasdair Monk: Deck icons](https://x.com/almonk/status/2084549282120511575)
- YouTube (Mitchell): [Pre-Alpha Demo](https://www.youtube.com/watch?v=XyxShCZrGiw), [Server Memory](https://www.youtube.com/watch?v=T5gV6anSt-4), [Remote Sessions](https://www.youtube.com/watch?v=PdwTjSBW6Y8), [CLI](https://www.youtube.com/watch?v=9fcfDF8SBnc), [High-Level Architecture](https://www.youtube.com/watch?v=Y6nFMmUPzXM)
- Ghostty: [#13534 snapshot format](https://github.com/ghostty-org/ghostty/pull/13534), [#13566](https://github.com/ghostty-org/ghostty/pull/13566), [#13556](https://github.com/ghostty-org/ghostty/pull/13556), [#13569](https://github.com/ghostty-org/ghostty/pull/13569), [#13580](https://github.com/ghostty-org/ghostty/pull/13580), [#13761](https://github.com/ghostty-org/ghostty/pull/13761), [discussion #12176](https://github.com/ghostty-org/ghostty/discussions/12176), [discussion #11998](https://github.com/ghostty-org/ghostty/discussions/11998)
- [go-libghostty](https://github.com/mitchellh/go-libghostty) ([source](https://tangled.org/mitchellh.com/go-libghostty))
- [Jack Pearkes, "This is my AI Assistant"](https://www.jackpearkes.com/posts/this-is-my-ai-assistant) · [Echo](https://replay.software/echo) · [Bonsplit](https://bonsplit.alasdairmonk.com/)
- Investors: [Amplify Partners](https://www.amplifypartners.com/blog-posts/announcing-our-investment-in-superlogical), [Notable Capital](https://www.notablecap.com/blog/backing-mitchell-hashimoto-again-12-years-later)

Secondary:

- [DevClass](https://www.devclass.com/devops/2026/08/01/dev-who-gave-hashicorp-its-name-returns-with-a-faster-terminal-multiplexer/5282025), [InfoWorld](https://www.infoworld.com/article/4204427/hashimotos-superlogical-bets-that-agentic-software-development-needs-more-than-a-better-terminal.html), [RuntimeWire](https://runtimewire.com/article/mitchell-hashimoto-superlogical-terminal-multiplexer), [Techzine](https://www.techzine.eu/news/devops/143363/hashicorp-founder-hashimoto-launches-superlogical/)
- [HN thread](https://news.ycombinator.com/item?id=49098965) (796 points; no founder comments)
- ["How terminal multiplexers work feat. Superlogical"](https://www.youtube.com/watch?v=Qmk-FRYIiyw) (walks through the architecture video)
- [OpenAgents teardown, 2026-07-29](https://github.com/OpenAgentsInc/openagents/blob/75600ae4cc3e19b9b68aaad49e61fbfb2be7ed9b/docs/teardowns/2026-07-29-superlogical-teardown.md) (speculative; predates all demos)
- [Peter Pistorius, "Why Are There So Many New Terminal Multiplexers?"](https://peterp.org/blog/terminal-multiplexers.html)
- [tailcat](https://tailscale.com/blog/tailcat) · [aventure.vc ($10M, unconfirmed)](https://aventure.vc/companies/superlogical-manhattan-beach-ca-us)
