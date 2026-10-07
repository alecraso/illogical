# Arugula

Keep track of your agents without checking every session. See which agents
need input and what your teammates are working on. Leave unfinished work
open, come back later, or join a teammate's session to help.

Arugula is a terminal multiplexer whose sessions outlive the window, the
daemon and the reboot. A daemon owns your terminals; the browser (desktop or
phone) draws tabs and splits you drive with the mouse.

![Tabs and splits in the browser](https://arugula.io/img/desktop.png)

- **The swarm.** Every pane on every machine you and your team can see, in
  one live view, clustered by project, machine, kind or person. Whatever
  needs someone (an agent asking, a build failing) lifts out to a rail of
  cards, where anyone on the team who may answer allows, answers or sends
  the agent its next instruction, and everyone sees who did.
- **Agents as blocks.** Claude Code, Codex or any
  [ACP](https://agentclientprotocol.com) agent as a UI beside your
  terminals: tool calls with their output, approvals and questions as
  cards big enough for a thumb.
- **Mouse first.** Click, drag and right-click for tabs and splits. No
  chords to learn, no prefix key.
- **Durable.** Close the window, lose the connection, restart the daemon or
  reboot: the layout, working directories and scrollback come back, and
  each pane does what you told it to (a shell where it was, re-run its
  command, `claude --resume`). A daemon restart doesn't even touch running
  programs.
- **Anywhere on your tailnet.** The same live layout on every window and
  your phone, over [Tailscale](https://tailscale.com). Push notifications
  when a pane rings, a long command finishes, or an agent needs you.
- **VS Code and dev servers beside your terminals** (opt-in). *Open in
  editor* (or `arugula edit src/main.rs:42`) opens VS Code on the pane's
  machine, in its directory, as a block, back with its file after a
  restart; *Open a port…* shows a dev server beside its terminal. Both are
  off until the daemon gets a listener for them (one flag for this
  computer's browser; a domain of yours for the phone): see
  [Blocks](https://docs.arugula.io/blocks/#browser-blocks-on-ports).
- **What did the agent change?** *Changes* on a pane (or `arugula diff`)
  lists the files changed in its repository, on its machine, with +/−; tap
  a file for its hunks and a line to see the file there, both updating
  while the agent works. Phone first, and a failed build is a *Rerun* tap
  away.
- **Your editor in the swarm.** VS Code, Cursor or nvim (over Remote-SSH
  too) shows up beside your panes once you ask it to. Follow its cursor
  from your phone; a debugger stopping, or Claude Code wanting to edit a
  file, is a card you answer from anywhere.
- **Scriptable.** `arugula`, a CLI for scripts and agents: run, send,
  wait for a command or a match, tail, search every pane's history.
- **In any terminal, too.** `arugula tui` draws the same tabs and splits
  in the terminal you're in (over ssh as well), with a sidebar of what
  needs you: allow an agent's request from there without opening its pane.
  Select, search a pane's whole history and copy, to your own clipboard
  over ssh.

[![A tour of the swarm: every pane clustered by project, then one project's panes up close, then an agent's request on the Needs You rail, then that agent's session opened](https://arugula.io/img/dive.gif)](https://arugula.io)

Linux (x86_64, arm64), macOS (Apple silicon and Intel) and Windows 10 and 11
(x86_64). Share a session with
someone, or a whole machine with a team, with roles and presence
([teams](https://docs.arugula.io/teams/), [sharing](https://docs.arugula.io/sharing/)). Remote access is over your tailnet, or
through [Arugula control](https://docs.arugula.io/control/) for devices without one: end
to end encrypted, so what the service relays it can't read, and it can't
add a reader to your machines. You do trust it for the web client it
serves ([what holds](docs/control-e2e.md#what-holds-against-control)).

## Install

The desktop app (macOS, Linux, Windows) is at [arugula.io](https://arugula.io). On a server, or
a machine without a screen:

```
curl -fsSL https://arugula.io/install.sh | sh
```

On Windows, in PowerShell: `irm https://arugula.io/install.ps1 | iex`. With Homebrew:
`brew tap arugula-salad/tap && brew trust arugula-salad/tap && brew install arugula && arugulad install`.

[Install](https://docs.arugula.io/install/) has the details, and the [quickstart](https://docs.arugula.io/quickstart/) goes from
the browser to your phone, scripts, agents and MCP.

## Docs

**[docs.arugula.io](https://docs.arugula.io/)** has the docs for using Arugula: [panes](https://docs.arugula.io/panes/),
[blocks](https://docs.arugula.io/blocks/), [agents](https://docs.arugula.io/agents/), [control](https://docs.arugula.io/control/),
[teams](https://docs.arugula.io/teams/), [the CLI](https://docs.arugula.io/cli/) and [MCP](https://docs.arugula.io/mcp/). They're built from
[arugula-salad/site](https://github.com/arugula-salad/site).

For working on Arugula itself:

- [docs/development.md](docs/development.md): building from source, testing, releasing, and the
  code's layout.
- [docs/testing.md](docs/testing.md): the test suites.
- [docs/control-e2e.md](docs/control-e2e.md): how control's end-to-end encryption keeps it out of
  your terminals.
- [DECISIONS.md](DECISIONS.md): the decisions that still hold.
- [docs/plan-archive.md](docs/plan-archive.md): the original plan and every milestone.

## License

MIT OR Apache-2.0, at your option. Terminal emulation by
[libghostty](https://ghostty.org); see [THIRD_PARTY.md](THIRD_PARTY.md).
