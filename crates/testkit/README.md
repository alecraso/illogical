# illogical-testkit

The harness for illogicald's integration tests (#200): a dev daemon with a
state dir and socket of its own, the requests tests make to it, waits, and
cleanup when it's dropped. [docs/testing.md](../../docs/testing.md) has how to
use it.

Depends on no other workspace crate (it runs the built daemon).

Start with `src/lib.rs`: `Builder` starts a `Daemon`; `Scratch` is a temp
dir that cleans up after itself.
