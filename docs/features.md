# Features

What illogical does, roughly in the order it was built. [PLAN.md](../PLAN.md) has the reasoning and the milestones.

Examples name the machine that serves the page `home`; on a real tailnet it's your machine's MagicDNS name.

Everything from M1 (sessions, tabs and splits held by the daemon, driven by
the mouse, the same live on every window and a phone), and now it survives
the daemon stopping, crashing, or the machine rebooting:

- Every pane's output goes to an append-only log as it happens, and its
  terminal is checkpointed (Ghostty's snapshot format, zstd) after 5s idle
  or every 2 MB. The layout is saved on every change.
- On start, each pane is rebuilt from its checkpoint plus the log after it,
  marked `── restored <time> ──`, and then does what its restart policy says
  (right-click a pane → *After a restart*): start a shell in its last
  directory (default), re-run its last command (asking first, or not), run a
  command you choose (e.g. `claude --continue`), or wait for Enter.
- A shell killed by a signal (OOM, `kill -9`) leaves its pane and scrollback
  in place and offers a new shell. Only an ordinary `exit` closes a pane.
- *Forget history* deletes a pane's saved output and clears its screen.
  History is kept to 256 MB per pane, in `~/.local/state/illogical`, which
  is private to you (0700/0600).

- **Restarting the daemon doesn't touch running programs** (M2b). Each pane's
  program runs in its own systemd scope behind a small shim, and its terminal
  is kept in systemd's FD store while the daemon is gone. A restarted (or
  crashed and auto-restarted) daemon adopts every live pane: vim keeps its
  screen, a build keeps building, output from the gap is read from the
  terminal, and open windows reconnect on their own. `just install` upgrades
  in place. `systemctl --user stop` is still the end of the panes, like a
  reboot. On macOS (and without systemd) each pane's shim keeps its
  terminal instead (`--keep-panes`), with the same result; a stop ends the
  panes a minute later.

- **Panes know about commands** (M3). bash gets shell integration
  automatically (the way Ghostty does it: no dotfile changes), so each pane
  knows where every command starts and ends, its exit code and its directory.
  In the browser each finished command gets a mark in the gutter (green or
  red); click it to select the output, right-click to copy it or run it
  again. zsh and fish scripts are included but untested.
- **`illogical`, a CLI for scripts and agents**, works from any shell and
  from inside every pane (`ILLOGICAL_PANE` and `ILLOGICAL_SOCK` are set and
  it's on `PATH`). See below.
- **Attention.** A pane that rings the bell, sends a notification (OSC 9,
  777, 99), has an agent go quiet mid-command, or is told by a hook, shows a
  badge on its tab and pane (and in a "Needs you" list on the phone). A long
  command finishing while you're elsewhere shows "done". With *Notify this
  device* on (session menu), the phone gets a push notification; tapping it
  opens the pane.
- **History.** Closed panes' output is kept for 7 days, and `illogical
  history` / `search` look across all panes.
- **VM tabs and panes** (M3b, M3c). *New VM tab* (the `+` button's
  right-click, the tab and session menus, the phone's sheet) or `illogical
  run --vm-tab` opens a tab with its own throwaway Firecracker microVM, a
  wisp sprite on this host. Splits in it join the VM, so a shell and
  `claude` side by side see the same files; *Split (local)* adds a shell on
  this host instead (badged `local`). Panes on the tab's VM can't be
  dragged out of it (local ones can). The tab's menu can start a new pane
  on the VM or reset it (delete and recreate it; the panes restart by
  policy), and closing the tab deletes it. *New VM pane on the right* or
  `illogical run --vm` gives one pane a VM of its own, deleted with the
  pane; *Share machine with tab* hands it to the tab. It's for agents and
  untrusted builds.
  Its shell gets the same integration (marks, `wait`, history), `process`
  asks the VM, and the output is logged here, so `illogical tail` still has
  the session after the VM is gone. Restarting the daemon reattaches to the
  VM's shell without losing or repeating output. If the VM is deleted from
  under a pane, Enter starts a new one; after a reboot, each VM comes back
  as one fresh VM (one per tab, not per pane) and its panes restore by
  policy. Needs wispd's token at `~/.local/share/wisp/token`
  (`--wisp-url`, `--wisp-token-file`); without it VM panes are off. The
  base image is plain Ubuntu 24.04: install what you need, e.g. Claude Code
  with `curl -fsSL https://claude.ai/install.sh | bash`.

- **Blocks** (M6, in progress). A pane is one kind of block; every kind
  shares the layout, ids, attention, `describe` and `call`. The first other
  kind is a browser block for ordinary pages: *Open a web page…* in the pane
  menu, or `illogical open example.com`. Sites that refuse to be framed get
  a card with "open in new tab". Block directories are `blocks/<id>/` in
  the state directory (`panes/` before; it's moved, and left as a link).

- **Browser blocks on ports** (M6a). Run `npm run dev` in a VM tab, then
  *Open a port on this machine…* (pane menu), *Open a port on machine…*
  (tab menu), *Open port* (phone sheet) or `illogical open --split right
  :5173` puts the app beside it, hot reload and all. A block opened from a
  pane shows that pane's machine's port (or this host's). Each block is
  served on an origin of its own, `https://b-<id>.illogical.example.com:7443`
  for example, by the daemon, which proxies it to the port (through the Sprites
  proxy for a VM). The dev server needs no config: the proxy rewrites
  `Host` and `Origin` to `localhost:<port>`, and since that switches off the
  server's own guards, the proxy enforces its own: only you (asked of
  tailscaled), only the block's own origin, nothing cross-site but page
  loads, framed only by the app. It strips `Tailscale-*` headers, so agent
  code never learns who you are, and a script in the page can't reach
  illogical: the app refuses every origin but its own. The block follows
  the frame's navigations; when the server dies it asks for you and shows
  the page again when the server is back. Events: `navigated`,
  `load_error`.
- **Agent blocks** (M6b). An agent run as UI instead of a TUI: messages,
  thoughts, tool-call cards with each command's output in a read-only
  terminal, permission requests as Approve / Always / Deny cards (big
  enough for a thumb, and actions on the push notification), a composer,
  Stop, and cost per turn. The block is an
  [ACP](https://agentclientprotocol.com) client, so one block type drives
  Claude Code (`claude-agent-acp`), Codex (`codex-acp`), a Fountain agent
  (`fountain acp`, in Fountain's sandbox) or any ACP agent server. Start
  one from *Start an agent…* in the pane menu, *New agent* in the phone's
  sheet, or `illogical agent`.
  - *Always* is remembered by the block (in its config) and answered by
    it; it never picks the agent's own "always", which would write
    `.claude/settings.local.json` into your repo. Claude Code runs with no
    settings sources, so your own hooks don't fire inside it.
  - The block's log is the JSON-RPC stream; `capture` is the transcript as
    Markdown, `history` lists the agent's commands and turns, `search`
    covers what agents said and ran.
  - A local agent server runs in its own scope with its pipes in systemd's
    FD store, so restarting the daemon mid-turn (even with an approval
    open) doesn't touch it. After a reboot the session reopens with
    `session/resume` (or `session/load`), unless the policy is `none` or
    `rerun-ask` (then *Resume*). A Fountain turn that ran on while nothing
    followed it shows "running on Fountain" and appears when it ends.
  - The adapters, pinned, go in `~/.local/share/illogical/agents/`:
    `npm install --prefix ~/.local/share/illogical/agents/claude @agentclientprotocol/claude-agent-acp@0.85.0`
    and `npm install --omit=optional --prefix ~/.local/share/illogical/agents/codex @agentclientprotocol/codex-acp@2.1.0`
    (Codex uses `~/.local/bin/codex`). They need Node on PATH (mise's
    shims are added if present).
  - **Questions and forms** (M6c). Claude Code's AskUserQuestion is a
    question card: buttons for one answer, checkboxes for several, each
    option's description, an "Other" box (on its own it's the answer; next
    to a pick it's a note), and an option's preview (mockups, code) in
    monospace when it's picked. *Submit*, *Skip* (the agent hears you
    didn't answer and goes on) or *Stop* (ends the turn). Any other form (an
    MCP server's, Codex's plan-mode question) is drawn from its schema, and
    an MCP server's sign-in link is a card with *Open link* that closes when
    the server says you're done. A question waits as long as it takes: the
    block needs you, the push notification says the first question (one
    question with two options is answered from the notification's
    buttons), and it survives a daemon restart and a reload; the first
    answer from any client wins. From a script: `wait %N --needs-input`
    prints it as JSON, `call %N answer '{"question_0":"Red"}'` answers
    (`question_<n>_custom` is "Other"; a multi-select takes a list), and
    `call %N decline` skips. The question and the answer are in the
    transcript, `history` and `search`. `illogical agent --mcp
    'NAME=COMMAND'` gives the session an MCP server. Fountain agents can't
    ask (Fountain doesn't pass questions on, so they ask in plain text),
    and Codex only asks this way in its plan mode.
  - **In a VM** (`--vm`, or the dialog's checkbox) the agent server runs
    over a non-TTY exec on the block's own machine; its first start
    installs Node and the adapter there (about 15s). Claude Code there
    needs credentials: a token from `claude setup-token` in
    `~/.config/illogical/claude-oauth-token` (given to it as
    `CLAUDE_CODE_OAUTH_TOKEN`), or an API key in
    `~/.config/illogical/anthropic-key` (`ANTHROPIC_API_KEY`, used first);
    `--claude-token-file` and `--anthropic-key-file` move them. They reach
    the agent on its stdin, into its environment only: never the VM's
    disk, a URL, an argv, the log or the layout. A VM agent survives a
    daemon restart (its exec session lives on wisp).

- **Other hosts** (M4a). Every daemon is a peer; the one the page comes
  from (the "home daemon") keeps a list of the others and checks on
  each every minute. The page shows a host switcher (desktop: the bar's
  left end; phone: the sheet), and each host has its own sessions and tabs.
  Switching connects straight to that daemon; nothing is relayed, and the
  host you left gets no connection (so a sandbox can sleep). The list is
  remembered in the browser, and the page itself by its service worker, so
  the other hosts stay reachable while the home daemon is down.
  `illogical --host NAME …` runs any command on another host. A sandbox
  (a sprite, a container: no systemd needed) gets a static daemon on the
  tailnet with one command and adds itself to the list; see *Use it*.
- **Hosts that can only dial out** (M4c). A sandbox that allows nothing
  in but outbound HTTPS runs `illogicald --peer wss://home.… --token FILE`:
  it keeps one WebSocket open to the home daemon and serves its own
  WebSocket and API over it, many streams at once. The home daemon lists it
  (`dial_out`) and answers for it at `/h/NAME/…`, behind its own access
  checks, so the page's host switcher and `illogical --host NAME` work as
  for any host. It's not a hub: only the home daemon opens streams, the
  host serves nothing that leads elsewhere, and what it answers is served
  defanged (no cookies or CORS, `nosniff`, a sandboxing CSP), since it
  lands on the home daemon's origin. It redials with backoff and works on
  its own meanwhile. The token is per host, minted by the home daemon
  (`illogical hosts token NAME`, or joining with an invite), stored only
  as a hash, good for that host alone, and revocable (`hosts revoke`,
  `hosts rm`).
- **Read-only share links** (M4c). `illogical share %N --ttl 1h`, or *Share
  read-only link…* on a pane, gives a `/share/…` link that shows that pane
  live (its screen and scrollback, then its output) and nothing else: no
  typing, sizes, other panes or API, and a viewer that sends anything is
  hung up on. Any tailnet user may open one (someone the node is shared
  with, say), never a tagged node, Funnel or the internet. Links expire (a
  week at most), are listed (`illogical shares`) and revocable (`shares
  revoke ID`), which cuts off anyone watching.
- **History that outlives a sandbox** (M4c). With `--sync` (closed panes)
  or `--sync-live` (open ones too), a host pushes its panes' log segments
  and indexes to the home daemon with its token, resuming from what is
  already there. The home daemon keeps them encrypted at rest and answers
  `illogical history|search|tail --synced NAME` (or `--host NAME`, once the
  host is gone) from them. Kept 256 MB per pane, 30 days after the last
  push. Encryption: each file is AES-256-GCM records under its own key
  (HKDF from a key ring only the home daemon holds, `<state>/synced/key`,
  0600, or `--sync-key-file`; salted per file, bound to the file's place),
  with counter nonces and the header and record number as associated data.
  `illogical synced rotate-key` re-encrypts everything under a new key and
  drops the old one. File names and sizes aren't secret; contents are.

- **Sandboxes** (M4b). *Sandboxes…* (session menu; *Sandboxes* in the
  phone's sheet) or `illogical sandboxes` lists the home daemon's provider's
  sandboxes (wisp sprites here; Fly's Sprites API fits the same adapter)
  with their state, asked of the provider, which doesn't wake them.
  - *Shell* (`illogical run --sandbox NAME`) opens a pane here whose
    terminal is a plain exec on that sandbox: nothing is installed there.
    It's disposable, and badged so: the output is logged here while it's
    attached, but the provider keeps only its replay buffer while nothing
    follows it (1 MB on wisp, about 6.5 KB on Fly), and closing the pane
    hangs the shell up and leaves the sandbox alone.
  - *Make resident* (`illogical sandboxes promote NAME [--as HOST]`) copies
    the static daemon in (`just static`; the home daemon finds it in
    `--static-dir`, default `~/.local/share/illogical/static`) and registers
    it as a sprite *service*, so it starts on every boot and restarts if it
    exits. It becomes a host in the list, a *provider* host: the client and
    `--host` reach it through the home daemon's **provider tunnel**
    (`/tunnel/HOST/…`, through the Sprites proxy to its port, never a
    public URL). Connecting wakes it; the page lets go of it 10s after it's
    hidden, so it can sleep. After it goes cold (on wisp, a real reboot)
    its daemon restores its layout and scrollback from its own disk, the
    way a reboot does here. A provider host with a tailnet URL too is
    switched to the tailnet if that answers within 5s of the wake.
    `illogical sandboxes demote NAME` stops it.
  - **Identity.** The tunnel is for callers the home daemon already let in
    (the owner, or its Unix socket). It strips their identity headers and
    presents a token it minted for that host when it made it resident; the
    resident daemon keeps only the token's SHA-256 (in its arguments) and
    refuses every loopback connection without it, so other programs in the
    sandbox can't use the provider's proxy path to it. These provider
    tunnel tokens (`ilp_…`, home → host) stay in the home daemon's
    `provider-tokens.json`, never in the host list clients get; dial-out
    host tokens (`ilh_…`, host → home, M4c) are the other direction.
    `hosts revoke` and `hosts rm` drop whatever a host has of both.
  - Like a dial-out host's (`/h/NAME`), only the WebSocket and the API go
    through `/tunnel/NAME`, and the answers are defanged: they're served on
    the home daemon's origin and the sandbox runs untrusted code.

- **Files and navigation** (M7). *Go to directory…* (a pane's menu; *In a
  directory…* on the `+` button's right-click; *Go to directory* in the
  phone's sheet, where it's a full-screen sheet; Ctrl+Shift+G) browses
  directories on the host the pane runs on: this daemon's, another host's
  (it answers for itself), or its VM's (through the provider). It starts
  where the pane is (OSC 7), lists directories used lately there first,
  and filters fuzzily as you type (`/…` or `~…` goes to a path; Backspace
  goes up). Then *New pane here* (on the same host: a VM tab's machine, a
  sandbox's shell), *New tab here* (not for a VM's own directories: they
  exist only in its tab), or *cd there*, which types `cd` into the shell
  only while it waits at its prompt (shell integration says so) and says
  why not otherwise.
  - The `fs` methods behind it (`/api/fs/list|stat|read|watch|recent`,
    `illogical fs`) are read-only and part of the owner's API: share-link
    viewers and host tokens never reach them. On a daemon's host they read
    as the daemon's user, so the OS's permissions are the limit, and they
    also refuse `/proc`, `/sys`, `/dev`, the daemon's state directory and
    the secrets it knows of (the wisp token, agent credentials); paths are
    resolved first and an opened file is checked again through
    `/proc/self/fd`, so no symlink (or one swapped in mid-open) gets
    around that. On a machine the provider's agent reads as the sandbox's
    root, in the user's own sandbox; the same places are refused by path,
    and a read through any symlink is refused. A listing holds at most
    5000 entries and a read at most 1 MiB (read in ranges); `watch`
    polls (1s here, 3s on a machine) until you hang up.
  - New sessions, and the machines of VM tabs and VM panes, get generated
    names ("drifting cedar", unique per daemon). Ids don't change, rename
    is still a double-click, and older sessions keep their names.

- **iTerm2 as a client** (M5). `illogical tmux -CC` speaks tmux's control
  mode, so iTerm2 (and Ghostty's and WezTerm's tmux support) shows
  illogical's sessions, tabs and splits as native windows, tabs and splits,
  live alongside the browser; see *Use it*.

- **The swarm** (M26, `/#swarm`, *Swarm* beside the tabs). Every pane on
  every machine you and your team can see, as one field of tiles coloured
  by kind and lit by activity, clustered by project (or directory, outside
  a repository), machine, kind, session or person. What needs you lifts out
  to a rail of cards bundled by cause ("3 failed on build-02", "2 agents
  ask"), where you allow, deny, answer or dismiss them all at once, and
  send an agent its next instruction. Hover a tile to peek at its last
  lines, click it to open it. On a phone the cards are a strip along the
  bottom. `just fake-fleet` runs three throwaway machines to try it on.

