# testnet/hosts: two machines for mixed-host layouts (#17)

`home` and `mac` are containers, each running the static `illogicald` on
its own loopback, as on two laptops. The browser (Playwright, on the host)
opens home's page; home's layout holds a tab and a split whose shells run
on mac, which the page reaches directly. Then mac drops off the network
with `docker network disconnect` and comes back with `docker network
connect`.

```sh
just testnet-hosts      # static build, then web/e2e/testnet-hosts.spec.ts
```

The spec brings the stack up and down itself (`ILLOGICAL_TESTNET_KEEP=1`
leaves it up) and is skipped unless `ILLOGICAL_TESTNET_HOSTS=1`, which the
recipe sets. `net.sh up|offline HOST|online HOST|down` does each step by
hand; it needs `ILLOGICAL_LOCAL_TOKEN_FILE`, the e2e tests' local token,
which both daemons take too.

It needs Docker: without it the recipe fails, unless `ILLOGICAL_SKIP_DOCKER=1`,
which skips and says nothing ran.

## Layout

| Box | Daemon | Host port | Address |
|---|---|---|---|
| `home` | `--name home` | `127.0.0.1:17748` | `172.31.77.10` |
| `mac` | `--name mac --allow-origin http://127.0.0.1:17748` | `127.0.0.1:17749` | `172.31.77.11` |

Each daemon listens on `127.0.0.1:<port>` inside its box, and a socat in
the same network namespace (`home-fwd`, `mac-fwd`) forwards the box's
address to it. So the daemon sees a loopback peer, which must show the
local token, and the Host and Origin the browser sends match the daemon's
own port. Ports, addresses and the subnet can be moved with the
`ILLOGICAL_TESTNET_HOME_*`, `ILLOGICAL_TESTNET_MAC_*` and
`ILLOGICAL_TESTNET_HOSTS_SUBNET` variables; `COMPOSE_PROJECT_NAME` runs a
second copy beside another.

## Offline is not stopped

A daemon that stops closes its connections, and `web/e2e/remote.spec.ts`
already covers that. Dropped off the network, mac closes nothing: its
daemon and shells keep running, and the page's connection to it goes
quiet. The first run of this spec found that remote panes never noticed:
they stayed "live" with mac gone. `web/src/blocks/remote.ts` now does for
remote hosts what the fleet does for its own (M25): a heartbeat after 3 s
of quiet, a dropped link after 6 s with nothing heard or no connect, then
the usual retry.

What the spec asserts, from #17's done-when:

- a tab on mac and a split of a home tab on mac, both live;
- mac offline: both say "mac is unreachable" (about 7 s), home's pane
  keeps working;
- mac back: both live again without a reload (about 1 s), with the lines
  its loop printed while it was away, and typing works.
