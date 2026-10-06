# illogical-core

The multiplexer's state, independent of PTYs and networking: sessions hold
tabs, tabs hold a split tree of panes, and clients change them by sending
intents. Pure state, property-tested.

Depends on no other workspace crate.

Start with `src/mux.rs` (sessions, tabs, split trees and the intents), then
`src/tree.rs` (a tab's split tree) and `src/layout.rs` (a tree into cell
rectangles). `src/access.rs` is who may do what: roles per session.
