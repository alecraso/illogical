# illogical desktop

The desktop app (M46, Tauri 2): the daemon's own web client in a native
window, with a tray, updates, deep links and a bundled daemon for first
install.

It's outside the Cargo workspace, with its own `Cargo.lock`, so it builds and
releases on its own (#388). It depends on `illogical-proto` by path.

Start with `src/main.rs`, then `src/service.rs` and `src/updates.rs`.
`just desktop-check` checks it.
