# arugula-e2e

End-to-end encryption between client devices and daemons, for the control
track: control introduces them and relays, but can't read. The design is in
[docs/control-e2e.md](../../docs/control-e2e.md).

Depends on no other workspace crate.

Start with `src/channel.rs` (Noise IK between a device and a daemon), then
`src/keys.rs` (a device's X25519 and Ed25519 keys). The browser's side is in
`web/src/e2e`; `just e2e-interop` checks the two against each other.
