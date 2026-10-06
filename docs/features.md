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
  777, 99), shows an agent's prompt waiting on you, or is told by a hook, shows a
  badge on its tab and pane (and in a "Needs you" list on the phone). A long
  command finishing while you're elsewhere shows "done". With *Notify this
  device* on (session menu; the phone's sheet), the phone gets a push notification; tapping it
  opens the pane.
  - **Agents in terminal panes** (#145). For Claude Code and Codex the
    daemon reads the bottom of the pane's screen and its title, as its own
    terminal has them (never where someone has scrolled to), with no hooks
    needed: a spinner or "esc to interrupt" is *working*, a permission or
    trust prompt is *needs you* with what it asks ("Claude Code asks to run
    `rm -rf build`"), and an empty prompt box is *idle*, or *done* when a
    turn someone started ends while nobody is watching. The agent is found
    by what runs in the pane's foreground, so `cd x && claude` counts. A turn has to look over in three reads
    100 ms apart (or for 700 ms) before it counts as ended, and the first
    second after an agent starts isn't read. Other agents (aider, gemini,
    opencode...) are known as agents but not read yet; for any agent,
    going quiet alone never means it wants you. Hooks, notifications and
    the bell still say so whatever the screen shows. The rules are a small
    table per agent in `crates/vt/src/detect.rs`.
  - **Why it wants you** (M24). Every pane that wants you says why: *ask*
    (an agent's question or permission request), *failed* (a command that
    ran a few seconds ended non-zero), *exited*, *input* (a bell or a
    notification) or *done*, with a one-line headline, the command and its
    exit code. Reasons bundle by cause (failures by machine, agents asking
    by project), and one call acts on a whole bundle: allow, deny, answer
    or dismiss (`/api/attention/act`). `illogical attention --json` lists
    them, `illogical events` streams them, and notifications are titled by
    them. Dismissing on one screen clears it on every other.
  - **Rerun** (M11). A failed command typed at a prompt can be run again:
    *Rerun* on the phone's *Needs you*, a swarm card, the notification, or
    the tab's ✗ badge, or `illogical rerun %N`. It's typed into the pane
    only once its shell is idle at its prompt.
  - **What each pane is** (M23). Every pane carries what it's running
    (shell, build, test, agent, server, logs or editor, from the program
    itself, so an alias for `claude` still reads as an agent), its git
    project, how busy it is and its title; `illogical ls --json` shows
    them. Clients get changes as small deltas rather than the whole layout,
    so a daemon with hundreds of busy panes costs each client a few KB a
    second.
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
- **Editor blocks** (M27). VS Code ([code-server](https://github.com/coder/code-server))
  where a pane runs, as a block: *Open in editor* (a pane's menu, or a
  tile's right-click in the swarm, or *Edit* on a card), or `illogical edit
  [PATH[:LINE]]`. A folder opens as itself, a file in its project (its git
  repository) at its line.
  - One server per machine, shared by every editor block there: this host,
    or the VM tab's machine (a sprite service there). It starts when a block
    needs it, stops after 15 minutes with nothing open (`--editor-idle`), and
    starts again when someone looks.
  - It has no auth of its own. On this host it listens on a 0600 Unix
    socket, not a port, and each block is served on its own origin like a
    browser block on a port, so the daemon's checks are its only auth.
    Opening one is the owner's: guests can't, viewers or editors.
  - The release is pinned and checked against its SHA-256, downloaded once
    into `~/.cache/illogical/code-server` the first time an editor opens
    (the block shows the download), or `--code-server PATH`.
  - Settings, extensions (from [Open VSX](https://open-vsx.org)) and state
    are in `<state>/editor/`, so they outlive restarts. New settings start
    with illogical's colours and VS Code's AI features off.
  - illogical's extension in each window reports the active file, the
    cursor and the lines around it: summaries say `kind: editor`, the
    project and `file`, and the swarm's preview (and `capture`) is those
    lines. It's the same extension as for your own VS Code (M28, below), so
    a block can be followed and its debugger's stops are cards too. After a
    daemon restart the window reconnects to the same session; after a
    reboot the block asks the new server for the file it had. In a VM it
    opens the file it was opened on and doesn't follow the cursor (the
    extension can't reach the daemon from there).
- **Changes: diff and file blocks** (M11). For checking what an agent
  did, from anywhere and especially the phone. *Changes* (a pane's or a
  tab's menu, the phone's sheet, a swarm tile with a project) opens a diff
  block beside the pane, on its machine (a VM tab's too), for its git
  repository: a list of changed files with +/− first (staged, unstaged and
  untracked against HEAD; or one revision against the working tree; or a
  range). Tap a file for its unified hunks, highlighted; tap a line to
  open a file block there, scrolled to it and marked. `illogical diff
  [%N] [REV_A [REV_B]]` and `illogical view [%N:|mN:]PATH[:LINE]` open them
  from a shell, `show_changes` and `show_file` from MCP, and *Open file*
  on an agent's tool call opens the file it touched.
  - **Live while looked at.** Both follow the files as they change (every
    second here, every 3 seconds on a VM), but only while some client
    draws them; with nobody looking they stop, so a VM can sleep, and
    catch up when someone looks again. The file block keeps its mark on
    the same line of text when lines above it change.
  - **Read-only.** Nothing in them edits or reverts. What they show is in
    their state, so a shared session's viewers see the same; changing it
    (opening a file's hunks, moving the mark) needs editor, and pointing a
    file block at another file, or opening one, is the owner's.
  - **Caps.** A file's diff over 256 KB shows as too big (open the file),
    a binary file as binary; a file block shows the first 1 MiB. `git`
    runs on the block's host (through the provider on a VM) and never
    takes the repository's lock. `capture --text` is the unified diff, or
    the file.
- **chant workspaces** (M34). A [chant](https://intentius.io/chant)
  workspace (a directory with a `chant.workspace.json`) as a block:
  `illogical workspace [DIR] [--env E]`, *Open as workspace* in a pane's
  menu or the directory picker when the directory holds one, or
  `open_workspace` from MCP. It shows the gates waiting for a person first,
  then a card per member (its kind, why chant doesn't read it, errors and
  warnings from `check`, releases, gates), then the records with what
  blocks them and pin drift.
  - **Members.** *Shell*, *Agent* and *Changes* open a terminal, a Claude
    Code agent block or a diff block in the member's directory, beside the
    workspace. A nested workspace's *Open* opens it as a block of its own.
  - **Gates are attention.** An op stopped at a gate (`chant run` exits 3)
    is `needs_input`: "delivery: ship waits at gate approve-ship". It's a
    card on the swarm's rail (one per workspace), first in the phone's
    *Needs you* list, and a push notification. *Approve* there or on the
    block runs `chant approve <op> <gate> --approver <you>` in the member's
    directory, so chant's ledger says who: the owner by their illogical
    name, an editor by theirs. A shared session's viewers see the gate and
    can't approve it. *Run op* opens a pane running `chant run <op>`, which
    walks through once the gate is approved. Approvals are in the block's
    history (`illogical history`) and the audit log, with who.
  - **Read through chant.** The block runs the workspace's own chant
    (`$CHANT`, else `node_modules/.bin` there or in a parent, else `PATH`,
    with your shell's environment) for `workspace ls`, `check`, `records`
    and `status`, and says so when there's none ("run npm install"). A
    read costs a few CPU-seconds, so it reads on open, on *Refresh* and
    after an approval, and otherwise only when git says something changed
    (HEAD, `chant/lifecycle`, the working tree): every 3 seconds while
    it's drawn, every 5 when it isn't (on this host; on a VM only while
    drawn). It never fetches, so a gate reached in CI shows once someone
    fetches `chant/lifecycle`. `illogical call %N approve|refresh|member|state`.
- **Your editor in the swarm** (M28). VS Code, Cursor or nvim on any of
  your machines shows up in the swarm beside your panes: a tile of kind
  editor in its project, with its file, its errors and unsaved files, and
  the lines around its cursor as its preview.
  - **Joining.** VS Code and Cursor: illogical's extension (`illogical
    editors install`, or the VSIX from `illogical editors vsix`), then
    *illogical: Show this workspace in the swarm*. nvim: `editors/nvim`
    (illogical.nvim) and `:IllogicalJoin`. Each folder joins only when
    asked, and that's remembered for it; *Take this workspace out of the
    swarm* (`:IllogicalLeave`) removes it at once.
  - **Where.** The editor talks to the illogical daemon on the machine its
    files are on: under Remote-SSH the extension runs on the remote
    machine, so it's that machine's daemon and that machine's cluster. In
    a dev container, the dev container feature in `editors/devcontainer`
    mounts the daemon's editors' socket (`<state>/editors/sock`, which lets
    an editor join and nothing else).
  - **Following.** Click its tile (or *Follow* on a card, or an editor
    block's right-click) for a read-only view of the file it has open that
    follows its cursor across files, with its selection, the file's
    diagnostics and the debugger's line. The editor sends this only while
    someone follows (its status bar says how many), only for files open in
    it, and only to the people following, on their own end-to-end
    connections; summaries carry no file contents or cursor, and control
    sees only that an editor exists. From the view: *Continue* a paused
    debugger, *Open here* (the same file and line in VS Code or Cursor on
    this computer or over SSH to that machine, or in an editor block
    there), and *Ask Claude* (puts `@file#L3-5` in Claude Code's prompt in
    a terminal on that machine).
  - **Cards on the rail.** The debugger stopping (*Continue*), errors that
    a save brought, and a merge conflict that's open. Dismiss clears one.
  - **Who sees it.** An editor isn't in a session: it's yours, and on a
    team's daemon its members' by their team role. Following is viewer
    access; *Continue* needs editor.
- **illogicald as Claude Code's IDE** (M28). Claude Code in a pane
  connects to illogicald the way it does to VS Code (every pane has
  `CLAUDE_CODE_SSE_PORT`; `--no-claude-ide` turns it off), so each edit it
  wants to make (Edit and Write, in default mode) waits as a diff card on
  the pane and on the swarm's rail. *Accept* (or *Change…* first) and
  Claude Code writes it; *Reject* and it doesn't. Its terminal prompt
  still works: when the terminal answers first, the card closes and says
  so. Anyone who may drive the pane's session may answer; viewers see the
  diff. The connections are held by a small relay process that outlives a
  daemon restart, so Claude Code (which never reconnects by itself) keeps
  its IDE and the card comes back. illogicald registers with no workspace
  folders, so Claude Code anywhere else never picks it; if you'd rather
  have diffs in VS Code with Claude Code's extension, `illogical ide
  --diffs "Visual Studio Code"` (or *Diffs here* on a card) passes them
  there. Bash and other tools stay with the hooks in [*Claude Code in a
  pane*](cli.md#claude-code-in-a-pane).
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
  - *From now on…* on a card keeps a standing rule on this machine
    (#166): the tool, or commands starting with a prefix, in the block's
    directory and below or in every agent block. New blocks never ask for
    what a rule allows. *Permission rules…* in the session menu (or
    `illogical rules`) lists them, and forgets them.
  - A block can start with rules and a mode (#163): `illogical agent
    --allow Bash --permission-mode auto`, or `allow` and `permission_mode`
    on MCP `start_agent`, so a lead pre-authorizes its subagents (an agent
    can't start one in `bypassPermissions`). `--user-settings` gives Claude
    Code your settings (allow and deny lists, default mode, `CLAUDE.md`)
    with every hook off, as an opened conversation has.
  - The block's log is the JSON-RPC stream; `capture` is the transcript as
    Markdown, `history` lists the agent's commands and turns, `search`
    covers what agents said and ran.
  - A local agent server runs in its own scope with its pipes in systemd's
    FD store, so restarting the daemon mid-turn (even with an approval
    open) doesn't touch it. After a reboot the session reopens with
    `session/resume` (or `session/load`), unless the policy is `none` or
    `rerun-ask` (then *Resume*). A Fountain turn that ran on while nothing
    followed it shows "running on Fountain" and appears when it ends.
  - The adapters, pinned, go in `~/.local/share/illogical/agents/`. When
    one isn't installed (or there's no Node 20+), *Start an agent…* and
    the block say so, with the command to copy and *Install*, which runs
    it in a new pane:
    `npm install --prefix ~/.local/share/illogical/agents/claude @agentclientprotocol/claude-agent-acp@0.85.0`
    and `npm install --omit=optional --prefix ~/.local/share/illogical/agents/codex @agentclientprotocol/codex-acp@2.1.0`
    (Codex uses `~/.local/bin/codex`). They need Node on PATH (a Node
    mise installed is used if there's none). A test keeps these in step
    with the pins in `defs.rs`.
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

- **Claude Code conversations** (M33). Every Claude Code conversation on
  the daemon's machine, from a terminal or the desktop app's Code tab, can
  be opened as an agent block and carried on there. *Claude Code
  conversations…* (a pane's menu; *Conversations* in the phone's sheet;
  the agent dialog's link; Ctrl-] `C` in `illogical tui`) lists them by
  folder, newest first, with a search box, *Open now* and *All*.
  - **Opening one** shows its transcript (prompts, replies, tool calls and
    their output, compactions and rewinds as notes) in a stopped agent
    block. Nothing runs, and the block keeps reading the transcript as it
    grows, so a terminal session can be followed from the phone. Picking
    one a block already has goes to that block; one running in an
    illogical pane goes to the pane.
  - **What Continue won't remember** is folded away and dimmed, under a
    note: *Not in what it remembers*. A resume follows one branch of the
    transcript (the newest `last-prompt` leaf, walked back by
    `parentUuid`), so a rewind's abandoned turns, another writer's turns,
    or an exchange an away summary cut off aren't in it, though the block
    shows them (#79).
  - **Continue** (or just send a message) freezes what it had into the
    block and resumes the session through `claude-agent-acp`, with your
    settings, skills and `CLAUDE.md` as in the terminal, every hook off,
    and the model it last used. `claude --resume` in a terminal afterwards
    shows the new turns. From then on it's an ordinary agent block, and it
    comes back after a restart or a reboot.
  - **One that's open somewhere else** (a terminal, the desktop app, a
    pane) can't be continued: two writers would each lose the other's
    turns. *Fork* makes a new session with its history and goes on in
    that, leaving the original alone.
  - The list leaves out `claude -p` and SDK runs (agent blocks among them),
    sessions archived in the desktop app, and ones whose folder is gone
    (except the desktop app's, whose scratch folder goes with them, and
    comes back empty if you continue). *All* shows everything.
  - **Every host's** (#78): with more than one host, the picker lists
    each host's conversations under its name, then by folder, asking them
    all at once over the fleet's connections and showing each as it
    answers. A sandbox that's asleep isn't woken to be asked, and a host
    that doesn't answer in 5 s says so. Picking another host's opens it
    on that host and shows it there, where it continues.
    `illogical claude ls --host all` does the same in a terminal;
    `illogical --host NAME claude open ID` opens one there.
  - Claude Desktop's chats aren't here: they live on claude.ai.
  - **On a Mac** (#81) it works the same way. Whether a session is open
    comes from `~/.claude/sessions` checked against the process's start
    time, which Claude Code writes there as `ps -o lstart` (`/proc` on
    Linux), so a reused pid doesn't count. A Claude Code started from one
    of the daemon's panes or agent blocks is placed by its parent
    processes, with no systemd scopes needed. The desktop app's own
    records (title, archived) are read from `~/Library/Application
    Support/Claude/claude-code-sessions/` (`~/.config/Claude/` on Linux).
    Not yet checked on a Mac: that folder and its fields (jake-mini's app
    hadn't run a Code tab session, so there's none), and whether the app
    deletes a scratch workspace with its session there as on Linux. Its
    Cowork sessions (`local-agent-mode-sessions/`) keep their transcripts
    inside their VM, not in `~/.claude/projects`, so they aren't listed.

- **Studio apps** (M35). An app box from your studio (arugula-salad's) as
  a block: the app in a frame, and its agent's questions as asks you
  answer in illogical.
  - **Your studio.** `illogical studio login https://studio.example` keeps
    a studio token in the daemon (read from stdin; in `studio.json` in the
    state directory, mode 0600, or `--studio-file`). It's never sent to a
    client. `illogical studio logout` forgets it.
  - **Opening one.** *Open a studio app…* (a pane's menu, the `+` button's
    menu; *Studio apps* in the phone's sheet), `illogical app NAME`
    (`illogical app` lists them) or MCP's `open_app`. The block keeps the
    box's address, the app's name and the studio, nothing else.
  - **Getting in.** Each time a client draws the block, the daemon mints a
    fresh ten-minute entry link from studio and the frame goes through it.
    After that the box's own cookie carries the frame. The link is never
    kept: not in the block's config, its log, the frame's address or the
    page's storage. ↻ gets a new one; *Records* opens hud's Decisions,
    Work, Intent or Sessions in the frame the same way. Only the owner may
    get in: a link signs in as them. It needs the box's door to hand out
    hud's session as a partitioned cookie (`SameSite=None; Secure;
    Partitioned`), or the framed box says "You need a link".
  - **Questions.** The daemon follows hud in the box itself (its own
    session, minted the same way, kept in memory only). When the box's
    agent waits on a question, it's a card on the block ("hud asks") and
    an `ask` on the swarm's rail, bundled by the app. The answer goes back
    to hud as the option picked. Answered in hud's own panel, interrupted
    or expired, the card goes. With several tabs asking, one card shows at
    a time. *Skip* closes the card and leaves the question to hud.
  - **Prompts.** Sending to the block prompts the box's agent through the
    daemon's hud session, for scripts and agents: `illogical call %N send
    '{"text":"…"}'` or MCP's `send_input`. (In a browser, hud's own
    composer is in the frame.) It goes to the box's first tab unless you
    name another by title or chat key (`"tab":"Main"`, `send_input`'s
    `tab`). hud queues it behind a running turn and says where it is in
    the queue. Refusals come back as hud gave them: an unknown tab, a full
    queue, or the box's turn budget. The block's history says who prompted
    which tab. hud has no `onBehalfOf` for prompts, so in hud's chat the
    prompt is the session's (the box's owner, or the follower).
  - **Gates.** A release or op waiting at a chant gate in the box is
    attention, the same as a workspace block's: the block lists it with
    *Approve*, and it's a `gate` card on the swarm's rail and in the
    phone's sheet. They come from hud's work board, read again each time
    hud's live feed says something changed, never on a timer. *Approve*
    goes to hud, which approves it only if `workspace status` still lists
    it as pending. The card stays until hud's board drops the gate; if hud
    refuses, the card says why.
  - **Who answered.** Viewers can't answer or approve; editors and the
    owner can. hud records whoever its session belongs to (the box's
    owner). With a follower credential (`hud share --role follower` in the
    box, kept with `illogical studio follower APP`; every block of that app
    then uses it), answers and approvals also name who clicked
    (`onBehalfOf`), and chant's ledger gets that name. hud takes names of
    at most 32 letters, digits, spaces and `-_.'`: an email address goes
    as its local part, other characters as `-`, and `owner` isn't sent.
    That needs hud's trusted-follower change.
  - **After a restart** the block comes back and mints again.
  - **Not yet:** an "Other" answer (hud only takes one of its options).
  - Any browser block takes questions too: `POST /api/panes/%N/ask` with
    `source` and `agent` (who asks), for something that follows a page's
    agent from outside.

- **Pull requests** (M36, Forgejo; M38, GitHub; M39, GitLab). A PR as a block beside the work on
  it: its checks, reviews and timeline, and what it waits on you for.
  - **Opening one.** *Open pull request…* (a pane's menu, the `+`
    button's menu; *Pull request* in the phone's sheet), `illogical pr
    URL | OWNER/REPO#N | N` (N: in this directory's repository), MCP's
    `open_pr`, or clicking a Forgejo PR link (`…/pulls/N`) in a terminal
    (Shift-click opens it in the browser instead).
  - **Your login.** It reads and writes with your own `tea` login, run
    with your shell's environment. The token comes from tea's credential
    helper and is kept in memory only: never in the block's config, its
    log, or anything a client gets. The remote's (or link's) host picks
    the login: one whose URL or SSH host is that host, else the one whose
    Forgejo says the repository's `ssh_url` is there. If none or several
    do, the block lists them to pick from (*Use login …*, `call %N login
    {"name":…}`), and keeps the pick. No tea here, or no login: the block
    says so.
  - **Fresh.** Every few seconds while you look at it or it wants you,
    every few minutes otherwise. Forgejo has no ETags, so a poll is the PR
    and its checks; reviews and the timeline are read again only when the
    PR changed.
  - **What waits on you,** as attention on the rail, the phone and push,
    bundled by repository: a review asked of you (or a team of yours),
    which *Approve* (here, on the rail, or the phone's sheet) sends as a
    review with your login; your PR's checks red (*Failed*: Forgejo has no
    API to rerun them, so the block links the run); changes requested on
    your PR, or a mention since you last looked (*Waiting for you*); your
    PR merged, or green with nothing holding it (*Finished*, once).
  - **Agents draft, people send.** An agent's comment, review or merge
    (MCP's `pr_comment`, `pr_review`, `pr_merge`, or `illogical pr …` and
    `illogical call` run under Claude Code) never reaches the forge by
    itself: it waits on the block as a card with the text to edit. *Send*
    posts it (as edited) with the owner's login; *Drop* drops it. The
    owner and editors may send; viewers can't. The block and its history
    say who sent each one, and that an agent drafted it. Several wait in
    turn. This holds on illogical's own surfaces; an agent on your account
    could still run `tea` itself. A person's own write (the block's
    buttons, `illogical pr comment %N …` in your shell) goes straight out.
  - **The code.** *Diff* fetches `refs/pull/N/head` into your clone (no
    branch is touched), makes a worktree of it in
    `.illogical/worktrees/pr-N` (or `.claude/worktrees/pr-N` where the
    repository keeps its worktrees), and opens a diff block on
    merge-base..head; *Checkout* opens a terminal there. The owner's.
  - `capture --text` is the PR as text; the block's log has the timeline,
    so `history` and `search` find its comments.
  - **GitLab merge requests** (M39). The same block for a merge request:
    open it from its link (`…/GROUP/[SUB/]PROJECT/-/merge_requests/N`, in
    a terminal too), `illogical pr URL`, `GROUP/PROJECT!N`, or N in a clone
    whose remote is gitlab.com (or a `gitlab.` host). It reads with your
    `glab` login's token for that host (`glab config get token --host H`,
    memory only, asked again after a 401) when glab knows the host
    (gitlab.com, its default host, or a host in its config). With none, a
    public project still reads anonymously and the block says *read-only:
    no glab login*: no discussions (GitLab keeps them for logins even on
    public projects), no "you", and no writes. Checks are the head
    pipeline's jobs (allowed failures and manual jobs don't count); a red
    pipeline on your MR offers *Rerun*, which retries it (`illogical rerun
    %N`, the rail, the block). Reviews are each reviewer's state and the
    approvals; *Approve* approves. *Request changes* posts your text as a
    comment (GitLab's API can't set a reviewer's state). Merge is `merge`
    or `squash`. *Diff* and *Checkout* fetch `refs/merge-requests/N/head`
    and diff from `diff_refs.base_sha`, the merge base. A poll is one
    conditional request when nothing moved (gitlab.com counts 304s too).
  - **GitHub pull requests** (M38). The same block for a GitHub PR: open
    it from its link (`github.com/OWNER/REPO/pull/N`, in a terminal too),
    `illogical pr URL`, or `OWNER/REPO#N` / N in a clone whose remote is on
    github.com. It reads with your `gh` login's token (`gh auth token
    --hostname H`, memory only, asked again after a 401). A GitHub
    Enterprise host (API `https://HOST/api/v3`) works the same when gh has
    a login there. Every read is conditional (ETags), so a poll with
    nothing changed is three 304s and costs none of GitHub's rate limit;
    when the limit runs low the block polls once a minute and says so.
    Checks are check runs and commit statuses both; a review asked of a
    team you're in counts as asked of you; branch protection blocking a
    merge holds *Finished* back. Red checks on your PR offer *Rerun*
    (`illogical pr rerun %N`, `illogical rerun %N`, the rail, the block),
    which reruns each red workflow run's failed jobs. Merge is `merge`,
    `squash` or `rebase`.
  - **Live updates** (M40). A poke from the forge makes the block read at
    once, and while the webhook path is healthy (something heard from it
    in the last ten minutes) the block polls only every few minutes, even
    while you look at it; the block's footer and its state say *live* or
    *polling* (and why), and `capture --text` says `live: webhook` or
    `live: polling (why)`. Issues on the same repository hear theirs too.
    - **GitHub** needs nothing on the block: a daemon joined to illogical
      control tells control which repositories it has blocks on, and
      control's GitHub App relays its webhooks (only "something changed on
      OWNER/REPO#N", never the event's contents) to the daemons of people
      it may tell: signed in to control with GitHub, the App installed on
      the repository's owner, and the repository theirs or one GitHub lists
      them as a collaborator on. Control's heartbeat each minute keeps it
      *live*.
    - **Forgejo and GitLab**: *Live updates* on the block (the owner's;
      `call %N live '{"on": true}'`) makes a webhook on the repository with
      your login, pointed at this daemon's tailnet address, with a secret
      made here (kept 0600 in `secrets/forge-hooks.json` in the daemon's
      state directory). The daemon takes only deliveries signed with it
      (Forgejo's `X-Forgejo-Signature`, GitLab's `X-Gitlab-Token`). *Stop
      live updates* removes the webhook. An agent's `live` is a draft, like
      any other write to the forge. The forge must reach the daemon over
      the tailnet; a hook that goes quiet just means polling again.
  - **A box with no `gh` login** (a hosted sandbox) joined to control
    reads GitHub through control's App: a read-only token for that one
    repository, held in memory until a minute before it expires. Who you
    are comes from your control sign-in, so a review asked of you still
    reaches the rail. Every write, a person's or an agent's draft, is
    refused there: writes go out as you, with your own `gh` login.

- **Issues** (M37, Forgejo and GitHub). An issue is the same block: its
  labels, assignees, the pull requests that refer to it and its timeline,
  read with the same `tea` (or `gh`) login. GitLab's issues aren't read
  yet.
  - **Opening one.** *Open issue…* (a pane's menu, the `+` button's menu;
    *Issue* in the phone's sheet), `illogical issue URL | OWNER/REPO#N | N`,
    MCP's `open_issue`, or clicking an issue link (`…/issues/N`) in a
    terminal.
  - **What waits on you:** an open issue given to you, or a mention, since
    you last looked (*Waiting for you*); one given to you that closes
    (*Finished*, once). A closed issue asks nothing else.
  - **Agent on this** (the owner's): a branch `iNN-<slug>` (from the title)
    off the repository's default branch, fetched fresh, in a worktree of
    its own (`.claude/worktrees/` where the repository keeps them, else
    `.illogical/worktrees/`). The branch tracks nothing, so a plain `git
    push` can't land on main. The issue moves to a tab of its own (named
    `#N`), and Claude Code (or `{"agent": "codex"}`…) starts beside it in
    the worktree with the issue's link as its prompt, told to open a PR
    from the branch that closes the issue. The issue's title and text go
    in a marked block the prompt calls its author's, to read as a
    description and not to follow as instructions (anyone who can open an
    issue on the repository writes them), and the agent starts with
    nothing allowed ahead of time (no rules, no permission mode, not your
    own Claude Code settings): what it wants to do comes to you as a card
    first. *With instructions…*
    adds to the prompt (yours, outside that block). The block looks for a PR from that
    branch (every 30 s, faster while you look) and, when one appears, opens
    it beside the agent, once. `illogical issue agent %N` does the same.
  - **New issues.** `illogical issue new -t TITLE [-b TEXT]` (in a clone,
    or `--repo`) opens one with your login, and the block shows it. An
    agent's (MCP's `issue_new`, or the CLI under Claude Code) is a draft:
    a block holding a card with the title and text to edit, which *Send*
    opens on the forge (the block becomes the issue) and *Drop* drops.
    Comments on issues (`issue_comment`, `illogical issue comment %N`) are
    drafts from agents too, as on a PR.

- **Fountain agents** (M43). The agents on your Fountain account as a
  catalog block: a card each, filters, and ways to run one.
  - **Opening it.** *Fountain agents…* (a pane's menu, the `+` button's
    menu; *Fountain agents* in the phone's sheet), `illogical fountain`
    (`-q WORDS`, `--source agent-specs`, `--profile P`), or MCP's
    `open_fountain`. `illogical fountain agents [WORDS]` lists them in the
    terminal without a block.
  - **Your login.** It reads with your own `fountain` CLI login, on this
    host with your shell's environment: `FOUNTAIN_API_KEY` (and
    `FOUNTAIN_BASE_URL`), else the profile's `api_key` and `base_url` in
    `~/.fountain/credentials` (`FOUNTAIN_PROFILE`, else `default`; the
    block's owner can pick another). The key is held in memory only. A host
    with no login says so on the block.
  - **The cards.** Name, where it comes from, runtime and model, its
    environment, sandbox provider and mode, conversations, when it last
    changed, the description, skills and MCP servers. Read when it opens,
    then every 3 minutes while it's on screen, and on *Refresh*.
  - **Where an agent comes from.** *agent-specs*: `managed-by: chant` in
    its metadata (the curated ones). *App-made*: `switchyard`, `salon`,
    `paddock`, `drydock`, `part-of` or `attemptId` in its metadata, a name
    ending in a UUID, or a name an app gives (`Mend: github.com/…`).
    *Hand-made*: the rest. Every agent is listed: app-made ones are
    filtered, never hidden.
  - **Filters.** A search over names, descriptions, skills and MCP
    servers, and chips for source, runtime and sandbox provider. They're
    the block's (kept in its config), so the phone and every client see
    the same list, and `capture --text` is that list.
  - ***Run on Fountain*** opens an agent block running it on Fountain
    (`fountain acp --agent NAME`) beside the catalog. ***Run here*** (for
    `claude` agents; below) wears it in a Claude Code on this host.
    ***Spec*** opens the agent-specs file
    that declares a `managed-by: chant` agent (a `.ts` under `src/agents`
    with its `name:`) as a file block; it looks in the checkout you pick
    (`~/agent-specs` if it's there). Any other agent, or with
    no checkout, opens its page on Fountain.
  - **Read-only.** agent-specs stays the one place a curated agent is
    edited; the catalog never writes to Fountain.
  - **For agents.** MCP's `list_agents {query?, source?}` (compact rows)
    and `read_agent {name}` (the whole recipe; a server's credentials show
    as their `${VAR}`s, since Fountain never returns a secret) let a local
    agent see the team, and `start_agent` with a Fountain agent hands one a
    task.

- **Wearing a Fountain agent here** (M44). A Claude Code agent block on
  this host, configured as one of your Fountain agents: its system prompt,
  its skills and its MCP servers, working in a folder of yours (a worktree)
  with your own tools.
  - **Ways in.** *Run here* on a catalog card (it asks for the folder, and
    offers the last one next time), `illogical agent --as NAME "prompt"`
    (in the current directory, or `--cwd`), and MCP's `start_agent {agent:
    claude, as_fountain: NAME}`. Only the owner can: it runs on the
    owner's machine with their secrets (an editor's *Run here*, or a
    block of theirs with `as_fountain`, is refused).
  - **The bundle**, built from the agent's recipe and cached in
    `~/.cache/illogical/fountain/<agent id>/<updated_at>/` for a day (a new
    version beside the old ones, which stay a week for blocks still on
    them): a
    plugin (`fountain-<name>`) whose skills are the agent's (inline ones
    written out; GitHub ones copied from shallow clones kept in
    `…/fountain/github/`, fetched again daily), and the system prompt,
    after a short preamble saying it runs locally (where it mentions
    `/home/sprite`, `/workspace`, vaults or spawning, those describe the
    sandbox). The session gets them as `_meta.systemPrompt.append` and
    `_meta.claudeCode.options.plugins`, the agent's model (`anthropic/`
    taken off), and still no settings sources, so your own hooks stay out.
  - **MCP servers** come with their `${VAR}`s resolved by Fountain's rules
    (`$$` is a literal `$`; values aren't expanded again). Each variable's
    value comes from, in order: Infisical (in your agent-specs checkout,
    through the agent's environment in `dist/fountain.yaml`, and its vault
    over that (`--vault`, or the only one it may use), to an `infisical://`
    URI, read with your `infisical` login; a variable neither maps is
    tried as itself in env `dev`, and the header says so), your shell's
    environment, then `gh auth token` for `GITHUB_TOKEN`.
  - **Never on a command line.** Claude Code's SDK puts the session's MCP
    config on `claude`'s command line, which anyone on the machine can
    read. So the session gets references (`${ILLOGICAL_FTN_…}`, which
    Claude Code expands itself), and the values are in the adapter's
    environment, readable only by you. A server whose command or arguments
    hold a variable is left out (its own command line would show it), as
    is one with a literal `${…}` (Fountain's `$$`), which Claude Code would
    expand. illogical's own MCP token goes the same way for every local
    Claude Code block (#128). Values are held in memory: never in the
    layout, the block's log or state, `capture`, MCP results or the web
    (the log shows `<redacted>`). Note that the agent's own commands
    inherit that environment.
  - **What doesn't carry over** is named in the block's header, which
    says "as NAME" with the skills and servers that came: a server with a
    variable nothing has, one that needs an OAuth sign-in (a headless
    Claude Code can't: no credentials, and a known OAuth host such as
    mem0's, or a `401` with `WWW-Authenticate`), a Fountain connection,
    and GitHub skills that couldn't be fetched.
  - **Not wearable:** agents of another runtime (*Run on Fountain*), and
    ones whose metadata says `illogical.local: false` (the orchestrators,
    written for Fountain's sandboxes); the card greys *Run here* with the
    reason. After a restart the block puts the agent on again before it
    takes over the agent still running (or starts it again).

- **This machine as the Fountain runner** (M45). The setup (the README's
  *A Fountain runner*) makes a `fountain` user, the `fountain-runner`
  systemd unit and a sudoers rule. A Fountain block's `view: runner`
  (*Fountain runner…* in a pane's or the `+` button's menu, `illogical
  fountain --view runner`, MCP's `open_fountain {view: "runner"}`) shows:
  - **The runner.** This host's (the unit's `--name`): online or offline,
    its version against the installed `fountain --version`, when Fountain
    last saw it, and how many sandboxes it holds; then every other runner
    on the account. Read from `GET /api/runners` every minute while it's on
    screen and every 5 minutes otherwise, so its attention still fires.
  - **Attention** (`failed`, one *Fountain runner* card on the rail):
    the unit is active (`systemctl is-active`) but Fountain has said the
    runner is offline for 5 minutes; or another runner is online, which
    would win placement (Fountain puts a runner conversation on the most
    recently connected one). It's raised once per change and clears by
    itself.
  - **Its sandboxes**, newest first (`GET /api/sandboxes`, this runner's),
    each with its directory, agent and conversations. A parked
    (suspended) one still opens:
    - ***Follow*** opens an agent block on a conversation: `fountain acp
      --agent NAME`, which loads it (`session/load`) and replays it. The
      block is marked `follow`: if the conversation can't be loaded (or the
      agent can't load one), it stops and says why; it never starts a new
      conversation.
    - ***Changes*** opens a diff block for each git checkout up to two
      levels down in the sandbox, from its upstream's merge base (else
      `origin/HEAD`'s, else an empty tree: everything in it).
    - ***Shell*** opens a terminal as `fountain` in the sandbox, with
      `HOME` the `fountain` user's own (`/home/fountain`), reading no
      profile, rc, inputrc or history file (`bash --noprofile --norc`,
      `INPUTRC` and `HISTFILE` `/dev/null`: the sandbox's agent may have
      written them). It runs outside
      the runner's sandboxing (the unit's protections don't apply) and it
      isn't Fountain's, so parking the sandbox doesn't stop it; the block
      says so.
  - **Read as `fountain`.** The sandboxes are the `fountain` user's (mode
    0700), so every read and shell goes through `sudo -n -u fountain
    /bin/bash -c SCRIPT _ ROOT DIR`, the one command the sudoers rule
    allows, with the directories as arguments, never in the script.
    - A sandbox's directory is the unit's `--root` and its name, only: a
      path Fountain gives must be exactly that, and each script checks the
      directory's real path is inside the root (a symlink out is refused).
    - Every git there reads only: no global or system config, no hooks,
      fsmonitor, pager, external diff or textconv, no remote contacted (no
      lazy fetch of a partial clone's objects, every protocol refused), and
      every filter driver
      the repository's config names emptied, so a repository's own config
      can't run anything when it's read.
    - A diff block takes `run_as: "fountain"` for this: only that user,
      only on this host, only under the unit's root. It has no *Open file*
      (that would read the sandbox's files as you).
  - **The machine's line.** `GET /api/host` has `fountain_runner` (name,
    online, version, sandboxes, and what wants you) on a host with the
    unit, from what was last read (never Fountain on the request). The
    host menu, the phone's host list and the swarm's bar show it.
  - **The owner's.** *Follow*, *Changes*, *Shell* and switching the view
    are the owner's; an editor sees the list. `capture --text` is the
    view as text.

- **Your machines through control** (M48, #151). The main way to reach
  more than one machine: each one runs `illogicald join` once, and control's
  page (in a browser or the desktop app) lists every machine of your
  account and your teams in its host menu, reaching each directly when it
  can and through control's encrypted relay otherwise. No machine is
  special: none keeps a list of the others. See [control.md](control.md).
- **Other hosts over the tailnet** (M4a, the older model, still there for
  the CLI's `--host` and sandboxes). Every daemon is a peer; the one the
  page comes from (the "home daemon") keeps a list of the others and checks on
  each every minute. The page shows a host switcher (desktop: the bar's
  left end; phone: the sheet), and each host has its own sessions and tabs.
  Switching connects straight to that daemon; nothing is relayed, and the
  host you left gets no connection (so a sandbox can sleep). The list is
  remembered in the browser, and the page itself by its service worker, so
  the other hosts stay reachable while the home daemon is down.
  `illogical --host NAME …` runs any command on another host. A sandbox
  (a sprite, a container: no systemd needed) gets a static daemon on the
  tailnet with one command and adds itself to the list; see *Use it*.
- **Panes from several hosts in one layout** (#17). The home daemon's
  tabs and splits can hold panes that run on another host in its list:
  *New tab on box* (the `+` button's right-click menu), *Split right on
  box* (a pane's menu), or `illogical --host box run --home`. The page
  connects to that host directly for the pane's bytes (the home daemon
  keeps only where it is, and relays nothing), and it moves, docks and
  breaks out like any pane, live in every window. Its terminal is the
  host's own: its size follows its place here, and its restart policy and
  history are the host's, where it sits in a session named after the home
  daemon. While the host is down the pane says so and greys out; it comes
  back by itself. Closing it here closes it there; if the host can't be
  reached, it stays open there. A pane its host closes (it exited, or was
  closed on the host's own page) leaves the layout here too.
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
- **A pane for a guest with only OpenSSH** (M65). `illogical share --guest
  %3` prints an `ssh` command to send someone: the username is a one-time
  token and the daemon's host key is pinned in the command, so nothing is
  saved on their side. The daemon's own ssh server (`--guest-ssh`, port
  7684) listens only while an invite exists, and a session can only watch
  that pane: no shell, no account, no commands, no forwarding. Read-only
  unless `--rw`, which types under the one-driver rule with the guest's
  `--name` on their input. Invites are single use unless `--reusable`,
  expire (an hour by default; a day at most, two hours with `--rw`), end
  with the pane, and `illogical guests revoke ID` cuts the guest off at
  once.
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

- **In any terminal** (M31). `illogical tui` (with `--host`, any host;
  `--session S` to start in one) draws the shown tab's panes in the
  terminal you're in, beside a sidebar of sessions and tabs, each tab
  marked with its panes' worst attention (● needs you, ✓ done, ◌ working),
  and a *needs you* list with each pane's reason. It's a client like the
  browser, so both edit one live layout.
  - **Keys** reach each program encoded for the modes it set, by Ghostty's
    own encoder: application cursor keys, modifyOtherKeys and the kitty
    keyboard protocol, so Shift+Enter in Claude Code and Neovim's kitty
    keys work when the outer terminal reports them (Ghostty, kitty,
    WezTerm, iTerm2, foot). Programs are told kitty keys are there while a
    TUI is attached to their pane. Pastes are bracketed when the program
    asked; focus changes are reported to programs that want them.
  - **Ctrl-]** then: `v`/`s` split right/down, `c` new tab, `x` close,
    `o` or arrows to move focus, `z` zoom, `[` copy mode, `n`/`p` or
    `1`–`9` tabs, `r` rename the tab, `m`/`t` the pane's and tab's menus,
    `w` the sidebar, `b` hide it, `q` detach, `?` all of these, Ctrl-]
    again to type it.
  - **The mouse**: click to focus, drag dividers, Alt-drag a pane onto
    another's edge to move it (or its middle to swap), right-click a pane,
    tab, session or *needs you* entry for its menu (the browser's: split,
    zoom, move, restart policy, shell integration, forget history,
    dismiss, close). The wheel scrolls back through a pane's history
    (Shift+PgUp/PgDn too) under a dim ↑ marker saying how far, until you
    type; it sends arrow keys to a pager or editor, or goes to the program
    if it takes the mouse.
  - **Selecting and copying** (M32). Drag to select within a pane, double-
    click a word, triple-click a line; letting go copies. When the program
    takes the mouse, Shift-drag selects. Copies go to your terminal's
    clipboard through OSC 52, so they work over ssh (in tmux, with
    `set-clipboard on`). The text is what `illogical capture` would print:
    soft-wrapped lines joined, no trailing blanks.
  - **Copy mode** (`Ctrl-] [`): hjkl or arrows, Ctrl-U/D/B/F and PgUp/PgDn,
    `0` `$` `g` `G` move; `v` selects, `V` selects lines, `y` or Enter
    copies and leaves; `/` and `?` search down and up (lower-case ignores
    case), `n`/`N` again; `[` and `]` jump between prompts and `o` selects
    a command's output, both by the shell integration's marks, so `o` `y`
    copies what `illogical capture --last-command` prints; `q` or Esc
    leaves. A search that runs out of the 10k rows the TUI holds reads the
    pane's saved output (up to 32 MB of it) and keeps looking; that deeper
    history shows until you leave.
  - **The sidebar** (`Ctrl-] w`): arrows move, Enter goes there, and on a
    *needs you* entry `a` allows, `A` allows always, `d` denies and `x`
    dismisses, without opening the pane.
  - **Agent blocks** show as a transcript (messages, thoughts, tool calls
    with their output); `a`/`A`/`d` answer the permission request at the
    bottom, `i` sends a message. Questions and forms are answered in the
    browser. Web pages show their address.
  - Each pane's cursor shape and color, its title in the status line, and
    synchronized output (a program's frame is drawn whole) are kept. A
    frame takes about 1 ms to draw with four busy panes at 200x50.

- **Every host at once** (M25, M30). The page keeps a light connection
  (summaries only) to every machine in its list, not just the one it
  shows: yours, your team's, and teammates' machines that shared a session
  with you or with the team. Through illogical control they share one
  connection to the relay. A machine that goes away greys out with when it
  was last seen and comes back on its own; a sandbox that's asleep isn't
  woken to be counted; reconnects after a laptop wakes are spread out.
  Private panes never leave their owner's view, and revoking a share or
  locking a team takes those panes off everyone else's screen within a
  second.
- **Team answers** (M29). When an agent on any of the team's machines asks
  something (an agent block, or Claude Code in a terminal through its
  hooks: see [*Claude Code in a pane*](cli.md#claude-code-in-a-pane)),
  anyone who may edit that session can answer: from the card beside the
  pane, the swarm's rail, or a notification (on a desktop the
  notification's buttons answer it directly). The first answer wins, and
  every card, the pane's history (`illogical log %N --who`) and the audit
  log say who answered. A *Send a follow-up* box gives the agent its next
  instruction, as its sender's input; on someone's own machine a teammate
  needs their trust first. Cards show who else is looking. Who gets
  notified is opt-in per person (*Notify me about its agents*).
- **Invites** (#233). "Bring Sam into this" in one step: *Share and
  notify* in *Share session…*, `illogical invite sam`, or `POST
  /api/invite` (the owner's only) shares the session (or upgrades a
  share; never downgrades one) and pushes that one person, "Alex brought
  you into api-work: take a look at the flaky test", opening at the pane,
  whatever anyone's notification settings. It reports what happened, not
  what was tried: *sent* when a subscription took it, *pending* while
  someone outside your teams hasn't accepted the machine (retried after
  each refresh for a day), else *unreachable* with why. People are named
  as the machine itself knows them: tailnet logins, people already shared
  with, and members of rosters it checked against a team pin your own
  browser gave it, never on control's word. A team's members on a team's
  machine already hold their role: they're just told. `--drive N` also
  trusts an editor to type on your machine for N minutes. Audit-logged.
- **An agent asks to invite someone** (#234). MCP's `invite_person
  {who, role?, pane?, note}`, from any of your own agents, Claude Code in
  a terminal included, shares nothing: it opens a small invite block
  beside the agent with a card, "claude-code (pane %3, you started it)
  wants to bring Sam [account:s1] (editor) into api-work at pane %3: …",
  whose names are the session's and the person's as the machine knows
  them when it's shown (the principal beside the name), and pushes it to you alone (not to
  editors who opted in), with no buttons to send it from. Only the
  session's owner answers it, by any route (the card, the push, the
  swarm, the CLI): *Invite* sends #233's invite as you, with the role and
  note as you left them and drive trust only if you set it; *Decline*
  tells the agent, with a reason if you give one. Editors, and agents
  (`agent_respond`, the CLI under one), are refused. An agent a guest
  started (or one such an agent started) can't ask at all; nor can an
  agent skip the card: `illogical invite` and `/api/team-pins` under one
  are refused, and only `invite_person` makes invite blocks. Unanswered, it's
  dropped after a day. Closing the invite block is yours alone too
  (editors and agents are refused); what still waited is dropped, and
  `read_invite` says so. `read_invite` tells the agent which: waiting,
  sent (with the delivery), declined, dropped or failed. Each agent has at
  most five waiting. The card lives on its own block, so it never
  replaces the agent's own permission or question card. The audit log
  names you as sender and the agent and its pane as drafter.
- **The swarm** (M26, `/#swarm`, *Swarm* beside the tabs). Every pane on
  every machine you and your team can see, as one field of tiles coloured
  by kind and lit by activity, clustered by project (or directory, outside
  a repository), machine, kind, session or person. What needs you lifts out
  to a rail of cards bundled by cause ("3 failed on build-02", "2 agents
  ask"), where you allow, deny, answer or dismiss them all at once, and
  send an agent its next instruction. Hover a tile to peek at its last
  lines, click it to open it (an editor that joined: follow it). On a
  phone the cards are a strip along the bottom. `just fake-fleet` runs three throwaway machines to try it on.
- **Swarm themes** (M41). *Theme* in the swarm's bar picks how it's
  drawn: *blocks* (the field above, the default) or *city*, the same panes
  in 3D, remembered per browser. In the city a cluster is a block, its
  rows are machines, and each pane keeps its lot (in pane order) until you
  regroup. A building's height is how long its command has run (log
  scale), kept after it's done; a lit roof means still running; its
  windows scroll while it prints, stay lit a while after, and go dark when
  it's quiet; colour is kind, and shape says whether it finishes (box),
  runs until stopped (drum), is an agent (hexagon), an editor (pentagon)
  or a pull request or issue (slab). A red roof is a failed last command,
  a beam is something that needs you (taller the longer it waits), a
  marker over a building is a teammate with it open (a cone while they
  type). The rail, hover peeks, click to open, *Show* and notification
  links work as in blocks. *How to read the city* under the legend says
  all this on the page. three.js loads only when the city is picked.
- **Hive and timeline themes** (M42). Two more choices under *Theme*.
  The *hive* is one hex cell per pane, packed into a comb per cluster, and
  it reads flat, so it suits a phone. A cell fills as its command runs
  (log scale, full at an hour), bright while running and faded once done.
  What runs until stopped is full and hatched. The cell's edge pulses
  while it prints, and a red rim means its last command failed. A pane
  that needs you fills with the reason's colour and shows how long it has
  waited, and its glow spills onto the cells around it, wider the longer it
  waits. The *timeline* is one lane per pane under its cluster's name,
  showing the last 40 minutes with now at the right edge, so you can see
  what happened while you were away. Each command is a bar as long as it
  ran, coloured by kind, with stripes for how much it printed and a red cap
  if it failed. The bars come from each machine's command history, as in
  `illogical history`. A pane that needs you gets a band from when it
  started waiting until now. Hover, click, *Show*, *Fit* and the key work as
  in the other themes.
- **Tools for any agent** (M16, MCP). Claude Code, Codex or any MCP client
  gets illogical as tools: `run` a command in a pane you can watch and
  take over (here, on a throwaway VM, or a sandbox; it outlives the
  agent's turn), `wait` for it and `read_output`, `send_input`, `list`,
  `close`, `history` and `search`, `open_port` (a dev server in a browser
  block beside its terminal), `start_agent` and `agent_respond` (one agent
  supervising another), `read_file`. `claude mcp add illogical --
  illogical mcp` sets it up; see the README. Output comes in pages, a long
  wait sends progress and answers "still running" by 100s with where to
  pick up, and errors say what happened ("pane %7 is gone; its last
  command `make` exited 2 3m ago"). A pane an MCP client started says
  "started by mcp:claude-code", and what it typed is in history as theirs.
  The tools' annotations are honest (read-only, destructive), so a
  client's permissions can allow the readers and ask before the rest.
  Every agent block gets it too, scoped to its own tab: it can start a dev
  server beside itself and show it in a browser block, start and answer
  other agents there, and read the rest of its tab, but not touch other
  tabs. An agent in a VM gets it through a relay the daemon opens into
  its VM, and what it runs lands on its machine. Over HTTP (`/mcp`), the owner gets in as for the web client;
  anything else needs a token from `illogical mcp token`, revocable at any
  time.
- **Threads on panes and sessions** (M61). Every pane and every session has
  a thread where the people working on it talk: *Thread* in a pane's menu,
  *Session thread* in the session menu, or the bubble on a pane. Messages
  arrive live on every window and phone. The machine that owns the pane
  keeps them (`<state>/threads/`), so they survive restarts and upgrades,
  outlive the pane, show up in `search`, and never pass through control
  unencrypted. Watchers read and drivers post. A private pane's thread is
  its owner's, and someone shared "from now" sees messages from then on.
  Each person has their own unread count: on the pane's bubble, a dot on
  the session button, and a folded corner in the swarm (blocks, hive and
  timeline; the city doesn't draw it yet). `@name` notifies someone, on
  their phone too (a tap opens the thread; team members are reached
  whether or not they're connected). *Quote selection in thread* posts terminal output as a
  quote that stays readable after the pane scrolls; clicking it jumps back
  to the output. `@agent` (or `@claude`) in a pane's thread goes to that
  pane's agent as a follow-up, from whoever may drive it, and agents read
  and answer with the MCP tools `read_thread` and `post_thread`. Only an
  `@` that reached someone is marked in the thread; one that reached no
  one (a name nobody here who can read the thread has, or an `@agent` in a
  session's thread or from someone who can't drive the pane) stays plain
  and the poster, and no one else, is told so under the message (and in
  `post_thread`'s `unreached`).
- **An @ of someone who can't see the thread offers to invite them**
  (#297). When you, the owner, write "@sam look" and Sam is someone this
  machine knows (shared with elsewhere, or on a roster it checked) but
  can't read that thread, the composer says "Sam can't see this. Invite
  them?" in place of the "nobody" note. One click is #233's invite, as a
  viewer, with your message as its note: Sam's push opens that thread.
  By default Sam sees that message and what follows in that thread only;
  *Share the whole thread* gives that thread's history instead. Either
  way every other thread of the session starts at the invite, like any
  "from now" share, and a later role change keeps what Sam could read.
  Nobody else's post is offered anything, so an @ never tells an editor
  who exists. Agents get no offer: `post_thread` points them at
  `invite_person`.
- **Huddles** (M63). A voice call on a session, for the people working in
  it: the headphones button by the session's name (or *Start a huddle* in
  the session menu) starts one, and everyone with the session sees it's on
  (the button goes green with how many are in) and joins with a click. Up
  to 5 people. The huddle bar stays in the corner while you move between
  tabs, sessions and chat (where it sits in the sidebar's corner): who's in, who's talking, who's muted,
  *Mute* (Ctrl/Cmd+Shift+Space) and *Leave*. Audio goes straight between
  devices, encrypted end to end (WebRTC, DTLS-SRTP), through a TURN relay
  when there's no direct path; the machine only introduces the members.
  Through control, each device signs its call fingerprints with its device
  key and the others check them, so neither the machine nor control can
  listen in by standing in the middle: one of your own devices shows as
  verified, someone else's as signed, and a mismatch is refused. A
  connection with no device key (over a tailnet, or local) shows as
  unverified. Someone whose access is removed drops out at once. A machine
  restart ends its huddles, and the page joins again when it's back. On an
  iPhone the mic stops while the app is in the background; the bar says
  so. The Linux desktop app's web view has no WebRTC, so the app runs the
  call itself (WebRTC in Rust, Opus, and WebRTC's echo cancellation and
  noise suppression), with the same bar and buttons.
- **Chat: every thread in one place.** *Chat* (beside *Panes* and *Swarm*
  in the bar, the phone's sheet, or the command palette; `/#chat`) is a
  page of its own, laid out like a team chat. On the left: *Activity*,
  any huddles that are on, then each machine's sessions as channels with
  each pane's thread under its session, newest first, unread in bold and
  @mentions counted. Fold a machine away, show only what's unread, or drag
  the sidebar wider; the browser remembers. The panes stay as they were
  under the page; *Panes*, or Back, returns to them.
  - **Messages** carry the poster's picture, run together under one
    heading, and have a line between days and a red *New* line at the
    first one you hadn't read. An agent's are badged. Text takes a little
    Markdown: `code`, fenced code blocks, **bold**, *italic*, links and
    @mentions (yours stand out); anything else, HTML included, stays text.
    Hover a message to quote it in your reply, copy a link to it (the link
    opens the thread at that message), or go to its pane.
  - **Writing:** Enter sends, Shift+Enter makes a new line, and @ offers
    the people here (and `@agent` in a pane's thread). An unsent message
    waits in its thread while you look at another.
  - **The header** says where the thread is and what its pane is doing,
    who's in it, and has the huddle button and *Go to pane* (or *Go to
    session*). ⓘ opens details: the pane's screen, live (or each of the
    session's panes), and the people.
  - **Getting around:** the search field in the bar searches every thread
    on every machine you can read as you type. *Activity* lists messages
    that mention you and agents' answers to you. Ctrl/Cmd+K jumps to a
    channel by name. Alt+↑/↓ moves between channels, Alt+Shift+↑/↓ between
    unread ones, and Shift+Esc marks everything read.
