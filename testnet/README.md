# testnet: a local stack of real services

Part A of #200: network shapes that one host's loopback can't give a test. A
box behind a bastion, a network where only ssh gets through, a box with no
illogical on it. One Compose file, one profile per scenario, borrowing
INTENTIUS/terragucci's `stack/`.

```sh
just testnet up ssh            # build the image, make keys, start the boxes
just testnet test ssh          # every claim
just testnet test ssh jump     # one claim
just testnet break ssh         # each claim under BREAK=1; all must fail
just testnet down              # remove containers, networks and .state
ssh -F testnet/.state/ssh_config box-bare   # or bastion, box-systemd

just testnet up control        # the ssh profile, plus illogical-control and its fakes
just testnet test control m52  # M52 end to end
just testnet test control m49  # M49: the CLI logged in, reaching a direct and a relayed box
just testnet test control m49team  # #254: the CLI reaching a team machine of another account
just testnet up tailnet        # headscale and two Tailscale nodes
just testnet measure tailnet   # S28: ssh against the tailnet path
```

The Rust tests that drive illogical against the stack,
`crates/daemon/tests/ssh.rs` (M51, box-bare and git) and
`crates/daemon/tests/reboot.rs` (#26, box-systemd), bring the `ssh` profile
up when it isn't. Both recreate the boxes they use and need `just static
<arch>` for the box's binaries.

Docker is required: without it every script and test here fails, saying
so. Only `ILLOGICAL_SKIP_DOCKER=1` skips, and then each prints that it did
not run. CI never sets it. The claims expect fresh boxes: after installing anything on one,
`just testnet down` and `up` again (`bare` fails otherwise, as it should).

## Profiles

| Profile | Services | Status | For |
|---|---|---|---|
| `ssh` | `bastion`, `box-bare`, `box-systemd`, `git` | validated | S28, M51, M52's install step, #26 |
| `control` | `ssh`'s, and `control`, `fakes` | validated | M52's join, the relay, a box with no way out |
| `tailnet` | `headscale`, `ts-box`, `ts-client` | validated | S28's tailnet comparison |

The relay is part of `control`, and Forgejo and GitLab have a stack of
their own in [`forges/`](forges/README.md). A `fountain` profile (#200) is
added when a milestone needs it.

### `ssh`

- `bastion`: Debian with sshd only. It's the one thing published to the
  host, on `127.0.0.1:22922` (`ILLOGICAL_TESTNET_SSH_PORT`), and it forwards
  TCP for ProxyJump.
- `box-bare`: the same image with no illogical and nothing set up for it,
  on an internal network with no route out. It's reached only through the
  bastion, so anything installed on it has to arrive over ssh.
- `box-systemd`: box-bare with systemd as PID 1, logind and polkit, also on
  the internal network. It runs privileged with its own cgroup namespace
  (Docker Desktop on macOS runs it too), for lingering and user services.
  Its journal is on disk, so `docker restart` keeps it (#26's reboot test).
- `git`: a git server on the internal network, bare repositories over ssh
  like a forge's: `git@git:/srv/git/repo.git`. The user `git` has
  `git-shell` and the stack's key. The boxes trust its host key, so a
  `git push` from a box needs only the client's agent forwarded (M51).

The boxes have one user, `illo`, who logs in with the stack's key only. Agent and
TCP forwarding are on. `up.sh` writes `testnet/.state/`: the client key,
a host key per box, `known_hosts`, and an `ssh_config` that reaches each box
by name with strict host key checking and `BatchMode`.

### `tailnet`

- `headscale`: a control server for the stack's own tailnet, plain http on
  the inner network. `up.sh` makes a user, `illo`, and a reusable,
  ephemeral preauthorized key (`tailnet-authkey` in the state directory)
  that both nodes join with.
- `ts-box`: a box with sshd and Tailscale in userspace mode, as a sandbox
  runs it: its netstack forwards tailnet connections to the daemon's port
  on loopback, and the daemon asks tailscaled who is connecting.
- `ts-client`: the client, with Tailscale on a real `tailscale0` interface
  (`NET_ADMIN` and `/dev/net/tun`), so programs on it dial the tailnet.

The nodes share a network and connect directly (`tailscale ping` says
"direct"). headscale requires a DERP map, so its embedded DERP server is on,
unused. `measure-tailnet.sh` installs illogical on ts-box over ssh from
ts-client, then times the same requests over both paths (S28's numbers are
in `spikes/s28-ssh/README.md`). Tailscale SSH isn't here: its check mode
needs a login at an identity provider, which a test can't do.

### `control`

Everything in `ssh`, and:

- `control`: this tree's `illogical-control`, its relay included, from the
  static build (`just static aarch64` on Apple silicon, `just static` on
  x86_64; `just testnet up control` runs it), mounted rather than built
  into an image. It's on both networks. On the inner one it has a fixed
  address, `10.229.80.10` (`ILLOGICAL_TESTNET_INNER_NET` changes the first
  three octets), and that address is its public URL,
  `http://10.229.80.10:8080`: the boxes reach it by dialing out, as joined
  machines do, and a private address lets a daemon use plain http. The host
  reaches it on `127.0.0.1:22980` (`ILLOGICAL_TESTNET_CONTROL_PORT`).
  Its ssh jump host for guests (M65) is published on `127.0.0.1:22982`
  (`ILLOGICAL_TESTNET_GUEST_SSH_PORT`), the address invites name.
- `fakes`: `web/fixtures/fakes.ts` in Node, the same fakes `just
  control-smoke` uses: GitHub sign-in (published on `127.0.0.1:22981`,
  `ILLOGICAL_TESTNET_FAKES_PORT`, for the browser's redirect), Stripe and a
  Web Push endpoint. Only control talks to the last two.

A person on the host is `web/fixtures/device-cli.ts`, the headless
approving device ([docs/testing.md](../docs/testing.md#a-device-that-approves-things)): it signs in, enrolls, approves join
codes and opens panes through the relay. `up.sh` writes
`.state/control.env` with control's URL and the `--via` mappings it needs.
The claims run the CLI from this tree (`cargo build -p illogical`, which
`just testnet test control` does) and need `node`.

## Claims

A claim checks one property of a running profile. `BREAK=1` breaks that
property, and the claim then has to fail; `just testnet break` checks that
every claim does, which shows each one can catch what it's about.
[docs/testing.md](../docs/testing.md#the-ssh-tracks-tests) says which
promise of the SSH track each claim and test guards, and what a failure
means.

| Claim | Checks | Broken by |
|---|---|---|
| `login` | the stack's key logs into the bastion, host key checked | a fresh key the boxes don't know |
| `jump` | box-bare answers through the bastion with ProxyJump | the bastion's `AllowTcpForwarding` off |
| `inner` | box-bare has no default route | checking the bastion instead |
| `bare` | no illogical on box-bare's login PATH, no state or config dir | a stub `/usr/local/bin/illogical` |
| `stdio` | 1 MiB of random bytes through `cat` on box-bare come back identical | a forced tty (`-tt`) |
| `agent` | a key in the client's agent shows on box-bare when forwarded | `ForwardAgent=no` |
| `push` | `git push` from box-bare to `git` with the key only in the forwarded agent | `ForwardAgent=no` |
| `linger` | on box-systemd, `loginctl enable-linger` works from an ssh login with no sudo | polkit masked |

The `control` profile's:

| Claim | Checks | Broken by |
|---|---|---|
| `signin` | a person signs in from the host with (the fake) GitHub; their first device is trusted on enrollment | GitHub down (`fakes` stopped) |
| `reach` | box-systemd, with no route out, reaches control at its inner address | control taken off the inner network |
| `m52` | on a fresh box-systemd, `illogical --ssh box-systemd join` installs, starts the daemon and shows a code; the device approves it; the box is on the account's device list and online; with the ssh master closed and the bastion paused, a marker round-trips through a pane over the relay; a Pixel 7 (Chrome) and an iPhone (WebKit) signed in to control and approved by the device type a marker into the box's pane, read back by the device (`web/fixtures/m52-phones.ts`); after `docker restart` the box is back on the relay, and the same phones and the device reach the pane again | polkit masked, so no lingering: the daemon doesn't come back after the restart |
| `unreachable` | box-bare joining the hosted control (no route out) is told it can't reach control, with `illogical --ssh box-bare tui` as the way in, and `--ssh` still works | joining the stack's control, which it can reach |
| `m49` | box-systemd and box-bare join the stack's control (the device approves both); box-bare's daemon also listens on the inner network and lists `http://box-bare:7681`, box-systemd lists no URL. On the bastion (the CLI copied there, no daemon, so no `hosts.json`), `illogical login` shows a code and the device approves it; `illogical hosts` lists both from control, marked; `--host box-bare` (direct) and `--host box-systemd` (relayed) each `run`, `ls` and `capture`; then on box-systemd `events --follow` and `tail --follow` print while running, and `attach` and `tui`, in a pty from `ssh -tt`, type a command, see its answer and leave with Ctrl-] | the CLI isn't logged in: neither name resolves |
| `m49team` | an owner (a second headless device) makes a team and joins box-systemd to it; the CLI's account asks to join and is let in; `illogical hosts` on the bastion lists box-systemd as the owner's, and `--host box-systemd` captures its pane and attaches to it as a team editor, relayed | the CLI pinned a different root for the owner's account first, so control's chain for it is refused |

## Conventions

- Containers, networks and the images are named `illogical-testnet*`; `down`
  removes those and `testnet/.state`, nothing else.
- `COMPOSE_PROJECT_NAME` renames a stack, so two can run side by side (one
  per worktree): `COMPOSE_PROJECT_NAME=illo-a2` gives containers and images
  `illo-a2-*`, networks `illo-a2` and `illo-a2-inner`, and state in
  `testnet/.state-illo-a2`. Give it its own `ILLOGICAL_TESTNET_SSH_PORT`
  and `ILLOGICAL_TESTNET_INNER_NET` (the inner network's subnet is fixed),
  and for `control` its own `ILLOGICAL_TESTNET_CONTROL_PORT`,
  `ILLOGICAL_TESTNET_FAKES_PORT` and `ILLOGICAL_TESTNET_GUEST_SSH_PORT`. The tests read the same variables.
- Host ports are off the defaults and each can be overridden with an
  `ILLOGICAL_TESTNET_*_PORT` variable.
- The scripts run on Linux and macOS (bash 3.2) and pass `shellcheck`.
- CI (`.github/workflows/check.yml`) runs `up`, `test` and `break` for the
  `ssh` and `control` profiles on the Linux runner; both runners run
  `ssh.rs` and `reboot.rs`, so both need Docker.

## Not yet

- Claims for the tailnet profile; its check is the measurement, which fails
  when either path doesn't reach the daemon.

The `ssh` profile's boxes never have illogical built in: tests put it there
over ssh from the client (`just static`), which is what `bare` guards.
