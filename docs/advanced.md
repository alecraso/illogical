# Advanced setup

Everything here is optional. The [quickstart](../README.md#install) gets you
durable panes, the phone and agent blocks; these add VMs, web apps beside
your terminals, more machines, and iTerm2.

Examples call the machine that serves the page `home` and the tailnet
`<tailnet>.ts.net`; use your own names.

- [Service and logs](#service-and-logs)
- [Pane environment](#pane-environment)
- [Claude Code hooks](#claude-code-hooks)
- [VM tabs and panes (wisp)](#vm-tabs-and-panes-wisp)
- [Browser blocks on ports](#browser-blocks-on-ports)
- [Editor blocks](#editor-blocks)
- [Agents in a VM](#agents-in-a-vm)
- [A Mac as another host](#a-mac-as-another-host)
- [A sandbox on the tailnet](#a-sandbox-on-the-tailnet)
- [A sandbox that can only dial out](#a-sandbox-that-can-only-dial-out)
- [Sandboxes from a provider](#sandboxes-from-a-provider)
- [iTerm2, as a tmux client](#iterm2-as-a-tmux-client)

## Service and logs

`illogicald install` copies the binary to `~/.local/bin` and installs a
service. Run it again to upgrade; the panes' programs keep running through
it. Flags after `--` are passed to the daemon on every start
(`illogicald install -- --owner you@example.com`).

- **Linux:** `~/.config/systemd/user/illogicald.service`, enabled. With
  lingering (`loginctl enable-linger $USER`) it starts at boot, before you
  log in. Logs: `journalctl --user -u illogicald`.
- **macOS:** `~/Library/LaunchAgents/illogicald.plist`, which starts it at
  login and after a crash. Logs: `~/Library/Logs/illogicald.log`.
  `launchctl bootout gui/$UID/illogicald` stops it. Panes run your login
  shell from the user database (launchd sets no `$SHELL`); zsh and bash get
  shell integration. There's no systemd FD store, so each pane's shim keeps
  its terminal while the daemon is gone (`--keep-panes`, which the plist
  sets): a restart (`launchctl kickstart -k gui/$UID/illogicald`), an
  upgrade or a crash leaves the programs running, and the new daemon adopts
  them. Stopping it for good (`launchctl bootout`, logging out) ends them a
  minute later, if no daemon has come back.
- **Without systemd on Linux** (a container, a box with another init): pass
  `--keep-panes` for the same behaviour.

**Updates.** At most every 12 hours the daemon asks where GitHub's
`releases/latest` redirects to (one request, with nothing about you or the
machine in it) and keeps the answer in `update-check.json` in the state
directory. When it's newer, the web client's top bar offers the command for
this install (`GET /api/update` says it too). `--no-update-check`
(`ILLOGICAL_NO_UPDATE_CHECK=true`) turns it off; a daemon run from where it
was built (`target/`) doesn't check. The desktop app replaces an older
daemon with the one it carries when that daemon runs as the service
(`ILLOGICAL_NO_DAEMON_UPGRADE=1` stops it).

State (layout, logs, checkpoints) is in `~/.local/state/illogical`, private
to you (0700/0600). `--state-dir` moves it.

**Who gets in.** On this machine, you: the CLI over its Unix socket (in
your private state directory), and anything on the TCP port that shows the
daemon's **local token** (`local-token` in the state directory, made at
the first start, 0600). Loopback is shared by every account and program on
the machine, so being on it isn't enough:

- **Your browser** gets the token as a cookie from a sign-in link:
  `illogical web` opens `http://127.0.0.1:7681` through it (`--print`
  prints the link, to open by hand or through an `ssh -L` forward). Once
  per browser; it stays signed in until the token changes. A page opened
  without it says to run `illogical web`. The desktop app signs its own
  window in.
- **Programs** send it as `Authorization: Bearer <token>` (`illogical
  --host http://127.0.0.1:7681` does that for you). MCP clients use
  `illogical mcp` (the socket) or an `illogical mcp token`.
- **A new token** signs every browser and program out: delete
  `local-token` and restart the daemon.

Over the tailnet, the daemon asks tailscaled who each caller is and lets
in only the login that owns the node; `--owner` names someone else. Behind
`tailscale serve`, the identity serve adds is believed only from
tailscaled's own connection (on Linux, the daemon checks which account
owns the other end). Tagged nodes, Funnel and the internet never get in.
Don't put it behind anything else that would forward requests to it.

## Pane environment

On Linux, at boot the daemon starts before you log in, so its own
environment has no `WAYLAND_DISPLAY`, `DISPLAY` or desktop `SSH_AUTH_SOCK`.
Each new pane takes the systemd user manager's environment as it is at that
moment, which your desktop session fills in at login. For variables every
pane should have from boot (`PATH` additions, `EDITOR`), put `KEY=value`
lines in `~/.config/environment.d/50-illogical.conf`. Panes run `$SHELL -l`,
so your profile runs too.

Blocks that run your tools for you (a chant workspace, say) don't have a
shell of their own, so the daemon reads your shell's environment once, as
VS Code does: it runs `$SHELL -l -i` when it starts and keeps the `PATH`
and variables your rc files set, so node from mise or nvm is found there as
in a pane. On a VM it does the same once per machine, with `bash -l -i`. If
your shell takes more than 10 seconds or fails, those blocks get the
daemon's own environment, and the log says why. After changing an rc file,
`illogical shell-env --refresh` reads it again (`illogical shell-env` shows
what blocks get).

## Claude Code hooks

Claude Code in a pane can tell you when it needs you, put its questions and
permission prompts on cards anyone on the team who may answer can answer,
and take follow-ups from them, all through hooks in
`~/.claude/settings.json`. The whole block, and what each part does, is in
[the CLI's *Claude Code in a pane*](cli.md#claude-code-in-a-pane). Outside
an illogical pane the hooks do nothing, so they're safe everywhere.

## VM tabs and panes (wisp)

*New VM tab* and *New VM pane* give a tab or a pane its own throwaway
Firecracker microVM, for agents and untrusted builds. They need wisp
(`wispd`, a separate sandbox daemon that isn't published yet) running on
the same Linux host, and its API token:

```
illogicald install -- --wisp-url http://127.0.0.1:7788 --wisp-token-file ~/.local/share/wisp/token
```

Those are the defaults, so with wispd installed the usual way, nothing is
needed. Without the token, VM panes are off and the menus don't offer them.
The base image is plain Ubuntu 24.04; install what you need in it (Claude
Code: `curl -fsSL https://claude.ai/install.sh | bash`).

## Browser blocks on ports

*Open a port…* (or `illogical open :5173`) shows a dev server beside its
terminal. Each block is served on an origin of its own by the daemon, so it
needs a listener and a wildcard name; without one they're off.

**On loopback only** (no names, no certificates; the browser on the same
machine):

```
illogicald install -- --block-listen 127.0.0.1:7701
```

Blocks are `http://b-<id>-<key>.localhost:7701`; browsers resolve
`*.localhost` themselves. Each name carries a random key only the app and
the CLI know.

**Over the tailnet** (the phone, other machines):

1. A domain you control in Cloudflare. Add a DNS-only (grey cloud) `A`
   record `*.illogical.example.com` pointing at the host's tailnet address
   (`tailscale ip -4`). Only the tailnet can reach that address.
2. A Cloudflare API token with *Zone › DNS › Edit* on that zone, in a file.
   The daemon uses it for Let's Encrypt DNS-01 challenges: it gets the
   wildcard certificate itself and renews it two thirds of the way through
   its life (kept in `<state>/acme/`).
3. Pick a free port on the tailnet address (443 is usually `tailscale
   serve`'s):

```
illogicald install -- --block-listen 100.x.y.z:7443 \
  --block-domain illogical.example.com \
  --block-acme-cloudflare-token-file ~/.config/illogical/cloudflare-token
```

`--block-acme-directory staging` uses the test CA while you try it;
`--block-cert`/`--block-key` serve a certificate you renew yourself.
Callers are checked with `tailscale whois`: only the owner gets in.

## Editor blocks

*Open in editor* and `illogical edit` run VS Code (code-server) and show it
like a browser block on a port, so they need block sites
(`--block-listen`, above); without them they say so.

- **code-server:** the release illogical pins is downloaded the first time
  an editor opens (about 230 MB) into `$XDG_CACHE_HOME/illogical/code-server`
  and checked against its SHA-256. `--code-server PATH` runs another one
  instead (a recent one: illogical passes `--idle-timeout-seconds` and
  `--socket-mode`). In a VM the same
  release is downloaded inside the VM.
- **Where things are:** settings, extensions and VS Code's state in
  `<state>/editor/` (`user/User/settings.json` is yours after the first
  start), its log in `<state>/editor/code-server.log`, its socket beside
  the CLI's (`<sock>-code`, mode 0600). code-server keeps its own logs in
  `~/.local/share/code-server`.
- **Stopping:** `--editor-idle SECONDS` (default 900, at least 60) after the
  last window closes. It runs in a scope of its own, so restarting the daemon
  leaves it, and open windows reconnect.

## Agents in a VM

`illogical agent --vm` (or the dialog's checkbox) runs the agent server on
a VM; its first start installs Node and the adapter there. Claude Code
there needs credentials: a token from `claude setup-token` in
`~/.config/illogical/claude-oauth-token`, or an API key in
`~/.config/illogical/anthropic-key` (used first). `--claude-token-file` and
`--anthropic-key-file` move them. They reach the agent on its stdin, into
its environment only: never the VM's disk, a URL, an argv, the log or the
layout.

## A Mac as another host

Install it on the Mac the same way, then make it reachable from the home
daemon's page:

1. Serve it on the tailnet with the Tailscale app:
   `/Applications/Tailscale.app/Contents/MacOS/Tailscale serve --bg --https=443 http://127.0.0.1:7681`
2. Let the home page talk to it:
   `illogicald install -- --allow-origin https://home.<tailnet>.ts.net`
3. On the home host: `illogical hosts add mac https://mac.<tailnet>.ts.net`.

The host switcher then shows it, and `illogical --host mac …` works from
the home host. The same steps add any other Linux machine.

## A sandbox on the tailnet

A container or VM without systemd can run the static Linux binaries from a
release. Copy `illogicald` and `illogical` into it, then:

```
illogical hosts invite                                  # on home: prints a token
illogical install --tailnet file:KEYFILE \
  --home https://home.<tailnet>.ts.net --join TOKEN     # in the sandbox
```

The key is an ephemeral, tagged Tailscale auth key (e.g. `tag:sandbox`), in
a file (or `-` for stdin; never on a command line). This downloads
tailscaled if it isn't there, runs it in userspace mode with its own state
in `~/.local/state/illogical-sandbox`, joins, puts the daemon behind
`tailscale serve`, and adds it to the home daemon's list. `illogicald
sandbox` keeps tailscaled and the daemon running; after a reboot, run
`illogicald sandbox &` again. Without `--join` it prints the `illogical
hosts add` line to run on home.

## A sandbox that can only dial out

For a sandbox that allows nothing in but outbound HTTPS. On home,
`illogical hosts token sbx` (or `hosts invite`); in the sandbox:

```
illogicald --peer wss://home.<tailnet>.ts.net --token ~/.config/illogical/host-token \
  [--join INVITE] [--sync [--sync-live]] &
```

The home daemon must be reachable at that URL from the sandbox and accept
its name as a Host (`--public-host`). The home daemon lists the sandbox and
answers for it at `/h/sbx/…`, so the host switcher and `illogical --host
sbx` work as for any host. `--sync` pushes closed panes' history to the
home daemon, encrypted at rest there; `--sync-live` pushes open ones too.

## Sandboxes from a provider

With a sandbox provider configured (wisp sprites; Fly's Sprites API fits
the same adapter), *Sandboxes…* lists them, *Shell* opens a disposable
shell on one with nothing installed, and *Make resident* (`illogical
sandboxes promote NAME`) copies the static daemon in and runs it as a
service there. The home daemon looks for the static binaries in
`--static-dir` (default `~/.local/share/illogical/static`); put a
release's Linux x86_64 binaries there.

## iTerm2, as a tmux client

iTerm2's tmux integration works with illogical in place of tmux: sessions
are sessions, tabs are native windows, splits are native splits, and the
same layout stays live in the browser. From iTerm2:

```
ssh -t home '~/.local/bin/illogical tmux -CC attach'          # the first session
ssh -t home '~/.local/bin/illogical tmux -CC attach -t work'
ssh -t home '~/.local/bin/illogical tmux -CC new -s ipad'
```

`-t` is a session name or `$N`; plain `illogical tmux -CC` is `attach`. To
detach, use *Shell › tmux › Detach*. Add `--host NAME` before `tmux` to
reach another daemon. The CLI behaves as `illogical tmux` when it is
called `tmux`, so for tools that run `tmux -CC` by name, put a link where
only they look (`mkdir -p ~/.local/share/illogical/tmux && ln -s
~/.local/bin/illogical ~/.local/share/illogical/tmux/tmux`, then `ssh -t
home 'PATH=~/.local/share/illogical/tmux:$PATH tmux -CC attach'`), not on
your `PATH`, where it would hide the real tmux.

- It reports tmux 3.5a. Typing, splits, divider drags, window resizes, new
  tabs and closing panes change the daemon's layout, which the browser
  shows at once, and the other way round.
- The window's size follows whoever claimed it last: iTerm2 when it
  resizes a window or you type in it, the browser when you click or type
  there.
- Agent and browser blocks show as read-only panes with their text and a
  note to open them in the web app.
- If iTerm2 falls behind a fast pane it shows "paused"; unpausing
  re-captures the pane and carries on.

A manual test script, and how to record the conversation, are in
[development.md](development.md#testing-iterm2).
