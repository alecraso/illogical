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
```

Every script prints `SKIP: Docker is not available` and exits 0 without
Docker. The claims expect fresh boxes: after installing anything on one,
`just testnet down` and `up` again (`bare` fails otherwise, as it should).

## Profiles

| Profile | Services | Status | For |
|---|---|---|---|
| `ssh` | `bastion`, `box-bare`, `box-systemd`, `git` | validated | S28, M51, M52's install step, #26 |
| `control` | `ssh`'s, and `control`, `fakes` | validated | M52's join, the relay, a box with no way out |

The other profiles in #200 (`relay`, `fountain`, `forgejo`) are added when
a milestone needs them.

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
- `fakes`: `web/fixtures/fakes.ts` in Node, the same fakes `just
  control-smoke` uses: GitHub sign-in (published on `127.0.0.1:22981`,
  `ILLOGICAL_TESTNET_FAKES_PORT`, for the browser's redirect), Stripe and a
  Web Push endpoint. Only control talks to the last two.

A person on the host is `web/fixtures/device-cli.ts`, the headless
approving device (docs/testing.md): it signs in, enrolls, approves join
codes and opens panes through the relay. `up.sh` writes
`.state/control.env` with control's URL and the `--via` mappings it needs.
The claims run the CLI from this tree (`cargo build -p illogical`, which
`just testnet test control` does) and need `node`.

## Claims

A claim checks one property of a running profile. `BREAK=1` breaks that
property, and the claim then has to fail; `just testnet break` checks that
every claim does, which shows each one can catch what it's about.

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
| `m52` | on a fresh box-systemd, `illogical --ssh box-systemd join` installs, starts the daemon and shows a code; the device approves it; the box is on the account's device list and online; with the ssh master closed and the bastion paused, a marker round-trips through a pane over the relay; after `docker restart` the box is back on the relay and the pane answers | polkit masked, so no lingering: the daemon doesn't come back after the restart |
| `unreachable` | box-bare joining the hosted control (no route out) is told it can't reach control, with `illogical --ssh box-bare tui` as the way in, and `--ssh` still works | joining the stack's control, which it can reach |

## Conventions

- Containers, networks and the images are named `illogical-testnet*`; `down`
  removes those and `testnet/.state`, nothing else.
- `COMPOSE_PROJECT_NAME` renames a stack, so two can run side by side (one
  per worktree): `COMPOSE_PROJECT_NAME=illo-a2` gives containers
  `illo-a2-*`, networks `illo-a2` and `illo-a2-inner`, and state in
  `testnet/.state-illo-a2`. Give it its own `ILLOGICAL_TESTNET_SSH_PORT`
  too (and for `control`, its own `ILLOGICAL_TESTNET_CONTROL_PORT`,
  `ILLOGICAL_TESTNET_FAKES_PORT` and `ILLOGICAL_TESTNET_INNER_NET`). The
  tests read the same variables.
- Host ports are off the defaults and each can be overridden with an
  `ILLOGICAL_TESTNET_*_PORT` variable.
- The scripts run on Linux and macOS (bash 3.2) and pass `shellcheck`.

## Not yet

- CI. A `testnet` job needs Docker on geek's runner (an open question in
  #200); until then the stack is run by hand.
- The illogical binaries on the `ssh` profile's boxes. When S28 and M51
  need them on a box, they arrive over ssh from the client (`just static`),
  which is what the claims guard.
