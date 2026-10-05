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
```

Every script prints `SKIP: Docker is not available` and exits 0 without
Docker. The claims expect fresh boxes: after installing anything on one,
`just testnet down` and `up` again (`bare` fails otherwise, as it should).

## Profiles

| Profile | Services | Status | For |
|---|---|---|---|
| `ssh` | `bastion`, `box-bare`, `box-systemd`, `git` | validated | S28, M51, M52's install step, #26 |

The other profiles in #200 (`control`, `relay`, `fountain`, `forgejo`) are
added when a milestone needs them.

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

## Conventions

- Containers, networks and the images are named `illogical-testnet*`; `down`
  removes those and `testnet/.state`, nothing else.
- `COMPOSE_PROJECT_NAME` renames a stack, so two can run side by side (one
  per worktree): `COMPOSE_PROJECT_NAME=illo-a2` gives containers
  `illo-a2-*`, networks `illo-a2` and `illo-a2-inner`, and state in
  `testnet/.state-illo-a2`. Give it its own `ILLOGICAL_TESTNET_SSH_PORT`
  too. The tests read the same variables.
- Host ports are off the defaults and each can be overridden with an
  `ILLOGICAL_TESTNET_*_PORT` variable.
- The scripts run on Linux and macOS (bash 3.2) and pass `shellcheck`.

## Not yet

- CI. A `testnet` job needs Docker on geek's runner (an open question in
  #200); until then the stack is run by hand.
- The illogical binaries. When S28 and M51 need them on a box, they arrive
  over ssh from the client (`just static`), which is what the claims guard.
