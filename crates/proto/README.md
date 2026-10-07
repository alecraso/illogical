# arugula-proto

The wire protocol shared by arugulad and its clients: the WebSocket's JSON
control messages and binary frames, and the HTTP API's routes and types
(`src/api.rs`). The web client's copy, `web/src/proto.gen.ts`, is generated
with ts-rs: after changing a type the web client uses, run `just proto-ts`
(CI fails if the file is stale).

Depends on `arugula-core`.

Start with `src/lib.rs`: its header describes the protocol. `src/hosts.rs`
is federation's host list, and where the `labs` switch is read.
`src/service.rs` finds the service that runs the daemon here, for the
desktop app's *Daemon* menu and `arugula status`: unlike the rest it
looks at the machine (`launchctl`, `systemctl`).
