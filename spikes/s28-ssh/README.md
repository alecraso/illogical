# S28: reach a machine over ssh
The code and fixtures for this spike are on the `archive/spikes` branch: `git show archive/spikes:spikes/s28-ssh/<file>`.

Spike for the SSH track (#153, tracker #157). Run on 2026-10-04 from a Mac
(Apple silicon, OpenSSH 9.x) against the test stack's `ssh` profile
(`testnet/`, #200): `bastion`, `box-bare` (Debian, no illogical, no route
out, reached only by ProxyJump) and `box-systemd` (the same with systemd,
logind and polkit). Daemons and clients were 0.16.0 from this tree, plus the
0.12.0 and 0.15.0 release tarballs for skew.

Not answered yet, because they need machines this run didn't have (see the
end): launchd on jake-mini with no GUI login, and Tailscale SSH's check
prompt. The comparison with the tailnet path was run on 2026-10-04 in the
test stack's `tailnet` profile instead of on geek (below).

## Answers

### Does the daemon need a bridge at all?

No, not when the box's sshd allows Unix-socket forwarding, which OpenSSH
does by default (`AllowStreamLocalForwarding yes`). One master connection
with a forward of the daemon's socket is enough:

```sh
ssh -o ControlMaster=yes -o ControlPath=./cm/%C -o ControlPersist=120 -fN \
    -o StreamLocalBindUnlink=yes \
    -L ./fwd.sock:/home/illo/.local/state/illogical/sock box-bare
illogical --socket ./fwd.sock ls          # and run, capture, attach, tui, events -f
```

Every CLI command, `attach`, `events -f` and `tui` worked through it
unchanged. On the daemon's side the socket is full owner control with no
token (`server.rs:153`), which is the trust an ssh login already has.

When forwarding is off (`AllowStreamLocalForwarding no`,
`DisableForwarding yes`, some managed sshds), the prototype stdio bridge here
(`src/main.rs`, `ssh box s28-bridge`: one daemon connection per ssh channel)
worked for the same commands, including `tui`. M51 uses the bridge for
everything: it works whatever the box's sshd allows, and it needs neither
the far socket's path nor a socket file on the client. The forward is about
11 ms faster per new connection (below), which is worth adding if a real
network shows it matters.

### What does each path cost?

Measured on loopback Docker, so these are the overhead of ssh and the
paths, not network latency. Medians, `GET /api/host`:

| Path | New connection | Request on an open connection |
|---|---|---|
| `-L` socket forward over a ControlMaster | 0.7–0.9 ms (p90 up to 41 ms just after the master starts) | 0.33 ms |
| stdio bridge, one `ssh` exec channel per connection, over a ControlMaster | 12 ms (p90 54 ms) | same stream as the forward |
| a fresh ssh connection (no master), two hops | 230 ms | |

An exec channel over a master costs about 8 ms with one hop or two, so
ProxyJump adds nothing measurable once the master is up. The CLI opens one
connection per request (`http.rs:229`), so the master matters much more than
the bridge's framing: without it every command pays two handshakes.

A mux over one ssh stdio stream (`illogical_e2e::mux`, as dial-out uses it)
would bring the bridge's new-connection cost down to the forward's. It isn't
needed for M51's first cut. The CLI has no tokio and doesn't depend on
illogical-e2e (PLAN.md: "the CLI stays without tokio"), so the bridge should
stay one stream per channel until a measurement on a real network says
otherwise.

### ssh against the tailnet path

Measured on 2026-10-04 with the stack's `tailnet` profile (`just testnet up
tailnet && just testnet measure tailnet`): headscale 0.29.4 and Tailscale
1.102.4 in containers, a box (`ts-box`, userspace tailscaled, as sandboxes
run it) and a client (`ts-client`, a real `tailscale0`), connected directly
on one Docker network. The same daemon on ts-box, the same CLI on
ts-client, timed there; four runs, ms:

| | median | p90 |
|---|---|---|
| `illogical ls` over `--ssh` (bridge channel on a warm master) | 11.7–13.6 | 13.7–18.5 |
| `illogical ls` over the tailnet (`--host http://100.64.0.2:7681`) | 2.3–3.3 | 2.8–10.0 |
| `illogical export` of an 8 MiB pane over `--ssh` | 36–40 | 39–51 |
| the same over the tailnet | 25–28 | 27–29 |

So on a fast link the tailnet path is about 10 ms quicker per new
connection (the bridge's exec channel, as above) and about a third quicker
for bulk output, where ssh's encryption and the bridge's copy both cost.
Neither is noticeable for a person at a terminal; it matters for scripts
that run many short commands, which is what the socket forward or a mux
over one channel would fix. One bulk transfer over the tailnet took
36 s in an earlier run of five; the eighty in the four runs above all took
under 32 ms, so it's noted, not explained. A real network (geek to a
box elsewhere) adds the same round trips to both paths.

### Gotchas found on the way

- **Half-close kills requests.** The daemon's HTTP server (hyper) treats a
  client's `shutdown(Write)` as the client leaving and drops a request it
  hasn't answered. The first bridge passed stdin's EOF through and got empty
  responses. Whatever carries a stream must not forward EOF early, or the
  daemon has to allow half-close.
- **A refused forward is silent.** With forwarding off, `ssh -fN -L …`
  still exits 0 (even with `ExitOnForwardFailure=yes`, which covers binding
  the local socket, not the far side). A connection through it just gets EOF.
  The client has to probe (`GET /api/host`) before trusting the forward.
- **Socket paths are short.** macOS allows 104 bytes for a Unix socket path,
  so the forward's local end and the `ControlPath` need a short directory
  (`$XDG_RUNTIME_DIR` or `~/.cache/illogical/ssh/%C`), never something under
  a deep temp dir. A relative path in `-L` must contain a `/` (`./fwd.sock`)
  or ssh reads it as a port.
- **`~/.local/bin` isn't on PATH** for `ssh box cmd` on Debian (it comes from
  `~/.profile`, which only login shells read). Call the binary by its full
  path. Don't go through a login shell instead: anything an rc file prints
  would corrupt the bridge's stdout.
- **`run` sends the client's directory.** `illogical run` puts the local cwd
  in the request even when the daemon is remote (`main.rs:2010` doesn't check
  `REMOTE`, unlike `diff` at `main.rs:1519`), so a pane on the box was
  labelled with a Mac path. That's true of `--host` today too. M51 should
  treat ssh as remote and fix `run`.
- **Old daemons need CA roots.** 0.12.0 and 0.15.0 panic at startup on a box
  with no CA certificates (`apps/studio.rs:87`, reqwest finds none). 0.16.0
  bundles its roots (#199) and starts. An install over ssh to a minimal box
  needs 0.16.0 or later.

- **The agent comes from the master.** Through a ControlMaster, a channel
  gets the agent the master was started with, and only if the master was
  started with `ForwardAgent=yes`; `-A` on the channel alone forwards
  nothing. So the master has to be started with the agent forwarded, and
  it's the client's agent at that moment that the box sees.

### Installing when the box has none

`command -v ~/.local/bin/illogical` over ssh says whether it's there. The
binaries went over the same connection with `cat > ~/.local/bin/X.tmp &&
mv`: 39 MiB in about 1 s, no scp or sftp, no network on the box. A Mac
client has no Linux binaries, so the client should fetch the release tarball
for its own version and the box's `uname -m`
(`illogical-VERSION-ARCH-unknown-linux-musl.tar.gz`), check it against
`SHA256SUMS`, cache it, and stream it over ssh. A Linux client of the same
architecture can send its own binaries. The box needs nothing but sshd and
`sh`.

### Does the daemon outlive the ssh login?

On Linux, yes, in each of these cases (box-systemd unless noted):

| Setup | Logout | Reboot |
|---|---|---|
| `illogicald install` + linger | kept (pane kept running) | daemon started at boot, panes restored with scrollback (programs restart as fresh shells, as M2 does) |
| `loginctl enable-linger` with no sudo, polkit present | works from the ssh session (Debian's polkit allows a user's own linger) | |
| `loginctl enable-linger` with no sudo, polkit masked | refused: "Could not enable linger" | |
| detached (`setsid -f illogicald --keep-panes`), no linger, `KillUserProcesses=no` (Debian's default) | kept (the session stays "closing") | gone |
| the same with `KillUserProcesses=yes` | killed | gone |
| no systemd at all (box-bare), detached | kept across disconnects and logouts | gone |

So M52 should run `loginctl enable-linger` itself and, if that's refused,
say so and offer `sudo loginctl enable-linger $USER`. Without linger, a
detached daemon is a fallback that survives logout on most boxes but never a
reboot. `illogicald install` only prints a note when linger is off
(`install.rs:133`), so M52 has to do the enabling.

macOS (launchd from an ssh session with no GUI login) is still open: it needs
jake-mini.

### Which ssh options

- **ControlMaster** with `ControlPersist` for everything after the first
  connection, at a short `ControlPath` (above). It's the difference between
  230 ms and under 1 ms per command.
- **Keepalives.** With none, a dead link hangs indefinitely (a paused bastion
  still hung after 40 s). `ServerAliveInterval=3`, `ServerAliveCountMax=2`
  noticed in 6 s ("Timeout, server box-bare not responding"). Something like
  `ServerAliveInterval=10` and `CountMax=3` (30 s) fits panes over flaky
  links.
- **BatchMode** for checks and anything in the background (probes, the
  bridge after the master exists). It fails at once on an unknown key or
  host key instead of prompting. The first connection to a box must not use
  it: that's where the user accepts the host key or types a password or 2FA
  code, in the client's terminal.
- **No tty** (`-T`) for the bridge. A forced tty (`-tt`) mangles the stream
  (the testnet's `stdio` claim).
- **ProxyJump** worked through the bastion with forwarding and the agent, at
  no measurable cost once the master is up.
- **The user's `~/.ssh/config`** is read as usual, since we run the system
  `ssh` and only add `-o` options.

### Version skew

Today nothing checks: the TUI ignores `Hello.version` (`tui/app.rs:189`) and
the CLI never asks. Every pair tested worked through the bridge: clients
0.12.0, 0.15.0 and 0.16.0 against daemons 0.12.0, 0.15.0 and 0.16.0 (`ls`,
`run` and `capture`, `attach`). Ruling 7's rule (refuse a different major
version, offer to upgrade) has nothing to catch yet, and reading 0.x minors
as majors would refuse pairs that work. The cheapest place for the check is
the probe the client already needs (`GET /api/host` returns `version`).

### `illogicald join` from an ssh session

By reading the code: it prints the code and the approval URL, waits, then
asks on stdin for the account's fingerprint (flushed, so the prompt shows
over a pipe) or takes `--account`. A plain `ssh box illogicald join URL`
passes stdin through, so it should work without a tty. It hasn't been run
against a control yet; M52 does that with the test stack's `control` profile.

## Recommendations for M51 and M52

- M51's ssh transport: one ControlMaster per box (short `ControlPath`,
  keepalives, the agent forwarded), and `illogical bridge` (this spike's
  one-stream bridge, in the CLI) on a channel per connection. `Target::Ssh`
  in `cli/src/http.rs` returns those. Mark it remote (`REMOTE`) and fix
  `run`'s cwd. The socket forward can come later as a speedup.
- The bridge must not pass a half-close to the daemon.
- Install when missing: probe `~/.local/bin/illogical`, ask once, stream the
  release tarball for the box's architecture over ssh.
- Version: compare `GET /api/host`'s version in the probe; refuse a different
  major and offer the install path to upgrade. Treating 0.x minors as majors
  would refuse pairs that work (every pair above), so 0.x is one major.
- M52: `illogicald install`, then `loginctl enable-linger` (offer sudo if
  refused), then `illogicald join` over the same ssh session.

## Still to do (needs a person or a machine)

- From geek: the same measurements over a real network. The comparison
  itself is done (above) in containers.
- jake-mini in person: `illogicald install` from an ssh session with no GUI
  login (does `launchctl bootstrap gui/<uid>` work, or does it need the
  `user/<uid>` domain?), and whether it survives logout and reboot.
- Tailscale SSH's check prompt (`tailscale ssh` with a check policy): does it
  appear in the client's terminal on the first connection, and does a
  ControlMaster keep later connections from asking again? Check mode sends
  the user to an identity provider's login, so no test can answer it; it
  stays a known limit (#214, "What stays human").
- The done-when's `illogical tui --ssh <fresh box>` from geek and into
  jake-mini. `--ssh` itself is M51's; here the same path was driven with
  `ssh -L` and `--socket`.

## Files

- `src/main.rs`: the one-stream stdio bridge (`s28-bridge`).
- Build it for a box: `just static`'s environment with `cargo build
  --release --target aarch64-unknown-linux-musl` in this directory.
