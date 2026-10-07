# Labs

What a machine offers only when its state directory has a `labs` file.
These features work, but a stranger doesn't have what they need (a
Fountain login, the studio, a sandbox provider, our own chant setup), so
the public docs at [docs.arugula.io](https://docs.arugula.io/) don't
describe them at all. This page is the team's description of them. It
was the *Labs* sections of the public features, teams, advanced and
control pages until those moved to the site.

## Turning labs on

A file named `labs` in a machine's state directory turns on what a new
install doesn't show: chat and threads, huddles, Fountain, studio apps,
chant workspaces, VM tabs and sandboxes, ssh invites for guests, the swarm's
city, hive and timeline views, and the matching tools, commands and options
of `arugula mcp`, `arugula --help` and `arugulad --help`. They all keep
working without it; they just aren't offered.

```
touch ~/.local/state/arugula/labs
```

The state directory is `$ARUGULA_STATE_DIR` if set, else
`$XDG_STATE_HOME/arugula`, else `~/.local/state/arugula`; on Windows,
`%LOCALAPPDATA%\arugula\state`. The file can be empty. Delete it to turn
labs off again.

- **Per machine.** It's read by the machine that serves the page, so every
  machine you want them on needs its own. Someone you share a session with
  sees chat and huddles on your machine if it has labs, and not otherwise.
- **No restart.** The daemon looks for the file whenever it's asked; reload the
  page to see the change.
- **Each feature still needs its own setup.** Fountain needs a login, studio a
  link, VMs a sandbox provider, guest ssh its listener; labs only stops
  them from being hidden.

## What labs unhides

- **CLI (`arugula --help`):** `fountain`, `studio`, `app`, `workspace`,
  `guests`, `machines`, `sandboxes`; `agent --fountain/--as/--vault/--vm`,
  `run --vm/--vm-tab/--image/--sandbox`, and
  `share --guest/--rw/--reusable/--relay/--addr/--name`
  (`LABS_COMMANDS`, `LABS_OPTIONS` in `crates/cli/src/main.rs`).
- **Daemon (`arugulad --help`):** `--guest-ssh`, `--guest-ssh-host`,
  `--studio-file`, `--wisp-url`, `--wisp-token-file`
  (`LABS_OPTIONS` in `crates/daemon/src/args.rs`).
- **MCP:** `read_thread` and `post_thread`, and the kinds `show:app`,
  `show:workspace`, `show:fountain`, `list:fountain_agents`,
  `list:fountain_agent` (`UNLISTED` in `crates/daemon/src/mcp/tools.rs`).
  Unlisted tools still answer when called by name.
- **Web client:** `Client.has("vms" | "fountain" | "studio")` and
  `hasLabs()`/`hasThreads()` in `web/src/client.ts`: VM panes and tabs,
  *Sandboxes…*, *Split (local)* and a VM tab's *Machine* items, studio
  apps, Fountain, *Open as workspace*, *Invite over ssh…*, threads, chat,
  huddles, and the swarm's city, hive and timeline themes
  (`web/src/swarm/view.tsx`).
- **Machines (VMs).** A *machine* (`--machine mN`, `mN:PATH`, `--machine
  local`, MCP `run`'s `machine`) is a VM or sandbox from the provider
  (`crates/daemon/src/machine.rs`). Examples that use them: `arugula open
  --machine m2 :3000`, `arugula edit --machine m2 ~/app`, `arugula view
  m2:src/main.rs`, `arugula fs cat m2:~/log.txt`, `arugula agent --machine
  m3 …`, and `arugula --host s1 ls` for a sandbox with a resident daemon
  (`arugula hosts promote`), which wakes it.
- **Control:** *New hosted VM* and *Delete this VM* in the host menu,
  offered when control says hosted sandboxes are open (not by the `labs`
  file). Hosted sandboxes run on control's provider, which writes their
  trust files, so their operator can read them; deleting an account
  deletes its hosted VMs.

## Threads, chat and huddles

Chat and huddles are on for a machine that has labs. The file belongs to
the machine, so on a team every team machine needs one. Someone you share
a session on a machine with labs with sees chat and huddles there; on a
machine without it, nobody does.

- **Threads on panes and sessions**. Every pane and every session has
  a thread where the people working on it talk: *Thread* in a pane's menu,
  *Session thread* in the session menu, or the bubble on a pane. Messages
  arrive live on every window and phone. The machine that owns the pane
  keeps them (`<state>/threads/`), so they survive restarts and upgrades,
  outlive the pane, show up in `search`, and never pass through control
  unencrypted. Watchers read and drivers post. A private pane's thread is
  its owner's, and someone shared "from now" sees messages from then on.
  Each person has their own unread count: on the pane's bubble, a dot on
  the session button, and a folded corner in the swarm. `@name` notifies someone, on
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
- **An @ of someone who can't see the thread offers to invite them**. When you, the owner, write "@sam look" and Sam is someone this
  machine knows (shared with elsewhere, or on a roster it checked) but
  can't read that thread, the composer says "Sam can't see this. Invite
  them?" in place of the "nobody" note. One click is the invite, as a
  viewer, with your message as its note: Sam's push opens that thread.
  By default Sam sees that message and what follows in that thread only;
  *Share the whole thread* gives that thread's history instead. Either
  way every other thread of the session starts at the invite, like any
  "from now" share, and a later role change keeps what Sam could read.
  Nobody else's post is offered anything, so an @ never tells an editor
  who exists. Agents get no offer: `post_thread` points them at
  `invite_person`.
- **Huddles**. A voice call on a session, for the people working in
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

### Who sees threads, and where they live

Every pane and every session has a thread: *Thread* in a pane's
menu, *Session thread* in the session menu, or the bubble on a pane.

- **Who sees what:** the same people as the pane or session. Watchers read;
  drivers and owners post. A private pane's thread is its owner's (on a
  team machine, the team's owners'). Someone you shared with "from now"
  sees messages from then on.
- **Where it lives:** on the machine that runs the pane, like its output.
  Control relays it encrypted, as it does the terminal.
- **@name** notifies them, on their phone too, whether or not they're
  connected (team members included); tapping it opens the thread. **@agent** in a pane's
  thread goes to the agent in that pane as a follow-up, if you may drive
  it; agents answer in the thread.
- **@name of someone who can't see it:** if you own the machine and it
  knows them (shared with elsewhere, or on a team roster it checked), the
  composer offers to invite them. They see your message and what follows
  in that thread, or the whole thread if you choose; no other thread's
  past.
- *Quote selection in thread* (pane menu) posts what you selected in the
  terminal; clicking the quote jumps back to it.

### Huddles: who joins, where the audio goes

A session can have a huddle, a voice call for the people in it: the
headphones button by the session's name, or *Start a huddle* in the
session menu. Up to 5 people.

- **Who can join:** anyone the session is shared with, watchers too. A
  read-only link can't. Someone whose share is removed drops out at once.
- **Where the audio goes:** straight between the devices in the call,
  encrypted end to end. When two devices can't reach each other directly
  it goes through a TURN relay (Cloudflare's, for the hosted control),
  which carries only ciphertext; Cloudflare sees addresses and how much is
  sent. The machine that runs the session only introduces the members.
- **Who you're talking to:** through control, each device signs its call
  fingerprints with its device key. Your own devices show as verified,
  other people's as signed (by the device their machine vouches for), and
  a mismatch is refused. Over a tailnet there are no device keys, so
  members show as unverified.

Huddles don't go through control. Control hands machines short-lived
TURN credentials (from Cloudflare, for the hosted control); a relayed
call's audio is encrypted end to end, so the relay sees only addresses and
volume. Operating TURN for the hosted control is in
[control-ops.md](control-ops.md#turn-for-huddles).
