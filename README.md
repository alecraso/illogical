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
- **Scriptable.** `illogical`, a CLI for scripts and agents: run, send,
  wait for a command or a match, tail, search every pane's history.

Single user, Linux (x86_64, arm64) and macOS (Apple silicon). Not meant
for the open internet: remote access is your tailnet only.

## Install

```
curl -fsSL https://illogical.widgets.wtf/install.sh | sh
```

This puts `illogicald` and `illogical` in `~/.local/bin` and starts the
daemon as a service (systemd user unit on Linux, launchd agent on macOS).
Run it again to upgrade. `ILLOGICAL_VERSION=v0.1.0` picks a version.

**Homebrew** (macOS, Linux):

```
brew tap jhgaylor/tap https://git.inevitable.fyi/jhgaylor/homebrew-tap
brew install illogical
illogicald install
```

**From source:** see [docs/development.md](docs/development.md#build-from-source).

On Linux, let it start at boot, before you log in:

```
loginctl enable-linger $USER
```

## Quickstart

1. Open <http://127.0.0.1:7681>. Right-click a pane or a tab for
   everything. Drag a tab or a pane onto another pane's edge to split it
   there; drag dividers to resize.
2. **From your phone and other machines**, put it behind Tailscale on this
   machine:

   ```
   tailscale serve --bg --https=443 http://127.0.0.1:7681
   ```

   and open `https://<this machine>.<tailnet>.ts.net`. Only the Tailscale
   login that owns the machine gets in (`illogicald install -- --owner
   you@example.com` for someone else). On the phone, add it to the home
   screen, then *Notify this device* in the session menu.
3. **From a script or another pane:**

   ```
   illogical run --wait -- cargo build        # a new tab; exits with its exit code
   illogical send %3 'git status' -e          # type a line and press Enter
   illogical wait %3 --match 'listening on'
   illogical tail %3 -f --text
   illogical search 'panic|Traceback' --since 1d
   ```

   [docs/cli.md](docs/cli.md) has the rest.
4. **Agents.** Install an adapter (needs Node), then *Start an agent…* in a
   pane's menu, or `illogical agent "fix the failing test"`:

   ```
   npm install --prefix ~/.local/share/illogical/agents/claude @agentclientprotocol/claude-agent-acp@0.85.0
   npm install --omit=optional --prefix ~/.local/share/illogical/agents/codex @agentclientprotocol/codex-acp@2.1.0
   ```

   Claude Code in an ordinary pane can raise the same notifications and
   question cards with three hooks: see
   [Claude Code hooks](docs/advanced.md#claude-code-hooks).

## On macOS

Everything above works, except that restarting or upgrading the daemon
ends the panes' programs (there's no systemd to hold them); scrollback and
layout still come back. VM tabs are Linux only.

## More

- [docs/features.md](docs/features.md): everything it does, in detail.
- [docs/advanced.md](docs/advanced.md): VM tabs (wisp), web apps beside
  their terminals, more machines and sandboxes, iTerm2 as a tmux client.
- [docs/cli.md](docs/cli.md): the CLI and the HTTP API.
- [docs/development.md](docs/development.md): building, testing, the code's
  layout, and what building it taught us.
- [BRIEF.md](BRIEF.md) and [PLAN.md](PLAN.md): why it exists, the
  decisions and the milestones.

## License

MIT OR Apache-2.0, at your option. Terminal emulation by
[libghostty](https://ghostty.org); see [THIRD_PARTY.md](THIRD_PARTY.md).
