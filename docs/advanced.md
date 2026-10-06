# Advanced setup

Everything here is optional. The [quickstart](../README.md#install) gets you
durable panes, the phone, your other machines through
[illogical control](control.md), and agent blocks; these add web apps and VS
Code beside your terminals, containers, and iTerm2. *Open a port…* and *Open
in editor* say how to turn them on until they are.

Examples call the machine that serves the page `home` and the tailnet
`<tailnet>.ts.net`; use your own names.

- [Service and logs](#service-and-logs)
- [Pane environment](#pane-environment)
- [Claude Code hooks](#claude-code-hooks)
- [Browser blocks on ports](#browser-blocks-on-ports)
- [Editor blocks](#editor-blocks)
- [More machines](#more-machines)
- [A container on the tailnet](#a-container-on-the-tailnet)
- [A box that can only dial out](#a-box-that-can-only-dial-out)
- [iTerm2, as a tmux client](#iterm2-as-a-tmux-client)
- [Labs](#labs)

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
- **macOS with no GUI login** (a Mac you reach only over ssh, where that
  user hasn't logged in to the desktop since it booted): there's no
  `gui/$UID` domain, so `illogicald install` puts the same plist in the
  background session (`user/$UID`, `LimitLoadToSessionType` Background).
  It keeps running after you log out, but after a reboot it doesn't start
  until you log in to the desktop or run `illogicald install` again (over
  ssh is fine; `illogical --ssh` does it for you when the daemon isn't
  running), and the install says so.
- **macOS, from boot:** `illogicald install --system` installs a
  LaunchDaemon, `/Library/LaunchDaemons/illogicald.$USER.plist`, that runs
  the daemon as you from boot with nobody logged in. Run it as yourself,
  not as root: it runs `sudo` for the two steps that need root and prints
  them first, so you need to be an admin. It replaces the LaunchAgent (one
  daemon per user). Later installs keep it; `illogicald uninstall` first
  to go back to an agent. `sudo launchctl kickstart -k
  system/illogicald.$USER` restarts it.
- **Removing it:** `illogicald uninstall` stops and removes whichever is
  installed (the agent, the background agent, the LaunchDaemon with sudo,
  or on Linux the systemd user service). The binaries in `~/.local/bin`
  and the state in `~/.local/state/illogical` stay.
- **Without systemd on Linux** (a container, a box with another init): pass
  `--keep-panes` for the same behaviour.

**Updates.** At most every 12 hours the daemon asks where GitHub's
`releases/latest` redirects to (one request, with nothing about you or the
machine in it) and keeps the answer in `update-check.json` in the state
directory. When it's newer, the web client's top bar offers it (`GET
/api/update` says so too). The daemon updates itself when you ask: *Update
now* there, or `illogicald update` in a terminal (`-y` doesn't ask). Either
downloads this platform's archive from the release, checks it against the
release's `SHA256SUMS`, and runs the new `illogicald install`, which keeps
your flags and restarts the service; panes keep running. If anything fails
before the restart, the old daemon carries on and says why (`update.log` in
the state directory). The button is for the service `illogicald install`
set up for you (install.sh, install.ps1 or the desktop app). Homebrew
installs, `--system` services (they need sudo or an administrator) and
daemons run by hand show the command instead. On macOS the desktop app's
login item runs the daemon in the app, and an update goes to
`~/.local/bin`, which the app's copy runs from then on, as long as it's
newer. `--no-update-check` (`ILLOGICAL_NO_UPDATE_CHECK=true`) turns the
check off; a daemon run from where it was built (`target/`) doesn't
check. The desktop app never replaces a running
daemon: the daemon it carries is for a machine that has none.

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

Blocks that run your tools for you don't have a
shell of their own, so the daemon reads your shell's environment once, as
VS Code does: it runs `$SHELL -l -i` when it starts and keeps the `PATH`
and variables your rc files set, so node from mise or nvm is found there as
in a pane. If
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

## Browser blocks on ports

*Open a port…* (or `illogical open :5173`) shows a dev server beside its
terminal. Each block is served on an origin of its own by the daemon, so it
needs a listener and a wildcard name; without one they're off, and *Open a
port…* says how to turn them on (this section).

The browser showing the block reaches that listener itself. Through
[illogical control](control.md) from another device (your phone, another
computer) blocks aren't relayed yet, so there they need the tailnet setup
below.

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
(`--block-listen`, above); without them *Open in editor* says how to turn
them on.

- **code-server:** the release illogical pins is downloaded the first time
  an editor opens (about 230 MB) into `$XDG_CACHE_HOME/illogical/code-server`
  and checked against its SHA-256. `--code-server PATH` runs another one
  instead (a recent one: illogical passes `--idle-timeout-seconds` and
  `--socket-mode`).
- **Where things are:** settings, extensions and VS Code's state in
  `<state>/editor/` (`user/User/settings.json` is yours after the first
  start), its log in `<state>/editor/code-server.log`, its socket beside
  the CLI's (`<sock>-code`, mode 0600). code-server keeps its own logs in
  `~/.local/share/code-server`.
- **Stopping:** `--editor-idle SECONDS` (default 900, at least 60) after the
  last window closes. It runs in a scope of its own, so restarting the daemon
  leaves it, and open windows reconnect.

## More machines

Install illogical on each machine (the desktop app, `install.sh` or
Homebrew), then add it to your account on
[illogical control](control.md):

```
illogicald join https://control.illogical.widgets.wtf
```

Approve the code it prints on a signed-in device. Every machine you join
shows in the host menu of control's page and of the desktop app, with its
sessions and tabs, on every device you've added; nothing has to be wired
from one machine to another. *Getting started*'s *Cloud* step does the same
with a button. [control.md](control.md) has the rest: teams, phones,
leaving, moving a machine.

The CLI still reaches only the daemon on its own machine, or one on the
tailnet with `illogical --host NAME …` once that daemon's list has it
(`illogical hosts add NAME https://NAME.<tailnet>.ts.net`).

## A container on the tailnet

A container or box without systemd can run the static Linux binaries from a
release. Copy `illogicald` and `illogical` into it, then:

```
illogical hosts invite                                  # on home: prints a token
illogical install --tailnet file:KEYFILE \
  --home https://home.<tailnet>.ts.net --join TOKEN     # in the container
```

The key is an ephemeral, tagged Tailscale auth key (e.g. `tag:container`), in
a file (or `-` for stdin; never on a command line). This downloads
tailscaled if it isn't there, runs it in userspace mode with its own state
in `~/.local/state/illogical-sandbox`, joins, puts the daemon behind
`tailscale serve`, and adds it to the home daemon's list. `illogicald
sandbox` keeps tailscaled and the daemon running; after a reboot, run
`illogicald sandbox &` again. Without `--join` it prints the `illogical
hosts add` line to run on home.

## A box that can only dial out

For a box that allows nothing in but outbound HTTPS. On home,
`illogical hosts token sbx` (or `hosts invite`); on the box:

```
illogicald --peer wss://home.<tailnet>.ts.net --token ~/.config/illogical/host-token \
  [--join INVITE] [--sync [--sync-live]] &
```

The home daemon must be reachable at that URL from the box and accept
its name as a Host (`--public-host`). The home daemon lists the box and
answers for it at `/h/sbx/…`, so the host switcher and `illogical --host
sbx` work as for any host. `--sync` pushes closed panes' history to the
home daemon, encrypted at rest there; `--sync-live` pushes open ones too.

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

## Labs

A file named `labs` in a machine's state directory turns on what a new
install doesn't show: chat and threads, huddles, Fountain, studio apps,
chant workspaces, VM tabs and sandboxes, ssh invites for guests, the swarm's
city, hive and timeline views, and the matching tools, commands and options
of `illogical mcp`, `illogical --help` and `illogicald --help`. They all keep
working without it; they just aren't offered.

```
touch ~/.local/state/illogical/labs
```

The state directory is `$ILLOGICAL_STATE_DIR` if set, else
`$XDG_STATE_HOME/illogical`, else `~/.local/state/illogical`; on Windows,
`%LOCALAPPDATA%\illogical\state`. The file can be empty. Delete it to turn
labs off again.

- **Per machine.** It's read by the machine that serves the page, so every
  machine you want them on needs its own. Someone you share a session with
  sees chat and huddles on your machine if it has labs, and not otherwise.
- **No restart.** The daemon looks for the file whenever it's asked; reload the
  page to see the change.
- **Each feature still needs its own setup.** Fountain needs a login, studio a
  link, VMs a sandbox provider, guest ssh its listener; labs only stops
  them from being hidden.
