# arugula-vt

A pane's terminal state on the daemon. `VtEngine` is what the daemon needs
from a terminal emulator; `src/ghostty.rs` implements it on libghostty-vt
(the sys crate is vendored in `vendor/libghostty-vt-sys`, which patches
Ghostty after checkout). Snapshots go to clients, checkpoints to disk.

Depends on `libghostty-vt`.

Start with `src/lib.rs` (`VtEngine`), then `src/compat.rs` (keeping the
daemon's answers within what clients can draw) and `src/detect.rs` (what an
agent in a pane is doing, read off its screen). `fixtures/` holds recorded
terminal sessions the tests replay.
