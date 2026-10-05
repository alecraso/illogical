# illogical

A terminal multiplexer whose sessions outlive the window, the daemon and the
reboot. A daemon owns your terminals; the browser (desktop or phone) draws
tabs and splits you drive with the mouse.

![Tabs and splits in the browser](site/img/desktop.png)

- **Mouse first.** Click, drag and right-click for tabs and splits. No
  chords to learn, no prefix key.
- **Durable.** Close the window, lose the connection, restart the daemon or
  reboot: the layout, working directories and scrollback come back, and
  each pane does what you told it to (a shell where it was, re-run its
  command, `claude --continue`). On Linux a daemon restart doesn't even
  touch running programs.
- **Anywhere on your tailnet.** The same live layout on every window and
  your phone, over [Tailscale](https://tailscale.com). Push notifications
  when a pane rings, a long command finishes, or an agent needs you.
- **Agents as blocks.** Claude Code, Codex or any
  [ACP](https://agentclientprotocol.com) agent as a UI beside your
  terminals: tool calls with their output, approvals and questions as
  cards big enough for a thumb.
- **VS Code and dev servers beside your terminals** (opt-in). *Open in
  editor* (or `illogical edit src/main.rs:42`) opens VS Code on the pane's
  machine, in its directory, as a block, back with its file after a
  restart; *Open a port…* shows a dev server beside its terminal. Both are
  off until the daemon gets a listener for them (one flag for this
  computer's browser; a domain of yours for the phone): see
  [advanced setup](docs/advanced.md#browser-blocks-on-ports).
- **What did the agent change?** *Changes* on a pane (or `illogical diff`)
  lists the files changed in its repository, on its machine, with +/−; tap
  a file for its hunks and a line to see the file there, both updating
  while the agent works. Phone first, and a failed build is a *Rerun* tap
  away.
- **The swarm.** Every pane on every machine you and your team can see, in
  one live view, clustered by project, machine, kind or person. Whatever
  needs someone (an agent asking, a build failing) lifts out to a rail of
  cards, where anyone on the team who may answer allows, answers or sends
  the agent its next instruction, and everyone sees who did.
- **Your editor in the swarm.** VS Code, Cursor or nvim (over Remote-SSH
  too) shows up beside your panes once you ask it to. Follow its cursor
  from your phone; a debugger stopping, or Claude Code wanting to edit a
  file, is a card you answer from anywhere.
- **Scriptable.** `illogical`, a CLI for scripts and agents: run, send,
  wait for a command or a match, tail, search every pane's history.
- **In any terminal, too.** `illogical tui` draws the same tabs and splits
  in the terminal you're in (over ssh as well), with a sidebar of what
  needs you: allow an agent's request from there without opening its pane.
  Select, search a pane's whole history and copy, to your own clipboard
  over ssh.

[![A tour of the swarm: every pane clustered by project, then one project's panes up close, then an agent's request on the Needs You rail, then that agent's session opened](site/img/dive.gif)](https://illogical.widgets.wtf)

Linux (x86_64, arm64) and macOS (Apple silicon and Intel). Share a session with
someone, or a whole machine with a team, with roles and presence
([docs/teams.md](docs/teams.md)). Remote access is over your tailnet, or
through [illogical control](docs/control.md) for devices without one: end
to end encrypted, so what the service relays it can't read, and it can't
add a reader to your machines. You do trust it for the web client it
serves ([what holds](docs/control-e2e.md#what-holds-against-control)).

## Install

**The desktop app** (macOS 13 or later on Apple silicon or Intel, Linux
x86_64), from [illogical.widgets.wtf](https://illogical.widgets.wtf) or the
[latest release](https://github.com/arugula-salad/illogical/releases/latest):
`illogical-desktop-macos-arm64.zip` (Apple silicon),
`illogical-desktop-macos-x86_64.zip` (Intel),
`illogical-desktop-linux-x86_64.AppImage` or `.deb` (the Linux app runs on
Ubuntu 22.04, Debian 12, Fedora 36 or newer: glibc 2.35 and up). The first
time it opens it installs `illogicald` and `illogical` in
`~/.local/bin` and starts the daemon as a service, then *Getting started*
sets up your phone, the cloud and Claude Code, a click each. Once the
machine is in illogical cloud, the app signs in through your browser on
the same computer (approve it as a new device once) and shows every machine in your account
and your teams. The macOS app
isn't notarized yet: the first time, open it, then choose *Open Anyway* in
System Settings › Privacy & Security.

**Servers and machines without a screen:**

```
curl -fsSL https://illogical.widgets.wtf/install.sh | sh
```

This puts `illogicald` and `illogical` in `~/.local/bin` and starts the
daemon as a service (systemd user unit on Linux, launchd agent on macOS).
Run it again to upgrade. `ILLOGICAL_VERSION=vX.Y.Z` picks a version.

**Homebrew** (macOS, Linux):

```
brew tap arugula-salad/tap
brew install illogical
illogicald install
```

**From source:** see [docs/development.md](docs/development.md#build-from-source).

On Linux, let it start at boot, before you log in:

```
loginctl enable-linger $USER
```

**Updating.** When a newer release is out, the top bar says so, with the
command for how you installed it. Panes keep running while the daemon
restarts.

- install.sh: run it again.
- Homebrew: `brew upgrade illogical && illogicald install`.
- The desktop app: download the new one and open it. When it finds an
  older daemon running as the service, it puts its own in its place,
  keeping the daemon's flags and your panes.

The daemon finds out by asking GitHub where its latest release is, at
most twice a day; nothing else is sent. `illogicald install --
--no-update-check` turns that off.

## Quickstart

1. Run `illogical web`: it opens <http://127.0.0.1:7681> in your browser,
   signed in (programs on this machine show its local token; see
   [docs/advanced.md](docs/advanced.md)). Over ssh it prints the link
   and the `ssh -L` that forwards the port to the computer you're at.
   Right-click a pane or a tab for
   everything. Drag a tab or a pane onto another pane's edge to split it
   there; drag dividers to resize.
2. **From your phone and other machines**, put it behind Tailscale on this
   machine. The first time:
   - turn on **MagicDNS** and **HTTPS certificates** in the tailnet's
     [DNS settings](https://login.tailscale.com/admin/dns), or `serve`
     fails;
   - on Linux, let yourself run `serve` without sudo: `sudo tailscale set
     --operator=$USER`;
   - on macOS, the app's CLI may not be on your PATH: it's
     `/Applications/Tailscale.app/Contents/MacOS/Tailscale`.

   Then:

   ```
   tailscale serve --bg --https=443 http://127.0.0.1:7681
   ```

   and open `https://<this machine>.<tailnet>.ts.net`: `tailscale serve
   status` prints it, and so does *Getting started* in the session menu
   (and `install.sh`, when Tailscale is up). Only the Tailscale
   login that owns the machine gets in (`illogicald install -- --owner
   you@example.com` for someone else). On the phone, add it to the home
   screen, then *Notify this device* in the menu (☰).
3. **From a script or another pane:**

   ```
   illogical run --wait -- cargo build        # a new tab; exits with its exit code
   illogical send %3 'git status' -e          # type a line and press Enter
   illogical wait %3 --match 'listening on'
   illogical tail %3 -f --text
   illogical search 'panic|Traceback' --since 1d
   ```

   [docs/cli.md](docs/cli.md) has the rest.
4. **In a terminal**, or over ssh: `illogical tui`. The mouse works as in
   the browser; Ctrl-] is the menu key (Ctrl-] ? lists the rest).
5. **Without a tailnet**, add the machine to an account on illogical
   control and use it from any browser:

   ```
   illogicald join https://control.illogical.widgets.wtf
   ```

   The hosted control is free during the beta, provided as is. See
   [docs/control.md](docs/control.md), including running your own.
   **With a team:** make one in control (*Teams…*), invite people, and
   pick the team when you approve a machine's join. Roles, personal vs
   team machines and sharing one session: [docs/teams.md](docs/teams.md).
6. **Agents.** *Start an agent…* in a pane's menu, or `illogical agent
   "fix the failing test"`. Claude Code and Codex run through an npm
   adapter (needs Node 20+): *Start an agent…* offers to install it, in a
   pane you can watch, or install it yourself:

   ```
   npm install --prefix ~/.local/share/illogical/agents/claude @agentclientprotocol/claude-agent-acp@0.85.0
   npm install --omit=optional --prefix ~/.local/share/illogical/agents/codex @agentclientprotocol/codex-acp@2.1.0
   ```

   Claude Code in an ordinary pane can raise the same notifications,
   question cards and permission cards, and take follow-ups, through its
   hooks: see [Claude Code in a pane](docs/cli.md#claude-code-in-a-pane).
7. **As tools for any agent (MCP).** Give Claude Code (or Codex, or any
   MCP client) illogical's tools: it runs builds and dev servers in panes
   you can watch from the phone and take over, waits on them, starts and
   answers other agents, and searches what happened yesterday.

   ```
   claude mcp add illogical -- illogical mcp
   ```

   The read-only tools are safe to allow outright; leave the rest to ask.
   In `~/.claude/settings.json` (or the project's `.claude/settings.json`):

   ```json
   {
     "permissions": {
       "allow": [
         "mcp__illogical__read_output", "mcp__illogical__capture_screen", "mcp__illogical__wait",
         "mcp__illogical__list", "mcp__illogical__history", "mcp__illogical__search",
         "mcp__illogical__list_conversations", "mcp__illogical__read_pr", "mcp__illogical__read_issue",
         "mcp__illogical__list_agents", "mcp__illogical__read_agent", "mcp__illogical__read_file"
       ],
       "ask": [
         "mcp__illogical__run", "mcp__illogical__send_input", "mcp__illogical__close",
         "mcp__illogical__open_port", "mcp__illogical__open_app", "mcp__illogical__start_agent",
         "mcp__illogical__open_conversation", "mcp__illogical__agent_respond", "mcp__illogical__show_changes",
         "mcp__illogical__show_file", "mcp__illogical__open_workspace", "mcp__illogical__open_pr",
         "mcp__illogical__pr_comment", "mcp__illogical__pr_review", "mcp__illogical__pr_merge",
         "mcp__illogical__open_issue", "mcp__illogical__issue_comment", "mcp__illogical__issue_new",
         "mcp__illogical__open_fountain"
       ]
     }
   }
   ```

   What it starts says "started by mcp:claude-code". Agent blocks get the
   same tools by themselves, limited to their own tab. Over HTTP, tokens,
   and the tools: [MCP](docs/cli.md#mcp).

## A Fountain runner (Linux)

A machine can be your Fountain account's runner (M45): Fountain's runner
conversations then run there, in sandboxes illogical can open a shell on,
diff and follow.

1. Once, with root, from a checkout:
   `sudo bash scripts/fountain-runner-setup.sh --fountain "$(command -v fountain)" --node "$(node -p process.execPath)"`.
   It makes a `fountain` user (its sandboxes under
   `/home/fountain/sandboxes`), the `fountain-runner` systemd unit, and a
   sudoers rule: you may run `/bin/bash` as `fountain`, and
   `systemctl start|stop|restart|status fountain-runner` as root. The
   unit keeps its agents off this machine's loopback (where illogicald
   and your other local services listen), but for DNS;
   `--allow-loopback` lets them use it (a dev server they start and
   test, say).
2. Then, as you: `illogical fountain runner install` (makes the runner's key
   without printing it, and starts the unit); `illogical fountain runner
   status` shows it.
3. `illogical fountain --view runner` (or *Fountain runner…* in a pane's
   menu) shows it as a block: online or not, the other runners, and its
   sandboxes, each with *Follow*, *Changes* and *Shell*.

Checking it by hand, after a setup (none of this is automated, since the
tests never run sudo or reach Fountain):

- [ ] `illogical fountain runner status`: the unit is active, and Fountain
      lists only this runner, online.
- [ ] The runner view says this host's runner is online, its version
      matches `fountain --version`, and no attention is raised.
- [ ] A conversation with an agent on the runner provider appears under
      the runner's sandboxes, with its directory and agent.
- [ ] *Changes* opens a diff of each git checkout in its sandbox (one with
      nothing upstream is diffed from an empty tree, so all of it shows).
- [ ] *Shell* opens a terminal (bash without profile or rc files, and
      outside the runner's sandboxing, as its card says) where `whoami;
      pwd` prints `fountain` and the sandbox's directory; there, `cat ~YOU/.ssh/id_ed25519` and
      `illogical --socket <your daemon's socket> ls` both fail.
- [ ] A sandbox agent can't see other users' processes: asked to run
      `ls /proc | wc -l` (or `ps aux`), a conversation's agent sees only
      the runner's own processes (the unit has `ProtectProc=invisible`).
      Ask the agent: illogical's *Shell* isn't in the unit, so it sees
      everything, as any shell of yours would.
- [ ] *Follow* opens an agent block with the conversation so far.
- [ ] `sudo systemctl stop fountain-runner`: within about 5 minutes the
      rail (and the phone) says *Fountain runner offline*; `sudo systemctl
      start fountain-runner` clears it.

## On macOS

Everything above works, except that restarting or upgrading the daemon
ends the panes' programs (there's no systemd to hold them); scrollback and
layout still come back. VM tabs are Linux only.

## More

- [docs/features.md](docs/features.md): everything it does, in detail.
- [docs/advanced.md](docs/advanced.md): VM tabs (wisp), web apps beside
  their terminals, more machines and sandboxes, iTerm2 as a tmux client.
- [docs/teams.md](docs/teams.md): your machines, your team: roles,
  personal vs team machines, sharing a session.
- [docs/control.md](docs/control.md): illogical control, hosted or your
  own; [docs/control-e2e.md](docs/control-e2e.md), how it keeps out of your
  terminals.
- [docs/cli.md](docs/cli.md): the CLI and the HTTP API.
- [docs/development.md](docs/development.md): building, testing, the code's
  layout, and what building it taught us.
- [PLAN.md](PLAN.md): the decisions and the milestones.

## License

MIT OR Apache-2.0, at your option. Terminal emulation by
[libghostty](https://ghostty.org); see [THIRD_PARTY.md](THIRD_PARTY.md).
