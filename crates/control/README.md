# illogical-control

illogical control: accounts, devices, the directory and the relay, for
people who don't run a tailnet and for teams. It holds metadata only:
terminal bytes travel end to end between a device and a daemon
([docs/control-e2e.md](../../docs/control-e2e.md), [docs/control.md](../../docs/control.md)).

Depends on `illogical-e2e`.

Start with `src/main.rs`, then `src/api.rs` (devices, approvals, daemons
joining, the directory) and `src/relay.rs` (the relay).
